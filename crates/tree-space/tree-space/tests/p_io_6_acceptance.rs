//! P-IO-6: acceptance + downstream contract — the tree-and-bucket surface as
//! finally consumable by downstream libraries.
//!
//! Per `_dev/树与桶管道/02-施工路线图.md` §8.4, this file proves that the
//! contract surface declared in `01-目标与设计.md` §1.10.2 is *really usable*
//! by a downstream consumer (beyond a clean compile):
//!
//! - **A. Main data path, full surface**: a tree carrying every element kind
//!   (inline scalar / `ArrowTable` block / String-keyed / bytes16-keyed map /
//!   `Vec` scalar sequence / `Vec` node / `Option` / nested node / ephemeral
//!   annotation, dual derive) commits to the bucket, reopens, verifies clean
//!   with the identical leaf count, and projects back to identical re-encoded
//!   bytes; verify cross-checks are clean and reference-table forgery is
//!   rejected; the GC `keep_from_sequence` window preserves in-window
//!   references and sweeps orphans/out-of-window history; ephemeral fields
//!   reopen to `Default`; one `RefId` referenced from several xpaths writes a
//!   single `tb-blocks/` blob; the reopened `XPathIndex` hits `/field`,
//!   `/map/key`, `/vec[index]` and misses missing/out-of-range paths.
//!
//! - **B. Downstream contract happy paths**: the bucket holds a whole
//!   heterogeneous table that projects back value-for-value identical; views
//!   (concat/select/join) resolve ≡ a manually constructed Arrow result
//!   (verified along the tb4_view RefId-equality pattern); a business-shaped
//!   derive (nested / mixed-case field names) round-trips.
//!
//! No new bytes golden is frozen here; every equality is either behavioral or
//! content-identity (`ref_id`), so the existing golden set is untouched.

// The B3 business-shape fixture deliberately uses mixed-case field names (the
// derive maps them to xpath steps as-is); suppress the cosmetic lint for the
// whole acceptance test crate.
#![allow(non_snake_case)]

use arrow::array::{Array, FixedSizeBinaryArray, Float64Array, Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tree_space::tree::codec::{EncodeTree, ImageContent, TreeImage, encode, encode_node};
use tree_space::xpath::XPath;
use tree_space::{
    ArrowTable, Blob, Block, Bucket, Combinator, ErrorCode, JoinMode, RefRow, Sequence, TbLibrary,
    TreeCodec, TreeNode, TreeNodeMeta, Value, ViewInput, ViewNode, block_blob_address,
    canonical_xpath_bytes, decode_ref_table, encode_ref_table, image_leaf_refs, named_field,
};

// -----------------------------------------------------------------------------
// Shared fixture: the P-IO-6 full-surface tree
// -----------------------------------------------------------------------------

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Inner {
    tag: Value,
    blob: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct FullSurface {
    name: Value,
    labels: Vec<Value>,
    table: ArrowTable,
    strings: BTreeMap<String, ArrowTable>,
    keyed: BTreeMap<[u8; 16], ArrowTable>,
    seq: Sequence,
    chunks: Vec<ArrowTable>,
    note: Option<Value>,
    inner: Inner,
    group: Vec<Inner>,
    #[tree_space(ephemeral)]
    scratch: Vec<ArrowTable>,
}

fn table(ids: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(ids.to_vec()))]).unwrap(),
    )
    .unwrap()
}

