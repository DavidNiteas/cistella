//! 统一验证面（01 §3）：字典 + 请求 + 报告 + 总入口 + 断言族骨架。
//!
//! 本模块是「数据验证系统」工单的统一入口面（02 §6）：`ValidateDepth`
//! 统一字典、`ValidateRequest` / `ValidateReport` / `ValidateEntry` 统一
//! 请求与报告、`validate()` 总入口（按 depth 调度）、四断言族（fail-fast，
//! 落 `TbLibrary<L>` 库级 `&self`）。
//!
//! 三个验证深度实现各自落点、不物理合并（01 §1）：L1 完整性 →
//! [`TbLibrary::assert_integrity`]（全库字节↔地址）；L2 引用一致性 →
//! `TbLibrary::verify()`（三列互验原位不动，经 [`validate`] 包成报告条目，
//! 02 §6.4-2）；L3 结构 → 断言族（`assert_schema` / `assert_table_schema` /
//! `assert_block_schema`，V-2 落签名 + 骨架，V-4 填为可用实现）。
//!
//! 验证只**见证**不**治理**（01 §8）：L1/L3 一律「不匹配 → Err」，绝不修复 /
//! 改写数据；[`validate`] 报告面把断言层 `Err` 承接为 `ok == false` 的 error
//! 条目（01 §3.4「断言族与报告是两条面」）。

use crate::block::{ArrowTable, BlockCaps, BlockKind, RefId, block_caps};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::Digest;
use crate::layout::TbLayout;
use crate::layout::tb::{TbPointers, block_blob_address, decode_ref_table};
use crate::tb_library::{TbLibrary, assert_ref_table_address, assert_tree_address};
use crate::tree::codec::DecodeTree;
use crate::xpath::XPath;
use arrow::datatypes::{DataType, Field, FieldRef, Schema};
use std::sync::Arc;

/// 验证深度（01 §2）。
///
/// 蕴含关系：`Integrity ⊂ Consistency ⊂ Schema`——按 depth 验证必含其下
/// 全部层次（01 §1）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ValidateDepth {
    /// L1 完整性：块字节 ↔ 哈希（必选）。
    Integrity,
    /// L2 引用一致性：树引用表 ↔ 块身份（必选，已落地）。
    Consistency,
    /// L3 结构：块内容 schema ↔ 类型声明期望（opt-in，显式断言）。
    Schema,
}

/// 验证对象面（01 §3.3）。
///
/// 树路径范围统一走 [`ValidateRequest::path`] 字段（已裁定：`subject::Path`
/// 不是对象面而是过滤条件，02 §6.4-1）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidateSubject {
    /// 整库（块 + 树 + 引用表）。
    Library,
    /// 单个桶块。
    Block(RefId),
}

/// 总入口请求（01 §3.3）。
#[derive(Clone, Debug)]
pub struct ValidateRequest {
    /// 验证深度（必含其下全部，01 §2 蕴含）。
    pub depth: ValidateDepth,
    /// 验证对象面。
    pub subject: ValidateSubject,
    /// L3 期望 schema（深 schema 断言 / `assert_table_schema` 的 `expected`
    /// 通道；schema 缺省时对注册块走 `assert_block_schema`，02 §6.3）。
    pub schema: Option<Arc<Schema>>,
    /// 可选树路径范围过滤条件（`subject::Path` 不在对象面，02 §6.4-1）。
    pub path: Option<XPath>,
}

/// 统一验证报告（01 §3.2）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidateReport {
    /// 本次验证深度（蕴含其下全部，01 §2）。
    pub depth: ValidateDepth,
    /// 本次验证对象面。
    pub subject: ValidateSubject,
    /// 每个验证目标的逐条结论。
    pub entries: Vec<ValidateEntry>,
}

