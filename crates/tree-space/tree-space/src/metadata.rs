//! Business-neutral authority records stored in the root special tables.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{Digest, NodeId, PathHash, TableId, TypeId};
use crate::layout::{StorageKind, TableLocator};
use crate::path::TablePath;
use crate::special::{fixed_ids_or_empty, schema, u32s, u64s};
use arrow::array::{
    Array, ArrayRef, BooleanArray, FixedSizeBinaryArray, StringArray, UInt32Array, UInt64Array,
};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;
use xxhash_rust::xxh3::xxh3_128;

/// Authority record for one published table instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableMetadata {
    /// Canonical slash-delimited path text.
    pub path: TablePath,
    /// Fast path hash recorded in the ABI.
    pub path_hash: PathHash,
    /// Whether the path hash is degraded after collision detection.
    pub degraded: bool,
    /// Stable physical table identity.
    pub table_id: TableId,
    /// Declared table type identity.
    pub type_id: TypeId,
    /// Declared table type version.
    pub type_version: u32,
    /// Canonical table digest.
    pub table_digest: Digest,
    /// Digest algorithm identifier.
    pub hash_algo: String,
    /// Concrete storage kind.
    pub storage_kind: StorageKind,
    /// Physical offset in append-only layouts.
    pub offset: u64,
    /// Physical byte length in append-only layouts.
    pub length: u64,
    /// Optional owner label supplied by higher metadata layers.
    pub owner_label: Option<String>,
    /// Optional physical path for directory-orientated layouts.
    pub storage_path: Option<String>,
}

impl TableMetadata {
    /// Builds a locator from the metadata fields.
    pub fn locator(&self) -> TableLocator {
        TableLocator {
            table_id: self.table_id,
            offset: self.offset,
            length: self.length,
            storage_kind: self.storage_kind.clone(),
            path: self.storage_path.as_ref().map(PathBuf::from),
        }
    }
}

use std::path::PathBuf;

/// Authority record for one column digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnDigestRecord {
    /// Canonical table path.
    pub path: TablePath,
    /// Logical column order.
    pub col_order: u32,
    /// Column digest.
    pub digest: Digest,
}

/// Authority record for one domain node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainMetadata {
    /// Stable node identity.
    pub node_id: NodeId,
    /// Canonical node path.
    pub path: TablePath,
    /// Declared domain type identity.
    pub type_id: TypeId,
    /// Parent node identity, or the zero node for the root.
    pub parent_node_id: NodeId,
    /// Optional child order within the parent.
    pub child_order: Option<u32>,
    /// Canonical domain digest.
    pub domain_digest: Digest,
}

/// Deterministically derives a stable physical table identity from its path.
pub fn table_id_for_path(path: &TablePath) -> TableId {
    TableId::from_bytes(xxh3_128(path.as_str().as_bytes()).to_be_bytes())
}

/// Parses the `table-metadata` special table into records.
pub fn parse_table_metadata(batch: &RecordBatch) -> Result<Vec<TableMetadata>> {
    let schema = schema("table-metadata")?;
    if batch.schema().as_ref() != schema.as_ref() {
        return Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "table-metadata schema does not match ABI",
        ));
    }
    let paths = downcast::<StringArray>(batch, 0, "path")?;
    let hashes = downcast::<FixedSizeBinaryArray>(batch, 1, "path_hash")?;
    let degraded = downcast::<BooleanArray>(batch, 2, "degraded")?;
    let table_ids = downcast::<FixedSizeBinaryArray>(batch, 3, "table_id")?;
    let type_ids = downcast::<FixedSizeBinaryArray>(batch, 4, "type_id")?;
    let versions = downcast::<UInt32Array>(batch, 5, "type_version")?;
    let digests = downcast::<FixedSizeBinaryArray>(batch, 6, "table_digest")?;
    let algos = downcast::<StringArray>(batch, 7, "hash_algo")?;
    let kinds = downcast::<StringArray>(batch, 8, "storage_kind")?;
    let offsets = downcast::<UInt64Array>(batch, 9, "offset")?;
    let lengths = downcast::<UInt64Array>(batch, 10, "length")?;
    let owners = downcast::<StringArray>(batch, 11, "owner_label")?;
    let storage_paths = downcast::<StringArray>(batch, 12, "storage_path")?;
    let mut records = Vec::new();
    for index in 0..batch.num_rows() {
        records.push(TableMetadata {
            path: TablePath::parse(paths.value(index))?,
            path_hash: PathHash(u128::from_be_bytes(
                hashes.value(index).try_into().expect("16-byte hash"),
            )),
            degraded: degraded.value(index),
            table_id: TableId::from_bytes(table_ids.value(index).try_into().expect("16-byte id")),
            type_id: TypeId::from_bytes(type_ids.value(index).try_into().expect("16-byte id")),
            type_version: versions.value(index),
            table_digest: Digest::from_bytes(
                digests.value(index).try_into().expect("16-byte digest"),
            ),
            hash_algo: algos.value(index).to_owned(),
            storage_kind: parse_storage_kind(kinds.value(index))?,
            offset: offsets.value(index),
            length: lengths.value(index),
            owner_label: if owners.is_null(index) {
                None
            } else {
                Some(owners.value(index).to_owned())
            },
            storage_path: if storage_paths.is_null(index) {
                None
            } else {
                Some(storage_paths.value(index).to_owned())
            },
        });
    }
    Ok(records)
}

