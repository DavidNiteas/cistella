//! E-1: core exchange abstraction + runtime configuration system.
//!
//! Asserts the protocol `交换协议/02-施工路线图.md` §5.4 obligations: block
//! abstraction (tree = special block), compile-time tree constraints,
//! CMake-style configuration (L1 default / L2 override / L3 runtime table),
//! invalidation states, and source construction with the `RegionHandle` reuse.

use std::path::PathBuf;
use std::time::Duration;
use tree_space::shared::SharedRegion;
use tree_space::{
    AccessMode, ExchangeKey, Holder, IndexPolicySetting, InvalidState, IoBlobSetting, IpcSource,
    RefId, RuntimeConfig, RuntimeEntry, RuntimeTable, Source, TreeAccess,
};

/// The tree-access mode is resolved exhaustively: this match has no wildcard,
/// so adding a third `TreeAccess` variant is a compile error under
/// `--all-targets` — the "tree cannot be lazy" constraint is type-enforced
/// (01 §4).
const fn tree_mode(access: TreeAccess) -> u8 {
    match access {
        TreeAccess::Mapped => 1,
        TreeAccess::Full => 2,
    }
}

#[test]
fn e_1_exchange_key_tree_vs_block_distinguishes() {
    let tree = ExchangeKey::Tree;
    let block = ExchangeKey::Block(RefId::from_bytes([7; 16]));
    let classify = |key: ExchangeKey| match key {
        ExchangeKey::Tree => "tree",
        ExchangeKey::Block(id) => {
            assert_eq!(id.as_bytes(), [7; 16]);
            "block"
        }
    };
    assert_eq!(classify(tree), "tree");
    assert_eq!(classify(block), "block");
    assert_ne!(
        ExchangeKey::Tree,
        ExchangeKey::Block(RefId::from_bytes([7; 16]))
    );
}

#[test]
fn e_1_tree_access_is_mapped_or_full_only() {
    assert_eq!(tree_mode(TreeAccess::Mapped), 1);
    assert_eq!(tree_mode(TreeAccess::Full), 2);
}

#[test]
fn e_1_runtime_config_l1_defaults() {
    let defaults = RuntimeConfig::default();
    assert_eq!(defaults.tree_access, TreeAccess::Full);
    assert_eq!(defaults.block_access, AccessMode::Lazy);
    assert_eq!(defaults.sync_timeout, Duration::from_secs(5));
    // Rect work-order P2 ruling: Balanced + Complete defaults.
    assert_eq!(defaults.index_policy, IndexPolicySetting::Balanced);
    assert_eq!(defaults.io_blob_policy, IoBlobSetting::Complete);
    let frozen = RuntimeConfig {
        tree_access: TreeAccess::Full,
        block_access: AccessMode::Lazy,
        sync_timeout: Duration::from_secs(5),
        index_policy: IndexPolicySetting::Balanced,
        io_blob_policy: IoBlobSetting::Complete,
    };
    assert_eq!(defaults, frozen);
}

#[test]
fn e_1_runtime_config_from_kv_overrides() {
    let config = RuntimeConfig::from_kv(
        "# comment line\ntree_access = mapped\nblock_access = full\nsync_timeout = 1234\n\
         index_policy = space\nio_blob_policy = seeds\n",
    )
    .unwrap();
    assert_eq!(config.tree_access, TreeAccess::Mapped);
    assert_eq!(config.block_access, AccessMode::Full);
    assert_eq!(config.sync_timeout, Duration::from_millis(1234));
    assert_eq!(config.index_policy, IndexPolicySetting::Space);
    assert_eq!(config.io_blob_policy, IoBlobSetting::Seeds);
}

#[test]
fn e_1_runtime_config_from_kv_partial_override() {
    let config = RuntimeConfig::from_kv("  sync_timeout = 42  \n\n").unwrap();
    assert_eq!(config.tree_access, TreeAccess::Full);
    assert_eq!(config.block_access, AccessMode::Lazy);
    assert_eq!(config.sync_timeout, Duration::from_millis(42));
    // The rect P2 defaults persist under a partial override.
    assert_eq!(config.index_policy, IndexPolicySetting::Balanced);
    assert_eq!(config.io_blob_policy, IoBlobSetting::Complete);
}