/// 报告条目（目标 + 结果 + 错误信息）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidateEntry {
    /// 验证目标（块 id / 引用表 / 路径等）。
    pub name: String,
    /// 该目标是否通过。
    pub ok: bool,
    /// 失败信息（不匹配 → Err 的文本；仅记录，不触发修复，01 §8）。
    pub error: Option<String>,
}

/// 总入口：按 depth 调度（01 §3.3）。
///
/// - `Integrity` → [`TbLibrary::assert_integrity`]（全库字节↔地址 + 两个
///   地址重算）；
/// - `Consistency` → `TbLibrary::verify()`（三列互验 + 两个地址重算），
///   已被裁定包成报告条目（02 §6.4-2），`verify()` 的 `Result<usize>` 保留
///   为内部实现、不直接作为报告面；
/// - `Schema` → L3 断言族（按 subject / schema / path；V-2 落骨架，
///   V-4 填实现）。
///
/// 报告面只记录、不改写任何数据（01 §8）；断言层 `Err` 由本入口承接为
/// `ok == false` 的 error 条目（01 §3.4），返回 `Ok(report)`。
///
/// **落地取值（02 §6.3）**：权威签名 `validate(req)` 为单参 free function；
/// 本入口需要库句柄才能触达布局与断言面，故取 `validate(library, req)`
/// 形态（库级，与断言族 `&self` 落点一致）。见验收报告。
pub fn validate<L: TbLayout>(
    library: &TbLibrary<L>,
    req: ValidateRequest,
) -> Result<ValidateReport> {
    let depth = req.depth;
    let subject = req.subject.clone();
    let entries = match depth {
        ValidateDepth::Integrity => validate_integrity(library, &req),
        ValidateDepth::Consistency => validate_consistency(library, &req),
        ValidateDepth::Schema => validate_schema(library, &req),
    };
    Ok(ValidateReport {
        depth,
        subject,
        entries,
    })
}

/// L1 全库字节↔地址断言族，落 `TbLibrary<L>` 库级 `&self`（02 §6.3 裁定）。
impl<L: TbLayout> TbLibrary<L> {
    /// L1 全库字节↔地址（含树 / ref 两个地址重算），返回已验证块数。
    ///
    /// 遍历每个桶块对象，重算 `block_blob_address` 与存储寻址名比对；
    /// tree / ref 对象各做一次地址重算（V-1 helper）。任一不匹配 →
    /// `Err(DigestMismatch)`，绝不修复 / 改写数据（01 §4 / §8）。
    /// `BootstrapIncomplete`：库无已提交头或头不是 TB 提交。
    pub fn assert_integrity(&self) -> Result<usize> {
        let pointers = head_pointers(self)?;
        // 每对象每公式只算一次（02 §7.2 免双算）：tree / ref 读走
        // `*_unchecked`，由紧随其后的地址重算（assert_tree_address /
        // assert_ref_table_address）承担与该公式唯一的一次比对。
        let tree_bytes = self
            .layout()
            .read_tree_object_unchecked(Digest::from_bytes(pointers.tree_blob))?;
        assert_tree_address(&tree_bytes, &pointers)?;
        let ref_bytes = self
            .layout()
            .read_ref_object_unchecked(Digest::from_bytes(pointers.refs))?;
        assert_ref_table_address(&ref_bytes, &pointers)?;
        let mut verified = 0_usize;
        for (address, bytes) in self.layout().block_objects()? {
            if block_blob_address(&bytes) != address {
                return Err(TreeSpaceError::new(
                    ErrorCode::DigestMismatch,
                    "tb-blocks blob content does not match its addressing name",
                )
                .with_context("address", address.to_string()));
            }
            verified += 1;
        }
        Ok(verified)
    }