/// Encodes table metadata records into the `table-metadata` special batch.
pub fn encode_table_metadata(records: &[TableMetadata]) -> Result<RecordBatch> {
    let mut paths = Vec::new();
    let mut hashes = Vec::new();
    let mut degraded = Vec::new();
    let mut ids = Vec::new();
    let mut type_ids = Vec::new();
    let mut versions = Vec::new();
    let mut digests = Vec::new();
    let mut algos = Vec::new();
    let mut kinds = Vec::new();
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    let mut owners: Vec<Option<String>> = Vec::new();
    let mut storage_paths: Vec<Option<String>> = Vec::new();
    for record in records {
        paths.push(record.path.as_str());
        hashes.push(record.path_hash.0.to_be_bytes());
        degraded.push(record.degraded);
        ids.push(record.table_id.as_bytes());
        type_ids.push(record.type_id.as_bytes());
        versions.push(record.type_version);
        digests.push(record.table_digest.as_bytes());
        algos.push(record.hash_algo.clone());
        kinds.push(storage_kind_name(record.storage_kind.clone()).to_owned());
        offsets.push(record.offset);
        lengths.push(record.length);
        owners.push(record.owner_label.clone());
        storage_paths.push(record.storage_path.clone());
    }
    RecordBatch::try_new(
        schema("table-metadata")?,
        vec![
            Arc::new(StringArray::from(paths)) as ArrayRef,
            Arc::new(fixed_ids_or_empty(hashes)),
            Arc::new(BooleanArray::from(degraded)),
            Arc::new(fixed_ids_or_empty(ids)),
            Arc::new(fixed_ids_or_empty(type_ids)),
            Arc::new(u32s(versions)),
            Arc::new(fixed_ids_or_empty(digests)),
            Arc::new(StringArray::from(algos)),
            Arc::new(StringArray::from(kinds)),
            Arc::new(u64s(offsets)),
            Arc::new(u64s(lengths)),
            Arc::new(StringArray::from(
                owners
                    .iter()
                    .map(|value| value.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                storage_paths
                    .iter()
                    .map(|value| value.as_deref())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|error| {
        TreeSpaceError::new(ErrorCode::ManifestInvalid, "cannot encode table-metadata")
            .with_context("detail", error.to_string())
    })
}

/// Replaces or inserts one table metadata record by canonical path.
pub fn upsert_table_metadata(batch: &RecordBatch, record: &TableMetadata) -> Result<RecordBatch> {
    let mut records = parse_table_metadata(batch)?;
    records.retain(|existing| existing.path != record.path);
    records.push(record.clone());
    records.sort_by(|left, right| left.path.cmp(&right.path));
    encode_table_metadata(&records)
}

/// Parses the `column-digests` special table into records.
pub fn parse_column_digests(batch: &RecordBatch) -> Result<Vec<ColumnDigestRecord>> {
    let expected = schema("column-digests")?;
    if batch.schema().as_ref() != expected.as_ref() {
        return Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "column-digests schema does not match ABI",
        ));
    }
    let paths = downcast::<StringArray>(batch, 0, "path")?;
    let orders = downcast::<UInt32Array>(batch, 1, "col_order")?;
    let digests = downcast::<FixedSizeBinaryArray>(batch, 2, "digest")?;
    let mut records = Vec::new();
    for index in 0..batch.num_rows() {
        records.push(ColumnDigestRecord {
            path: TablePath::parse(paths.value(index))?,
            col_order: orders.value(index),
            digest: Digest::from_bytes(digests.value(index).try_into().expect("16-byte digest")),
        });
    }
    Ok(records)
}

/// Encodes column digest records into the `column-digests` special batch.
pub fn encode_column_digests(records: &[ColumnDigestRecord]) -> Result<RecordBatch> {
    let mut paths = Vec::new();
    let mut orders = Vec::new();
    let mut digests = Vec::new();
    for record in records {
        paths.push(record.path.as_str());
        orders.push(record.col_order);
        digests.push(record.digest.as_bytes());
    }
    RecordBatch::try_new(
        schema("column-digests")?,
        vec![
            Arc::new(StringArray::from(paths)) as ArrayRef,
            Arc::new(u32s(orders)),
            Arc::new(fixed_ids_or_empty(digests)),
        ],
    )
    .map_err(|error| {
        TreeSpaceError::new(ErrorCode::ManifestInvalid, "cannot encode column-digests")
            .with_context("detail", error.to_string())
    })
}

/// Replaces column digest records for the given path.
pub fn put_column_digests(
    batch: &RecordBatch,
    path: &TablePath,
    records: &[ColumnDigestRecord],
) -> Result<RecordBatch> {
    let mut all = parse_column_digests(batch)?;
    all.retain(|record| &record.path != path);
    let mut records = records.to_vec();
    records.sort_by_key(|record| record.col_order);
    all.extend(records);
    all.sort_by(|left, right| {
        (left.path.as_str(), left.col_order).cmp(&(right.path.as_str(), right.col_order))
    });
    encode_column_digests(&all)
}

/// Parses the `domain-metadata` special table into records.
pub fn parse_domain_metadata(batch: &RecordBatch) -> Result<Vec<DomainMetadata>> {
    let expected = schema("domain-metadata")?;
    if batch.schema().as_ref() != expected.as_ref() {
        return Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "domain-metadata schema does not match ABI",
        ));
    }
    let node_ids = downcast::<FixedSizeBinaryArray>(batch, 0, "node_id")?;
    let paths = downcast::<StringArray>(batch, 1, "path")?;
    let type_ids = downcast::<FixedSizeBinaryArray>(batch, 2, "type_id")?;
    let parents = downcast::<FixedSizeBinaryArray>(batch, 3, "parent_node_id")?;
    let orders = downcast::<UInt32Array>(batch, 4, "child_order")?;
    let digests = downcast::<FixedSizeBinaryArray>(batch, 5, "domain_digest")?;
    let mut records = Vec::new();
    for index in 0..batch.num_rows() {
        records.push(DomainMetadata {
            node_id: NodeId::from_bytes(node_ids.value(index).try_into().expect("16-byte id")),
            path: TablePath::parse(paths.value(index))?,
            type_id: TypeId::from_bytes(type_ids.value(index).try_into().expect("16-byte id")),
            parent_node_id: NodeId::from_bytes(
                parents.value(index).try_into().expect("16-byte id"),
            ),
            child_order: if orders.is_null(index) {
                None
            } else {
                Some(orders.value(index))
            },
            domain_digest: Digest::from_bytes(
                digests.value(index).try_into().expect("16-byte digest"),
            ),
        });
    }
    Ok(records)
}

