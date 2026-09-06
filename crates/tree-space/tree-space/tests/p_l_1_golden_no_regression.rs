//! PL-1 S8 golden no-regression anchor (02 §5.10 / §4.4 / §2 里程碑).
//!
//! The S8 closing red line: **every pre-existing golden is zero-change**
//! except the two S5 re-frozen commit batches (golden 19 / golden 24, now
//! nine-column, commit ids unchanged) and golden 21 (a v4-only assertion,
//! deleted with the v4 face at S7). This file re-derives every registered
//! golden value from the same public inputs as the owning tests and asserts
//! it against the ledger below, and registers the two new S8 schema-byte
//! goldens under numbers **26** (boot schema, S3 `p_l_1_boot_schema_frozen`)
//! and **27** (tb_versions schema, S5 `p_l_1_sidetable_schema_frozen`).
//!
//! Byte-stability is asserted through each object's content address / frozen
//! id (a 128-bit fingerprint of the exact file bytes — a single changed byte
//! changes the address) plus the frozen byte lengths; the owning tests
//! (`p_io_2/3/4/5`, `p_l_1_versions`, `a2_*`, `tb4_view`, `src/block/tests`)
//! keep the full hex-byte constants. A full-suite green is therefore the
//! byte-level guard, this file the centralized catalog + address/id guard.
//!
//! Golden numbering follows the 树与桶管道 03 golden 登记 (16–25) and the
//! 树与桶模型改造 03 golden 登记 (1–15); 26/27 continue as the S8
//! registrations (02 §5.10). M2's §7.6 default semantic numbering 26–28 will
//! shift accordingly (adjustable per §7.6 "编号以施工登记为准").

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::convert::IpcSchemaEncoder;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::layout::tb::{RefRow, decode_tb_commit_pointers, prune_image};
use tree_space::plugin::block::{ArrowIpcBlockPlugin, ArrowParquetBlockPlugin};
use tree_space::plugin::boot::boot_schema;
use tree_space::tree::codec::{
    ImageContent, TreeImage, encode, encode_node, named_field, positioned_field, tree_id,
};
use tree_space::xpath::XPath;
use tree_space::{
    ArrowTable, Blob, Block, BlockKind, BlockMaterializer, BlockPlugin, Bucket, Combinator,
    Decimal128, IpcMaterializer, JoinMode, Kv, Materialization, ParquetMaterializer, RefId,
    Sequence, Time32Unit, TimestampUnit, Value, ViewInput, ViewNode, block_blob_address,
    block_ref_id, canonical_xpath_bytes, encode_ref_table, probe_block_materialization,
    ref_table_address, tb_commit_batch, tb_commit_id, view_to_image,
};

// ---------------------------------------------------------------------------
// The registered golden ledger
// ---------------------------------------------------------------------------

