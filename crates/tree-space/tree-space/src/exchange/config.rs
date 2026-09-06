//! Runtime configuration system (protocol `交换协议/01-目标与设计.md` §2).
//!
//! A CMake-style layered configuration (01 §2):
//!
//! ```text
//! L1  code-embedded defaults (compile time, shipped by the library)
//! L2  user config-file / builder overrides (post-compile, deploy time)
//! L3  runtime table generated from the effective L2 config  -> RuntimeTable
//! ```
//!
//! The configuration system is the **generator**; the runtime table is the
//! **output** (01 §2). Runtime events recorded in entries — upstream death →
//! invalidation, source changes — are runtime overlay and are **not**
//! persisted; only the L1/L2 rules are persistent.

use super::{AccessMode, ExchangeKey, Holder, InvalidState, Source, TreeAccess};
use crate::block::RefId;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use std::collections::BTreeMap;
use std::time::Duration;

/// Index build-tier policy setting (L1/L2 face, rect work order 02 §5.1):
/// the tree-space-local mirror of `table-index::IndexPolicy`'s three
/// variants. This crate never depends on `table-index` (the direction is the
/// other way), so the runtime config carries its own value domain; the
/// mapping to the build policy happens in the persist integration layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPolicySetting {
    /// Space = L0 + L1: tree specs are excluded from L2.
    Space,
    /// Time = L0 + L1 + L2: every tree spec is built.
    Time,
    /// Balanced = L0 + L1 + tree specs built (the rect work-order default:
    /// a declared spatial tree is built under zero configuration).
    Balanced,
}

/// Index blob persistence mode (rect work order 02 §5.1, the 7.10 ruling):
/// `Seeds` = space-lean frames, the read side watches the frame header and
/// rebuilds lazily when geometry is absent; `Complete` = full geometry
/// frames (`rstar-rect.v1`), open-and-query with zero rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoBlobSetting {
    /// Seeds mode: space-lean persistence; rect slots drop their blob and
    /// rebuild lazily from the data columns on read.
    Seeds,
    /// Complete mode: frame geometry is fully persisted; rect indexes open
    /// and query with zero runtime rebuild.
    Complete,
}

/// The effective configuration after applying L1 defaults and L2 overrides
/// (01 §2). The schema covers the fields currently used by the protocol; the
/// schema is extensible downstream (01 §2).
///
/// `sync_timeout` is the proxy-write / synchronization timeout; an expired
/// upstream manifests as invalidation (01 §3.2, §7).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Default read access of the tree (only `Mapped`/`Full`, 01 §4).
    pub tree_access: TreeAccess,
    /// Default read access of bucket blocks (01 §4).
    pub block_access: AccessMode,
    /// Synchronization / proxy-write timeout (01 §3.2).
    pub sync_timeout: Duration,
    /// Index build-tier policy (rect work order 02 §5.1; L1 default
    /// `Balanced` per the P2 ruling).
    pub index_policy: IndexPolicySetting,
    /// Index blob persistence mode (rect work order 02 §5.1; L1 default
    /// `Complete` per the P2 ruling — open-and-query for rect indexes).
    pub io_blob_policy: IoBlobSetting,
}

impl Default for RuntimeConfig {
    /// L1: code-embedded defaults (protocol `02-施工路线图.md` §5.6): tree
    /// `Full`, blocks `Lazy`, 5 s timeout; the rect work-order P2 ruling
    /// adds `index_policy = Balanced` (a declared spatial tree is built
    /// under zero configuration — the point-cloud R-tree path needs it) and
    /// `io_blob_policy = Complete` (rect indexes open and query with zero
    /// rebuild — the geo-tree acceptance contract).
    fn default() -> Self {
        Self {
            tree_access: TreeAccess::Full,
            block_access: AccessMode::Lazy,
            sync_timeout: Duration::from_secs(5),
            index_policy: IndexPolicySetting::Balanced,
            io_blob_policy: IoBlobSetting::Complete,
        }
    }
}

