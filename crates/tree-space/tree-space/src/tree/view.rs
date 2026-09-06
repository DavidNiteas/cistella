//! In-memory view nodes and deterministic block combinators.

use super::super::bucket::Bucket;
use crate::block::RefId;
use crate::block::{Blob, Block, BlockKind, Kv, Sequence};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ipc::encode_batch;
use crate::tree::{AccessOut, TreeNode};
use crate::xpath::XPath;
use arrow::array::UInt32Array;
use arrow::compute::{concat, take};
use arrow::record_batch::RecordBatch;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// The join mode supported by the closed view kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinMode {
    /// Keep only rows with a matching key on both sides.
    Inner,
    /// Keep every left row and null-fill missing right rows.
    Left,
}

/// A closed or registered operation applied to ordered block inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Combinator {
    /// Concatenate homogeneous inputs.
    Concat,
    /// Join exactly two table inputs by the named columns.
    Join {
        /// Key column names, in comparison order.
        keys: Vec<String>,
        /// Join retention mode.
        mode: JoinMode,
    },
    /// Project columns and optionally take a row interval `(offset, len)`.
    Select {
        /// Output column names, in output order.
        columns: Vec<String>,
        /// Optional `(offset, length)` row interval.
        rows: Option<(usize, usize)>,
    },
    /// Dispatch to a [`CombinatorRegistry`] entry.
    Registered(String),
}

/// A view input is either a bucket block reference or another view node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewInput {
    /// A content-addressed block in the bucket.
    Ref(RefId),
    /// A nested, zero-payload view node.
    View(Box<ViewNode>),
}

/// A zero-payload tree node describing a deferred block combination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewNode {
    combinator: Combinator,
    inputs: Vec<ViewInput>,
    materialized: Option<RefId>,
}

impl ViewNode {
    /// Creates a view node. Inputs are kept in the supplied order.
    pub fn new(combinator: Combinator, inputs: impl Into<Vec<ViewInput>>) -> Result<Self> {
        let inputs = inputs.into();
        validate_arity(&combinator, inputs.len())?;
        Ok(Self {
            combinator,
            inputs,
            materialized: None,
        })
    }

    /// Returns the operation.
    pub fn combinator(&self) -> &Combinator {
        &self.combinator
    }

    /// Returns the ordered inputs.
    pub fn inputs(&self) -> &[ViewInput] {
        &self.inputs
    }

    /// Returns the optional materialization hint.
    pub fn materialized(&self) -> Option<RefId> {
        self.materialized
    }

    /// Sets or clears the non-semantic materialization hint.
    pub fn set_materialized(&mut self, reference: Option<RefId>) {
        self.materialized = reference;
    }

    /// Returns all bucket references reachable from this view tree.
    pub fn refs(&self) -> Vec<(XPath, RefId)> {
        let mut out = Vec::new();
        for (index, input) in self.inputs.iter().enumerate() {
            match input {
                ViewInput::Ref(id) => out.push((XPath::root().index(index), *id)),
                ViewInput::View(view) => out.extend(
                    view.refs()
                        .into_iter()
                        .map(|(path, id)| (XPath::root().index(index).join(&path), id)),
                ),
            }
        }
        out
    }
    /// Resolves this view using the closed kernel.
    pub fn resolve(&self, bucket: &Bucket) -> Result<Box<dyn Block>> {
        self.resolve_with_registry(bucket, &CombinatorRegistry::new())
    }

    /// Resolves this view using built-ins and registered extensions.
    pub fn resolve_with_registry(
        &self,
        bucket: &Bucket,
        registry: &CombinatorRegistry,
    ) -> Result<Box<dyn Block>> {
        let mut blocks = Vec::with_capacity(self.inputs.len());
        for input in &self.inputs {
            blocks.push(match input {
                ViewInput::Ref(reference) => bucket.read(*reference)?,
                ViewInput::View(view) => view.resolve_with_registry(bucket, registry)?,
            });
        }
        apply(&self.combinator, &blocks, registry)
    }
}

/// Type-erased resolver for a registered combinator.
pub type RegisteredResolver = Arc<dyn Fn(&[&dyn Block]) -> Result<Box<dyn Block>> + Send + Sync>;

/// Registry for open-ended combinators; closed kernel names cannot be replaced.
#[derive(Clone, Default)]
pub struct CombinatorRegistry {
    entries: BTreeMap<String, RegisteredResolver>,
}

impl std::fmt::Debug for CombinatorRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CombinatorRegistry")
            .field("entries", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CombinatorRegistry {
    /// Creates an empty extension registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a resolver, rejecting built-in names and duplicate extensions.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        resolver: RegisteredResolver,
    ) -> Result<()> {
        let name = name.into();
        if is_builtin_name(&name) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "closed combinator name cannot be overridden",
            ));
        }
        if self.entries.contains_key(&name) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "registered combinator name already exists",
            ));
        }
        self.entries.insert(name, resolver);
        Ok(())
    }

    /// Looks up an extension resolver.
    pub fn get(&self, name: &str) -> Option<&RegisteredResolver> {
        self.entries.get(name)
    }
}

