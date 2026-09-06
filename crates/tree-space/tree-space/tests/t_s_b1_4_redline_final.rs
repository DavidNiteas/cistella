//! TS-B1-2 B1-T4+: final redline rerun + the explicit M2 golden checklist
//! (`_dev/注册面注入/02-施工路线图.md` §6.3 表 B1-T4+ / 01 §2.2 / §6).
//!
//! TS-B1-1's B1-T4 static redline guards (`t_s_b1_4_redline_static`) re-run
//! unchanged, plus the explicit M2 golden rerun checklist: the three frozen
//! identity-golden tests of `tests/p_l_2_identity.rs` are pinned by exact
//! function names at compile time (source-text guard), and their *runtime
//! rerun* is carried unmodified by §3 command 2 (`-p tree-space`) and command
//! 5 (workspace `--all-targets`). Nothing is re-frozen here (01 §6 golden
//! discipline: this work order adds or changes no byte golden).

use tree_space::layout::materializer::{materializer_for, select_materializer};
use tree_space::{BlockKind, Materialization};

/// The redline branch text of `plugin/semantic.rs` (M2 named identity =
/// raw payload byte value; `BlockKind::Named(_) => byte_value(payload)`, :637).
const SEMANTIC_NAMED_BRANCH_REDLINE: &str = "BlockKind::Named(_) => byte_value(payload),";

/// The M2 identity golden rerun checklist (`tests/p_l_2_identity.rs`): exact
/// function names pinned against the committed source at compile time; their
/// unmodified runtime rerun is carried by §3 command 2 / command 5.
const M2_GOLDEN_RERUN_FNS: &[&str] = &[
    "fn p_l_2_semantic_identity_cross_encoding(",
    "fn p_l_2_identity_goldens_refrozen(",
    "fn p_l_2_converter_hook(",
];

#[test]
fn t_s_b1_4_redline_final() {
    // --- B1-T4 rerun: `plugin/semantic.rs` is untouched — the M2 named
    // identity branch stays byte-for-byte (redline, 01 §2.2).
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

    // --- B1-T4 rerun: `select_materializer` public signature pinned by
    // compiling calls — `(Materialization, &BlockKind) -> &'static dyn
    // BlockMaterializer` (the untouched write-side surface, redline).
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

    // --- Explicit M2 golden rerun checklist: the frozen identity-golden
    // tests are pinned by exact function names in the committed test file;
    // their unmodified runtime rerun is §3 command 2 / command 5 (they are
    // never re-frozen, edited, or skipped here — 01 §6).
    let identity = include_str!("p_l_2_identity.rs");
    for name in M2_GOLDEN_RERUN_FNS {
        assert!(
            identity.contains(name),
            "M2 golden rerun checklist item missing from p_l_2_identity.rs: {name}"
        );
    }
    // The golden file keeps its doc-level identity-class anchoring.
    assert!(
        identity.contains("Identity class (M2 re-frozen values"),
        "the golden file keeps the M2 re-frozen identity anchor"
    );
}
