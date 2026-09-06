//! Digest golden constants (protocol freeze).

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::*;

// Frozen canonical_digest outputs. Any change to the Arrow encoding, the
// canonical_digest framing, or the empty-domain formula must update these.
const EMPTY_DOMAIN: [u8; 16] = [
    0x85, 0x56, 0x52, 0x51, 0x12, 0x97, 0x08, 0x4e, 0xa7, 0x90, 0xa6, 0x1b, 0xe3, 0x64, 0x1b, 0x2e,
];
const CONTENT_HASH: [u8; 16] = [
    0x20, 0xab, 0x16, 0xde, 0x0e, 0x54, 0xcc, 0x59, 0x5b, 0x0c, 0x34, 0xcb, 0xbd, 0x81, 0x8f, 0x44,
];

#[test]
fn empty_domain_is_frozen_constant() {
    assert_eq!(
        tree_space::hash::empty_domain_digest().as_bytes(),
        EMPTY_DOMAIN
    );
    // The constant must be exactly the canonical zero-part domain digest.
    assert_eq!(
        tree_space::hash::empty_domain_digest(),
        tree_space::hash::canonical_digest(b"domain", Vec::<Vec<u8>>::new())
    );
}

#[test]
fn content_hash_is_frozen_and_split_independent() {
    let type_id = TypeId::from_bytes([7; 16]);
    let column = ColumnDefinition::from_field(0, Field::new("value", DataType::Int32, false), true);
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    let whole = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int32Array::from(vec![1, 2, 3]))],
    )
    .unwrap();
    let split_a =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(Int32Array::from(vec![1, 2]))]).unwrap();
    let split_b =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(Int32Array::from(vec![3]))]).unwrap();

    let whole_hash =
        tree_space::hash::table_content_hash(type_id, 1, &[column.clone()], &[whole]).unwrap();
    // Split-independent: re-splitting the same logical column yields the same hash.
    let split_hash =
        tree_space::hash::table_content_hash(type_id, 1, &[column], &[split_a, split_b]).unwrap();

    assert_eq!(whole_hash.as_bytes(), CONTENT_HASH);
    assert_eq!(whole_hash, split_hash);
}
