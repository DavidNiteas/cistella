//! V-1 冒烟：open 恢复 + `TbLibrary::verify()` 的两个地址重算
//! （`tree_blob_address(tree_bytes) == pointers.tree_blob`、
//!  `ref_table_address(ref_bytes) == pointers.refs`）。
//!
//! 断言面（02 §5.3）：正向 commit → open / verify 通过；负向篡改磁盘
//! tree blob / ref 对象字节 → `Err(DigestMismatch)`，且库数据未被改写
//! （不修复语义，01 §8）。全量锚定在 V-5 `tests/v_5_validation.rs`。

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::layout::tb::RefRow;
use tree_space::tree::codec::encode_node;
use tree_space::{ArrowTable, Bucket, Sequence, TbLibrary, TreeCodec, TreeNode, Value};

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Fixture {
    name: Value,
    table: ArrowTable,
    seq: Sequence,
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

/// 提交一颗含两枚叶（table + seq）的树，返回 (tree_bytes, bucket, library)。
fn commit_fixture(root: &std::path::Path) -> (Vec<u8>, Bucket, TbLibrary) {
    let library = TbLibrary::create(root.join("library")).unwrap();
    let table_block = table(&[1, 2]);
    let seq_block = Sequence::new(vec![Value::I32(5), Value::I64(6)]);
    let mut bucket = Bucket::new();
    bucket.put(&table_block);
    bucket.put(&seq_block);
    let fixture = Fixture {
        name: Value::Utf8("exp".into()),
        table: table_block,
        seq: seq_block,
    };
    let tree_bytes = encode_node(&fixture).unwrap();
    let leaf_refs = fixture.leaf_refs();
    let receipt = library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    assert_eq!(
        receipt.tree_blob[..],
        tree_space::layout::tb::tree_blob_address(&tree_bytes).as_bytes()[..],
        "commit tree_blob pointer is the tree-blob address"
    );
    (tree_bytes, bucket, library)
}

#[test]
fn v_1_addr_positive_commit_open_verify() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, _library) = commit_fixture(temp.path());

    let reopened = TbLibrary::open(temp.path().join("library")).unwrap();
    let checked = reopened.verify().unwrap();
    assert_eq!(checked, 2, "fixture has table + seq = 2 leaves");
}

#[test]
fn v_1_addr_tree_tamper_open_err_no_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());

    // tb-trees/ 下唯一文件即已提交树 blob；覆写为受损字节。
    let tree_dir = temp.path().join("library").join("tb-trees");
    let entry = std::fs::read_dir(&tree_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let path = entry.path();
    let tampered = b"ARROW1\x00\x01\x02addr-tamper".to_vec();
    std::fs::write(&path, &tampered).unwrap();
    drop(library);

    let error = match TbLibrary::open(temp.path().join("library")) {
        Ok(_) => panic!("tampered tree blob must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::DigestMismatch);
    let after = std::fs::read(&path).unwrap();
    assert_eq!(after, tampered, "failed open must not rewrite library data");
}

#[test]
fn v_1_addr_ref_tamper_open_err_no_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, _library) = commit_fixture(temp.path());

    // 覆写 ref 对象为「合法编码但行内容不同」的变体：地址重算必与
    // 已提交 refs 锚点不符。
    let ref_path = ref_object_path(temp.path());
    let tampered = tampered_ref_bytes(temp.path());
    std::fs::write(&ref_path, &tampered).unwrap();

    let error = match TbLibrary::open(temp.path().join("library")) {
        Ok(_) => panic!("tampered ref object must be rejected on open"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::DigestMismatch);
    let after = std::fs::read(&ref_path).unwrap();
    assert_eq!(after, tampered, "failed open must not rewrite library data");
}

#[test]
fn v_1_addr_ref_tamper_verify_err_no_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());

    let ref_path = ref_object_path(temp.path());
    let tampered = tampered_ref_bytes(temp.path());
    std::fs::write(&ref_path, &tampered).unwrap();

    let error = match library.verify() {
        Ok(_) => panic!("tampered ref object must be rejected by verify"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::DigestMismatch);
    let after = std::fs::read(&ref_path).unwrap();
    assert_eq!(
        after, tampered,
        "failed verify must not rewrite library data"
    );
}

/// 已提交头的 `tb_refs` 指针 → `tb-refs/{address}.ipc` 路径。
fn ref_object_path(root: &std::path::Path) -> std::path::PathBuf {
    let committed = tree_space::ipc::decode_batch(
        &std::fs::read(root.join("library").join("committed")).unwrap(),
    )
    .unwrap();
    let head_id = committed
        .column(0)
        .as_any()
        .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    let batch = tree_space::ipc::decode_batch(
        &std::fs::read(
            root.join("library")
                .join("commits")
                .join(format!("{}.ipc", hex(&head_id))),
        )
        .unwrap(),
    )
    .unwrap();
    let refs_addr = batch
        .column(6)
        .as_any()
        .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
        .unwrap()
        .value(0)
        .to_vec();
    root.join("library")
        .join("tb-refs")
        .join(format!("{}.ipc", hex(&refs_addr)))
}

/// 读磁盘 ref 对象 → 翻第一行 ref_id 一位 → 重编码（xpath 升序不变，
/// 仍可被 `decode_ref_table` 接受）。
fn tampered_ref_bytes(root: &std::path::Path) -> Vec<u8> {
    let original = std::fs::read(ref_object_path(root)).unwrap();
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
