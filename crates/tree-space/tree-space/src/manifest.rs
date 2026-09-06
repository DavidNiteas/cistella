//! Manifest validation and self-describing bootstrap images.

use crate::DIGEST_ALGORITHM;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::TypeId;
use crate::ipc::{decode_batch, encode_batch};
use crate::path::Name;
use crate::registry::TypeRegistry;
use crate::special::{
    SPECIAL_TABLE_NAMES, empty_tables, fixed_ids_or_empty, manifest_batch, schema, u32s,
};
use crate::types::{
    Cardinality, ChildDefinition, ChildKind, ColumnDefinition, DomainType, InstanceMode, TableType,
};
use arrow::array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, StringArray, UInt8Array, UInt32Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Supported manifest value encodings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestValue {
    /// A little-endian unsigned 32-bit integer.
    U32(u32),
    /// A UTF-8 string.
    Utf8(String),
    /// One Arrow 55 IPC record batch.
    RecordBatchIpc(Vec<u8>),
    /// Opaque bytes whose meaning is assigned by its key.
    Bytes(Vec<u8>),
}

impl ManifestValue {
    fn kind(&self) -> u8 {
        match self {
            Self::U32(_) => 1,
            Self::Utf8(_) => 2,
            Self::RecordBatchIpc(_) => 3,
            Self::Bytes(_) => 4,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        match self {
            Self::U32(value) => value.to_le_bytes().to_vec(),
            Self::Utf8(value) => value.as_bytes().to_vec(),
            Self::RecordBatchIpc(value) | Self::Bytes(value) => value.clone(),
        }
    }
    fn parse(kind: u8, bytes: &[u8]) -> Result<Self> {
        match kind {
            1 if bytes.len() == 4 => Ok(Self::U32(u32::from_le_bytes(
                bytes.try_into().expect("checked length"),
            ))),
            2 => String::from_utf8(bytes.to_vec())
                .map(Self::Utf8)
                .map_err(|_| {
                    TreeSpaceError::new(
                        ErrorCode::ManifestInvalid,
                        "manifest UTF-8 value is invalid",
                    )
                }),
            3 => {
                let _ = decode_batch(bytes)?;
                Ok(Self::RecordBatchIpc(bytes.to_vec()))
            }
            4 => Ok(Self::Bytes(bytes.to_vec())),
            _ => Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "manifest value type or bytes are invalid",
            )
            .with_context("value_type", kind.to_string())),
        }
    }
}

/// Fully validated manifest whose unknown keys are retained byte-for-byte.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    entries: BTreeMap<String, ManifestValue>,
}

impl Manifest {
    /// Validates explicit manifest entries; duplicate keys are rejected before insertion.
    pub fn new(entries: impl IntoIterator<Item = (String, ManifestValue)>) -> Result<Self> {
        let mut output = BTreeMap::new();
        for (key, value) in entries {
            validate_key(&key)?;
            if output.insert(key.clone(), value).is_some() {
                return Err(TreeSpaceError::new(
                    ErrorCode::ManifestInvalid,
                    "manifest key is repeated",
                )
                .with_context("key", key));
            }
        }
        validate_required(&output)?;
        Ok(Self { entries: output })
    }

    /// Returns a manifest value by exact key.
    pub fn get(&self, key: &str) -> Option<&ManifestValue> {
        self.entries.get(key)
    }

    /// Returns entries in deterministic key order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &ManifestValue)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// Produces the frozen manifest Arrow batch.
    pub fn to_batch(&self) -> Result<RecordBatch> {
        let entries = self
            .entries
            .iter()
            .map(|(key, value)| (key.clone(), value.kind(), value.bytes()))
            .collect::<Vec<_>>();
        manifest_batch(&entries)
    }

    /// Parses and validates a frozen manifest Arrow batch.
    pub fn from_batch(batch: &RecordBatch) -> Result<Self> {
        if batch.schema().as_ref() != schema("manifest")?.as_ref() {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "manifest schema does not match ABI",
            ));
        }
        let keys = downcast::<StringArray>(batch, 0, "key")?;
        let kinds = downcast::<UInt8Array>(batch, 1, "value_type")?;
        let values = downcast::<BinaryArray>(batch, 2, "value_ipc")?;
        let mut entries = Vec::new();
        for index in 0..batch.num_rows() {
            if keys.is_null(index) || kinds.is_null(index) || values.is_null(index) {
                return Err(TreeSpaceError::new(
                    ErrorCode::ManifestInvalid,
                    "manifest values cannot be null",
                ));
            }
            entries.push((
                keys.value(index).to_owned(),
                ManifestValue::parse(kinds.value(index), values.value(index))?,
            ));
        }
        Self::new(entries)
    }
}