/// Encodes domain metadata records into the `domain-metadata` special batch.
pub fn encode_domain_metadata(records: &[DomainMetadata]) -> Result<RecordBatch> {
    let mut node_ids = Vec::new();
    let mut paths = Vec::new();
    let mut type_ids = Vec::new();
    let mut parents = Vec::new();
    let mut orders: Vec<Option<u32>> = Vec::new();
    let mut digests = Vec::new();
    for record in records {
        node_ids.push(record.node_id.as_bytes());
        paths.push(record.path.as_str());
        type_ids.push(record.type_id.as_bytes());
        parents.push(record.parent_node_id.as_bytes());
        orders.push(record.child_order);
        digests.push(record.domain_digest.as_bytes());
    }
    RecordBatch::try_new(
        schema("domain-metadata")?,
        vec![
            Arc::new(fixed_ids_or_empty(node_ids)) as ArrayRef,
            Arc::new(StringArray::from(paths)),
            Arc::new(fixed_ids_or_empty(type_ids)),
            Arc::new(fixed_ids_or_empty(parents)),
            Arc::new(UInt32Array::from(orders)),
            Arc::new(fixed_ids_or_empty(digests)),
        ],
    )
    .map_err(|error| {
        TreeSpaceError::new(ErrorCode::ManifestInvalid, "cannot encode domain-metadata")
            .with_context("detail", error.to_string())
    })
}

/// Replaces or inserts one domain metadata record by node identity.
pub fn upsert_domain_metadata(batch: &RecordBatch, record: &DomainMetadata) -> Result<RecordBatch> {
    let mut records = parse_domain_metadata(batch)?;
    records.retain(|existing| existing.node_id != record.node_id);
    records.push(record.clone());
    records.sort_by(|left, right| left.path.cmp(&right.path));
    encode_domain_metadata(&records)
}

/// Returns a stable collection of table metadata keyed by path.
pub fn table_metadata_map(batch: &RecordBatch) -> Result<BTreeMap<TablePath, TableMetadata>> {
    Ok(parse_table_metadata(batch)?
        .into_iter()
        .map(|record| (record.path.clone(), record))
        .collect())
}

fn parse_storage_kind(value: &str) -> Result<StorageKind> {
    match value {
        "memory" => Ok(StorageKind::Memory),
        "single_file" => Ok(StorageKind::SingleFile),
        "flat_dir" => Ok(StorageKind::FlatDir),
        _ => Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "unknown storage_kind in table-metadata",
        )
        .with_context("storage_kind", value)),
    }
}

fn storage_kind_name(kind: StorageKind) -> &'static str {
    match kind {
        StorageKind::Memory => "memory",
        StorageKind::SingleFile => "single_file",
        StorageKind::FlatDir => "flat_dir",
    }
}

fn downcast<'a, T: arrow::array::Array + 'static>(
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
                "metadata column has incorrect type",
            )
            .with_context("column", name)
        })
}
