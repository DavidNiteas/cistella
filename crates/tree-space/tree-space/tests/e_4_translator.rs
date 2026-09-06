//! E-4: the layout-translator primitives surface (协议
//! `交换协议/02-施工路线图.md` §8.4) — 01 §6's seven sync primitives laid onto
//! the flat and single-file translators through `LayoutTranslator`.
//!
//! Asserts the §8.4 obligations for `tests/e_4_translator.rs`: read/write
//! roundtrips on both layouts (flat = per-file read, single-file = offset
//! positioned read), atomic read == plain read (a block is immutable, 01 §6),
//! content-addressed write dedup (`exists→skip`), the flat atomic write
//! (tmp+rename, no residue), the single-file atomic write (append epoch, old
//! objects preserved), GC on both layouts (flat sweep orphans / single-file
//! compaction repack) and reachable-block enumeration.

use tree_space::layout::{FlatDirLayout, SingleFileLayout};
use tree_space::tree::codec::{ImageContent, TreeImage, encode, named_field};
use tree_space::{
    Blob, Block, BootstrapImage, Bucket, Digest, ErrorCode, LayoutTranslator, SingleFileTbLibrary,
    StorageLayout, TbLayout, TbLibrary, block_blob_address, derive_ref_rows, encode_ref_table,
    image_leaf_refs, persist_block, ref_table_address, tree_blob_address,
};

/// Commits `leaves` (named block leaves with payloads) into a fresh flat
/// `TbLibrary` under `root`, returning the handle, the committed image and the
/// committed bucket.
fn commit_flat_leaves(
    root: &std::path::Path,
    leaves: &[(&str, Vec<u8>)],
) -> (TbLibrary, TreeImage, Bucket) {
    let library = TbLibrary::create(root).unwrap();
    let mut bucket = Bucket::new();
    let mut fields = Vec::new();
    for (name, payload) in leaves {
        let blob = Blob::new(payload.clone());
        let id = bucket.put(&blob);
        fields.push(named_field(name, ImageContent::Ref(id)));
    }
    let image = TreeImage::new(fields);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    (library, image, bucket)
}

#[test]
fn e_4_translator_read_write_roundtrip_flat() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let layout = FlatDirLayout::new(&root);
    let translator = LayoutTranslator::new(FlatDirLayout::new(&root));

    // Block channel: flat = per-file read.
    let block_bytes: Vec<u8> = Blob::new(vec![1, 2, 3]).envelope().encode();
    let block_addr = block_blob_address(&block_bytes);
    translator.write_block(block_addr, &block_bytes).unwrap();
    assert_eq!(translator.read_block(block_addr).unwrap(), block_bytes);
    assert_eq!(layout.read_block_object(block_addr).unwrap(), block_bytes);

    // Tree channel.
    let tree_bytes = encode(&TreeImage::new_empty()).unwrap();
    let tree_addr = tree_blob_address(&tree_bytes);
    translator.write_tree(tree_addr, &tree_bytes).unwrap();
    assert_eq!(translator.read_tree(tree_addr).unwrap(), tree_bytes);
    assert_eq!(layout.read_tree_object(tree_addr).unwrap(), tree_bytes);

    // Ref channel.
    let ref_bytes = encode_ref_table(&[]).unwrap();
    let ref_addr = ref_table_address(&ref_bytes);
    translator.write_ref(ref_addr, &ref_bytes).unwrap();
    assert_eq!(translator.read_ref(ref_addr).unwrap(), ref_bytes);
    assert_eq!(layout.read_ref_object(ref_addr).unwrap(), ref_bytes);
}

#[test]
fn e_4_translator_read_write_roundtrip_single_file() {
    // Single-file writes are staged and flushed as one epoch at
    // `write_committed` (每 commit 一 epoch); reads are offset-positioned via
    // the object-directory snapshot.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let layout = SingleFileLayout::new(&path);
    let bootstrap = BootstrapImage::built_in().unwrap();
    layout.create(&bootstrap).unwrap();
    let (_head, head_sequence) = layout.read_head().unwrap().unwrap();
    let commit_id = Digest::from_bytes([0x5a; 16]);

    let block_bytes: Vec<u8> = Blob::new(vec![9, 8, 7]).envelope().encode();
    let block_addr = block_blob_address(&block_bytes);
    layout.write_block_object(block_addr, &block_bytes).unwrap();
    let tree_bytes = encode(&TreeImage::new_empty()).unwrap();
    let tree_addr = tree_blob_address(&tree_bytes);
    layout.write_tree_object(tree_addr, &tree_bytes).unwrap();
    let ref_bytes = encode_ref_table(&[]).unwrap();
    let ref_addr = ref_table_address(&ref_bytes);
    layout.write_ref_object(ref_addr, &ref_bytes).unwrap();
    layout
        .write_commit_object(commit_id, b"sample-commit-bytes")
        .unwrap();
    layout
        .write_committed(commit_id, head_sequence + 1)
        .unwrap();

    let translator = LayoutTranslator::new(SingleFileLayout::new(&path));
    assert_eq!(translator.read_block(block_addr).unwrap(), block_bytes);
    assert_eq!(translator.read_tree(tree_addr).unwrap(), tree_bytes);
    assert_eq!(translator.read_ref(ref_addr).unwrap(), ref_bytes);
    assert_eq!(layout.read_block_object(block_addr).unwrap(), block_bytes);
}

