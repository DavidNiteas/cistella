//! Flat-directory layout: a content-addressed flat object library rooted at
//! `library/`.
//!
//! P-IO-7.2 正名 (01 §1.11.1): the type was the v4 table-space carrier
//! `ParquetDirLayout`; its TB object library always stored Arrow IPC objects
//! (`tb-blocks/{address}.bin` = envelope, `encode_batch`/`decode_batch`), so the
//! legacy name is replaced by `FlatDirLayout`, reverting to the "content
//! addressed flat object library" intent. Only the name changed, never the
//! object bytes or the v4 golden values.
//!
//! The inherited v4 table-space surface is the content-addressed commit-tree
//! (frozen §6.3 protocol): immutable blobs `tables/<table_id>-<content_hash>.parquet`,
//! trees `trees/<tree_id>.ipc`, commits `commits/<commit_id>.ipc`, metadata snapshots
//! `metadata/<metadata_ref>/`, and the single mutable visibility point
//! `<root>/committed`. GC is reachability-driven tree compaction.

use super::{
    CommitNode, GcReport, GcRequest, Materialization, PublishPlan, PublishReceipt, StorageKind,
    StorageLayout, TableBytes, TableLocator, TablePayload, TbLayout,
};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::translator::{ObjectSpan, TbChannel};
use crate::fault::{FaultPlan, FaultPoint};
use crate::hash::canonical_digest;
use crate::ids::{Digest, TableId};
use crate::ipc::{decode_batch, encode_batch};
use crate::lock::FileLock;
use crate::manifest::{BootstrapImage, Manifest};
use crate::metadata::{encode_table_metadata, parse_table_metadata};
use crate::special::SPECIAL_TABLE_NAMES;
use arrow::array::{ArrayRef, FixedSizeBinaryArray, StringArray, UInt64Array};
use arrow::buffer::Buffer;
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Directory layout rooted at `library/`.
pub struct FlatDirLayout {
    root: PathBuf,
    fault: FaultPlan,
    block_materialization: Materialization,
}