fn full_surface_fixture() -> (Bucket, FullSurface) {
    let main = table(&[5]);
    let strings = BTreeMap::from([
        ("alpha".to_string(), table(&[1])),
        ("beta".to_string(), table(&[2])),
    ]);
    let keyed = BTreeMap::from([([7u8; 16], table(&[3])), ([8u8; 16], table(&[4]))]);
    let seq = Sequence::new(vec![Value::I32(7), Value::I64(8)]);
    let chunks = vec![table(&[9]), table(&[10])];
    let inner_blob = Blob::new(vec![1, 2, 3]);
    let group = vec![
        Inner {
            tag: Value::Bool(true),
            blob: Blob::new(vec![4]),
        },
        Inner {
            tag: Value::Utf8("g".into()),
            blob: Blob::new(vec![5]),
        },
    ];
    let scratch = vec![table(&[11]), table(&[12])];

    let mut bucket = Bucket::new();
    bucket.put(&main);
    for value in strings.values() {
        bucket.put(value);
    }
    for value in keyed.values() {
        bucket.put(value);
    }
    bucket.put(&seq);
    for chunk in &chunks {
        bucket.put(chunk);
    }
    bucket.put(&inner_blob);
    for inner in &group {
        bucket.put(&inner.blob);
    }
    for chunk in &scratch {
        bucket.put(chunk);
    }

    let surface = FullSurface {
        name: Value::Utf8("exp".into()),
        labels: vec![Value::I32(1), Value::I64(2)],
        table: main,
        strings,
        keyed,
        seq,
        chunks,
        note: Some(Value::Utf8("n".into())),
        inner: Inner {
            tag: Value::Bool(true),
            blob: inner_blob,
        },
        group,
        scratch,
    };
    (bucket, surface)
}

/// The number of block leaves the full-surface tree carries.
const FULL_SURFACE_LEAVES: usize = 13;

fn commit_full_surface(root: &Path) -> (Bucket, Vec<u8>) {
    let library = TbLibrary::create(root).unwrap();
    let (bucket, surface) = full_surface_fixture();
    let tree_bytes = encode_node(&surface).unwrap();
    let leaf_refs = surface.leaf_refs();
    assert_eq!(leaf_refs.len(), FULL_SURFACE_LEAVES);
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    (bucket, tree_bytes)
}

// -----------------------------------------------------------------------------
// A. Main data path, full surface
// -----------------------------------------------------------------------------

#[test]
fn p_io_6_full_surface_roundtrip() {
    // A1: a tree carrying every element kind commits as whole-table blocks,
    // reopens, verifies the identical leaf count, and projects back to the
    // original canonical bytes.
    let temp = tempfile::tempdir().unwrap();
    let (bucket, tree_bytes) = commit_full_surface(temp.path());
    let reopened = TbLibrary::open(temp.path()).unwrap();
    assert_eq!(reopened.verify().unwrap(), FULL_SURFACE_LEAVES);

    let projected: FullSurface = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);

    // The reopened bucket envelope set equals the in-memory original.
    let restored = reopened.bucket().unwrap();
    assert_eq!(restored.len(), bucket.len());
    let mut restored_ids = restored.ids().collect::<Vec<_>>();
    restored_ids.sort();
    let mut original_ids = bucket.ids().collect::<Vec<_>>();
    original_ids.sort();
    assert_eq!(restored_ids, original_ids);
}

#[test]
fn p_io_6_verify_clean_and_forgery_rejected() {
    // A2: the A1 tree verifies clean (three-column cross-check), and manually
    // forging either the `ref_id` or the `address` column of the committed
    // reference table is rejected by verify.
    let temp = tempfile::tempdir().unwrap();
    let (_, _) = commit_full_surface(temp.path());
    let reopened = TbLibrary::open(temp.path()).unwrap();
    assert_eq!(reopened.verify().unwrap(), FULL_SURFACE_LEAVES);

    // (a) identity forgery: keep valid addresses, flip one ref_id byte.
    // 先 open 后篡改、再 verify：open 恢复必验 ref 地址（02 §5.2），
    // 「先篡改后 open」会在 open 层直接 Err，verify 的负向检查无法到达。
    let root_a = temp.path().join("forged_ref_id");
    let (_, _) = commit_full_surface(&root_a);
    let library_a = TbLibrary::open(&root_a).unwrap();
    tamper_ref_table(&root_a, |row| row.ref_id[0] ^= 0xff);
    let error = library_a
        .verify()
        .expect_err("forged ref_id must be rejected");
    assert_eq!(error.code, ErrorCode::DigestMismatch);

    // (b) address forgery: point a row at an address with no blob behind it.
    let root_b = temp.path().join("forged_address");
    let (_, _) = commit_full_surface(&root_b);
    let library_b = TbLibrary::open(&root_b).unwrap();
    tamper_ref_table(&root_b, |row| row.address = [0xee; 16]);
    let error = library_b
        .verify()
        .expect_err("dangling address must be rejected");
    assert_eq!(error.code, ErrorCode::DanglingReference);
}

