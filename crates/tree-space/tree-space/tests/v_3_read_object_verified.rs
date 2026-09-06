//! V-3 冒烟：整块读默认验哈希（01 §4.2 / 02 §7 施工细则）。
//!
//! 断言面（02 §7.4）：篡改磁盘整块字节 → 默认 verified 路径（既有
//! `read_*_object` 签名不变、行为升级为验后返回）`Err(DigestMismatch)`，
//! 且库数据未被改写（不修复语义，01 §8）；`*_unchecked` 不验（返回篡改
//! 字节）；内部已验路径（restore_bucket / verify）在篡改后仍各自报
//! `Err(DigestMismatch)`（无双算行为回归，02 §7.5）。健康库 open /
//! verify / 默认 verified 读全绿。零拷贝三处（`map_*_disk` /
//! `MappedView::bytes` / IPC 共享区）**不**默认验——全部锚定在 V-5
//! `tests/v_5_validation.rs`（02 §9.1）。

use std::path::Path;
use tree_space::layout::tb::{RefRow, block_blob_address};
use tree_space::layout::{FlatDirLayout, SingleFileLayout};
use tree_space::tree::codec::{ImageContent, TreeImage, encode, named_field};
use tree_space::{
    Blob, Block, Bucket, Digest, ErrorCode, SingleFileTbLibrary, TbLayout, TbLibrary,
    image_leaf_refs, ref_table_address,
};