impl FlatDirLayout {
    /// Creates a layout rooted at a library directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fault: FaultPlan::new(),
            block_materialization: Materialization::Ipc,
        }
    }
    /// Injects a deterministic fault plan for M0 fault tests.
    pub fn with_fault(mut self, fault: FaultPlan) -> Self {
        self.fault = fault;
        self
    }
    /// Sets the layout-configured default block materialization.
    pub fn with_block_materialization(mut self, materialization: Materialization) -> Self {
        self.block_materialization = materialization;
        self
    }
    /// Returns the library directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn blob_dir(&self) -> PathBuf {
        self.root.join("tables")
    }
    fn tree_dir(&self) -> PathBuf {
        self.root.join("trees")
    }
    fn commit_dir(&self) -> PathBuf {
        self.root.join("commits")
    }
    fn metadata_dir(&self) -> PathBuf {
        self.root.join("metadata")
    }
    fn lock_dir(&self) -> PathBuf {
        self.root.join("locks")
    }
    pub(crate) fn tb_blocks_dir(&self) -> PathBuf {
        self.root.join("tb-blocks")
    }
    pub(crate) fn tb_trees_dir(&self) -> PathBuf {
        self.root.join("tb-trees")
    }
    pub(crate) fn tb_refs_dir(&self) -> PathBuf {
        self.root.join("tb-refs")
    }
    pub(crate) fn tb_versions_dir(&self) -> PathBuf {
        self.root.join("tb-versions")
    }
    pub(crate) fn tb_boot_dir(&self) -> PathBuf {
        self.root.join("tb-boot")
    }
    pub(crate) fn tb_boot_path(&self) -> PathBuf {
        self.tb_boot_dir().join("boot.ipc")
    }
    pub(crate) fn committed_path(&self) -> PathBuf {
        self.root.join("committed")
    }
    fn blob_path(&self, table_id: TableId, content_hash: Digest) -> PathBuf {
        self.blob_dir()
            .join(format!("{table_id}-{content_hash}.parquet"))
    }
    fn tree_path(&self, tree_id: Digest) -> PathBuf {
        self.tree_dir().join(format!("{tree_id}.ipc"))
    }
    fn metadata_snapshot_dir(&self, metadata_ref: &str) -> PathBuf {
        self.metadata_dir().join(metadata_ref)
    }
    fn lock_dir_path(&self) -> PathBuf {
        self.lock_dir().join("metadata.lock")
    }
    fn table_lock(&self, table_id: TableId) -> Result<FileLock> {
        FileLock::try_exclusive(self.lock_dir().join(format!("{table_id}.lock")))
    }
    fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.blob_dir(),
            self.tree_dir(),
            self.commit_dir(),
            self.metadata_dir(),
            self.lock_dir(),
        ] {
            fs::create_dir_all(&dir).map_err(io_error)?;
        }
        Ok(())
    }
    fn read_tree(&self, path: &Path) -> Result<Vec<(TableId, Digest)>> {
        let batch = decode_batch(&fs::read(path).map_err(io_error)?)?;
        if batch.num_rows() == 0 {
            return Ok(Vec::new());
        }
        let ids = downcast::<FixedSizeBinaryArray>(&batch, 0)?;
        let hashes = downcast::<FixedSizeBinaryArray>(&batch, 1)?;
        let mut rows = Vec::new();
        for index in 0..batch.num_rows() {
            let id = ids
                .value(index)
                .try_into()
                .map_err(|_| corrupt("tree table_id length"))?;
            let hash = hashes
                .value(index)
                .try_into()
                .map_err(|_| corrupt("tree content_hash length"))?;
            rows.push((TableId::from_bytes(id), Digest::from_bytes(hash)));
        }
        Ok(rows)
    }
    fn write_blob_dedup(
        &self,
        table_id: TableId,
        content_hash: Digest,
        ipc_bytes: &[u8],
    ) -> Result<()> {
        let path = self.blob_path(table_id, content_hash);
        if path.exists() {
            return Ok(());
        }
        let batch = decode_batch(ipc_bytes)?;
        write_parquet_atomic(&path, &batch)
    }
    fn write_tree(&self, tree_id: Digest, rows: &[(TableId, Digest)]) -> Result<()> {
        let path = self.tree_path(tree_id);
        if path.exists() {
            return Ok(());
        }
        let batch = RecordBatch::try_new(
            object_schema("tree"),
            vec![
                Arc::new(fixed16(rows.iter().map(|(id, _)| id.as_bytes()).collect())) as ArrayRef,
                Arc::new(fixed16(
                    rows.iter().map(|(_, hash)| hash.as_bytes()).collect(),
                )),
            ],
        )
        .map_err(|error| corrupt(error.to_string()))?;
        atomic_write_bytes(&path, &encode_batch(&batch)?)
    }
    fn write_commit(
        &self,
        commit_id: Digest,
        sequence: u64,
        parent: [u8; 16],
        tree_id: Digest,
        metadata_ref: &str,
    ) -> Result<()> {
        let path = self.commit_path(commit_id);
        if path.exists() {
            return Ok(());
        }
        let batch = RecordBatch::try_new(
            object_schema("commit"),
            vec![
                Arc::new(UInt64Array::from(vec![sequence])),
                Arc::new(fixed16(vec![parent])),
                Arc::new(fixed16(vec![tree_id.as_bytes()])),
                Arc::new(StringArray::from(vec![metadata_ref])),
            ],
        )
        .map_err(|error| corrupt(error.to_string()))?;
        atomic_write_bytes(&path, &encode_batch(&batch)?)
    }
    fn write_metadata_snapshot(&self, tables: &BTreeMap<String, RecordBatch>) -> Result<Digest> {
        let meta_ref = metadata_ref(tables)?;
        let dir = self.metadata_snapshot_dir(&meta_ref.to_string());
        if dir.exists() {
            return Ok(meta_ref);
        }
        fs::create_dir_all(&dir).map_err(io_error)?;
        for name in SPECIAL_TABLE_NAMES {
            let batch = tables.get(name).ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::BootstrapIncomplete,
                    "special table is absent from metadata snapshot",
                )
                .with_context("table", name)
            })?;
            write_parquet_atomic(&dir.join(format!("{name}.parquet")), batch)?;
        }
        Ok(meta_ref)
    }
    fn collect_reachable(
        &self,
        head_commit_id: Digest,
        keep_from_sequence: u64,
    ) -> Result<Reachable> {
        let mut reachable = Reachable::default();
        let mut stack = vec![head_commit_id];
        while let Some(id) = stack.pop() {
            if !reachable.commits.insert(id.as_bytes()) {
                continue;
            }
            let commit = self.read_commit(&self.commit_path(id))?;
            reachable.trees.insert(commit.tree_id);
            reachable.metadata.insert(commit.metadata_ref.clone());
            for (table_id, hash) in
                self.read_tree(&self.tree_path(Digest::from_bytes(commit.tree_id)))?
            {
                reachable.blobs.insert((table_id, hash));
            }
            if let Some(tb) = commit.tb {
                reachable.tb_trees.insert(tb.tree_blob);
                reachable.tb_refs.insert(tb.refs);
                if let Some(versions) = tb.versions {
                    // The version side-table is a tree companion: reachable
                    // through the commit's ninth column (01 §3-1).
                    reachable.tb_versions.insert(versions);
                }
                // GC never decodes tree bytes: block reachability is derived
                // solely from the three-column reference table (single hop).
                let ref_table_path = self.tb_ref_path(Digest::from_bytes(tb.refs));
                if ref_table_path.exists() {
                    let rows = crate::layout::tb::decode_ref_table(
                        &fs::read(&ref_table_path).map_err(io_error)?,
                    )?;
                    for row in rows {
                        reachable.tb_blobs.insert(row.address);
                    }
                }
            }
            if commit.parent != [0; 16] {
                let parent_path = self.commit_path(Digest::from_bytes(commit.parent));
                if parent_path.exists() {
                    let parent = self.read_commit(&parent_path)?;
                    if parent.sequence >= keep_from_sequence {
                        stack.push(Digest::from_bytes(commit.parent));
                    }
                }
            }
        }
        Ok(reachable)
    }
}

