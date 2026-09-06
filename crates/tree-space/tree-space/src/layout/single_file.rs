//! Append-only single-file container with 64-byte header and trailer scanning.
//!
//! The container is a sequence of 64-byte-aligned epochs. Every epoch appends a
//! payload plus a directory batch plus a trailer; reopening scans backward for
//! the newest structurally valid trailer (crash-safe recovery, see
//! [`find_latest`]). Two epoch kinds share the container:
//!
//! - v4 bootstrap epochs (from [`SingleFileLayout::create`] / `publish`) whose
//!   payload is a [`BootstrapImage`] IPC snapshot;
//! - TB tree-and-bucket epochs (P-IO-7.3, `_dev/树与桶管道/01-目标与设计.md`
//!   §1.11.6) whose payload is the **object-directory snapshot** — the complete
//!   catalog of every stored TB object at its absolute file offset. The head
//!   commit identity and a payload hash ride in the outer directory batch, so
//!   the epoch payload itself stays pure canonical Arrow IPC.
//!
//! A TB epoch therefore commits the whole reachable object catalog in one
//! append (每 commit 一 epoch): reopening reads the newest epoch's directory,
//! locates every commit/tree/ref/block object by offset, rebuilds the bucket and
//! the head commit. Deduplication consults the catalog before appending an
//! object (`exists → skip`, mirroring the flat layout's file-name dedup).
//! GC is a compaction repack: reachable objects inside the `keep_from_sequence`
//! window are rewritten into a fresh file that is renamed over the original — no
//! in-place per-object surgery (01 §1.11.5 single-file semantics). Block
//! materialization is forced to IPC (the container cannot map Parquet objects
//! zero-copy, 01 §1.11.3).

use super::{
    CommitNode, GcReport, GcRequest, Materialization, PublishPlan, PublishReceipt, StorageKind,
    StorageLayout, TableBytes, TableLocator, TablePayload, TbLayout,
};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::translator::{ObjectSpan, TbChannel};
use crate::fault::{FaultPlan, FaultPoint};
use crate::ids::Digest;
use crate::ipc::{decode_batch, encode_batch};
use crate::lock::FileLock;
use crate::manifest::BootstrapImage;
use crate::metadata::{encode_table_metadata, parse_table_metadata};
use arrow::array::{Array, ArrayRef, FixedSizeBinaryArray, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use xxhash_rust::xxh3::xxh3_64;

const HEADER_LEN: usize = 64;
const TRAILER_LEN: usize = 64;
const ALIGNMENT: u64 = 64;
const HEADER_MAGIC: &[u8; 8] = b"TSDB\0\0\0\0";
const TRAILER_MAGIC: &[u8; 8] = b"TSTRAIL\0";

/// The object kinds tracked by the TB object-directory snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum ObjectKind {
    /// A block envelope object.
    Block,
    /// A boot record object (fixed four-column IPC, 01 §4-3).
    Boot,
    /// A canonical tree blob.
    Tree,
    /// A three-column reference table.
    Ref,
    /// A per-commit version side-table (01 §3-1).
    Versions,
    /// A TB/nine-column commit object.
    Commit,
}
impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Boot => "boot",
            Self::Tree => "tree",
            Self::Ref => "ref",
            Self::Versions => "versions",
            Self::Commit => "commit",
        }
    }
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "block" => Some(Self::Block),
            "boot" => Some(Self::Boot),
            "tree" => Some(Self::Tree),
            "ref" => Some(Self::Ref),
            "versions" => Some(Self::Versions),
            "commit" => Some(Self::Commit),
            _ => None,
        }
    }
}

/// The fixed non-content-addressed id of the single-file boot object: the boot
/// record is unique per library and never addressed by content (01 §4-3).
const BOOT_FIXED_ID: [u8; 16] = *b"tree-space-boot!";

/// One object-directory row: where a stored object lives in the file.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct CatalogRow {
    kind: ObjectKind,
    address: [u8; 16],
    offset: u64,
    length: u64,
}

/// An object staged by a TB write method, flushed as part of the commit epoch.
#[derive(Clone, Debug)]
struct PendingRecord {
    kind: ObjectKind,
    address: [u8; 16],
    bytes: Vec<u8>,
}

