//! Storage layouts and their shared immutable publication boundary.

pub mod flat_dir;
pub mod materializer;
pub mod memory;
pub mod single_file;
pub mod tb;

pub use flat_dir::FlatDirLayout;
pub use materializer::{
    BlockMaterializer, IpcMaterializer, Materialization, ParquetMaterializer, materializer_for,
    probe_block_materialization, select_materializer,
};
pub use memory::MemoryLayout;
pub use single_file::SingleFileLayout;
pub use tb::{RefRow, TbPointers, VersionRow, versions_table_address};

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{Digest, TableId};
use crate::layout::tb::{block_blob_address, ref_table_address, tree_blob_address};
use crate::lock::FileLock;
use crate::manifest::BootstrapImage;
use std::path::{Path, PathBuf};

/// Physical storage layout selected when a library is created or opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageKind {
    /// In-memory bootstrap and test layout.
    Memory,
    /// One append-only `.umdb` file.
    SingleFile,
    /// A manifest IPC file, Parquet metadata, and Parquet table directory.
    FlatDir,
}

/// Layout-specific location of a table payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableLocator {
    /// Physical table identity.
    pub table_id: TableId,
    /// Offset used by append-only layouts.
    pub offset: u64,
    /// Byte length used by append-only layouts.
    pub length: u64,
    /// Layout that owns this locator.
    pub storage_kind: StorageKind,
    /// Optional physical path for directory layouts.
    pub path: Option<PathBuf>,
}

/// Immutable bytes returned by a storage layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableBytes {
    /// Arrow 55 IPC payload bytes.
    pub bytes: Vec<u8>,
    /// Whether bytes were loaded through a read-only mapped path.
    pub mapped: bool,
}

/// One immutable table payload handed to the publication phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TablePayload {
    /// Physical table identity.
    pub table_id: TableId,
    /// Storage content hash (full content, incl. `in_hash=false`), computed by
    /// the writer in memory. Required by FlatDir content addressing; ignored by
    /// Memory and SingleFile.
    pub content_hash: Option<Digest>,
    /// Arrow 55 IPC payload bytes.
    pub bytes: Vec<u8>,
}

/// Fully validated input to the publication phase.
#[derive(Clone)]
pub struct PublishPlan {
    /// The complete next bootstrap image.
    pub bootstrap: BootstrapImage,
    /// Layout-local monotonically increasing publication sequence.
    pub sequence: u64,
    /// Optional immutable table payloads.
    pub table_payloads: Vec<TablePayload>,
}

/// Receipt returned only after a layout has made a complete snapshot visible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishReceipt {
    /// Layout-local publication sequence.
    pub sequence: u64,
    /// The visible location of the bootstrap image.
    pub bootstrap_locator: TableLocator,
    /// Locators for each published table payload in plan order.
    pub table_locators: Vec<TableLocator>,
}

/// Request to reclaim obsolete append-only or replaced files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcRequest {
    /// Keep history newer than this layout-local sequence where supported.
    pub keep_from_sequence: u64,
}

/// Layout reclamation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcReport {
    /// Number of reclaimed obsolete artifacts.
    pub reclaimed: u64,
}

/// The only storage boundary used by the runtime.
pub trait StorageLayout: Send + Sync {
    /// Creates an empty storage target from a fully validated bootstrap image.
    fn create(&self, bootstrap: &BootstrapImage) -> Result<()>;
    /// Opens and validates the latest complete bootstrap image.
    fn open_bootstrap(&self) -> Result<BootstrapImage>;
    /// Loads an immutable table payload.
    fn load_table(&self, locator: &TableLocator) -> Result<TableBytes>;
    /// Publishes a prevalidated plan without inferring business schema.
    fn publish(&self, plan: PublishPlan) -> Result<PublishReceipt>;
    /// Reclaims only artifacts that are no longer part of the current snapshot.
    fn gc(&self, request: GcRequest) -> Result<GcReport>;
    /// Reports the concrete layout kind.
    fn kind(&self) -> StorageKind;
}

/// Layout-specific view of one commit object row.
///
/// Carries the v4 four columns plus the parsed TB extension pointers (see
/// `_dev/树与桶管道/01-目标与设计.md` §1.9.5).
#[derive(Clone, Debug)]
pub struct CommitNode {
    /// The layout-local commit sequence.
    pub sequence: u64,
    /// The parent commit id (`[0; 16]` for the genesis commit).
    pub parent: [u8; 16],
    /// The v4 table-space tree id (the constant degradation object on TB commits).
    pub tree_id: [u8; 16],
    /// The v4 metadata snapshot reference.
    pub metadata_ref: String,
    /// The parsed TB extension pointers, when the commit carries them.
    pub tb: Option<crate::layout::tb::TbPointers>,
}