impl StorageLayout for FlatDirLayout {
    fn create(&self, bootstrap: &BootstrapImage) -> Result<()> {
        let _lock = self.metadata_lock()?;
        if self.committed_path().exists() {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "flat-dir target already exists",
            )
            .with_context("path", self.root.display().to_string()));
        }
        self.ensure_dirs()?;
        self.commit_snapshot(bootstrap, 0, [0; 16], Vec::new())?;
        Ok(())
    }
    fn open_bootstrap(&self) -> Result<BootstrapImage> {
        let (head, _) = self.read_head()?.ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "flat-dir has no committed head",
            )
        })?;
        let commit = self.read_commit(&self.commit_path(head))?;
        let snapshot_dir = self.metadata_snapshot_dir(&commit.metadata_ref);
        let mut tables = BTreeMap::new();
        for name in SPECIAL_TABLE_NAMES {
            let path = snapshot_dir.join(format!("{name}.parquet"));
            let batch = read_parquet(&path, Some(crate::special::schema(name)?))?;
            tables.insert(name.to_owned(), batch);
        }
        let manifest = Manifest::from_batch(&tables["manifest"])?;
        BootstrapImage::from_parts(manifest, tables)
    }
    fn load_table(&self, locator: &TableLocator) -> Result<TableBytes> {
        self.fault.hit(FaultPoint::TableLoad)?;
        let path = locator.path.as_ref().ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::RequiredDataMissing,
                "flat-dir locator has no blob path",
            )
        })?;
        let batch = read_parquet(path, None)?;
        Ok(TableBytes {
            bytes: encode_batch(&batch)?,
            mapped: false,
        })
    }
    fn publish(&self, plan: PublishPlan) -> Result<PublishReceipt> {
        let _lock = self.metadata_lock()?;
        self.fault.hit(FaultPoint::BeforePayload)?;
        let previous = self.read_head()?;
        let parent = previous.map(|(id, _)| id.as_bytes()).unwrap_or([0; 16]);
        let parent_seq = previous.map(|(_, sequence)| sequence).unwrap_or(0);
        if plan.sequence <= parent_seq {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "publication sequence is not monotonic",
            ));
        }
        let mut full_tree = match previous {
            Some((head, _)) => self
                .read_tree(&self.tree_path(Digest::from_bytes(
                    self.read_commit(&self.commit_path(head))?.tree_id,
                )))?
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            None => BTreeMap::new(),
        };
        let mut metadata = parse_table_metadata(&plan.bootstrap.special_tables["table-metadata"])?;
        for payload in &plan.table_payloads {
            let content_hash = payload.content_hash.ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::RequiredDataMissing,
                    "flat-dir publish requires a content hash",
                )
            })?;
            let _table_lock = self.table_lock(payload.table_id)?;
            self.write_blob_dedup(payload.table_id, content_hash, &payload.bytes)?;
            if let Some(record) = metadata
                .iter_mut()
                .find(|record| record.table_id == payload.table_id)
            {
                record.storage_path = Some(
                    self.blob_path(payload.table_id, content_hash)
                        .display()
                        .to_string(),
                );
            }
            full_tree.insert(payload.table_id, content_hash);
        }
        let mut bootstrap = plan.bootstrap;
        if !metadata.is_empty() {
            bootstrap = bootstrap.with_special_tables([(
                "table-metadata".to_owned(),
                encode_table_metadata(&metadata)?,
            )])?;
        }
        let meta_ref = self.write_metadata_snapshot(&bootstrap.special_tables)?;
        let rows = full_tree
            .iter()
            .map(|(id, hash)| (*id, *hash))
            .collect::<Vec<_>>();
        let tree_id = tree_id(&rows);
        self.write_tree(tree_id, &rows)?;
        let commit = commit_id(plan.sequence, parent, tree_id, meta_ref);
        self.write_commit(
            commit,
            plan.sequence,
            parent,
            tree_id,
            &meta_ref.to_string(),
        )?;
        self.fault.hit(FaultPoint::BeforeManifest)?;
        self.write_committed(commit, plan.sequence)?;
        let table_locators = plan
            .table_payloads
            .iter()
            .map(|payload| TableLocator {
                table_id: payload.table_id,
                offset: 0,
                length: 0,
                storage_kind: StorageKind::FlatDir,
                path: Some(self.blob_path(
                    payload.table_id,
                    payload.content_hash.unwrap_or(Digest::from_bytes([0; 16])),
                )),
            })
            .collect();
        Ok(PublishReceipt {
            sequence: plan.sequence,
            bootstrap_locator: TableLocator {
                table_id: TableId::from_bytes([0; 16]),
                offset: 0,
                length: 0,
                storage_kind: StorageKind::FlatDir,
                path: Some(self.committed_path()),
            },
            table_locators,
        })
    }
    fn gc(&self, request: GcRequest) -> Result<GcReport> {
        let _lock = self.metadata_lock()?;
        let Some((head, _)) = self.read_head()? else {
            return Ok(GcReport { reclaimed: 0 });
        };
        let reachable = self.collect_reachable(head, request.keep_from_sequence)?;
        let mut reclaimed = 0_u64;
        reclaimed += sweep_blobs(&self.blob_dir(), &reachable.blobs)?;
        reclaimed += sweep_ids(&self.tree_dir(), &reachable.trees)?;
        reclaimed += sweep_ids(&self.commit_dir(), &reachable.commits)?;
        reclaimed += sweep_metadata(&self.metadata_dir(), &reachable.metadata)?;
        reclaimed += crate::layout::tb::sweep_tb_blobs(&self.tb_blocks_dir(), &reachable.tb_blobs)?;
        reclaimed += sweep_ids(&self.tb_trees_dir(), &reachable.tb_trees)?;
        reclaimed += sweep_ids(&self.tb_refs_dir(), &reachable.tb_refs)?;
        reclaimed += sweep_ids(&self.tb_versions_dir(), &reachable.tb_versions)?;
        Ok(GcReport { reclaimed })
    }
    fn kind(&self) -> StorageKind {
        StorageKind::FlatDir
    }
}

