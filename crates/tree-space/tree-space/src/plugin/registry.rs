//! Process-wide plugin registry (`01-目标与设计.md` §4-1): registration,
//! companion-pair conflict checking, disk/kind routing, and the M2
//! in-memory conversion hook.
//!
//! Mirrors the process-level pattern of `block::registry`: a process-global
//! [`PluginRegistry::global`] serves the routing consumers (the boot chain of
//! `TbLibrary` in later PL-1 stages), while tests and custom compositions can
//! build their own registries through [`PluginRegistry::new`] /
//! [`PluginRegistry::empty`].
//!
//! Conflict semantics (01 §4-1 / §2-2): a second block plugin — or second tree
//! plugin — for the same `(MEMORY, DISK)` pair is rejected with
//! [`ErrorCode::TypeConflict`]; [`PluginRegistry::validate_pair`] returns
//! `Err(SchemaMismatch)` for a pair no plugin declares (an unreachable
//! combination that must not be labeled). The built-in pairs
//! (`Arrow55 ↔ ArrowIpc`, `Arrow55 ↔ ArrowParquet`) are auto-registered by
//! `new()` and are therefore always routable.
//!
//! The in-memory converter hook (`01 §4-1 [M2]` / §2-3) registers "memory →
//! memory" layout converters; PL-2 lands the interface plus the built-in
//! same-version identity converter only (the Polars converter entity is a
//! separate work order, 01 §3-2).

use crate::block::{BlockKind, Envelope};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::plugin::block::{ArrowIpcBlockPlugin, ArrowParquetBlockPlugin, BlockPlugin};
use crate::plugin::tree::{Arrow55TreePlugin, TreePlugin};
use crate::plugin::version::{DiskLayout, MemLayout};
use crate::tree::codec::TreeImage;
use std::sync::{Arc, LazyLock};

/// An in-memory layout converter (`01 §2-3 / §3-2`): converts blocks and trees
/// between memory layout versions at runtime — I/O and conversion are separate
/// links; conversion is memory → memory after direct reads.
///
/// PL-2 (M2) lands the interface plus the built-in same-version identity
/// converter (`converter(Arrow55, Arrow55)` is always available); the Polars
/// converter entities belong to the Polars work order.
pub trait MemoryConverter: Send + Sync {
    /// The source memory layout.
    fn from_layout(&self) -> MemLayout;
    /// The target memory layout.
    fn to_layout(&self) -> MemLayout;
    /// Converts a tree image in memory (M2: identity).
    fn convert_tree(&self, image: &TreeImage) -> Result<TreeImage>;
    /// Converts an in-memory block envelope (M2: identity).
    fn convert_block(&self, envelope: &Envelope) -> Result<Envelope>;
}

/// The built-in same-version identity converter (M2).
///
/// `convert_tree` / `convert_block` return their input unchanged; the
/// conversion link for the pair `(mem, mem)` is therefore always available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityMemConverter {
    mem: MemLayout,
}

impl IdentityMemConverter {
    /// Builds the identity converter for one memory layout.
    pub fn for_layout(mem: MemLayout) -> Self {
        Self { mem }
    }
}

impl MemoryConverter for IdentityMemConverter {
    fn from_layout(&self) -> MemLayout {
        self.mem
    }
    fn to_layout(&self) -> MemLayout {
        self.mem
    }
    fn convert_tree(&self, image: &TreeImage) -> Result<TreeImage> {
        Ok(image.clone())
    }
    fn convert_block(&self, envelope: &Envelope) -> Result<Envelope> {
        Ok(envelope.clone())
    }
}

/// The plugin registry: registered block/tree plugins, memory converters, and
/// routing.
pub struct PluginRegistry {
    blocks: Vec<Arc<dyn BlockPlugin>>,
    trees: Vec<Arc<dyn TreePlugin>>,
    converters: Vec<Arc<dyn MemoryConverter>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    /// Creates a registry with the built-in plugins auto-registered:
    /// `ArrowIpcBlockPlugin` + `ArrowParquetBlockPlugin` on the block side,
    /// `Arrow55TreePlugin` on the tree side, and the same-version identity
    /// memory converter (`Arrow55 → Arrow55`, M2).
    pub fn new() -> Self {
        let mut registry = Self {
            blocks: Vec::new(),
            trees: Vec::new(),
            converters: Vec::new(),
        };
        registry
            .register(Arc::new(ArrowIpcBlockPlugin))
            .expect("built-in block plugin pairs conflict only on duplicate registration");
        registry
            .register(Arc::new(ArrowParquetBlockPlugin))
            .expect("built-in block plugin pairs conflict only on duplicate registration");
        registry
            .register_tree(Arc::new(Arrow55TreePlugin))
            .expect("built-in tree plugin pair conflicts only on duplicate registration");
        registry
            .register_converter(Arc::new(IdentityMemConverter::for_layout(
                MemLayout::Arrow55,
            )))
            .expect("the identity converter is the only Arrow55 → Arrow55 converter");
        registry
    }