#[test]
fn p_io_6_gc_window_preserves_and_sweeps() {
    // A3: two commits; GC with the head as the keep window preserves the
    // in-window references while reclaiming orphans and out-of-window history.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let block_a = Blob::new(vec![0xaa]);
    let block_b = Blob::new(vec![0xbb]);
    let block_orphan = Blob::new(vec![0xcc]);
    let a = block_a.ref_id();
    let b = block_b.ref_id();
    let orph = block_orphan.ref_id();
    let mut bucket = Bucket::new();
    bucket.put(&block_a);
    bucket.put(&block_b);
    bucket.put(&block_orphan);

    // Sequence 1: references `a` and the orphan.
    let tree_ab = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(a)),
        named_field("orph", ImageContent::Ref(orph)),
    ]);
    let tb_ab = encode(&tree_ab).unwrap();
    let first = library
        .commit(&tb_ab, &image_leaf_refs(&tree_ab).unwrap(), &bucket)
        .unwrap();

    // Sequence 2: references only `b`; `a` and the orphan become orphans.
    let tree_b = TreeImage::new(vec![named_field("b", ImageContent::Ref(b))]);
    let tb_b = encode(&tree_b).unwrap();
    let second = library
        .commit(&tb_b, &image_leaf_refs(&tree_b).unwrap(), &bucket)
        .unwrap();
    assert_eq!(second.sequence, first.sequence + 1);

    // Window behaviour: in-window (`b`) kept, out-of-window history (`a`,
    // orphan, first commit's tree/refs/commit) reclaimed.
    let report = library.gc(second.sequence).unwrap();
    assert!(report.reclaimed >= 4);

    let root = temp.path().join("library");
    for (block, present) in [(&block_a, false), (&block_b, true), (&block_orphan, false)] {
        let address = block_blob_address(&block.envelope().encode());
        let path = root.join("tb-blocks").join(format!("{address}.bin"));
        assert_eq!(
            path.exists(),
            present,
            "blob {address} existence after GC is {present}"
        );
    }

    // The head commit still opens and verifies clean after the sweep.
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
}

#[derive(TreeCodec, TreeNode, Clone, Debug, Default)]
struct Leaf {
    note: Option<Value>,
    tbl: ArrowTable,
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct EphemeralSurface {
    name: Value,
    data: ArrowTable,
    #[tree_space(ephemeral)]
    scratch: Vec<ArrowTable>,
    #[tree_space(ephemeral)]
    temp: Leaf,
    #[tree_space(ephemeral)]
    extras: Option<Leaf>,
}

#[test]
fn p_io_6_ephemeral_reopens_defaults() {
    // A4 (golden-25 semantics folded into the acceptance panorama): ephemeral
    // containers/nodes commit_pruned reopen to `Default`, while non-ephemeral
    // fields survive.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let data = table(&[1]);
    let scratch_a = table(&[2]);
    let scratch_b = table(&[3]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&scratch_a);
    bucket.put(&scratch_b);
    let fixture = EphemeralSurface {
        name: Value::Utf8("n".into()),
        data: data.clone(),
        scratch: vec![scratch_a, scratch_b],
        temp: Leaf {
            note: Some(Value::Utf8("t".into())),
            tbl: table(&[4]),
        },
        extras: Some(Leaf {
            note: Some(Value::Utf8("e".into())),
            tbl: table(&[5]),
        }),
    };
    let fields = EncodeTree::tree_children(&fixture).unwrap();
    let prefixes = <EphemeralSurface as TreeNodeMeta>::ephemeral_prefixes()
        .iter()
        .map(canonical_xpath_bytes)
        .collect::<Vec<_>>();
    let receipt = library.commit_pruned(&fields, &prefixes, &bucket).unwrap();
    assert!(receipt.sequence >= 1);

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        1,
        "only the persistent `data` leaf survives"
    );
    let projected: EphemeralSurface = reopened.project().unwrap();
    assert_eq!(projected.name, fixture.name);
    assert_eq!(
        projected.data.ref_id(),
        data.ref_id(),
        "non-ephemeral block is preserved"
    );
    assert!(projected.scratch.is_empty());
    assert_eq!(projected.temp.note, None);
    assert_eq!(
        projected.temp.tbl.ref_id(),
        ArrowTable::default().ref_id(),
        "ephemeral node reopens to its Default"
    );
    assert!(projected.extras.is_none());
}