impl RuntimeConfig {
    /// L2: overlays the defaults from `key = value` text.
    ///
    /// Parsing is hand-written with **zero new dependencies** (introducing
    /// toml/serde is forbidden, protocol `02-施工路线图.md` §5.6). Recognized
    /// keys and values:
    ///
    /// - `tree_access` = `mapped` | `full`
    /// - `block_access` = `mapped` | `full` | `lazy`
    /// - `sync_timeout` = positive integer milliseconds
    /// - `index_policy` = `space` | `time` | `balanced`
    /// - `io_blob_policy` = `seeds` | `complete`
    ///
    /// Blank lines and full-line `#` comments are skipped; later lines
    /// override earlier ones for the same key. Unknown keys, malformed lines
    /// (no `=`), and invalid values are rejected.
    pub fn from_kv(kv: &str) -> Result<Self> {
        let mut config = Self::default();
        for (line_index, raw) in kv.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(separator) = line.find('=') else {
                return Err(config_line_error(
                    line_index,
                    "malformed line: expected `key = value`",
                ));
            };
            let key = line[..separator].trim();
            let value = line[separator + 1..].trim();
            match key {
                "tree_access" => config.tree_access = parse_tree_access(value)?,
                "block_access" => config.block_access = parse_block_access(value)?,
                "sync_timeout" => config.sync_timeout = parse_sync_timeout(value)?,
                "index_policy" => config.index_policy = parse_index_policy(value)?,
                "io_blob_policy" => config.io_blob_policy = parse_io_blob_policy(value)?,
                other => {
                    return Err(config_key_error(
                        other,
                        format!("unknown runtime config key (line {})", line_index + 1),
                    ));
                }
            }
        }
        Ok(config)
    }

    /// L2 builder override: sets the tree access, keeping the other fields.
    pub fn with_tree_access(mut self, tree: TreeAccess) -> Self {
        self.tree_access = tree;
        self
    }
    /// L2 builder override: sets the block access, keeping the other fields.
    pub fn with_block_access(mut self, block: AccessMode) -> Self {
        self.block_access = block;
        self
    }
    /// L2 builder override: sets the sync timeout, keeping the other fields.
    pub fn with_sync_timeout(mut self, timeout: Duration) -> Self {
        self.sync_timeout = timeout;
        self
    }
    /// L2 builder override: sets the index build-tier policy (rect work order
    /// 02 §5.3), keeping the other fields.
    pub fn with_index_policy(mut self, policy: IndexPolicySetting) -> Self {
        self.index_policy = policy;
        self
    }
    /// L2 builder override: sets the index blob persistence mode (rect work
    /// order 02 §5.3), keeping the other fields.
    pub fn with_io_blob_policy(mut self, mode: IoBlobSetting) -> Self {
        self.io_blob_policy = mode;
        self
    }
}

fn parse_index_policy(value: &str) -> Result<IndexPolicySetting> {
    match value {
        "space" => Ok(IndexPolicySetting::Space),
        "time" => Ok(IndexPolicySetting::Time),
        "balanced" => Ok(IndexPolicySetting::Balanced),
        _ => Err(config_value_error(
            "index_policy",
            value,
            "expected `space`, `time`, or `balanced`",
        )),
    }
}

fn parse_io_blob_policy(value: &str) -> Result<IoBlobSetting> {
    match value {
        "seeds" => Ok(IoBlobSetting::Seeds),
        "complete" => Ok(IoBlobSetting::Complete),
        _ => Err(config_value_error(
            "io_blob_policy",
            value,
            "expected `seeds` or `complete`",
        )),
    }
}

fn parse_tree_access(value: &str) -> Result<TreeAccess> {
    match value {
        "mapped" => Ok(TreeAccess::Mapped),
        "full" => Ok(TreeAccess::Full),
        _ => Err(config_value_error(
            "tree_access",
            value,
            "expected `mapped` or `full`",
        )),
    }
}

fn parse_block_access(value: &str) -> Result<AccessMode> {
    match value {
        "mapped" => Ok(AccessMode::Mapped),
        "full" => Ok(AccessMode::Full),
        "lazy" => Ok(AccessMode::Lazy),
        _ => Err(config_value_error(
            "block_access",
            value,
            "expected `mapped`, `full`, or `lazy`",
        )),
    }
}

fn parse_sync_timeout(value: &str) -> Result<Duration> {
    let millis = value.parse::<u64>().map_err(|_| {
        config_value_error(
            "sync_timeout",
            value,
            "expected a positive integer (milliseconds)",
        )
    })?;
    if millis == 0 {
        return Err(config_value_error(
            "sync_timeout",
            value,
            "expected a positive integer (milliseconds)",
        ));
    }
    Ok(Duration::from_millis(millis))
}

fn config_line_error(line_index: usize, message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
        .with_context("line", (line_index + 1).to_string())
}

