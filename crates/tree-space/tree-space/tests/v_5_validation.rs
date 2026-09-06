//! V-5 全量锚定：数据验证系统 V-1..V-4 的能力收口（02 §9.1 测试义务全量清单）。
//!
//! 结构：L1（地址重算 + 整块读默认验 + 零拷贝显式验 + assert_integrity）→ L2
//! （verify 三列互验不动 + 两地址重算并进）→ L3（Table 深 schema / 注册块
//! payload / typed 结构 + 不物化叶子计数器）→ 总入口（深度蕴含 + 全程不修复）。
//!
//! 关键夹具（02 §5.3 两分法 + §9.1）：
//! - **指针篡改夹具**（测地址断言）：改写 head commit 的 `tb_tree_blob` /
//!   `tb_refs` 指针列 →「原字节副本的新地址」（内容未改、tree_id / 三列互验仍
//!   通过，唯 `tree_blob_address` / `ref_table_address` 与指针不符）→ 命中
//!   `assert_tree_address` / `assert_ref_table_address` 的 `Err(DigestMismatch)`；
//! - **字节篡改夹具**（测 tree_id / 三列互验）：改对象文件内容字节、文件名地址
//!   不变——tree 由 tree_id 校验命中、ref 由 verify 三列互验命中；
//! - **「不物化叶子」计数器级证明**：typed `assert_schema<T>` 走
//!   `check_image_children` 结构面，叶子 `DecodeBlock::decode_block` 调用计数
//!   == 0（本文件用带计数器的测试局部 `Blob` 叶型；对照 `project()` 证明计数器
//!   有效）。
//!
//! 全部测试零写路径：每个 Err 场景后数据原样（01 §8「不修复」铁律）。

use arrow::array::{
    ArrayRef, FixedSizeBinaryArray, Int32Array, Int64Array, StringArray, StructArray,
};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use arrow::record_batch::RecordBatch;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use tree_space::ipc::{decode_batch, encode_batch};
use tree_space::layout::FlatDirLayout;
use tree_space::layout::tb::{RefRow, block_blob_address};
use tree_space::shared::SharedRegion;
use tree_space::tree::codec::{ImageContent, TreeImage, encode, encode_node, named_field, tree_id};
use tree_space::{
    ArrowCaps, ArrowTable, Blob, Block, BlockCaps, Bucket, Digest, ErrorCode, MappedView, RefId,
    RegisteredBlock, Sequence, Source, TbLayout, TbLibrary, TreeCodec, TreeNode, ValidateDepth,
    ValidateRequest, ValidateSubject, Value, image_leaf_refs, map_block_disk, map_ref_disk,
    map_tree_disk, ref_table_address, register_block, tree_blob_address, validate,
};

// ---------------------------------------------------------------------------
// 基础夹具：tb 树 + `Blob`/`Sequence` 叶（02 §9.1「既有 tb 树 + Blob 叶」）
// ---------------------------------------------------------------------------

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct FlatTree {
    b: Blob,
    s: Sequence,
}

/// 一次 TB commit 的关键落盘信息（flat 布局）。
struct FlatFixture {
    library: TbLibrary,
    bucket: Bucket,
    tree_bytes: Vec<u8>,
    block_bytes: Vec<u8>,
    block_addr: Digest,
    tree_addr: Digest,
    ref_addr: Digest,
    root_tree_id: [u8; 16],
    root: PathBuf,
}

/// 提交一颗含两枚叶的树（Blob + Sequence），返回落盘地址 + 创建句柄。
fn flat_fixture(temp: &Path) -> FlatFixture {
    let root = temp.join("library");
    let library = TbLibrary::create(&root).unwrap();
    let blob = Blob::new(vec![1, 2, 3]);
    let seq = Sequence::new(vec![Value::I32(5)]);
    let tree = FlatTree {
        b: blob.clone(),
        s: seq.clone(),
    };
    let block_bytes = blob.envelope().encode();
    let block_addr = block_blob_address(&block_bytes);
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    bucket.put(&seq);
    let tree_bytes = encode_node(&tree).unwrap();
    let leaf_refs = tree.leaf_refs();
    let receipt = library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    FlatFixture {
        library,
        bucket,
        tree_bytes,
        block_bytes,
        block_addr,
        tree_addr: Digest::from_bytes(receipt.tree_blob),
        ref_addr: Digest::from_bytes(receipt.refs),
        root_tree_id: receipt.tb_root_tree_id,
        root,
    }
}

// ---------------------------------------------------------------------------
// 夹具与断言助手
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fsb16(column: &ArrayRef, row: usize) -> [u8; 16] {
    column
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(row)
        .try_into()
        .unwrap()
}

fn fsb16_array(value: [u8; 16]) -> ArrayRef {
    Arc::new(FixedSizeBinaryArray::try_from_iter(std::iter::once(value.to_vec())).unwrap())
}

/// 读 head commit 批次（8 列）+ head_id。
fn head_commit_batch(root: &Path) -> (Vec<u8>, RecordBatch) {
    let committed = decode_batch(&std::fs::read(root.join("committed")).unwrap()).unwrap();
    let head_id = committed
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    let path = root.join("commits").join(format!("{}.ipc", hex(&head_id)));
    let batch = decode_batch(&std::fs::read(&path).unwrap()).unwrap();
    (head_id, batch)
}

/// 重写 head commit 指针列（5 = `tb_tree_blob`，6 = `tb_refs`）。
fn rewrite_commit_pointer(root: &Path, column_index: usize, value: [u8; 16]) {
    let (head_id, batch) = head_commit_batch(root);
    let mut columns = batch.columns().to_vec();
    columns[column_index] = fsb16_array(value);
    let rewritten = RecordBatch::try_new(batch.schema().clone(), columns).unwrap();
    let path = root.join("commits").join(format!("{}.ipc", hex(&head_id)));
    std::fs::write(&path, encode_batch(&rewritten).unwrap()).unwrap();
}