/// Single-file `.umdb` storage layout.
pub struct SingleFileLayout {
    path: PathBuf,
    fault: FaultPlan,
    /// Objects staged by `TbLayout` writes while the commit lock is held; the
    /// whole batch becomes one epoch at `write_committed`.
    pending: Mutex<Vec<PendingRecord>>,
}
impl SingleFileLayout {
    /// Creates a layout rooted at a single `.umdb` file path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            fault: FaultPlan::new(),
            pending: Mutex::new(Vec::new()),
        }
    }
    /// Injects a deterministic fault plan for M0 fault tests.
    pub fn with_fault(mut self, fault: FaultPlan) -> Self {
        self.fault = fault;
        self
    }
    /// Returns the data file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn lock_path(&self) -> PathBuf {
        self.path.with_extension(format!(
            "{}lock",
            self.path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!("{value}."))
                .unwrap_or_default()
        ))
    }
    fn lock(&self) -> Result<FileLock> {
        FileLock::try_exclusive(self.lock_path())
    }
    /// Stages one object for the current commit's epoch (dedup against the
    /// already-staged set; the on-disk catalog dedup happens at flush time).
    fn stage(&self, kind: ObjectKind, address: [u8; 16], bytes: &[u8]) -> Result<()> {
        let mut guard = self.pending.lock().expect("single-file pending poisoned");
        if guard
            .iter()
            .any(|record| record.kind == kind && record.address == address)
        {
            return Ok(());
        }
        guard.push(PendingRecord {
            kind,
            address,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }
    /// Reads one staged object from the newest epoch catalog by kind and address.
    fn read_object_at(&self, kind: ObjectKind, address: [u8; 16]) -> Result<Vec<u8>> {
        let Some((rows, _, _)) = self.read_latest_tb_state()? else {
            return Err(self.missing_object(kind, address));
        };
        let row =
            catalog_row(&rows, kind, address).ok_or_else(|| self.missing_object(kind, address))?;
        let mut file = File::open(&self.path).map_err(storage_io)?;
        read_range(&mut file, row.offset, row.length)
    }
    /// The stable error for an absent object of a given kind.
    fn missing_object(&self, kind: ObjectKind, address: [u8; 16]) -> TreeSpaceError {
        let code = match kind {
            ObjectKind::Block => ErrorCode::DanglingReference,
            _ => ErrorCode::StorageCorrupt,
        };
        TreeSpaceError::new(code, "single-file TB object is missing")
            .with_context("address", hex16(&address))
    }
    /// Reads the newest epoch's TB object-directory snapshot, if the newest
    /// epoch is a TB epoch.
    pub(crate) fn read_latest_tb_state(&self) -> Result<Option<(Vec<CatalogRow>, [u8; 16], u64)>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_io(error)),
        };
        read_latest_tb_state_from(&mut file)
    }
    /// The genesis commit id: the v4 commit of the built-in empty bootstrap,
    /// identical across the whole parquet/container family, so the first TB
    /// commit of a single-file library chains off the same genesis as a flat-dir
    /// library.
    fn genesis_commit_id() -> Result<Digest> {
        let tree_id = crate::layout::tb::empty_tree_id();
        let metadata = crate::layout::tb::empty_metadata_ref()?;
        Ok(crate::layout::flat_dir::commit_id_golden(
            0, [0; 16], tree_id, metadata,
        ))
    }
    /// The synthesized genesis commit node (v4 fields only, `tb = None`), used
    /// before the first TB commit so a fresh library opens as "empty library"
    /// instead of corrupting.
    fn genesis_commit() -> Result<CommitNode> {
        let metadata = crate::layout::tb::empty_metadata_ref()?;
        Ok(CommitNode {
            sequence: 0,
            parent: [0; 16],
            tree_id: crate::layout::tb::empty_tree_id().as_bytes(),
            metadata_ref: metadata.to_string(),
            tb: None,
        })
    }
    /// Reads the genesis v4 bootstrap from the header's manifest region (never
    /// the newest epoch, which may be a TB epoch).
    fn read_genesis_bootstrap(&self) -> Result<BootstrapImage> {
        let mut file = File::open(&self.path).map_err(storage_io)?;
        let header = read_header(&mut file)?;
        let payload = read_range(&mut file, header.manifest_offset, header.manifest_length)?;
        BootstrapImage::decode(&payload)
    }
}
impl StorageLayout for SingleFileLayout {
    fn create(&self, bootstrap: &BootstrapImage) -> Result<()> {
        let _lock = self.lock()?;
        if self.path.exists() {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "single-file target already exists",
            )
            .with_context("path", self.path.display().to_string()));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(storage_io)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(storage_io)?;
        file.write_all(&[0; HEADER_LEN]).map_err(storage_io)?;
        let payload = bootstrap.encode()?;
        let trailer = append_epoch(&mut file, &payload, 0, 0)?;
        write_header(
            &mut file,
            HEADER_LEN as u64,
            payload.len() as u64,
            HEADER_LEN as u64,
        )?;
        file.sync_all().map_err(storage_io)?;
        debug_assert!(trailer >= HEADER_LEN as u64);
        Ok(())
    }
    fn open_bootstrap(&self) -> Result<BootstrapImage> {
        let mut file = File::open(&self.path).map_err(storage_io)?;
        let header = read_header(&mut file)?;
        let (_, offset, length) =
            find_latest(&mut file)?.unwrap_or((0, header.manifest_offset, header.manifest_length));
        let payload = read_range(&mut file, offset, length)?;
        BootstrapImage::decode(&payload)
    }
    fn load_table(&self, locator: &TableLocator) -> Result<TableBytes> {
        self.fault.hit(FaultPoint::TableLoad)?;
        let mut file = File::open(&self.path).map_err(storage_io)?;
        Ok(TableBytes {
            bytes: read_range(&mut file, locator.offset, locator.length)?,
            mapped: true,
        })
    }
    fn publish(&self, plan: PublishPlan) -> Result<PublishReceipt> {
        self.fault.hit(FaultPoint::BeforePayload)?;
        let _lock = self.lock()?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(storage_io)?;
        let previous = find_latest(&mut file)?
            .map(|(offset, _, _)| offset)
            .unwrap_or(0);
        let mut end = file.seek(SeekFrom::End(0)).map_err(storage_io)?;
        let mut table_locators = Vec::new();
        let mut metadata = parse_table_metadata(&plan.bootstrap.special_tables["table-metadata"])?;
        for TablePayload {
            table_id,
            content_hash: _,
            bytes: payload,
        } in &plan.table_payloads
        {
            pad_to(&mut file, &mut end)?;
            let offset = end;
            file.write_all(payload).map_err(storage_io)?;
            end += payload.len() as u64;
            let length = payload.len() as u64;
            pad_to(&mut file, &mut end)?;
            if let Some(record) = metadata
                .iter_mut()
                .find(|record| record.table_id == *table_id)
            {
                record.offset = offset;
                record.length = length;
            }
            table_locators.push(TableLocator {
                table_id: *table_id,
                offset,
                length,
                storage_kind: StorageKind::SingleFile,
                path: Some(self.path.clone()),
            });
        }
        let mut bootstrap = plan.bootstrap;
        if !metadata.is_empty() {
            let metadata_batch = encode_table_metadata(&metadata)?;
            bootstrap =
                bootstrap.with_special_tables([("table-metadata".to_owned(), metadata_batch)])?;
        }
        let payload = bootstrap.encode()?;
        self.fault.hit(FaultPoint::BeforeTrailer)?;
        let trailer = append_epoch(&mut file, &payload, plan.sequence, previous)?;
        file.sync_all().map_err(storage_io)?;
        Ok(PublishReceipt {
            sequence: plan.sequence,
            bootstrap_locator: TableLocator {
                table_id: crate::ids::TableId::from_bytes([0; 16]),
                offset: trailer,
                length: TRAILER_LEN as u64,
                storage_kind: StorageKind::SingleFile,
                path: Some(self.path.clone()),
            },
            table_locators,
        })
    }
    fn gc(&self, request: GcRequest) -> Result<GcReport> {
        let _lock = self.lock()?;
        let mut file = File::open(&self.path).map_err(storage_io)?;
        if find_latest(&mut file)?.is_none() {
            return Ok(GcReport { reclaimed: 0 });
        }
        let Some((catalog, head_id, head_sequence)) = read_latest_tb_state_from(&mut file)? else {
            // Pure v4 table-space library: the legacy repack path is unchanged.
            drop(file);
            return self.gc_v4();
        };
        let reachable =
            collect_reachable_tb(&mut file, &catalog, head_id, request.keep_from_sequence)?;
        let mut kept = Vec::new();
        for row in &catalog {
            let keep = match row.kind {
                // The boot record is always reachable: it is the library's
                // version spine and must survive every compaction (01 §4-3).
                ObjectKind::Boot => true,
                ObjectKind::Commit => reachable.commits.contains(&row.address),
                ObjectKind::Tree => reachable.trees.contains(&row.address),
                ObjectKind::Ref => reachable.refs.contains(&row.address),
                ObjectKind::Versions => reachable.versions.contains(&row.address),
                ObjectKind::Block => reachable.blocks.contains(&row.address),
            };
            if keep {
                kept.push(row.clone());
            }
        }
        kept.sort();
        let reclaimed = (catalog.len() - kept.len()) as u64;
        if reclaimed == 0 {
            return Ok(GcReport { reclaimed });
        }
        let temporary = self.path.with_extension("umdb.gc.tmp");
        if temporary.exists() {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "GC temporary already exists",
            )
            .with_context("path", temporary.display().to_string()));
        }
        let bootstrap = self.read_genesis_bootstrap()?;
        let compacted = Self::new(&temporary);
        compacted.create(&bootstrap)?;
        // Rewrite every kept object into the fresh file as one compaction epoch.
        let mut objects = Vec::with_capacity(kept.len());
        for row in &kept {
            let bytes = read_range(&mut file, row.offset, row.length)?;
            objects.push(PendingRecord {
                kind: row.kind,
                address: row.address,
                bytes,
            });
        }
        let mut temporary_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)
            .map_err(storage_io)?;
        append_tb_epoch(&mut temporary_file, &[], &objects, head_id, head_sequence)?;
        temporary_file.sync_all().map_err(storage_io)?;
        drop(temporary_file);
        drop(file);
        fs::rename(&temporary, &self.path).map_err(|error| {
            TreeSpaceError::new(
                if error.kind() == std::io::ErrorKind::PermissionDenied {
                    ErrorCode::ReplaceBlocked
                } else {
                    ErrorCode::StorageCorrupt
                },
                "single-file replacement failed; original was not overwritten",
            )
            .with_context("detail", error.to_string())
            .with_context("temporary", temporary.display().to_string())
        })?;
        Ok(GcReport { reclaimed })
    }
    fn kind(&self) -> StorageKind {
        StorageKind::SingleFile
    }
}

