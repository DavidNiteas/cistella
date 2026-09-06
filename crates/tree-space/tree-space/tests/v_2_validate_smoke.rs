//! V-2 冒烟：统一字典 + 总入口 + 断言族骨架
//! （02 §6.2 测试义务 / §6.5 关闭标准）。
//!
//! 断言面：字典三变体构造 / 匹配；`ValidateRequest` 字段 roundtrip；
//! `validate()` 最小调度链路（Integrity 对 Healthy 库 → 全 `ok == true`
//! entries；不匹配场景 → 报告内 error 条目非空且数据未被改写）；
//! 断言族编译绿 + 行为可跑（`assert_integrity` 可用实现 + 三条 L3 断言：
//! V-2 骨架，V-4 填为可用实现）。
//! 全量锚定在 V-5 `tests/v_5_validation.rs`。

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::encode_node;
use tree_space::{
    ArrowTable, Bucket, RefId, Sequence, TbLibrary, TreeCodec, TreeNode, ValidateDepth,
    ValidateRequest, ValidateSubject, Value, validate,
};

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
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    (tree_bytes, bucket, library)
}

fn integrity_request() -> ValidateRequest {
    ValidateRequest {
        depth: ValidateDepth::Integrity,
        subject: ValidateSubject::Library,
        schema: None,
        path: None,
    }
}

/// 字典三变体：构造 / 匹配 / 排序（02 §6.2 义务 1）。
#[test]
fn v_2_depth_variants_construct_and_match() {
    let depths = [
        ValidateDepth::Integrity,
        ValidateDepth::Consistency,
        ValidateDepth::Schema,
    ];
    for depth in depths {
        match depth {
            ValidateDepth::Integrity | ValidateDepth::Consistency | ValidateDepth::Schema => {}
        }
    }
    assert_eq!(
        depths,
        [
            ValidateDepth::Integrity,
            ValidateDepth::Consistency,
            ValidateDepth::Schema,
        ]
    );
    assert!(ValidateDepth::Integrity < ValidateDepth::Consistency);
    assert!(ValidateDepth::Consistency < ValidateDepth::Schema);
}

/// `ValidateSubject` 两变体：构造 / 匹配（02 §6.2 义务 1 + §6.4-1 裁定形态）。
#[test]
fn v_2_subject_two_variants_construct_and_match() {
    let library_subject = ValidateSubject::Library;
    assert_eq!(library_subject, ValidateSubject::Library);
    let block_subject = ValidateSubject::Block(RefId::from_bytes([7; 16]));
    match block_subject {
        ValidateSubject::Block(id) => assert_eq!(id.as_bytes(), [7; 16]),
        ValidateSubject::Library => panic!("subject must stay a Block(RefId)"),
    }
}

/// `ValidateRequest` 字段 roundtrip：构造 → 取值（02 §6.2 义务 2）。
#[test]
fn v_2_request_roundtrip() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    let req = ValidateRequest {
        depth: ValidateDepth::Schema,
        subject: ValidateSubject::Library,
        schema: Some(schema),
        path: Some(tree_space::xpath::XPath::parse("/table").unwrap()),
    };
    assert_eq!(req.depth, ValidateDepth::Schema);
    assert_eq!(req.subject, ValidateSubject::Library);
    assert_eq!(
        req.schema
            .as_ref()
            .unwrap()
            .field_with_name("value")
            .unwrap()
            .data_type(),
        &DataType::Int32
    );
    assert_eq!(req.path.as_ref().unwrap().to_string(), "/table");
}

/// `validate()` 最小调度链路：Integrity 对 Healthy 库 → 报告含全 `ok ==
/// true` entries（02 §6.2 义务 3）。
#[test]
fn v_2_validate_integrity_healthy_ok() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());
    let report = validate(&library, integrity_request()).unwrap();
    assert_eq!(report.depth, ValidateDepth::Integrity);
    assert_eq!(report.subject, ValidateSubject::Library);
    assert!(!report.entries.is_empty());
    assert!(
        report
            .entries
            .iter()
            .all(|entry| entry.ok && entry.error.is_none()),
        "healthy library must yield only ok entries"
    );
}

/// `validate()` 不匹配场景：篡改树 blob → 报告内 error 条目非空，
/// 且库数据未被改写（报告面记录，不修复；02 §6.2 义务 3 + 01 §8）。
#[test]
fn v_2_validate_integrity_tamper_error_entry_no_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());

    // tb-trees/ 下唯一文件即已提交树 blob；覆写为受损字节 → 地址重算必与
    // 已提交 tree_blob 锚点不符。
    let tree_dir = temp.path().join("library").join("tb-trees");
    let entry = std::fs::read_dir(&tree_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let path = entry.path();
    let tampered = b"ARROW1\x00\x01\x02validate-tamper".to_vec();
    std::fs::write(&path, &tampered).unwrap();

    let report = validate(&library, integrity_request()).unwrap();
    assert!(
        report
            .entries
            .iter()
            .any(|entry| !entry.ok && entry.error.is_some()),
        "tampered tree blob must surface as an error report entry"
    );
    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        after, tampered,
        "failed validation must not rewrite library data"
    );
}