/// **指针篡改夹具**（02 §5.3 / §9.1 两分法，测地址断言专用）：把 head commit
/// 的指针列指向「原字节副本的新地址」——先把原对象字节投放到新地址文件，再改写
/// 指针列。返回 (真实地址, 伪造地址)。
fn tamper_pointer(root: &Path, column_index: usize, object_dir: &str) -> ([u8; 16], [u8; 16]) {
    let (_head_id, batch) = head_commit_batch(root);
    let real = fsb16(&batch.column(column_index), 0);
    let original =
        std::fs::read(root.join(object_dir).join(format!("{}.ipc", hex(&real)))).unwrap();
    let mut fake = real;
    fake[0] ^= 0xff;
    std::fs::write(
        root.join(object_dir).join(format!("{}.ipc", hex(&fake))),
        &original,
    )
    .unwrap();
    rewrite_commit_pointer(root, column_index, fake);
    (real, fake)
}

fn tamper_tree_pointer(root: &Path) -> ([u8; 16], [u8; 16]) {
    tamper_pointer(root, 5, "tb-trees")
}

fn tamper_ref_pointer(root: &Path) -> ([u8; 16], [u8; 16]) {
    tamper_pointer(root, 6, "tb-refs")
}

/// **字节篡改夹具**（测 tree_id / 三列互验）：读磁盘 ref 对象 → 翻第一行
/// `ref_id` 一位 → 重编码（xpath 升序不变，`decode_ref_table` 仍可接受；
/// 内容↔地址必然不匹配）。
fn tampered_ref_rows_bytes(root: &Path, ref_addr: Digest) -> Vec<u8> {
    let path = root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&ref_addr.as_bytes())));
    let original = std::fs::read(&path).unwrap();
    let rows = tree_space::decode_ref_table(&original).unwrap();
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

/// 目录内容快照（相对路径 → 字节），用于「数据原样、无写路径」断言。
fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// 同长字节翻转（零拷贝测试用：不改变文件长度，mmap 范围稳定）。
fn flip_byte(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let at = out.len() / 2;
    out[at] ^= 0x01;
    out
}

fn request(
    depth: ValidateDepth,
    subject: ValidateSubject,
    schema: Option<Arc<Schema>>,
) -> ValidateRequest {
    ValidateRequest {
        depth,
        subject,
        schema,
        path: None,
    }
}

// ---------------------------------------------------------------------------
// L1：V-1（两个地址公式读侧重算）+ V-3（整块读默认验 + 零拷贝显式验）锚定
// ---------------------------------------------------------------------------

/// 正向：commit 的 `tb_tree_blob` 指针列 = 树字节真实地址 → open / verify 通过。
#[test]
fn v_5_tree_addr_ptr_tamper_ok() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());

    let (_head, batch) = head_commit_batch(&fixture.root);
    assert_eq!(
        fsb16(&batch.column(5), 0),
        tree_blob_address(&fixture.tree_bytes).as_bytes(),
        "committed tb_tree_blob pointer is the tree-blob address"
    );

    let reopened = TbLibrary::open(&fixture.root).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        fixture.bucket.len(),
        "healthy library: open + verify stay green under the untampered tree pointer"
    );
}

/// 负向（open 与 verify 两路）：树指针篡改 → 地址断言 `Err(DigestMismatch)`，
/// tree_id / 三列互验仍通过；库数据未被改写（不修复，01 §8）。
#[test]
fn v_5_tree_addr_ptr_tamper_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());

    let (real, fake) = tamper_tree_pointer(&fixture.root);
    assert_ne!(
        real, fake,
        "tampered pointer must differ from the real address"
    );

    let error = fixture
        .library
        .verify()
        .expect_err("tampered tree pointer must be rejected by verify");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert!(
        error.to_string().contains("tree blob recomputed address"),
        "verify 的失败必须由地址断言命中（tree_id / 三列互验仍通过）：{error}"
    );

    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("tampered tree pointer must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert!(error.to_string().contains("tree blob recomputed address"));

    let (_, batch) = head_commit_batch(&fixture.root);
    assert_eq!(
        fsb16(&batch.column(5), 0),
        fake,
        "commit pointer column stays tampered after failures"
    );
    assert_eq!(
        std::fs::read(
            fixture
                .root
                .join("tb-trees")
                .join(format!("{}.ipc", hex(&real)))
        )
        .unwrap(),
        fixture.tree_bytes,
        "original tree blob file untouched"
    );
    assert_eq!(
        std::fs::read(
            fixture
                .root
                .join("tb-trees")
                .join(format!("{}.ipc", hex(&fake)))
        )
        .unwrap(),
        fixture.tree_bytes,
        "shadow copy untouched"
    );
}

/// 正向：ref 指针列 = 引用表真实地址 → open / verify 通过。
#[test]
fn v_5_ref_addr_ptr_tamper_ok() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());

    let ref_bytes = std::fs::read(
        fixture
            .root
            .join("tb-refs")
            .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes()))),
    )
    .unwrap();
    let (_head, batch) = head_commit_batch(&fixture.root);
    assert_eq!(
        fsb16(&batch.column(6), 0),
        ref_table_address(&ref_bytes).as_bytes(),
        "committed tb_refs pointer is the reference-table address"
    );

    let reopened = TbLibrary::open(&fixture.root).unwrap();
    assert_eq!(
        reopened.verify().unwrap(),
        fixture.bucket.len(),
        "healthy library: open + verify stay green under the untampered ref pointer"
    );
}

/// 负向（open 与 verify 两路）：ref 指针篡改 → `assert_ref_table_address`
/// `Err(DigestMismatch)`；库数据未被改写。
#[test]
fn v_5_ref_addr_ptr_tamper_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());

    let (real, fake) = tamper_ref_pointer(&fixture.root);
    assert_ne!(
        real, fake,
        "tampered pointer must differ from the real address"
    );

    let error = fixture
        .library
        .verify()
        .expect_err("tampered ref pointer must be rejected by verify");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert!(
        error
            .to_string()
            .contains("reference table recomputed address"),
        "verify 的失败必须由 ref 地址断言命中（三列互验 / tree_id 仍通过）：{error}"
    );

    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("tampered ref pointer must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert!(
        error
            .to_string()
            .contains("reference table recomputed address")
    );

    let (_, batch) = head_commit_batch(&fixture.root);
    assert_eq!(
        fsb16(&batch.column(6), 0),
        fake,
        "commit pointer column stays tampered after failures"
    );
    assert_eq!(
        std::fs::read(
            fixture
                .root
                .join("tb-refs")
                .join(format!("{}.ipc", hex(&real)))
        )
        .unwrap(),
        std::fs::read(
            fixture
                .root
                .join("tb-refs")
                .join(format!("{}.ipc", hex(&fake)))
        )
        .unwrap(),
        "original ref object and shadow copy both untouched"
    );
}

