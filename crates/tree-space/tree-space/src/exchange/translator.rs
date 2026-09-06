//! Layout-translator adapter (protocol `交换协议/01-目标与设计.md` §6 + §7).
//!
//! E-4 lays the seven synchronization primitives of 01 §6 onto the two disk
//! layouts (**read / atomic read / write / atomic write / delete / GC /
//! reachable enumeration**), adds the disk zero-copy mapped read (01 §7 base)
//! that E-2 deferred, the cross-process IPC fetch that E-3 deferred, the
//! single-block write (`persist_block`) and the lazy cross-process block pull
//! (`LazyIpcSnapshot`) — the four E-trail items pushed to E-4 by
//! `02-施工路线图.md` §8.
//!
//! No parallel trait is introduced: `TbLayout` already *is* the layout
//! translator (`FlatDirLayout` / `SingleFileLayout` are one translator
//! implementation each), so this module exposes a named primitives surface
//! ([`LayoutTranslator`]) over the same polymorphic `L: TbLayout` bound, plus
//! the disk-mmap / IPC items that have no `TbLayout` slot. `TbLayout` /
//! `TbLibrary` / `SharedRegion` / `BlockMaterializer` and every frozen E-1/E-2/
//! E-3 public type stay untouched (02 §8.2).

use crate::block::{Envelope, RefId, block_ref_id};
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::Source;
use crate::exchange::access::OwnedSnapshot;
use crate::exchange::merge::{SyncScope, reachable_refs};
use crate::ids::Digest;
use crate::layout::materializer::select_materializer;
use crate::layout::tb::{
    RefRow, decode_ref_table, derive_ref_rows, encode_ref_table, image_leaf_refs,
};
use crate::layout::{GcReport, GcRequest, StorageKind, TbLayout};
use crate::shared::{RegionHandle, SharedRegion};
use crate::tree::codec::{TreeImage, decode, encode};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The frozen single-file container header magic (`TSDB\0\0\0\0`, see
/// `layout/single_file.rs::HEADER_MAGIC`). Duplicated here so
/// [`detect_disk_layout`] can probe a container without touching the layout
/// module's private constants.
const SINGLE_FILE_MAGIC: [u8; 8] = *b"TSDB\0\0\0\0";

/// The three TB object channels (corresponding to `TbLayout`'s object
/// channels). The commit object channel is deliberately absent: commits are
/// the visibility layer, never disk-mapped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TbChannel {
    /// A bucket block envelope object (`tb-blocks/`).
    Block,
    /// A canonical tree blob (`tb-trees/`).
    Tree,
    /// A three-column reference table (`tb-refs/`).
    Ref,
}

/// Resolves a `(channel, address)` pair into the `(file, offset, length)`
/// triple that backs a disk zero-copy mapping (crate-internal, 02 §8.2-2).
///
/// Flat = the object file itself (offset 0, length = file length); single-file
/// = the `.umdb` container plus the object-directory offset. `None` = the
/// object does not exist. `MemoryLayout` implements nothing (no disk).
pub(crate) trait ObjectSpan {
    /// The object's `(file, offset, length)` triple, or `None` when the object
    /// is absent from the layout.
    fn object_span(
        &self,
        channel: TbChannel,
        address: Digest,
    ) -> Result<Option<(PathBuf, u64, u64)>>;
}

/// Zero-copy byte backing: an IPC shared region or a disk mmap (01 §7 base).
pub enum MappedBacking {
    /// A cross-process shared region (01 §8 scope: same-machine IPC only).
    Ipc(SharedRegion),
    /// A disk file mmap (the 01 §7 zero-copy mapped read's disk backing).
    File(memmap2::Mmap),
}

/// A zero-copy mapped object (backing + byte range).
///
/// [`bytes`](Self::bytes) re-slices on every call, so the type holds the
/// backing and a `Range` instead of a self-referential slice. `Send + Sync`
/// — `SharedRegion` is `Arc`-backed and `memmap2::Mmap` is `Send + Sync`.
pub struct MappedObject {
    backing: MappedBacking,
    range: Range<usize>,
}

