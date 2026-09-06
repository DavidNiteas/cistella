//! PL-2 S1: semantic fingerprint machinery + golden freeze
//! (`_dev/插件化改造/02-施工路线图.md` §6.3).
//!
//! Three anchors:
//!
//! - `p_l_2_semantic_value_roundtrip`: `SemanticValue` scalar encoding is
//!   frozen (golden 28) and the self-describing length-prefixed stream
//!   decodes back exactly (including the `-0.0`/NaN canonicalization);
//! - `p_l_2_semantic_table_fingerprint_frozen`: the sample table's coarse
//!   logical-type fingerprint is frozen (golden 29), and adding/removing
//!   Arrow's own metadata (field metadata, schema metadata) changes nothing;
//! - `p_l_2_semantic_tree_walk_frozen`: the sample tree's deterministic walk
//!   is frozen (golden 30), and an `order_children`-equivalent reshuffle of
//!   the same logical tree produces the same fingerprint.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Fields, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use tree_space::plugin::semantic::{SemanticValue, semantic_table, semantic_tree};
use tree_space::tree::codec::{
    ImageContent, ImageField, ImageView, ImageViewInput, TreeImage, named_field, positioned_field,
};
use tree_space::{BlockKind, RefId, Value};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

// ---------------------------------------------------------------------------
// Golden 28 sample values (every family member + nested list)
// ---------------------------------------------------------------------------

fn scalar_samples() -> Vec<SemanticValue> {
    vec![
        SemanticValue::Null,
        SemanticValue::Bool(true),
        SemanticValue::Bool(false),
        SemanticValue::Int(-42),
        SemanticValue::Int(0),
        SemanticValue::UInt(7),
        SemanticValue::Float(3.5),
        SemanticValue::Float(0.0),
        SemanticValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        SemanticValue::Str("héllo".into()),
        SemanticValue::List(vec![
            SemanticValue::Bool(false),
            SemanticValue::Str("nested".into()),
            SemanticValue::List(vec![SemanticValue::Int(9)]),
        ]),
    ]
}

/// Golden 28: the concatenated `SemanticValue::encode` bytes of every scalar
/// sample (a single self-describing stream).
///
/// Layout: `[tag][u32 BE length][payload]` per value; tags are 0x00 null,
/// 0x01 bool, 0x02 int, 0x03 uint, 0x04 float, 0x05 bytes, 0x06 str,
/// 0x07 list. Order of the samples: Null, Bool(true), Bool(false), Int(-42),
/// Int(0), UInt(7), Float(3.5), Float(0.0), Bytes([de ad be ef]),
/// Str("héllo"), List([Bool(false), Str("nested"), List([Int(9)])]).
const GOLDEN_28: &str = "\
00000000000100000001010100000001000200000008ffffffffffffffd6\
020000000800000000000000000300000008000000000000000704000000\
08400c000000000000040000000800000000000000000500000004deadbeef\
060000000668c3a96c6c6f070000002301000000010006000000066e657374\
6564070000000d02000000080000000000000009";

