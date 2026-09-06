//! PL-1 S1 + S5 anchor: layout-version enums (`MemLayout` / `DiskLayout`) and
//! the version side-table + nine-column commit (02 §5.7).
//!
//! S1 covers str round-trips (`p_l_1_ver_as_str_roundtrip`), payload-magic
//! probing (`p_l_1_ver_probe_magic`) and the orthogonality of the memory and
//! disk layout dimensions (`p_l_1_mem_disk_orthogonal`). S5 covers the
//! per-commit version side-table roundtrip across a mixed-materialization
//! library (`p_l_1_versions_commit_roundtrip`), the nine-column commit decode
//! with legacy eight-column readability (`p_l_1_commit_nine_column_decode`),
//! the re-frozen golden 19 with its stable id
//! (`p_l_1_golden19_refrozen`) and the frozen side-table schema bytes
//! (`p_l_1_sidetable_schema_frozen`). Spec: 02-施工路线图 §5.3/§5.7, design:
//! 01-目标与设计 §2-1/§2-9/§3-1/§4-2.

use arrow::array::FixedSizeBinaryArray;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::convert::IpcSchemaEncoder;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tempfile::tempdir;
use tree_space::layout::tb::{
    decode_tb_commit_pointers, decode_versions_table, tb_commit_batch, tb_versions_schema,
};
use tree_space::tree::codec::{ImageContent, TreeImage, encode};
use tree_space::xpath::XPath;
use tree_space::{
    ArrowTable, Blob, Bucket, DiskLayout, Materialization, MemLayout, TbLibrary,
    canonical_xpath_bytes, image_leaf_refs, named_field,
};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn unhex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Three values round-trip through their explicit strings; case variants and
/// unknown strings map to `None`.
///
/// `MemLayout` is a closed compile-time enum (01 §2-9: in-memory enums never
/// enter disk bytes), so only its forward `as_str` direction is public; the
/// disk dimensions carry the explicit `from_str` mapping.
#[test]
fn p_l_1_ver_as_str_roundtrip() {
    assert_eq!(MemLayout::Arrow55.as_str(), "arrow55");

    let disk_cases: [(&str, DiskLayout); 2] = [
        ("arrow-ipc", DiskLayout::ArrowIpc),
        ("arrow-parquet", DiskLayout::ArrowParquet),
    ];
    for (want, layout) in disk_cases {
        assert_eq!(layout.as_str(), want);
        assert_eq!(DiskLayout::from_str(want), Some(layout));
    }

    // Case variants and unknown strings are not explicit mapping keys.
    let unknown: [&str; 9] = [
        "Arrow55",
        "ARROW55",
        "ArrowIpc",
        "ARROW-IPC",
        "ArrowParquet",
        "ARROW-PARQUET",
        "arrow-ipc2",
        "polars-ipc",
        "nope",
    ];
    for s in unknown {
        assert_eq!(DiskLayout::from_str(s), None, "unknown string {s:?}");
    }
    assert_eq!(DiskLayout::from_str(""), None);
}

/// `ARROW1` → Ipc, `PAR1` → Parquet, native/opaque bytes → `None`.
#[test]
fn p_l_1_ver_probe_magic() {
    // Known magics, full payload prefixes as read from real files.
    assert_eq!(
        DiskLayout::probe_magic(b"ARROW1\x00\x00\x00\x00"),
        Some(DiskLayout::ArrowIpc)
    );
    assert_eq!(
        DiskLayout::probe_magic(b"PAR1\x00\x00\x00\x00MARK"),
        Some(DiskLayout::ArrowParquet)
    );
    // Prefix matching: a longer known magic still probes to its format.
    assert_eq!(
        DiskLayout::probe_magic(b"ARROW1with-more-bytes"),
        Some(DiskLayout::ArrowIpc)
    );
    assert_eq!(
        DiskLayout::probe_magic(b"PAR1\x00\x00thrift-parquet"),
        Some(DiskLayout::ArrowParquet)
    );
    // Native/opaque bytes and near-miss prefixes → None (degraded candidate).
    for other in [
        b"".as_slice(),
        b"native-bytes",
        b"ARROW",
        b"ARROW2",
        b"xARROW1",
        b"PAR",
        b"PAR2",
        b"PAR0",
        b"Thrift",
    ] {
        assert_eq!(DiskLayout::probe_magic(other), None, "magic {other:?}");
    }
}