#[test]
fn p_io_6_block_dedup_single_blob() {
    // A5: one RefId referenced from several xpaths writes exactly one
    // `tb-blocks/` blob (`exists` → skip idempotency across commits).
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let block = Blob::new(vec![1, 2, 3]);
    let id = block.ref_id();
    let mut bucket = Bucket::new();
    bucket.put(&block);
    let image = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(id)),
        named_field("b", ImageContent::Ref(id)),
        named_field("c", ImageContent::Ref(id)),
    ]);
    let tree_bytes = encode(&image).unwrap();
    let leaf_refs = image_leaf_refs(&image).unwrap();
    assert_eq!(leaf_refs.len(), 3);

    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    let blocks_dir = temp.path().join("library").join("tb-blocks");
    assert_eq!(std::fs::read_dir(&blocks_dir).unwrap().count(), 1);

    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    assert_eq!(
        std::fs::read_dir(&blocks_dir).unwrap().count(),
        1,
        "re-committing the same block set must not rewrite the blob"
    );

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        3,
        "three rows all cross-check against the single blob"
    );
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct IndexSurface {
    field: ArrowTable,
    records: BTreeMap<String, ArrowTable>,
    vec: Vec<ArrowTable>,
}

#[test]
fn p_io_6_index_query_hit_and_miss() {
    // A6: the reopened XPathIndex hits `/field`, `/records/<key>` and
    // `/vec[index]`, and misses missing / out-of-range paths. (The map field is
    // named `records` — the derive's decode internal binds a `map` variable, so
    // a downstream field literally named `map` would shadow it; see 04 log.)
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    let field = table(&[1]);
    let map_a = table(&[2]);
    let map_b = table(&[3]);
    let vec_0 = table(&[4]);
    let vec_1 = table(&[5]);
    bucket.put(&field);
    bucket.put(&map_a);
    bucket.put(&map_b);
    bucket.put(&vec_0);
    bucket.put(&vec_1);
    let surface = IndexSurface {
        field: field.clone(),
        records: BTreeMap::from([("a".to_string(), map_a), ("b".to_string(), map_b)]),
        vec: vec![vec_0, vec_1],
    };
    let tree_bytes = encode_node(&surface).unwrap();
    library
        .commit(&tree_bytes, &surface.leaf_refs(), &bucket)
        .unwrap();

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    let index = reopened.index().unwrap();
    assert_eq!(index.iter().count(), 5);

    assert!(index.get(&XPath::root().field("field")).is_some());
    assert!(
        index
            .get(&XPath::root().field("records").field("a"))
            .is_some()
    );
    assert!(
        index
            .get(&XPath::root().field("records").field("b"))
            .is_some()
    );
    assert!(index.get(&XPath::root().field("vec").index(0)).is_some());
    assert!(index.get(&XPath::root().field("vec").index(1)).is_some());

    assert!(index.get(&XPath::root().field("missing")).is_none());
    assert!(index.get(&XPath::root().field("vec").index(2)).is_none());
    assert!(
        index
            .get(&XPath::root().field("records").field("zzz"))
            .is_none()
    );
    assert!(
        index.get(&XPath::root().field("field").index(0)).is_none(),
        "an Index step over a plain field must miss"
    );
}

// -----------------------------------------------------------------------------
// B. Downstream contract happy paths
// -----------------------------------------------------------------------------

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct SingleTable {
    data: ArrowTable,
}

