//! V-4 冒烟：L3 结构断言三件套（02 §8.2 测试义务 / §8.4 关闭标准）。
//!
//! 覆盖：`assert_table_schema`（Table 深 Arrow schema，任选维度不符 → Err）；
//! `assert_block_schema`（`ArrowCaps.schema` 首个消费点：注册块 payload 实解
//! schema 比对 / 未注册 kind → `Err(SchemaMismatch)` 裁定锚定）；`assert_schema<T>`
//! （typed 结构 + kind，不物化叶子 payload——由 derive 生成的
//! `DecodeTree::check_image_children` 保证叶子位只走 kind / 身份面）。
//!
//! 全量锚定在 V-5 `tests/v_5_validation.rs`（含
//! `v_5_assert_schema_no_leaf_materialization` 的计数器级行为证明）。

use arrow::array::{ArrayRef, Int32Array, Int64Array, StringArray, StructArray};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use arrow::record_batch::RecordBatch;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tree_space::tree::codec::{ImageContent, TreeImage, encode, encode_node, named_field};
use tree_space::{
    ArrowCaps, ArrowTable, BlockCaps, Bucket, ErrorCode, RefId, RegisteredBlock, Sequence,
    TbLibrary, TreeCodec, TreeNode, Value, image_leaf_refs, register_block,
};

/// 注册块的名字空间（AB-1 名字规则允许，注册面零改动，02 §8.2）。
const METRICS_NAME: &str = "v4.metrics";
const MISMATCHED_NAME: &str = "v4.mismatched";
const CAPS_ONLY_NAME: &str = "v4.caps-only";

fn count_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        "count",
        DataType::Int64,
        false,
    )]))
}

/// 正向注册块：payload 实解 schema 与 `ArrowCaps.schema` 一致。
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetricsBlock(Vec<i64>);

impl RegisteredBlock for MetricsBlock {
    const NAME: &'static str = METRICS_NAME;
    fn encode(&self) -> Vec<u8> {
        tree_space::ipc::encode_batch(
            &RecordBatch::try_new(
                count_schema(),
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
            schema: count_schema(),
            to_batch: |bytes| tree_space::ipc::decode_batch(bytes),
        })
    }
}

/// 负向注册块：payload 实解 schema（Int32）与 `ArrowCaps.schema`（Int64）不符
/// ——内容自洽、可正常提交（L1/L2 均无异议），唯 L3 断言捕获。
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
            schema: count_schema(), // 故意声明 Int64，与 payload（Int32）不符
            to_batch: |bytes| tree_space::ipc::decode_batch(bytes),
        })
    }
}

/// 隔离证明：`ArrowCaps` 声明成立但用户解码器（`decode` / `to_batch`）会炸——
/// `assert_block_schema` 只消费 `ArrowCaps.schema` 与实解（IPC decode），
/// 绝不调用用户解码器（首个消费点，01 §6.2）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct CapsOnlyBlock(Vec<i64>);

impl RegisteredBlock for CapsOnlyBlock {
    const NAME: &'static str = CAPS_ONLY_NAME;
    fn encode(&self) -> Vec<u8> {
        MetricsBlock(self.0.clone()).encode()
    }
    fn decode(_bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        panic!("registered decode must never run under the L3 schema assertion")
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Arrow(ArrowCaps {
            schema: count_schema(),
            to_batch: |_| panic!("to_batch must never run under the L3 schema assertion"),
        })
    }
}

fn ensure_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_block::<MetricsBlock>().unwrap();
        register_block::<MismatchedBlock>().unwrap();
        register_block::<CapsOnlyBlock>().unwrap();
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
struct V4Tree {
    name: Value,
    table: ArrowTable,
    seq: Sequence,
    nested: ArrowTable,
}

