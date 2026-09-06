//! Canonical XXH3-128 Arrow IPC digest and namespace Merkle helpers.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{Digest, NodeId, TypeId};
use crate::ipc::encode_batch;
use crate::types::ColumnDefinition;
use arrow::array::{Array, ArrayRef, BooleanArray, PrimitiveArray};
use arrow::buffer::{BooleanBuffer, ScalarBuffer};
use arrow::compute::concat;
use arrow::datatypes::Schema;
use arrow::datatypes::{
    ArrowPrimitiveType, DataType, Date32Type, Date64Type, Int8Type, Int16Type, Int32Type,
    Int64Type, UInt8Type, UInt16Type, UInt32Type, UInt64Type,
};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use xxhash_rust::xxh3::xxh3_128;

/// Returns a canonical copy of a primitive array with every null slot's data
/// buffer value zeroed.
///
/// Arrow does not define the physical value stored in a null slot, and different
/// producers under-fill it differently (for example IPC writes zeros while a
/// Parquet round trip may leave the recycled reader buffer untouched). The
/// canonical table digest hashes the IPC encoding of the column, so this method
/// normalizes null slots before encoding to keep the digest independent of the
/// storage path taken to produce the array.
///
/// `pub(crate)`: reused by the v5 leaf layer (`Table` leaf) to keep its content
/// hash stable across producer round trips.
pub(crate) fn canonicalize_null_slots(array: &ArrayRef) -> ArrayRef {
    if array.null_count() == 0 {
        return array.clone();
    }
    match array.data_type() {
        DataType::Int8 => canonical_primitive::<Int8Type>(array),
        DataType::Int16 => canonical_primitive::<Int16Type>(array),
        DataType::Int32 => canonical_primitive::<Int32Type>(array),
        DataType::Int64 => canonical_primitive::<Int64Type>(array),
        DataType::UInt8 => canonical_primitive::<UInt8Type>(array),
        DataType::UInt16 => canonical_primitive::<UInt16Type>(array),
        DataType::UInt32 => canonical_primitive::<UInt32Type>(array),
        DataType::UInt64 => canonical_primitive::<UInt64Type>(array),
        DataType::Date32 => canonical_primitive::<Date32Type>(array),
        DataType::Date64 => canonical_primitive::<Date64Type>(array),
        DataType::Float32 => canonical_primitive::<arrow::datatypes::Float32Type>(array),
        DataType::Float64 => canonical_primitive::<arrow::datatypes::Float64Type>(array),
        DataType::Boolean => canonical_boolean(array),
        // String, binary, list, and struct columns carry their null semantics in
        // offsets/lengths that are already canonical across producer round trips.
        _ => array.clone(),
    }
}

fn canonical_primitive<T: ArrowPrimitiveType>(array: &ArrayRef) -> ArrayRef
where
    T::Native: Copy + Default,
{
    let Some(values) = array.as_any().downcast_ref::<PrimitiveArray<T>>() else {
        return array.clone();
    };
    let mut slots = values.values().iter().copied().collect::<Vec<_>>();
    if let Some(nulls) = values.nulls() {
        for index in 0..values.len() {
            if nulls.is_null(index) {
                slots[index] = T::Native::default();
            }
        }
    }
    Arc::new(PrimitiveArray::<T>::new(
        ScalarBuffer::from(slots),
        values.nulls().cloned(),
    )) as ArrayRef
}

fn canonical_boolean(array: &ArrayRef) -> ArrayRef {
    let Some(values) = array.as_any().downcast_ref::<BooleanArray>() else {
        return array.clone();
    };
    let mut bits = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        bits.push(!values.is_null(index) && values.value(index));
    }
    Arc::new(BooleanArray::new(
        BooleanBuffer::from(bits),
        values.nulls().cloned(),
    )) as ArrayRef
}

/// Canonically hashes length-delimited parts under a domain tag.
pub fn canonical_digest(tag: &[u8], parts: impl IntoIterator<Item = Vec<u8>>) -> Digest {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(tag.len() as u64).to_le_bytes());
    bytes.extend_from_slice(tag);
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&part);
    }
    Digest(xxh3_128(&bytes).to_be_bytes())
}

/// Computes a digest for a one-column canonical Arrow IPC record batch.
pub fn column_digest(
    type_id: TypeId,
    version: u32,
    definition: &ColumnDefinition,
    values: ArrayRef,
) -> Result<Digest> {
    let field = definition.field()?;
    if field.data_type() != values.data_type() {
        return Err(TreeSpaceError::new(
            ErrorCode::SchemaMismatch,
            "column values do not match declared Arrow field",
        )
        .with_context("column", definition.name.to_string()));
    }
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![field])),
        vec![canonicalize_null_slots(&values)],
    )
    .map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::SchemaMismatch,
            "cannot construct canonical one-column batch",
        )
        .with_context("detail", error.to_string())
    })?;
    Ok(canonical_digest(
        b"column",
        vec![
            type_id.as_bytes().to_vec(),
            version.to_le_bytes().to_vec(),
            definition.col_order.to_le_bytes().to_vec(),
            definition.field_ipc.clone(),
            encode_batch(&batch)?,
        ],
    ))
}