/// V-3 锚定：磁盘整块读默认验哈希（01 §4.2）——健康对象按原字节返回；篡改
/// 整块 → 默认 verified 路径 `Err(DigestMismatch)`，库数据未被改写。
#[test]
fn v_5_read_object_verified_default() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let layout = FlatDirLayout::new(&fixture.root);

    assert_eq!(
        layout.read_block_object(fixture.block_addr).unwrap(),
        fixture.block_bytes,
        "healthy block reads back byte-identical through the default verified read"
    );
    assert_eq!(
        layout.read_tree_object(fixture.tree_addr).unwrap(),
        fixture.tree_bytes,
        "healthy tree reads back byte-identical"
    );
    assert!(
        layout.read_ref_object(fixture.ref_addr).unwrap().len() > 0,
        "healthy ref object reads back"
    );

    // 篡改整块 → 默认验 Err（不修复）。
    let blob_path = fixture
        .root
        .join("tb-blocks")
        .join(format!("{}.bin", hex(&fixture.block_addr.as_bytes())));
    let tampered = b"tampered-block-v5".to_vec();
    std::fs::write(&blob_path, &tampered).unwrap();
    let error = layout
        .read_block_object(fixture.block_addr)
        .expect_err("tampered block must be rejected by the default verified read");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    let after = std::fs::read(&blob_path).unwrap();
    assert_eq!(after, tampered, "failed read must not rewrite library data");
}

/// V-3 锚定：`*_unchecked` 整块读不验（返回篡改字节）；默认 verified 变体 Err。
#[test]
fn v_5_read_object_unchecked_skips() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let layout = FlatDirLayout::new(&fixture.root);

    // block 通道。
    let blob_path = fixture
        .root
        .join("tb-blocks")
        .join(format!("{}.bin", hex(&fixture.block_addr.as_bytes())));
    let tampered_block = b"tampered-block-unchecked".to_vec();
    std::fs::write(&blob_path, &tampered_block).unwrap();
    assert_eq!(
        layout
            .read_block_object_unchecked(fixture.block_addr)
            .unwrap(),
        tampered_block,
        "unchecked read returns the raw tampered bytes"
    );
    assert_eq!(
        layout
            .read_block_object(fixture.block_addr)
            .unwrap_err()
            .code,
        ErrorCode::DigestMismatch
    );

    // tree 通道。
    let tree_path = fixture
        .root
        .join("tb-trees")
        .join(format!("{}.ipc", hex(&fixture.tree_addr.as_bytes())));
    let tampered_tree = b"ARROW1\x00\x01\x02v5-tree-unchecked".to_vec();
    std::fs::write(&tree_path, &tampered_tree).unwrap();
    assert_eq!(
        layout
            .read_tree_object_unchecked(fixture.tree_addr)
            .unwrap(),
        tampered_tree
    );
    assert_eq!(
        layout.read_tree_object(fixture.tree_addr).unwrap_err().code,
        ErrorCode::DigestMismatch
    );

    // ref 通道（合法编码但行内容不同的变体）。
    let ref_path = fixture
        .root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes())));
    let tampered_ref = tampered_ref_rows_bytes(&fixture.root, fixture.ref_addr);
    std::fs::write(&ref_path, &tampered_ref).unwrap();
    assert_eq!(
        layout.read_ref_object_unchecked(fixture.ref_addr).unwrap(),
        tampered_ref
    );
    assert_eq!(
        layout.read_ref_object(fixture.ref_addr).unwrap_err().code,
        ErrorCode::DigestMismatch
    );
}

/// V-3 锚定（01 §4.3）：三处零拷贝显式验入口清单——`map_*_disk` /
/// `MappedView::bytes` / IPC 共享区 `bytes()` 对篡改字节**仍原样返回、不默认
/// 验**；显式走断言 / 重算才命中 `Err`。
#[test]
fn v_5_zero_copy_no_default_hash() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let layout = FlatDirLayout::new(&fixture.root);

    // 健康：三个磁盘映射通道零拷贝返回原字节。
    let mapped_tree = map_tree_disk(&layout).unwrap();
    assert_eq!(mapped_tree.bytes(), fixture.tree_bytes.as_slice());
    let mapped_block = map_block_disk(&layout, fixture.block_addr).unwrap();
    assert_eq!(mapped_block.bytes(), fixture.block_bytes.as_slice());
    let ref_file = std::fs::read(
        fixture
            .root
            .join("tb-refs")
            .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes()))),
    )
    .unwrap();
    let mapped_ref = map_ref_disk(&layout, fixture.ref_addr).unwrap();
    assert_eq!(mapped_ref.bytes(), ref_file.as_slice());
    // Windows 上 mmap 未释放时不能覆写同一文件（ERROR_USER_MAPPED_FILE）——
    // 先释放映射再篡改，之后重新映射验证「零拷贝不默认验」。
    drop(mapped_tree);
    drop(mapped_block);
    drop(mapped_ref);

    // 篡改三个对象文件（同长字节翻转）→ 重新映射仍原样返回字节，不 Err。
    let tree_path = fixture
        .root
        .join("tb-trees")
        .join(format!("{}.ipc", hex(&fixture.tree_addr.as_bytes())));
    let tampered_tree = flip_byte(&fixture.tree_bytes);
    std::fs::write(&tree_path, &tampered_tree).unwrap();
    let block_path = fixture
        .root
        .join("tb-blocks")
        .join(format!("{}.bin", hex(&fixture.block_addr.as_bytes())));
    let tampered_block = flip_byte(&fixture.block_bytes);
    std::fs::write(&block_path, &tampered_block).unwrap();
    let ref_path = fixture
        .root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes())));
    let tampered_ref = flip_byte(&ref_file);
    std::fs::write(&ref_path, &tampered_ref).unwrap();

    assert_eq!(
        map_tree_disk(&layout).unwrap().bytes(),
        tampered_tree.as_slice(),
        "map_tree_disk must not default-hash (zero-copy)"
    );
    assert_eq!(
        map_block_disk(&layout, fixture.block_addr).unwrap().bytes(),
        tampered_block.as_slice(),
        "map_block_disk must not default-hash (zero-copy)"
    );
    assert_eq!(
        map_ref_disk(&layout, fixture.ref_addr).unwrap().bytes(),
        tampered_ref.as_slice(),
        "map_ref_disk must not default-hash (zero-copy)"
    );

    // `MappedView::bytes`（Source::Disk 映射已提交树 blob）：同样零验证返回。
    let view = MappedView::open(Source::Disk(fixture.root.clone())).unwrap();
    assert_eq!(view.bytes().unwrap(), tampered_tree.as_slice());

    // IPC 共享区 `bytes()`：publish 原样 / 篡改字节都零验证返回。
    let region = SharedRegion::publish(fixture.tree_bytes.clone()).unwrap();
    assert_eq!(region.bytes(), fixture.tree_bytes.as_slice());
    let tampered_region = SharedRegion::publish(tampered_tree.clone()).unwrap();
    assert_eq!(tampered_region.bytes(), tampered_tree.as_slice());

    // 显式走断言 / 重算才 Err（三处显式验入口的下游）。
    assert_ne!(
        tree_id(&tampered_tree).as_bytes(),
        fixture.root_tree_id,
        "explicit tree_id recompute catches the tamper"
    );
    assert_ne!(
        tree_blob_address(&tampered_tree).as_bytes(),
        fixture.tree_addr.as_bytes(),
        "explicit tree_blob_address recompute catches the tamper"
    );
    let error = fixture
        .library
        .assert_integrity()
        .expect_err("zero-copy 不默认验；显式断言才 Err");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
}