impl SingleFileLayout {
    /// The legacy v4-only repack: rebuild a compacted copy of the bootstrap
    /// snapshot and rename it over the original.
    fn gc_v4(&self) -> Result<GcReport> {
        let bootstrap = self.open_bootstrap()?;
        let temporary = self.path.with_extension("umdb.gc.tmp");
        if temporary.exists() {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "GC temporary already exists",
            )
            .with_context("path", temporary.display().to_string()));
        }
        let compacted = Self::new(&temporary);
        compacted.create(&bootstrap)?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            TreeSpaceError::new(
                if error.kind() == std::io::ErrorKind::PermissionDenied {
                    ErrorCode::ReplaceBlocked
                } else {
                    ErrorCode::StorageCorrupt
                },
                "single-file replacement failed; original was not overwritten",
            )
            .with_context("detail", error.to_string())
            .with_context("temporary", temporary.display().to_string())
        })?;
        Ok(GcReport { reclaimed: 1 })
    }
}

impl TbLayout for SingleFileLayout {
    fn metadata_lock(&self) -> Result<FileLock> {
        self.lock()
    }
    fn root(&self) -> &Path {
        self.path()
    }
    fn block_materialization(&self) -> Materialization {
        // Single-file is force-IPC materialization (01 §1.11.3/§1.11.6); the
        // container cannot map Parquet objects zero-copy.
        Materialization::Ipc
    }