#[test]
fn p_l_2_semantic_value_roundtrip() {
    let samples = scalar_samples();
    let stream = samples
        .iter()
        .flat_map(|value| value.encode())
        .collect::<Vec<u8>>();
    assert_eq!(
        hex(&stream),
        GOLDEN_28,
        "golden 28: SemanticValue scalar encode bytes"
    );

    // Self-describing decode: every sample decodes exactly, consuming its
    // declared bytes, and re-encodes byte-stably.
    let mut cursor = 0;
    for want in &samples {
        let (got, used) = SemanticValue::decode(&stream[cursor..])
            .unwrap_or_else(|error| panic!("sample {want:?} decodes: {error}"));
        assert_eq!(&got, want, "decoded value equals the sample");
        assert_eq!(got.encode(), want.encode(), "re-encode is byte-stable");
        cursor += used;
    }
    assert_eq!(cursor, stream.len(), "the stream is fully consumed");

    // Float canonicalization: -0.0 == 0.0 and every NaN payload maps to the
    // canonical NaN bit pattern.
    assert_eq!(
        SemanticValue::Float(-0.0).encode(),
        SemanticValue::Float(0.0).encode()
    );
    let noise_nan = f64::from_bits(0x7ff8_1234_5678_9012);
    let signaling_nan = f64::from_bits(0x7ff0_0000_0000_0034);
    assert_eq!(
        SemanticValue::Float(noise_nan).encode(),
        SemanticValue::Float(signaling_nan).encode(),
        "any NaN payload follows one shared encoding"
    );

    // Malformed streams are rejected with PayloadMalformed.
    assert!(SemanticValue::decode(&[]).is_err());
    assert!(
        SemanticValue::decode(&[0x02]).is_err(),
        "missing length prefix"
    );
    assert!(
        SemanticValue::decode(&[0x02, 0, 0, 0, 9]).is_err(),
        "truncated int payload"
    );
    assert!(
        SemanticValue::decode(&[0x99, 0, 0, 0, 0]).is_err(),
        "unknown tag"
    );
}

// ---------------------------------------------------------------------------
// Golden 29 sample table (coarse logical type shape)
// ---------------------------------------------------------------------------

fn sample_schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("label", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
        Field::new(
            "when",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        Field::new(
            "tags",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        ),
        Field::new(
            "items",
            DataType::List(Arc::new(Field::new("item", DataType::Int64, false))),
            true,
        ),
        Field::new(
            "payload",
            DataType::Struct(Fields::from(vec![
                Arc::new(Field::new("a", DataType::UInt16, false)),
                Arc::new(Field::new("b", DataType::Binary, true)),
            ])),
            true,
        ),
        Field::new("dec", DataType::Decimal128(10, 2), true),
        Field::new(
            "mapping",
            DataType::Map(
                Arc::new(Field::new(
                    "entries",
                    DataType::Struct(Fields::from(vec![
                        Arc::new(Field::new("key", DataType::Utf8, false)),
                        Arc::new(Field::new("value", DataType::Int16, true)),
                    ])),
                    false,
                )),
                false,
            ),
            true,
        ),
    ])
}

/// Golden 29: `semantic_table` bytes of the sample table's logical shape
/// (fields in schema order; per field: `name`, nullable byte, then type tag +
/// extras; 0x04 int32, 0x0c float64, 0x0d utf8, 0x16 timestamp, 0x19 list,
/// 0x1b struct, 0x17 decimal128, 0x1d map).
const GOLDEN_29: &str = "\
0000000269640004000000056c6162656c010d0000000573636f7265010c0000\
00047768656e01160301000000035554430000000474616773010d000000056974\
656d73011905000000077061796c6f6164011b000000016100070000000162010e\
0000000364656301170a02000000076d617070696e67011d00000000036b657900\
0d0000000576616c75650103";

#[test]
fn p_l_2_semantic_table_fingerprint_frozen() {
    let schema = Arc::new(sample_schema());
    let batch = RecordBatch::new_empty(schema.clone());
    let fingerprint = semantic_table(&batch);
    assert_eq!(
        hex(&fingerprint),
        GOLDEN_29,
        "golden 29: sample table logical-shape fingerprint"
    );

    // Arrow metadata must never enter the fingerprint: add field-level
    // metadata to every field and schema-level metadata, byte-for-byte the
    // same fingerprint.
    let mut metadata_fields = Vec::new();
    for index in 0..schema.fields().len() {
        let field = schema.field(index);
        let mut map = HashMap::new();
        map.insert("unit".to_string(), "count".to_string());
        map.insert("origin".to_string(), "sample".to_string());
        metadata_fields.push(field.clone().with_metadata(map));
    }
    let mut meta_map = HashMap::new();
    meta_map.insert("producer".to_string(), "p_l_2".to_string());
    let meta_schema = Arc::new(Schema::new_with_metadata(metadata_fields, meta_map));
    let meta_batch = RecordBatch::new_empty(meta_schema);
    assert_eq!(
        fingerprint,
        semantic_table(&meta_batch),
        "field/schema metadata are excluded from the logical shape"
    );

    // Nullability and field names stay in the fingerprint (guards against a
    // broken "coarse" over-collapse).
    let widened = Arc::new(Schema::new(
        schema
            .fields()
            .iter()
            .map(|field| field.as_ref().clone().with_nullable(!field.is_nullable()))
            .collect::<Vec<_>>(),
    ));
    assert_ne!(
        fingerprint,
        semantic_table(&RecordBatch::new_empty(widened)),
        "nullability is part of the logical shape"
    );
}