/// Number / name / registered frozen value (or "—") / status.
///
/// `status` ∈ {`frozen`, `frozen-new-s8`, `behavioral`, `refrozen-s5`,
/// `refrozen-m2`, `deleted-s7`}. `refrozen-m2` marks the identity-class
/// goldens re-frozen at PL-2 S2 (02 §6.2/§6.4): the `ref_id` / `tree_id`
/// formulas switched from bytes to semantics — the diff is registered in the
/// final report; the address/schema byte classes (17/18/22/23 addresses,
/// 19/24 commit bytes+id) are untouched.
#[rustfmt::skip]
const LEDGER: &[(&str, &str, &str, &str)] = &[
    // Block identities (树与桶模型改造 03 golden 登记; AB-1 re-freeze).
    ("1",  "empty Table RefId",      "1c6c9cb0056fc09ed09f02dc3e033560", "refrozen-m2"),
    ("2",  "empty Sequence RefId",   "ddf9ce63037f24a8f605b6f630848176", "refrozen-m2"),
    ("3",  "empty Kv RefId",         "82bdddacfd3099c21048930ad865cdef", "refrozen-m2"),
    ("4",  "empty Blob RefId",       "5a9fa702e7a3f651e83b67167fad78b8", "refrozen-m2"),
    ("5",  "Value↔Arrow encodings",  "22 kinds, unit-tested in src/block", "frozen"),
    ("6",  "Kv canonical order",     "unit-tested in src/block",          "frozen"),
    ("7",  "view determinism (Select / Join-Inner / Join-Left)",
                                    "5eb4ec85a16ae8066b0d7e51afe7f030 / 765df74e6ddcc64dc5aadf29da7d67b1 / 77909bc58d3bda247dfbc6ed9633dac9", "refrozen-m2"),
    ("8",  "envelope frame bytes",   "unit-tested in src/block",          "frozen"),
    ("9",  "Sequence null sample",   "unit-tested in src/block",          "frozen"),
    ("10", "registered empty-block identity",
                                    "386083eca7ae526afee46a9d15af70d3", "refrozen-m2"),
    // Tree identities (A-2; 15 is behavioral by design).
    ("11", "empty tree TreeId",      "b421e79e3e8369989a2b2efcf5c08a79", "refrozen-m2"),
    ("12", "inline scalar tree TreeId",
                                    "80d36a83355b79dfbb61eb521922e6fc", "refrozen-m2"),
    ("13", "block-ref + chunk tree TreeId",
                                    "83c6aaaaa138687ebe1a9fd04957f071", "refrozen-m2"),
    ("14", "view tree TreeId",       "6dfa6239c44aa13f4f30c5bbb4be92cd", "refrozen-m2"),
    ("15", "typed ≡ dynamic TreeId", "—",                                 "behavioral"),
    // P-IO pipeline (树与桶管道 03 golden 登记 16–25).
    ("16", "degradation objects",    "94f7112c7db38093bcd4d7879177ac2c / 6e9b3f26ff015f878a4c3e1f166244dc", "frozen"),
    ("17", "sample Blob frame + address",
                                    "d796cdd97c2ecac5dbeb3cc6ac9dc605 / d647966165bab1e26a2cbec5ec794735", "refrozen-m2"),
    ("18", "tree blob + ref table",  "daeccfca911e826a3609331e7cd83f18 / 2a34434f6038528a916fe19d506a8438 / c35c93d83d536237e4ec44e356e5d542", "refrozen-m2"),
    ("19", "TB commit bytes",        "ea454fc52da0fdc1bcce099eb7f75c9e", "refrozen-s5"),
    ("20", "reopen equivalence",     "—",                                 "behavioral"),
    ("21", "old v4 reader degrade",  "—",                                 "deleted-s7"),
    ("22", "pruned tree bytes",      "25fb5e48798d234969eaa329aca585af / 98719b9c3328d2e1f5caf0535706b5a5", "refrozen-m2"),
    ("23", "pruned ref table",       "3aa14961d6d1c041ec82e40cfe25e1f0", "frozen"),
    ("24", "tb_pruned commit bytes", "ea454fc52da0fdc1bcce099eb7f75c9e", "refrozen-s5"),
    ("25", "read best-effort defaults", "—",                              "behavioral"),
    // P-IO-7.2: the Parquet-branch address pair (树与桶管道 04 §7.2).
    ("Pq", "parquet address/identity",
                                    "efaf05e1754df32a1ebec16a0a5a881d / 4ee1fd39d16713d6aedcbb37056c6b65", "refrozen-m2"),
    // S8 registrations: schema-byte goldens frozen at S3 / S5, numbered here.
    ("26", "boot schema IPC bytes",  "see p_l_1_boot_schema_frozen",      "frozen-new-s8"),
    ("27", "tb_versions schema IPC bytes",
                                    "see p_l_1_sidetable_schema_frozen",  "frozen-new-s8"),
];

// ---------------------------------------------------------------------------
// Helpers (same public inputs as the owning tests)
// ---------------------------------------------------------------------------

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Golden-17 / golden-18 sample blob (`b"tb-sample\0"`).
fn sample_blob() -> Blob {
    Blob::new(vec![
        0x74, 0x62, 0x2d, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x00,
    ])
}

