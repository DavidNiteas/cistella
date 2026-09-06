//! TS-B1-2 B1-T5: downstream zero breakage (`_dev/注册面注入/02-施工路线图.md`
//! §6.3 表 B1-T5 / 02 §3 命令 5 / 01 §4.1 zero migration).
//!
//! `mzml-db` and `uni-mass-db` consume tree-space; they are migration-free
//! today — the B1 injection face (`create_with_registry` /
//! `open_with_registry` / `plugin_registry`) is referenced nowhere in their
//! sources (verified at build time by the guards below), so they stay on the
//! default constructors whose signatures and behavior are byte-identical to
//! the pre-B1 surface (01 §4.1). This file pins, statically:
//!
//! - both crates declare the `tree-space` dependency (they consume the crate
//!   under change);
//! - none of their sources reference the B1-injected API (zero migration —
//!   the consumer code never touched the new face);
//! - their downstream test files exist in the workspace, unmodified; the
//!   runtime rerun of every one of them is carried by §3 command 5
//!   (`cargo test --workspace --all-targets`).
//!
//! These static guards are the compile-time half of the B1-T5 zero-migration
//! assertion; the runtime half (zero failures, zero warnings, all consumer
//! tests green) is §3 command 5 — see the acceptance run in `04-施工日志.md`.

/// The B1-injected API surface tokens: a downstream source referencing any of
/// them would need a migration argument (02 §6.3 zero-migration assertion).
const B1_INJECTION_TOKENS: &[&str] = &[
    "create_with_registry",
    "open_with_registry",
    "plugin_registry",
];

/// Compile-time guard for one downstream source file: it exists (the
/// `include_str!` itself fails the build otherwise) and references none of the
/// B1 injection tokens.
macro_rules! assert_downstream_zero_b1 {
    ($file: literal) => {{
        let source = include_str!($file);
        for token in B1_INJECTION_TOKENS {
            assert!(
                !source.contains(token),
                "downstream source {} references the B1 injection face ({}) — a migration argument is required",
                $file,
                token
            );
        }
    }};
}

#[test]
fn t_s_b1_5_downstream_zero() {
    // --- Both consumers declare their tree-space dependency (Cargo.toml is a
    // compile-time existence/edge check too).
    let mzml_manifest = include_str!("../../mzml-db/Cargo.toml");
    assert!(
        mzml_manifest.contains("tree-space = { path = \"../tree-space\" }"),
        "mzml-db declares the tree-space dependency (consumer side)"
    );
    let uni_manifest = include_str!("../../uni-mass-db/Cargo.toml");
    assert!(
        uni_manifest.contains("tree-space = { path = \"../tree-space\" }"),
        "uni-mass-db declares the tree-space dependency (consumer side)"
    );

    // --- Zero migration: mzml-db sources never touched the B1 face.
    // mzml-db/src full file set (all 15 src files).
    assert_downstream_zero_b1!("../../mzml-db/src/export.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/ids.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/layout.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/lib.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/schema.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/api.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/batches.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/mod.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/mzml_table_dataset.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/parser.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/perf.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/split.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/parse/topology.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/query_tb.rs");
    assert_downstream_zero_b1!("../../mzml-db/src/tb_store.rs");

    // --- Zero migration: uni-mass-db sources never touched the B1 face.
    // uni-mass-db/src full file set (30 src files; the MVP redesign S2
    // retired definition/mvp.rs and added definition/templates.rs +
    // definition/project.rs, anchored below -- S3 added persist/config_map.rs
    // and persist/tb/merged.rs and rewrote persist/tb/conformance.rs, the S1b
    // type extension added persist/type_extension_e2e.rs, the index
    // enhancement work order D3 batch 3 split persist/read/query_face.rs out
    // of read.rs as its private query-face submodule, the M2 batches 1-2
    // added the logical-face and lazy-chain submodules
    // persist/read/logical.rs and persist/read/lazy_chain.rs, the M3
    // batch 1 added the Named Set read face persist/read/named_set.rs and
    // the registered block persist/tb/named_set_block.rs, and the M3
    // batch 2 added the Expr engine persist/read/expr.rs).
    assert_downstream_zero_b1!("../../uni-mass-db/src/definition/id.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/definition/mod.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/definition/project.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/definition/templates.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/ids.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/lib.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/performance.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/arrow.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/config_map.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/family.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/mod.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read/query_face.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read/logical.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read/lazy_chain.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read/named_set.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/read/expr.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/registry.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/type_extension_e2e.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/conformance.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/family_db.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/instance.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/manifest.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/merged.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/named_set_block.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/tb/mod.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/typeid.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/types.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/verify.rs");
    assert_downstream_zero_b1!("../../uni-mass-db/src/persist/writer.rs");

    // --- Consumer test-suite existence anchors (unmodified; the runtime
    // rerun is §3 command 5). `include_str!` is itself the compile-time
    // existence check; the anchors below are the tree-space-adjacent faces:
    // mzml-db's `tb_consumption` (the TbLibrary consumer) and uni-mass-db's
    // identity/golden suites. The MVP closure suite (`mvp_closure`,
    // `mvp_golden`, plus the M3 tier-path and index end-to-end suites
    // `tier_paths`/`index_end_to_end`) is the B1/B2/B3 end-to-end golden
    // authority of the uni-mass-db MVP package and must keep running
    // downstream of tree-space.
    let tb_consumption = include_str!("../../mzml-db/tests/tb_consumption.rs");
    assert!(
        tb_consumption.contains("tree_space"),
        "tb_consumption.rs exercises the tree-space consumer surface"
    );
    let golden_ipc = include_str!("../../uni-mass-db/tests/golden_ipc.rs");
    let persist_identity = include_str!("../../uni-mass-db/tests/persist_identity.rs");
    let mvp_closure = include_str!("../../uni-mass-db/tests/mvp_closure.rs");
    let mvp_golden = include_str!("../../uni-mass-db/tests/mvp_golden.rs");
    let mvp_tier_paths = include_str!("../../uni-mass-db/tests/tier_paths.rs");
    let mvp_index_e2e = include_str!("../../uni-mass-db/tests/index_end_to_end.rs");
    let _ = (
        golden_ipc.len(),
        persist_identity.len(),
        mvp_closure.len(),
        mvp_golden.len(),
        mvp_tier_paths.len(),
        mvp_index_e2e.len(),
    );
}