impl MappedObject {
    /// The zero-copy byte slice of the mapped object (no copy).
    pub fn bytes(&self) -> &[u8] {
        match &self.backing {
            MappedBacking::Ipc(region) => &region.bytes()[self.range.clone()],
            MappedBacking::File(mapping) => &mapping.as_ref()[self.range.clone()],
        }
    }
    /// Whether the backing is a disk mmap (IPC is always `false`).
    pub fn is_file_mapped(&self) -> bool {
        matches!(self.backing, MappedBacking::File(_))
    }
    /// Builds an IPC-backed object covering the whole region bytes.
    pub fn from_ipc(region: SharedRegion) -> Self {
        let len = region.bytes().len();
        Self {
            backing: MappedBacking::Ipc(region),
            range: 0..len,
        }
    }
}

/// The layout-translator adapter: maps a `TbLayout` onto the named primitives
/// surface of 01 §6. Zero-sized, not a trait — the physical differences are
/// already absorbed by the `L: TbLayout` polymorphism.
pub struct LayoutTranslator<L: TbLayout> {
    layout: L,
}

impl<L: TbLayout> LayoutTranslator<L> {
    /// Wraps a layout under the named primitives surface.
    pub fn new(layout: L) -> Self {
        Self { layout }
    }

    // ---- read (整块读; flat = per-file read; single-file = offset read) ----

    /// Reads one block object whole (flat = per-file read).
    pub fn read_block(&self, address: Digest) -> Result<Vec<u8>> {
        self.layout.read_block_object(address)
    }
    /// Reads one canonical tree blob whole.
    pub fn read_tree(&self, address: Digest) -> Result<Vec<u8>> {
        self.layout.read_tree_object(address)
    }
    /// Reads one reference-table object whole.
    pub fn read_ref(&self, address: Digest) -> Result<Vec<u8>> {
        self.layout.read_ref_object(address)
    }

    // ---- atomic read (桶块单块一次读完整; tree unsupported at type level) ----

    /// Atomic read of one bucket block: blocks are immutable, so one read is
    /// already complete (01 §6 atomic-read row). The tree has no atomic-read
    /// entry point — it is always read whole (类型层无树拆分入口).
    pub fn atomic_read_block(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_block(address)
    }

    // ---- write (整块写, content-addressed `exists→skip`) ----

    /// Writes one block object at its content address (dedup: exists → skip).
    pub fn write_block(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.layout.write_block_object(address, bytes)
    }
    /// Writes one canonical tree blob at its address (dedup: exists → skip).
    pub fn write_tree(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.layout.write_tree_object(address, bytes)
    }
    /// Writes one reference-table object at its address (dedup: exists → skip).
    pub fn write_ref(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.layout.write_ref_object(address, bytes)
    }

    // ---- atomic write (flat = tmp+rename; single-file = append epoch) ----

    /// Atomic write of one block: identical to [`write_block`](Self::write_block)
    /// — the `TbLayout` writes are already atomic (flat tmp+rename /
    /// single-file append epoch).
    pub fn atomic_write_block(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.write_block(address, bytes)
    }

    // ---- delete: no standalone entry — GC carries it (01 §6) ----

    // ---- GC (flat = sweep orphans; single-file = compaction repack) ----

    /// Runs the layout GC, keeping the commit history inside the
    /// `keep_from_sequence` window and reclaiming orphaned objects.
    pub fn gc(&self, keep_from_sequence: u64) -> Result<GcReport> {
        self.layout.gc(GcRequest { keep_from_sequence })
    }

    // ---- reachable enumeration (reuses the object directory / ref table) ----

    /// Lists every stored block object `(address, bytes)` (the object
    /// directory scan).
    pub fn block_objects(&self) -> Result<Vec<(Digest, Vec<u8>)>> {
        self.layout.block_objects()
    }
    /// The reachable reference-table rows of the committed head (read head →
    /// commit → `tb.refs` → decode; single hop, tree bytes never decoded, the
    /// same path as GC).
    pub fn enumerate_reachable_blocks(&self) -> Result<Vec<RefRow>> {
        committed_ref_rows(&self.layout)
    }
}

/// The committed head's reference-table rows (single hop, no tree decode).
fn committed_ref_rows(layout: &impl TbLayout) -> Result<Vec<RefRow>> {
    let Some((head, _)) = layout.read_head()? else {
        return Ok(Vec::new());
    };
    let commit = layout.read_commit(&layout.commit_path(head))?;
    let Some(pointers) = commit.tb else {
        return Ok(Vec::new());
    };
    let ref_bytes = layout.read_ref_object(Digest::from_bytes(pointers.refs))?;
    decode_ref_table(&ref_bytes)
}