    fn read_head(&self) -> Result<Option<(Digest, u64)>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_io(error)),
        };
        let Some((trailer_offset, payload_offset, payload_length)) = find_latest(&mut file)? else {
            return Ok(None);
        };
        match read_tb_epoch_payload(&mut file, trailer_offset, payload_offset, payload_length)? {
            Some((_, head_id, head_sequence)) => {
                Ok(Some((Digest::from_bytes(head_id), head_sequence)))
            }
            None => {
                // The newest (only) epoch is the v4 genesis: report the genesis
                // commit so a TB commit can chain off it (mirrors the flat-dir
                // create-time genesis commit, `tb = None`).
                Ok(Some((Self::genesis_commit_id()?, 0)))
            }
        }
    }
    fn write_committed(&self, commit_id: Digest, sequence: u64) -> Result<()> {
        // Visibility = the new epoch's trailer: every object staged by the
        // write methods is flushed here as one epoch (每 commit 一 epoch).
        let pending = {
            let mut guard = self.pending.lock().expect("single-file pending poisoned");
            std::mem::take(&mut *guard)
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(storage_io)?;
        let old_rows = match read_latest_tb_state_from(&mut file)? {
            Some((rows, _, _)) => rows,
            None => Vec::new(),
        };
        self.fault.hit(FaultPoint::BeforeTrailer)?;
        append_tb_epoch(
            &mut file,
            &old_rows,
            &pending,
            commit_id.as_bytes(),
            sequence,
        )?;
        file.sync_all().map_err(storage_io)?;
        Ok(())
    }
    fn read_commit(&self, path: &Path) -> Result<CommitNode> {
        let name = path.to_string_lossy();
        let hex_digest = name
            .rsplit("#commit:")
            .next()
            .ok_or_else(|| corrupt("single-file commit path has no commit marker"))?;
        let id = crate::layout::flat_dir::hex_to_bytes(hex_digest)
            .ok_or_else(|| corrupt("single-file commit path has no 32-hex-char commit id"))?;
        let commit_id = Digest::from_bytes(id);
        if commit_id == Self::genesis_commit_id()? {
            return Self::genesis_commit();
        }
        let Some((rows, _, _)) = self.read_latest_tb_state()? else {
            return Err(corrupt("single-file has no TB epoch")
                .with_context("commit", commit_id.to_string()));
        };
        let row = catalog_row(&rows, ObjectKind::Commit, id).ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::StorageCorrupt, "commit object is absent")
                .with_context("commit", commit_id.to_string())
        })?;
        let mut file = File::open(&self.path).map_err(storage_io)?;
        let bytes = read_range(&mut file, row.offset, row.length)?;
        decode_commit_bytes(&bytes)
    }
    fn commit_path(&self, commit_id: Digest) -> PathBuf {
        PathBuf::from(format!("{}#commit:{}", self.path.display(), commit_id))
    }
    fn tb_block_path(&self, address: Digest) -> PathBuf {
        PathBuf::from(format!("{}#block:{}", self.path.display(), address))
    }
    fn tb_tree_path(&self, address: Digest) -> PathBuf {
        PathBuf::from(format!("{}#tree:{}", self.path.display(), address))
    }
    fn tb_ref_path(&self, address: Digest) -> PathBuf {
        PathBuf::from(format!("{}#ref:{}", self.path.display(), address))
    }
    fn write_block_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Block, address.as_bytes(), bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）：offset 定位读，不重算地址。
    /// 默认 verified 路径（`read_block_object`）由 trait default 提供。
    fn read_block_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_object_at(ObjectKind::Block, address.as_bytes())
    }
    fn write_tree_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Tree, address.as_bytes(), bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）。
    fn read_tree_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_object_at(ObjectKind::Tree, address.as_bytes())
    }
    fn write_ref_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Ref, address.as_bytes(), bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）。
    fn read_ref_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_object_at(ObjectKind::Ref, address.as_bytes())
    }
    fn tb_versions_path(&self, address: Digest) -> PathBuf {
        PathBuf::from(format!("{}#versions:{}", self.path.display(), address))
    }
    fn write_versions_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Versions, address.as_bytes(), bytes)
    }
    fn read_versions_object(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_object_at(ObjectKind::Versions, address.as_bytes())
    }
    fn write_commit_object(&self, commit_id: Digest, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Commit, commit_id.as_bytes(), bytes)
    }
    fn ensure_tb_dirs(&self) -> Result<()> {
        // The single-file container is its own object directory: no channels.
        Ok(())
    }
    fn block_objects(&self) -> Result<Vec<(Digest, Vec<u8>)>> {
        let Some((rows, _, _)) = self.read_latest_tb_state()? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut file = File::open(&self.path).map_err(storage_io)?;
        for row in rows.iter().filter(|row| row.kind == ObjectKind::Block) {
            let bytes = read_range(&mut file, row.offset, row.length)?;
            out.push((Digest::from_bytes(row.address), bytes));
        }
        Ok(out)
    }
    /// Stages the boot record at the fixed per-library id (01 §4-3). Boot is
    /// not content-addressed; like every object it flushes with the next
    /// commit epoch (`write_committed`), so a fresh single-file library gains
    /// its readable boot record at its first commit.
    fn write_boot(&self, bytes: &[u8]) -> Result<()> {
        self.stage(ObjectKind::Boot, BOOT_FIXED_ID, bytes)
    }
    /// Reads the boot record from the newest epoch's object directory; an
    /// absent record (no TB epoch yet, or no boot object) is a hard
    /// [`ErrorCode::BootstrapIncomplete`] failure.
    fn read_boot(&self) -> Result<Vec<u8>> {
        let Some((rows, _, _)) = self.read_latest_tb_state()? else {
            return Err(TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "single-file has no TB epoch; boot record is absent",
            ));
        };
        let Some(row) = catalog_row(&rows, ObjectKind::Boot, BOOT_FIXED_ID) else {
            return Err(TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "single-file boot object is missing from the object directory",
            ));
        };
        let mut file = File::open(&self.path).map_err(storage_io)?;
        read_range(&mut file, row.offset, row.length)
    }
}

impl ObjectSpan for SingleFileLayout {
    fn object_span(
        &self,
        channel: TbChannel,
        address: Digest,
    ) -> Result<Option<(PathBuf, u64, u64)>> {
        // Single-file mapping = the whole `.umdb` container mmap, sliced at the
        // object-directory offset of the addressed object (02 §8.2-2/§8.6).
        let Some((rows, _, _)) = self.read_latest_tb_state()? else {
            return Ok(None);
        };
        let kind = match channel {
            TbChannel::Block => ObjectKind::Block,
            TbChannel::Tree => ObjectKind::Tree,
            TbChannel::Ref => ObjectKind::Ref,
        };
        let Some(row) = catalog_row(&rows, kind, address.as_bytes()) else {
            return Ok(None);
        };
        Ok(Some((self.path.clone(), row.offset, row.length)))
    }
}