/// One-column `Int32` table: the 7.2-parquet sample.
fn one_col_table(rows: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(rows.to_vec()))]).unwrap(),
    )
    .unwrap()
}

/// Two-column (id, label) table: the tb4 view samples.
fn pair_table(ids: &[i32], labels: &[&str]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new("label", DataType::Utf8, false)),
    ]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(ids.to_vec())),
                Arc::new(arrow::array::StringArray::from(labels.to_vec())),
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

/// Golden-18 sample tree (`block` Ref + `chunks` Table group of two).
fn sample_image() -> TreeImage {
    TreeImage::new(vec![
        named_field("block", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x22; 16]))),
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x33; 16]))),
                ],
            },
        ),
    ])
}

/// Golden-18 sample reference-table rows.
fn sample_ref_rows() -> Vec<RefRow> {
    vec![
        RefRow {
            xpath: canonical_xpath_bytes(&XPath::root().field("block")),
            ref_id: [0x11; 16],
            address: [0xaa; 16],
        },
        RefRow {
            xpath: canonical_xpath_bytes(&XPath::root().field("chunks").index(0)),
            ref_id: [0x22; 16],
            address: [0xbb; 16],
        },
        RefRow {
            xpath: canonical_xpath_bytes(&XPath::root().field("chunks").index(1)),
            ref_id: [0x33; 16],
            address: [0xcc; 16],
        },
    ]
}

/// Golden-22/23/24 ephemeral sample tree (P-IO-5 `sample_fields`).
fn sample_fields() -> Vec<tree_space::ImageField> {
    vec![
        named_field("block", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
        named_field(
            "common",
            ImageContent::Node(vec![named_field(
                "table",
                ImageContent::Ref(RefId::from_bytes([0x22; 16])),
            )]),
        ),
        named_field(
            "scratch",
            ImageContent::Node(vec![named_field(
                "lazy",
                ImageContent::Ref(RefId::from_bytes([0x33; 16])),
            )]),
        ),
        named_field(
            "cache",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x44; 16]))),
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x55; 16]))),
                ],
            },
        ),
    ]
}

/// Golden-22/23/24 pruned prefixes: `/scratch` + `/cache` (canonical order).
fn sample_prefixes() -> Vec<Vec<u8>> {
    vec![
        canonical_xpath_bytes(&XPath::root().field("scratch")),
        canonical_xpath_bytes(&XPath::root().field("cache")),
    ]
}

/// The P-IO-3 golden-18/19 commit samples: parent 0a, root 1a, tree 2a, refs 3a.
fn commit_samples() -> ([u8; 16], [u8; 16], [u8; 16], [u8; 16]) {
    ([0x0a; 16], [0x1a; 16], [0x2a; 16], [0x3a; 16])
}

/// The S8 schema-byte encoding (arrow IPC schema message, hex).
fn schema_bytes(schema: &Schema) -> String {
    hex(&IpcSchemaEncoder::new().schema_to_fb(schema).finished_data())
}

// ---------------------------------------------------------------------------
// The catalog assertions (one centralized entry)
// ---------------------------------------------------------------------------