/// All data required to create or open an immutable library snapshot.
#[derive(Clone)]
pub struct BootstrapImage {
    /// Validated root manifest.
    pub manifest: Manifest,
    /// Every root special table, including `manifest`.
    pub special_tables: BTreeMap<String, RecordBatch>,
    /// Registry reconstructed from type special tables.
    pub registry: TypeRegistry,
}

impl BootstrapImage {
    /// Creates the built-in empty v3 library through the same manifest validator as user input.
    pub fn built_in() -> Result<Self> {
        let mut tables = empty_tables()?;
        let seed_manifest = encode_batch(&tables["manifest"])?;
        let mut entries = vec![
            ("format_major".to_owned(), ManifestValue::U32(3)),
            ("format_minor".to_owned(), ManifestValue::U32(0)),
            (
                "created_by".to_owned(),
                ManifestValue::Utf8("tree-space".to_owned()),
            ),
            (
                "digest_algorithm".to_owned(),
                ManifestValue::Utf8(DIGEST_ALGORITHM.to_owned()),
            ),
        ];
        for name in SPECIAL_TABLE_NAMES {
            let bytes = if name == "manifest" {
                seed_manifest.clone()
            } else {
                encode_batch(&tables[name])?
            };
            entries.push((format!("seed.{name}"), ManifestValue::RecordBatchIpc(bytes)));
        }
        let manifest = Manifest::new(entries)?;
        tables.insert("manifest".to_owned(), manifest.to_batch()?);
        Self::from_parts(manifest, tables)
    }