impl TbLayout for FlatDirLayout {
    fn metadata_lock(&self) -> Result<FileLock> {
        FileLock::try_exclusive(self.lock_dir_path())
    }
    fn read_head(&self) -> Result<Option<(Digest, u64)>> {
        let path = self.committed_path();
        if !path.exists() {
            return Ok(None);
        }
        let batch = decode_batch(&fs::read(path).map_err(io_error)?)?;
        let heads = batch
            .column(0)
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| corrupt("committed head_commit_id is not fixed binary"))?;
        let seqs = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| corrupt("committed sequence is not u64"))?;
        if batch.num_rows() != 1 {
            return Err(corrupt("committed must be a single row"));
        }
        let head = heads
            .value(0)
            .try_into()
            .map_err(|_| corrupt("committed head id length"))?;
        Ok(Some((Digest::from_bytes(head), seqs.value(0))))
    }
    fn write_committed(&self, commit_id: Digest, sequence: u64) -> Result<()> {
        let batch = RecordBatch::try_new(
            object_schema("committed"),
            vec![
                Arc::new(fixed16(vec![commit_id.as_bytes()])),
                Arc::new(UInt64Array::from(vec![sequence])),
            ],
        )
        .map_err(|error| corrupt(error.to_string()))?;
        atomic_write_synced(&self.committed_path(), &encode_batch(&batch)?)
    }
    fn read_commit(&self, path: &Path) -> Result<CommitNode> {
        let batch = decode_batch(&fs::read(path).map_err(io_error)?)?;
        let seq = downcast::<UInt64Array>(&batch, 0)?.value(0);
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
            sequence: seq,
            parent,
            tree_id,
            metadata_ref,
            tb,
        })
    }
    fn commit_path(&self, commit_id: Digest) -> PathBuf {
        self.commit_dir().join(format!("{commit_id}.ipc"))
    }
    fn tb_block_path(&self, address: Digest) -> PathBuf {
        self.tb_blocks_dir().join(format!("{address}.bin"))
    }
    fn tb_tree_path(&self, address: Digest) -> PathBuf {
        self.tb_trees_dir().join(format!("{address}.ipc"))
    }
    fn tb_ref_path(&self, address: Digest) -> PathBuf {
        self.tb_refs_dir().join(format!("{address}.ipc"))
    }
    fn root(&self) -> &Path {
        &self.root
    }
    fn write_block_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        let path = self.tb_block_path(address);
        if path.exists() {
            return Ok(());
        }
        atomic_write_bytes(&path, bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）：纯 `fs::read`，不重算地址。
    /// 默认 verified 路径（`read_block_object`）由 trait default 提供。
    fn read_block_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        let path = self.tb_block_path(address);
        read_object(&path, ErrorCode::DanglingReference)
    }
    fn write_tree_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        let path = self.tb_tree_path(address);
        if path.exists() {
            return Ok(());
        }
        atomic_write_bytes(&path, bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）。
    fn read_tree_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        read_object(&self.tb_tree_path(address), ErrorCode::StorageCorrupt)
    }
    fn write_ref_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        let path = self.tb_ref_path(address);
        if path.exists() {
            return Ok(());
        }
        atomic_write_bytes(&path, bytes)
    }
    /// 整块读（免验，01 §4.2 / 02 §7.2）。
    fn read_ref_object_unchecked(&self, address: Digest) -> Result<Vec<u8>> {
        read_object(&self.tb_ref_path(address), ErrorCode::StorageCorrupt)
    }
    fn tb_versions_path(&self, address: Digest) -> PathBuf {
        self.tb_versions_dir().join(format!("{address}.ipc"))
    }
    fn write_versions_object(&self, address: Digest, bytes: &[u8]) -> Result<()> {
        let path = self.tb_versions_path(address);
        if path.exists() {
            return Ok(());
        }
        atomic_write_bytes(&path, bytes)
    }
    fn read_versions_object(&self, address: Digest) -> Result<Vec<u8>> {
        read_object(&self.tb_versions_path(address), ErrorCode::StorageCorrupt)
    }
    fn write_commit_object(&self, commit_id: Digest, bytes: &[u8]) -> Result<()> {
        let path = self.commit_path(commit_id);
        if path.exists() {
            return Ok(());
        }
        atomic_write_bytes(&path, bytes)
    }
    fn ensure_tb_dirs(&self) -> Result<()> {
        for dir in [
            self.tb_blocks_dir(),
            self.tb_trees_dir(),
            self.tb_refs_dir(),
            self.tb_versions_dir(),
            self.tb_boot_dir(),
        ] {
            fs::create_dir_all(&dir).map_err(io_error)?;
        }
        Ok(())
    }
    /// Writes the boot record at the fixed name `<root>/tb-boot/boot.ipc`
    /// (01 §4-3), atomically like the `committed` visibility switch.
    fn write_boot(&self, bytes: &[u8]) -> Result<()> {
        atomic_write_synced(&self.tb_boot_path(), bytes)
    }
    /// Reads the boot record bytes from `<root>/tb-boot/boot.ipc`; a missing
    /// record is a hard [`ErrorCode::BootstrapIncomplete`] failure.
    fn read_boot(&self) -> Result<Vec<u8>> {
        let path = self.tb_boot_path();
        fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TreeSpaceError::new(ErrorCode::BootstrapIncomplete, "TB boot record is missing")
                    .with_context("path", path.display().to_string())
            } else {
                TreeSpaceError::new(ErrorCode::StorageCorrupt, "TB flat-dir boot read failed")
                    .with_context("detail", error.to_string())
            }
        })
    }
    fn block_materialization(&self) -> Materialization {
        self.block_materialization
    }
    fn block_objects(&self) -> Result<Vec<(Digest, Vec<u8>)>> {
        let dir = self.tb_blocks_dir();
        let mut objects = Vec::new();
        if !dir.exists() {
            return Ok(objects);
        }
        for entry in fs::read_dir(&dir).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if !entry.file_type().map_err(io_error)?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".bin") else {
                continue;
            };
            let Some(address) = hex_to_bytes(stem) else {
                continue;
            };
            // 免验通道：restore_bucket_from_refs / assert_integrity 会在读后
            // 各自做一次地址比对（02 §7.2 免双算——此处若走默认 verified 会
            // 重复 hash）。
            let bytes = self.read_block_object_unchecked(Digest::from_bytes(address))?;
            objects.push((Digest::from_bytes(address), bytes));
        }
        Ok(objects)
    }
}

