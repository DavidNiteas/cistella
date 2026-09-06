//! Versioned plugin system for `tree-space` (plugin overhaul: `01-目标与设计.md` §4).
//!
//! PL-1 lands the layout-version enums (S1), the block/tree plugin traits,
//! their disk implementations and the registry (S2); boot blocks and
//! degraded-block semantics land in later stages of the same milestone.

/// Block plugins: `BlockPlugin` trait plus the Arrow IPC / Parquet disk
/// implementations.
pub mod block;
/// Boot block: the frozen four-column record written at library creation
/// (01 §4-3).
pub mod boot;
/// Degraded-block residency: `DegradedBlock` + `DiskVersion` for
/// plugin-unreachable blocks (01 §4-5).
pub mod degraded;
/// Plugin registry: registration, companion-pair conflicts, and routing.
pub mod registry;
/// M2 semantic identity (02 §6.3 / 01 §4-4): non-Arrow self-describing
/// fingerprints (`SemanticValue` / `semantic_table` / `semantic_tree`).
pub mod semantic;
/// Tree plugins: `TreePlugin` trait plus the `Arrow55` tree plugin.
pub mod tree;
/// Layout-version enums: a closed in-memory enum plus explicit on-disk strings.
pub mod version;