// ---------------------------------------------------------------------------
// Epoch container primitives (shared v4 + TB frame)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Header {
    manifest_offset: u64,
    manifest_length: u64,
}
fn write_header(
    file: &mut File,
    manifest_offset: u64,
    manifest_length: u64,
    first_epoch_offset: u64,
) -> Result<()> {
    let mut header = [0_u8; HEADER_LEN];
    header[..8].copy_from_slice(HEADER_MAGIC);
    header[8..10].copy_from_slice(&3_u16.to_le_bytes());
    header[10..12].copy_from_slice(&0_u16.to_le_bytes());
    header[16..24].copy_from_slice(&manifest_offset.to_le_bytes());
    header[24..32].copy_from_slice(&manifest_length.to_le_bytes());
    header[32..40].copy_from_slice(&first_epoch_offset.to_le_bytes());
    header[40..44].copy_from_slice(&(ALIGNMENT as u32).to_le_bytes());
    let checksum = checksum32(&header[..44]);
    header[44..48].copy_from_slice(&checksum.to_le_bytes());
    file.seek(SeekFrom::Start(0)).map_err(storage_io)?;
    file.write_all(&header).map_err(storage_io)
}
fn read_header(file: &mut File) -> Result<Header> {
    let bytes = read_range(file, 0, HEADER_LEN as u64)?;
    if bytes[..8] != *HEADER_MAGIC
        || u16::from_le_bytes(bytes[8..10].try_into().expect("slice")) != 3
        || u32::from_le_bytes(bytes[40..44].try_into().expect("slice")) != ALIGNMENT as u32
        || checksum32(&bytes[..44]) != u32::from_le_bytes(bytes[44..48].try_into().expect("slice"))
        || bytes[48..].iter().any(|value| *value != 0)
    {
        return Err(TreeSpaceError::new(
            ErrorCode::StorageCorrupt,
            "single-file header is invalid",
        ));
    }
    Ok(Header {
        manifest_offset: u64::from_le_bytes(bytes[16..24].try_into().expect("slice")),
        manifest_length: u64::from_le_bytes(bytes[24..32].try_into().expect("slice")),
    })
}
fn append_epoch(file: &mut File, payload: &[u8], epoch: u64, previous: u64) -> Result<u64> {
    let mut end = file.seek(SeekFrom::End(0)).map_err(storage_io)?;
    pad_to(file, &mut end)?;
    let payload_offset = end;
    file.write_all(payload).map_err(storage_io)?;
    end += payload.len() as u64;
    pad_to(file, &mut end)?;
    let directory = directory_bytes(payload_offset, payload.len() as u64, epoch)?;
    let directory_offset = end;
    file.write_all(&directory).map_err(storage_io)?;
    end += directory.len() as u64;
    pad_to(file, &mut end)?;
    let trailer_offset = end;
    let mut trailer = [0_u8; TRAILER_LEN];
    trailer[..8].copy_from_slice(TRAILER_MAGIC);
    trailer[8..10].copy_from_slice(&1_u16.to_le_bytes());
    trailer[12..20].copy_from_slice(&epoch.to_le_bytes());
    trailer[20..28].copy_from_slice(&directory_offset.to_le_bytes());
    trailer[28..36].copy_from_slice(&(directory.len() as u64).to_le_bytes());
    trailer[36..44].copy_from_slice(&previous.to_le_bytes());
    trailer[44..60].copy_from_slice(&xxhash_rust::xxh3::xxh3_128(&directory).to_be_bytes());
    let checksum = checksum32(&trailer[..60]);
    trailer[60..64].copy_from_slice(&checksum.to_le_bytes());
    file.write_all(&trailer).map_err(storage_io)?;
    Ok(trailer_offset)
}
fn find_latest(file: &mut File) -> Result<Option<(u64, u64, u64)>> {
    let len = file.seek(SeekFrom::End(0)).map_err(storage_io)?;
    if len < (HEADER_LEN + TRAILER_LEN) as u64 {
        return Ok(None);
    }
    let mut cursor = len - TRAILER_LEN as u64;
    loop {
        if cursor % ALIGNMENT == 0 {
            if let Ok(bytes) = read_range(file, cursor, TRAILER_LEN as u64) {
                if let Some((epoch, offset, length)) = parse_trailer(file, &bytes, len)? {
                    let _ = epoch;
                    return Ok(Some((cursor, offset, length)));
                }
            }
        }
        if cursor < ALIGNMENT {
            break;
        }
        cursor -= ALIGNMENT;
    }
    Ok(None)
}
fn parse_trailer(file: &mut File, bytes: &[u8], file_len: u64) -> Result<Option<(u64, u64, u64)>> {
    if bytes[..8] != *TRAILER_MAGIC
        || u16::from_le_bytes(bytes[8..10].try_into().expect("slice")) != 1
        || checksum32(&bytes[..60]) != u32::from_le_bytes(bytes[60..64].try_into().expect("slice"))
    {
        return Ok(None);
    }
    let epoch = u64::from_le_bytes(bytes[12..20].try_into().expect("slice"));
    let offset = u64::from_le_bytes(bytes[20..28].try_into().expect("slice"));
    let length = u64::from_le_bytes(bytes[28..36].try_into().expect("slice"));
    if offset
        .checked_add(length)
        .filter(|end| *end <= file_len)
        .is_none()
    {
        return Ok(None);
    }
    let directory = read_range(file, offset, length)?;
    if xxhash_rust::xxh3::xxh3_128(&directory).to_be_bytes() != bytes[44..60] {
        return Ok(None);
    }
    let batch = decode_batch(&directory).map_err(|_| {
        TreeSpaceError::new(
            ErrorCode::StorageCorrupt,
            "epoch directory is not Arrow IPC",
        )
    })?;
    let payload_offset = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::StorageCorrupt,
                "directory offset type is invalid",
            )
        })?
        .value(0);
    let payload_length = batch
        .column(1)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::StorageCorrupt,
                "directory length type is invalid",
            )
        })?
        .value(0);
    if payload_offset
        .checked_add(payload_length)
        .filter(|end| *end <= file_len)
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some((epoch, payload_offset, payload_length)))
}
fn directory_bytes(offset: u64, length: u64, epoch: u64) -> Result<Vec<u8>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("payload_offset", DataType::UInt64, false),
        Field::new("payload_length", DataType::UInt64, false),
        Field::new("generation", DataType::UInt64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(vec![offset])),
            Arc::new(UInt64Array::from(vec![length])),
            Arc::new(UInt64Array::from(vec![epoch])),
        ],
    )
    .map_err(|error| {
        TreeSpaceError::new(ErrorCode::StorageCorrupt, "cannot build epoch directory")
            .with_context("detail", error.to_string())
    })?;
    encode_batch(&batch)
}
fn pad_to(file: &mut File, position: &mut u64) -> Result<()> {
    let padding = (ALIGNMENT - (*position % ALIGNMENT)) % ALIGNMENT;
    if padding != 0 {
        file.write_all(&vec![0; padding as usize])
            .map_err(storage_io)?;
        *position += padding;
    }
    Ok(())
}
fn read_range(file: &mut File, offset: u64, length: u64) -> Result<Vec<u8>> {
    let end = file.seek(SeekFrom::End(0)).map_err(storage_io)?;
    if offset
        .checked_add(length)
        .filter(|value| *value <= end)
        .is_none()
    {
        return Err(TreeSpaceError::new(
            ErrorCode::StorageCorrupt,
            "offset and length are outside container",
        )
        .with_context("offset", offset.to_string()));
    }
    file.seek(SeekFrom::Start(offset)).map_err(storage_io)?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes).map_err(storage_io)?;
    Ok(bytes)
}
fn checksum32(bytes: &[u8]) -> u32 {
    xxh3_64(bytes) as u32
}
fn storage_io(error: std::io::Error) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, "storage I/O failed")
        .with_context("detail", error.to_string())
}