impl ObjectSpan for FlatDirLayout {
    fn object_span(
        &self,
        channel: TbChannel,
        address: Digest,
    ) -> Result<Option<(PathBuf, u64, u64)>> {
        // An object file is the whole object: offset 0, length = file length
        // (the mmap is sliced at its actual mapped length by the caller).
        let path = match channel {
            TbChannel::Block => self.tb_block_path(address),
            TbChannel::Tree => self.tb_tree_path(address),
            TbChannel::Ref => self.tb_ref_path(address),
        };
        if !path.exists() {
            return Ok(None);
        }
        let length = std::fs::metadata(&path).map_err(io_error)?.len();
        Ok(Some((path, 0, length)))
    }
}

impl FlatDirLayout {
    /// Writes the genesis/idempotent commit snapshot for create().
    fn commit_snapshot(
        &self,
        bootstrap: &BootstrapImage,
        sequence: u64,
        parent: [u8; 16],
        payloads: Vec<TablePayload>,
    ) -> Result<()> {
        let mut full_tree = BTreeMap::new();
        let mut metadata = parse_table_metadata(&bootstrap.special_tables["table-metadata"])?;
        for payload in payloads {
            let content_hash = payload.content_hash.unwrap_or(Digest::from_bytes([0; 16]));
            self.write_blob_dedup(payload.table_id, content_hash, &payload.bytes)?;
            if let Some(record) = metadata
                .iter_mut()
                .find(|record| record.table_id == payload.table_id)
            {
                record.storage_path = Some(
                    self.blob_path(payload.table_id, content_hash)
                        .display()
                        .to_string(),
                );
            }
            full_tree.insert(payload.table_id, content_hash);
        }
        let mut final_bootstrap = bootstrap.clone();
        if !metadata.is_empty() {
            final_bootstrap = final_bootstrap.with_special_tables([(
                "table-metadata".to_owned(),
                encode_table_metadata(&metadata)?,
            )])?;
        }
        let meta_ref = self.write_metadata_snapshot(&final_bootstrap.special_tables)?;
        let rows = full_tree
            .iter()
            .map(|(id, hash)| (*id, *hash))
            .collect::<Vec<_>>();
        let tree_id = tree_id(&rows);
        self.write_tree(tree_id, &rows)?;
        let commit = commit_id(sequence, parent, tree_id, meta_ref);
        self.write_commit(commit, sequence, parent, tree_id, &meta_ref.to_string())?;
        self.write_committed(commit, sequence)
    }
}

