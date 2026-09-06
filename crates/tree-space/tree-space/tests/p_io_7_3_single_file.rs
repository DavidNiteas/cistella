//! P-IO-7.3 单文件 (02 §9.5): the append-only single-file container as the
//! second complete `TbLayout` implementation.
//!
//! The single-file TB library packages every commit as one epoch whose payload
//! is the complete object-directory snapshot (address → offset/length), so
//! reopening reads the newest epoch, locates every object and rebuilds the
//! bucket and head commit. This file covers the §9.5 test obligations:
//!
//! - roundtrip: commit → open → verify/project equivalent to the flat layout,
//!   and the *content-addressed object bytes* are byte-identical between the
//!   two layouts (对象字节相同、容器不同);
//! - epoch append: every commit appends exactly one tail trailer (genesis + one
//!   epoch per commit), and reopening returns the newest head;
//! - dedup: one block referenced from several xpaths — and across commits — is
//!   stored physically once;
//! - force-IPC: a parquet materialization attempt on the single-file layout is
//!   rejected and stored objects all probe `ARROW1`;
//! - GC: a compaction repack keeps the `keep_from_sequence` window and compacts
//!   out-of-window/orphan objects away;
//! - crash recovery: a corrupt final trailer falls back to the previous valid
//!   epoch (reusing the v4 container recovery semantics) and appends continue
//!   off the recovered tail;
//! - pruned commits reopen ephemeral fields to `Default` through the same
//!   write path.
//!
//! No new byte golden is frozen here: every equality is behavioral, structural
//! (epoch/trailer positions) or content-identity.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::layout::{FlatDirLayout, SingleFileLayout};
use tree_space::tree::codec::{
    EncodeTree, ImageContent, TreeImage, encode, encode_node, named_field,
};
use tree_space::xpath::XPath;
use tree_space::{
    ArrowTable, Blob, Block, Bucket, Digest, ErrorCode, Materialization, RefId,
    SingleFileTbLibrary, TbLayout, TbLibrary, TreeCodec, TreeNode, TreeNodeMeta,
    block_blob_address, canonical_xpath_bytes, image_leaf_refs, probe_block_materialization,
    tb_commit_batch, tree_blob_address,
};

/// The container trailer magic, matching the layout's `TRAILER_MAGIC`.
const TRAILER_MAGIC: &[u8; 8] = b"TSTRAIL\0";