/// Every registered golden, re-derived and asserted. See the module doc and
/// the [`LEDGER`] for the number ↔ value ↔ owner mapping.
#[test]
fn p_l_1_golden_no_regression() {
    // The red-line ledger itself: exactly two S5 re-frozen exceptions (19, 24),
    // the PL-2 M2 identity-class re-freeze set (1/2/3/4/7/10/11/12/13/14/17/18/
    // 22/Pq — 02 §6.2 已裁定清单), and exactly one S7-deleted entry (21) — the
    // whole point of the full checkout.
    assert_eq!(
        LEDGER.len(),
        28,
        "27 numbered goldens + the Pq parquet pair entry"
    );
    let refrozen_s5: Vec<&str> = LEDGER
        .iter()
        .filter(|entry| entry.3 == "refrozen-s5")
        .map(|entry| entry.0)
        .collect();
    assert_eq!(
        refrozen_s5,
        vec!["19", "24"],
        "golden 19/24 are the only S5 re-frozen exceptions"
    );
    let refrozen_m2: Vec<&str> = LEDGER
        .iter()
        .filter(|entry| entry.3 == "refrozen-m2")
        .map(|entry| entry.0)
        .collect();
    assert_eq!(
        refrozen_m2,
        vec![
            "1", "2", "3", "4", "7", "10", "11", "12", "13", "14", "17", "18", "22", "Pq"
        ],
        "PL-2 M2 re-freezes exactly the adjudicated identity-kind goldens (02 §6.2)"
    );
    let deleted: Vec<&str> = LEDGER
        .iter()
        .filter(|entry| entry.3 == "deleted-s7")
        .map(|entry| entry.0)
        .collect();
    assert_eq!(
        deleted,
        vec!["21"],
        "golden 21 (v4-only old-reader degrade) is the only S7 deletion"
    );

    digest_protocol();
    block_identities_and_combinators();
    tree_identities();
    pio_pipeline();
    parquet_branch();
    schema_goldens();
}

/// Digest protocol constants (`tests/digest_golden.rs`).
fn digest_protocol() {
    assert_eq!(
        tree_space::hash::empty_domain_digest().as_bytes(),
        hex16("855652511297084ea790a61be3641b2e"),
        "digest golden: empty-domain digest"
    );
    assert_eq!(
        tree_space::hash::canonical_digest(b"domain", Vec::<Vec<u8>>::new()).as_bytes(),
        hex16("855652511297084ea790a61be3641b2e"),
        "the constant is exactly the canonical zero-part domain digest"
    );

    let type_id = tree_space::TypeId::from_bytes([7; 16]);
    let column = tree_space::ColumnDefinition::from_field(
        0,
        Field::new("value", DataType::Int32, false),
        true,
    );
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
    let split_hash =
        tree_space::hash::table_content_hash(type_id, 1, &[column], &[split_a, split_b]).unwrap();
    assert_eq!(
        whole_hash.as_bytes(),
        hex16("20ab16de0e54cc595b0c34cbbd818f44"),
        "digest golden: content hash"
    );
    assert_eq!(
        whole_hash, split_hash,
        "split independence is part of the golden"
    );
}

/// Block identities (goldens 1–4, 10) and view combinator determinism (golden 7).
fn block_identities_and_combinators() {
    assert_eq!(
        block_ref_id(
            BlockKind::Table,
            &tree_space::block::Table::default().payload()
        )
        .as_bytes(),
        hex16("1c6c9cb0056fc09ed09f02dc3e033560"),
        "golden 1 (M2 re-freeze): empty Table"
    );
    assert_eq!(
        Sequence::new(Vec::<Value>::new()).ref_id().as_bytes(),
        hex16("ddf9ce63037f24a8f605b6f630848176"),
        "golden 2 (M2 re-freeze): empty Sequence"
    );
    assert_eq!(
        Kv::try_new(Vec::<(Value, Value)>::new())
            .unwrap()
            .ref_id()
            .as_bytes(),
        hex16("82bdddacfd3099c21048930ad865cdef"),
        "golden 3 (M2 re-freeze): empty Kv"
    );
    assert_eq!(
        Blob::new(Vec::<u8>::new()).ref_id().as_bytes(),
        hex16("5a9fa702e7a3f651e83b67167fad78b8"),
        "golden 4 (M2 re-freeze): empty Blob"
    );
    assert_eq!(
        block_ref_id(BlockKind::Named("org.example.ab2.empty".into()), &[]).to_string(),
        "386083eca7ae526afee46a9d15af70d3",
        "golden 10 (M2 re-freeze): registered empty-block identity"
    );

    // Golden 7: Select over Concat ([1,2]+[3], rows (1,2), label/id).
    let left = pair_table(&[1, 2], &["a", "b"]);
    let right = pair_table(&[3], &["c"]);
    let mut bucket = Bucket::new();
    let left_id = bucket.put(&left);
    let right_id = bucket.put(&right);
    let concat = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(left_id), ViewInput::Ref(right_id)],
    )
    .unwrap();
    let select = ViewNode::new(
        Combinator::Select {
            columns: vec!["label".into(), "id".into()],
            rows: Some((1, 2)),
        },
        vec![ViewInput::View(Box::new(concat))],
    )
    .unwrap();
    let resolved = select.resolve(&bucket).unwrap();
    assert_eq!(
        resolved.ref_id(),
        RefId::from_bytes(hex16("5eb4ec85a16ae8066b0d7e51afe7f030")),
        "golden 7 (M2 re-freeze): Select"
    );

    // Golden 7: Join on `id` ([1,2]×[2,3]) — inner 1 row, left 2 rows.
    let left = pair_table(&[1, 2], &["a", "b"]);
    let right = pair_table(&[2, 3], &["B", "C"]);
    let mut bucket = Bucket::new();
    let left_id = bucket.put(&left);
    let right_id = bucket.put(&right);
    for (mode, want) in [
        (JoinMode::Inner, "765df74e6ddcc64dc5aadf29da7d67b1"),
        (JoinMode::Left, "77909bc58d3bda247dfbc6ed9633dac9"),
    ] {
        let view = ViewNode::new(
            Combinator::Join {
                keys: vec!["id".into()],
                mode,
            },
            vec![ViewInput::Ref(left_id), ViewInput::Ref(right_id)],
        )
        .unwrap();
        let resolved = view.resolve(&bucket).unwrap();
        assert_eq!(
            resolved.ref_id(),
            RefId::from_bytes(hex16(want)),
            "golden 7: Join {mode:?}"
        );
    }
}