/// Resolves a view with the closed kernel.
pub fn resolve(view: &ViewNode, bucket: &Bucket) -> Result<Box<dyn Block>> {
    view.resolve(bucket)
}

/// Resolves a view with an extension registry.
pub fn resolve_with_registry(
    view: &ViewNode,
    bucket: &Bucket,
    registry: &CombinatorRegistry,
) -> Result<Box<dyn Block>> {
    view.resolve_with_registry(bucket, registry)
}

fn validate_arity(combinator: &Combinator, count: usize) -> Result<()> {
    let valid = match combinator {
        Combinator::Concat => count >= 1,
        Combinator::Join { .. } => count == 2,
        Combinator::Select { .. } => count == 1,
        // Registered combinators are open-ended: the resolver receives the
        // full ordered input slice and is free to impose its own arity.
        Combinator::Registered(_) => count >= 1,
    };
    if valid {
        Ok(())
    } else {
        Err(TreeSpaceError::new(
            ErrorCode::CardinalityViolation,
            "view input count violates combinator arity",
        ))
    }
}

fn apply(
    combinator: &Combinator,
    blocks: &[Box<dyn Block>],
    registry: &CombinatorRegistry,
) -> Result<Box<dyn Block>> {
    match combinator {
        Combinator::Concat => concat_blocks(blocks),
        Combinator::Join { keys, mode } => join_tables(blocks, keys, *mode),
        Combinator::Select { columns, rows } => select_table(blocks, columns, *rows),
        Combinator::Registered(name) => {
            let resolver = registry.get(name).ok_or_else(|| {
                TreeSpaceError::new(ErrorCode::TypeNotFound, "registered combinator is missing")
            })?;
            let inputs = blocks
                .iter()
                .map(|block| block.as_ref() as &dyn Block)
                .collect::<Vec<_>>();
            resolver(&inputs)
        }
    }
}