fn sample_table(rows: &[i32]) -> ArrowTable {
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

/// Builds a tree image with one named `Ref` field per entry and returns its
/// encoded bytes plus its canonical `(XPath, RefId)` leaf references.
fn image_of(fields: &[(&str, RefId)]) -> (Vec<u8>, Vec<(XPath, RefId)>) {
    let fields = fields
        .iter()
        .map(|(name, id)| named_field(name, ImageContent::Ref(*id)))
        .collect::<Vec<_>>();
    let image = TreeImage::new(fields);
    let bytes = encode(&image).unwrap();
    let leaf_refs = image_leaf_refs(&image).unwrap();
    (bytes, leaf_refs)
}

/// The sequence (generation) of every structurally aligned trailer in the file.
fn trailer_generations(bytes: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut start = 64usize;
    while start + 64 <= bytes.len() {
        if &bytes[start..start + 8] == TRAILER_MAGIC {
            out.push(u64::from_le_bytes(
                bytes[start + 12..start + 20].try_into().unwrap(),
            ));
        }
        start += 64;
    }
    out
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Surface {
    data: ArrowTable,
    blob: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Pruned {
    data: ArrowTable,
    #[tree_space(ephemeral)]
    scratch: Vec<ArrowTable>,
}

// ---------------------------------------------------------------------------
// Roundtrip + flat-equivalence
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_roundtrip_verify_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    let data = sample_table(&[1, 2, 3]);
    let blob = Blob::new(vec![9, 8, 7]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&blob);
    let tree = Surface {
        data: data.clone(),
        blob: blob.clone(),
    };
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();

    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(reopened.verify().unwrap(), 2);
    assert_eq!(reopened.bucket().unwrap().len(), 2);
    let projected: Surface = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);
    assert_eq!(projected.data.ref_id(), data.ref_id());
    assert_eq!(projected.blob.envelope().payload, blob.envelope().payload);
}

#[test]
fn p_io_7_3_object_bytes_identical_to_flat_dir() {
    // 02 §9.5 closing standard: the object *bytes* are the same across layouts,
    // only the container differs. Same tree + bucket → same content-addressed
    // digests and byte-for-byte identical block/tree/ref/commit objects.
    let temp = tempfile::tempdir().unwrap();
    let flat_root = temp.path().join("flat");
    let file_path = temp.path().join("library.umdb");
    let flat = TbLibrary::<FlatDirLayout>::create(&flat_root).unwrap();
    let single = TbLibrary::<SingleFileLayout>::create(&file_path).unwrap();

    let data = sample_table(&[4, 5]);
    let blob = Blob::new(vec![1, 2]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&blob);
    let tree = Surface {
        data: data.clone(),
        blob: blob.clone(),
    };
    let tree_bytes = encode_node(&tree).unwrap();
    let leaf_refs = tree.leaf_refs();
    let receipt_flat = flat.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    let receipt_single = single.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

    assert_eq!(receipt_flat.commit_id, receipt_single.commit_id);
    assert_eq!(receipt_flat.tb_root_tree_id, receipt_single.tb_root_tree_id);
    assert_eq!(receipt_flat.tree_blob, receipt_single.tree_blob);
    assert_eq!(receipt_flat.refs, receipt_single.refs);

    let single_layout = tree_space::layout::SingleFileLayout::new(&file_path);
    for (_, ref_id) in &leaf_refs {
        let envelope = bucket.get(*ref_id).unwrap();
        let address = block_blob_address(&envelope.encode());
        let flat_bytes =
            std::fs::read(flat_root.join("tb-blocks").join(format!("{address}.bin"))).unwrap();
        let single_bytes = single_layout.read_block_object(address).unwrap();
        assert_eq!(
            flat_bytes, single_bytes,
            "block object bytes match across layouts"
        );
    }
    let tree_addr = tree_blob_address(&tree_bytes);
    assert_eq!(
        std::fs::read(flat_root.join("tb-trees").join(format!("{tree_addr}.ipc"))).unwrap(),
        single_layout.read_tree_object(tree_addr).unwrap(),
        "tree blob bytes match across layouts"
    );
    let ref_addr = Digest::from_bytes(receipt_flat.refs);
    assert_eq!(
        std::fs::read(flat_root.join("tb-refs").join(format!("{ref_addr}.ipc"))).unwrap(),
        single_layout.read_ref_object(ref_addr).unwrap(),
        "reference-table bytes match across layouts"
    );

    // The commit object is the deterministic nine-column canonical batch (02
    // §5.7: the ninth `tb_versions` column points at the per-commit version
    // side-table). The side-table rows are derived from the reference-table
    // rows — the leaf xpaths already sorted canonically — with every disk
    // version "arrow-ipc" (this IPC-only layout materializes every block as
    // IPC; native bytes stay on the IPC path).
    let genesis = tree_space::layout::flat_dir::commit_id_golden(
        0,
        [0; 16],
        tree_space::empty_tree_id(),
        tree_space::empty_metadata_ref().unwrap(),
    );
    let ref_rows = tree_space::layout::tb::decode_ref_table(
        &std::fs::read(flat_root.join("tb-refs").join(format!("{ref_addr}.ipc"))).unwrap(),
    )
    .unwrap();
    let version_rows = ref_rows
        .iter()
        .map(|row| tree_space::layout::tb::VersionRow {
            xpath: row.xpath.clone(),
            mem_ver: "arrow55".to_owned(),
            disk_ver: "arrow-ipc".to_owned(),
        })
        .collect::<Vec<_>>();
    let versions_bytes = tree_space::layout::tb::encode_versions_table(&version_rows).unwrap();
    let versions_address = tree_space::layout::tb::versions_table_address(&versions_bytes);
    let commit_batch = tb_commit_batch(
        receipt_flat.sequence,
        genesis.as_bytes(),
        receipt_flat.tb_root_tree_id,
        receipt_flat.tree_blob,
        receipt_flat.refs,
        None,
        Some(versions_address.as_bytes()),
    )
    .unwrap();
    let commit_bytes = tree_space::ipc::encode_batch(&commit_batch).unwrap();
    assert_eq!(
        commit_bytes,
        std::fs::read(
            flat_root
                .join("commits")
                .join(format!("{}.ipc", receipt_flat.commit_id))
        )
        .unwrap(),
        "commit object bytes are the canonical batch"
    );
    let single_commit = single_layout
        .read_commit(&single_layout.commit_path(receipt_single.commit_id))
        .unwrap();
    assert_eq!(single_commit.sequence, receipt_single.sequence);
    assert_eq!(single_commit.tb.as_ref().unwrap().refs, receipt_single.refs);
    assert_eq!(
        single_commit.tb.as_ref().unwrap().tree_blob,
        receipt_single.tree_blob
    );
}

// ---------------------------------------------------------------------------
// Epoch append semantics
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_each_commit_appends_one_epoch_and_reopens_latest_head() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    let t1 = sample_table(&[1]);
    let b1 = Blob::new(vec![10]);
    let mut bucket1 = Bucket::new();
    bucket1.put(&t1);
    bucket1.put(&b1);
    let tree1 = Surface {
        data: t1.clone(),
        blob: b1.clone(),
    };
    let bytes1 = encode_node(&tree1).unwrap();
    let receipt1 = library
        .commit(&bytes1, &tree1.leaf_refs(), &bucket1)
        .unwrap();

    let t2 = sample_table(&[2]);
    let b2 = Blob::new(vec![20]);
    let mut bucket2 = Bucket::new();
    bucket2.put(&t2);
    bucket2.put(&b2);
    let tree2 = Surface {
        data: t2.clone(),
        blob: b2.clone(),
    };
    let bytes2 = encode_node(&tree2).unwrap();
    let receipt2 = library
        .commit(&bytes2, &tree2.leaf_refs(), &bucket2)
        .unwrap();

    assert_eq!(receipt1.sequence, 1);
    assert_eq!(receipt2.sequence, 2);

    // Each commit is exactly one epoch: genesis + one trailer per commit, in
    // sequence order.
    let raw = std::fs::read(&path).unwrap();
    assert_eq!(trailer_generations(&raw), vec![0, 1, 2]);

    // Reopening takes the newest head.
    let layout = tree_space::layout::SingleFileLayout::new(&path);
    let (_, head_sequence) = layout.read_head().unwrap().unwrap();
    assert_eq!(head_sequence, receipt2.sequence);

    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    let projected: Surface = reopened.project().unwrap();
    assert_eq!(projected.data.ref_id(), t2.ref_id());
    assert_eq!(projected.data.envelope().payload, t2.envelope().payload);
}