// ---------------------------------------------------------------------------
// Golden 30 sample tree (deterministic walk)
// ---------------------------------------------------------------------------

fn sample_tree() -> TreeImage {
    TreeImage::new(vec![
        named_field("zebra", ImageContent::Inline(Value::Bool(true))),
        named_field("alpha", ImageContent::Ref(RefId::from_bytes([0xaa; 16]))),
        named_field(
            "deep",
            ImageContent::Node(vec![
                named_field("inner", ImageContent::Inline(Value::I64(7))),
                named_field("flag", ImageContent::Inline(Value::Utf8("x".into()))),
            ]),
        ),
        named_field(
            "bytes",
            ImageContent::Inline(Value::Binary(vec![0x01, 0x02])),
        ),
        named_field(
            "stamp",
            ImageContent::Inline(Value::Timestamp(
                tree_space::TimestampUnit::Nanosecond,
                Some("UTC".into()),
                7,
            )),
        ),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x11; 16]))),
                    positioned_field(ImageContent::ChunkGroup {
                        kind: BlockKind::Table,
                        children: vec![positioned_field(ImageContent::ChunkEntry(
                            RefId::from_bytes([0x22; 16]),
                        ))],
                    }),
                ],
            },
        ),
        named_field(
            "view",
            ImageContent::View(ImageView {
                combinator: "join".into(),
                inputs: vec![
                    ImageViewInput::Ref(RefId::from_bytes([0x33; 16])),
                    ImageViewInput::View(Box::new(ImageView {
                        combinator: "select".into(),
                        inputs: vec![ImageViewInput::Ref(RefId::from_bytes([0x33; 16]))],
                        params: vec![
                            ("rows".into(), Value::I32(1)),
                            ("cols".into(), Value::Utf8("id".into())),
                        ],
                    })),
                ],
                params: vec![("mode".into(), Value::Utf8("inner".into()))],
            }),
        ),
    ])
}

/// The same logical tree with a different physical insertion order: named
/// children and view params reshuffled (the codec's `order_children` reorders
/// named children by name and the walker sorts view params), while chunk-group
/// children — positional, thus semantically ordered — stay in place.
fn reshuffled_tree() -> TreeImage {
    TreeImage::new(vec![
        named_field(
            "view",
            ImageContent::View(ImageView {
                combinator: "join".into(),
                inputs: vec![
                    ImageViewInput::Ref(RefId::from_bytes([0x33; 16])),
                    ImageViewInput::View(Box::new(ImageView {
                        combinator: "select".into(),
                        inputs: vec![ImageViewInput::Ref(RefId::from_bytes([0x33; 16]))],
                        params: vec![
                            ("cols".into(), Value::Utf8("id".into())),
                            ("rows".into(), Value::I32(1)),
                        ],
                    })),
                ],
                params: vec![("mode".into(), Value::Utf8("inner".into()))],
            }),
        ),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x11; 16]))),
                    positioned_field(ImageContent::ChunkGroup {
                        kind: BlockKind::Table,
                        children: vec![positioned_field(ImageContent::ChunkEntry(
                            RefId::from_bytes([0x22; 16]),
                        ))],
                    }),
                ],
            },
        ),
        named_field(
            "bytes",
            ImageContent::Inline(Value::Binary(vec![0x01, 0x02])),
        ),
        named_field("alpha", ImageContent::Ref(RefId::from_bytes([0xaa; 16]))),
        named_field(
            "stamp",
            ImageContent::Inline(Value::Timestamp(
                tree_space::TimestampUnit::Nanosecond,
                Some("UTC".into()),
                7,
            )),
        ),
        named_field(
            "deep",
            ImageContent::Node(vec![
                named_field("flag", ImageContent::Inline(Value::Utf8("x".into()))),
                named_field("inner", ImageContent::Inline(Value::I64(7))),
            ]),
        ),
        named_field("zebra", ImageContent::Inline(Value::Bool(true))),
    ])
}

