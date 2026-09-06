//! Exchange-unit abstraction (protocol `交换协议/01-目标与设计.md` §1).
//!
//! The only core abstraction of the exchange protocol is the **block** ("data
//! block"); the tree is a *special block* with extra constraints, not a
//! sibling object kind (01 §1). This module defines the exchange-unit identity
//! ([`ExchangeKey`]), the holding/ownership envelope ([`Holder`] +
//! [`Source`]), the read access forms ([`AccessMode`] / [`TreeAccess`], 01 §4)
//! and the zero-copy invalidation states (01 §7).
//!
//! E-1 only lands the **types + schema**: none of the mapped/full/lazy read
//! behavior (E-2), synchronization or merge (E-3), or layout translators (E-4)
//! is implemented here.
//!
//! E-2 adds the permission / access-form / proxy-write **behavior** on top of
//! the frozen E-1 types (`02-施工路线图.md` §6): [`ownership`] (single-writer
//! gate), [`access`] (mapped / full / lazy reads) and [`proxy`] (async "sudo"
//! writes + IPC transport framing).
//!
//! E-3 adds synchronization on top of the frozen E-1/E-2 types (§7): [`merge`]
//! (sync scope + merge primitives + reachable-block enumeration, pure
//! functions) and [`sync`] (pull / push entries + fetch helpers over the E-2
//! read and proxy surfaces). No E-1/E-2 signature is altered.

use crate::block::RefId;
use crate::shared::RegionHandle;
use std::path::PathBuf;

/// Identity of an exchange unit (01 §1): the library-level tree or a
/// leaf-level bucket block.
///
/// The tree is a *special block*: it is IO'd whole, is never splittable, and
/// is never lazy (01 §1, §4). It has no payload of its own — the tree identity
/// is resolved at runtime from the loaded state (`LoadedState`), so this enum
/// carries no `TreeId` (protocol `02-施工路线图.md` §5.6).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ExchangeKey {
    /// The single library-level tree. Whole-tree IO only (不可拆分/不可惰性, 01 §1).
    Tree,
    /// A bucket block, content-addressed and leaf-level owned (01 §1).
    Block(RefId),
}

/// The holding party of an exchange unit (01 §3.1, grammar of the runtime
/// table 01 §2): I (the owner, the only writer) or a zero-copy mapping from a
/// peer.
///
/// `Own` is the only write-capable holder; `Mapped(..)` readers may invalidate
/// when the upstream changes or dies (01 §7).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Holder {
    /// I own the data (owner = the only writer, 01 §3.1).
    Own,
    /// I read a zero-copy mapping of data owned by a peer (`Source`).
    Mapped(Source),
}

impl Holder {
    /// True iff this holder is the owner — the only write-capable party
    /// (01 §3.1).
    pub fn is_owner(&self) -> bool {
        matches!(self, Holder::Own)
    }
    /// True iff this holder is a zero-copy mapping of a peer (read-only,
    /// 01 §3.1/§7).
    pub fn is_mapped(&self) -> bool {
        matches!(self, Holder::Mapped(_))
    }
    /// The mapped source, when mapped.
    pub fn source(&self) -> Option<&Source> {
        match self {
            Holder::Mapped(source) => Some(source),
            Holder::Own => None,
        }
    }
}

/// Where data comes from (01 §2 runtime-table `来源`). This work order only
/// covers IPC + disk; RPC is deliberately out of scope (01 §8) and therefore
/// has **no** variant here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// A local library on disk.
    Disk(PathBuf),
    /// A cross-process shared region (01 §8 scope: same-machine IPC only).
    Ipc(IpcSource),
}

/// Cross-process source handle, reusing the existing
/// [`RegionHandle`](crate::shared::RegionHandle) shape from `shared.rs`
/// (Windows named file mapping / Unix `memfd`).
///
/// This is a pure schema value: it records *where* the shared region lives,
/// not an active mapping (opening/reading is E-2). `RegionHandle` is `Clone` +
/// `Eq` on Windows and only `Debug` on Unix (owned-fd semantics), so the
/// `Clone`/`Eq`/`PartialEq` impls mirror the platform shape: derived on
/// Windows, duplicated-descriptor/value-based on Unix.
#[cfg_attr(windows, derive(Clone, Eq, PartialEq))]
#[derive(Debug)]
pub struct IpcSource {
    handle: RegionHandle,
}

