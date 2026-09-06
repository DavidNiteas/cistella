//! PL-1 S4: eight-step boot chain + ref-table-driven bucket rebuild
//! (`_dev/插件化改造/02-施工路线图.md` §5.6 / 01 §4-3 / §4-5).
//!
//! Three anchors:
//!
//! - `p_l_1_ref_driven_equivalent`: a healthy library reopens through the
//!   reference-table-driven restore and its bucket envelope set equals the
//!   legacy scan-based rebuild (`block_objects()` + probe + decode), on both
//!   disk layouts — behavioral equivalence of the open path;
//! - `p_l_1_open_missing_object`: a ref-table row addressing a missing block
//!   object is [`ErrorCode::DanglingReference`] (object missing ≠ plugin
//!   missing, the future degraded branch);
//! - `p_l_1_open_order_boot_first`: a committed library with its boot record
//!   removed fails the bootstrap at step 1 with [`ErrorCode::BootstrapIncomplete`]
//!   — the boot chain runs before any commit reading (the error is the step-1
//!   boot error, not a later-step code).

use tempfile::tempdir;
use tree_space::layout::flat_dir::FlatDirLayout;
use tree_space::layout::single_file::SingleFileLayout;
use tree_space::tree::codec::TreeImage;
use tree_space::{
    Blob, Block, Bucket, ErrorCode, ImageContent, Sequence, SingleFileTbLibrary, TbLayout,
    TbLibrary, Value, block_blob_address, encode, image_leaf_refs, materializer_for, named_field,
    probe_block_materialization,
};

/// The healthy fixture: two distinct leaf blocks (`blob` + `sequence`) — two
/// reference-table rows, two `tb-blocks/` objects.
fn healthy_tree() -> TreeImage {
    TreeImage::new(vec![
        named_field("blob", ImageContent::Ref(Blob::new(vec![0xbb]).ref_id())),
        named_field(
            "seq",
            ImageContent::Ref(Sequence::new(vec![Value::I32(5)]).ref_id()),
        ),
    ])
}

fn healthy_bucket() -> Bucket {
    let mut bucket = Bucket::new();
    bucket.put(&Blob::new(vec![0xbb]));
    bucket.put(&Sequence::new(vec![Value::I32(5)]));
    bucket
}

/// The legacy scan-based bucket rebuild (the S3-era `restore_bucket` behavior),
/// replicated through the public surface: list every `tb-blocks/` object,
/// assert its addressing name, probe and decode through the materializer.
fn scan_restore(layout: &impl TbLayout) -> Bucket {
    let mut bucket = Bucket::new();
    for (address, bytes) in layout.block_objects().expect("block listing reads") {
        assert_eq!(
            address.as_bytes(),
            block_blob_address(&bytes).as_bytes(),
            "tb-blocks blob content does not match its addressing name"
        );
        let format = probe_block_materialization(&bytes).expect("probeable object");
        let envelope = materializer_for(format)
            .decode(&bytes)
            .expect("decodable object");
        bucket.put_envelope(envelope).expect("canonical envelope");
    }
    bucket
}

/// Compares two bucket envelope sets item by item (identity set + envelopes).
fn assert_same_envelope_set(ref_driven: &Bucket, scan: &Bucket) {
    let mut ref_ids = ref_driven.ids().collect::<Vec<_>>();
    ref_ids.sort();
    let mut scan_ids = scan.ids().collect::<Vec<_>>();
    scan_ids.sort();
    assert_eq!(ref_driven.len(), scan.len(), "envelope-set size");
    assert_eq!(ref_ids, scan_ids, "envelope identity set");
    for id in &ref_ids {
        assert_eq!(
            ref_driven.get(*id),
            scan.get(*id),
            "envelope {id} equals the scan-rebuilt one"
        );
    }
}

#[test]
fn p_l_1_ref_driven_equivalent() {
    // FlatDir: create → commit → open (ref-table driven) ≡ scan rebuild.
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();
    let tree = healthy_tree();
    let bucket = healthy_bucket();
    let tree_bytes = encode(&tree).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&tree).unwrap(), &bucket)
        .unwrap();
    let reopened = TbLibrary::open(&root).unwrap();
    assert_same_envelope_set(
        reopened.bucket().unwrap(),
        &scan_restore(&FlatDirLayout::new(&root)),
    );
    assert_eq!(reopened.verify().unwrap(), 2, "both leaves verified");

    // SingleFile: the same equality through the container layout.
    let temp = tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let single = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    single
        .commit(&tree_bytes, &image_leaf_refs(&tree).unwrap(), &bucket)
        .unwrap();
    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert_same_envelope_set(
        reopened.bucket().unwrap(),
        &scan_restore(&SingleFileLayout::new(&path)),
    );
    assert_eq!(reopened.verify().unwrap(), 2, "both leaves verified");
}

#[test]
fn p_l_1_open_missing_object() {
    // A ref-table row whose address has no object file → DanglingReference
    // (对象缺失 ≠ 插件缺失): the ref-driven restore reads per row, so a hole
    // under a referenced address is an error even though the directory-scan
    // rebuild would silently skip it.
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();
    let tree = healthy_tree();
    let bucket = healthy_bucket();
    let tree_bytes = encode(&tree).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&tree).unwrap(), &bucket)
        .unwrap();

    let blob_address = block_blob_address(&Blob::new(vec![0xbb]).envelope().encode());
    std::fs::remove_file(root.join("tb-blocks").join(format!("{blob_address}.bin"))).unwrap();

    let error = match TbLibrary::open(&root) {
        Ok(_) => panic!("open must reject a ref-table row addressing a missing object"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DanglingReference);
    assert_ne!(
        error.code,
        ErrorCode::SchemaMismatch,
        "a missing object is not a plugin-routing miss"
    );
}

#[test]
fn p_l_1_open_order_boot_first() {
    // Boot first: with the boot record present the committed library opens
    // cleanly (steps 3+ are healthy); removing the boot record makes the open
    // fail at chain step 1 with BootstrapIncomplete — never with a later
    // commit/ref/block error.
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();
    let tree = healthy_tree();
    let bucket = healthy_bucket();
    let tree_bytes = encode(&tree).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&tree).unwrap(), &bucket)
        .unwrap();

    // Control: the same library opens through the full chain (commit + tree +
    // refs + blocks all fine).
    assert!(TbLibrary::open(&root).is_ok(), "control open succeeds");

    std::fs::remove_file(root.join("tb-boot").join("boot.ipc")).unwrap();
    let error = match TbLibrary::open(&root) {
        Ok(_) => panic!("missing boot record must fail the bootstrap"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::BootstrapIncomplete);
    assert!(
        error.message.contains("boot"),
        "the failure is the step-1 boot read, not a later-chain error: {}",
        error.message
    );
    // The later-step error categories must not leak: the commit was complete.
    assert_ne!(error.code, ErrorCode::DanglingReference);
    assert_ne!(error.code, ErrorCode::DigestMismatch);
    assert_ne!(error.code, ErrorCode::SchemaMismatch);
}