#[test]
fn p_io_6_bucket_holds_whole_table() {
    // B1: the bucket stores a whole heterogeneous ArrowTable (≥2 columns of
    // differing types, polars style) which projects back with the same ref_id
    // and identical values.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let whole = hetero_table();
    let original_id = whole.ref_id();
    let mut bucket = Bucket::new();
    bucket.put(&whole);
    let tree = SingleTable {
        data: whole.clone(),
    };
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    let projected: SingleTable = reopened.project().unwrap();
    assert_eq!(projected.data.ref_id(), original_id);
    assert_batches_equal(projected.data.as_batch(), whole.as_batch());
}

#[test]
fn p_io_6_view_materializes_read() {
    // B2: multiple blocks composed through ViewNode concat/select/join resolve
    // ≡ a manually constructed Arrow result (RefId-equality pattern from
    // tb4_view plus a per-value equality).
    let mut bucket = Bucket::new();
    let left = ArrowTable::try_new(
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Arc::new(Field::new("id", DataType::Int32, false)),
                Arc::new(Field::new("label", DataType::Utf8, false)),
            ])),
            vec![
                Arc::new(Int32Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["a".to_string(), "b".to_string()])),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let extra = ArrowTable::try_new(
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Arc::new(Field::new("id", DataType::Int32, false)),
                Arc::new(Field::new("label", DataType::Utf8, false)),
            ])),
            vec![
                Arc::new(Int32Array::from(vec![3])),
                Arc::new(StringArray::from(vec!["c".to_string()])),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let right = ArrowTable::try_new(
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Arc::new(Field::new("id", DataType::Int32, false)),
                Arc::new(Field::new("score", DataType::Float64, false)),
            ])),
            vec![
                Arc::new(Int32Array::from(vec![2, 3])),
                Arc::new(Float64Array::from(vec![20.0, 30.0])),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    let left_id = bucket.put(&left);
    let extra_id = bucket.put(&extra);
    let right_id = bucket.put(&right);

    // concat → select over two blocks.
    let concat = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(left_id), ViewInput::Ref(extra_id)],
    )
    .unwrap();
    let select = ViewNode::new(
        Combinator::Select {
            columns: vec!["label".into(), "id".into()],
            rows: None,
        },
        vec![ViewInput::View(Box::new(concat))],
    )
    .unwrap();
    let resolved = select.resolve(&bucket).unwrap();
    let manual_select = ArrowTable::try_new(
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Arc::new(Field::new("label", DataType::Utf8, false)),
                Arc::new(Field::new("id", DataType::Int32, false)),
            ])),
            vec![
                Arc::new(StringArray::from(vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                ])),
                Arc::new(Int32Array::from(vec![1, 2, 3])),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(resolved.ref_id(), manual_select.ref_id());
    assert_batches_equal(
        resolved.as_table().unwrap().as_batch(),
        manual_select.as_batch(),
    );

    // inner join over two blocks.
    let join = ViewNode::new(
        Combinator::Join {
            keys: vec!["id".into()],
            mode: JoinMode::Inner,
        },
        vec![ViewInput::Ref(left_id), ViewInput::Ref(right_id)],
    )
    .unwrap();
    let resolved_join = join.resolve(&bucket).unwrap();
    let manual_join = ArrowTable::try_new(
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Arc::new(Field::new("id", DataType::Int32, false)),
                Arc::new(Field::new("label", DataType::Utf8, false)),
                Arc::new(Field::new("score", DataType::Float64, false)),
            ])),
            vec![
                Arc::new(Int32Array::from(vec![2])),
                Arc::new(StringArray::from(vec!["b".to_string()])),
                Arc::new(Float64Array::from(vec![20.0])),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(resolved_join.ref_id(), manual_join.ref_id());
    assert_batches_equal(
        resolved_join.as_table().unwrap().as_batch(),
        manual_join.as_batch(),
    );
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct RunMeta {
    runId: Value,
    counts: Sequence,
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct BusinessSample {
    sampleID: Value,
    displayLabel: Value,
    measurements: Vec<ArrowTable>,
    meta: RunMeta,
    perChrom: BTreeMap<String, ArrowTable>,
    optionTag: Option<Value>,
}

#[test]
fn p_io_6_typed_derive_from_business_shape() {
    // B3: a business-shaped tree (nested nodes, mixed-case field names) derives
    // with the dual derive and round-trips through commit → open → project with
    // byte-identical re-encoding.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    let measurement_a = table(&[1]);
    let measurement_b = table(&[2]);
    let chr_a = table(&[3]);
    let chr_b = table(&[4]);
    for block in [&measurement_a, &measurement_b, &chr_a, &chr_b] {
        bucket.put(block);
    }
    let tree = BusinessSample {
        sampleID: Value::Utf8("S-1".into()),
        displayLabel: Value::Utf8("sample one".into()),
        measurements: vec![measurement_a, measurement_b],
        meta: RunMeta {
            runId: Value::Utf8("R-9".into()),
            counts: Sequence::new(vec![Value::U64(42)]),
        },
        perChrom: BTreeMap::from([("c1".to_string(), chr_a), ("c2".to_string(), chr_b)]),
        optionTag: Some(Value::I32(7)),
    };
    bucket.put(&tree.meta.counts);
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(reopened.verify().unwrap(), 5);
    let projected: BusinessSample = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);
    assert_eq!(projected.sampleID, tree.sampleID);
    assert_eq!(projected.meta.runId, tree.meta.runId);
}

// -----------------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hetero_table() -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new("label", DataType::Utf8, false)),
        Arc::new(Field::new("score", DataType::Float64, false)),
    ]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["a".to_string(), "b".to_string()])),
                Arc::new(Float64Array::from(vec![10.0, 20.0])),
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