    /// Validates an image supplied by a caller before it can be published.
    pub fn from_parts(
        manifest: Manifest,
        special_tables: BTreeMap<String, RecordBatch>,
    ) -> Result<Self> {
        validate_special_tables(&special_tables)?;
        let manifest_batch = manifest.to_batch()?;
        if special_tables.get("manifest") != Some(&manifest_batch) {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "manifest table differs from manifest object",
            ));
        }
        for name in SPECIAL_TABLE_NAMES {
            let key = format!("seed.{name}");
            let value = manifest.get(&key).ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::BootstrapIncomplete,
                    "required manifest seed is absent",
                )
                .with_context("key", key.clone())
            })?;
            let ManifestValue::RecordBatchIpc(bytes) = value else {
                return Err(TreeSpaceError::new(
                    ErrorCode::ManifestInvalid,
                    "manifest seed must be a RecordBatch IPC value",
                )
                .with_context("key", key));
            };
            let seed = decode_batch(bytes)?;
            if seed.schema().as_ref() != schema(name)?.as_ref() {
                return Err(TreeSpaceError::new(
                    ErrorCode::ManifestInvalid,
                    "manifest seed schema differs from special ABI",
                )
                .with_context("key", key));
            }
        }
        let registry = registry_from_tables(&special_tables)?;
        Ok(Self {
            manifest,
            special_tables,
            registry,
        })
    }

    /// Encodes all bootstrap objects in an Arrow IPC bundle; no non-Arrow payload encoding is used.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut names = Vec::new();
        let mut payloads = Vec::new();
        for name in SPECIAL_TABLE_NAMES {
            names.push(name);
            payloads.push(encode_batch(&self.special_tables[name])?);
        }
        let bundle_schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("ipc", DataType::Binary, false),
        ]));
        let batch = RecordBatch::try_new(
            bundle_schema,
            vec![
                Arc::new(StringArray::from(names)),
                Arc::new(BinaryArray::from(
                    payloads.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                )),
            ],
        )
        .map_err(|error| {
            TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "cannot create bootstrap Arrow bundle",
            )
            .with_context("detail", error.to_string())
        })?;
        encode_batch(&batch)
    }

    /// Decodes and validates an Arrow IPC bootstrap bundle.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bundle = decode_batch(bytes)?;
        if bundle.num_columns() != 2 {
            return Err(TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "bootstrap bundle schema has wrong column count",
            ));
        }
        let names = downcast::<StringArray>(&bundle, 0, "name")?;
        let values = downcast::<BinaryArray>(&bundle, 1, "ipc")?;
        let mut tables = BTreeMap::new();
        for index in 0..bundle.num_rows() {
            let name = names.value(index).to_owned();
            if tables
                .insert(name.clone(), decode_batch(values.value(index))?)
                .is_some()
            {
                return Err(TreeSpaceError::new(
                    ErrorCode::PayloadMalformed,
                    "bootstrap bundle repeats a table",
                )
                .with_context("table", name));
            }
        }
        let manifest = Manifest::from_batch(tables.get("manifest").ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "bootstrap bundle lacks manifest",
            )
        })?)?;
        Self::from_parts(manifest, tables)
    }

    /// Replaces one or more special tables and rebuilds the matching manifest seeds.
    pub fn with_special_tables(
        &self,
        tables: impl IntoIterator<Item = (String, RecordBatch)>,
    ) -> Result<Self> {
        let mut special_tables = self.special_tables.clone();
        for (name, batch) in tables {
            if !SPECIAL_TABLE_NAMES.contains(&name.as_str()) {
                return Err(TreeSpaceError::new(
                    ErrorCode::ManifestInvalid,
                    "cannot replace a non-special table",
                )
                .with_context("table", name));
            }
            special_tables.insert(name, batch);
        }
        let mut entries = Vec::new();
        for (key, value) in self.manifest.entries() {
            if let Some(seed_name) = key.strip_prefix("seed.") {
                if let Some(batch) = special_tables.get(seed_name) {
                    entries.push((
                        key.to_owned(),
                        ManifestValue::RecordBatchIpc(encode_batch(batch)?),
                    ));
                    continue;
                }
            }
            entries.push((key.to_owned(), value.clone()));
        }
        let manifest = Manifest::new(entries)?;
        special_tables.insert("manifest".to_owned(), manifest.to_batch()?);
        Self::from_parts(manifest, special_tables)
    }

    /// Materializes an in-memory registry into the type special tables.
    pub fn with_registry(&self, registry: &TypeRegistry) -> Result<Self> {
        let export = registry.export();
        let mut domain_ids = Vec::new();
        let mut domain_names = Vec::new();
        let mut domain_versions = Vec::new();
        let mut domain_modes = Vec::new();
        for ty in &export.domain_types {
            domain_ids.push(ty.type_id.as_bytes());
            domain_names.push(ty.name.as_str());
            domain_versions.push(ty.version);
            domain_modes.push(instance_mode_u8(ty.instance_mode));
        }
        let dt_types = RecordBatch::try_new(
            schema("dt_types")?,
            vec![
                Arc::new(fixed_ids_or_empty(domain_ids)),
                Arc::new(StringArray::from(domain_names)),
                Arc::new(u32s(domain_versions)),
                Arc::new(UInt8Array::from(domain_modes)),
            ],
        )
        .map_err(manifest_batch_error("dt_types"))?;

        let mut parent_ids = Vec::new();
        let mut child_orders = Vec::new();
        let mut child_names = Vec::new();
        let mut child_kinds = Vec::new();
        let mut child_type_ids = Vec::new();
        let mut cardinalities = Vec::new();
        let mut child_modes = Vec::new();
        for ty in &export.domain_types {
            for child in &ty.children {
                parent_ids.push(ty.type_id.as_bytes());
                child_orders.push(child.child_order);
                child_names.push(child.name.as_str());
                child_kinds.push(child_kind_u8(child.child_kind));
                child_type_ids.push(child.child_type_id.as_bytes());
                cardinalities.push(cardinality_u8(child.cardinality));
                child_modes.push(instance_mode_u8(child.instance_mode));
            }
        }
        let dt_children = RecordBatch::try_new(
            schema("dt_children")?,
            vec![
                Arc::new(fixed_ids_or_empty(parent_ids)),
                Arc::new(u32s(child_orders)),
                Arc::new(StringArray::from(child_names)),
                Arc::new(UInt8Array::from(child_kinds)),
                Arc::new(fixed_ids_or_empty(child_type_ids)),
                Arc::new(UInt8Array::from(cardinalities)),
                Arc::new(UInt8Array::from(child_modes)),
            ],
        )
        .map_err(manifest_batch_error("dt_children"))?;

        let mut table_ids = Vec::new();
        let mut table_names = Vec::new();
        let mut table_versions = Vec::new();
        for ty in &export.table_types {
            table_ids.push(ty.type_id.as_bytes());
            table_names.push(ty.name.as_str());
            table_versions.push(ty.version);
        }
        let tt_types = RecordBatch::try_new(
            schema("tt_types")?,
            vec![
                Arc::new(fixed_ids_or_empty(table_ids)),
                Arc::new(StringArray::from(table_names)),
                Arc::new(u32s(table_versions)),
            ],
        )
        .map_err(manifest_batch_error("tt_types"))?;

        let mut column_type_ids = Vec::new();
        let mut column_orders = Vec::new();
        let mut column_names = Vec::new();
        let mut column_fields = Vec::new();
        let mut nullable = Vec::new();
        let mut in_hash = Vec::new();
        for ty in &export.table_types {
            for column in &ty.columns {
                column_type_ids.push(ty.type_id.as_bytes());
                column_orders.push(column.col_order);
                column_names.push(column.name.as_str());
                column_fields.push(column.field_ipc.as_slice());
                nullable.push(column.nullable);
                in_hash.push(column.in_hash);
            }
        }
        let tt_columns = RecordBatch::try_new(
            schema("tt_columns")?,
            vec![
                Arc::new(fixed_ids_or_empty(column_type_ids)),
                Arc::new(u32s(column_orders)),
                Arc::new(StringArray::from(column_names)),
                Arc::new(BinaryArray::from(column_fields)),
                Arc::new(BooleanArray::from(nullable)),
                Arc::new(BooleanArray::from(in_hash)),
            ],
        )
        .map_err(manifest_batch_error("tt_columns"))?;

        let mut composition_type_ids = Vec::new();
        let mut composition_includes = Vec::new();
        for ty in &export.table_types {
            for include in &ty.includes_type_ids {
                composition_type_ids.push(ty.type_id.as_bytes());
                composition_includes.push(include.as_bytes());
            }
        }
        let tt_composition = RecordBatch::try_new(
            schema("tt_composition")?,
            vec![
                Arc::new(fixed_ids_or_empty(composition_type_ids)),
                Arc::new(fixed_ids_or_empty(composition_includes)),
            ],
        )
        .map_err(manifest_batch_error("tt_composition"))?;

        self.with_special_tables([
            ("dt_types".to_owned(), dt_types),
            ("dt_children".to_owned(), dt_children),
            ("tt_types".to_owned(), tt_types),
            ("tt_columns".to_owned(), tt_columns),
            ("tt_composition".to_owned(), tt_composition),
        ])
    }

    /// Adds or replaces a manifest entry and rebuilds the manifest special table.
    pub fn with_manifest_entry(&self, key: &str, value: ManifestValue) -> Result<Self> {
        let mut entries = self
            .manifest
            .entries()
            .filter(|(existing, _)| *existing != key)
            .map(|(existing, value)| (existing.to_owned(), value.clone()))
            .collect::<Vec<_>>();
        entries.push((key.to_owned(), value));
        let manifest = Manifest::new(entries)?;
        let mut tables = self.special_tables.clone();
        tables.insert("manifest".to_owned(), manifest.to_batch()?);
        Self::from_parts(manifest, tables)
    }
}