/// `MemLayout` and `DiskLayout` are distinct nominal enums (a match on one
/// cannot name the other's variants — compile-time proof), and their `as_str`
/// values are pairwise disjoint.
#[test]
fn p_l_1_mem_disk_orthogonal() {
    // If the two enums were unified, the exhaustive matches below would fail
    // to compile (missing/vetoed variants), so successful compilation is the
    // distinctness proof.
    let mem_arm = match MemLayout::Arrow55 {
        MemLayout::Arrow55 => "arrow55",
    };
    let disk_arm = match DiskLayout::ArrowIpc {
        DiskLayout::ArrowIpc | DiskLayout::ArrowParquet => "arrow-disk",
    };
    assert_ne!(mem_arm, disk_arm);

    let names = [
        MemLayout::Arrow55.as_str(),
        DiskLayout::ArrowIpc.as_str(),
        DiskLayout::ArrowParquet.as_str(),
    ];
    for (i, a) in names.iter().enumerate() {
        for (j, b) in names.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "overlapping layout strings: {a} vs {b}");
            }
        }
    }
    assert_eq!(names[1], "arrow-ipc");
    assert_eq!(names[2], "arrow-parquet");
}

// ---------------------------------------------------------------------------
// S5: version side-table + nine-column commit (02 §5.7)
// ---------------------------------------------------------------------------

fn sample_table(rows: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![Arc::new(arrow::array::Int32Array::from(rows.to_vec()))],
        )
        .unwrap(),
    )
    .unwrap()
}

/// Golden-19 nine-column commit object bytes (PL-1 S5 re-freeze; byte-identical
/// to the P-IO-3 `TB_COMMIT_HEX` constant — re-frozen here as the S5 anchor).
const G19_NINE_COLUMN_HEX: &str = "4152524f573100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff780200001000000000000a000c000a00090004000a00000010000000000104000800080000000400080000000400000009000000fc010000bc0100007c010000440100000c010000c400000094000000380000000400000018ffffff140000000c0000000000010f10000000000000002afeffff100000000b00000074625f76657273696f6e730048ffffff180000000c0000000000010c38000000010000000800000008ffffff68ffffff140000000c000000000001040c0000000000000024ffffff040000006974656d000000000900000074625f7072756e6564000000a0ffffff140000000c0000000000010f1000000000000000b2feffff100000000700000074625f7265667300ccffffff140000000c0000000000010f1000000000000000defeffff100000000c00000074625f747265655f626c6f62000000001000140010000e000f0004000000080010000000140000000c0000000000010f100000000000000022ffffff100000000f00000074625f726f6f745f747265655f696400a0ffffff180000000c00000000000005100000000000000004000400040000000c0000006d657461646174615f72656600000000d4ffffff140000000c0000000000000f10000000000000008affffff1000000007000000747265655f69640010001400100000000f0004000000080010000000140000000c0000000000000f1000000000000000c6ffffff1000000006000000706172656e74000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000040000000000000000800000073657175656e6365000000000000000000000000000000000000000000000000ffffffff78020000100000000c001a0018001700040008000c000000200000008004000000000000000000000000000304000a0018000c00080004000a000000bc000000100000000100000000000000000000000a000000010000000000000000000000000000000100000000000000000000000000000001000000000000000000000000000000010000000000000000000000000000000100000000000000000000000000000001000000000000000000000000000000010000000000000000000000000000000100000000000000010000000000000000000000000000000000000000000000010000000000000001000000000000000000000016000000000000000000000001000000000000004000000000000000080000000000000080000000000000000100000000000000c0000000000000001000000000000000000100000000000001000000000000004001000000000000100000000000000080010000000000000100000000000000c0010000000000000800000000000000000200000000000020000000000000004002000000000000010000000000000080020000000000001000000000000000c0020000000000000100000000000000000300000000000010000000000000004003000000000000010000000000000080030000000000001000000000000000c00300000000000001000000000000000004000000000000080000000000000040040000000000000000000000000000400400000000000000000000000000004004000000000000000000000000000040040000000000000100000000000000800400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000094f7112c7db38093bcd4d7879177ac2c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000036653962336632366666303135663837386134633365316631363632343464630000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff00000000100000000c00140012000c00080004000c000000580200007402000010000000000004000800080000000400080000000400000009000000fc010000bc0100007c010000440100000c010000c400000094000000380000000400000018ffffff140000000c0000000000010f10000000000000002afeffff100000000b00000074625f76657273696f6e730048ffffff180000000c0000000000010c38000000010000000800000008ffffff68ffffff140000000c000000000001040c0000000000000024ffffff040000006974656d000000000900000074625f7072756e6564000000a0ffffff140000000c0000000000010f1000000000000000b2feffff100000000700000074625f7265667300ccffffff140000000c0000000000010f1000000000000000defeffff100000000c00000074625f747265655f626c6f62000000001000140010000e000f0004000000080010000000140000000c0000000000010f100000000000000022ffffff100000000f00000074625f726f6f745f747265655f696400a0ffffff180000000c00000000000005100000000000000004000400040000000c0000006d657461646174615f72656600000000d4ffffff140000000c0000000000000f10000000000000008affffff1000000007000000747265655f69640010001400100000000f0004000000080010000000140000000c0000000000000f1000000000000000c6ffffff1000000006000000706172656e74000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000040000000000000000800000073657175656e63650000000001000000c002000000000000800200000000000080040000000000000000000000000000900200004152524f5731";