/// L1 断言族锚定：`assert_integrity` 对 Healthy 库返回已验证块数 == 桶块数
/// （== tb-blocks 落盘对象数）；篡改后 `Err(DigestMismatch)`，数据不被改写。
#[test]
fn v_5_assert_integrity_full() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());

    let verified = fixture.library.assert_integrity().unwrap();
    assert_eq!(
        verified,
        fixture.bucket.len(),
        "Healthy: verified block count == bucket block count"
    );
    let on_disk = std::fs::read_dir(fixture.root.join("tb-blocks"))
        .unwrap()
        .count();
    assert_eq!(verified, on_disk, "tb-blocks objects == bucket count");

    let blob_path = fixture
        .root
        .join("tb-blocks")
        .join(format!("{}.bin", hex(&fixture.block_addr.as_bytes())));
    let tampered = b"tampered-integrity".to_vec();
    std::fs::write(&blob_path, &tampered).unwrap();
    let error = fixture
        .library
        .assert_integrity()
        .expect_err("tampered block must fail assert_integrity");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    let after = std::fs::read(&blob_path).unwrap();
    assert_eq!(
        after, tampered,
        "failed assert must not rewrite library data"
    );
}

// ---------------------------------------------------------------------------
// L2：V-1 锚定——verify 三列互验不动 + 两地址重算并进
// ---------------------------------------------------------------------------

/// 既有三列互验路径语义仍绿 + 两个地址重算已并进 verify()（02 §5.2 插桩点 2）：
/// - 正向：Healthy 库 verify() 返回叶数 == 桶块数；
/// - 负向 A：ref 行字节篡改（同一地址文件）→ 三列互验命中 `Err(DigestMismatch)`；
/// - 负向 B：ref 指针篡改（内容未改、三列互验仍过）→ verify 内 `assert_ref_table_address`
///   命中 `Err(DigestMismatch)`——证明地址重算是 verify 的组成部分，而非替代。
#[test]
fn v_5_verify_three_column_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    assert_eq!(
        fixture.library.verify().unwrap(),
        fixture.bucket.len(),
        "healthy three-column interop stays green"
    );

    // 负向 A：ref 行字节篡改 → 三列互验命中（L2 语义未破坏）。
    let ref_path = fixture
        .root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes())));
    let tampered = tampered_ref_rows_bytes(&fixture.root, fixture.ref_addr);
    std::fs::write(&ref_path, &tampered).unwrap();
    let error = fixture
        .library
        .verify()
        .expect_err("tampered ref rows must still be rejected by the three-column cross-check");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    let after = std::fs::read(&ref_path).unwrap();
    assert_eq!(
        after, tampered,
        "failed verify must not rewrite library data"
    );

    // 负向 B：ref 指针篡改（原字节副本新地址）→ 地址重算命中 verify。
    let temp2 = tempfile::tempdir().unwrap();
    let fixture2 = flat_fixture(temp2.path());
    let (_real, fake) = tamper_ref_pointer(&fixture2.root);
    let error = fixture2
        .library
        .verify()
        .expect_err("tampered ref pointer must be rejected by verify's address recompute");
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert!(
        error
            .to_string()
            .contains("reference table recomputed address")
    );
    let (_, batch) = head_commit_batch(&fixture2.root);
    assert_eq!(fsb16(&batch.column(6), 0), fake);
}

// ---------------------------------------------------------------------------
// L3：V-4 锚定——Table 深 schema / 注册块 payload / typed 结构 + 不物化叶子
// ---------------------------------------------------------------------------

const METRICS_NAME: &str = "v5.metrics";
const MISMATCHED_NAME: &str = "v5.mismatched";

fn metrics_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        "count",
        DataType::Int64,
        false,
    )]))
}

/// 正向注册块：payload 实解 schema 与 `ArrowCaps.schema`（Int64）一致 → Ok。
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetricsBlock(Vec<i64>);

