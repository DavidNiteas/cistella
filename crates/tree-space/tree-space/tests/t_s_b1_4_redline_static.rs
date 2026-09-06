//! TS-B1-1 B1-T4: static redline guards (`_dev/注册面注入/02-施工路线图.md` §5.3
//! 表 B1-T4 / 01 §2.2). The M2 identity golden tests pass unmodified — carried
//! by §3 command 2 (`p_l_2_identity.rs` is not touched by B1; the diff surface
//! is audited at acceptance).
//!
//! Anchors:
//!
//! - `plugin/semantic.rs` keeps the `BlockKind::Named(_) => byte_value(payload)`
//!   branch byte-for-byte — the M2 identity formula of named blocks is a
//!   redline; the guard completes against the committed source text at compile
//!   time via [`include_str!`];
//! - `select_materializer` keeps its public signature
//!   `(Materialization, &BlockKind) -> &'static dyn BlockMaterializer` — pinned
//!   by compiling calls, and its kind-constrained selection behavior (the
//!   write-side materialization face is untouched by B1).

use tree_space::layout::materializer::{materializer_for, select_materializer};
use tree_space::{BlockKind, Materialization};

/// The redline branch text of `plugin/semantic.rs` (M2 named identity =
/// raw payload byte value; `BlockKind::Named(_) => byte_value(payload)`).
const SEMANTIC_NAMED_BRANCH_REDLINE: &str = "BlockKind::Named(_) => byte_value(payload),";

#[test]
fn t_s_b1_4_redline_static() {
    // --- `plugin/semantic.rs` Named branch stays byte-for-byte (M2 redline).
    let semantic_source = include_str!("../src/plugin/semantic.rs");
    assert!(
        semantic_source.contains(SEMANTIC_NAMED_BRANCH_REDLINE),
        "semantic.rs must keep the Named-branch byte formula: {SEMANTIC_NAMED_BRANCH_REDLINE}"
    );
    assert!(
        semantic_source.contains(
            "pub fn semantic_block_value(kind: &BlockKind, payload: &[u8]) -> SemanticValue"
        ),
        "semantic.rs keeps the M2 semantic-value entry point intact"
    );
    assert!(
        semantic_source
            .contains("pub fn semantic_block_ref_id(kind: BlockKind, payload: &[u8]) -> RefId"),
        "semantic.rs keeps the M2 RefId formula entry point intact"
    );

    // --- `select_materializer` public signature pinned by compiling calls:
    // `(Materialization, &BlockKind) -> &'static dyn BlockMaterializer`.
    // The behavior (selection by layout default + kind constraint) is the
    // untouched write-side surface (redline, 01 §2.2).
    let ipc_blob = select_materializer(Materialization::Ipc, &BlockKind::Blob);
    let parquet_blob = select_materializer(Materialization::Parquet, &BlockKind::Blob);
    let parquet_table = select_materializer(Materialization::Parquet, &BlockKind::Table);
    assert!(
        std::ptr::eq(ipc_blob, materializer_for(Materialization::Ipc)),
        "IPC default routes Blob to the IPC materializer"
    );
    assert!(
        std::ptr::eq(parquet_blob, materializer_for(Materialization::Ipc)),
        "kind constraint: a Blob under the Parquet default falls back to the IPC materializer"
    );
    assert!(
        std::ptr::eq(parquet_table, materializer_for(Materialization::Parquet)),
        "ArrowTable under the Parquet default routes to the Parquet materializer"
    );

    // --- The M2 identity golden tests are carried unmodified by §3 command 2
    // (`p_l_2_semantic_identity_cross_encoding`,
    // `p_l_2_identity_goldens_refrozen`, `p_l_2_converter_hook`) — no golden
    // re-freeze here (01 §6 golden discipline).
}