/// 两次 commit 的夹具：tree1（弱图）把三个注册块写进对象通道（typed 树
/// 无法承载注册块叶子，经弱图引用进桶）；tree2（typed `V4Tree`）为当前头。
///
/// 插件化改造（01 §4-5）后 open 走 **ref 表驱动**恢复：只装当前头引用表行
/// 对应的对象——tree2（typed）头部引用的三个注册块是「未被头树引用的孤儿
/// 对象」，**不再进恢复桶**；注册块的 L3 断言走 [`weak_fixture`]（弱图为头）。
fn commit_fixture(
    root: &Path,
) -> (
    Bucket,
    tree_space::RefId,
    tree_space::RefId,
    tree_space::RefId,
) {
    ensure_registered();
    let library = TbLibrary::create(root.join("library")).unwrap();

    let metrics = MetricsBlock(vec![1, 2, 3]);
    let mismatched = MismatchedBlock(vec![9, 8]);
    let caps_only = CapsOnlyBlock(vec![4, 5]);
    let mut weak_bucket = Bucket::new();
    let metrics_id = weak_bucket.put(&metrics);
    let mismatched_id = weak_bucket.put(&mismatched);
    let caps_only_id = weak_bucket.put(&caps_only);
    let weak_image = TreeImage::new(vec![
        named_field("metrics", ImageContent::Ref(metrics_id)),
        named_field("mismatched", ImageContent::Ref(mismatched_id)),
        named_field("caps_only", ImageContent::Ref(caps_only_id)),
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
    let _nested_id = bucket.put(&nested_block);
    let _seq_id = bucket.put(&seq_block);
    let fixture = V4Tree {
        name: Value::Utf8("exp".into()),
        table: table_block,
        seq: seq_block,
        nested: nested_block,
    };
    let tree_bytes = encode_node(&fixture).unwrap();
    let leaf_refs = fixture.leaf_refs();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();
    (bucket, table_id, metrics_id, mismatched_id)
}

fn open_library(root: &Path) -> TbLibrary {
    TbLibrary::open(root.join("library")).unwrap()
}

/// 注册块 L3 夹具：以「弱图（三个注册块）」为**当前头**的库。ref 表驱动恢复
/// 只装当前头引用表行可达的对象（插件化 01 §4-5），注册块须由头树可达才进
/// 恢复桶——L3 `assert_block_schema` 的恢复态首消费点因此落到本夹具。
fn weak_fixture(root: &Path) -> (RefId, RefId, RefId) {
    ensure_registered();
    let library = TbLibrary::create(root.join("library")).unwrap();
    let metrics = MetricsBlock(vec![1, 2, 3]);
    let mismatched = MismatchedBlock(vec![9, 8]);
    let caps_only = CapsOnlyBlock(vec![4, 5]);
    let mut weak_bucket = Bucket::new();
    let metrics_id = weak_bucket.put(&metrics);
    let mismatched_id = weak_bucket.put(&mismatched);
    let caps_only_id = weak_bucket.put(&caps_only);
    let weak_image = TreeImage::new(vec![
        named_field("metrics", ImageContent::Ref(metrics_id)),
        named_field("mismatched", ImageContent::Ref(mismatched_id)),
        named_field("caps_only", ImageContent::Ref(caps_only_id)),
    ]);
    let weak_bytes = encode(&weak_image).unwrap();
    let weak_refs = image_leaf_refs(&weak_image).unwrap();
    library
        .commit(&weak_bytes, &weak_refs, &weak_bucket)
        .unwrap();
    (metrics_id, mismatched_id, caps_only_id)
}

fn flat_expected() -> Schema {
    Schema::new(vec![Field::new("value", DataType::Int32, false)])
}

/// L3-1 正向：Table 深 schema 全等（字段名 / 类型 / nullability / 嵌套）。
#[test]
fn v_4_assert_table_schema_ok() {
    let temp = tempfile::tempdir().unwrap();
    let (bucket, table_id, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    assert!(bucket.get(table_id).is_some());
    library
        .assert_table_schema(table_id, &flat_expected())
        .unwrap();
    let nested_id = bucket
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
        .expect("nested table block is in the restored bucket");
    let expected_nested = Schema::new(vec![
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
    library
        .assert_table_schema(nested_id, &expected_nested)
        .unwrap();
}

/// L3-1 负向：字段名不符 → `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_table_schema_field_name_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_, table_id, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let renamed = Schema::new(vec![Field::new("value_renamed", DataType::Int32, false)]);
    let error = library
        .assert_table_schema(table_id, &renamed)
        .expect_err("renamed field must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-1 负向：数据类型不符 → `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_table_schema_field_type_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_, table_id, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let retyped = Schema::new(vec![Field::new("value", DataType::Int64, false)]);
    let error = library
        .assert_table_schema(table_id, &retyped)
        .expect_err("retyped field must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-1 负向：nullability 不符 → `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_table_schema_nullability_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_, table_id, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let nullable = Schema::new(vec![Field::new("value", DataType::Int32, true)]);
    let error = library
        .assert_table_schema(table_id, &nullable)
        .expect_err("flipped nullability must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-1 负向：嵌套 Struct 子字段不符（深度维）→ `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_table_schema_nested_field_mismatch_err() {
    let temp = tempfile::tempdir().unwrap();
    let (bucket, _, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let nested_id = bucket
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
        .expect("nested table block is in the restored bucket");
    // 仅翻转嵌套子字段 v 的可空性（true → false）→ 深比对必须在嵌套层命中。
    let expected = Schema::new(vec![
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
        .assert_table_schema(nested_id, &expected)
        .expect_err("nested sub-field mismatch must be a deep-schema mismatch");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-2 正向：注册块 payload 实解 schema == `ArrowCaps.schema`（首个消费点）。
#[test]
fn v_4_assert_block_schema_ok() {
    let temp = tempfile::tempdir().unwrap();
    let (metrics_id, _, _) = weak_fixture(temp.path());
    let library = open_library(temp.path());
    library.assert_block_schema(metrics_id).unwrap();
}

/// L3-2 负向：注册块 payload 实解 schema 与声明不符 → `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_block_schema_payload_schema_violation_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_, mismatched_id, _) = weak_fixture(temp.path());
    let library = open_library(temp.path());
    let error = library
        .assert_block_schema(mismatched_id)
        .expect_err("payload Int32 schema must violate the declared Int64 ArrowCaps.schema");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-2 隔离证明：断言只走 `ArrowCaps.schema` + IPC 实解，经 `decode` /
/// `to_batch` 的用户解码器绝不触达（`CapsOnlyBlock` 的解码器会 panic）。
#[test]
fn v_4_assert_block_schema_never_invokes_user_decoders() {
    let temp = tempfile::tempdir().unwrap();
    let _ = weak_fixture(temp.path());
    let library = open_library(temp.path());
    let restored = library.bucket().unwrap();
    let caps_only_id = restored
        .ids()
        .find(|id| {
            matches!(
                restored.get(*id).map(|envelope| &envelope.kind),
                Some(tree_space::BlockKind::Named(name)) if name.as_ref() == CAPS_ONLY_NAME
            )
        })
        .expect("caps-only block is in the restored bucket");
    library.assert_block_schema(caps_only_id).unwrap();
}

/// L3-2 裁定锚定：未注册 kind → `Err(SchemaMismatch)`（02 §8.3-1）。
#[test]
fn v_4_assert_block_schema_unregistered_kind_err() {
    let temp = tempfile::tempdir().unwrap();
    let (_, table_id, _, _) = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let error = library
        .assert_block_schema(table_id)
        .expect_err("built-in Table kind is unregistered; no expected schema is declared");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-3 正向：typed 结构 + kind 断言全等（叶子只验 kind / 身份面）。
#[test]
fn v_4_assert_schema_typed_ok() {
    let temp = tempfile::tempdir().unwrap();
    let _ = commit_fixture(temp.path());
    let library = open_library(temp.path());
    library.assert_schema::<V4Tree>().unwrap();
}

/// L3-3 负向：叶子 kind 与 typed 声明不符 → `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_schema_typed_kind_mismatch_err() {
    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct WrongKindTree {
        name: Value,
        table: Sequence, // 图像里该字段实为 Table 块
        seq: ArrowTable,
        nested: ArrowTable,
    }
    let temp = tempfile::tempdir().unwrap();
    let _ = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let error = library
        .assert_schema::<WrongKindTree>()
        .expect_err("leaf kind must match the typed declaration");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

/// L3-3 负向：结构不符（typed 声明多一必填字段）→ `Err(SchemaMismatch)`。
#[test]
fn v_4_assert_schema_typed_structure_mismatch_err() {
    #[derive(TreeCodec, TreeNode, Clone, Debug)]
    struct ExtraFieldTree {
        name: Value,
        table: ArrowTable,
        seq: Sequence,
        nested: ArrowTable,
        extra: Value, // 树图像没有这一子字段
    }
    let temp = tempfile::tempdir().unwrap();
    let _ = commit_fixture(temp.path());
    let library = open_library(temp.path());
    let error = library
        .assert_schema::<ExtraFieldTree>()
        .expect_err("a typed-declared required field absent from the image is structural");
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}