fn validate_key(key: &str) -> Result<()> {
    let valid = key.starts_with("seed.")
        || (!key.is_empty()
            && key.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            }));
    if valid {
        Ok(())
    } else {
        Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "manifest key is not portable dotted ASCII",
        )
        .with_context("key", key))
    }
}
fn validate_required(entries: &BTreeMap<String, ManifestValue>) -> Result<()> {
    let mut required = BTreeSet::from(["format_major", "format_minor", "created_by"]);
    for name in SPECIAL_TABLE_NAMES {
        required.insert(Box::leak(format!("seed.{name}").into_boxed_str()));
    }
    for key in required {
        if !entries.contains_key(key) {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "required manifest key is absent",
            )
            .with_context("key", key));
        }
    }
    match entries.get("format_major") {
        Some(ManifestValue::U32(3)) => {}
        _ => {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "format_major must be u32 value 3",
            ));
        }
    }
    match entries.get("format_minor") {
        Some(ManifestValue::U32(_)) => {}
        _ => {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "format_minor must be u32",
            ));
        }
    }
    match entries.get("created_by") {
        Some(ManifestValue::Utf8(_)) => {}
        _ => {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "created_by must be UTF-8",
            ));
        }
    }
    Ok(())
}
fn validate_special_tables(tables: &BTreeMap<String, RecordBatch>) -> Result<()> {
    if tables.len() != SPECIAL_TABLE_NAMES.len() {
        return Err(TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "bootstrap does not contain exactly nine special tables",
        ));
    }
    for name in SPECIAL_TABLE_NAMES {
        let batch = tables.get(name).ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::BootstrapIncomplete, "special table is absent")
                .with_context("table", name)
        })?;
        if batch.schema().as_ref() != schema(name)?.as_ref() {
            return Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "special table schema does not match ABI",
            )
            .with_context("table", name));
        }
    }
    Ok(())
}
fn downcast<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> Result<&'a T> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "manifest column has incorrect Arrow type",
            )
            .with_context("column", name)
        })
}