    /// Creates an empty registry without the built-in plugins.
    ///
    /// Intended for tests (an unpaired combination must fail
    /// [`Self::validate_pair`]) and fully custom compositions; `new()` remains
    /// the built-in-complete construction.
    pub fn empty() -> Self {
        Self {
            blocks: Vec::new(),
            trees: Vec::new(),
            converters: Vec::new(),
        }
    }

    /// Registers a block plugin, rejecting a second plugin for the same
    /// `(MEMORY, DISK)` pair with [`ErrorCode::TypeConflict`].
    pub fn register(&mut self, plugin: Arc<dyn BlockPlugin>) -> Result<()> {
        if self.blocks.iter().any(|existing| {
            existing.memory() == plugin.memory() && existing.disk() == plugin.disk()
        }) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "a block plugin for the same (MEMORY, DISK) pair is already registered",
            ));
        }
        self.blocks.push(plugin);
        Ok(())
    }

    /// Registers a tree plugin, rejecting a second plugin for the same
    /// `(MEMORY, DISK)` pair with [`ErrorCode::TypeConflict`].
    pub fn register_tree(&mut self, plugin: Arc<dyn TreePlugin>) -> Result<()> {
        if self.trees.iter().any(|existing| {
            existing.memory() == plugin.memory() && existing.disk() == plugin.disk()
        }) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "a tree plugin for the same (MEMORY, DISK) pair is already registered",
            ));
        }
        self.trees.push(plugin);
        Ok(())
    }

    /// Companion-pair validation: the `(mem, disk)` combination is routable
    /// when at least one block or tree plugin declares it.
    ///
    /// At-most-one per side is enforced at registration time; a pair with no
    /// declaration at all (an unreachable combination) is rejected with
    /// [`ErrorCode::SchemaMismatch`] so it cannot be labeled (01 §4-1 / §2-2).
    /// The built-in pairs are auto-registered, so `validate_pair(Arrow55,
    /// ArrowIpc)` and `validate_pair(Arrow55, ArrowParquet)` always pass on
    /// [`PluginRegistry::new`].
    pub fn validate_pair(&self, mem: MemLayout, disk: DiskLayout) -> Result<()> {
        let block = self
            .blocks
            .iter()
            .any(|plugin| plugin.memory() == mem && plugin.disk() == disk);
        let tree = self
            .trees
            .iter()
            .any(|plugin| plugin.memory() == mem && plugin.disk() == disk);
        if block || tree {
            Ok(())
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "no plugin declares the requested (MEMORY, DISK) layout pair",
            ))
        }
    }

    /// Read routing: finds the block plugin serving a disk version and kind.
    ///
    /// A miss (`None`) is the degraded-block candidate: the object cannot be
    /// decoded by the current plugin set (unknown version or out-of-kind).
    pub fn route_disk(&self, disk: DiskLayout, kind: &BlockKind) -> Option<&dyn BlockPlugin> {
        self.blocks
            .iter()
            .find(|plugin| plugin.disk() == disk && plugin.matches(kind))
            .map(|plugin| plugin.as_ref())
    }

    /// Routes a tree plugin by its disk layout version.
    ///
    /// A miss (`None`) is a hard bootstrap failure in the boot chain (the tree
    /// version is unknown, so tree structure cannot be parsed at all).
    pub fn tree_plugin(&self, disk: DiskLayout) -> Option<&dyn TreePlugin> {
        self.trees
            .iter()
            .find(|plugin| plugin.disk() == disk)
            .map(|plugin| plugin.as_ref())
    }

    /// Registers an in-memory converter, rejecting a second converter for the
    /// same `(from, to)` memory-layout pair with [`ErrorCode::TypeConflict`].
    pub fn register_converter(&mut self, converter: Arc<dyn MemoryConverter>) -> Result<()> {
        if self.converters.iter().any(|existing| {
            existing.from_layout() == converter.from_layout()
                && existing.to_layout() == converter.to_layout()
        }) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "a memory converter for the same (from, to) layout pair is already registered",
            ));
        }
        self.converters.push(converter);
        Ok(())
    }

    /// The directed in-memory conversion hook (`01 §4-1 [M2]`): finds the
    /// converter serving the `(from, to)` memory-layout pair.
    ///
    /// PL-2 registers only the same-version identity converter, so
    /// `converter(Arrow55, Arrow55)` is always available and every other
    /// combination is `None` (the Polars converters land in the Polars work
    /// order).
    pub fn converter(&self, from: MemLayout, to: MemLayout) -> Option<&dyn MemoryConverter> {
        self.converters
            .iter()
            .find(|converter| converter.from_layout() == from && converter.to_layout() == to)
            .map(|converter| converter.as_ref())
    }

    /// The process-global singleton registry (shared by `tb_library` and
    /// tests; built-in plugins auto-registered).
    pub fn global() -> &'static PluginRegistry {
        static GLOBAL: LazyLock<PluginRegistry> = LazyLock::new(PluginRegistry::new);
        &GLOBAL
    }
}
