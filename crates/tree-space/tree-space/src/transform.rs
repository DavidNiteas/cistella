//! Deterministic create/migrate/repair conversion planning and execution.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ipc::decode_batch;
use crate::path::{Name, TablePath};
use crate::types::decode_field_ipc;
use arrow::array::ArrayRef;
use arrow::compute::{cast as arrow_cast, concat as arrow_concat};
use arrow::datatypes::{DataType, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// One of the six explicit conversion rule kinds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConversionRule {
    /// Renames a physical column.
    RenameColumn {
        /// Source name.
        from: Name,
        /// Target name.
        to: Name,
    },
    /// Moves a path subtree.
    Move {
        /// Source path.
        from: String,
        /// Target path.
        to: String,
    },
    /// Converts a column to an explicitly named Arrow type.
    ConvertType {
        /// Column name.
        column: Name,
        /// Target Arrow type.
        target: DataType,
    },
    /// Supplies a deterministic constant Arrow IPC scalar payload.
    FillConstant {
        /// Column name.
        column: Name,
        /// Canonical scalar IPC bytes.
        value_ipc: Vec<u8>,
    },
    /// Drops a source column.
    DropColumn {
        /// Column name.
        column: Name,
    },
    /// Invokes a pre-registered schema-checked function.
    MapFunction {
        /// Registered function name.
        name: String,
        /// Declared input Arrow schema IPC.
        input_schema_ipc: Vec<u8>,
        /// Declared output Arrow schema IPC.
        output_schema_ipc: Vec<u8>,
    },
}
/// A planned conversion that can run dry before a normal publish.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConversionPlan {
    /// Ordered explicit rules.
    pub rules: Vec<ConversionRule>,
    /// Do not write if true.
    pub dry_run: bool,
}
/// Result of checking conversion coverage before changing storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversionReport {
    /// Whether all required inputs are mapped.
    pub executable: bool,
    /// Stable diagnostic codes.
    pub diagnostics: Vec<String>,
}
impl ConversionPlan {
    /// Performs deterministic rule conflict checks without storage I/O.
    pub fn dry_run(&self) -> Result<ConversionReport> {
        let mut renamed = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for rule in &self.rules {
            if let ConversionRule::RenameColumn { from, .. } = rule {
                if !renamed.insert(from.clone()) {
                    diagnostics.push("conversion_ambiguous".to_owned());
                }
            }
        }
        if diagnostics.is_empty() {
            Ok(ConversionReport {
                executable: true,
                diagnostics,
            })
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::ConversionAmbiguous,
                "multiple rules consume the same source column",
            ))
        }
    }

    /// Checks that every column rule references a real source column.
    pub fn coverage(&self, schema: &Schema) -> ConversionReport {
        let names: BTreeSet<&str> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        let diagnostics: Vec<String> = self
            .rules
            .iter()
            .filter_map(|rule| {
                let column = match rule {
                    ConversionRule::RenameColumn { from, .. }
                    | ConversionRule::ConvertType { column: from, .. }
                    | ConversionRule::DropColumn { column: from }
                    | ConversionRule::FillConstant { column: from, .. } => Some(from.as_str()),
                    _ => None,
                };
                column
                    .filter(|name| !names.contains(name))
                    .map(|name| format!("missing_column:{name}"))
            })
            .collect();
        ConversionReport {
            executable: diagnostics.is_empty(),
            diagnostics,
        }
    }

    /// Applies the conversion rules to each batch, producing new schema-valid batches.
    pub fn apply(&self, batches: &[RecordBatch]) -> Result<Vec<RecordBatch>> {
        self.apply_with(batches, &FunctionRegistry::new())
    }

    /// Applies the conversion rules, resolving `MapFunction` through `funcs`.
    pub fn apply_with(
        &self,
        batches: &[RecordBatch],
        funcs: &FunctionRegistry,
    ) -> Result<Vec<RecordBatch>> {
        if batches.is_empty() {
            return Err(TreeSpaceError::new(
                ErrorCode::RequiredDataMissing,
                "conversion requires at least one batch",
            ));
        }
        let report = self.coverage(batches[0].schema().as_ref());
        if !report.executable {
            return Err(TreeSpaceError::new(
                ErrorCode::ConversionAmbiguous,
                "conversion coverage is incomplete",
            )
            .with_context("detail", report.diagnostics.join(";")));
        }
        batches
            .iter()
            .map(|batch| self.apply_one(batch, funcs))
            .collect()
    }

    /// Applies the first `Move` rule to rewrite a path prefix.
    ///
    /// Only the first `Move` rule is honored; subsequent `Move` rules are ignored
    /// so the plan stays deterministic for a single subtree move.
    pub fn move_path(&self, path: &TablePath) -> Result<TablePath> {
        for rule in &self.rules {
            if let ConversionRule::Move { from, to } = rule {
                let source = TablePath::parse(from)?;
                let target = TablePath::parse(to)?;
                return path.replace_prefix(&source, &target).ok_or_else(|| {
                    TreeSpaceError::new(
                        ErrorCode::PathInvalid,
                        "Move source is not a prefix of the target path",
                    )
                    .with_context("path", path.to_string())
                    .with_context("from", from.clone())
                });
            }
        }
        Ok(path.clone())
    }

    fn apply_one(&self, batch: &RecordBatch, funcs: &FunctionRegistry) -> Result<RecordBatch> {
        let mut fields = batch.schema().fields().iter().cloned().collect::<Vec<_>>();
        let mut arrays = batch.columns().to_vec();
        for rule in &self.rules {
            match rule {
                ConversionRule::RenameColumn { from, to } => {
                    let index = find_column(&fields, from)?;
                    let renamed = fields[index].as_ref().clone().with_name(to.as_str());
                    fields[index] = Arc::new(renamed);
                }
                ConversionRule::DropColumn { column } => {
                    let index = find_column(&fields, column)?;
                    fields.remove(index);
                    arrays.remove(index);
                }
                ConversionRule::ConvertType { column, target } => {
                    let index = find_column(&fields, column)?;
                    let casted = arrow_cast(&arrays[index], target).map_err(|error| {
                        TreeSpaceError::new(
                            ErrorCode::PayloadMalformed,
                            "column type conversion failed",
                        )
                        .with_context("detail", error.to_string())
                    })?;
                    arrays[index] = casted;
                    let converted = fields[index]
                        .as_ref()
                        .clone()
                        .with_data_type(target.clone());
                    fields[index] = Arc::new(converted);
                }
                ConversionRule::FillConstant { column, value_ipc } => {
                    let index = find_column(&fields, column)?;
                    let value_batch = decode_batch(value_ipc)?;
                    if value_batch.num_rows() != 1 || value_batch.num_columns() != 1 {
                        return Err(TreeSpaceError::new(
                            ErrorCode::PayloadMalformed,
                            "fill constant must be a single one-row column",
                        ));
                    }
                    let value = value_batch.column(0).clone();
                    let repeated = arrow_concat(
                        &std::iter::repeat(value.as_ref())
                            .take(batch.num_rows())
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|error| {
                        TreeSpaceError::new(
                            ErrorCode::PayloadMalformed,
                            "cannot broadcast fill constant",
                        )
                        .with_context("detail", error.to_string())
                    })?;
                    arrays[index] = repeated;
                }
                ConversionRule::MapFunction {
                    name,
                    input_schema_ipc,
                    output_schema_ipc,
                } => {
                    let input_field = decode_field_ipc(input_schema_ipc)?;
                    let index = fields
                        .iter()
                        .position(|field| field.as_ref() == &input_field)
                        .ok_or_else(|| {
                            TreeSpaceError::new(
                                ErrorCode::ConversionAmbiguous,
                                "map_function input schema matches no source column",
                            )
                            .with_context("function", name.clone())
                        })?;
                    let output = funcs.call(name, arrays[index].clone())?;
                    let output_field = decode_field_ipc(output_schema_ipc)?;
                    arrays[index] = output;
                    fields[index] = Arc::new(output_field);
                }
                ConversionRule::Move { .. } => {
                    // Path-level; handled by `move_path` outside the batch transform.
                }
            }
        }
        let schema = Schema::new(
            fields
                .iter()
                .map(|field| field.as_ref().clone())
                .collect::<Vec<_>>(),
        );
        RecordBatch::try_new(Arc::new(schema), arrays).map_err(|error| {
            TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "cannot construct converted batch",
            )
            .with_context("detail", error.to_string())
        })
    }
}

fn find_column(fields: &[Arc<arrow::datatypes::Field>], name: &Name) -> Result<usize> {
    fields
        .iter()
        .position(|field| field.name() == name.as_str())
        .ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::ConversionAmbiguous,
                "conversion references a missing column",
            )
            .with_context("column", name.as_str())
        })
}

/// Registry of schema-checked column functions for `MapFunction` rules.
#[derive(Default)]
pub struct FunctionRegistry {
    functions: HashMap<String, Arc<dyn Fn(ArrayRef) -> Result<ArrayRef> + Send + Sync>>,
}

impl FunctionRegistry {
    /// Creates an empty function registry.
    pub fn new() -> Self {
        Self::default()
    }
    /// Registers a named, schema-checked column function.
    pub fn register<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(ArrayRef) -> Result<ArrayRef> + Send + Sync + 'static,
    {
        self.functions.insert(name.into(), Arc::new(function));
    }
    /// Invokes a registered function by name.
    pub fn call(&self, name: &str, input: ArrayRef) -> Result<ArrayRef> {
        self.functions
            .get(name)
            .ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::ConversionAmbiguous,
                    "map_function is not registered",
                )
                .with_context("function", name)
            })
            .map(|function| function(input))?
    }
}