    /// L3 typed 结构 + kind 断言（fail-fast）。
    ///
    /// `T` 声明的结构期望 ↔ 块内容结构；只走结构 + kind、不物化叶子 payload
    /// （01 §6.3）：经 `DecodeTree::check_image_children`（`#[derive(TreeCodec)]`
    /// 生成的结构面专用实现）校验字段集 / 多重度 / locator / chunk-kind，叶子只验
    /// kind / 身份面（`tree/codec.rs::check_block_leaf_identity`），不展开叶子内容。
    /// 不匹配 → `Err(SchemaMismatch)`；悬垂引用 → `Err(DanglingReference)`。
    pub fn assert_schema<T: DecodeTree>(&self) -> Result<()> {
        let image = self.tree_image()?;
        let bucket = self.bucket()?;
        T::check_image_children(image.children(), bucket)
    }

    /// L3 Table 深 Arrow schema 断言（fail-fast）。
    ///
    /// 实解 `block/table.rs` 的 Table payload（`Table::try_read_blob`）→ 与
    /// `expected` 深比对（字段名 / 数据类型 / nullability / 嵌套结构；不比对
    /// 行内容，01 §6.1）。不匹配 → `Err(SchemaMismatch)`；`id` 未指向 Table
    /// 块同样归结构不符（`SchemaMismatch`）。
    pub fn assert_table_schema(&self, id: RefId, expected: &Schema) -> Result<()> {
        let envelope = self.bucket()?.get(id).ok_or_else(|| dangling(id))?;
        if envelope.kind != BlockKind::Table {
            return Err(schema_mismatch("expected a Table block"));
        }
        let table = ArrowTable::try_read_blob(&envelope.payload)?;
        if schema_deep_equal(table.as_batch().schema().as_ref(), expected) {
            Ok(())
        } else {
            Err(schema_mismatch(
                "table payload schema does not match the expected schema",
            ))
        }
    }

    /// L3 注册块断言（fail-fast）。
    ///
    /// 查注册表 `block_caps(name)` → `ArrowCaps.schema`，块 payload 实解 schema
    /// 与之深比对（`block/registry.rs` 声明的**首个消费点**，01 §6.2）。
    /// 未注册 kind → `Err(SchemaMismatch)`（02 §8.3-1 已裁定：无期望 schema 可
    /// 断言，语义属结构 / 类型不符）；注册但无 Arrow 能力（`Opaque`）同归
    /// `SchemaMismatch`。
    pub fn assert_block_schema(&self, id: RefId) -> Result<()> {
        let envelope = self.bucket()?.get(id).ok_or_else(|| dangling(id))?;
        let BlockKind::Named(name) = &envelope.kind else {
            return Err(schema_mismatch(
                "block kind is not registered; no expected schema is declared",
            ));
        };
        let caps = match block_caps(name) {
            Some(BlockCaps::Arrow(caps)) => caps,
            Some(BlockCaps::Opaque) | None => {
                return Err(schema_mismatch(
                    "registered block kind exposes no Arrow schema capability",
                ));
            }
        };
        let batch = crate::ipc::decode_batch(&envelope.payload)?;
        if schema_deep_equal(batch.schema().as_ref(), &caps.schema) {
            Ok(())
        } else {
            Err(schema_mismatch(
                "registered block payload schema does not match its declared ArrowCaps.schema",
            ))
        }
    }
}

/// Reads the committed head's TB pointers, rejecting a missing or non-TB head.
fn head_pointers<L: TbLayout>(library: &TbLibrary<L>) -> Result<TbPointers> {
    let Some((head, _)) = library.layout().read_head()? else {
        return Err(TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "TB validation requires a committed head",
        ));
    };
    let commit = library
        .layout()
        .read_commit(&library.layout().commit_path(head))?;
    commit.tb.ok_or_else(|| {
        TreeSpaceError::new(ErrorCode::BootstrapIncomplete, "head is not a TB commit")
    })
}