fn registry_from_tables(tables: &BTreeMap<String, RecordBatch>) -> Result<TypeRegistry> {
    let mut registry = TypeRegistry::new();
    let dt_types = tables["dt_types"].clone();
    let ids = downcast::<FixedSizeBinaryArray>(&dt_types, 0, "type_id")?;
    let names = downcast::<StringArray>(&dt_types, 1, "name")?;
    let versions = downcast::<UInt32Array>(&dt_types, 2, "version")?;
    let modes = downcast::<UInt8Array>(&dt_types, 3, "instance_mode")?;
    let mut domains = Vec::new();
    for index in 0..dt_types.num_rows() {
        domains.push(DomainType {
            type_id: TypeId::from_bytes(ids.value(index).try_into().expect("fixed ID")),
            name: Name::new(names.value(index))
                .map_err(|error| TreeSpaceError::new(ErrorCode::ManifestInvalid, error.message))?,
            version: versions.value(index),
            instance_mode: decode_mode(modes.value(index))?,
            children: Vec::new(),
            includes_type_ids: Vec::new(),
        });
    }
    let children = &tables["dt_children"];
    let parent_ids = downcast::<FixedSizeBinaryArray>(children, 0, "parent_type_id")?;
    let orders = downcast::<UInt32Array>(children, 1, "child_order")?;
    let child_names = downcast::<StringArray>(children, 2, "name")?;
    let kinds = downcast::<UInt8Array>(children, 3, "child_kind")?;
    let child_ids = downcast::<FixedSizeBinaryArray>(children, 4, "child_type_id")?;
    let cardinalities = downcast::<UInt8Array>(children, 5, "cardinality")?;
    let child_modes = downcast::<UInt8Array>(children, 6, "instance_mode")?;
    for index in 0..children.num_rows() {
        let parent = TypeId::from_bytes(parent_ids.value(index).try_into().expect("fixed ID"));
        let child = ChildDefinition::new(
            orders.value(index),
            Name::new(child_names.value(index))
                .map_err(|error| TreeSpaceError::new(ErrorCode::ManifestInvalid, error.message))?,
            decode_kind(kinds.value(index))?,
            TypeId::from_bytes(child_ids.value(index).try_into().expect("fixed ID")),
            decode_cardinality(cardinalities.value(index))?,
            decode_mode(child_modes.value(index))?,
        );
        if let Some(domain) = domains.iter_mut().find(|domain| domain.type_id == parent) {
            domain.children.push(child);
        } else {
            return Err(TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "dt_children parent is not in dt_types",
            ));
        }
    }
    for domain in domains {
        registry.register_domain_type(domain)?;
    }
    let tt_types = tables["tt_types"].clone();
    let table_ids = downcast::<FixedSizeBinaryArray>(&tt_types, 0, "type_id")?;
    let table_names = downcast::<StringArray>(&tt_types, 1, "name")?;
    let table_versions = downcast::<UInt32Array>(&tt_types, 2, "version")?;
    let mut table_types = Vec::new();
    for index in 0..tt_types.num_rows() {
        table_types.push(TableType {
            type_id: TypeId::from_bytes(table_ids.value(index).try_into().expect("fixed ID")),
            name: Name::new(table_names.value(index))
                .map_err(|error| TreeSpaceError::new(ErrorCode::ManifestInvalid, error.message))?,
            version: table_versions.value(index),
            columns: Vec::new(),
            includes_type_ids: Vec::new(),
        });
    }
    let columns = &tables["tt_columns"];
    let column_type_ids = downcast::<FixedSizeBinaryArray>(columns, 0, "type_id")?;
    let column_orders = downcast::<UInt32Array>(columns, 1, "col_order")?;
    let column_names = downcast::<StringArray>(columns, 2, "name")?;
    let field_ipc = downcast::<BinaryArray>(columns, 3, "field_ipc")?;
    let nullable = downcast::<BooleanArray>(columns, 4, "nullable")?;
    let in_hash = downcast::<BooleanArray>(columns, 5, "in_hash")?;
    for index in 0..columns.num_rows() {
        let type_id =
            TypeId::from_bytes(column_type_ids.value(index).try_into().expect("fixed ID"));
        let column = ColumnDefinition {
            col_order: column_orders.value(index),
            name: column_names.value(index).to_owned(),
            field_ipc: field_ipc.value(index).to_vec(),
            nullable: nullable.value(index),
            in_hash: in_hash.value(index),
        };
        if let Some(table) = table_types
            .iter_mut()
            .find(|table| table.type_id == type_id)
        {
            table.columns.push(column);
        } else {
            return Err(TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "tt_columns type is not in tt_types",
            ));
        }
    }
    let compositions = &tables["tt_composition"];
    let composition_ids = downcast::<FixedSizeBinaryArray>(compositions, 0, "type_id")?;
    let includes_ids = downcast::<FixedSizeBinaryArray>(compositions, 1, "includes_type_id")?;
    for index in 0..compositions.num_rows() {
        let type_id =
            TypeId::from_bytes(composition_ids.value(index).try_into().expect("fixed ID"));
        let include = TypeId::from_bytes(includes_ids.value(index).try_into().expect("fixed ID"));
        if let Some(table) = table_types
            .iter_mut()
            .find(|table| table.type_id == type_id)
        {
            table.includes_type_ids.push(include);
        } else {
            return Err(TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "tt_composition type is not in tt_types",
            ));
        }
    }
    let mut remaining = table_types;
    while !remaining.is_empty() {
        let before = remaining.len();
        let mut next = Vec::new();
        for table in remaining {
            if registry.register_table_type(table.clone()).is_err() {
                next.push(table);
            }
        }
        if next.len() == before {
            return Err(TreeSpaceError::new(
                ErrorCode::CompositionCycle,
                "table composition cannot be topologically registered",
            ));
        }
        remaining = next;
    }
    Ok(registry)
}
fn decode_mode(value: u8) -> Result<InstanceMode> {
    match value {
        0 => Ok(InstanceMode::Exclusive),
        1 => Ok(InstanceMode::Shared),
        _ => Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "invalid instance_mode",
        )),
    }
}
fn decode_kind(value: u8) -> Result<ChildKind> {
    match value {
        0 => Ok(ChildKind::Domain),
        1 => Ok(ChildKind::Table),
        _ => Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "invalid child_kind",
        )),
    }
}
fn decode_cardinality(value: u8) -> Result<Cardinality> {
    match value {
        0 => Ok(Cardinality::ExactlyOne),
        1 => Ok(Cardinality::ZeroOrOne),
        2 => Ok(Cardinality::ZeroOrMore),
        3 => Ok(Cardinality::OneOrMore),
        _ => Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "invalid cardinality",
        )),
    }
}

fn instance_mode_u8(value: InstanceMode) -> u8 {
    match value {
        InstanceMode::Exclusive => 0,
        InstanceMode::Shared => 1,
    }
}

fn child_kind_u8(value: ChildKind) -> u8 {
    match value {
        ChildKind::Domain => 0,
        ChildKind::Table => 1,
    }
}

fn cardinality_u8(value: Cardinality) -> u8 {
    match value {
        Cardinality::ExactlyOne => 0,
        Cardinality::ZeroOrOne => 1,
        Cardinality::ZeroOrMore => 2,
        Cardinality::OneOrMore => 3,
    }
}

fn manifest_batch_error(name: &'static str) -> impl Fn(arrow::error::ArrowError) -> TreeSpaceError {
    move |error| {
        TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "cannot encode type special table",
        )
        .with_context("table", name)
        .with_context("detail", error.to_string())
    }
}