#[derive(Default)]
struct Reachable {
    commits: BTreeSet<[u8; 16]>,
    trees: BTreeSet<[u8; 16]>,
    metadata: BTreeSet<String>,
    blobs: BTreeSet<(TableId, Digest)>,
    tb_blobs: BTreeSet<[u8; 16]>,
    tb_trees: BTreeSet<[u8; 16]>,
    tb_refs: BTreeSet<[u8; 16]>,
    tb_versions: BTreeSet<[u8; 16]>,
}

fn tree_id(rows: &[(TableId, Digest)]) -> Digest {
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|(id, _)| *id);
    canonical_digest(
        b"tree",
        sorted.into_iter().map(|(id, hash)| {
            let mut part = id.as_bytes().to_vec();
            part.extend_from_slice(&hash.as_bytes());
            part
        }),
    )
}

/// Public §6.3 golden surface: tree id derivation is frozen.
pub fn tree_id_golden(rows: &[(TableId, Digest)]) -> Digest {
    tree_id(rows)
}

/// Public §6.3 golden surface: commit id derivation is frozen.
pub fn commit_id_golden(
    sequence: u64,
    parent: [u8; 16],
    tree_id: Digest,
    metadata_ref: Digest,
) -> Digest {
    commit_id(sequence, parent, tree_id, metadata_ref)
}