/// The golden-19 commit id, unchanged by the ninth `tb_versions` column (01
/// §3-1: the side-table pointer never enters the five-part domain).
const G19_ID: &str = "ea454fc52da0fdc1bcce099eb7f75c9e";

/// Commits a mixed leaf set (Blob + ArrowTable) into a parquet-default flat
/// library and reads back the head commit plus its version side-table.
#[test]
fn p_l_1_versions_commit_roundtrip() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create_with_materialization(&root, Materialization::Parquet).unwrap();
    let blob = Blob::new(vec![1, 2, 3]);
    let table = sample_table(&[4, 5]);
    let mut bucket = Bucket::new();
    let blob_id = bucket.put(&blob);
    let table_id = bucket.put(&table);
    let image = TreeImage::new(vec![
        named_field("blob", ImageContent::Ref(blob_id)),
        named_field("table", ImageContent::Ref(table_id)),
    ]);
    let tree_bytes = encode(&image).unwrap();
    let leaf_refs = image_leaf_refs(&image).unwrap();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

    // Read the head commit object and its ninth-column side-table pointer.
    let head_batch =
        tree_space::ipc::decode_batch(&std::fs::read(root.join("committed")).unwrap()).unwrap();
    let head_id = head_batch
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    let commit_batch = tree_space::ipc::decode_batch(
        &std::fs::read(root.join("commits").join(format!("{}.ipc", hex(&head_id)))).unwrap(),
    )
    .unwrap();
    let pointers = decode_tb_commit_pointers(&commit_batch)
        .unwrap()
        .expect("head commit carries TB pointers");
    let versions_addr = pointers
        .versions
        .expect("the nine-column commit records a version side-table pointer");

    // The side-table object is stored under `tb-versions/` and decodes to the
    // committed leaf set, labeled with the materialized disk layouts: the
    // `table` leaf went through the Parquet materializer, the `blob` leaf
    // stayed on native bytes / the IPC path.
    let versions_bytes = std::fs::read(
        root.join("tb-versions")
            .join(format!("{}.ipc", hex(&versions_addr))),
    )
    .unwrap();
    let rows = decode_versions_table(&versions_bytes).unwrap();
    let by_xpath: std::collections::BTreeMap<_, _> = rows
        .iter()
        .map(|row| {
            (
                row.xpath.clone(),
                (row.mem_ver.as_str(), row.disk_ver.as_str()),
            )
        })
        .collect();
    let blob_xpath = canonical_xpath_bytes(&XPath::root().field("blob"));
    let table_xpath = canonical_xpath_bytes(&XPath::root().field("table"));
    assert_eq!(by_xpath.len(), 2, "every leaf has one side-table row");
    assert_eq!(by_xpath[&blob_xpath], ("arrow55", "arrow-ipc"));
    assert_eq!(by_xpath[&table_xpath], ("arrow55", "arrow-parquet"));
    assert_eq!(
        rows.iter()
            .map(|row| row.disk_ver.as_str())
            .collect::<Vec<_>>(),
        vec!["arrow-ipc", "arrow-parquet"],
        "rows are canonically ordered by xpath bytes (blob before table)"
    );

    // Reopen consumes the side-table and rebuilds an equivalent bucket.
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 2);
}