#[test]
fn e_4_atomic_read_block_complete() {
    // Atomic read == plain read: a bucket block is immutable, one read is
    // already complete (01 §6 atomic-read row).
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let translator = LayoutTranslator::new(FlatDirLayout::new(&root));
    let block_bytes: Vec<u8> = Blob::new(vec![4, 5, 6]).envelope().encode();
    let addr = block_blob_address(&block_bytes);
    translator.write_block(addr, &block_bytes).unwrap();
    assert_eq!(
        translator.atomic_read_block(addr).unwrap(),
        translator.read_block(addr).unwrap()
    );
    assert_eq!(translator.atomic_read_block(addr).unwrap(), block_bytes);
}

#[test]
fn e_4_write_dedup_exists_skip_flat() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let layout = FlatDirLayout::new(&root);
    let translator = LayoutTranslator::new(FlatDirLayout::new(&root));
    let first: Vec<u8> = Blob::new(vec![1, 1]).envelope().encode();
    let addr = block_blob_address(&first);
    translator.write_block(addr, &first).unwrap();

    // A same-address second write is a no-op (`exists → skip`): the stored
    // bytes and the single object file are unchanged.
    let second: Vec<u8> = Blob::new(vec![2, 2]).envelope().encode();
    assert_ne!(second, first);
    translator.write_block(addr, &second).unwrap();
    assert_eq!(translator.read_block(addr).unwrap(), first);
    assert_eq!(layout.read_block_object(addr).unwrap(), first);
    let files = std::fs::read_dir(root.join("tb-blocks"))
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1, "dedup leaves exactly one object file");
}

#[test]
fn e_4_atomic_write_flat_tmp_rename() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let translator = LayoutTranslator::new(FlatDirLayout::new(&root));
    let block_bytes: Vec<u8> = Blob::new(vec![7, 7, 7]).envelope().encode();
    let addr = block_blob_address(&block_bytes);
    translator.atomic_write_block(addr, &block_bytes).unwrap();

    // Immediately visible: the rename replaced the temporary.
    let path = root.join("tb-blocks").join(format!("{addr}.bin"));
    assert_eq!(std::fs::read(&path).unwrap(), block_bytes);
    // No `.tmp` residue in the object directory.
    let leftovers = std::fs::read_dir(root.join("tb-blocks"))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.ends_with(".tmp").then_some(name)
        })
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "no temporary files remain: {leftovers:?}"
    );
}

#[test]
fn e_4_atomic_write_single_file_append_epoch() {
    // Single-file atomic write = 全量写 append epoch (非覆写): the old object
    // stays readable after a new epoch appends a second object.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let layout = SingleFileLayout::new(&path);
    layout.create(&BootstrapImage::built_in().unwrap()).unwrap();
    let (_head, head_sequence) = layout.read_head().unwrap().unwrap();

    // Epoch 1: object A.
    let a_bytes: Vec<u8> = Blob::new(vec![1, 1, 1]).envelope().encode();
    let a_addr = block_blob_address(&a_bytes);
    layout.write_block_object(a_addr, &a_bytes).unwrap();
    let commit_a = Digest::from_bytes([0xa1; 16]);
    layout.write_commit_object(commit_a, b"commit-a").unwrap();
    layout.write_committed(commit_a, head_sequence + 1).unwrap();

    // Epoch 2: object B (append, not overwrite).
    let b_bytes: Vec<u8> = Blob::new(vec![2, 2, 2]).envelope().encode();
    let b_addr = block_blob_address(&b_bytes);
    layout.write_block_object(b_addr, &b_bytes).unwrap();
    let commit_b = Digest::from_bytes([0xb2; 16]);
    layout.write_commit_object(commit_b, b"commit-b").unwrap();
    layout.write_committed(commit_b, head_sequence + 2).unwrap();

    // Both objects remain readable from the same layout and after a re-open.
    assert_eq!(layout.read_block_object(a_addr).unwrap(), a_bytes);
    assert_eq!(layout.read_block_object(b_addr).unwrap(), b_bytes);
    let reopened = SingleFileLayout::new(&path);
    assert_eq!(reopened.read_block_object(a_addr).unwrap(), a_bytes);
    assert_eq!(reopened.read_block_object(b_addr).unwrap(), b_bytes);
}