/// Tree identities (goldens 11–14 re-derived; golden 15's typed ≡ dynamic
/// essence re-asserted).
fn tree_identities() {
    // 11: empty tree.
    let bytes = encode(&TreeImage::new_empty()).unwrap();
    assert_eq!(
        tree_id(&bytes).as_bytes(),
        hex16("b421e79e3e8369989a2b2efcf5c08a79"),
        "golden 11 (M2 re-freeze): empty tree"
    );

    // 12: inline scalars incl. parameter-bag parameterized kinds.
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
    let bytes = encode(&TreeImage::new(children)).unwrap();
    assert_eq!(
        tree_id(&bytes).as_bytes(),
        hex16("80d36a83355b79dfbb61eb521922e6fc"),
        "golden 12 (M2 re-freeze): inline scalar tree"
    );

    // 13: block-ref + nested chunk group.
    let children = vec![
        named_field("block", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    positioned_field(ImageContent::ChunkEntry(RefId::from_bytes([0x22; 16]))),
                    positioned_field(ImageContent::ChunkGroup {
                        kind: BlockKind::Table,
                        children: vec![positioned_field(ImageContent::ChunkEntry(
                            RefId::from_bytes([0x33; 16]),
                        ))],
                    }),
                ],
            },
        ),
    ];
    let bytes = encode(&TreeImage::new(children)).unwrap();
    assert_eq!(
        tree_id(&bytes).as_bytes(),
        hex16("83c6aaaaa138687ebe1a9fd04957f071"),
        "golden 13 (M2 re-freeze): block-ref + chunk tree"
    );

    // 14: view tree (Join + ViewParam + nested Select + Registered).
    let join = view_to_image(
        &ViewNode::new(
            Combinator::Join {
                keys: vec!["k".into()],
                mode: JoinMode::Inner,
            },
            vec![
                ViewInput::Ref(RefId::from_bytes([0x11; 16])),
                ViewInput::View(Box::new(
                    ViewNode::new(
                        Combinator::Select {
                            columns: vec!["id".into()],
                            rows: Some((0, 4)),
                        },
                        vec![ViewInput::Ref(RefId::from_bytes([0x11; 16]))],
                    )
                    .unwrap(),
                )),
            ],
        )
        .unwrap(),
    );
    let registered = view_to_image(
        &ViewNode::new(
            Combinator::Registered("example.slice".into()),
            vec![ViewInput::Ref(RefId::from_bytes([0x22; 16]))],
        )
        .unwrap(),
    );
    let bytes = encode(&TreeImage::new(vec![
        named_field("join", ImageContent::View(join)),
        named_field("reg", ImageContent::View(registered)),
    ]))
    .unwrap();
    assert_eq!(
        tree_id(&bytes).as_bytes(),
        hex16("6dfa6239c44aa13f4f30c5bbb4be92cd"),
        "golden 14 (M2 re-freeze): view tree"
    );

    // 15: typed ≡ dynamic — same logical tree, same TreeId.
    #[derive(tree_space::TreeCodec, tree_space::TreeNode, Clone, Debug)]
    struct TwoLeaf {
        tag: Value,
        seq: Sequence,
    }
    let fixture = TwoLeaf {
        tag: Value::Utf8("x".into()),
        seq: Sequence::new(vec![Value::I32(7)]),
    };
    let typed_bytes = encode_node(&fixture).unwrap();
    let dynamic_bytes = encode(&TreeImage::new(vec![
        named_field("tag", ImageContent::Inline(Value::Utf8("x".into()))),
        named_field("seq", ImageContent::Ref(fixture.seq.ref_id())),
    ]))
    .unwrap();
    assert_eq!(
        tree_id(&typed_bytes).as_bytes(),
        tree_id(&dynamic_bytes).as_bytes(),
        "golden 15: typed and dynamic encodings share the TreeId"
    );
}