/// Computes a column digest across multiple batches with split-independent semantics.
pub fn column_digest_multi(
    type_id: TypeId,
    version: u32,
    definition: &ColumnDefinition,
    batches: &[RecordBatch],
) -> Result<Digest> {
    if batches.is_empty() {
        return Err(TreeSpaceError::new(
            ErrorCode::RequiredDataMissing,
            "cannot digest a column with no batches",
        ));
    }
    let field = definition.field()?;
    let mut arrays: Vec<&dyn arrow::array::Array> = Vec::with_capacity(batches.len());
    for batch in batches {
        let column = batch.column(definition.col_order as usize);
        if column.data_type() != &field.data_type().clone() {
            return Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "column values do not match declared Arrow field",
            )
            .with_context("column", definition.name.to_string()));
        }
        arrays.push(column.as_ref());
    }
    let array = concat(&arrays).map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "cannot concatenate record batches for canonical digest",
        )
        .with_context("detail", error.to_string())
    })?;
    column_digest(type_id, version, definition, array)
}

/// Computes a table digest from already canonical column digests in column-order.
pub fn table_digest(
    type_id: TypeId,
    version: u32,
    columns: impl IntoIterator<Item = (u32, Digest)>,
) -> Digest {
    let mut columns = columns.into_iter().collect::<Vec<_>>();
    columns.sort_by_key(|(order, _)| *order);
    let mut parts = vec![type_id.as_bytes().to_vec(), version.to_le_bytes().to_vec()];
    for (order, digest) in columns {
        let mut part = order.to_le_bytes().to_vec();
        part.extend_from_slice(&digest.as_bytes());
        parts.push(part);
    }
    canonical_digest(b"table", parts)
}

/// Computes a domain digest independently of child registration order.
pub fn domain_digest(
    node_id: NodeId,
    children: impl IntoIterator<Item = (u32, u8, String, Digest)>,
) -> Digest {
    let mut children = children.into_iter().collect::<Vec<_>>();
    children.sort_by(|left, right| (left.0, left.1, &left.2).cmp(&(right.0, right.1, &right.2)));
    let mut parts = vec![node_id.as_bytes().to_vec()];
    for (_, kind, name, digest) in children {
        let mut part = vec![kind];
        part.extend_from_slice(name.as_bytes());
        part.extend_from_slice(&digest.as_bytes());
        parts.push(part);
    }
    canonical_digest(b"domain", parts)
}

/// Returns the ABI constant digest for a domain with no children.
///
/// This is the `canonical_digest(b"domain", [])` constant (zero child parts),
/// fully consistent with the domain formula; it is not a separate literal.
pub fn empty_domain_digest() -> Digest {
    canonical_digest(b"domain", Vec::<Vec<u8>>::new())
}

/// Computes the storage content hash of a full table (all columns, including
/// `in_hash=false`). Used only for content-addressed blob addressing in
/// FlatDir; it is distinct from the semantic Merkle `table_digest`.
///
/// Each "完整列" is the single-column canonical IPC of the column concatenated
/// across all batches, so the result is RecordBatch-split independent and the
/// writer computes it in memory (never by reading back file bytes).
pub fn table_content_hash(
    type_id: TypeId,
    version: u32,
    columns: &[crate::types::ColumnDefinition],
    batches: &[RecordBatch],
) -> Result<Digest> {
    if batches.is_empty() {
        return Err(TreeSpaceError::new(
            ErrorCode::RequiredDataMissing,
            "cannot content-hash a table with no batches",
        ));
    }
    let mut parts = vec![type_id.as_bytes().to_vec(), version.to_le_bytes().to_vec()];
    for definition in columns {
        let field = definition.field()?;
        let mut arrays: Vec<&dyn arrow::array::Array> = Vec::with_capacity(batches.len());
        for batch in batches {
            let column = batch.column(definition.col_order as usize);
            if column.data_type() != field.data_type() {
                return Err(TreeSpaceError::new(
                    ErrorCode::SchemaMismatch,
                    "content-hash column values do not match declared Arrow field",
                )
                .with_context("column", definition.name.to_string()));
            }
            arrays.push(column.as_ref());
        }
        let array = concat(&arrays).map_err(|error| {
            TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "cannot concatenate record batches for content hash",
            )
            .with_context("detail", error.to_string())
        })?;
        let one_batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![field])),
            vec![canonicalize_null_slots(&array)],
        )
        .map_err(|error| {
            TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "cannot construct one-column batch for content hash",
            )
            .with_context("detail", error.to_string())
        })?;
        let mut part = definition.col_order.to_le_bytes().to_vec();
        part.extend_from_slice(&encode_batch(&one_batch)?);
        parts.push(part);
    }
    Ok(canonical_digest(b"content", parts))
}
