//! E-4: disk zero-copy mapping + invalidation (协议 `交换协议/02-施工路线图.md`
//! §8.4) — the 01 §7 zero-copy mapped read's disk base.
//!
//! Asserts the §8.4 obligations for `tests/e_4_disk_mmap.rs`: tree/block disk
//! mapping equals the object reads on both layouts (flat = object file mmap,
//! single-file = whole `.umdb` mmap + catalog offset slice), missing-object
//! rejection (`DanglingReference`), layout probing
//! (`detect_disk_layout`), the `MappedView::open(Source::Disk)` committed-tree
//! roundtrip with real `UpstreamChanged` / `UpstreamDead` exposure on
//! `refresh()` (01 §7 cases 1 + 2), the preserved `Send + Sync` bound, and
//! `persist_block` address/dedup/GC-reachability semantics.

use std::path::Path;
use tree_space::layout::{FlatDirLayout, SingleFileLayout};
use tree_space::tree::codec::{ImageContent, TreeImage, encode, named_field};
use tree_space::{
    Blob, Block, Bucket, Digest, ErrorCode, InvalidState, MappedView, SingleFileTbLibrary, Source,
    StorageKind, TbLayout, TbLibrary, block_blob_address, detect_disk_layout, image_leaf_refs,
    map_block_disk, map_ref_disk, map_tree_disk, open_tree_mapped, persist_block,
};

/// Commits a one-blob library under `root` and reopens it, returning the
/// loaded handle plus the canonical tree bytes, the tree-blob address, the
/// block address and the block envelope bytes.
fn committed_flat(root: &Path) -> (TbLibrary, Vec<u8>, Digest, Digest, Vec<u8>) {
    let library = TbLibrary::create(root).unwrap();
    let blob = Blob::new(vec![3, 1, 4]);
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    let receipt = library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let block_bytes = blob.envelope().encode();
    let block_addr = block_blob_address(&block_bytes);
    drop(library);
    (
        TbLibrary::open(root).unwrap(),
        tree_bytes,
        Digest::from_bytes(receipt.tree_blob),
        block_addr,
        block_bytes,
    )
}

/// Commits a one-blob single-file library at `path`, returning the loaded
/// handle plus the blob envelope bytes and the block address.
fn committed_single(path: &Path) -> (TbLibrary<SingleFileLayout>, Vec<u8>, Digest) {
    let library = TbLibrary::<SingleFileLayout>::create(path).unwrap();
    let blob = Blob::new(vec![6, 6]);
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let block_bytes = blob.envelope().encode();
    let block_addr = block_blob_address(&block_bytes);
    (library, block_bytes, block_addr)
}

#[test]
fn e_4_map_tree_disk_flat() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, tree_bytes, tree_blob, _, _) = committed_flat(&root);
    drop(library);

    let layout = FlatDirLayout::new(&root);
    let mapped = map_tree_disk(&layout).unwrap();
    assert!(mapped.is_file_mapped());
    assert_eq!(mapped.bytes(), tree_bytes.as_slice());
    assert_eq!(
        mapped.bytes(),
        layout.read_tree_object(tree_blob).unwrap().as_slice()
    );
}

#[test]
fn e_4_map_block_disk_flat() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, _, _, block_addr, block_bytes) = committed_flat(&root);
    drop(library);

    let layout = FlatDirLayout::new(&root);
    let mapped = map_block_disk(&layout, block_addr).unwrap();
    assert!(mapped.is_file_mapped());
    assert_eq!(mapped.bytes(), block_bytes.as_slice());
    assert_eq!(
        mapped.bytes(),
        layout.read_block_object(block_addr).unwrap().as_slice()
    );
}

#[test]
fn e_4_map_block_disk_single_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let (library, block_bytes, block_addr) = committed_single(&path);
    drop(library);

    // The whole `.umdb` is mmapped; the object is sliced at its catalog offset.
    let layout = SingleFileLayout::new(&path);
    let mapped = map_block_disk(&layout, block_addr).unwrap();
    assert!(mapped.is_file_mapped());
    assert_eq!(mapped.bytes(), block_bytes.as_slice());
    assert_eq!(
        mapped.bytes(),
        layout.read_block_object(block_addr).unwrap().as_slice()
    );
}

#[test]
fn e_4_map_missing_object() {
    let temp = tempfile::tempdir().unwrap();
    let missing = block_blob_address(b"no-such-object");

    // Flat layout.
    let root = temp.path().join("library");
    let (library, _, _, _, _) = committed_flat(&root);
    drop(library);
    let flat = FlatDirLayout::new(&root);
    assert_eq!(
        map_block_disk(&flat, missing).err().unwrap().code,
        ErrorCode::DanglingReference
    );
    assert_eq!(
        map_ref_disk(&flat, missing).err().unwrap().code,
        ErrorCode::StorageCorrupt
    );

    // Single-file layout.
    let path = temp.path().join("library.umdb");
    let (library, _, _) = committed_single(&path);
    drop(library);
    let single = SingleFileLayout::new(&path);
    assert_eq!(
        map_block_disk(&single, missing).err().unwrap().code,
        ErrorCode::DanglingReference
    );
}