fn head_commit_id(root: &Path) -> Vec<u8> {
    let committed =
        tree_space::ipc::decode_batch(&std::fs::read(root.join("committed")).unwrap()).unwrap();
    committed
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec()
}

fn head_refs_address(root: &Path) -> Vec<u8> {
    let commit = tree_space::ipc::decode_batch(
        &std::fs::read(
            root.join("commits")
                .join(format!("{}.ipc", hex(&head_commit_id(root)))),
        )
        .unwrap(),
    )
    .unwrap();
    commit
        .column(6)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec()
}

fn read_head_ref_rows(root: &Path) -> Vec<RefRow> {
    let bytes = std::fs::read(
        root.join("tb-refs")
            .join(format!("{}.ipc", hex(&head_refs_address(root)))),
    )
    .unwrap();
    decode_ref_table(&bytes).unwrap()
}

/// Rewrites the committed reference table with every row mutated by `mutate`.
fn tamper_ref_table(root: &Path, mutate: impl Fn(&mut RefRow)) {
    let mut rows = read_head_ref_rows(root);
    for row in &mut rows {
        mutate(row);
    }
    rows.sort();
    let path = root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&head_refs_address(root))));
    std::fs::write(&path, encode_ref_table(&rows).unwrap()).unwrap();
}

/// Per-value equality for two record batches whose schema matches.
fn assert_batches_equal(actual: &RecordBatch, expected: &RecordBatch) {
    assert_eq!(actual.num_rows(), expected.num_rows());
    assert_eq!(actual.num_columns(), expected.num_columns());
    for index in 0..actual.num_columns() {
        assert_eq!(
            actual.schema().field(index).name(),
            expected.schema().field(index).name()
        );
        assert_eq!(
            actual.schema().field(index).data_type(),
            expected.schema().field(index).data_type()
        );
        let actual_array = actual.column(index);
        let expected_array = expected.column(index);
        assert_eq!(actual_array.len(), expected_array.len());
        for row in 0..actual_array.len() {
            assert_eq!(actual_array.is_null(row), expected_array.is_null(row));
            if !actual_array.is_null(row) {
                let a = actual_array.slice(row, 1);
                let b = expected_array.slice(row, 1);
                let equal = arrow::compute::kernels::cmp::eq(&a, &b).unwrap().value(0);
                assert!(equal, "value differs at row {row} column {index}");
            }
        }
    }
}