impl RegisteredBlock for MetricsBlock {
    const NAME: &'static str = METRICS_NAME;
    fn encode(&self) -> Vec<u8> {
        tree_space::ipc::encode_batch(
            &RecordBatch::try_new(
                metrics_schema(),
                vec![Arc::new(Int64Array::from(self.0.clone()))],
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        let batch = tree_space::ipc::decode_batch(bytes)?;
        let array = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("metrics payload carries an Int64 column");
        Ok(Self(array.values().to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Arrow(ArrowCaps {
            schema: metrics_schema(),
            to_batch: |bytes| tree_space::ipc::decode_batch(bytes),
        })
    }
}

/// 负向注册块：payload 实解 schema（Int32）与 `ArrowCaps.schema`（Int64）不符
/// ——内容自洽、可正常提交（L1/L2 均无异议），唯 L3 断言捕获（payload 篡改
/// 语义的静态实现：磁盘篡改块字节会被 open 的地址校验拦截，故以「自洽但违反
/// 声明」的 payload 承载，02 §8.3-1）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct MismatchedBlock(Vec<i32>);

impl RegisteredBlock for MismatchedBlock {
    const NAME: &'static str = MISMATCHED_NAME;
    fn encode(&self) -> Vec<u8> {
        tree_space::ipc::encode_batch(
            &RecordBatch::try_new(
                Arc::new(Schema::new(vec![Field::new(
                    "count",
                    DataType::Int32,
                    false,
                )])),
                vec![Arc::new(Int32Array::from(self.0.clone()))],
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        let batch = tree_space::ipc::decode_batch(bytes)?;
        let array = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("mismatched payload carries an Int32 column");
        Ok(Self(array.values().to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Arrow(ArrowCaps {
            schema: metrics_schema(), // 故意声明 Int64，与 payload（Int32）不符
            to_batch: |bytes| tree_space::ipc::decode_batch(bytes),
        })
    }
}

fn ensure_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_block::<MetricsBlock>().unwrap();
        register_block::<MismatchedBlock>().unwrap();
    });
}

/// 平表层。
fn flat_table(ids: &[i32]) -> ArrowTable {
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

/// 嵌套 Struct 层。
fn nested_table() -> ArrowTable {
    let meta = StructArray::from(vec![
        (
            Arc::new(Field::new("kind", DataType::Utf8, false)),
            Arc::new(StringArray::from(vec!["a", "b"])) as ArrayRef,
        ),
        (
            Arc::new(Field::new("v", DataType::Int64, true)),
            Arc::new(Int64Array::from(vec![Some(1), None])),
        ),
    ]);
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new(
            "meta",
            DataType::Struct(Fields::from(vec![
                Field::new("kind", DataType::Utf8, false),
                Field::new("v", DataType::Int64, true),
            ])),
            false,
        ),
    ]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![7, 8])), Arc::new(meta)],
        )
        .unwrap(),
    )
    .unwrap()
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct TableTree {
    name: Value,
    table: ArrowTable,
    seq: Sequence,
    nested: ArrowTable,
}

/// L3 typed 夹具：tree1（弱图）把两个注册块写进对象通道；tree2（typed
/// `TableTree`）为当前头。插件化改造（01 §4-5）后 open 走 ref 表驱动恢复——
/// 当前头（typed）引用的注册块是「未被头树引用的孤儿对象」，**不再进恢复桶**；
/// 注册块断言走 [`registered_fixture`]（弱图为头）。
fn l3_fixture(temp: &Path) -> (Bucket, RefId, RefId, RefId) {
    ensure_registered();
    let root = temp.join("library");
    let library = TbLibrary::create(&root).unwrap();

    let metrics = MetricsBlock(vec![1, 2, 3]);
    let mismatched = MismatchedBlock(vec![9, 8]);
    let mut weak_bucket = Bucket::new();
    let metrics_id = weak_bucket.put(&metrics);
    let mismatched_id = weak_bucket.put(&mismatched);
    let weak_image = TreeImage::new(vec![
        named_field("metrics", ImageContent::Ref(metrics_id)),
        named_field("mismatched", ImageContent::Ref(mismatched_id)),
    ]);
    let weak_bytes = encode(&weak_image).unwrap();
    let weak_refs = image_leaf_refs(&weak_image).unwrap();
    library
        .commit(&weak_bytes, &weak_refs, &weak_bucket)
        .unwrap();

    let table_block = flat_table(&[1, 2]);
    let nested_block = nested_table();
    let seq_block = Sequence::new(vec![Value::I32(5), Value::I64(6)]);
    let mut bucket = Bucket::new();
    let table_id = bucket.put(&table_block);
    bucket.put(&nested_block);
    bucket.put(&seq_block);
    let tree = TableTree {
        name: Value::Utf8("exp".into()),
        table: table_block,
        seq: seq_block,
        nested: nested_block,
    };
    let tree_bytes = encode_node(&tree).unwrap();
    let leaf_refs = tree.leaf_refs();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    (bucket, table_id, metrics_id, mismatched_id)
}

fn open_library(temp: &Path) -> TbLibrary {
    TbLibrary::open(temp.join("library")).unwrap()
}

/// 注册块 L3 夹具：以「弱图（两个注册块）」为**当前头**的库——ref 表驱动
/// 恢复只装当前头可达对象（插件化 01 §4-5），注册块断言落在此恢复态。
fn registered_fixture(temp: &Path) -> (RefId, RefId) {
    ensure_registered();
    let root = temp.join("library");
    let library = TbLibrary::create(&root).unwrap();

    let metrics = MetricsBlock(vec![1, 2, 3]);
    let mismatched = MismatchedBlock(vec![9, 8]);
    let mut weak_bucket = Bucket::new();
    let metrics_id = weak_bucket.put(&metrics);
    let mismatched_id = weak_bucket.put(&mismatched);
    let weak_image = TreeImage::new(vec![
        named_field("metrics", ImageContent::Ref(metrics_id)),
        named_field("mismatched", ImageContent::Ref(mismatched_id)),
    ]);
    let weak_bytes = encode(&weak_image).unwrap();
    let weak_refs = image_leaf_refs(&weak_image).unwrap();
    library
        .commit(&weak_bytes, &weak_refs, &weak_bucket)
        .unwrap();
    (metrics_id, mismatched_id)
}

fn flat_expected() -> Schema {
    Schema::new(vec![Field::new("value", DataType::Int32, false)])
}

/// 恢复桶中带嵌套 Struct 的 Table 块 id（fields().len() == 2）。
fn nested_id_from(bucket: &Bucket) -> RefId {
    bucket
        .ids()
        .find(|id| {
            matches!(
                bucket.get(*id).map(|envelope| &envelope.kind),
                Some(tree_space::BlockKind::Table)
            ) && bucket.get(*id).is_some_and(|envelope| {
                ArrowTable::try_read_blob(&envelope.payload)
                    .unwrap()
                    .as_batch()
                    .schema()
                    .fields()
                    .len()
                    == 2
            })
        })
        .expect("nested table block is in the restored bucket")
}

/// L3-1 负向：深 schema 字段名不符 → `Err(SchemaMismatch)`（正向全等 Ok）。
#[test]
fn v_5_assert_table_schema_field_name() {
    let temp = tempfile::tempdir().unwrap();
    let (bucket, table_id, _, _) = l3_fixture(temp.path());
    let library = open_library(temp.path());
    assert!(bucket.get(table_id).is_some());

    library
        .assert_table_schema(table_id, &flat_expected())
        .unwrap();

    let renamed = Schema::new(vec![Field::new("value_renamed", DataType::Int32, false)]);
    let error = library
        .assert_table_schema(table_id, &renamed)
        .expect_err("renamed field must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-1 负向：深 schema 数据类型不符 → `Err(SchemaMismatch)`（正向全等 Ok）。
#[test]
fn v_5_assert_table_schema_type() {
    let temp = tempfile::tempdir().unwrap();
    let (_, table_id, _, _) = l3_fixture(temp.path());
    let library = open_library(temp.path());

    library
        .assert_table_schema(table_id, &flat_expected())
        .unwrap();

    let retyped = Schema::new(vec![Field::new("value", DataType::Int64, false)]);
    let error = library
        .assert_table_schema(table_id, &retyped)
        .expect_err("retyped field must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-1 负向：深 schema 嵌套 Struct 子字段不符（深度维）→ `Err(SchemaMismatch)`
/// （正向嵌套全等 Ok）。
#[test]
fn v_5_assert_table_schema_nested() {
    let temp = tempfile::tempdir().unwrap();
    let (bucket, _, _, _) = l3_fixture(temp.path());
    let library = open_library(temp.path());
    let nested_id = nested_id_from(&bucket);

    let expected = Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new(
            "meta",
            DataType::Struct(Fields::from(vec![
                Field::new("kind", DataType::Utf8, false),
                Field::new("v", DataType::Int64, true),
            ])),
            false,
        ),
    ]);
    library.assert_table_schema(nested_id, &expected).unwrap();

    // 仅翻转嵌套子字段 v 的可空性（true → false）→ 深比对在嵌套层命中。
    let flipped = Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new(
            "meta",
            DataType::Struct(Fields::from(vec![
                Field::new("kind", DataType::Utf8, false),
                Field::new("v", DataType::Int64, false),
            ])),
            false,
        ),
    ]);
    let error = library
        .assert_table_schema(nested_id, &flipped)
        .expect_err("nested sub-field mismatch must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-2：注册块 payload 实解 schema 违反 `ArrowCaps.schema` 声明 → `Err`
/// （正向 Ok；`ArrowCaps.schema` 首个消费点，01 §6.2）。
#[test]
fn v_5_assert_block_schema_payload_violation() {
    let temp = tempfile::tempdir().unwrap();
    let (metrics_id, mismatched_id) = registered_fixture(temp.path());
    let library = open_library(temp.path());

    library
        .assert_block_schema(metrics_id)
        .expect("declared-consistent registered payload passes");

    let error = library
        .assert_block_schema(mismatched_id)
        .expect_err("payload Int32 schema must violate the declared Int64 ArrowCaps.schema");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-3 负向：叶子 kind 与 typed 声明不符 → `Err(SchemaMismatch)`（正向 Ok）。
#[test]
fn v_5_assert_schema_typed_kind_mismatch() {
    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct WrongKindTree {
        name: Value,
        table: Sequence, // 图像里该字段实为 Table 块
        seq: ArrowTable,
        nested: ArrowTable,
    }
    let temp = tempfile::tempdir().unwrap();
    let _ = l3_fixture(temp.path());
    let library = open_library(temp.path());

    library.assert_schema::<TableTree>().unwrap();

    let error = library
        .assert_schema::<WrongKindTree>()
        .expect_err("leaf kind must match the typed declaration");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-3 负向：结构不符（typed 声明多一必填字段）→ `Err(SchemaMismatch)`
/// （正向 Ok）。
#[test]
fn v_5_assert_schema_typed_structure_mismatch() {
    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct ExtraFieldTree {
        name: Value,
        table: ArrowTable,
        seq: Sequence,
        nested: ArrowTable,
        extra: Value, // 树图像没有这一子字段
    }
    let temp = tempfile::tempdir().unwrap();
    let _ = l3_fixture(temp.path());
    let library = open_library(temp.path());

    library.assert_schema::<TableTree>().unwrap();

    let error = library
        .assert_schema::<ExtraFieldTree>()
        .expect_err("a typed-declared required field absent from the image is structural");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// **计数器级「不物化叶子」证明**（02 §9.1）：`assert_schema<T>` 走
/// `check_image_children` 结构面，叶子解码调用计数 == 0。
///
/// 夹具为既有 tb 树 + `Blob` 叶。`leaf_counter` 内的测试局部 `Blob` 在
/// `DecodeBlock::decode_block` 递增全局计数——derive 生成的
/// `check_image_children` 在叶子位只消费 `check_block_leaf_identity`（kind /
/// 身份面），**不调用** `decode_block`；对照 `project()`（物化路径，必然解码
/// 两枚 Blob 叶）证明计数器确实能捕获叶子解码。
mod leaf_counter {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tree_space::tree::codec::DecodeBlock;
    use tree_space::{Blob as TsBlob, Block, BlockKind};

    pub static DECODE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone, Debug)]
    pub struct Blob(pub TsBlob);

    impl Block for Blob {
        fn kind(&self) -> BlockKind {
            BlockKind::Blob
        }
        fn write_payload(&self, out: &mut Vec<u8>) {
            <TsBlob as Block>::write_payload(&self.0, out);
        }
    }

    // TreeNode derive 需要叶型实现 `LeafToOut` / `LeafRefOf`（弱投影接缝，
    // 块叶按身份答，payload 不上桥）。
    impl tree_space::tree::LeafToOut for Blob {
        fn to_out(&self) -> tree_space::tree::AccessOut {
            tree_space::tree::AccessOut::Ref(tree_space::tree::LeafRefOf::leaf_ref(self))
        }
    }

    impl tree_space::tree::LeafRefOf for Blob {
        fn leaf_ref(&self) -> tree_space::RefId {
            tree_space::Block::ref_id(self)
        }
    }

    impl DecodeBlock for Blob {
        fn decode_block(kind: &BlockKind, payload: &[u8]) -> tree_space::Result<Self> {
            DECODE_CALLS.fetch_add(1, Ordering::SeqCst);
            let inner = <TsBlob as DecodeBlock>::decode_block(kind, payload)?;
            Ok(Self(inner))
        }
    }

    #[derive(tree_space::TreeCodec, tree_space::TreeNode, Clone, Debug)]
    pub struct BlobTree {
        pub a: Blob,
        pub b: Blob,
    }
}

#[test]
fn v_5_assert_schema_no_leaf_materialization() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create(&root).unwrap();

    let a = leaf_counter::Blob(tree_space::Blob::new(vec![1, 2, 3]));
    let b = leaf_counter::Blob(tree_space::Blob::new(vec![4, 5, 6]));
    let tree = leaf_counter::BlobTree { a, b };
    let mut bucket = Bucket::new();
    bucket.put(&tree.a);
    bucket.put(&tree.b);
    let tree_bytes = encode_node(&tree).unwrap();
    let leaf_refs = tree.leaf_refs();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    let library = TbLibrary::open(&root).unwrap();

    // 断言过程（结构面走完即返回）：叶子解码调用计数 == 0。
    leaf_counter::DECODE_CALLS.store(0, Ordering::SeqCst);
    library
        .assert_schema::<leaf_counter::BlobTree>()
        .expect("structural check passes on the healthy tree");
    assert_eq!(
        leaf_counter::DECODE_CALLS.load(Ordering::SeqCst),
        0,
        "assert_schema must not materialize any leaf payload"
    );

    // 对照：project()（物化路径）必然解码 2 枚 Blob 叶——计数器有效。
    leaf_counter::DECODE_CALLS.store(0, Ordering::SeqCst);
    let materialized: leaf_counter::BlobTree = library.project().unwrap();
    assert_eq!(
        leaf_counter::DECODE_CALLS.load(Ordering::SeqCst),
        2,
        "project materializes every Blob leaf (control for the counter)"
    );
    assert_eq!(materialized.a.0.bytes(), &[1, 2, 3]);
    assert_eq!(materialized.b.0.bytes(), &[4, 5, 6]);
}

// ---------------------------------------------------------------------------
// 总入口
// ---------------------------------------------------------------------------

/// `Integrity ⊂ Consistency ⊂ Schema` 蕴含（02 §9.1）：
/// - 深度字典序成立（Integrity < Consistency < Schema）；
/// - Healthy 库三深度报告全绿（深度上界必含其下各面）；
/// - verify()（L2）已并进 L1 的两个地址重算——tree 指针篡改（tree_id / 三列
///   互验仍通过、仅地址断言命中）在 Integrity 与 Consistency 两个报告里都击中
///   error 条目（同一断言面、两个深度）；
/// - L3 断言面自带 L2/L1 锚（引用解析 + kind / 身份面），且报告面承接
///   未注册 kind 的 `SchemaMismatch` 裁定（V-4 §8.3-1）。
#[test]
fn v_5_validate_depth_entailment() {
    assert!(ValidateDepth::Integrity < ValidateDepth::Consistency);
    assert!(ValidateDepth::Consistency < ValidateDepth::Schema);

    // Healthy：三深度报告全绿（深度上界必含其下）。
    let temp = tempfile::tempdir().unwrap();
    let (bucket, table_id, _, _) = l3_fixture(temp.path());
    let library = open_library(temp.path());
    assert!(bucket.get(table_id).is_some());

    let integrity = validate(
        &library,
        request(ValidateDepth::Integrity, ValidateSubject::Library, None),
    )
    .unwrap();
    assert_eq!(integrity.depth, ValidateDepth::Integrity);
    assert_eq!(integrity.subject, ValidateSubject::Library);
    assert!(!integrity.entries.is_empty());
    assert!(integrity.entries.iter().all(|entry| entry.ok));

    let consistency = validate(
        &library,
        request(ValidateDepth::Consistency, ValidateSubject::Library, None),
    )
    .unwrap();
    assert_eq!(consistency.depth, ValidateDepth::Consistency);
    assert!(!consistency.entries.is_empty());
    assert!(consistency.entries.iter().all(|entry| entry.ok));

    let schema = validate(
        &library,
        request(
            ValidateDepth::Schema,
            ValidateSubject::Block(table_id),
            Some(Arc::new(flat_expected())),
        ),
    )
    .unwrap();
    assert_eq!(schema.depth, ValidateDepth::Schema);
    assert_eq!(schema.subject, ValidateSubject::Block(table_id));
    assert!(!schema.entries.is_empty());
    assert!(schema.entries.iter().all(|entry| entry.ok));

    // L2 报告覆盖 L1 断言面：tree 指针篡改 → Integrity 与 Consistency 报告都
    // 出现 error 条目（verify 内含两地址重算，02 §5.2 插桩点 2）。
    let temp2 = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp2.path());
    tamper_tree_pointer(&fixture.root);

    let integrity_tampered = validate(
        &fixture.library,
        request(ValidateDepth::Integrity, ValidateSubject::Library, None),
    )
    .unwrap();
    assert!(integrity_tampered.entries.iter().any(|entry| {
        !entry.ok
            && entry
                .error
                .as_deref()
                .is_some_and(|message| message.contains("tree blob recomputed address"))
    }));

    let consistency_tampered = validate(
        &fixture.library,
        request(ValidateDepth::Consistency, ValidateSubject::Library, None),
    )
    .unwrap();
    assert!(consistency_tampered.entries.iter().any(|entry| {
        !entry.ok
            && entry
                .error
                .as_deref()
                .is_some_and(|message| message.contains("tree blob recomputed address"))
    }));

    // L3 断言面的 L2/L1 锚 + 报告面承接未注册 kind 裁定。
    let unregistered = validate(
        &library,
        request(
            ValidateDepth::Schema,
            ValidateSubject::Block(table_id),
            None,
        ),
    )
    .unwrap();
    assert!(unregistered.entries.iter().any(|entry| {
        !entry.ok
            && entry
                .error
                .as_deref()
                .is_some_and(|message| message.contains("schema_mismatch"))
    }));

    let registered_ok = {
        // 注册块断言面走「以弱图（注册块）为头」的库：ref 表驱动恢复只装
        // 当前头可达对象（01 §4-5），typed 头不再把注册块装进恢复桶。
        let reg_temp = tempfile::tempdir().unwrap();
        let (metrics_id, _) = registered_fixture(reg_temp.path());
        let reg_library = open_library(reg_temp.path());
        validate(
            &reg_library,
            request(
                ValidateDepth::Schema,
                ValidateSubject::Block(metrics_id),
                None,
            ),
        )
        .expect("registered-block schema report runs")
    };
    assert!(
        registered_ok.entries.iter().all(|entry| entry.ok),
        "declared-consistent registered block passes the L3 report"
    );

    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct WrongKindTree {
        name: Value,
        table: Sequence,
        seq: ArrowTable,
        nested: ArrowTable,
    }
    let error = library
        .assert_schema::<WrongKindTree>()
        .expect_err("L3 typed surface anchors leaf identity / kind (L2 anchor)");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// 所有 Err 场景后数据原样（01 §8「不修复」铁律；无写路径被触发）。
#[test]
fn v_5_validate_no_repair() {
    // A. 树字节篡改 → open Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let tree_path = fixture
        .root
        .join("tb-trees")
        .join(format!("{}.ipc", hex(&fixture.tree_addr.as_bytes())));
    let tampered_tree = b"ARROW1\x00\x01\x02no-repair-tree".to_vec();
    std::fs::write(&tree_path, &tampered_tree).unwrap();
    let before = snapshot_tree(&fixture.root);
    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("tampered tree must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert_eq!(snapshot_tree(&fixture.root), before, "case A: no repair");

    // B. 树指针篡改 → verify + open 都 Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    tamper_tree_pointer(&fixture.root);
    let before = snapshot_tree(&fixture.root);
    assert_eq!(
        fixture.library.verify().unwrap_err().code,
        ErrorCode::DigestMismatch
    );
    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("tampered tree pointer must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert_eq!(snapshot_tree(&fixture.root), before, "case B: no repair");

    // C. ref 指针篡改 → verify + open 都 Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    tamper_ref_pointer(&fixture.root);
    let before = snapshot_tree(&fixture.root);
    assert_eq!(
        fixture.library.verify().unwrap_err().code,
        ErrorCode::DigestMismatch
    );
    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("tampered ref pointer must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DigestMismatch);
    assert_eq!(snapshot_tree(&fixture.root), before, "case C: no repair");

    // D. 块字节篡改 → 默认 verified 读 Err + assert_integrity Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let blob_path = fixture
        .root
        .join("tb-blocks")
        .join(format!("{}.bin", hex(&fixture.block_addr.as_bytes())));
    let tampered_block = b"tampered-block-no-repair".to_vec();
    std::fs::write(&blob_path, &tampered_block).unwrap();
    let before = snapshot_tree(&fixture.root);
    let layout = FlatDirLayout::new(&fixture.root);
    assert_eq!(
        layout
            .read_block_object(fixture.block_addr)
            .unwrap_err()
            .code,
        ErrorCode::DigestMismatch
    );
    assert_eq!(
        fixture.library.assert_integrity().unwrap_err().code,
        ErrorCode::DigestMismatch
    );
    assert_eq!(snapshot_tree(&fixture.root), before, "case D: no repair");

    // E. ref 行字节篡改 → verify Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let ref_path = fixture
        .root
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&fixture.ref_addr.as_bytes())));
    let tampered = tampered_ref_rows_bytes(&fixture.root, fixture.ref_addr);
    std::fs::write(&ref_path, &tampered).unwrap();
    let before = snapshot_tree(&fixture.root);
    assert_eq!(
        fixture.library.verify().unwrap_err().code,
        ErrorCode::DigestMismatch
    );
    assert_eq!(snapshot_tree(&fixture.root), before, "case E: no repair");

    // F. L3 内存面：Table 深 schema 不符 / typed kind 不符 → Err → 无写。
    let temp = tempfile::tempdir().unwrap();
    let (bucket, table_id, _, _) = l3_fixture(temp.path());
    let library = open_library(temp.path());
    let root = temp.path().join("library");
    let before = snapshot_tree(&root);
    let wrong = Schema::new(vec![Field::new("wrong", DataType::Int64, false)]);
    assert!(library.assert_table_schema(table_id, &wrong).is_err());
    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct WrongKindTree {
        name: Value,
        table: Sequence,
        seq: ArrowTable,
        nested: ArrowTable,
    }
    assert!(library.assert_schema::<WrongKindTree>().is_err());
    assert!(bucket.get(table_id).is_some());
    assert_eq!(snapshot_tree(&root), before, "case F: no repair");

    // G. 总入口 validate() 只报告不改写：树字节篡改 → Integrity 报告 error
    // 条目 → 无写。
    let temp = tempfile::tempdir().unwrap();
    let fixture = flat_fixture(temp.path());
    let tree_path = fixture
        .root
        .join("tb-trees")
        .join(format!("{}.ipc", hex(&fixture.tree_addr.as_bytes())));
    std::fs::write(&tree_path, b"ARROW1\x00\x01\x02validate-no-repair").unwrap();
    let before = snapshot_tree(&fixture.root);
    let report = validate(
        &fixture.library,
        request(ValidateDepth::Integrity, ValidateSubject::Library, None),
    )
    .unwrap();
    assert!(report.entries.iter().any(|entry| !entry.ok));
    assert_eq!(snapshot_tree(&fixture.root), before, "case G: no repair");
}
