//! A2-1: flat-entry-table codec core — universal image round trips and the
//! frozen tree identities (golden 11–13) plus the §5.4 negative checklist.

use arrow::array::{StringArray, UInt8Array, UInt64Array};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, TreeImage, decode, encode, tree_id, tree_schema};
use tree_space::tree::codec::{keyed_field, named_field, positioned_field};
use tree_space::{BlockKind, Decimal128, RefId, Time32Unit, TimestampUnit, Value};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

#[test]
fn a2_1_schema_columns_and_types_are_frozen() {
    let schema = tree_schema();
    let names: Vec<_> = schema
        .fields()
        .iter()
        .map(|field| field.name().as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "parent",
            "name",
            "key16",
            "kind",
            "value",
            "ref",
            "block_kind",
            "combinator"
        ]
    );
    use arrow::datatypes::DataType;
    assert_eq!(schema.field(0).data_type(), &DataType::UInt64);
    assert_eq!(schema.field(1).data_type(), &DataType::Utf8);
    assert_eq!(schema.field(2).data_type(), &DataType::FixedSizeBinary(16));
    assert_eq!(schema.field(3).data_type(), &DataType::UInt8);
    let DataType::Union(fields, mode) = schema.field(4).data_type() else {
        panic!("value column must be a union");
    };
    assert_eq!(fields.len(), 22);
    assert_eq!(mode, &arrow::datatypes::UnionMode::Dense);
    assert_eq!(schema.field(5).data_type(), &DataType::FixedSizeBinary(16));
    assert_eq!(schema.field(6).data_type(), &DataType::Utf8);
    assert_eq!(schema.field(7).data_type(), &DataType::Utf8);
}

#[test]
fn a2_1_empty_tree_id_is_frozen() {
    // Golden 11 (PL-2 M2 re-freeze): the empty tree (single root row), identity
    // over the semantic walk of the decoded image.
    let bytes = encode(&TreeImage::new_empty()).unwrap();
    assert!(bytes.starts_with(b"ARROW1"));
    assert_eq!(
        tree_id(&bytes),
        tree_space::TreeId::from_bytes(hex16("b421e79e3e8369989a2b2efcf5c08a79"))
    );
}

#[test]
fn a2_1_inline_scalar_tree_id_is_frozen() {
    // Golden 12: inline scalars across many Value kinds, including the
    // parameter-bag parameterized kinds.
    let children = vec![
        named_field("bool", ImageContent::Inline(Value::Bool(true))),
        named_field("i64", ImageContent::Inline(Value::I64(-4))),
        named_field(
            "time32",
            ImageContent::Inline(Value::Time32(Time32Unit::Millisecond, 822)),
        ),
        named_field(
            "stamp",
            ImageContent::Inline(Value::Timestamp(
                TimestampUnit::Nanosecond,
                Some("UTC".into()),
                7,
            )),
        ),
        named_field(
            "decimal",
            ImageContent::Inline(Value::Decimal128(Decimal128 {
                precision: 10,
                scale: -2,
                value: [4; 16],
            })),
        ),
    ];
    let image = TreeImage::new(children);
    let bytes = encode(&image).unwrap();
    assert_eq!(
        tree_id(&bytes),
        tree_space::TreeId::from_bytes(hex16("80d36a83355b79dfbb61eb521922e6fc"))
    );
}

#[test]
fn a2_1_ref_tree_and_chunk_group_tree_id_is_frozen() {
    // Golden 13: block-reference slots plus a chunk group (nested group,
    // order preserved, stats absent from canonical bytes).
    let ref_a = RefId::from_bytes(hex16("11111111111111111111111111111111"));
    let ref_b = RefId::from_bytes(hex16("22222222222222222222222222222222"));
    let ref_c = RefId::from_bytes(hex16("33333333333333333333333333333333"));
    let children = vec![
        named_field("block", ImageContent::Ref(ref_a)),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(ref_b)),
                    positioned_field(ImageContent::ChunkGroup {
                        kind: BlockKind::Table,
                        children: vec![positioned_field(ImageContent::ChunkEntry(ref_c))],
                    }),
                ],
            },
        ),
    ];
    let image = TreeImage::new(children);
    let bytes = encode(&image).unwrap();
    assert_eq!(
        tree_id(&bytes),
        tree_space::TreeId::from_bytes(hex16("83c6aaaaa138687ebe1a9fd04957f071"))
    );
}

#[test]
fn a2_1_decode_rejects_truncated_or_bad_magic_bytes() {
    assert!(decode(b"").is_err());
    assert!(decode(b"\x00\x01\x02\x03").is_err());
    assert!(decode(b"ARROW1").is_err());
}