/// Integrity 调度面：[`TbLibrary::assert_integrity`]（Library）或单块
/// 字节↔地址（Block，经引用表解析物理地址）。
fn validate_integrity<L: TbLayout>(
    library: &TbLibrary<L>,
    req: &ValidateRequest,
) -> Vec<ValidateEntry> {
    let mut entries = Vec::new();
    match &req.subject {
        ValidateSubject::Library => {
            record_result(&mut entries, "integrity", library.assert_integrity())
        }
        ValidateSubject::Block(id) => record_result(
            &mut entries,
            &id.to_string(),
            assert_block_integrity(library, *id),
        ),
    }
    entries
}

/// 单块 L1：经引用表把 `RefId` 解析为物理地址 → 读对象 → 重算
/// `block_blob_address` 比对。未引用 → `DanglingReference`。
fn assert_block_integrity<L: TbLayout>(library: &TbLibrary<L>, id: RefId) -> Result<usize> {
    let pointers = head_pointers(library)?;
    let ref_bytes = library
        .layout()
        .read_ref_object(Digest::from_bytes(pointers.refs))?;
    let rows = decode_ref_table(&ref_bytes)?;
    let address = rows
        .iter()
        .find(|row| row.ref_id == id.as_bytes())
        .map(|row| row.address)
        .ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "block reference is absent from the reference table",
            )
            .with_context("ref_id", id.to_string())
        })?;
    // 单块 L1（02 §7.2 免双算）：块读走 `*_unchecked`，由紧随其后的
    // `block_blob_address` 显式比对承担该公式唯一一次运算。ref 读后仅
    // `decode_ref_table`、无同公式显式比对 → 保持默认 verified（单算面）。
    let bytes = library
        .layout()
        .read_block_object_unchecked(Digest::from_bytes(address))?;
    if block_blob_address(&bytes) != Digest::from_bytes(address) {
        return Err(TreeSpaceError::new(
            ErrorCode::DigestMismatch,
            "block content hash does not match its address",
        )
        .with_context("address", crate::layout::tb::hex16(&address)));
    }
    Ok(1)
}

/// Consistency 调度面：`TbLibrary::verify()` 三列互验 + 两个地址重算，包成
/// 报告条目（02 §6.4-2 裁定）；`Result<usize>` 保留为内部实现。
fn validate_consistency<L: TbLayout>(
    library: &TbLibrary<L>,
    _req: &ValidateRequest,
) -> Vec<ValidateEntry> {
    let mut entries = Vec::new();
    record_result(&mut entries, "consistency", library.verify());
    entries
}

/// Schema 调度面：按 subject / schema 落到 L3 断言族骨架（V-2 占位）。
///
/// - `Block(id)` + `schema = Some(s)` → `assert_table_schema(id, s)`；
/// - `Block(id)` + `schema = None` → `assert_block_schema(id)`（02 §6.3）；
/// - `Library` → 占位条目：typed 结构断言需调用侧自供 `T`
///   （02 §6.3「`schema` 缺省时对注册块走 `assert_block_schema`」库级面
///   缺块 id，V-4 按 `request.path` 范围填实现）。
fn validate_schema<L: TbLayout>(
    library: &TbLibrary<L>,
    req: &ValidateRequest,
) -> Vec<ValidateEntry> {
    let mut entries = Vec::new();
    match &req.subject {
        ValidateSubject::Library => {
            entries.push(ValidateEntry {
                name: "schema".to_owned(),
                ok: true,
                error: None,
            });
        }
        ValidateSubject::Block(id) => {
            if let Some(expected) = &req.schema {
                record_result(
                    &mut entries,
                    &id.to_string(),
                    library.assert_table_schema(*id, expected),
                );
            } else {
                record_result(
                    &mut entries,
                    &id.to_string(),
                    library.assert_block_schema(*id),
                );
            }
        }
    }
    entries
}

/// 把一条断言结果承接为报告条目：失败 → `ok == false` + error 文本
/// （报告面记录，不修复；01 §3.4 / §8）。
fn record_result<T>(entries: &mut Vec<ValidateEntry>, name: &str, result: Result<T>) {
    match result {
        Ok(_) => entries.push(ValidateEntry {
            name: name.to_owned(),
            ok: true,
            error: None,
        }),
        Err(error) => entries.push(ValidateEntry {
            name: name.to_owned(),
            ok: false,
            error: Some(error.to_string()),
        }),
    }
}