// ---------------------------------------------------------------------------
// Disk zero-copy mapping (01 §7 base)
// ---------------------------------------------------------------------------

/// Probes a disk path's layout kind: a directory → `FlatDir`; a file with the
/// `TSDB\0\0\0\0` header → `SingleFile`; anything else (missing path, non-TSDB
/// file) → `Err(MappingAmbiguous)`. `Memory` has no disk-path form and is
/// never probed.
pub fn detect_disk_layout(path: &Path) -> Result<StorageKind> {
    if path.is_dir() {
        return Ok(StorageKind::FlatDir);
    }
    if path.is_file() {
        if let Ok(mut file) = File::open(path) {
            let mut magic = [0_u8; 8];
            if file.read(&mut magic).unwrap_or(0) == 8 && magic == SINGLE_FILE_MAGIC {
                return Ok(StorageKind::SingleFile);
            }
        }
    }
    Err(TreeSpaceError::new(
        ErrorCode::MappingAmbiguous,
        "path is neither a flat-dir library directory nor a single-file TSDB container",
    )
    .with_context("path", path.display().to_string()))
}

/// The committed tree blob's storage address (the only object locatable by a
/// bare library path, 02 §8.6). Used by `MappedView::open(Source::Disk)` to
/// record the mapped identity for `refresh()` change detection.
pub(crate) fn committed_tree_blob_address(layout: &impl TbLayout) -> Result<Digest> {
    let Some((head, _)) = layout.read_head()? else {
        return Err(TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "disk-mapped library has no committed head",
        ));
    };
    let commit = layout.read_commit(&layout.commit_path(head))?;
    let pointers = commit.tb.ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "head commit is not a TB commit",
        )
    })?;
    Ok(Digest::from_bytes(pointers.tree_blob))
}

/// Disk zero-copy mapping: the committed tree blob (the one object locatable
/// by a bare library path, 02 §8.2-3/§8.6).
///
/// `ObjectSpan` is deliberately `pub(crate)` (02 §8.7-2), so the bound is
/// satisfied by the two disk layouts without exposing the seam.
#[allow(private_bounds)]
pub fn map_tree_disk<L: TbLayout + ObjectSpan>(layout: &L) -> Result<MappedObject> {
    let Some((head, _)) = layout.read_head()? else {
        return Err(TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "disk-mapped library has no committed head",
        ));
    };
    let commit = layout.read_commit(&layout.commit_path(head))?;
    let pointers = commit.tb.ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "head commit is not a TB commit",
        )
    })?;
    map_object(
        layout,
        TbChannel::Tree,
        Digest::from_bytes(pointers.tree_blob),
    )
}

/// Disk zero-copy mapping of a block object. `address` must come from a
/// reference-table row — the addressing iron rule of 01 §1 (no out-of-band
/// random address reads).
#[allow(private_bounds)]
pub fn map_block_disk<L: TbLayout + ObjectSpan>(
    layout: &L,
    address: Digest,
) -> Result<MappedObject> {
    map_object(layout, TbChannel::Block, address)
}

/// Disk zero-copy mapping of a reference-table object.
#[allow(private_bounds)]
pub fn map_ref_disk<L: TbLayout + ObjectSpan>(layout: &L, address: Digest) -> Result<MappedObject> {
    map_object(layout, TbChannel::Ref, address)
}

/// Resolves `(channel, address)` through [`ObjectSpan`] and mmaps the object
/// file, slicing the mapping at the object's `(offset, length)` range.
fn map_object<L: TbLayout + ObjectSpan>(
    layout: &L,
    channel: TbChannel,
    address: Digest,
) -> Result<MappedObject> {
    let Some((path, offset, length)) = layout.object_span(channel, address)? else {
        let code = match channel {
            TbChannel::Block => ErrorCode::DanglingReference,
            TbChannel::Tree | TbChannel::Ref => ErrorCode::StorageCorrupt,
        };
        return Err(
            TreeSpaceError::new(code, "mapped TB object is absent from the layout")
                .with_context("channel", channel_label(channel))
                .with_context("address", address.to_string()),
        );
    };
    let file = File::open(&path).map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::StorageCorrupt,
            "cannot open the object file for zero-copy mapping",
        )
        .with_context("path", path.display().to_string())
        .with_context("detail", error.to_string())
    })?;
    let mapping = unsafe { memmap2::Mmap::map(&file) }.map_err(|error| {
        TreeSpaceError::new(ErrorCode::StorageCorrupt, "cannot mmap the object file")
            .with_context("path", path.display().to_string())
            .with_context("detail", error.to_string())
    })?;
    let start = usize::try_from(offset).map_err(|_| span_error(&path, offset, length))?;
    let run = usize::try_from(length).map_err(|_| span_error(&path, offset, length))?;
    let end = start
        .checked_add(run)
        .ok_or_else(|| span_error(&path, offset, length))?;
    if end > mapping.len() {
        return Err(span_error(&path, offset, length));
    }
    Ok(MappedObject {
        backing: MappedBacking::File(mapping),
        range: start..end,
    })
}