#[test]
fn a2_1_decode_rejects_schema_mismatch() {
    let image = TreeImage::new(vec![named_field("x", ImageContent::Inline(Value::I32(1)))]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();
    let schema = arrow::datatypes::Schema::new(
        batch
            .schema()
            .fields()
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let name = if index == 3 { "kend" } else { field.name() };
                Arc::new(field.as_ref().clone().with_name(name))
            })
            .collect::<Vec<_>>(),
    );
    let bad = RecordBatch::try_new(Arc::new(schema), batch.columns().to_vec()).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "schema with a renamed column must be rejected"
    );
}

#[test]
fn a2_1_decode_rejects_non_sentinel_parent_and_dangling_anchor() {
    let image = TreeImage::new(vec![named_field(
        "child",
        ImageContent::Inline(Value::I32(1)),
    )]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();

    let bad_parent = UInt64Array::from(vec![u64::MAX, 1]);
    let bad = rebuild_parent(&batch, bad_parent);
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "child parent must be < self"
    );

    let dangling = UInt64Array::from(vec![u64::MAX, 99]);
    let bad = rebuild_parent(&batch, dangling);
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "dangling parent must be rejected"
    );

    let sentinel = UInt64Array::from(vec![u64::MAX, u64::MAX]);
    let bad = rebuild_parent(&batch, sentinel);
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "a non-root sentinel row must be rejected"
    );
}

fn rebuild_parent(batch: &RecordBatch, parent: UInt64Array) -> RecordBatch {
    let mut columns = batch.columns().to_vec();
    columns[0] = Arc::new(parent);
    RecordBatch::try_new(batch.schema().clone(), columns).unwrap()
}

#[test]
fn a2_1_decode_rejects_duplicate_or_unsorted_named_siblings() {
    let image = TreeImage::new(vec![
        named_field("a", ImageContent::Inline(Value::I32(1))),
        named_field("b", ImageContent::Inline(Value::I32(2))),
    ]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();

    let swapped: Vec<Option<&str>> = vec![None, Some("b"), Some("a")];
    let mut columns = batch.columns().to_vec();
    columns[1] = Arc::new(StringArray::from(swapped));
    let bad = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "unsorted named siblings must be rejected"
    );

    let dup: Vec<Option<&str>> = vec![None, Some("a"), Some("a")];
    let mut columns = batch.columns().to_vec();
    columns[1] = Arc::new(StringArray::from(dup));
    let bad = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "duplicate named siblings must be rejected"
    );
}

#[test]
fn a2_1_decode_rejects_root_kind_not_node() {
    let image = TreeImage::new(vec![named_field("x", ImageContent::Inline(Value::I32(1)))]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();
    let mut columns = batch.columns().to_vec();
    columns[3] = Arc::new(UInt8Array::from(vec![2, 2]));
    let bad = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "root row must be a Node"
    );
}

#[test]
fn a2_1_decode_rejects_name_and_key16_together() {
    let image = TreeImage::new(vec![named_field("x", ImageContent::Inline(Value::I32(1)))]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();
    let mut columns = batch.columns().to_vec();
    // Root row carries no locator; the named child also gets a key16.
    columns[2] = Arc::new(
        arrow::array::FixedSizeBinaryArray::try_from_sparse_iter_with_size(
            vec![None::<&[u8]>, Some([0u8; 16].as_slice())].into_iter(),
            16,
        )
        .unwrap(),
    );
    let bad = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "name and key16 are mutually exclusive"
    );
}

#[test]
fn a2_1_decode_rejects_keyed_siblings_out_of_order() {
    let image = TreeImage::new(vec![
        keyed_field([0x01; 16], ImageContent::Inline(Value::I32(1))),
        keyed_field([0x02; 16], ImageContent::Inline(Value::I32(2))),
    ]);
    let bytes = encode(&image).unwrap();
    let batch = tree_space::ipc::decode_batch(&bytes).unwrap();
    let mut columns = batch.columns().to_vec();
    let swapped = vec![
        None::<&[u8]>,
        Some([0x02; 16].as_slice()),
        Some([0x01; 16].as_slice()),
    ];
    columns[2] = Arc::new(
        arrow::array::FixedSizeBinaryArray::try_from_sparse_iter_with_size(swapped.into_iter(), 16)
            .unwrap(),
    );
    let bad = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    assert!(
        decode(&tree_space::ipc::encode_batch(&bad).unwrap()).is_err(),
        "keyed siblings must be strictly increasing"
    );
}