/// P-IO pipeline goldens 16–19 and 22–24 (20 / 25 are behavioral — asserted by
/// `p_io_4_reopen_verify` / `p_io_5_ephemeral`, documented in [`LEDGER`]).
fn pio_pipeline() {
    // 16: degradation objects.
    assert_eq!(
        tree_space::layout::tb::empty_tree_id().as_bytes(),
        hex16("94f7112c7db38093bcd4d7879177ac2c"),
        "golden 16: empty tree id"
    );
    assert_eq!(
        tree_space::layout::tb::empty_metadata_ref()
            .unwrap()
            .as_bytes(),
        hex16("6e9b3f26ff015f878a4c3e1f166244dc"),
        "golden 16: empty metadata ref"
    );

    // 17: sample blob frame — content address (byte fingerprint), identity,
    // length, and the S2 plugin/materializer byte-identity.
    let blob = sample_blob();
    let bytes = blob.envelope().encode();
    assert_eq!(
        block_blob_address(&bytes).as_bytes(),
        hex16("d796cdd97c2ecac5dbeb3cc6ac9dc605"),
        "golden 17: blob address"
    );
    assert_eq!(
        blob.ref_id().as_bytes(),
        hex16("d647966165bab1e26a2cbec5ec794735"),
        "golden 17 (M2 re-freeze): blob RefId"
    );
    assert_eq!(bytes.len(), 765, "golden 17 frame length (1530 hex chars)");
    assert_ne!(
        hex16("d796cdd97c2ecac5dbeb3cc6ac9dc605"),
        hex16("d647966165bab1e26a2cbec5ec794735"),
        "identity stays orthogonal to addressing"
    );
    let envelope = blob.envelope();
    assert_eq!(
        ArrowIpcBlockPlugin.encode(&envelope).unwrap(),
        bytes,
        "S2: the IPC plugin encodes the golden-17 frame byte-identically"
    );
    assert_eq!(
        IpcMaterializer.encode(&envelope).unwrap(),
        bytes,
        "S2: the legacy materializer face agrees byte-for-byte"
    );

    // 18: tree blob + ref table (addresses = content-address fingerprints).
    let tree_bytes = encode(&sample_image()).unwrap();
    assert_eq!(
        tree_space::tree_blob_address(&tree_bytes).as_bytes(),
        hex16("daeccfca911e826a3609331e7cd83f18"),
        "golden 18: tree blob address"
    );
    assert_eq!(
        tree_id(&tree_bytes).as_bytes(),
        hex16("2a34434f6038528a916fe19d506a8438"),
        "golden 18 (M2 re-freeze): TreeId"
    );
    assert_ne!(
        hex16("daeccfca911e826a3609331e7cd83f18"),
        hex16("2a34434f6038528a916fe19d506a8438"),
        "tree-blob addressing stays independent of the TreeId"
    );
    let ref_bytes = encode_ref_table(&sample_ref_rows()).unwrap();
    assert_eq!(
        ref_table_address(&ref_bytes).as_bytes(),
        hex16("c35c93d83d536237e4ec44e356e5d542"),
        "golden 18: ref table address"
    );
    assert_eq!(
        ref_bytes.len(),
        1306,
        "golden 18 ref table length (2612 hex chars)"
    );

    // 19: nine-column commit (S5 re-freeze exception #1) — id unchanged by
    // columns 8–9, byte length of the re-frozen object.
    let (parent, root, tree_blob, refs) = commit_samples();
    let commit_id = tb_commit_id(9, parent, root, tree_blob, refs);
    assert_eq!(
        commit_id.as_bytes(),
        hex16("ea454fc52da0fdc1bcce099eb7f75c9e"),
        "golden 19 id: unchanged by the tb_pruned / tb_versions columns"
    );
    let batch = tb_commit_batch(9, parent, root, tree_blob, refs, None, None).unwrap();
    let pointers = decode_tb_commit_pointers(&batch)
        .unwrap()
        .expect("nine-column commit yields TB pointers");
    assert_eq!(pointers.pruned, None);
    assert_eq!(
        pointers.versions, None,
        "golden 19 sample carries no side-table pointer"
    );
    assert_eq!(
        tree_space::ipc::encode_batch(&batch).unwrap().len(),
        3170,
        "golden 19 nine-column byte length"
    );

    // 22: pruned tree (persistent form) — bytes, address, TreeId, recorded
    // maximal prefixes.
    let (kept, recorded) = prune_image(sample_fields(), &sample_prefixes());
    assert_eq!(
        recorded,
        vec![
            canonical_xpath_bytes(&XPath::root().field("cache")),
            canonical_xpath_bytes(&XPath::root().field("scratch")),
        ],
        "golden 22: recorded maximal prefixes, canonically sorted"
    );
    let pruned_bytes = encode(&TreeImage::new(kept)).unwrap();
    assert_eq!(
        tree_space::tree_blob_address(&pruned_bytes).as_bytes(),
        hex16("25fb5e48798d234969eaa329aca585af"),
        "golden 22: pruned tree address"
    );
    assert_eq!(
        tree_id(&pruned_bytes).as_bytes(),
        hex16("98719b9c3328d2e1f5caf0535706b5a5"),
        "golden 22 (M2 re-freeze): pruned TreeId"
    );
    assert_eq!(
        pruned_bytes.len(),
        5994,
        "golden 22 pruned tree length (11988 hex chars)"
    );

    // 23: pruned ref table — only the persistent rows, pruned leaves absent.
    let pruned_rows = vec![
        RefRow {
            xpath: canonical_xpath_bytes(&XPath::root().field("block")),
            ref_id: [0x11; 16],
            address: [0xaa; 16],
        },
        RefRow {
            xpath: canonical_xpath_bytes(&XPath::root().field("common").field("table")),
            ref_id: [0x22; 16],
            address: [0xbb; 16],
        },
    ];
    let pruned_ref = encode_ref_table(&pruned_rows).unwrap();
    assert_eq!(
        ref_table_address(&pruned_ref).as_bytes(),
        hex16("3aa14961d6d1c041ec82e40cfe25e1f0"),
        "golden 23: pruned ref table address"
    );
    assert_eq!(
        pruned_ref.len(),
        1306,
        "golden 23 pruned ref table length (2612 hex chars)"
    );
    let scratch = canonical_xpath_bytes(&XPath::root().field("scratch"));
    assert!(
        pruned_rows
            .iter()
            .all(|row| !row.xpath.starts_with(&scratch)),
        "golden 23: the pruned subtree never appears in the ref table"
    );

    // 24: tb_pruned commit (S5 re-freeze exception #2) — same five-part id as
    // golden 19, byte length of the re-frozen object.
    let batch = tb_commit_batch(9, parent, root, tree_blob, refs, Some(&recorded), None).unwrap();
    let pointers = decode_tb_commit_pointers(&batch)
        .unwrap()
        .expect("nine-column commit yields TB pointers");
    assert_eq!(pointers.pruned.as_ref().map(Vec::len), Some(2));
    assert_eq!(pointers.versions, None);
    assert_eq!(
        tree_space::ipc::encode_batch(&batch).unwrap().len(),
        3362,
        "golden 24 nine-column byte length"
    );
    assert_eq!(
        tb_commit_id(9, parent, root, tree_blob, refs),
        commit_id,
        "golden 19 and golden 24 share the five-part commit id ± pruning/versions columns"
    );
}