fn span_error(path: &Path, offset: u64, length: u64) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::StorageCorrupt,
        "mapped object range exceeds the file length",
    )
    .with_context("path", path.display().to_string())
    .with_context("offset", offset.to_string())
    .with_context("length", length.to_string())
}

fn channel_label(channel: TbChannel) -> &'static str {
    match channel {
        TbChannel::Block => "block",
        TbChannel::Tree => "tree",
        TbChannel::Ref => "ref",
    }
}

// ---------------------------------------------------------------------------
// Single-block write (01 §6 write row: content-addressed, `exists→skip`)
// ---------------------------------------------------------------------------

/// Materializes and writes one block object, returning its physical storage
/// address (the physical content hash of the materialized bytes).
///
/// Reachability rule: a block becomes reachable only when a tree references it
/// through a later `commit` (01 §6 GC row — a block's lifetime is its tree
/// leaf reference). This entry does not manufacture orphan-persistent
/// semantics; `TbLibrary` stays untouched (02 §8.7 裁定 5).
pub fn persist_block(layout: &impl TbLayout, envelope: &Envelope) -> Result<Digest> {
    let materializer = select_materializer(layout.block_materialization(), &envelope.kind);
    let physical = materializer.encode(envelope)?;
    let address = materializer.address(&physical);
    layout.write_block_object(address, &physical)?;
    Ok(address)
}

// ---------------------------------------------------------------------------
// Cross-process IPC fetch (01 §5.2; the E-3-deferred item)
// ---------------------------------------------------------------------------
//
// Snapshot wire (reuses the `publish_request` `push_*`/`Reader` style — the
// helpers are self-contained here, `proxy.rs` stays untouched):
//
// ```text
// [tree_len u64 LE][tree bytes]
// [ref_table_len u64 LE][ref_table ipc]   = encode_ref_table(derive_ref_rows(image_leaf_refs, bucket))
// [block_count u64 LE]
//    per block: [ref_id 16 B][envelope_len u64 LE][envelope bytes]
// ```

/// Frames a whole snapshot into one shared region (tree bytes + reference
/// table + bucket envelope segment), returning the region and its export
/// handle (for the peer's [`receive_snapshot`]/[`fetch_snapshot_ipc`]).
pub fn publish_snapshot(snapshot: &OwnedSnapshot) -> Result<(SharedRegion, RegionHandle)> {
    let mut bytes = Vec::new();
    let tree_bytes = encode(&snapshot.image)?;
    push_bytes(&mut bytes, &tree_bytes);
    let leaf_refs = image_leaf_refs(&snapshot.image)?;
    let rows = derive_ref_rows(&leaf_refs, &snapshot.bucket)?;
    push_bytes(&mut bytes, &encode_ref_table(&rows)?);
    push_u64(&mut bytes, snapshot.bucket.len() as u64);
    for id in snapshot.bucket.ids() {
        let envelope = snapshot
            .bucket
            .get(id)
            .expect("bucket ids iterate stored envelopes");
        push_16(&mut bytes, id.as_bytes());
        push_bytes(&mut bytes, &envelope.encode());
    }
    let region = SharedRegion::publish(bytes)?;
    let handle = region.export()?;
    Ok((region, handle))
}