fn config_key_error(key: &str, message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message).with_context("key", key.to_owned())
}

fn config_value_error(key: &str, value: &str, message: impl Into<String>) -> TreeSpaceError {
    config_key_error(
        key,
        format!("{} (key `{key}`, value `{value}`)", message.into()),
    )
}

/// The generated runtime table (L3 output of the configuration generator, 01
/// §2): one library-level tree entry plus a leaf-level entry per bucket block.
///
/// It records each unit's runtime view — holder, source, access form, and the
/// (non-persistent) invalidation state — exactly the schema of 01 §2 with the
/// tree's library-level ownership marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTable {
    tree: RuntimeEntry,
    blocks: BTreeMap<RefId, RuntimeEntry>,
}

impl Default for RuntimeTable {
    fn default() -> Self {
        Self {
            tree: RuntimeEntry::default(),
            blocks: BTreeMap::new(),
        }
    }
}

impl RuntimeTable {
    /// L3 generation: builds the initial runtime table from an effective
    /// configuration — a tree entry following the config plus an empty block
    /// table (blocks materialize at runtime, E-2).
    pub fn from_config(config: &RuntimeConfig) -> Self {
        Self {
            tree: RuntimeEntry {
                holder: Holder::Own,
                access: AccessMode::from(config.tree_access),
                source: None,
                invalid: InvalidState::Valid,
            },
            blocks: BTreeMap::new(),
        }
    }
    /// Returns the runtime view of the library-level tree.
    pub fn tree_entry(&self) -> &RuntimeEntry {
        &self.tree
    }
    /// Returns the runtime view of a bucket block, if present.
    pub fn block_entry(&self, id: RefId) -> Option<&RuntimeEntry> {
        self.blocks.get(&id)
    }
    /// Records the runtime view of a bucket block (inserting or replacing).
    ///
    /// Runtime updates come from the E-2 mapping/copy machinery and the E-3
    /// synchronization; E-1 only provides the schema entry point.
    pub fn set_block_entry(&mut self, id: RefId, entry: RuntimeEntry) {
        self.blocks.insert(id, entry);
    }
}

/// Runtime view of a single exchange unit (01 §2 schema).
///
/// The default is the conservative "fully owned" entry — my own data, full
/// read, no external source, valid — used as the L3 initial tree view and for
/// locally created blocks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEntry {
    /// Holder: I (owner, only writer) or mapped from a peer (01 §3).
    pub holder: Holder,
    /// The effective read access form (01 §4).
    pub access: AccessMode,
    /// Where the data comes from, when not locally owned (01 §2 `来源`).
    pub source: Option<Source>,
    /// Invalidation state; a runtime event, never persisted (01 §2, §7).
    pub invalid: InvalidState,
}

impl Default for RuntimeEntry {
    fn default() -> Self {
        Self {
            holder: Holder::Own,
            access: AccessMode::Full,
            source: None,
            invalid: InvalidState::Valid,
        }
    }
}

impl RuntimeEntry {
    /// True iff this entry is `Holder::Own` (the only writer, 01 §3.1).
    pub fn is_owner(&self) -> bool {
        self.holder.is_owner()
    }
}

impl RuntimeTable {
    /// Records the runtime view of the library-level tree (inserting/replacing).
    ///
    /// E-1 only exposed the getter; E-2 needs the setter to mark the tree as
    /// `Mapped(..)` or to overlay runtime invalidation events (01 §2, §7).
    pub fn set_tree_entry(&mut self, entry: RuntimeEntry) {
        self.tree = entry;
    }

    /// Overlays a runtime invalidation event on `key`'s entry (01 §7). The
    /// entry must already exist; an absent block entry is
    /// `Err(DanglingReference)` ("the table has no such block", `02-施工路线图.md`
    /// §6.6).
    pub fn mark_invalid(&mut self, key: ExchangeKey, state: InvalidState) -> Result<()> {
        match key {
            ExchangeKey::Tree => {
                self.tree.invalid = state;
                Ok(())
            }
            ExchangeKey::Block(id) => {
                let entry = self.blocks.get_mut(&id).ok_or_else(|| {
                    TreeSpaceError::new(
                        ErrorCode::DanglingReference,
                        "runtime table has no entry for this block",
                    )
                    .with_context("ref_id", id.to_string())
                })?;
                entry.invalid = state;
                Ok(())
            }
        }
    }
}