#[test]
fn e_4_gc_flat_sweeps_orphan() {
    // 提交树仅引用 A; B 是 persist_block 写入的孤儿 → gc 后 A 文件在、B 文件消失
    // (扁平 sweep 孤儿; 块生命周期 = 树叶引用, 01 §6 GC 行).
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, _image, _bucket) = commit_flat_leaves(&root, &[("a", vec![1, 2, 3])]);
    let a = Blob::new(vec![1, 2, 3]);
    let a_addr = block_blob_address(&a.envelope().encode());
    let b = Blob::new(vec![9, 9, 9]);
    let b_addr = persist_block(&FlatDirLayout::new(&root), &b.envelope()).unwrap();

    // Both objects exist before GC: A (referenced) and the orphan B.
    let layout = FlatDirLayout::new(&root);
    assert!(layout.read_block_object(a_addr).is_ok());
    assert!(layout.read_block_object(b_addr).is_ok());

    let report = library.gc(0).unwrap();
    assert!(report.reclaimed >= 1, "the orphan is reclaimed");
    assert!(layout.read_block_object(a_addr).is_ok());
    assert_eq!(
        layout.read_block_object(b_addr).unwrap_err().code,
        ErrorCode::DanglingReference
    );
}

#[test]
fn e_4_gc_single_file_repack() {
    // Single-file compaction repack: reachable (in-window) objects survive,
    // the orphan/out-of-window segment is dropped, and the repacked container
    // keeps accepting commits.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();

    let a = Blob::new(vec![1, 1]);
    let mut bucket_a = Bucket::new();
    bucket_a.put(&a);
    let image_a = TreeImage::new(vec![named_field("a", ImageContent::Ref(a.ref_id()))]);
    let bytes_a = encode(&image_a).unwrap();
    library
        .commit(&bytes_a, &image_leaf_refs(&image_a).unwrap(), &bucket_a)
        .unwrap();

    let b = Blob::new(vec![2, 2]);
    let mut bucket_b = Bucket::new();
    bucket_b.put(&b);
    let image_b = TreeImage::new(vec![named_field("b", ImageContent::Ref(b.ref_id()))]);
    let bytes_b = encode(&image_b).unwrap();
    library
        .commit(&bytes_b, &image_leaf_refs(&image_b).unwrap(), &bucket_b)
        .unwrap();

    let a_addr = block_blob_address(&a.envelope().encode());
    let b_addr = block_blob_address(&b.envelope().encode());

    // GC with the window at the head (sequence 2): commit-1's unique block
    // leaves the window and is compacted away; the in-window commit-2 block
    // survives.
    let report = library.gc(2).unwrap();
    assert!(report.reclaimed >= 1, "out-of-window objects are reclaimed");
    let layout = SingleFileLayout::new(&path);
    assert!(layout.read_block_object(b_addr).is_ok());
    assert_eq!(
        layout.read_block_object(a_addr).unwrap_err().code,
        ErrorCode::DanglingReference
    );

    // The repacked file remains a fully appendable single file.
    let mut bucket_ab = Bucket::new();
    bucket_ab.put(&a);
    bucket_ab.put(&b);
    let image_ab = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(a.ref_id())),
        named_field("b", ImageContent::Ref(b.ref_id())),
    ]);
    let bytes_ab = encode(&image_ab).unwrap();
    library
        .commit(&bytes_ab, &image_leaf_refs(&image_ab).unwrap(), &bucket_ab)
        .unwrap();
    let reopened = TbLibrary::<SingleFileLayout>::open(&path).unwrap();
    assert!(reopened.verify().unwrap() >= 1);
}

#[test]
fn e_4_enumerate_reachable_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (library, image, bucket) = commit_flat_leaves(&root, &[("a", vec![1]), ("b", vec![2])]);
    drop(library);

    let translator = LayoutTranslator::new(FlatDirLayout::new(&root));
    // enumerate == the committed head's reference-table row set.
    let expected_rows = derive_ref_rows(&image_leaf_refs(&image).unwrap(), &bucket).unwrap();
    assert_eq!(
        translator.enumerate_reachable_blocks().unwrap(),
        expected_rows
    );
    // block_objects == every stored block file, address = content hash.
    let mut expected_objects = bucket
        .ids()
        .map(|id| {
            let envelope = bucket.get(id).expect("bucket ids iterate stored envelopes");
            (block_blob_address(&envelope.encode()), envelope.encode())
        })
        .collect::<Vec<_>>();
    expected_objects.sort();
    let mut objects = translator.block_objects().unwrap();
    objects.sort();
    assert_eq!(objects, expected_objects);
}