/// Public §6.3 golden surface: the Arrow IPC schema bytes of an object type.
pub fn object_schema_ipc(kind: &str) -> Result<Vec<u8>> {
    use arrow::ipc::convert::IpcSchemaEncoder;
    Ok(IpcSchemaEncoder::new()
        .schema_to_fb(object_schema(kind).as_ref())
        .finished_data()
        .to_vec())
}

pub(crate) fn metadata_ref(tables: &BTreeMap<String, RecordBatch>) -> Result<Digest> {
    let mut names = tables.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let mut parts = Vec::new();
    for name in names {
        let mut part = name.as_bytes().to_vec();
        part.extend_from_slice(&encode_batch(&tables[&name])?);
        parts.push(part);
    }
    Ok(canonical_digest(b"metadata", parts))
}

fn commit_id(sequence: u64, parent: [u8; 16], tree_id: Digest, metadata_ref: Digest) -> Digest {
    canonical_digest(
        b"commit",
        vec![
            sequence.to_le_bytes().to_vec(),
            parent.to_vec(),
            tree_id.as_bytes().to_vec(),
            metadata_ref.as_bytes().to_vec(),
        ],
    )
}

fn object_schema(kind: &str) -> SchemaRef {
    let fields = match kind {
        "tree" => vec![
            Field::new("table_id", DataType::FixedSizeBinary(16), false),
            Field::new("content_hash", DataType::FixedSizeBinary(16), false),
        ],
        "commit" => vec![
            Field::new("sequence", DataType::UInt64, false),
            Field::new("parent", DataType::FixedSizeBinary(16), false),
            Field::new("tree_id", DataType::FixedSizeBinary(16), false),
            Field::new("metadata_ref", DataType::Utf8, false),
        ],
        "committed" => vec![
            Field::new("head_commit_id", DataType::FixedSizeBinary(16), false),
            Field::new("sequence", DataType::UInt64, false),
        ],
        _ => unreachable!("unknown object schema"),
    };
    Arc::new(Schema::new(fields))
}

fn fixed16(values: Vec<[u8; 16]>) -> FixedSizeBinaryArray {
    if values.is_empty() {
        FixedSizeBinaryArray::new(16, Buffer::from(Vec::<u8>::new()), None)
    } else {
        FixedSizeBinaryArray::try_from_iter(values.into_iter().map(|value| value.to_vec()))
            .expect("16-byte values are valid fixed binary")
    }
}

pub(crate) fn hex_to_bytes(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut out = [0_u8; 16];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out[index] = (high << 4) | low;
    }
    Some(out)
}