// ---------------------------------------------------------------------------
// Object dedup
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_referenced_block_stored_physically_once() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    let shared = sample_table(&[7]);
    let mut bucket = Bucket::new();
    bucket.put(&shared);

    // One block referenced from two xpaths in the same tree.
    let image = TreeImage::new(vec![
        named_field("first", ImageContent::Ref(shared.ref_id())),
        named_field("second", ImageContent::Ref(shared.ref_id())),
    ]);
    let bytes = encode(&image).unwrap();
    let leaf_refs = image_leaf_refs(&image).unwrap();
    library.commit(&bytes, &leaf_refs, &bucket).unwrap();

    let layout = tree_space::layout::SingleFileLayout::new(&path);
    let objects = layout.block_objects().unwrap();
    assert_eq!(objects.len(), 1, "one physical copy per referenced block");

    // Re-committing the same tree does not duplicate the block either
    // (catalog `exists → skip` across epochs).
    library.commit(&bytes, &leaf_refs, &bucket).unwrap();
    assert_eq!(
        layout.block_objects().unwrap().len(),
        1,
        "cross-commit dedup keeps a single physical copy"
    );

    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        2,
        "the reference table still records both xpaths"
    );
    assert_eq!(reopened.bucket().unwrap().len(), 1);
    assert_eq!(reopened.index().unwrap().iter().count(), 2);
}