/// Nine-column decode: `tb_versions` non-null on a nine-column commit, `None`
/// on a legacy eight-column commit, and an over-extended (ten-column) commit
/// rejected.
#[test]
fn p_l_1_commit_nine_column_decode() {
    let parent = [0x0a; 16];
    let root = [0x1a; 16];
    let tree_blob = [0x2a; 16];
    let refs = [0x3a; 16];
    let versions = [0x4a; 16];

    // Nine columns, versions non-null.
    let nine = tb_commit_batch(9, parent, root, tree_blob, refs, None, Some(versions)).unwrap();
    let decoded = decode_tb_commit_pointers(&nine)
        .unwrap()
        .expect("nine-column commit yields pointers");
    assert_eq!(decoded.root_tree_id, root);
    assert_eq!(decoded.tree_blob, tree_blob);
    assert_eq!(decoded.refs, refs);
    assert_eq!(decoded.pruned, None);
    assert_eq!(decoded.versions, Some(versions));

    // Legacy eight-column commit (tb_pruned non-null, physically no
    // tb_versions column) still decodes with `versions = None` — the
    // `num_columns() == 8` fast path, fast-forward compatible.
    let recorded = vec![canonical_xpath_bytes(&XPath::root().field("scratch"))];
    let nine_pruned =
        tb_commit_batch(9, parent, root, tree_blob, refs, Some(&recorded), None).unwrap();
    let legacy_fields = nine_pruned
        .schema()
        .fields()
        .iter()
        .take(8)
        .map(|field| Arc::new(field.as_ref().clone()))
        .collect::<Vec<_>>();
    let legacy_columns = nine_pruned.columns()[..8].to_vec();
    let eight = RecordBatch::try_new(Arc::new(Schema::new(legacy_fields)), legacy_columns).unwrap();
    assert_eq!(
        eight.num_columns(),
        8,
        "legacy batch is physically eight columns"
    );
    let legacy = decode_tb_commit_pointers(&eight)
        .unwrap()
        .expect("eight-column commit still yields TB pointers");
    assert_eq!(legacy.root_tree_id, root);
    assert_eq!(legacy.tree_blob, tree_blob);
    assert_eq!(legacy.refs, refs);
    assert_eq!(legacy.pruned, Some(recorded));
    assert_eq!(
        legacy.versions, None,
        "legacy commits carry no side-table pointer"
    );

    // Over-extended: a ten-column commit is rejected.
    let fields = nine_pruned
        .schema()
        .fields()
        .iter()
        .map(|field| Arc::new(field.as_ref().clone()))
        .collect::<Vec<_>>();
    let mut columns = nine_pruned.columns().to_vec();
    columns.push(Arc::new(arrow::array::StringArray::from(vec![
        "unexpected".to_owned(),
    ])));
    let ten = RecordBatch::try_new(
        Arc::new(Schema::new(
            fields
                .into_iter()
                .chain(std::iter::once(Arc::new(arrow::datatypes::Field::new(
                    "extra",
                    DataType::Utf8,
                    true,
                ))))
                .collect::<Vec<_>>(),
        )),
        columns,
    )
    .unwrap();
    let error = decode_tb_commit_pointers(&ten).unwrap_err();
    assert_eq!(error.code, tree_space::ErrorCode::SchemaMismatch);
}

/// Golden 19 re-frozen as a nine-column batch: the object bytes now carry the
/// nullable `tb_versions` column while the `tb_commit_id` (five-part formula,
/// ninth column excluded) stays `ea454fc5...`.
#[test]
fn p_l_1_golden19_refrozen() {
    let parent = [0x0a; 16];
    let root = [0x1a; 16];
    let tree_blob = [0x2a; 16];
    let refs = [0x3a; 16];
    let batch = tb_commit_batch(9, parent, root, tree_blob, refs, None, None).unwrap();
    let bytes = tree_space::ipc::encode_batch(&batch).unwrap();
    assert_eq!(
        unhex(G19_NINE_COLUMN_HEX),
        bytes,
        "golden 19 nine-column bytes"
    );
    assert_eq!(
        tree_space::layout::tb::tb_commit_id(9, parent, root, tree_blob, refs),
        tree_space::ids::Digest::from_bytes(hex16(G19_ID)),
        "golden 19 id must be unchanged by the ninth column"
    );
    assert_eq!(
        G19_ID, "ea454fc52da0fdc1bcce099eb7f75c9e",
        "the ninth tb_versions column must not enter the five-part commit id domain"
    );
}

/// The `tb_versions_schema` IPC bytes are frozen (02 §5.8: new golden for the
/// S8 close).
#[test]
fn p_l_1_sidetable_schema_frozen() {
    let bytes = IpcSchemaEncoder::new()
        .schema_to_fb(&tb_versions_schema())
        .finished_data()
        .to_vec();
    assert_eq!(
        hex(&bytes),
        "0c0000000800080000000400080000000400000003000000700000003400000004000000acffffff140000000c000000000000050c000000000000009cffffff080000006469736b5f76657200000000d8ffffff140000000c000000000000050c00000000000000c8ffffff070000006d656d5f7665720010001400100000000f0004000000080010000000180000000c0000000000000410000000000000000400040004000000050000007870617468000000",
        "version side-table schema bytes"
    );
    // The three columns are xpath Binary / mem_ver Utf8 / disk_ver Utf8, all
    // non-null (01 §3-1).
    let schema = tb_versions_schema();
    let fields = schema.fields();
    let names: Vec<_> = fields.iter().map(|f| f.name()).collect();
    assert_eq!(names, vec!["xpath", "mem_ver", "disk_ver"]);
    assert_eq!(fields[0].data_type(), &DataType::Binary);
    assert_eq!(fields[1].data_type(), &DataType::Utf8);
    assert_eq!(fields[2].data_type(), &DataType::Utf8);
    assert!(fields.iter().all(|f| !f.is_nullable()));
}