/// The P-IO-7.2 Parquet-branch pair (address + materialization-independent
/// identity) plus the S2 plugin/materializer byte-identity on the Parquet path.
fn parquet_branch() {
    let data = one_col_table(&[1, 2, 3]);
    let envelope = data.envelope();
    let ref_id = block_ref_id(envelope.kind.clone(), &envelope.payload);
    assert_eq!(
        hex(&ref_id.as_bytes()),
        "4ee1fd39d16713d6aedcbb37056c6b65",
        "parquet golden (M2 re-freeze): sample-table semantic identity"
    );

    let physical = ParquetMaterializer.encode(&envelope).unwrap();
    assert_eq!(
        probe_block_materialization(&physical).unwrap(),
        Materialization::Parquet
    );
    assert_eq!(
        block_blob_address(&physical).as_bytes(),
        hex16("efaf05e1754df32a1ebec16a0a5a881d"),
        "parquet golden: materialized storage address"
    );
    assert_eq!(
        ArrowParquetBlockPlugin.encode(&envelope).unwrap(),
        physical,
        "S2: the Parquet plugin encodes byte-identically to the materializer"
    );

    // Identity ⊥ address: the IPC bytes of the same logical block address
    // differently (the golden-17 blob keeps its own frozen IPC address).
    assert_ne!(
        block_blob_address(&physical).as_bytes(),
        hex16("d796cdd97c2ecac5dbeb3cc6ac9dc605"),
        "the parquet address is distinct from the IPC blob address"
    );
}