// ---------------------------------------------------------------------------
// Force-IPC
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_parquet_materialization_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let error =
        TbLibrary::<SingleFileLayout>::create_with_materialization(&path, Materialization::Parquet)
            .err()
            .unwrap();
    assert_eq!(error.code, ErrorCode::TargetFrozen);
    assert!(!path.exists(), "a rejected create writes nothing");

    let layout = tree_space::layout::SingleFileLayout::new(&path);
    assert_eq!(layout.block_materialization(), Materialization::Ipc);

    // A real single-file library stores every block as IPC regardless of any
    // selection attempt: all objects probe `ARROW1`.
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    let data = sample_table(&[3]);
    let blob = Blob::new(vec![0xaa]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&blob);
    let tree = Surface {
        data: data.clone(),
        blob: blob.clone(),
    };
    let bytes = encode_node(&tree).unwrap();
    library.commit(&bytes, &tree.leaf_refs(), &bucket).unwrap();
    for (_, block) in layout.block_objects().unwrap() {
        assert_eq!(
            probe_block_materialization(&block).unwrap(),
            Materialization::Ipc
        );
    }
}

// ---------------------------------------------------------------------------
// GC compaction repack
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_gc_compaction_preserves_window_reclaims_orphans() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    let a = sample_table(&[1]);
    let b = sample_table(&[2]);
    let mut bucket_a = Bucket::new();
    bucket_a.put(&a);
    let (bytes_a, leafs_a) = image_of(&[("a", a.ref_id())]);
    library.commit(&bytes_a, &leafs_a, &bucket_a).unwrap();

    let mut bucket_b = Bucket::new();
    bucket_b.put(&b);
    let (bytes_b, leafs_b) = image_of(&[("b", b.ref_id())]);
    library.commit(&bytes_b, &leafs_b, &bucket_b).unwrap();

    let layout = tree_space::layout::SingleFileLayout::new(&path);
    let a_address = block_blob_address(&a.envelope().encode()).as_bytes();
    let b_address = block_blob_address(&b.envelope().encode()).as_bytes();
    assert_eq!(layout.block_objects().unwrap().len(), 2);

    // GC with the window at the latest commit (sequence 2): commit 1's history
    // is out of window, so its unique block becomes an orphan and is compacted
    // away; the in-window commit-2 block survives.
    let report = library.gc(2).unwrap();
    assert!(report.reclaimed >= 1, "out-of-window objects are reclaimed");

    let blocks = layout.block_objects().unwrap();
    let addresses = blocks
        .iter()
        .map(|(digest, _)| digest.as_bytes())
        .collect::<Vec<_>>();
    assert!(addresses.contains(&b_address), "in-window block survives");
    assert!(
        !addresses.contains(&a_address),
        "orphan/out-of-window block is compacted away"
    );

    // Reopen after the repack: head is commit 2, verify clean, only b restored.
    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    let restored = reopened.bucket().unwrap();
    assert!(restored.get(b.ref_id()).is_some());
    assert!(restored.get(a.ref_id()).is_none());

    // The file remains a fully appendable single file after compaction.
    let mut bucket = Bucket::new();
    bucket.put(&a);
    bucket.put(&b);
    let (bytes_ab, leafs_ab) = image_of(&[("a", a.ref_id()), ("b", b.ref_id())]);
    let receipt = reopened.commit(&bytes_ab, &leafs_ab, &bucket).unwrap();
    assert_eq!(receipt.sequence, 3);
    let after = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(after.verify().unwrap(), 2);
}

