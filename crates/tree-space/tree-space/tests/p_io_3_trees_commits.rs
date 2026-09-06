//! P-IO-3: canonical tree blobs (`tb-trees/`), three-column reference tables
//! (`tb-refs/`), the commit extension, and the GC extension.
//!
//! Freezes golden 18 (tree blob address + reference-table object bytes) and 19
//! (TB commit object bytes), and covers: canonical ref-table derivation and
//! dedup, the commit chain, the extended-GC reachability, and the
//! reference-table/commit negative checklist (order, schema, half-extension,
//! wrong column type).
//!
//! PL-1 S7 removed the pure-v4 assertions from this file (02 §5.9): the
//! creation-time "no TB channels under a pure-v4 layout" pair and the
//! old-v4-reader degradation check (assertion objects on the v4 open face,
//! since retired alongside the v4 `Library` surface). The TB-side assertions
//! (genesis commit stays a four-column batch included) are kept.

use arrow::array::{FixedSizeBinaryArray, StringArray};
use arrow::datatypes::{DataType, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::block::RefId;
use tree_space::ipc::{decode_batch, encode_batch};
use tree_space::layout::tb::{RefRow, block_blob_address};
use tree_space::tree::codec::{ImageContent, TreeImage, encode, tree_id};
use tree_space::xpath::XPath;
use tree_space::{
    Blob, Block, BlockKind, Bucket, Digest, canonical_xpath_bytes, encode_ref_table, named_field,
    positioned_field, ref_table_address_of_rows,
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

const TREE_BLOB_ADDRESS: &str = "daeccfca911e826a3609331e7cd83f18";
/// Golden-18 tree identity (PL-2 M2 re-freeze: `semantic_tree` walk).
const TREE_ID: &str = "2a34434f6038528a916fe19d506a8438";
const REF_TABLE_ADDRESS: &str = "c35c93d83d536237e4ec44e356e5d542";
const REF_TABLE_HEX: &str = "4152524f573100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000fffffffff80000001000000000000a000c000a00090004000a000000100000000001040008000800000004000800000004000000030000007c0000003400000004000000a0ffffff140000000c0000000000000f1000000000000000d2ffffff10000000070000006164647265737300ccffffff1c0000000c0000000000000f180000000000000000000600080004000600000010000000060000007265665f6964000010001400100000000f0004000000080010000000180000000c00000000000004100000000000000004000400040000000500000078706174680000000000000000000000000000000000000000000000000000000000000000000000fffffffff8000000100000000c001a0018001700040008000c00000020000000c001000000000000000000000000000304000a0018000c00080004000a0000004c00000010000000030000000000000000000000030000000300000000000000000000000000000003000000000000000000000000000000030000000000000000000000000000000000000007000000000000000000000001000000000000004000000000000000100000000000000080000000000000003e00000000000000c0000000000000000100000000000000000100000000000030000000000000004001000000000000010000000000000080010000000000003000000000000000ff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e000000260000003e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000460500000000000000626c6f636b4606000000000000006368756e6b734900000000000000004606000000000000006368756e6b734901000000000000000000ff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000011111111111111111111111111111111222222222222222222222222222222223333333333333333333333333333333300000000000000000000000000000000ff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbcccccccccccccccccccccccccccccccc00000000000000000000000000000000ffffffff0000000014000000000000000c00140012000c00080004000c000000cc000000e8000000100000000000040008000800000004000800000004000000030000007c0000003400000004000000a0ffffff140000000c0000000000000f1000000000000000d2ffffff10000000070000006164647265737300ccffffff1c0000000c0000000000000f180000000000000000000600080004000600000010000000060000007265665f6964000010001400100000000f0004000000080010000000180000000c00000000000004100000000000000004000400040000000500000078706174680000000100000040010000000000000001000000000000c0010000000000000000000000000000080100004152524f5731";
const TB_COMMIT_ID: &str = "ea454fc52da0fdc1bcce099eb7f75c9e";
const TB_COMMIT_HEX: &str = "4152524f573100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff780200001000000000000a000c000a00090004000a00000010000000000104000800080000000400080000000400000009000000fc010000bc0100007c010000440100000c010000c400000094000000380000000400000018ffffff140000000c0000000000010f10000000000000002afeffff100000000b00000074625f76657273696f6e730048ffffff180000000c0000000000010c38000000010000000800000008ffffff68ffffff140000000c000000000001040c0000000000000024ffffff040000006974656d000000000900000074625f7072756e6564000000a0ffffff140000000c0000000000010f1000000000000000b2feffff100000000700000074625f7265667300ccffffff140000000c0000000000010f1000000000000000defeffff100000000c00000074625f747265655f626c6f62000000001000140010000e000f0004000000080010000000140000000c0000000000010f100000000000000022ffffff100000000f00000074625f726f6f745f747265655f696400a0ffffff180000000c00000000000005100000000000000004000400040000000c0000006d657461646174615f72656600000000d4ffffff140000000c0000000000000f10000000000000008affffff1000000007000000747265655f69640010001400100000000f0004000000080010000000140000000c0000000000000f1000000000000000c6ffffff1000000006000000706172656e74000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000040000000000000000800000073657175656e6365000000000000000000000000000000000000000000000000ffffffff78020000100000000c001a0018001700040008000c000000200000008004000000000000000000000000000304000a0018000c00080004000a000000bc000000100000000100000000000000000000000a000000010000000000000000000000000000000100000000000000000000000000000001000000000000000000000000000000010000000000000000000000000000000100000000000000000000000000000001000000000000000000000000000000010000000000000000000000000000000100000000000000010000000000000000000000000000000000000000000000010000000000000001000000000000000000000016000000000000000000000001000000000000004000000000000000080000000000000080000000000000000100000000000000c0000000000000001000000000000000000100000000000001000000000000004001000000000000100000000000000080010000000000000100000000000000c0010000000000000800000000000000000200000000000020000000000000004002000000000000010000000000000080020000000000001000000000000000c0020000000000000100000000000000000300000000000010000000000000004003000000000000010000000000000080030000000000001000000000000000c00300000000000001000000000000000004000000000000080000000000000040040000000000000000000000000000400400000000000000000000000000004004000000000000000000000000000040040000000000000100000000000000800400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000094f7112c7db38093bcd4d7879177ac2c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000036653962336632366666303135663837386134633365316631363632343464630000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff00000000100000000c00140012000c00080004000c000000580200007402000010000000000004000800080000000400080000000400000009000000fc010000bc0100007c010000440100000c010000c400000094000000380000000400000018ffffff140000000c0000000000010f10000000000000002afeffff100000000b00000074625f76657273696f6e730048ffffff180000000c0000000000010c38000000010000000800000008ffffff68ffffff140000000c000000000001040c0000000000000024ffffff040000006974656d000000000900000074625f7072756e6564000000a0ffffff140000000c0000000000010f1000000000000000b2feffff100000000700000074625f7265667300ccffffff140000000c0000000000010f1000000000000000defeffff100000000c00000074625f747265655f626c6f62000000001000140010000e000f0004000000080010000000140000000c0000000000010f100000000000000022ffffff100000000f00000074625f726f6f745f747265655f696400a0ffffff180000000c00000000000005100000000000000004000400040000000c0000006d657461646174615f72656600000000d4ffffff140000000c0000000000000f10000000000000008affffff1000000007000000747265655f69640010001400100000000f0004000000080010000000140000000c0000000000000f1000000000000000c6ffffff1000000006000000706172656e74000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000040000000000000000800000073657175656e63650000000001000000c002000000000000800200000000000080040000000000000000000000000000900200004152524f5731";

#[test]
fn p_io_3_tree_blob_address_and_ref_table_bytes_are_frozen() {
    // Golden 18: the tree blob storage address is the full-content hash of the
    // canonical tree bytes (decoupled from its semantic TreeId), and the
    // three-column reference-table object bytes are frozen together with their
    // content address.
    let tree_bytes = encode(&sample_image()).unwrap();
    assert_eq!(
        tree_space::layout::tb::tree_blob_address(&tree_bytes),
        Digest::from_bytes(hex16(TREE_BLOB_ADDRESS))
    );
    assert_eq!(tree_id(&tree_bytes).as_bytes(), hex16(TREE_ID));
    assert_ne!(
        hex16(TREE_BLOB_ADDRESS),
        hex16(TREE_ID),
        "tree-blob addressing must stay independent of the TreeId"
    );

    let table_bytes = encode_ref_table(&sample_ref_rows()).unwrap();
    assert_eq!(unhex(REF_TABLE_HEX), table_bytes);
    assert_eq!(
        ref_table_address_of_rows(&sample_ref_rows()).unwrap(),
        Digest::from_bytes(hex16(REF_TABLE_ADDRESS))
    );
}

#[test]
fn p_io_3_tb_commit_batch_and_id_are_frozen() {
    // Golden 19 (PL-1 S5 re-freeze): a nine-column TB commit (sequence 9,
    // parent 0a, tb_root_tree_id 1a, tree blob 2a, refs 3a, tb_pruned null,
    // tb_versions null — this sample carries no side-table pointer). The v4
    // columns carry the constant degradation objects. The commit id uses the
    // extended domain formula (see 01 §1.9.5); adding the `tb_pruned` column
    // (P-IO-5) and the `tb_versions` column (PL-1 S5) re-froze the object
    // bytes but left the id unchanged — neither column enters the five-part
    // domain (01 §3-1).
    let parent = [0x0a; 16];
    let root = [0x1a; 16];
    let tree_blob = [0x2a; 16];
    let refs = [0x3a; 16];
    let batch =
        tree_space::layout::tb::tb_commit_batch(9, parent, root, tree_blob, refs, None, None)
            .unwrap();
    let bytes = encode_batch(&batch).unwrap();
    assert_eq!(unhex(TB_COMMIT_HEX), bytes);
    assert_eq!(
        tree_space::layout::tb::tb_commit_id(9, parent, root, tree_blob, refs),
        Digest::from_bytes(hex16(TB_COMMIT_ID))
    );
}

#[test]
fn p_io_3_commit_writes_tree_blob_and_ref_table_objects() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();

    let mut bucket = Bucket::new();
    let blob_id = bucket.put(&Blob::new(vec![1, 2, 3]));
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob_id))]);
    let tree_bytes = encode(&image).unwrap();
    let leaf_refs = tree_space::layout::tb::image_leaf_refs(&image).unwrap();
    let receipt = library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

    assert_eq!(receipt.sequence, 1);
    assert_eq!(receipt.tb_root_tree_id, tree_id(&tree_bytes).as_bytes());
    assert_eq!(
        receipt.tree_blob,
        tree_space::layout::tb::tree_blob_address(&tree_bytes).as_bytes()
    );

    let root = temp.path().join("library");
    // The canonical tree blob is stored at its content address.
    let tree_path = root.join("tb-trees").join(format!(
        "{}.ipc",
        receipt
            .tree_blob
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    assert_eq!(std::fs::read(&tree_path).unwrap(), tree_bytes);

    // The reference table object is decodable and matches the derived rows.
    let ref_path = root.join("tb-refs").join(format!(
        "{}.ipc",
        receipt
            .refs
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    let rows =
        tree_space::layout::tb::decode_ref_table(&std::fs::read(&ref_path).unwrap()).unwrap();
    let bucket_bytes = Blob::new(vec![1, 2, 3]).envelope().encode();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].address,
        block_blob_address(&bucket_bytes).as_bytes()
    );
    assert_eq!(rows[0].ref_id, blob_id.as_bytes());
}

#[test]
fn p_io_3_ref_table_derivation_is_canonical_and_dedups() {
    // Derivation dedups on (xpath, ref_id), sorts canonically by xpath bytes,
    // and re-encoding the same rows yields byte-identical objects.
    let mut bucket = Bucket::new();
    let blob = Blob::new(vec![7]);
    let blob_id = bucket.put(&blob);
    let leaf_refs = vec![
        (XPath::root().field("chunks").index(0), blob_id),
        (XPath::root().field("chunks").index(1), blob_id),
        (XPath::root().field("chunks").index(1), blob_id),
        (XPath::root().field("single"), blob_id),
    ];
    let rows = tree_space::layout::tb::derive_ref_rows(&leaf_refs, &bucket).unwrap();
    // (xpath, ref_id) is unique; the duplicate third entry collapses.
    assert_eq!(rows.len(), 3);
    assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
    let address = block_blob_address(&blob.envelope().encode()).as_bytes();
    assert!(rows.iter().all(|row| row.address == address));

    let bytes_a = encode_ref_table(&rows).unwrap();
    let bytes_b = encode_ref_table(&rows).unwrap();
    assert_eq!(bytes_a, bytes_b);
}

#[test]
fn p_io_3_commit_chain_extends_from_genesis() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&Blob::new(vec![4, 5, 6]));
    let image = TreeImage::new(vec![named_field(
        "blob",
        ImageContent::Ref(Blob::new(vec![4, 5, 6]).ref_id()),
    )]);
    let tree_bytes = encode(&image).unwrap();
    let leaf_refs = tree_space::layout::tb::image_leaf_refs(&image).unwrap();

    let first = library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    let second = library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);

    // The head commit's parent links back to the first TB commit, whose parent
    // is the v4 genesis commit.
    let batch =
        decode_batch(&std::fs::read(temp.path().join("library").join("committed")).unwrap())
            .unwrap();
    let head = batch
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    assert_eq!(head, second.commit_id.as_bytes().to_vec());

    let second_commit = decode_batch(
        &std::fs::read(
            temp.path()
                .join("library")
                .join("commits")
                .join(format!("{}.ipc", second.commit_id)),
        )
        .unwrap(),
    )
    .unwrap();
    let parent = second_commit
        .column(1)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    assert_eq!(parent, first.commit_id.as_bytes().to_vec());
}