// ---------------------------------------------------------------------------
// TB epoch framing (P-IO-7.3)
// ---------------------------------------------------------------------------

/// The frozen Arrow schema of the TB object-directory batch.
fn catalog_schema() -> Schema {
    Schema::new(vec![
        Field::new("kind", DataType::Utf8, false),
        Field::new("address", DataType::FixedSizeBinary(16), false),
        Field::new("offset", DataType::UInt64, false),
        Field::new("length", DataType::UInt64, false),
    ])
}

/// Encodes the object-directory snapshot as one canonical Arrow IPC batch.
fn encode_catalog(rows: &[CatalogRow]) -> Result<Vec<u8>> {
    let kinds = rows.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>();
    let addresses = rows.iter().map(|row| row.address).collect::<Vec<_>>();
    let offsets = rows.iter().map(|row| row.offset).collect::<Vec<_>>();
    let lengths = rows.iter().map(|row| row.length).collect::<Vec<_>>();
    let batch = RecordBatch::try_new(
        Arc::new(catalog_schema()),
        vec![
            Arc::new(StringArray::from(kinds)) as ArrayRef,
            Arc::new(fixed16(addresses)) as ArrayRef,
            Arc::new(UInt64Array::from(offsets)) as ArrayRef,
            Arc::new(UInt64Array::from(lengths)) as ArrayRef,
        ],
    )
    .map_err(|error| corrupt(format!("cannot build epoch catalog: {error}")))?;
    encode_batch(&batch)
}

/// Decodes an object-directory snapshot, validating schema, nulls and kinds.
fn decode_catalog(bytes: &[u8]) -> Result<Vec<CatalogRow>> {
    let batch = decode_batch(bytes)?;
    let expected = catalog_schema();
    let actual = batch.schema();
    if actual.fields().len() != expected.fields().len() {
        return Err(storage(
            "epoch catalog schema does not match the canonical column set",
        ));
    }
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(storage(
                "epoch catalog schema does not match the canonical column set",
            ));
        }
    }
    let kinds = downcast::<StringArray>(&batch, 0)?;
    let addresses = downcast::<FixedSizeBinaryArray>(&batch, 1)?;
    let offsets = downcast::<UInt64Array>(&batch, 2)?;
    let lengths = downcast::<UInt64Array>(&batch, 3)?;
    if addresses.value_length() != 16 {
        return Err(storage("epoch catalog address column is not 16 bytes"));
    }
    let mut rows = Vec::with_capacity(batch.num_rows());
    for index in 0..batch.num_rows() {
        if kinds.is_null(index)
            || addresses.is_null(index)
            || offsets.is_null(index)
            || lengths.is_null(index)
        {
            return Err(storage("epoch catalog rows cannot be null"));
        }
        let kind = ObjectKind::from_str(kinds.value(index)).ok_or_else(|| {
            storage(format!(
                "epoch catalog has an unknown object kind: {}",
                kinds.value(index)
            ))
        })?;
        let address = addresses
            .value(index)
            .try_into()
            .map_err(|_| storage("epoch catalog address has the wrong width"))?;
        rows.push(CatalogRow {
            kind,
            address,
            offset: offsets.value(index),
            length: lengths.value(index),
        });
    }
    Ok(rows)
}

/// The frozen Arrow schema of the TB epoch outer directory.
fn tb_directory_schema() -> Schema {
    Schema::new(vec![
        Field::new("payload_offset", DataType::UInt64, false),
        Field::new("payload_length", DataType::UInt64, false),
        Field::new("generation", DataType::UInt64, false),
        Field::new("payload_hash", DataType::FixedSizeBinary(16), false),
        Field::new("head_commit_id", DataType::FixedSizeBinary(16), false),
        Field::new("head_sequence", DataType::UInt64, false),
    ])
}

