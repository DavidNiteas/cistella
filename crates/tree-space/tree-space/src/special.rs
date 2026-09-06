//! Immutable Arrow schemas for the nine root special tables.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{Digest, NodeId, PathHash, TableId, TypeId};
use arrow::array::{
    ArrayRef, BinaryArray, FixedSizeBinaryArray, StringArray, UInt8Array, UInt32Array, UInt64Array,
    new_empty_array,
};
use arrow::buffer::Buffer;
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::ipc::convert::IpcSchemaEncoder;
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The root-only ABI names in canonical order.
pub const SPECIAL_TABLE_NAMES: [&str; 9] = [
    "manifest",
    "dt_types",
    "dt_children",
    "tt_types",
    "tt_columns",
    "tt_composition",
    "table-metadata",
    "domain-metadata",
    "column-digests",
];

fn id_field(name: &str) -> Field {
    Field::new(name, DataType::FixedSizeBinary(16), false)
}
fn digest_field(name: &str) -> Field {
    Field::new(name, DataType::FixedSizeBinary(16), false)
}

/// Returns the frozen Arrow 55 schema for a named special table.
pub fn schema(name: &str) -> Result<SchemaRef> {
    let fields = match name {
        "manifest" => vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("value_type", DataType::UInt8, false),
            Field::new("value_ipc", DataType::Binary, false),
        ],
        "dt_types" => vec![
            id_field("type_id"),
            Field::new("name", DataType::Utf8, false),
            Field::new("version", DataType::UInt32, false),
            Field::new("instance_mode", DataType::UInt8, false),
        ],
        "dt_children" => vec![
            id_field("parent_type_id"),
            Field::new("child_order", DataType::UInt32, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("child_kind", DataType::UInt8, false),
            id_field("child_type_id"),
            Field::new("cardinality", DataType::UInt8, false),
            Field::new("instance_mode", DataType::UInt8, false),
        ],
        "tt_types" => vec![
            id_field("type_id"),
            Field::new("name", DataType::Utf8, false),
            Field::new("version", DataType::UInt32, false),
        ],
        "tt_columns" => vec![
            id_field("type_id"),
            Field::new("col_order", DataType::UInt32, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("field_ipc", DataType::Binary, false),
            Field::new("nullable", DataType::Boolean, false),
            Field::new("in_hash", DataType::Boolean, false),
        ],
        "tt_composition" => vec![id_field("type_id"), id_field("includes_type_id")],
        "table-metadata" => vec![
            Field::new("path", DataType::Utf8, false),
            Field::new("path_hash", DataType::FixedSizeBinary(16), false),
            Field::new("degraded", DataType::Boolean, false),
            id_field("table_id"),
            id_field("type_id"),
            Field::new("type_version", DataType::UInt32, false),
            digest_field("table_digest"),
            Field::new("hash_algo", DataType::Utf8, false),
            Field::new("storage_kind", DataType::Utf8, false),
            Field::new("offset", DataType::UInt64, false),
            Field::new("length", DataType::UInt64, false),
            Field::new("owner_label", DataType::Utf8, true),
            Field::new("storage_path", DataType::Utf8, true),
        ],
        "domain-metadata" => vec![
            id_field("node_id"),
            Field::new("path", DataType::Utf8, false),
            id_field("type_id"),
            id_field("parent_node_id"),
            Field::new("child_order", DataType::UInt32, true),
            digest_field("domain_digest"),
        ],
        "column-digests" => vec![
            Field::new("path", DataType::Utf8, false),
            Field::new("col_order", DataType::UInt32, false),
            digest_field("digest"),
        ],
        _ => {
            return Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "unknown special table schema",
            )
            .with_context("table", name));
        }
    };
    Ok(Arc::new(Schema::new(fields)))
}

/// Returns the raw Arrow 55 IPC schema FlatBuffer used as the ABI golden value.
pub fn schema_ipc_bytes(name: &str) -> Result<Vec<u8>> {
    Ok(IpcSchemaEncoder::new()
        .schema_to_fb(schema(name)?.as_ref())
        .finished_data()
        .to_vec())
}