pub(crate) fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn sweep_blobs(dir: &Path, keep: &BTreeSet<(TableId, Digest)>) -> Result<u64> {
    let mut reclaimed = 0;
    if !dir.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".parquet") else {
            continue;
        };
        let mut parts = stem.splitn(3, '-');
        let (Some(tid_hex), Some(ch_hex)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Some(tid), Some(ch)) = (hex_to_bytes(tid_hex), hex_to_bytes(ch_hex)) else {
            continue;
        };
        if !keep.contains(&(TableId::from_bytes(tid), Digest::from_bytes(ch))) {
            fs::remove_file(entry.path()).map_err(io_error)?;
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

pub(crate) fn sweep_ids(dir: &Path, keep: &BTreeSet<[u8; 16]>) -> Result<u64> {
    let mut reclaimed = 0;
    if !dir.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".ipc") else {
            continue;
        };
        let Some(id) = hex_to_bytes(stem) else {
            continue;
        };
        if !keep.contains(&id) {
            fs::remove_file(entry.path()).map_err(io_error)?;
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

fn sweep_metadata(dir: &Path, keep: &BTreeSet<String>) -> Result<u64> {
    let mut reclaimed = 0;
    if !dir.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            fs::remove_dir_all(entry.path()).map_err(io_error)?;
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

fn write_parquet_atomic(path: &Path, batch: &RecordBatch) -> Result<()> {
    let temporary = temporary_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), None).map_err(parquet_error)?;
    writer.write(batch).map_err(parquet_error)?;
    writer.close().map_err(parquet_error)?;
    replace(temporary, path.to_path_buf())
}

fn read_parquet(path: &Path, fallback_schema: Option<SchemaRef>) -> Result<RecordBatch> {
    let file = File::open(path).map_err(io_error)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_error)?;
    let embedded_schema = builder.schema().clone();
    let reader = builder.build().map_err(parquet_error)?;
    let batches = reader
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(parquet_error)?;
    if batches.is_empty() {
        // An empty table is a legal state (e.g. an mzML file with no
        // chromatograms). The Parquet writer emits only a schema for zero-row
        // batches, so reconstruct the empty batch from the embedded Arrow
        // schema rather than treating it as corruption.
        let schema = match fallback_schema {
            Some(schema) => schema,
            None => embedded_schema,
        };
        return Ok(RecordBatch::new_empty(schema));
    }
    if batches.len() == 1 {
        return Ok(batches.into_iter().next().expect("checked length"));
    }
    // Large tables are written as multiple Parquet row groups and read back
    // as several batches; concat them back into the single-batch contract.
    let schema = batches[0].schema();
    arrow::compute::concat_batches(&schema, &batches).map_err(parquet_error)
}

/// Reads a TB object file, mapping a missing file to `missing_code` and every
/// other I/O failure to the storage-corrupt category.
fn read_object(path: &Path, missing_code: ErrorCode) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            TreeSpaceError::new(missing_code, "TB object is missing")
                .with_context("path", path.display().to_string())
        } else {
            TreeSpaceError::new(ErrorCode::StorageCorrupt, "TB flat-dir I/O failed")
                .with_context("detail", error.to_string())
        }
    })
}

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    replace(temporary, path.to_path_buf())
}

pub(crate) fn atomic_write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    replace(temporary, path.to_path_buf())
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("new")
    ))
}

fn replace(temporary: PathBuf, path: PathBuf) -> Result<()> {
    fs::rename(&temporary, &path).map_err(|error| {
        TreeSpaceError::new(
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                ErrorCode::ReplaceBlocked
            } else {
                ErrorCode::StorageCorrupt
            },
            "atomic replacement failed; no in-place fallback is used",
        )
        .with_context("temporary", temporary.display().to_string())
        .with_context("target", path.display().to_string())
        .with_context("detail", error.to_string())
    })
}

fn downcast<'a, T: arrow::array::Array + 'static>(
    batch: &'a RecordBatch,
    index: usize,
) -> Result<&'a T> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| corrupt("object column has incorrect Arrow type"))
}

fn corrupt(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, message.into())
}

fn io_error(error: std::io::Error) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, "flat-dir I/O failed")
        .with_context("detail", error.to_string())
}

fn parquet_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "Parquet Arrow conversion failed",
    )
    .with_context("detail", error.to_string())
}
