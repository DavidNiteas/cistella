//! Read access forms (protocol `交换协议/01-目标与设计.md` §4): mapped
//! (zero-copy, invalidatable), full (owned copy), lazy (tree up front, blocks
//! on demand). Access forms concern **reads** only; writes are bound to the
//! holder (01 §3).
//!
//! E-2 lands the behavior over the frozen E-1 types (`02-施工路线图.md` §6):
//! `MappedView` reuses the [`crate::exchange::translator::MappedObject`]
//! zero-copy base, `OwnedSnapshot` and `LazySnapshot` copy through the owning
//! library's loaded state. The tree is only ever `Mapped`/`Full` — lazy is a
//! library-level block deferral only (`TreeAccess` has no `Lazy` variant, 01
//! §4).
//!
//! Invalidation (01 §7) is exposed at the view level (`MappedView::invalid_state`)
//! and at the runtime-table level (`RuntimeTable::mark_invalid`). E-4 lands
//! the `Source::Disk` branch as a real disk mapping of the committed tree blob
//! (02 §8.2-3): `refresh()` re-probes + re-maps and exposes `UpstreamChanged`
//! (tree-blob address moved) / `UpstreamDead` (file gone) for real (02 §8.2-4).

use crate::block::{Envelope, RefId};
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::translator::{
    MappedObject, committed_tree_blob_address, detect_disk_layout, map_tree_disk,
};
use crate::exchange::{AccessMode, InvalidState, RuntimeTable, Source};
use crate::ids::Digest;
use crate::layout::{FlatDirLayout, SingleFileLayout, StorageKind, TbLayout};
use crate::shared::SharedRegion;
use crate::tb_library::TbLibrary;
use crate::tree::codec::{DecodeTree, TreeImage};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A zero-copy mapped view of a single exchange unit (tree or block), 01 §4
/// Mapped. The reader does **not** own the data: reads are live against the
/// mapped backing (an IPC shared region or a disk mmap) and may invalidate
/// (01 §7). `Send + Sync` is preserved by guarding the invalidation flag and
/// the mapped identity with `Mutex`es.
pub struct MappedView {
    object: MappedObject, // zero-copy backing: IPC(SharedRegion) | File(memmap2::Mmap)
    source: Source,       // kept for a later refresh() re-open
    invalid: Mutex<InvalidState>, // runtime event; never persisted (01 §2)
    mapped_id: Mutex<Option<Digest>>, // disk-mapped object identity (tree blob address)
}

impl MappedView {
    /// Opens a mapped view over a peer's shared region (`Source::Ipc`, 01 §4)
    /// or, in E-4, over a local library's committed tree blob (`Source::Disk`):
    /// the path is probed with [`detect_disk_layout`] (directory → `FlatDir`,
    /// `TSDB` container → `SingleFile`; anything else →
    /// `Err(MappingAmbiguous)`) and the committed tree blob is mapped with
    /// [`map_tree_disk`]; the tree-blob address is recorded in `mapped_id` for
    /// the disk `refresh()` change detection (02 §8.2-3/§8.6).
    ///
    /// The original `source` is kept inside the view so a later
    /// [`refresh`](Self::refresh) can re-open the same region / path.
    pub fn open(source: Source) -> Result<Self> {
        match source {
            Source::Ipc(ipc) => {
                let source = Source::Ipc(ipc.clone());
                let region = SharedRegion::open(ipc.into_handle())?;
                Ok(Self {
                    object: MappedObject::from_ipc(region),
                    source,
                    invalid: Mutex::new(InvalidState::Valid),
                    mapped_id: Mutex::new(None),
                })
            }
            Source::Disk(path) => {
                let source = Source::Disk(path.clone());
                let storage_kind = detect_disk_layout(&path)?;
                let (object, tree_blob) = match storage_kind {
                    StorageKind::FlatDir => {
                        let layout = FlatDirLayout::new(path);
                        let object = map_tree_disk(&layout)?;
                        let tree_blob = committed_tree_blob_address(&layout)?;
                        (object, tree_blob)
                    }
                    StorageKind::SingleFile => {
                        let layout = SingleFileLayout::new(path);
                        let object = map_tree_disk(&layout)?;
                        let tree_blob = committed_tree_blob_address(&layout)?;
                        (object, tree_blob)
                    }
                    StorageKind::Memory => {
                        return Err(TreeSpaceError::new(
                            ErrorCode::MappingAmbiguous,
                            "a memory layout has no disk path to map",
                        ));
                    }
                };
                Ok(Self {
                    object,
                    source,
                    invalid: Mutex::new(InvalidState::Valid),
                    mapped_id: Mutex::new(Some(tree_blob)),
                })
            }
        }
    }