/// The unified tree-and-bucket data interface shared by every disk layout
/// (P-IO-7.1 统御, `_dev/树与桶管道/01-目标与设计.md` §1.11.5).
///
/// This is the public surface a [`crate::TbLibrary`] drives, decoupling the
/// handle from any concrete layout: the drawn first instance is
/// [`FlatDirLayout`], with the single-file container ported in P-IO-7.3.
/// The interface covers the commit-tree object channels (`metadata_lock`,
/// `read_head`, `write_committed`, `read_commit`, `commit_path`), the three TB
/// channels, reachability GC, and the block-object read/write surface through
/// which block materialization (IPC/Parquet) flows. The v4 table-space surface
/// is inherited from [`StorageLayout`].
pub trait TbLayout: StorageLayout {
    /// Acquires the layout-local metadata lock (cross-layout unified semantics).
    fn metadata_lock(&self) -> Result<FileLock>;
    /// Reads the committed head pointer, returning `(commit_id, sequence)`.
    fn read_head(&self) -> Result<Option<(Digest, u64)>>;
    /// Switches the single visibility point to `commit_id` at `sequence`.
    fn write_committed(&self, commit_id: Digest, sequence: u64) -> Result<()>;
    /// Reads and decodes a commit object by its full path.
    fn read_commit(&self, path: &Path) -> Result<CommitNode>;
    /// The path of a commit object.
    fn commit_path(&self, commit_id: Digest) -> PathBuf;
    /// The path of a block envelope object (`tb-blocks/`).
    fn tb_block_path(&self, address: Digest) -> PathBuf;
    /// The path of a canonical tree blob object (`tb-trees/`).
    fn tb_tree_path(&self, address: Digest) -> PathBuf;
    /// The path of a reference-table object (`tb-refs/`).
    fn tb_ref_path(&self, address: Digest) -> PathBuf;
    /// The library root directory.
    fn root(&self) -> &Path;
    /// Writes one block object at its content address (dedup: exists → skip).
    fn write_block_object(&self, address: Digest, bytes: &[u8]) -> Result<()>;
    /// Reads one block object by its content address.
    ///
    /// 对外语义升级（01 §4.2 / 02 §7.2）：默认走 verified——bytes 已在内存，
    /// 多算一次 `block_blob_address` 并比对，不匹配 → `Err(DigestMismatch)`
    /// （不修复）。签名不变。内部已验路径（restore_bucket_from_refs / verify
    /// / IPC receive / `LazyIpcSnapshot::block`）请走
    /// [`Self::read_block_object_unchecked`] 避免双 hash（02 §7.2）。
    fn read_block_object(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_block_object_verified(address)
    }
    /// 整块读（免验）：纯读不重算，供内部已验路径复用，避免已验处重复计算
    /// （restore_bucket_from_refs / verify / IPC receive / `LazyIpcSnapshot::block`，
    /// 01 §4.2 / 02 §7.2）。
    fn read_block_object_unchecked(&self, address: Digest) -> Result<Vec<u8>>;
    /// 整块读（默认验）：`*_unchecked` 之后多算一次 `block_blob_address` 并比对；
    /// 不匹配 → `Err(DigestMismatch)`（不修复，01 §4.2 / §8）。
    fn read_block_object_verified(&self, address: Digest) -> Result<Vec<u8>> {
        let bytes = self.read_block_object_unchecked(address)?;
        if block_blob_address(&bytes).as_bytes() != address.as_bytes() {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "block blob recomputed address does not match the requested address",
            ));
        }
        Ok(bytes)
    }
    /// Writes one canonical tree blob at its address (exists → skip).
    fn write_tree_object(&self, address: Digest, bytes: &[u8]) -> Result<()>;
    /// Reads one canonical tree blob by its address.
    ///
    /// 对外语义升级（01 §4.2 / 02 §7.2）：默认走 verified（多算一次
    /// `tree_blob_address` 并比对，不匹配 → `Err(DigestMismatch)`，不修复；
    /// 签名不变）。内部已验路径走 [`Self::read_tree_object_unchecked`]。
    fn read_tree_object(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_tree_object_verified(address)
    }
    /// 整块读（免验）：纯读不重算，供内部已验路径复用避免双 hash（02 §7.2）。
    fn read_tree_object_unchecked(&self, address: Digest) -> Result<Vec<u8>>;
    /// 整块读（默认验）：`*_unchecked` 之后多算一次 `tree_blob_address` 并比对；
    /// 不匹配 → `Err(DigestMismatch)`（不修复，01 §4.2 / §8）。
    fn read_tree_object_verified(&self, address: Digest) -> Result<Vec<u8>> {
        let bytes = self.read_tree_object_unchecked(address)?;
        if tree_blob_address(&bytes).as_bytes() != address.as_bytes() {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "tree blob recomputed address does not match the requested address",
            ));
        }
        Ok(bytes)
    }
    /// Writes one reference-table object at its address (exists → skip).
    fn write_ref_object(&self, address: Digest, bytes: &[u8]) -> Result<()>;
    /// Reads one reference-table object by its address.
    ///
    /// 对外语义升级（01 §4.2 / 02 §7.2）：默认走 verified（多算一次
    /// `ref_table_address` 并比对，不匹配 → `Err(DigestMismatch)`，不修复；
    /// 签名不变）。内部已验路径走 [`Self::read_ref_object_unchecked`]。
    fn read_ref_object(&self, address: Digest) -> Result<Vec<u8>> {
        self.read_ref_object_verified(address)
    }
    /// 整块读（免验）：纯读不重算，供内部已验路径复用避免双 hash（02 §7.2）。
    fn read_ref_object_unchecked(&self, address: Digest) -> Result<Vec<u8>>;
    /// 整块读（默认验）：`*_unchecked` 之后多算一次 `ref_table_address` 并比对；
    /// 不匹配 → `Err(DigestMismatch)`（不修复，01 §4.2 / §8）。
    fn read_ref_object_verified(&self, address: Digest) -> Result<Vec<u8>> {
        let bytes = self.read_ref_object_unchecked(address)?;
        if ref_table_address(&bytes).as_bytes() != address.as_bytes() {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "reference table recomputed address does not match the requested address",
            ));
        }
        Ok(bytes)
    }
    /// Writes one commit object (dedup: exists → skip).
    fn write_commit_object(&self, commit_id: Digest, bytes: &[u8]) -> Result<()>;
    /// Ensures the TB object channels exist for a fresh library.
    fn ensure_tb_dirs(&self) -> Result<()>;
    /// The layout-configured default block materialization
    /// (`_dev/树与桶管道/01-目标与设计.md` §1.11.3).
    fn block_materialization(&self) -> Materialization;
    /// Lists every stored block object as `(address, bytes)` for bucket rebuild.
    fn block_objects(&self) -> Result<Vec<(Digest, Vec<u8>)>>;
    /// Writes the boot record bytes for this library (01 §4-3).
    ///
    /// The boot block is a fixed-name, per-library object: `FlatDir` stores it
    /// at `<root>/tb-boot/boot.ipc`, `SingleFile` stores it as an
    /// `ObjectKind::Boot` object at a fixed id. Layouts that cannot store a
    /// boot record report [`ErrorCode::Unsupported`].
    fn write_boot(&self, bytes: &[u8]) -> Result<()> {
        let _ = bytes;
        Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "boot unsupported by this layout",
        ))
    }
    /// Reads the boot record bytes of this library (01 §4-3).
    ///
    /// A missing or unreadable boot record is a hard
    /// [`ErrorCode::BootstrapIncomplete`] failure; layouts that cannot store a
    /// boot record report [`ErrorCode::Unsupported`].
    fn read_boot(&self) -> Result<Vec<u8>> {
        Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "boot unsupported by this layout",
        ))
    }
    /// The path of a version side-table object (`tb-versions/`, 01 §3-1).
    ///
    /// `FlatDir` stores side-tables as `<root>/tb-versions/{address}.ipc`;
    /// `SingleFile` has no side channel (the objects ride the epoch
    /// object-directory instead) and expresses its object path as the
    /// `path#versions:{address}` marker form, mirroring the other channels.
    fn tb_versions_path(&self, address: Digest) -> PathBuf {
        self.root()
            .join("tb-versions")
            .join(format!("{address}.ipc"))
    }
    /// Writes one version side-table object at its address (dedup: exists →
    /// skip). Layouts that cannot store a side-table report
    /// [`ErrorCode::Unsupported`].
    fn write_versions_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        let _ = (address, bytes);
        Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "version side-table unsupported by this layout",
        ))
    }
    /// Reads one version side-table object by its address. Layouts that cannot
    /// store a side-table report [`ErrorCode::Unsupported`].
    fn read_versions_object(&self, address: Digest) -> Result<Vec<u8>> {
        let _ = address;
        Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "version side-table unsupported by this layout",
        ))
    }
}