/// Decodes a snapshot region back into a full owned snapshot (tree + whole
/// bucket; per-block `Envelope::decode` + identity comparison). An envelope
/// whose recomputed identity does not match its `ref_id` → `Err(DigestMismatch)`.
pub fn receive_snapshot(region: &SharedRegion) -> Result<OwnedSnapshot> {
    let mut reader = Reader::new(region.bytes());
    let tree_bytes = reader.take_bytes()?;
    let image = decode(tree_bytes)?;
    let ref_bytes = reader.take_bytes()?;
    decode_ref_table(ref_bytes)?;
    let count = reader.take_u64()?;
    let mut bucket = Bucket::new();
    for _ in 0..count {
        let ref_id = RefId::from_bytes(reader.take_16()?);
        let envelope_bytes = reader.take_bytes()?;
        let envelope = Envelope::decode(envelope_bytes)?;
        let computed = block_ref_id(envelope.kind.clone(), &envelope.payload);
        if computed != ref_id {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "snapshot envelope recomputed identity does not match its ref_id",
            )
            .with_context("ref_id", ref_id.to_string()));
        }
        bucket.put_envelope(envelope)?;
    }
    reader.finish()?;
    Ok(OwnedSnapshot { image, bucket })
}

/// Cross-process IPC fetch: maps the tree segment → [`decode`] →
/// `image_leaf_refs` + [`reachable_refs`] (reachable enumeration) → per-block
/// slice + `Envelope::decode` (per-block map, 02 §8.2-5).
///
/// `scope` may be `FullTree`/`TreeFragment` only; `Blocks` →
/// `Err(PayloadMalformed)` (use [`fetch_blocks_ipc`]). Only `Source::Ipc` is
/// mapped here — a disk source goes through [`map_tree_disk`] (01 §1
/// addressing iron rule).
pub fn fetch_snapshot_ipc(source: Source, scope: &SyncScope) -> Result<OwnedSnapshot> {
    if matches!(scope, SyncScope::Blocks(_)) {
        return Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "Blocks is a bucket-level scope; use fetch_blocks_ipc, not fetch_snapshot_ipc",
        ));
    }
    let region = open_ipc_source(&source)?;
    let snapshot = receive_snapshot(&region)?;
    match scope {
        SyncScope::FullTree => Ok(snapshot),
        SyncScope::TreeFragment(_) => {
            let reachable = reachable_refs(&snapshot.image, scope)?;
            let mut bucket = Bucket::new();
            for id in reachable {
                let envelope = snapshot.bucket.get(id).ok_or_else(|| {
                    TreeSpaceError::new(
                        ErrorCode::DanglingReference,
                        "a reachable tree leaf has no envelope in the snapshot region",
                    )
                    .with_context("ref_id", id.to_string())
                })?;
                bucket.put_envelope(envelope.clone())?;
            }
            Ok(OwnedSnapshot {
                image: snapshot.image,
                bucket,
            })
        }
        SyncScope::Blocks(_) => unreachable!("guarded above"),
    }
}

/// Cross-process block-level fetch: takes the requested blocks from the
/// snapshot region (01 §5.2 桶的若干块). Unknown id → `Err(DanglingReference)`.
pub fn fetch_blocks_ipc(source: Source, ids: &[RefId]) -> Result<Bucket> {
    let region = open_ipc_source(&source)?;
    let snapshot = receive_snapshot(&region)?;
    let mut fetched = Bucket::new();
    for id in ids {
        let envelope = snapshot.bucket.get(*id).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "block is absent from the snapshot region",
            )
            .with_context("ref_id", id.to_string())
        })?;
        fetched.put_envelope(envelope.clone())?;
    }
    Ok(fetched)
}

/// Opens the shared region of an `IpcSource`; a disk source is rejected — the
/// IPC-fetch entries map shared regions only (disk goes through the disk mmap
/// entries).
fn open_ipc_source(source: &Source) -> Result<SharedRegion> {
    match source {
        Source::Ipc(ipc) => SharedRegion::open(ipc.clone().into_handle()),
        Source::Disk(_) => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "IPC fetch maps only Source::Ipc; a disk source goes through the disk-mmap entries",
        )),
    }
}

// ---------------------------------------------------------------------------
// Lazy cross-process block pull (01 §4 Lazy's IPC form; E-2 deferred)
// ---------------------------------------------------------------------------

/// A lazy IPC snapshot (01 §4 Lazy over IPC): the tree is decoded up front,
/// bucket blocks stay lazy. Holds the snapshot region plus a
/// `ref_id → Range` lazy index; [`block_bytes`](Self::block_bytes) slices each
/// block zero-copy, [`block`](Self::block) decodes on demand and caches. The
/// tree is never lazy (type level: `TreeAccess` has no `Lazy`, 01 §4).
pub struct LazyIpcSnapshot {
    image: TreeImage,
    region: SharedRegion,
    spans: BTreeMap<RefId, Range<usize>>,
    copied: Mutex<BTreeMap<RefId, Envelope>>,
}