#[test]
fn e_4_detect_disk_layout() {
    let temp = tempfile::tempdir().unwrap();

    // A directory → FlatDir.
    let dir = temp.path().join("dir-library");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(detect_disk_layout(&dir).unwrap(), StorageKind::FlatDir);

    // A `TSDB\0\0\0\0` header file → SingleFile.
    let sf = temp.path().join("container.umdb");
    TbLibrary::<SingleFileLayout>::create(&sf).unwrap();
    assert_eq!(detect_disk_layout(&sf).unwrap(), StorageKind::SingleFile);

    // A non-TSDB file → ambiguous.
    let junk = temp.path().join("junk.bin");
    std::fs::write(&junk, b"not-a-library").unwrap();
    assert_eq!(
        detect_disk_layout(&junk).unwrap_err().code,
        ErrorCode::MappingAmbiguous
    );
    // A missing path → ambiguous.
    assert_eq!(
        detect_disk_layout(&temp.path().join("missing-path"))
            .unwrap_err()
            .code,
        ErrorCode::MappingAmbiguous
    );
}

#[test]
fn e_4_mapped_view_disk_tree_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, tree_bytes, _, _, _) = committed_flat(&root);
    drop(library);

    // `Source::Disk` maps the committed tree blob: bytes equal the committed
    // canonical tree bytes, invalidation state Valid.
    let view = open_tree_mapped(Source::Disk(root)).unwrap();
    assert_eq!(view.bytes().unwrap(), tree_bytes.as_slice());
    assert_eq!(view.invalid_state(), InvalidState::Valid);
}

#[test]
fn e_4_mapped_view_disk_upstream_changed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();

    // Commit 1 and map it.
    let b1 = Blob::new(vec![1, 2]);
    let mut bucket1 = Bucket::new();
    bucket1.put(&b1);
    let image1 = TreeImage::new(vec![named_field("blob", ImageContent::Ref(b1.ref_id()))]);
    let tree_bytes1 = encode(&image1).unwrap();
    library
        .commit(&tree_bytes1, &image_leaf_refs(&image1).unwrap(), &bucket1)
        .unwrap();
    let mut view = MappedView::open(Source::Disk(root.clone())).unwrap();
    assert_eq!(view.bytes().unwrap(), tree_bytes1.as_slice());
    assert_eq!(view.invalid_state(), InvalidState::Valid);

    // Re-commit a different tree: the tree blob address moves → refresh()
    // reports UpstreamChanged (01 §7 case 1: mapping = upstream's current view).
    let b2 = Blob::new(vec![8, 9]);
    let mut bucket2 = Bucket::new();
    bucket2.put(&b2);
    let image2 = TreeImage::new(vec![named_field("blob", ImageContent::Ref(b2.ref_id()))]);
    let tree_bytes2 = encode(&image2).unwrap();
    assert_ne!(tree_bytes2, tree_bytes1);
    library
        .commit(&tree_bytes2, &image_leaf_refs(&image2).unwrap(), &bucket2)
        .unwrap();
    assert_eq!(view.refresh(), InvalidState::UpstreamChanged);
    // The mapped view now shows the upstream's current view.
    assert_eq!(view.bytes().unwrap(), tree_bytes2.as_slice());
}

#[test]
fn e_4_mapped_view_disk_upstream_dead() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, tree_bytes, _, _, _) = committed_flat(&root);
    drop(library);
    let mut view = open_tree_mapped(Source::Disk(root.clone())).unwrap();
    assert_eq!(view.bytes().unwrap(), tree_bytes.as_slice());

    // The upstream's committed pointer disappears → refresh() reports
    // UpstreamDead (01 §7 case 2) and bytes() is refused afterwards. Only the
    // pointer file is removed — the mapped tree blob stays untouched so the
    // assertion is portable even where mapped files are delete-locked.
    std::fs::remove_file(root.join("committed")).unwrap();
    assert_eq!(view.refresh(), InvalidState::UpstreamDead);
    assert_eq!(
        view.bytes().unwrap_err().code,
        ErrorCode::RequiredDataMissing
    );
}

#[test]
fn e_4_mapped_view_send_sync() {
    // Closing criterion (02 §8.5): `MappedView` keeps the E-2 `Send + Sync`
    // bound after the private-field swap to `MappedObject`.
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<MappedView>();
    assert_sync::<MappedView>();
}

#[test]
fn e_4_persist_block_address_and_dedup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();
    let a = Blob::new(vec![1, 1, 1]);
    let mut bucket = Bucket::new();
    bucket.put(&a);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(a.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let a_addr = block_blob_address(&a.envelope().encode());

    // persist_block returns the physical content-hash address (identity ⊥
    // address, 01 §1.11.4).
    let orphan = Blob::new(vec![7, 8, 9]);
    let orphan_env = orphan.envelope();
    let addr = persist_block(&FlatDirLayout::new(&root), &orphan_env).unwrap();
    assert_eq!(addr, block_blob_address(&orphan_env.encode()));

    // A second write of the same envelope is a dedup no-op (`exists → skip`).
    assert_eq!(
        persist_block(&FlatDirLayout::new(&root), &orphan_env).unwrap(),
        addr
    );

    // The orphan has no tree reference → GC reclaims it (block lifetime = tree
    // leaf reference, 01 §6 GC row); the committed block survives.
    let report = library.gc(0).unwrap();
    assert!(report.reclaimed >= 1, "the orphan block is swept");
    let layout = FlatDirLayout::new(&root);
    assert_eq!(
        layout.read_block_object(addr).unwrap_err().code,
        ErrorCode::DanglingReference
    );
    assert!(layout.read_block_object(a_addr).is_ok());
}