// ---------------------------------------------------------------------------
// Crash recovery
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_corrupt_final_trailer_falls_back_to_previous_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    let a = sample_table(&[1]);
    let b = sample_table(&[2]);
    let mut bucket_a = Bucket::new();
    bucket_a.put(&a);
    let (bytes_a, leafs_a) = image_of(&[("a", a.ref_id())]);
    library.commit(&bytes_a, &leafs_a, &bucket_a).unwrap();

    let mut bucket_ab = Bucket::new();
    bucket_ab.put(&a);
    bucket_ab.put(&b);
    let (bytes_ab, leafs_ab) = image_of(&[("a", a.ref_id()), ("b", b.ref_id())]);
    library.commit(&bytes_ab, &leafs_ab, &bucket_ab).unwrap();

    // Corrupt the final trailer's checksum region in place.
    let mut raw = std::fs::read(&path).unwrap();
    let trailer_start = raw.len() - 64;
    for byte in &mut raw[trailer_start + 60..trailer_start + 64] {
        *byte ^= 0xff;
    }
    std::fs::write(&path, &raw).unwrap();

    // Opening falls back to the previous valid epoch (commit 1): only `a` is
    // reachable and the head is the commit-1 sequence.
    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    let restored = reopened.bucket().unwrap();
    assert!(restored.get(a.ref_id()).is_some());
    assert!(
        restored.get(b.ref_id()).is_none(),
        "commit-2-only block is unreachable after rollback"
    );

    // Appends continue off the recovered tail.
    let receipt = reopened.commit(&bytes_ab, &leafs_ab, &bucket_ab).unwrap();
    assert_eq!(receipt.sequence, 2, "the recovered head was sequence 1");
    let after = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(after.verify().unwrap(), 2);
    let after_layout = tree_space::layout::SingleFileLayout::new(&path);
    let (_, head_sequence) = after_layout.read_head().unwrap().unwrap();
    assert_eq!(head_sequence, 2);
}

// ---------------------------------------------------------------------------
// Genesis / empty-library semantics
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_fresh_library_has_genesis_head_and_opens_as_empty() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    // The genesis v4 epoch yields the genesis head so a first TB commit chains
    // off the same genesis as the flat layout.
    let layout = SingleFileLayout::new(&path);
    let (_, head_sequence) = layout.read_head().unwrap().unwrap();
    assert_eq!(head_sequence, 0);

    // Opening a never-committed library fails cleanly ("not a TB commit"),
    // matching the flat-layout behavior.
    let error = TbLibrary::<SingleFileLayout>::open(&path).err().unwrap();
    assert_eq!(error.code, ErrorCode::BootstrapIncomplete);
}

// ---------------------------------------------------------------------------
// Pruned commits share the write path
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_3_pruned_commit_reopens_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    let data = sample_table(&[5]);
    let scratch = sample_table(&[6]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&scratch);
    let tree = Pruned {
        data: data.clone(),
        scratch: vec![scratch.clone()],
    };
    let fields = EncodeTree::tree_children(&tree).unwrap();
    let prefixes = <Pruned as TreeNodeMeta>::ephemeral_prefixes()
        .iter()
        .map(canonical_xpath_bytes)
        .collect::<Vec<_>>();
    library.commit_pruned(&fields, &prefixes, &bucket).unwrap();

    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        1,
        "the ephemeral scratch block is pruned from the reference table"
    );
    let projected: Pruned = reopened.project().unwrap();
    assert_eq!(projected.data.ref_id(), data.ref_id());
    assert!(
        projected.scratch.is_empty(),
        "ephemeral container reopens to Default"
    );
}