#[test]
fn e_1_runtime_config_from_kv_rejects_invalid() {
    let inputs = [
        "unknown_key = full",
        "tree_access = bogus",
        "tree_access = lazy", // lazy is a block mode, never a tree mode
        "block_access = face",
        "sync_timeout = abc",
        "sync_timeout = 0",
        "sync_timeout = -1",
        "sync_timeout = 1.5",
        "index_policy = bogus",
        "index_policy = lazy",
        "io_blob_policy = face",
        "865001",        // no '=' separator
        "= full",        // empty key
        "tree_access =", // empty value
    ];
    for text in inputs {
        assert!(
            RuntimeConfig::from_kv(text).is_err(),
            "config text `{text}` must be rejected"
        );
    }
}

#[test]
fn e_1_runtime_config_builder_chain() {
    let config = RuntimeConfig::default()
        .with_tree_access(TreeAccess::Mapped)
        .with_block_access(AccessMode::Full)
        .with_sync_timeout(Duration::from_millis(999))
        .with_index_policy(IndexPolicySetting::Time)
        .with_io_blob_policy(IoBlobSetting::Seeds);
    assert_eq!(config.tree_access, TreeAccess::Mapped);
    assert_eq!(config.block_access, AccessMode::Full);
    assert_eq!(config.sync_timeout, Duration::from_millis(999));
    assert_eq!(config.index_policy, IndexPolicySetting::Time);
    assert_eq!(config.io_blob_policy, IoBlobSetting::Seeds);
}

#[test]
fn e_1_runtime_table_from_config_generates_tree_entry() {
    let config = RuntimeConfig {
        tree_access: TreeAccess::Mapped,
        ..RuntimeConfig::default()
    };
    let table = RuntimeTable::from_config(&config);
    let tree = table.tree_entry();
    assert_eq!(
        tree,
        &RuntimeEntry {
            holder: Holder::Own,
            access: AccessMode::Mapped,
            source: None,
            invalid: InvalidState::Valid,
        }
    );
    assert!(table.block_entry(RefId::from_bytes([1; 16])).is_none());
}

#[test]
fn e_1_runtime_table_from_config_maps_tree_access() {
    assert_eq!(
        RuntimeTable::from_config(&RuntimeConfig::default())
            .tree_entry()
            .access,
        AccessMode::Full
    );
}

#[test]
fn e_1_runtime_table_block_entry_set_get_roundtrip() {
    let mut table = RuntimeTable::from_config(&RuntimeConfig::default());
    let first = RefId::from_bytes([1; 16]);
    let second = RefId::from_bytes([2; 16]);
    let mapped = RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        access: AccessMode::Mapped,
        source: Some(Source::Disk(PathBuf::from("peer-library"))),
        invalid: InvalidState::UpstreamChanged,
    };
    table.set_block_entry(first, mapped.clone());
    table.set_block_entry(second, mapped.clone());
    assert_eq!(table.block_entry(first), Some(&mapped));
    assert_eq!(table.block_entry(second), Some(&mapped));

    let replacement = RuntimeEntry::default();
    table.set_block_entry(first, replacement.clone());
    assert_eq!(table.block_entry(first), Some(&replacement));
    assert!(table.block_entry(RefId::from_bytes([9; 16])).is_none());
}

#[test]
fn e_1_invalid_state_has_three_variants() {
    let flag = |state: InvalidState| match state {
        InvalidState::Valid => 0,
        InvalidState::UpstreamChanged => 1,
        InvalidState::UpstreamDead => 2,
    };
    assert_eq!(flag(InvalidState::Valid), 0);
    assert_eq!(flag(InvalidState::UpstreamChanged), 1);
    assert_eq!(flag(InvalidState::UpstreamDead), 2);
    assert_ne!(InvalidState::Valid, InvalidState::UpstreamChanged);
    assert_ne!(InvalidState::Valid, InvalidState::UpstreamDead);
    assert_ne!(InvalidState::UpstreamChanged, InvalidState::UpstreamDead);
}

#[test]
fn e_1_source_disk_and_ipc_construction() {
    assert_eq!(
        Source::Disk(PathBuf::from("peer-library")),
        Source::Disk(PathBuf::from("peer-library"))
    );

    // IpcSource reuses the shared.rs RegionHandle produced by export().
    let payload = b"e1-ipc-payload".to_vec();
    let region = SharedRegion::publish(payload.clone()).unwrap();
    let ipc = IpcSource::new(region.export().unwrap());
    let source = Source::Ipc(ipc.clone());
    assert_eq!(source, Source::Ipc(ipc.clone()));
    assert!(matches!(source, Source::Ipc(_)));

    // The wrapped handle still opens the very same shared bytes zero-copy.
    let reopened = SharedRegion::open(ipc.into_handle()).unwrap();
    assert_eq!(reopened.bytes(), payload.as_slice());
}