/// Golden 30: `semantic_tree` bytes of the sample tree's deterministic walk
/// (root count 0x00000007, children ordered canonically by name:
/// alpha/bytes/chunks/deep/stamp/view/zebra; locators 0x00 positioned /
/// 0x01 named / 0x02 keyed; contents 0x10 node, 0x11 inline, 0x12 ref,
/// 0x13 chunk-group, 0x14 chunk-entry, 0x15 view).
const GOLDEN_30: &str = "\
000000070100000005616c70686112aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0100\
0000056279746573110500000002010201000000066368756e6b73130000000574\
61626c650000000200141111111111111111111111111111111100130000000574\
61626c650000000100142222222222222222222222222222222201000000046465\
657010000000020100000004666c6167110600000001780100000005696e6e6572\
110200000008000000000000000701000000057374616d70110700000030060000\
000974696d657374616d7003000000080000000000000003060000000355544302\
00000008000000000000000701000000047669657715000000046a6f696e000000\
020033333333333333333333333333333333010000000673656c65637400000001\
00333333333333333333333333333333330000000200000004636f6c7306000000\
02696400000004726f77730200000008000000000000000100000001000000046d\
6f64650600000005696e6e657201000000057a6562726111010000000101";

#[test]
fn p_l_2_semantic_tree_walk_frozen() {
    let walk = semantic_tree(&sample_tree());
    assert_eq!(hex(&walk), GOLDEN_30, "golden 30: sample tree walk");

    // Row-order independence: the reshuffled tree walks identically (chunk
    // group children are positional but the group/entry sets are unchanged;
    // named children reorder canonically; view params sort by name).
    let reshuffled = semantic_tree(&reshuffled_tree());
    assert_eq!(
        walk, reshuffled,
        "same logical tree in a different storage order walks to the same fingerprint"
    );
}

/// Keyed children also reorder canonically by key bytes (order_children
/// path `Some(2)` — the named/keyed sort shares the walker).
#[test]
fn p_l_2_semantic_tree_keyed_order_independent() {
    use tree_space::tree::codec::Locator;
    let a = TreeImage::new(vec![
        ImageField {
            locator: Locator::Keyed(hex16("0000000000000000000000000000000a")),
            content: ImageContent::Inline(Value::I32(1)),
        },
        ImageField {
            locator: Locator::Keyed(hex16("00000000000000000000000000000002")),
            content: ImageContent::Inline(Value::I32(2)),
        },
        ImageField {
            locator: Locator::Keyed(hex16("00000000000000000000000000000005")),
            content: ImageContent::Inline(Value::I32(3)),
        },
    ]);
    let b = TreeImage::new(vec![
        ImageField {
            locator: Locator::Keyed(hex16("00000000000000000000000000000005")),
            content: ImageContent::Inline(Value::I32(3)),
        },
        ImageField {
            locator: Locator::Keyed(hex16("0000000000000000000000000000000a")),
            content: ImageContent::Inline(Value::I32(1)),
        },
        ImageField {
            locator: Locator::Keyed(hex16("00000000000000000000000000000002")),
            content: ImageContent::Inline(Value::I32(2)),
        },
    ]);
    assert_eq!(semantic_tree(&a), semantic_tree(&b));
}