/// Encodes the TB epoch outer directory: the v4 three columns plus a full
/// payload hash (crash/bit-flip detection) and the head commit pointer.
fn tb_directory_bytes(
    payload_offset: u64,
    payload_length: u64,
    generation: u64,
    payload_hash: [u8; 16],
    head_commit_id: [u8; 16],
    head_sequence: u64,
) -> Result<Vec<u8>> {
    let batch = RecordBatch::try_new(
        Arc::new(tb_directory_schema()),
        vec![
            Arc::new(UInt64Array::from(vec![payload_offset])) as ArrayRef,
            Arc::new(UInt64Array::from(vec![payload_length])) as ArrayRef,
            Arc::new(UInt64Array::from(vec![generation])) as ArrayRef,
            Arc::new(fixed16(vec![payload_hash])) as ArrayRef,
            Arc::new(fixed16(vec![head_commit_id])) as ArrayRef,
            Arc::new(UInt64Array::from(vec![head_sequence])) as ArrayRef,
        ],
    )
    .map_err(|error| corrupt(format!("cannot build TB epoch directory: {error}")))?;
    encode_batch(&batch)
}

/// The decoded TB epoch directory (payload location, hash, head pointer).
struct TbDirectory {
    head_commit_id: [u8; 16],
    head_sequence: u64,
    payload_hash: [u8; 16],
}

/// Decodes a TB epoch outer directory, or `None` when the directory is the v4
/// three-column form (a v4 bootstrap epoch, not a TB epoch).
fn decode_tb_directory(bytes: &[u8]) -> Result<Option<TbDirectory>> {
    let batch = decode_batch(bytes)?;
    if batch.num_columns() != 6 {
        return Ok(None);
    }
    let expected = tb_directory_schema();
    let actual = batch.schema();
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(storage("TB epoch directory schema does not match"));
        }
    }
    let hash_column = downcast::<FixedSizeBinaryArray>(&batch, 3)?;
    let head_column = downcast::<FixedSizeBinaryArray>(&batch, 4)?;
    if hash_column.value_length() != 16 || head_column.value_length() != 16 {
        return Err(storage(
            "TB epoch directory digest columns are not 16 bytes",
        ));
    }
    let payload_hash = hash_column
        .value(0)
        .try_into()
        .map_err(|_| storage("TB epoch payload hash has the wrong width"))?;
    let head_commit_id = head_column
        .value(0)
        .try_into()
        .map_err(|_| storage("TB epoch head commit id has the wrong width"))?;
    Ok(Some(TbDirectory {
        head_commit_id,
        head_sequence: downcast::<UInt64Array>(&batch, 5)?.value(0),
        payload_hash,
    }))
}

/// Reads and validates the newest epoch, returning the TB state when the newest
/// epoch is a TB epoch (`Some`), `None` when it is a v4 epoch or absent.
fn read_latest_tb_state_from(file: &mut File) -> Result<Option<(Vec<CatalogRow>, [u8; 16], u64)>> {
    let Some((trailer_offset, payload_offset, payload_length)) = find_latest(file)? else {
        return Ok(None);
    };
    read_tb_epoch_payload(file, trailer_offset, payload_offset, payload_length)
}

/// Extracts the catalog snapshot, head commit id and sequence from one TB
/// epoch, validating its directory payload hash. `None` for a v4 epoch.
fn read_tb_epoch_payload(
    file: &mut File,
    trailer_offset: u64,
    payload_offset: u64,
    payload_length: u64,
) -> Result<Option<(Vec<CatalogRow>, [u8; 16], u64)>> {
    let trailer = read_range(file, trailer_offset, TRAILER_LEN as u64)?;
    let directory_offset = u64::from_le_bytes(trailer[20..28].try_into().expect("slice"));
    let directory_length = u64::from_le_bytes(trailer[28..36].try_into().expect("slice"));
    let directory = read_range(file, directory_offset, directory_length)?;
    let Some(dir) = decode_tb_directory(&directory)? else {
        return Ok(None);
    };
    let payload = read_range(file, payload_offset, payload_length)?;
    if xxhash_rust::xxh3::xxh3_128(&payload).to_be_bytes() != dir.payload_hash {
        return Err(TreeSpaceError::new(
            ErrorCode::StorageCorrupt,
            "single-file TB epoch payload hash mismatch",
        ));
    }
    let rows = decode_catalog(&payload)?;
    Ok(Some((rows, dir.head_commit_id, dir.head_sequence)))
}

/// Appends one TB epoch: deduplicated objects, then the catalog-snapshot
/// payload, a TB outer directory and a trailer (the visibility switch).
fn append_tb_epoch(
    file: &mut File,
    old_rows: &[CatalogRow],
    objects: &[PendingRecord],
    head_id: [u8; 16],
    head_sequence: u64,
) -> Result<()> {
    let previous = find_latest(file)?.map(|(offset, _, _)| offset).unwrap_or(0);
    let mut known = BTreeSet::new();
    for row in old_rows {
        known.insert((row.kind, row.address));
    }
    let mut end = file.seek(SeekFrom::End(0)).map_err(storage_io)?;
    pad_to(file, &mut end)?;
    let mut new_rows = Vec::new();
    for object in objects {
        let key = (object.kind, object.address);
        if !known.insert(key) {
            // `exists → skip`: the object is already stored at this address.
            continue;
        }
        let offset = end;
        file.write_all(&object.bytes).map_err(storage_io)?;
        end += object.bytes.len() as u64;
        pad_to(file, &mut end)?;
        new_rows.push(CatalogRow {
            kind: object.kind,
            address: object.address,
            offset,
            length: object.bytes.len() as u64,
        });
    }
    let payload_start = end;
    let mut all_rows = old_rows.to_vec();
    all_rows.extend(new_rows);
    all_rows.sort();
    let payload = encode_catalog(&all_rows)?;
    let payload_hash = xxhash_rust::xxh3::xxh3_128(&payload).to_be_bytes();
    file.write_all(&payload).map_err(storage_io)?;
    end += payload.len() as u64;
    pad_to(file, &mut end)?;
    let directory = tb_directory_bytes(
        payload_start,
        payload.len() as u64,
        head_sequence,
        payload_hash,
        head_id,
        head_sequence,
    )?;
    let directory_offset = end;
    file.write_all(&directory).map_err(storage_io)?;
    end += directory.len() as u64;
    pad_to(file, &mut end)?;
    let _trailer_offset = end;
    let mut trailer = [0_u8; TRAILER_LEN];
    trailer[..8].copy_from_slice(TRAILER_MAGIC);
    trailer[8..10].copy_from_slice(&1_u16.to_le_bytes());
    trailer[12..20].copy_from_slice(&head_sequence.to_le_bytes());
    trailer[20..28].copy_from_slice(&directory_offset.to_le_bytes());
    trailer[28..36].copy_from_slice(&(directory.len() as u64).to_le_bytes());
    trailer[36..44].copy_from_slice(&previous.to_le_bytes());
    trailer[44..60].copy_from_slice(&xxhash_rust::xxh3::xxh3_128(&directory).to_be_bytes());
    let checksum = checksum32(&trailer[..60]);
    trailer[60..64].copy_from_slice(&checksum.to_le_bytes());
    file.write_all(&trailer).map_err(storage_io)?;
    Ok(())
}