    /// Borrows the mapped canonical bytes live (tree bytes or block envelope
    /// bytes). `Err(RequiredDataMissing)` when `invalid == UpstreamDead`
    /// (the backing may hold no data, 01 §7 case 2). `UpstreamChanged` still
    /// reads (the upstream's current view), but consecutive reads are not
    /// guaranteed to agree (01 §7 case 1).
    pub fn bytes(&self) -> Result<&[u8]> {
        if self.invalid_state() == InvalidState::UpstreamDead {
            return Err(TreeSpaceError::new(
                ErrorCode::RequiredDataMissing,
                "the mapped view's upstream is dead; the mapped object may hold no data (01 §7)",
            ));
        }
        Ok(self.object.bytes())
    }

    /// Current invalidation state (01 §7 exposure, view level).
    pub fn invalid_state(&self) -> InvalidState {
        *self
            .invalid
            .lock()
            .expect("mapped-view invalidation lock poisoned")
    }

    /// Overlays a runtime invalidation event (01 §7); called when the upstream
    /// is observed to have changed or died.
    pub fn mark_invalid(&self, state: InvalidState) {
        *self
            .invalid
            .lock()
            .expect("mapped-view invalidation lock poisoned") = state;
    }

    /// Best-effort re-open from the recorded source. Re-open failure →
    /// `UpstreamDead`. A disk source additionally compares the re-mapped tree
    /// blob address against the recorded one: an address change →
    /// `UpstreamChanged` (the mapping follows the upstream's current view, 01
    /// §7 case 1); no change → `Valid`. IPC re-open behavior is unchanged
    /// (success → `Valid`). Returns the new state.
    pub fn refresh(&mut self) -> InvalidState {
        let reopened = match Self::open(self.source.clone()) {
            Ok(opened) => opened,
            Err(_) => {
                self.mark_invalid(InvalidState::UpstreamDead);
                return InvalidState::UpstreamDead;
            }
        };
        let disk_changed = match &self.source {
            Source::Disk(_) => {
                let previous = *self
                    .mapped_id
                    .lock()
                    .expect("mapped-view identity lock poisoned");
                let current = {
                    let guard = reopened
                        .mapped_id
                        .lock()
                        .expect("mapped-view identity lock poisoned");
                    *guard
                };
                previous != current
            }
            Source::Ipc(_) => false,
        };
        self.object = reopened.object;
        self.mapped_id = reopened.mapped_id;
        if disk_changed {
            self.mark_invalid(InvalidState::UpstreamChanged);
            InvalidState::UpstreamChanged
        } else {
            self.invalid = Mutex::new(InvalidState::Valid);
            InvalidState::Valid
        }
    }
}

/// A full owned snapshot: tree + bucket instant copy; ownership belongs to the
/// reader and never invalidates (01 §4 Full).
#[derive(Clone, Debug)]
pub struct OwnedSnapshot {
    /// The up-front tree image copy (owned by the reader).
    pub image: TreeImage,
    /// The up-front bucket copy (owned by the reader).
    pub bucket: Bucket,
}

impl PartialEq for OwnedSnapshot {
    /// Snapshot equality compares the tree image and the bucket envelope set.
    ///
    /// `Bucket` itself does not implement `PartialEq` (by design, it predates
    /// the exchange protocol); equality is derived here from the content
    /// addressed envelopes so that the E-2 "full snapshot equals the library
    /// state" assertion holds without touching the frozen bucket type.
    fn eq(&self, other: &Self) -> bool {
        self.image == other.image && buckets_equal(&self.bucket, &other.bucket)
    }
}