fn concat_blocks(blocks: &[Box<dyn Block>]) -> Result<Box<dyn Block>> {
    let kind = blocks[0].kind();
    if blocks.iter().any(|block| block.kind() != kind) {
        return Err(schema_error("concat inputs must have one block kind"));
    }
    match kind {
        BlockKind::Blob => Ok(Box::new(Blob::new(
            blocks
                .iter()
                .flat_map(|block| block.as_blob().expect("blob kind").bytes())
                .copied()
                .collect::<Vec<_>>(),
        ))),
        BlockKind::Sequence => Ok(Box::new(Sequence::new(
            blocks
                .iter()
                .flat_map(|block| {
                    block
                        .as_sequence()
                        .expect("sequence kind")
                        .values()
                        .iter()
                        .cloned()
                })
                .collect::<Vec<_>>(),
        ))),
        BlockKind::Kv => {
            let entries = blocks
                .iter()
                .flat_map(|block| block.as_kv().expect("kv kind").entries().iter().cloned())
                .collect::<Vec<_>>();
            Ok(Box::new(Kv::try_new(entries)?))
        }
        BlockKind::Table => {
            let tables = blocks
                .iter()
                .map(|block| block.as_table().expect("table kind").as_batch())
                .collect::<Vec<_>>();
            let first = tables[0];
            if tables.iter().any(|batch| batch.schema() != first.schema()) {
                return Err(schema_error("table schemas differ"));
            }
            let columns = (0..first.num_columns())
                .map(|index| {
                    let arrays = tables
                        .iter()
                        .map(|batch| batch.column(index).as_ref())
                        .collect::<Vec<_>>();
                    concat(&arrays).map_err(|error| arrow_error(error.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            let batch = RecordBatch::try_new(first.schema().clone(), columns)
                .map_err(|error| arrow_error(error.to_string()))?;
            Ok(Box::new(crate::block::ArrowTable::try_new(batch)?))
        }
        BlockKind::Named(name) => Err(schema_error(format!(
            "registered block kind '{name}' is not combinable by the closed kernel"
        ))),
    }
}

fn select_table(
    blocks: &[Box<dyn Block>],
    columns: &[String],
    rows: Option<(usize, usize)>,
) -> Result<Box<dyn Block>> {
    let table = as_table(blocks[0].as_ref())?;
    let schema = table.as_batch().schema();
    let mut fields = Vec::with_capacity(columns.len());
    let mut arrays = Vec::with_capacity(columns.len());
    for name in columns {
        let index = schema.index_of(name).map_err(|_| missing_column(name))?;
        fields.push(schema.field(index).clone());
        arrays.push(table.as_batch().column(index).clone());
    }
    let selected = RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), arrays)
        .map_err(|error| arrow_error(error.to_string()))?;
    let selected = match rows {
        None => selected,
        Some((offset, len)) => {
            let rows = selected.num_rows();
            if offset > rows || len > rows - offset {
                return Err(TreeSpaceError::new(
                    ErrorCode::CardinalityViolation,
                    "select row interval is out of range",
                ));
            }
            selected.slice(offset, len)
        }
    };
    Ok(Box::new(crate::block::ArrowTable::try_new(selected)?))
}

fn join_tables(
    blocks: &[Box<dyn Block>],
    keys: &[String],
    mode: JoinMode,
) -> Result<Box<dyn Block>> {
    let left = as_table(blocks[0].as_ref())?;
    let right = as_table(blocks[1].as_ref())?;
    let left_batch = left.as_batch();
    let right_batch = right.as_batch();
    let left_keys = key_indices(left_batch, keys)?;
    let right_keys = key_indices(right_batch, keys)?;
    let mut right_map: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    for row in 0..right_batch.num_rows() {
        if let Some(key) = row_key(right_batch, &right_keys, row)? {
            right_map.entry(key).or_default().push(row);
        }
    }
    let mut pairs = Vec::new();
    for row in 0..left_batch.num_rows() {
        let matches = row_key(left_batch, &left_keys, row)?.and_then(|key| right_map.get(&key));
        match matches {
            Some(rows) => {
                pairs.extend(rows.iter().copied().map(|right_row| (row, Some(right_row))))
            }
            None if mode == JoinMode::Left => pairs.push((row, None)),
            None => {}
        }
    }
    let mut fields = left_batch
        .schema()
        .fields()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let right_key_set = right_keys.iter().copied().collect::<BTreeSet<_>>();
    fields.extend(
        right_batch
            .schema()
            .fields()
            .iter()
            .enumerate()
            .filter(|(index, _)| !right_key_set.contains(index))
            .map(|(_, field)| {
                if mode == JoinMode::Left {
                    Arc::new(field.as_ref().clone().with_nullable(true))
                } else {
                    field.clone()
                }
            }),
    );
    let left_indices = UInt32Array::from(
        pairs
            .iter()
            .map(|(left_row, _)| Some(*left_row as u32))
            .collect::<Vec<_>>(),
    );
    let mut columns = left_batch
        .columns()
        .iter()
        .map(|array| {
            take(array.as_ref(), &left_indices, None)
                .map_err(|error| arrow_error(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    for (index, _) in right_batch.schema().fields().iter().enumerate() {
        if right_keys.contains(&index) {
            continue;
        }
        let indices = UInt32Array::from(
            pairs
                .iter()
                .map(|(_, right_row)| right_row.map(|row| row as u32))
                .collect::<Vec<_>>(),
        );
        columns.push(
            take(right_batch.column(index).as_ref(), &indices, None)
                .map_err(|error| arrow_error(error.to_string()))?,
        );
    }
    let batch = RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), columns)
        .map_err(|error| arrow_error(error.to_string()))?;
    Ok(Box::new(crate::block::ArrowTable::try_new(batch)?))
}

fn as_table(block: &dyn Block) -> Result<&crate::block::ArrowTable> {
    block
        .as_table()
        .ok_or_else(|| schema_error("view requires table blocks"))
}
fn key_indices(batch: &RecordBatch, keys: &[String]) -> Result<Vec<usize>> {
    keys.iter()
        .map(|key| {
            batch
                .schema()
                .index_of(key)
                .map_err(|_| missing_column(key))
        })
        .collect()
}
fn row_key(batch: &RecordBatch, indices: &[usize], row: usize) -> Result<Option<Vec<u8>>> {
    let columns = indices
        .iter()
        .map(|index| batch.column(*index).slice(row, 1))
        .collect::<Vec<_>>();
    if columns.iter().any(|column| column.null_count() != 0) {
        return Ok(None);
    }
    let schema = Arc::new(arrow::datatypes::Schema::new(
        indices
            .iter()
            .map(|index| batch.schema().field(*index).clone())
            .collect::<Vec<_>>(),
    ));
    let one =
        RecordBatch::try_new(schema, columns).map_err(|error| arrow_error(error.to_string()))?;
    Ok(Some(encode_batch(&one)?))
}
fn is_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "Concat" | "Join" | "Select" | "concat" | "join" | "select"
    )
}
fn schema_error(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}
fn missing_column(name: &str) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, "view column is missing")
        .with_context("column", name)
}
fn arrow_error(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

impl TreeNode for ViewNode {
    fn get(&self, xpath: &XPath) -> Result<AccessOut> {
        let (out, rest) = self.get_best_effort(xpath);
        if rest.is_root() {
            Ok(out)
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::XpathUnreachable,
                "view xpath is not reachable",
            ))
        }
    }

    fn get_best_effort(&self, xpath: &XPath) -> (AccessOut, XPath) {
        if xpath.is_root() {
            (AccessOut::Node(Box::new(self.clone())), XPath::root())
        } else {
            (AccessOut::Node(Box::new(self.clone())), xpath.clone())
        }
    }

    fn set(&mut self, _xpath: &XPath, _value: crate::tree::Slot) -> Result<()> {
        Err(TreeSpaceError::new(
            ErrorCode::XpathUnreachable,
            "view nodes are immutable",
        ))
    }

    fn leaf_refs(&self) -> Vec<(XPath, RefId)> {
        self.refs()
    }
}