#[test]
fn p_io_3_tb_genesis_commit_stays_four_columns() {
    // The genesis commit of a fresh TB library is the v4 genesis object: a
    // four-column batch (no TB append columns). `TbLibrary::create` must not
    // extend it (TB-side assertion; the pure-v4 half — that a v4 library
    // creates no TB channel directories — was deleted by PL-1 S7, 02 §5.9).
    let temp = tempfile::tempdir().unwrap();
    let _library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let genesis_file = std::fs::read_dir(temp.path().join("library").join("commits"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let genesis_batch = decode_batch(&std::fs::read(genesis_file).unwrap()).unwrap();
    assert_eq!(genesis_batch.num_columns(), 4);
}

#[test]
fn p_io_3_gc_keeps_referenced_and_sweeps_orphans() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let a_env = Blob::new(vec![0xaa]).envelope();
    let b_env = Blob::new(vec![0xbb]).envelope();
    let a = tree_space::block::block_ref_id(a_env.kind.clone(), &a_env.payload);
    let b = tree_space::block::block_ref_id(b_env.kind.clone(), &b_env.payload);

    let tree_ab = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(a)),
        named_field("b", ImageContent::Ref(b)),
    ]);
    let tree_a = TreeImage::new(vec![named_field("a", ImageContent::Ref(a))]);

    let mut bucket = Bucket::new();
    bucket.put_envelope(a_env.clone()).unwrap();
    bucket.put_envelope(b_env.clone()).unwrap();

    let tb_ab = encode(&tree_ab).unwrap();
    let leaf_ab = tree_space::layout::tb::image_leaf_refs(&tree_ab).unwrap();
    let receipt_ab = library.commit(&tb_ab, &leaf_ab, &bucket).unwrap();

    let tb_a = encode(&tree_a).unwrap();
    let leaf_a = tree_space::layout::tb::image_leaf_refs(&tree_a).unwrap();
    let receipt_a = library.commit(&tb_a, &leaf_a, &bucket).unwrap();
    assert_eq!(receipt_a.sequence, receipt_ab.sequence + 1);

    // After committing the A-only head, B's blob, the AB tree blob, its ref
    // table and the AB commit are orphans. GC must reclaim them while keeping
    // A and everything referenced by the head.
    let report = library.gc(receipt_a.sequence).unwrap();
    assert!(report.reclaimed >= 3);

    let root = temp.path().join("library");
    let a_addr = block_blob_address(&a_env.encode());
    assert!(
        root.join("tb-blocks")
            .join(format!("{a_addr}.bin"))
            .exists()
    );
    assert!(
        !root
            .join("tb-blocks")
            .join(format!("{}.bin", block_blob_address(&b_env.encode())))
            .exists()
    );
    assert!(
        !root
            .join("tb-trees")
            .join(format!(
                "{}.ipc",
                receipt_ab
                    .tree_blob
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ))
            .exists()
    );
    assert!(
        !root
            .join("tb-refs")
            .join(format!(
                "{}.ipc",
                receipt_ab
                    .refs
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ))
            .exists()
    );
    assert!(
        !root
            .join("commits")
            .join(format!("{}.ipc", receipt_ab.commit_id))
            .exists()
    );

    // GC never decodes tree bytes: writing garbage over the (already swept)
    // stale tree blob is harmless because reclamation is driven by the ref
    // table only.
    let stale_tree = root.join("tb-trees").join(format!(
        "{}.ipc",
        receipt_ab
            .tree_blob
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    let _ = std::fs::write(&stale_tree, b"\x00\x01\x02");
    let _ = report;
}

#[test]
fn p_io_3_ref_table_decode_rejects_unsorted_rows() {
    // Negative: a reference-table object with unsorted xpath rows must be
    // rejected on decode.
    let mut rows = sample_ref_rows();
    assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
    rows.reverse();
    let bytes = encode_ref_table(&rows).unwrap();
    let error = tree_space::layout::tb::decode_ref_table(&bytes).unwrap_err();
    assert_eq!(error.code, tree_space::ErrorCode::SchemaMismatch);
}

#[test]
fn p_io_3_open_rejects_half_extended_commit() {
    // Negative: a commit with an incomplete TB column set (5 or 6 columns)
    // must be rejected when the head commit is read.
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&Blob::new(vec![1]));
    let image = TreeImage::new(vec![named_field(
        "x",
        ImageContent::Ref(Blob::new(vec![1]).ref_id()),
    )]);
    let tb = encode(&image).unwrap();
    let leaf = tree_space::layout::tb::image_leaf_refs(&image).unwrap();
    let receipt = library.commit(&tb, &leaf, &bucket).unwrap();

    // Rebuild the head commit as a six-column batch (only tb_root_tree_id and
    // tb_tree_blob appended).
    let path = temp
        .path()
        .join("library")
        .join("commits")
        .join(format!("{}.ipc", receipt.commit_id));
    let batch = decode_batch(&std::fs::read(&path).unwrap()).unwrap();
    let schema = Arc::new(Schema::new(
        batch
            .schema()
            .fields()
            .iter()
            .take(6)
            .map(|field| Arc::new(field.as_ref().clone()))
            .collect::<Vec<_>>(),
    ));
    let mut columns = batch.columns().to_vec();
    columns.truncate(6);
    let six = RecordBatch::try_new(schema, columns).unwrap();
    std::fs::write(&path, encode_batch(&six).unwrap()).unwrap();

    let error = match tree_space::TbLibrary::open(temp.path().join("library")) {
        Ok(_) => panic!("half-extended commit must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::SchemaMismatch);
}

#[test]
fn p_io_3_open_rejects_wrong_append_column_type() {
    // Negative: a commit whose tb_root_tree_id column has a wrong Arrow type
    // must be rejected.
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&Blob::new(vec![2]));
    let image = TreeImage::new(vec![named_field(
        "x",
        ImageContent::Ref(Blob::new(vec![2]).ref_id()),
    )]);
    let tb = encode(&image).unwrap();
    let leaf = tree_space::layout::tb::image_leaf_refs(&image).unwrap();
    let receipt = library.commit(&tb, &leaf, &bucket).unwrap();

    let path = temp
        .path()
        .join("library")
        .join("commits")
        .join(format!("{}.ipc", receipt.commit_id));
    let batch = decode_batch(&std::fs::read(&path).unwrap()).unwrap();
    // Both the schema and the data of column 4 are switched to a wrong type.
    let fields = batch
        .schema()
        .fields()
        .iter()
        .enumerate()
        .map(|(index, field)| {
            Arc::new(field.as_ref().clone().with_data_type(if index == 4 {
                DataType::Utf8
            } else {
                field.data_type().clone()
            })) as Arc<arrow::datatypes::Field>
        })
        .collect::<Vec<_>>();
    let mut columns = batch.columns().to_vec();
    columns[4] = Arc::new(StringArray::from(vec!["not-a-16-byte-id".to_owned()]));
    let wrong = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap();
    std::fs::write(&path, encode_batch(&wrong).unwrap()).unwrap();

    let error = match tree_space::TbLibrary::open(temp.path().join("library")) {
        Ok(_) => panic!("wrong append column type must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::SchemaMismatch);
}