impl LazyIpcSnapshot {
    /// Opens an IPC source, decodes the tree segment (tree up front) and builds
    /// the `ref_id → Range` lazy index without decoding any block.
    pub fn new(source: Source) -> Result<Self> {
        let region = open_ipc_source(&source)?;
        let bytes = region.bytes();
        let mut reader = Reader::new(bytes);
        let tree_bytes = reader.take_bytes()?;
        let image = decode(tree_bytes)?;
        let ref_bytes = reader.take_bytes()?;
        decode_ref_table(ref_bytes)?;
        let count = reader.take_u64()?;
        let mut spans = BTreeMap::new();
        for _ in 0..count {
            let ref_id = RefId::from_bytes(reader.take_16()?);
            let envelope_bytes = reader.take_bytes()?;
            let start = envelope_bytes.as_ptr() as usize - bytes.as_ptr() as usize;
            let end = start + envelope_bytes.len();
            spans.insert(ref_id, start..end);
        }
        reader.finish()?;
        Ok(Self {
            image,
            region,
            spans,
            copied: Mutex::new(BTreeMap::new()),
        })
    }
    /// The up-front tree copy (owned by the reader).
    pub fn image(&self) -> &TreeImage {
        &self.image
    }
    /// Per-block map: the zero-copy slice of one block's envelope bytes.
    /// Unknown id → `Err(DanglingReference)`.
    pub fn block_bytes(&self, id: RefId) -> Result<&[u8]> {
        let span = self.spans.get(&id).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "lazy IPC snapshot has no such block",
            )
            .with_context("ref_id", id.to_string())
        })?;
        let bytes = self.region.bytes();
        if span.end > bytes.len() {
            return Err(TreeSpaceError::new(
                ErrorCode::StorageCorrupt,
                "lazy IPC snapshot block span exceeds the region",
            ));
        }
        Ok(&bytes[span.start..span.end])
    }
    /// On-demand decode + cache: first access decodes and caches, later
    /// accesses hit the cache. The decoded envelope's recomputed identity must
    /// match `id` → otherwise `Err(DigestMismatch)`.
    pub fn block(&self, id: RefId) -> Result<Envelope> {
        let mut copied = self.copied.lock().expect("lazy-ipc cache lock poisoned");
        if let Some(envelope) = copied.get(&id) {
            return Ok(envelope.clone());
        }
        let envelope = Envelope::decode(self.block_bytes(id)?)?;
        let computed = block_ref_id(envelope.kind.clone(), &envelope.payload);
        if computed != id {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "lazy snapshot envelope recomputed identity does not match its ref_id",
            )
            .with_context("ref_id", id.to_string()));
        }
        copied.insert(id, envelope.clone());
        Ok(envelope)
    }
    /// Whether `id` has already been decoded into the reader's cache.
    pub fn is_materialized(&self, id: RefId) -> bool {
        self.copied
            .lock()
            .expect("lazy-ipc cache lock poisoned")
            .contains_key(&id)
    }
}

// ---------------------------------------------------------------------------
// Wire helpers (self-contained copy of the `publish_request` style)
// ---------------------------------------------------------------------------

/// A little-endian cursor over the snapshot wire form.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take_u64(&mut self) -> Result<u64> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 8)
            .ok_or_else(|| payload("truncated snapshot wire payload"))?;
        self.pos += 8;
        Ok(u64::from_le_bytes(slice.try_into().expect("8-byte slice")))
    }
    /// Takes a length-prefixed byte run: `[len u64 LE][bytes]`.
    fn take_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.take_u64()?;
        let len = usize::try_from(len)
            .map_err(|_| payload("snapshot wire length overflows the host usize"))?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| payload("snapshot wire length overflows the host usize"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| payload("truncated snapshot wire payload"))?;
        self.pos = end;
        Ok(slice)
    }
    /// Takes a fixed 16-byte run (`[ref_id 16 B]`).
    fn take_16(&mut self) -> Result<[u8; 16]> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 16)
            .ok_or_else(|| payload("truncated snapshot wire payload"))?;
        self.pos += 16;
        Ok(slice.try_into().expect("16-byte slice"))
    }
    fn finish(&self) -> Result<()> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(payload("trailing bytes in snapshot wire payload"))
        }
    }
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_u64(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
}

fn push_16(bytes: &mut Vec<u8>, value: [u8; 16]) {
    bytes.extend_from_slice(&value);
}

fn payload(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}