/// Returns an empty batch which exactly follows a special table ABI schema.
pub fn empty_batch(name: &str) -> Result<RecordBatch> {
    let schema = schema(name)?;
    let columns = schema
        .fields()
        .iter()
        .map(|field| new_empty_array(field.data_type()) as ArrayRef)
        .collect();
    RecordBatch::try_new(schema, columns).map_err(|error| {
        TreeSpaceError::new(ErrorCode::SchemaMismatch, "cannot construct special table")
            .with_context("detail", error.to_string())
    })
}

/// Returns every special table as an empty, schema-valid root table.
pub fn empty_tables() -> Result<BTreeMap<String, RecordBatch>> {
    SPECIAL_TABLE_NAMES
        .into_iter()
        .map(|name| Ok((name.to_owned(), empty_batch(name)?)))
        .collect()
}

/// Builds a schema-valid manifest batch.
pub fn manifest_batch(entries: &[(String, u8, Vec<u8>)]) -> Result<RecordBatch> {
    let keys = StringArray::from(
        entries
            .iter()
            .map(|entry| entry.0.as_str())
            .collect::<Vec<_>>(),
    );
    let kinds = UInt8Array::from(entries.iter().map(|entry| entry.1).collect::<Vec<_>>());
    let values = BinaryArray::from(
        entries
            .iter()
            .map(|entry| entry.2.as_slice())
            .collect::<Vec<_>>(),
    );
    RecordBatch::try_new(
        schema("manifest")?,
        vec![Arc::new(keys), Arc::new(kinds), Arc::new(values)],
    )
    .map_err(|error| {
        TreeSpaceError::new(ErrorCode::SchemaMismatch, "cannot construct manifest batch")
            .with_context("detail", error.to_string())
    })
}

/// Converts a 16-byte identity sequence into its ABI Arrow array.
pub fn fixed_ids(values: impl IntoIterator<Item = [u8; 16]>) -> FixedSizeBinaryArray {
    FixedSizeBinaryArray::try_from_iter(values.into_iter().map(|value| value.to_vec()))
        .expect("16-byte IDs are valid fixed binary values")
}

/// Converts a 16-byte identity sequence into an ABI Arrow array, handling empty input.
pub fn fixed_ids_or_empty(values: Vec<[u8; 16]>) -> FixedSizeBinaryArray {
    if values.is_empty() {
        FixedSizeBinaryArray::new(16, Buffer::from(Vec::<u8>::new()), None)
    } else {
        fixed_ids(values)
    }
}

/// Converts fixed digest values into their ABI Arrow array.
pub fn fixed_digests(values: impl IntoIterator<Item = [u8; 16]>) -> FixedSizeBinaryArray {
    fixed_ids(values)
}

/// Converts path hashes to their ABI big-endian fixed binary representation.
pub fn fixed_hashes(values: impl IntoIterator<Item = PathHash>) -> FixedSizeBinaryArray {
    fixed_ids(values.into_iter().map(|value| value.0.to_be_bytes()))
}

/// Converts special metadata integer data to the Arrow ABI type.
pub fn u32s(values: Vec<u32>) -> UInt32Array {
    UInt32Array::from(values)
}
/// Converts special metadata integer data to the Arrow ABI type.
pub fn u64s(values: Vec<u64>) -> UInt64Array {
    UInt64Array::from(values)
}
/// Exposes identity type aliases to keep ABI-focused callers explicit.
pub fn identity_bytes(value: TypeId) -> [u8; 16] {
    value.as_bytes()
}
/// Exposes identity type aliases to keep ABI-focused callers explicit.
pub fn node_bytes(value: NodeId) -> [u8; 16] {
    value.as_bytes()
}
/// Exposes identity type aliases to keep ABI-focused callers explicit.
pub fn table_bytes(value: TableId) -> [u8; 16] {
    value.as_bytes()
}
/// Exposes digest type aliases to keep ABI-focused callers explicit.
pub fn digest_bytes(value: Digest) -> [u8; 16] {
    value.as_bytes()
}