/// 提交一颗含一枚 Blob 叶的树，返回 (block_bytes, tree_bytes, block_addr,
/// tree_addr, ref_addr)。`TbLibrary::create`（flat 默认布局）。
fn commit_flat_fixture(root: &Path) -> (Vec<u8>, Vec<u8>, Digest, Digest, Digest) {
    let library = TbLibrary::create(root).unwrap();
    let blob = Blob::new(vec![1, 2, 3]);
    let block_bytes = blob.envelope().encode();
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("b", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    let receipt = library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let block_addr = block_blob_address(&block_bytes);
    (
        block_bytes,
        tree_bytes,
        block_addr,
        Digest::from_bytes(receipt.tree_blob),
        Digest::from_bytes(receipt.refs),
    )
}

/// 正向冒烟：flat + single-file 两个布局的健康对象走默认 verified 读均返回
/// 原字节（行为升级不改正常读，02 §7.5）。
#[test]
fn v_3_read_object_verified_default_healthy_ok() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");

    let (block_bytes, tree_bytes, block_addr, tree_addr, ref_addr) = commit_flat_fixture(&root);
    let layout = FlatDirLayout::new(&root);
    assert_eq!(
        layout.read_block_object(block_addr).unwrap(),
        block_bytes,
        "healthy block reads back byte-identical through the default verified read"
    );
    assert_eq!(
        layout.read_tree_object(tree_addr).unwrap(),
        tree_bytes,
        "healthy tree reads back byte-identical"
    );
    assert!(
        layout.read_ref_object(ref_addr).unwrap().len() > 0,
        "healthy ref object reads back"
    );

    // single-file 正向：默认 verified 读 == 原字节。
    let path = temp.path().join("single.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    let blob = Blob::new(vec![4, 5, 6]);
    let block_bytes = blob.envelope().encode();
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("s", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    let receipt = library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let single = SingleFileLayout::new(&path);
    assert_eq!(
        single
            .read_block_object(block_blob_address(&block_bytes))
            .unwrap(),
        block_bytes
    );
    assert_eq!(
        single
            .read_tree_object(Digest::from_bytes(receipt.tree_blob))
            .unwrap(),
        tree_bytes
    );
    assert!(
        single
            .read_ref_object(Digest::from_bytes(receipt.refs))
            .unwrap()
            .len()
            > 0
    );
}

/// 负向冒烟：篡改磁盘整块字节（block）→ 默认 verified 路径
/// `Err(DigestMismatch)`；`*_unchecked` 不验（返回篡改字节）；库数据未被
/// 改写；内部已验路径（open 的 restore_bucket / verify）仍各报
/// `Err(DigestMismatch)`（不修复，02 §7.2 免双算后既有校验仍绿）。
#[test]
fn v_3_read_block_tamper_verified_err_unchecked_skips() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    // 保留库句柄供 verify() 使用。
    let library = TbLibrary::create(&root).unwrap();
    let blob = Blob::new(vec![1, 2, 3]);
    let block_bytes = blob.envelope().encode();
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("b", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let block_addr = block_blob_address(&block_bytes);

    // 篡改块对象文件内容（文件名字址不变 → 命中内容↔地址比对）。
    let blob_path = root.join("tb-blocks").join(format!("{block_addr}.bin"));
    let tampered: Vec<u8> = b"tampered-block-bytes-v3".to_vec();
    std::fs::write(&blob_path, &tampered).unwrap();

    let layout = FlatDirLayout::new(&root);

    // 默认 verified 路径：Err(DigestMismatch)。
    let error = layout
        .read_block_object(block_addr)
        .expect_err("tampered block must be rejected by the default verified read");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    let after = std::fs::read(&blob_path).unwrap();
    assert_eq!(after, tampered, "failed read must not rewrite library data");

    // `*_unchecked` 不验：返回篡改字节（与默认验路径分道）。
    assert_eq!(
        layout.read_block_object_unchecked(block_addr).unwrap(),
        tampered,
        "unchecked read returns the raw tampered bytes"
    );

    // 内部已验路径仍各自校验一次：
    // - verify()：三列互验 + 地址重算（read 走 unchecked，verify 内比对命中）。
    let error = library
        .verify()
        .expect_err("tampered block must be rejected by verify");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    // - open 恢复（restore_bucket 的地址比对）。
    drop(library);
    let error = match TbLibrary::open(&root) {
        Ok(_) => panic!("tampered block must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    let after = std::fs::read(&blob_path).unwrap();
    assert_eq!(after, tampered, "failed open must not rewrite library data");
}

/// 负向冒烟：tree / ref 整块读同构——默认 verified `Err(DigestMismatch)`，
/// `*_unchecked` 返回篡改字节。
#[test]
fn v_3_read_tree_ref_tamper_verified_err_unchecked_skips() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let (_block_bytes, _tree_bytes, _block_addr, tree_addr, ref_addr) = commit_flat_fixture(&root);
    let layout = FlatDirLayout::new(&root);

    // tree 对象。
    let tree_path = root.join("tb-trees").join(format!("{tree_addr}.ipc"));
    let tree_tampered = b"ARROW1\x00\x01\x02v3-tree-tamper".to_vec();
    std::fs::write(&tree_path, &tree_tampered).unwrap();
    let error = layout
        .read_tree_object(tree_addr)
        .expect_err("tampered tree blob must be rejected by the default verified read");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert_eq!(
        layout.read_tree_object_unchecked(tree_addr).unwrap(),
        tree_tampered
    );

    // ref 对象：篡改为「合法编码但行内容不同」的变体（地址重算必与请求地址
    // 不符；`decode_ref_table` 仍可接受）。
    let ref_path = root.join("tb-refs").join(format!("{ref_addr}.ipc"));
    let ref_tampered = tampered_ref_bytes(&root, ref_addr);
    std::fs::write(&ref_path, &ref_tampered).unwrap();
    let error = layout
        .read_ref_object(ref_addr)
        .expect_err("tampered ref object must be rejected by the default verified read");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert_eq!(
        layout.read_ref_object_unchecked(ref_addr).unwrap(),
        ref_tampered
    );
}

/// 读磁盘 ref 对象 → 翻第一行 ref_id 一位 → 重编码（xpath 升序不变，
/// 仍可被 `decode_ref_table` 接受；内容↔地址必然不匹配）。
fn tampered_ref_bytes(root: &Path, ref_addr: Digest) -> Vec<u8> {
    let path = root.join("tb-refs").join(format!("{ref_addr}.ipc"));
    let original = std::fs::read(path).unwrap();
    assert_eq!(
        ref_table_address(&original),
        ref_addr,
        "read back the committed ref object at its own address"
    );
    let rows = tree_space::layout::tb::decode_ref_table(&original).unwrap();
    let mut variant = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let mut ref_id = row.ref_id;
        if index == 0 {
            ref_id[0] ^= 0xff;
        }
        variant.push(RefRow {
            xpath: row.xpath,
            ref_id,
            address: row.address,
        });
    }
    variant.sort();
    tree_space::encode_ref_table(&variant).unwrap()
}