impl IpcSource {
    /// Wraps a handle produced by [`SharedRegion::export`](crate::shared::SharedRegion::export).
    pub fn new(handle: RegionHandle) -> Self {
        Self { handle }
    }
    /// Borrows the wrapped cross-process handle.
    pub fn handle(&self) -> &RegionHandle {
        &self.handle
    }
    /// Consumes the source, returning the cross-process handle.
    pub fn into_handle(self) -> RegionHandle {
        self.handle
    }
}

#[cfg(unix)]
impl Clone for IpcSource {
    fn clone(&self) -> Self {
        use std::os::unix::io::{FromRawFd, OwnedFd};
        let duplicated = libc::dup(self.handle.fd());
        assert!(
            duplicated >= 0,
            "duplicating the shared-region descriptor failed"
        );
        Self {
            handle: RegionHandle::from_fd(
                unsafe { OwnedFd::from_raw_fd(duplicated) },
                self.handle.len(),
            ),
        }
    }
}

#[cfg(unix)]
impl PartialEq for IpcSource {
    fn eq(&self, other: &Self) -> bool {
        self.handle.fd() == other.handle.fd() && self.handle.len() == other.handle.len()
    }
}

#[cfg(unix)]
impl Eq for IpcSource {}

/// Read access form of a bucket block (01 §4). Access forms concern **reads**
/// only; writes are bound to the holder (01 §3).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AccessMode {
    /// Zero-copy mapping: data is not held, read live on access, and may be
    /// invalidated by the upstream (01 §4, §7).
    Mapped,
    /// Full copy: a complete snapshot is produced at access time; the copy is
    /// owned by the reader and never invalidates (01 §4).
    Full,
    /// Lazy copy: the tree is synchronized up front, bucket blocks are copied
    /// on demand; the new data is owned by the reader (01 §4).
    Lazy,
}

/// Read access form of the tree (01 §4): only **Mapped** or **Full**.
///
/// **The `Lazy` variant is deliberately excluded at the type level** — a lazy
/// tree is a lazy library, which is a library-level semantic outside the
/// crate (01 §4). This is the compile-time expression of "the tree cannot be
/// lazy".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TreeAccess {
    /// Zero-copy mapping of a peer library's tree (01 §4).
    Mapped,
    /// Full read: the tree is loaded whole and the copy is owned by the reader
    /// (01 §4).
    Full,
}

impl From<TreeAccess> for AccessMode {
    fn from(access: TreeAccess) -> Self {
        match access {
            TreeAccess::Mapped => AccessMode::Mapped,
            TreeAccess::Full => AccessMode::Full,
        }
    }
}

/// Invalidation state of a zero-copy mapping (01 §7). There are exactly two
/// failure modes — upstream changed and upstream dead — plus the valid state.
/// Full/lazy copies never invalidate because copy ownership belongs to the
/// reader (01 §7).
///
/// Runtime events such as this state are overlaid at runtime and are **not**
/// persisted (01 §2 partial persistence).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum InvalidState {
    /// The mapped view is current; reads are consistent snapshot-style.
    Valid,
    /// The upstream has changed since the last read; consecutive reads are not
    /// guaranteed to agree (01 §7 case 1).
    UpstreamChanged,
    /// The upstream has died (or its timeout expired for proxy writes); the
    /// region may already hold no data (01 §7 case 2).
    UpstreamDead,
}

pub mod config;

// E-2 additions (protocol `02-施工路线图.md` §6): ownership gate, read access
// forms and proxy writes. Each is a behavior layer over the frozen E-1 types.
pub mod access;
pub mod ownership;
pub mod proxy;

// E-3 additions (protocol `02-施工路线图.md` §7): synchronization — merge
// primitives + scope + reachable-block enumeration (`merge`, pure) and the
// pull/push entries + fetch helpers (`sync`). All additions; frozen E-1/E-2
// types are untouched.
pub mod merge;
pub mod sync;

// E-4 addition (protocol `02-施工路线图.md` §8): the layout-translator surface
// — named primitives over `TbLayout`, disk zero-copy mapping, cross-process
// IPC fetch, single-block write and the lazy IPC block pull. All additions;
// frozen types untouched.
pub mod translator;

pub use config::{IndexPolicySetting, IoBlobSetting, RuntimeConfig, RuntimeEntry, RuntimeTable};