/// Decodes a stored commit object's bytes into a [`CommitNode`].
fn decode_commit_bytes(bytes: &[u8]) -> Result<CommitNode> {
    let batch = decode_batch(bytes)?;
    let sequence = downcast::<UInt64Array>(&batch, 0)?.value(0);
    let parent = downcast::<FixedSizeBinaryArray>(&batch, 1)?
        .value(0)
        .try_into()
        .map_err(|_| corrupt("commit parent length"))?;
    let tree_id = downcast::<FixedSizeBinaryArray>(&batch, 2)?
        .value(0)
        .try_into()
        .map_err(|_| corrupt("commit tree length"))?;
    let metadata_ref = downcast::<StringArray>(&batch, 3)?.value(0).to_owned();
    let tb = crate::layout::tb::decode_tb_commit_pointers(&batch)?;
    Ok(CommitNode {
        sequence,
        parent,
        tree_id,
        metadata_ref,
        tb,
    })
}

/// Finds one catalog row by kind and address.
pub(crate) fn catalog_row<'a>(
    rows: &'a [CatalogRow],
    kind: ObjectKind,
    address: [u8; 16],
) -> Option<&'a CatalogRow> {
    rows.iter()
        .find(|row| row.kind == kind && row.address == address)
}

/// The reachable object-address sets of a commit chain walk.
#[derive(Default)]
struct TbReachable {
    commits: BTreeSet<[u8; 16]>,
    trees: BTreeSet<[u8; 16]>,
    refs: BTreeSet<[u8; 16]>,
    versions: BTreeSet<[u8; 16]>,
    blocks: BTreeSet<[u8; 16]>,
}

/// Walks the commit chain from `head_id`, keeping commits whose sequence is
/// inside the `keep_from_sequence` window and collecting every object they
/// reference (tree blobs, reference tables, and through the tables the blocks).
fn collect_reachable_tb(
    file: &mut File,
    catalog: &[CatalogRow],
    head_id: [u8; 16],
    keep_from_sequence: u64,
) -> Result<TbReachable> {
    let mut reachable = TbReachable::default();
    let mut stack = vec![head_id];
    while let Some(commit_id) = stack.pop() {
        if !reachable.commits.insert(commit_id) {
            continue;
        }
        let row = catalog_row(catalog, ObjectKind::Commit, commit_id).ok_or_else(|| {
            corrupt("commit chain references a commit absent from the object directory")
        })?;
        let node = decode_commit_bytes(&read_range(file, row.offset, row.length)?)?;
        if let Some(tb) = node.tb {
            reachable.trees.insert(tb.tree_blob);
            reachable.refs.insert(tb.refs);
            if let Some(versions) = tb.versions {
                // The version side-table is a tree companion reachable through
                // the commit's ninth column (01 §3-1).
                reachable.versions.insert(versions);
            }
            // GC never decodes tree bytes: block reachability is derived solely
            // from the three-column reference table (single hop).
            let ref_row = catalog_row(catalog, ObjectKind::Ref, tb.refs).ok_or_else(|| {
                corrupt("commit references a reference table absent from the object directory")
            })?;
            let ref_bytes = read_range(file, ref_row.offset, ref_row.length)?;
            for ref_row in crate::layout::tb::decode_ref_table(&ref_bytes)? {
                reachable.blocks.insert(ref_row.address);
            }
        }
        if node.parent != [0; 16] {
            if let Some(parent_row) = catalog_row(catalog, ObjectKind::Commit, node.parent) {
                let parent =
                    decode_commit_bytes(&read_range(file, parent_row.offset, parent_row.length)?)?;
                if parent.sequence >= keep_from_sequence {
                    stack.push(node.parent);
                }
            }
        }
    }
    Ok(reachable)
}

/// Encodes a 16-byte address as its 32-char lowercase hex name.
fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fixed16(values: Vec<[u8; 16]>) -> FixedSizeBinaryArray {
    if values.is_empty() {
        FixedSizeBinaryArray::new(16, arrow::buffer::Buffer::from(Vec::<u8>::new()), None)
    } else {
        FixedSizeBinaryArray::try_from_iter(values.into_iter().map(|value| value.to_vec()))
            .expect("16-byte values are valid fixed binary")
    }
}

fn downcast<'a, T: arrow::array::Array + 'static>(
    batch: &'a RecordBatch,
    index: usize,
) -> Result<&'a T> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| storage("TB epoch column has incorrect Arrow type"))
}

fn storage(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, message.into())
}

fn corrupt(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, message.into())
}