/// L3 结构 / 类型不符的 fail-fast 错误（01 §8：不匹配 → Err、不修复）。
fn schema_mismatch(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}

/// 块 id 在恢复桶中缺失（与 `decode_block_leaf` 同一错误语义）。
fn dangling(id: RefId) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::DanglingReference,
        "block reference is absent from the restored bucket",
    )
    .with_context("ref_id", id.to_string())
}

/// L3 深 Arrow schema 比对（01 §6.1）：字段名 / 数据类型 / nullability /
/// 嵌套结构全等，**不比 metadata / 行内容**。逐层递归、排除 metadata 面，
/// 使「字段集合 / 数据类型 / 可空性 / 嵌套」四个维度精确成立。
fn schema_deep_equal(actual: &Schema, expected: &Schema) -> bool {
    actual.fields().len() == expected.fields().len()
        && actual
            .fields()
            .iter()
            .zip(expected.fields())
            .all(|(a, b)| field_deep_equal(a, b))
}

fn field_deep_equal(a: &Field, b: &Field) -> bool {
    a.name() == b.name()
        && a.is_nullable() == b.is_nullable()
        && data_type_deep_equal(a.data_type(), b.data_type())
}

fn data_type_deep_equal(a: &DataType, b: &DataType) -> bool {
    use arrow::datatypes::DataType as D;
    match (a, b) {
        // 容器类型：递归进嵌套字段（嵌套结构维度的精确面）。
        (D::Struct(a_fields), D::Struct(b_fields)) => fields_deep_equal(a_fields, b_fields),
        (D::List(a_field), D::List(b_field)) => inner_field_deep_equal(a_field, b_field),
        (D::LargeList(a_field), D::LargeList(b_field)) => inner_field_deep_equal(a_field, b_field),
        (D::FixedSizeList(a_field, a_len), D::FixedSizeList(b_field, b_len)) => {
            a_len == b_len && inner_field_deep_equal(a_field, b_field)
        }
        (D::Map(a_field, a_keys), D::Map(b_field, b_keys)) => {
            a_keys == b_keys && inner_field_deep_equal(a_field, b_field)
        }
        (D::RunEndEncoded(a_run, a_encode), D::RunEndEncoded(b_run, b_encode)) => {
            inner_field_deep_equal(a_run, b_run) && inner_field_deep_equal(a_encode, b_encode)
        }
        (D::Dictionary(a_key, a_value), D::Dictionary(b_key, b_value)) => {
            let (a_key, b_key) = (a_key.as_ref(), b_key.as_ref());
            let (a_value, b_value) = (a_value.as_ref(), b_value.as_ref());
            data_type_deep_equal(a_key, b_key) && data_type_deep_equal(a_value, b_value)
        }
        (D::Union(a_fields, a_mode), D::Union(b_fields, b_mode)) => {
            a_mode == b_mode && union_fields_deep_equal(a_fields, b_fields)
        }
        // 叶子类型：类型本身精确相等（含精度 / 时区 / 位宽等构成部分）。
        _ => a == b,
    }
}

fn inner_field_deep_equal(a: &FieldRef, b: &FieldRef) -> bool {
    field_deep_equal(a.as_ref(), b.as_ref())
}

fn fields_deep_equal(a: &arrow::datatypes::Fields, b: &arrow::datatypes::Fields) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| field_deep_equal(a, b))
}

fn union_fields_deep_equal(
    a: &arrow::datatypes::UnionFields,
    b: &arrow::datatypes::UnionFields,
) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((a_type, a_field), (b_type, b_field))| {
                a_type == b_type && field_deep_equal(a_field, b_field)
            })
}