/// The two new S8 schema-byte goldens: 26 boot schema (S3), 27 tb_versions
/// schema (S5) — full IPC schema byte equality (frozen at S3/S5, numbered
/// here; changing either schema changes these bytes).
fn schema_goldens() {
    assert_eq!(
        schema_bytes(&boot_schema()),
        "0c0000000800080000000400080000000400000004000000c8000000840000003c0000000400000098ffffff140000000c000000000000050c0000000000000088ffffff11000000747265655f6469736b5f76657273696f6e000000ccffffff140000000c000000000000050c00000000000000bcffffff10000000747265655f6d656d5f76657273696f6e0000000010001400100000000f0004000000080010000000180000000c00000000000005100000000000000004000400040000000a0000006c6962726172795f6964000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000010000000000000000c000000626f6f745f76657273696f6e00000000",
        "golden 26: boot schema IPC bytes (S8 registration of p_l_1_boot_schema_frozen)"
    );

    assert_eq!(
        schema_bytes(&tree_space::tb_versions_schema()),
        "0c0000000800080000000400080000000400000003000000700000003400000004000000acffffff140000000c000000000000050c000000000000009cffffff080000006469736b5f76657200000000d8ffffff140000000c000000000000050c00000000000000c8ffffff070000006d656d5f7665720010001400100000000f0004000000080010000000180000000c0000000000000410000000000000000400040004000000050000007870617468000000",
        "golden 27: tb_versions schema IPC bytes (S8 registration of p_l_1_sidetable_schema_frozen)"
    );
}