/// `validate()` Consistency 面：走既有 verify（三列互验 + 两个地址重算），
/// 包成报告条目（02 §6.4-2 裁定），Healthy 库全 `ok == true`。
#[test]
fn v_2_validate_consistency_healthy_ok() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());
    let req = ValidateRequest {
        depth: ValidateDepth::Consistency,
        subject: ValidateSubject::Library,
        schema: None,
        path: None,
    };
    let report = validate(&library, req).unwrap();
    assert_eq!(report.depth, ValidateDepth::Consistency);
    assert!(!report.entries.is_empty());
    assert!(
        report
            .entries
            .iter()
            .all(|entry| entry.ok && entry.error.is_none())
    );
}

/// `validate()` Schema 面：按 subject / schema 走 L3 断言族
/// （V-2 占位骨架；V-4 填成可用实现），Healthy 库全 `ok == true`。
#[test]
fn v_2_validate_schema_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, bucket, _library) = commit_fixture(temp.path());
    // L3 断言消费 **恢复态**（restored tree image + bucket，与
    // `TbLibrary::project` 同一接缝，02 §8.1）：commit 只写对象不回灌 state，
    // 断言前须 open 重建加载态。
    let library = TbLibrary::open(temp.path().join("library")).unwrap();
    let ids = bucket.ids().collect::<Vec<_>>();
    let table_id = ids
        .iter()
        .copied()
        .find(|id| {
            matches!(
                bucket.get(*id).map(|envelope| &envelope.kind),
                Some(tree_space::BlockKind::Table)
            )
        })
        .expect("fixture bucket holds the table block");
    let expected = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));

    // Block + schema → assert_table_schema（table 块真实深比对 → ok）。
    let report = validate(
        &library,
        ValidateRequest {
            depth: ValidateDepth::Schema,
            subject: ValidateSubject::Block(table_id),
            schema: Some(expected),
            path: None,
        },
    )
    .unwrap();
    assert!(report.entries.iter().all(|entry| entry.ok));

    // Block 无 schema → assert_block_schema（02 §6.3）：fixture 块均为内建
    // kind、未注册 → 报告承接 `SchemaMismatch` error 条目（02 §8.3-1 裁定，
    // 未注册 = 无期望 schema 可断言）。
    let report = validate(
        &library,
        ValidateRequest {
            depth: ValidateDepth::Schema,
            subject: ValidateSubject::Block(table_id),
            schema: None,
            path: None,
        },
    )
    .unwrap();
    assert!(
        report
            .entries
            .iter()
            .any(|entry| !entry.ok && entry.error.is_some()),
        "assert_block_schema on an unregistered kind must surface as an error entry"
    );
    assert_eq!(
        report.entries[0].error.as_deref(),
        Some("schema_mismatch: block kind is not registered; no expected schema is declared"),
        "the error entry must carry the ruled SchemaMismatch payload"
    );

    // Library 级 → 占位条目（typed 断言需调用侧自供 T）。
    let report = validate(
        &library,
        ValidateRequest {
            depth: ValidateDepth::Schema,
            subject: ValidateSubject::Library,
            schema: None,
            path: None,
        },
    )
    .unwrap();
    assert!(!report.entries.is_empty());
    assert!(report.entries.iter().all(|entry| entry.ok));
}

/// 断言族编译绿 + 行为可跑（02 §6.2 义务 4）：`assert_integrity` 可用实现
/// （Healthy 库返回已验证块数 == 桶块数）+ 三条 L3 断言真实实现
/// （V-2 骨架 → V-4 填实现：typed 结构校验消费恢复态；Table 深比对；注册块
/// `ArrowCaps.schema` 断言——fixture 全为内建未注册 kind 故归 `SchemaMismatch`，
/// 02 §8.3-1 裁定）。
#[test]
fn v_2_assert_family_compiles_and_runs() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, bucket, _library) = commit_fixture(temp.path());
    let library = TbLibrary::open(temp.path().join("library")).unwrap();

    let verified = library.assert_integrity().unwrap();
    assert_eq!(
        verified,
        bucket.len(),
        "verified block count == stored block count"
    );

    library.assert_schema::<Fixture>().unwrap();

    let ids = bucket.ids().collect::<Vec<_>>();
    let table_id = ids
        .iter()
        .copied()
        .find(|id| {
            matches!(
                bucket.get(*id).map(|envelope| &envelope.kind),
                Some(tree_space::BlockKind::Table)
            )
        })
        .expect("fixture bucket holds the table block");
    let expected = Schema::new(vec![Field::new("value", DataType::Int32, false)]);
    library.assert_table_schema(table_id, &expected).unwrap();
    // 未注册 kind → Err(SchemaMismatch)（02 §8.3-1 已裁定）。
    let error = library
        .assert_block_schema(ids[0])
        .expect_err("fixture blocks are built-in and therefore unregistered");
    assert_eq!(error.code, tree_space::ErrorCode::SchemaMismatch);
}

/// `assert_integrity` 负向：篡改桶块字节 → `Err(DigestMismatch)`
/// （L1 不匹配 → Err 不修复，01 §4.1）。
#[test]
fn v_2_assert_integrity_tamper_block_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_bytes, _bucket, library) = commit_fixture(temp.path());

    // 实际落盘目录名是 `tb-blocks/{address}.bin`（layout/flat_dir.rs
    // `tb_blocks_dir`）；doc 注释里的 `tb-blobs` 是文档笔误，全量回归守护。
    let blob_dir = temp.path().join("library").join("tb-blocks");
    let entry = std::fs::read_dir(&blob_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let path = entry.path();
    std::fs::write(&path, b"tampered-block-bytes").unwrap();

    let error = match library.assert_integrity() {
        Ok(_) => panic!("tampered block blob must be rejected by assert_integrity"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::DigestMismatch);
}