impl OwnedSnapshot {
    /// Full read: instant tree+bucket snapshot of a library (01 §4). The copy
    /// is owned by the caller; no invalidation applies (01 §7).
    pub fn read_full<L: TbLayout>(library: &TbLibrary<L>) -> Result<Self> {
        Ok(Self {
            image: library.tree_image()?.clone(),
            bucket: library.bucket()?.clone(),
        })
    }
    /// Projects the snapshot image into a typed tree (mirrors
    /// `TbLibrary::project`), materializing block leaves through the snapshot
    /// bucket.
    pub fn project<T: DecodeTree>(&self) -> Result<T> {
        crate::tree::codec::project::<T>(&self.image, &self.bucket)
    }
}

/// A lazy owned snapshot (01 §4 Lazy): the tree is copied up front; bucket
/// blocks are copied on demand from the owning library's bucket and cached by
/// the reader, which owns them. Only bucket blocks are lazy — the tree never
/// is (type level: `TreeAccess` has no `Lazy`).
///
/// `Send + Sync` is preserved by guarding the on-demand copy cache with a
/// `Mutex`.
pub struct LazySnapshot<'a> {
    image: TreeImage,
    source: &'a Bucket, // read-only block source (owned by the library)
    copied: Mutex<BTreeMap<RefId, Envelope>>, // reader-owned on-demand copies
}

impl<'a> LazySnapshot<'a> {
    /// Lazy read: copies the tree up front, defers bucket blocks (01 §4).
    /// Fails with `Err(RequiredDataMissing)` when the library has no loaded
    /// state.
    pub fn new<L: TbLayout>(library: &'a TbLibrary<L>) -> Result<Self> {
        Ok(Self {
            image: library.tree_image()?.clone(),
            source: library.bucket()?,
            copied: Mutex::new(BTreeMap::new()),
        })
    }
    /// The up-front tree copy (owned by the reader).
    pub fn image(&self) -> &TreeImage {
        &self.image
    }
    /// On-demand copy: returns the reader-owned envelope for `id`, copying it
    /// from the source on first access and caching it. Unknown id →
    /// `Err(DanglingReference)`.
    pub fn block(&self, id: RefId) -> Result<Envelope> {
        let mut copied = self
            .copied
            .lock()
            .expect("lazy-snapshot copy cache lock poisoned");
        if let Some(envelope) = copied.get(&id) {
            return Ok(envelope.clone());
        }
        let envelope = self.source.get(id).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "lazy snapshot source has no such block",
            )
            .with_context("ref_id", id.to_string())
        })?;
        let owned = envelope.clone();
        copied.insert(id, owned.clone());
        Ok(owned)
    }
    /// Whether `id` has already been copied into the reader's ownership.
    pub fn is_materialized(&self, id: RefId) -> bool {
        self.copied
            .lock()
            .expect("lazy-snapshot copy cache lock poisoned")
            .contains_key(&id)
    }
}

// ---- Tree access (TreeAccess: Mapped / Full only, 01 §4) ----

/// Tree mapped read (zero-copy, invalidatable); dispatches to
/// [`MappedView::open`] so the tree abides by the two read forms only.
pub fn open_tree_mapped(source: Source) -> Result<MappedView> {
    MappedView::open(source)
}

/// Tree full read (owned tree+bucket snapshot); dispatches to
/// [`OwnedSnapshot::read_full`].
pub fn read_tree_full<L: TbLayout>(library: &TbLibrary<L>) -> Result<OwnedSnapshot> {
    OwnedSnapshot::read_full(library)
}

// ---- Block access (AccessMode: Mapped / Full / Lazy, 01 §4) ----

/// Block mapped read (zero-copy, invalidatable; per-block independent, 01 §4).
pub fn open_block_mapped(source: Source) -> Result<MappedView> {
    MappedView::open(source)
}

/// Block full read: an owned copy of one bucket block (instant snapshot, 01
/// §4). Unknown id → `Err(DanglingReference)`.
pub fn read_block_full<L: TbLayout>(library: &TbLibrary<L>, id: RefId) -> Result<Envelope> {
    library.bucket()?.get(id).cloned().ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::DanglingReference,
            "block is absent from the library bucket",
        )
        .with_context("ref_id", id.to_string())
    })
}

// ---- library-level lazy (AccessMode::Lazy, 01 §4) ----

/// Lazy read: tree up front, blocks on demand. Lazy is library-level — there
/// is no per-block lazy entry point; lazy blocks are pulled via
/// [`LazySnapshot::block`]. Dispatches to [`LazySnapshot::new`].
pub fn read_lazy<L: TbLayout>(library: &TbLibrary<L>) -> Result<LazySnapshot<'_>> {
    LazySnapshot::new(library)
}

// ---- Runtime-table dispatch (honors the recorded access form, 01 §4) ----

/// Result of a tree read dispatched by the runtime table.
pub enum TreeRead {
    /// Zero-copy mapped tree (read live, invalidatable, 01 §4).
    Mapped(MappedView),
    /// Owned full tree+bucket snapshot (01 §4).
    Full(OwnedSnapshot),
}

/// Result of a block read dispatched by the runtime table.
pub enum BlockRead {
    /// Zero-copy mapped block (read live, invalidatable, 01 §4).
    Mapped(MappedView),
    /// Owned full copy of one block envelope (01 §4).
    Full(Envelope),
}

/// Tree read dispatched on the table's recorded access form (Mapped/Full
/// only, 01 §4).
///
/// - `AccessMode::Mapped`: the tree entry's recorded `source` is mapped
///   (`Err(RequiredDataMissing)` if the entry records no source);
/// - `AccessMode::Full`: a full owned snapshot is taken from `library`;
/// - `AccessMode::Lazy`: unreachable at the type level (`TreeAccess` has no
///   `Lazy`); a hand-built entry carrying `Lazy` is rejected as
///   `Err(PayloadMalformed)`.
pub fn read_tree<L: TbLayout>(library: &TbLibrary<L>, table: &RuntimeTable) -> Result<TreeRead> {
    let entry = table.tree_entry();
    match entry.access {
        AccessMode::Mapped => {
            let source = entry.source.clone().ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::RequiredDataMissing,
                    "mapped tree access requires the tree entry to record a source",
                )
            })?;
            Ok(TreeRead::Mapped(MappedView::open(source)?))
        }
        AccessMode::Full => Ok(TreeRead::Full(OwnedSnapshot::read_full(library)?)),
        AccessMode::Lazy => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "the runtime table encodes a lazy tree access; the tree can only be Mapped or Full (01 §4)",
        )),
    }
}

/// Block read dispatched on the table's recorded `AccessMode` (Mapped/Full).
/// `Lazy` is not a per-block read — lazy blocks go through
/// `read_lazy()?.block(id)` (01 §4); a hand-built entry carrying `Lazy` on a
/// per-block dispatch is reported as `Err(PayloadMalformed)`, the same
/// unreachable-branch code as the lazy tree (`Lazy` ≠ missing data).
///
/// An absent block entry is `Err(DanglingReference)` ("the table has no such
/// block").
pub fn read_block<L: TbLayout>(
    library: &TbLibrary<L>,
    table: &RuntimeTable,
    id: RefId,
) -> Result<BlockRead> {
    let entry = table.block_entry(id).ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::DanglingReference,
            "runtime table has no entry for this block",
        )
        .with_context("ref_id", id.to_string())
    })?;
    match entry.access {
        AccessMode::Mapped => {
            let source = entry.source.clone().ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::RequiredDataMissing,
                    "mapped block access requires the block entry to record a source",
                )
            })?;
            Ok(BlockRead::Mapped(MappedView::open(source)?))
        }
        AccessMode::Full => Ok(BlockRead::Full(read_block_full(library, id)?)),
        AccessMode::Lazy => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "lazy blocks are read through read_lazy()?.block(id), not per-block dispatch (01 §4)",
        )),
    }
}

/// Whether two buckets hold the identical envelope set, keyed by `RefId`.
fn buckets_equal(left: &Bucket, right: &Bucket) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ids().all(|id| left.get(id) == right.get(id))
}
