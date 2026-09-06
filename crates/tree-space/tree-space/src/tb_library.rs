//! Tree-and-bucket library handle.
//!
//! A [`TbLibrary`] is the disk-facing handle of the tree-and-bucket model
//! (a tree instance plus its bucket, `_dev/树与桶管道/01-目标与设计.md` §1.1). It
//! layers the TB channels (`tb-blocks/`, `tb-trees/`, `tb-refs/`,
//! `tb-versions/`) and the nine-column commit extension (with the P-IO-5
//! `tb_pruned` pruning record and the PL-1 S5 `tb_versions` side-table pointer)
//! on top of the v4 content-addressed commit-tree layout. Pure v4 publication
//! is never touched by this type.
//!
//! The handle is decoupled from any concrete disk layout through the unified
//! [`TbLayout`] data interface (P-IO-7.1); the first layout instance is
//! [`FlatDirLayout`]. Block objects are written through a
//! [`BlockMaterializer`] selected by the layout's configured default plus the
//! block-kind constraint (01 §1.11.3), and read back by probing the object's
//! payload magic (01 §1.11.4); every committed leaf's disk version is recorded
//! in the per-commit version side-table (01 §3-1). The commit entry point
//! takes the *encoded* canonical tree bytes together with the commit-form
//! tree's leaf references, so both typed (`encode_node` + `leaf_refs`) and
//! dynamic (`to_image` + `encode` + `leaf_refs`) trees commit through one
//! path. Pruning commits take the full image children plus the ephemeral
//! prefix bytes and derive both the encoded bytes and the leaf set from the
//! pruned form.

use crate::block::{BlockKind, RefId};
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::Digest;
use crate::index::{XPathIndex, xpath_from_canonical_bytes};
use crate::layout::flat_dir::FlatDirLayout;
use crate::layout::materializer::{probe_block_materialization, select_materializer};
use crate::layout::single_file::SingleFileLayout;
use crate::layout::tb::{
    VersionRow, block_blob_address, decode_ref_table, decode_versions_table, encode_ref_table,
    encode_versions_table, hex16, ref_table_address, tree_blob_address, versions_table_address,
};
use crate::layout::{
    GcReport, GcRequest, Materialization, RefRow, StorageLayout, TbLayout, TbPointers,
};
use crate::manifest::BootstrapImage;
use crate::plugin::boot::{BOOT_VERSION, BootRecord, decode_boot, encode_boot};
use crate::plugin::degraded::{DegradedBlock, DiskVersion};
use crate::plugin::registry::PluginRegistry;
use crate::plugin::version::{DiskLayout, MemLayout};
use crate::tree::codec::{DecodeTree, ImageField, TreeImage, decode, tree_id};
use crate::xpath::XPath;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// A tree-and-bucket library persisted in a flat directory.
///
/// `L` is the disk layout backing the library (P-IO-7.1 统御). The default is
/// the first layout instance, [`FlatDirLayout`]; every layout must implement
/// the unified [`TbLayout`] data interface.
pub struct TbLibrary<L: TbLayout = FlatDirLayout> {
    layout: L,
    /// The plugin routing registry of this library instance (boot-chain tree
    /// route, restore-path and verify row routes; 01 §3.1). Injected through
    /// [`TbLibrary::create_with_registry`] / [`TbLibrary::open_with_registry`]
    /// or the default [`PluginRegistry::new()`].
    registry: Arc<PluginRegistry>,
    state: Option<LoadedState>,
}

struct LoadedState {
    image: TreeImage,
    bucket: Bucket,
    index: XPathIndex,
}

/// The receipt returned by a successful TB commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TbCommitReceipt {
    /// The layout-local monotonic commit sequence.
    pub sequence: u64,
    /// The content-addressed commit id.
    pub commit_id: Digest,
    /// The root `TreeId` recorded in the commit.
    pub tb_root_tree_id: [u8; 16],
    /// The `tb-trees/` address of the committed tree blob.
    pub tree_blob: [u8; 16],
    /// The `tb-refs/` address of the committed reference table.
    pub refs: [u8; 16],
}

impl TbLibrary<FlatDirLayout> {
    /// Creates a new TB library rooted at `root`, with the built-in empty
    /// bootstrap as the v4 genesis commit and the three TB channels created.
    /// Blocks are materialized as Arrow IPC (the default).
    ///
    /// The instance's plugin routing registry is the default
    /// [`PluginRegistry::new()`] (01 §3.1); [`Self::create_with_registry`]
    /// injects a caller-provided registry instead.
    pub fn create(root: impl Into<PathBuf>) -> Result<Self> {
        Self::create_with_registry(root, Arc::new(PluginRegistry::new()))
    }

    /// Creates a new TB library rooted at `root` whose block materialization
    /// default is `materialization`.
    ///
    /// `ArrowTable` blocks may then be materialized by the Parquet materializer
    /// when `Parquet` is selected; `Blob`/opaque blocks always stay on native
    /// bytes whatever the default (01 §1.11.3). Reopening a library written
    /// under either default always probes each object and needs no same-default
    /// hint.
    ///
    /// The instance's plugin routing registry is the default
    /// [`PluginRegistry::new()`] (01 §3.1).
    pub fn create_with_materialization(
        root: impl Into<PathBuf>,
        materialization: Materialization,
    ) -> Result<Self> {
        Self::create_shared(root, materialization, Arc::new(PluginRegistry::new()))
    }

    /// Creates a new TB library rooted at `root` whose block materialization
    /// default is Arrow IPC (the default), with a caller-provided plugin
    /// routing registry injected into the instance (01 §3.1/§4).
    ///
    /// The injected registry is the instance's plugin route source: the boot
    /// chain's tree-plugin route, the restore path's per-row block route and
    /// `verify`'s row route all go through it (01 §3.2). A registry missing
    /// built-in plugins reproduces the existing `BootstrapIncomplete` /
    /// `DegradedBlock` semantics on the injected surface (01 §3.4).
    pub fn create_with_registry(
        root: impl Into<PathBuf>,
        registry: Arc<PluginRegistry>,
    ) -> Result<Self> {
        Self::create_shared(root, Materialization::Ipc, registry)
    }

    /// The shared flat-dir create path (01 §3.1): provisions the v4 genesis
    /// layout, the three TB channels and the boot record, then returns the
    /// instance with `registry` as its plugin route source. The default
    /// constructors delegate here with [`PluginRegistry::new()`].
    fn create_shared(
        root: impl Into<PathBuf>,
        materialization: Materialization,
        registry: Arc<PluginRegistry>,
    ) -> Result<Self> {
        let layout = FlatDirLayout::new(root).with_block_materialization(materialization);
        let bootstrap = BootstrapImage::built_in()?;
        layout.create(&bootstrap)?;
        layout.ensure_tb_dirs()?;
        layout.write_boot(&encode_boot(&BootRecord {
            boot_version: BOOT_VERSION,
            library_id: "tree-space".to_owned(),
            tree_mem_version: MemLayout::Arrow55.as_str().to_owned(),
            tree_disk_version: DiskLayout::ArrowIpc.as_str().to_owned(),
        })?)?;
        Ok(Self {
            layout,
            registry,
            state: None,
        })
    }

    /// Opens an existing TB library and restores its committed state.
    ///
    /// The head commit must be a TB commit. Restoring performs: read
    /// `committed` → append columns → `tb-trees/` tree bytes → [`TreeImage`],
    /// bucket rebuild from `tb-blocks/` (with address-forgery detection and
    /// format-probe decode), and `XPathIndex` rebuild from the `tb-refs/`
    /// reference table.
    ///
    /// The instance's plugin routing registry is the default
    /// [`PluginRegistry::new()`] (01 §3.1); [`Self::open_with_registry`]
    /// injects a caller-provided registry instead.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_registry(root, Arc::new(PluginRegistry::new()))
    }

    /// Opens an existing TB library and restores its committed state with a
    /// caller-provided plugin routing registry injected into the instance
    /// (01 §3.1/§4).
    ///
    /// Restore semantics equal [`Self::open`] on the injected surface: the
    /// boot chain's tree-plugin route, the restore path's per-row block route
    /// and `verify`'s row route all go through `registry` (01 §3.2). A registry
    /// missing the boot tree plugin reproduces `BootstrapIncomplete`; a
    /// registry missing a block plugin reproduces the `DegradedBlock`
    /// downgrade on that row (01 §3.4).
    pub fn open_with_registry(
        root: impl Into<PathBuf>,
        registry: Arc<PluginRegistry>,
    ) -> Result<Self> {
        Self::open_with(FlatDirLayout::new(root), &registry)
    }
}

/// Construction entry points specific to the single-file layout backend.
///
/// These are exposed through a trait implemented for `TbLibrary<SingleFileLayout>`
/// rather than a second inherent `impl`: a second inherent `create`/`open` on
/// the same generic type would make every bare `TbLibrary::create` call (which
/// relies on the `= FlatDirLayout` default) ambiguous. With the trait, the
/// single-file constructors keep the exact requested call shape
/// (`TbLibrary::<SingleFileLayout>::create` / `open`) while the default
/// flat-dir surface stays byte-call-compatible.
pub trait SingleFileTbLibrary {
    /// Creates a new TB library packaged in the single `.umdb` container at
    /// `path`, with the built-in empty bootstrap as the v4 genesis epoch.
    ///
    /// The single-file layout forces IPC block materialization (01 §1.11.3 /
    /// §1.11.6) and cannot be configured otherwise.
    fn create(path: impl Into<PathBuf>) -> Result<TbLibrary<SingleFileLayout>>;

    /// Creates a new single-file TB library whose block materialization default
    /// is `materialization`.
    ///
    /// Only [`Materialization::Ipc`] is accepted: the single-file container
    /// requires continuous zero-copy-mappable blocks, so every parquet attempt
    /// is rejected here (02 §9.5 force-IPC obligation). Reopening never needs a
    /// materialization hint because every single-file object is IPC and is
    /// probed anyway.
    fn create_with_materialization(
        path: impl Into<PathBuf>,
        materialization: Materialization,
    ) -> Result<TbLibrary<SingleFileLayout>>;

    /// Opens an existing single-file TB library and restores its committed
    /// state from the newest epoch's object-directory snapshot.
    ///
    /// The head commit must be a TB commit; the newest valid trailer wins (crash
    /// recovery falls back to the previous valid epoch). Restoring performs the
    /// same image/bucket/index rebuild as the flat layout.
    fn open(path: impl Into<PathBuf>) -> Result<TbLibrary<SingleFileLayout>>;
}

impl SingleFileTbLibrary for TbLibrary<SingleFileLayout> {
    fn create(path: impl Into<PathBuf>) -> Result<Self> {
        Self::create_with_materialization(path, Materialization::Ipc)
    }
    fn create_with_materialization(
        path: impl Into<PathBuf>,
        materialization: Materialization,
    ) -> Result<Self> {
        if materialization != Materialization::Ipc {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "single-file force-IPC: parquet materialization is rejected on the single-file layout",
            ));
        }
        let layout = SingleFileLayout::new(path);
        let bootstrap = BootstrapImage::built_in()?;
        layout.create(&bootstrap)?;
        layout.ensure_tb_dirs()?;
        layout.write_boot(&encode_boot(&BootRecord {
            boot_version: BOOT_VERSION,
            library_id: "tree-space".to_owned(),
            tree_mem_version: MemLayout::Arrow55.as_str().to_owned(),
            tree_disk_version: DiskLayout::ArrowIpc.as_str().to_owned(),
        })?)?;
        Ok(Self {
            layout,
            // 01 §3.1: the single-file face keeps the default registry (no
            // injection constructor - minimal surface, 02 §7-1).
            registry: Arc::new(PluginRegistry::new()),
            state: None,
        })
    }
    fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with(
            SingleFileLayout::new(path),
            &Arc::new(PluginRegistry::new()),
        )
    }
}

impl<L: TbLayout> TbLibrary<L> {
    /// The shared open path: the PL-1 eight-step boot chain (01 §4-3).
    ///
    /// `registry` is the library instance's plugin routing registry (injected
    /// or the default [`PluginRegistry::new()`], 01 §3.1) — every route below
    /// goes through it instead of the process global.
    ///
    /// 1. read the `tb-boot` record and decode it (validates `boot_version`
    ///    and resolves the tree layout-version pair; an unknown tree disk
    ///    version is a hard [`ErrorCode::BootstrapIncomplete`] failure);
    /// 2. route the tree plugin through the instance registry's
    ///    [`PluginRegistry::tree_plugin`] by the boot disk version
    ///    (unreachable → `BootstrapIncomplete`);
    /// 3. `read_head` → `read_commit` → `TbPointers` (the nine-column decode:
    ///    the eighth `tb_pruned` and the ninth `tb_versions` columns land in
    ///    P-IO-5 and S5 respectively);
    /// 4. read the tree bytes (`*_unchecked`), recompute the tree identity
    ///    through the routed tree plugin (M1 = `tree::codec::tree_id`
    ///    forwarding), assert the tree address, and decode through the plugin
    ///    into the [`TreeImage`];
    /// 5. read the reference-table bytes (`*_unchecked`) → `decode_ref_table`
    ///    → rows, and assert the ref-table address; when the commit carries a
    ///    `tb_versions` pointer, read and decode the per-commit version
    ///    side-table into the `xpath → (mem_ver, disk_ver)` map (01 §3-1);
    /// 6. rebuild the bucket ref-table driven
    ///    ([`restore_bucket_from_refs`]): every row restores its addressed
    ///    object through plugin routing; rows with an unknown disk version, no
    ///    routable plugin, or a failing plugin decode become
    ///    [`crate::plugin::degraded::DegradedBlock`]s in the bucket's parallel
    ///    slot (S6, 01 §4-5);
    /// 7. rebuild the `XPathIndex` from the rows (unchanged);
    /// 8. structural-parse entry: the tree-plugin decode of step 4 already
    ///    proved the tree structure is parseable (the verify-side degraded
    ///    semantics land with the bucket rebuild in S6).
    pub(crate) fn open_with(layout: L, registry: &Arc<PluginRegistry>) -> Result<Self> {
        // 1. Boot: fix the tree layout-version pair or fail the bootstrap.
        let boot_bytes = layout.read_boot()?;
        let boot = decode_boot(&boot_bytes)?;
        // `decode_boot` already hard-rejects an unsupported boot version and
        // an unknown tree disk version; the explicit from_str re-derivation
        // keeps the version-pair parse of the boot chain spelled out.
        let tree_disk = DiskLayout::from_str(&boot.tree_disk_version).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "boot tree disk version is not a known disk layout",
            )
            .with_context("tree_disk_version", boot.tree_disk_version.clone())
        })?;

        // 2. Route the tree plugin (unreachable → the tree structure cannot
        //    be parsed at all).
        let tree_plugin = registry.tree_plugin(tree_disk).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "no tree plugin is registered for the boot tree disk version",
            )
            .with_context("tree_disk_version", boot.tree_disk_version.clone())
        })?;

        // 3. Head commit → TbPointers.
        let Some((head, _)) = layout.read_head()? else {
            return Err(TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "TB library has no committed head",
            ));
        };
        let commit = layout.read_commit(&layout.commit_path(head))?;
        let pointers = commit.tb.ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "head commit is not a TB commit",
            )
        })?;

        // 内部已验路径：tree / ref 读走 `*_unchecked`，紧随其后的
        // tree_id 重算 / `assert_tree_address` / `assert_ref_table_address`
        // 各算一次（02 §7.2 免双算）。
        // 4. Tree bytes through the routed tree plugin: identity recomputation
        //    (M1 = codec forwarding), address assertion, and decode.
        let tree_bytes =
            layout.read_tree_object_unchecked(Digest::from_bytes(pointers.tree_blob))?;
        if tree_plugin.tree_id(&tree_bytes).as_bytes() != pointers.root_tree_id {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "tree blob recomputed identity does not match the commit root tree id",
            ));
        }
        assert_tree_address(&tree_bytes, &pointers)?;
        let image = tree_plugin.decode(&tree_bytes)?;

        // 5. Reference-table rows + the version side-table (01 §3-1): the
        //    ninth commit column points at the per-commit side-table; when it
        //    is present the rows label every leaf's (mem_ver, disk_ver) —
        //    an absent pointer (legacy seven/eight-column commit) leaves the
        //    map empty and `restore_bucket_from_refs` falls back to probing.
        let ref_bytes = layout.read_ref_object_unchecked(Digest::from_bytes(pointers.refs))?;
        let rows = decode_ref_table(&ref_bytes)?;
        assert_ref_table_address(&ref_bytes, &pointers)?;
        let versions = read_versions_map(&layout, &pointers)?;

        // 6. Bucket rebuild: ref-table driven (per-row address read + disk
        //    label + plugin route + decode, degraded slot for misses).
        let bucket = restore_bucket_from_refs(&layout, &rows, &versions, registry)?;

        // 7. XPathIndex rebuild (ref table rows, unchanged).
        let mut index = XPathIndex::new();
        for row in &rows {
            let xpath = xpath_from_canonical_bytes(&row.xpath)?;
            index.insert(&xpath, RefId::from_bytes(row.ref_id));
        }

        // 8. Structural-parse check: the tree-plugin decode in step 4 succeeded
        //    — the tree layout equals → the tree structure is parseable (the
        //    verify-side degraded semantics hook up in S6).
        Ok(Self {
            layout,
            registry: registry.clone(),
            state: Some(LoadedState {
                image,
                bucket,
                index,
            }),
        })
    }

    /// Returns the restored tree image, if the library has been opened.
    pub fn tree_image(&self) -> Result<&TreeImage> {
        self.state
            .as_ref()
            .map(|state| &state.image)
            .ok_or_else(|| not_loaded())
    }

    /// Returns the restored bucket, if the library has been opened.
    pub fn bucket(&self) -> Result<&Bucket> {
        self.state
            .as_ref()
            .map(|state| &state.bucket)
            .ok_or_else(|| not_loaded())
    }

    /// Returns the restored xpath index, if the library has been opened.
    pub fn index(&self) -> Result<&XPathIndex> {
        self.state
            .as_ref()
            .map(|state| &state.index)
            .ok_or_else(|| not_loaded())
    }

    /// Projects the restored image into a typed tree, materializing block
    /// leaves through the restored bucket.
    pub fn project<T: DecodeTree>(&self) -> Result<T> {
        let state = self.state.as_ref().ok_or_else(|| not_loaded())?;
        crate::tree::codec::project::<T>(&state.image, &state.bucket)
    }

    /// Commits a tree (its canonical bytes and leaf references) plus its bucket.
    ///
    /// This is the full-tree commit: nothing is pruned and the recorded
    /// `tb_pruned` commit column is null. Prefer [`Self::commit_pruned`] when
    /// an ephemeral subtree should be pruned from the persisted form. The
    /// store order is the mandated one: every reachable block blob envelope is
    /// materialized (IPC or Parquet per the layout default) and written to
    /// `tb-blocks/`, the canonical tree bytes to `tb-trees/`, the derived
    /// three-column reference table to `tb-refs/`, the per-commit version
    /// side-table (01 §3-1) to `tb-versions/`, and only then the commit object
    /// with the `committed` visibility switch.
    pub fn commit(
        &self,
        tree_bytes: &[u8],
        leaf_refs: &[(XPath, RefId)],
        bucket: &Bucket,
    ) -> Result<TbCommitReceipt> {
        self.write_commit(tree_bytes, leaf_refs, bucket, None)
    }

    /// Commits a tree whose ephemeral subtrees are pruned before encoding.
    ///
    /// `fields` are the *full* root image children of the instance (for a typed
    /// tree that is `EncodeTree::tree_children()`, for a dynamic node
    /// `TreeImage::children()`); `pruned` holds the canonical bytes of every
    /// ephemeral prefix (the merged template and runtime prefixes). The image
    /// is pruned under those prefixes, encoded to the committed tree bytes, and
    /// the reference table is derived from the pruned leaf set — pruned blocks
    /// are never written and become unreachable in the same commit. The commit
    /// records the maximally pruned field xpaths in the `tb_pruned` column
    /// (non-null; empty when nothing was pruned). The `tb_commit_id` formula is
    /// unchanged by pruning.
    pub fn commit_pruned(
        &self,
        fields: &[ImageField],
        pruned: &[Vec<u8>],
        bucket: &Bucket,
    ) -> Result<TbCommitReceipt> {
        let (kept, recorded) = crate::layout::tb::prune_image(fields.to_vec(), pruned);
        let image = TreeImage::new(kept);
        let tree_bytes = crate::tree::codec::encode(&image)?;
        let leaf_refs = crate::layout::tb::image_leaf_refs(&image)?;
        self.write_commit(&tree_bytes, &leaf_refs, bucket, Some(&recorded))
    }

    /// The shared commit write path (see [`Self::commit`]/[`Self::commit_pruned`]).
    fn write_commit(
        &self,
        tree_bytes: &[u8],
        leaf_refs: &[(XPath, RefId)],
        bucket: &Bucket,
        pruned: Option<&[Vec<u8>]>,
    ) -> Result<TbCommitReceipt> {
        let _lock = self.layout.metadata_lock()?;
        let Some((head, head_sequence)) = self.layout.read_head()? else {
            return Err(TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "TB commit requires an existing library",
            ));
        };
        let sequence = head_sequence + 1;
        let parent = head.as_bytes();

        // 1. Persist every reachable block object through the materializer
        //    selected by the layout default + block-kind constraint (`exists` →
        //    skip). The address is the physical content hash of the materialized
        //    bytes; the semantic `RefId` from `leaf_refs` is untouched (01
        //    §1.11.4). The per-leaf disk layout is recorded alongside from the
        //    physical bytes (probe == the materializer actually applied), for
        //    the version side-table of step 3.5.
        let mut blob_by_id = BTreeMap::new();
        let mut disk_by_id = BTreeMap::new();
        for (_, ref_id) in leaf_refs {
            if blob_by_id.contains_key(ref_id) {
                continue;
            }
            let envelope = bucket.get(*ref_id).ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::DanglingReference,
                    "tree leaf references a block that is absent from the bucket",
                )
                .with_context("ref_id", ref_id.to_string())
            })?;
            let materializer =
                select_materializer(self.layout.block_materialization(), &envelope.kind);
            let physical = materializer.encode(envelope)?;
            let address = materializer.address(&physical);
            self.layout.write_block_object(address, &physical)?;
            blob_by_id.insert(*ref_id, address.as_bytes());
            // The physical bytes deterministically label the disk layout the
            // materializer actually produced (IPC or Parquet; native bytes
            // stay on the IPC/native path) — identical to the read-side probe.
            let disk = DiskLayout::try_from(probe_block_materialization(&physical)?)
                .expect("ipc/parquet materializations map to the two M1 disk layouts");
            disk_by_id.insert(*ref_id, disk);
        }

        // 2. Persist the canonical tree bytes.
        let tree_address = tree_blob_address(tree_bytes);
        self.layout.write_tree_object(tree_address, tree_bytes)?;
        let root_tree_id = tree_id(tree_bytes).as_bytes();

        // 3. Derive and persist the three-column reference table. Addresses are
        //    the physical persistence addresses of step 1.
        let rows = crate::layout::tb::derive_ref_rows_with_addresses(leaf_refs, bucket, |id| {
            *blob_by_id
                .get(&id)
                .expect("every committed leaf was persisted")
        })?;
        let ref_bytes = encode_ref_table(&rows)?;
        let refs_address = ref_table_address(&ref_bytes);
        self.layout.write_ref_object(refs_address, &ref_bytes)?;

        // 3.5. Derive and persist the per-commit version side-table (01 §3-1):
        //      every reference-table row (already in canonical xpath order)
        //      labeled with the leaf's memory/disk layout pair as actually
        //      materialized in step 1 — M1 memory is always `Arrow55`, the
        //      disk version is per-leaf (IPC or Parquet). The side-table is a
        //      tree companion: its address enters the ninth commit column and
        //      never the `tb_commit_id` domain. A whole-library single disk
        //      version still writes every row explicitly (consistency first,
        //      no sparsing optimization).
        let mut version_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let disk = disk_by_id
                .get(&RefId::from_bytes(row.ref_id))
                .expect("every committed leaf was persisted");
            version_rows.push(VersionRow {
                xpath: row.xpath.clone(),
                mem_ver: MemLayout::Arrow55.as_str().to_owned(),
                disk_ver: disk.as_str().to_owned(),
            });
        }
        let versions_bytes = encode_versions_table(&version_rows)?;
        let versions_address = versions_table_address(&versions_bytes);
        self.layout
            .write_versions_object(versions_address, &versions_bytes)?;

        // 4. Write the commit object and switch the visibility point.
        let commit_id = crate::layout::tb::tb_commit_id(
            sequence,
            parent,
            root_tree_id,
            tree_address.as_bytes(),
            refs_address.as_bytes(),
        );
        let batch = crate::layout::tb::tb_commit_batch(
            sequence,
            parent,
            root_tree_id,
            tree_address.as_bytes(),
            refs_address.as_bytes(),
            pruned,
            Some(versions_address.as_bytes()),
        )?;
        self.layout
            .write_commit_object(commit_id, &crate::ipc::encode_batch(&batch)?)?;
        self.layout.write_committed(commit_id, sequence)?;

        Ok(TbCommitReceipt {
            sequence,
            commit_id,
            tb_root_tree_id: root_tree_id,
            tree_blob: tree_address.as_bytes(),
            refs: refs_address.as_bytes(),
        })
    }

    /// Verifies the committed reference table and tree blob.
    ///
    /// Three-column cross-check: for every reference-table row the blob at its
    /// `address` must exist and its bytes must recompute to the `address`
    /// column (the address assertion is unconditional — degradation never
    /// exempts content integrity). Rows whose disk version is unknown, whose
    /// disk/kind pair has no routable plugin, or whose plugin decode fails are
    /// degraded rows (01 §4-5): only the address is verified and their
    /// canonical identity is not compared (no plugin can decode them back to a
    /// canonical payload); they still count into the returned row total and
    /// the caller observes them through the opened bucket's
    /// [`Bucket::degraded`] slot. Every other row additionally probes its
    /// payload magic and decodes through the matching plugin, its canonical
    /// payload must recompute to the `ref_id` column, the tree blob's
    /// recomputed leaf set must equal the reference-table row set. When the
    /// commit records pruned prefixes, pruning consistency is checked: no
    /// reference-table or recomputed leaf xpath may fall under a recorded
    /// prefix (see `_dev/树与桶管道/01-目标与设计.md` §1.9.8 (e)2).
    pub fn verify(&self) -> Result<usize> {
        let Some((head, _)) = self.layout.read_head()? else {
            return Err(TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "TB verify requires a committed head",
            ));
        };
        let commit = self.layout.read_commit(&self.layout.commit_path(head))?;
        let pointers = commit.tb.ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::BootstrapIncomplete, "head is not a TB commit")
        })?;

        // 内部已验路径：below reads 走 `*_unchecked`，各自的地址 / 身份重算
        // 在本函数内完成（块地址、tree_id + 两个地址重算；02 §7.2 免双算）。
        let ref_bytes = self
            .layout
            .read_ref_object_unchecked(Digest::from_bytes(pointers.refs))?;
        let rows = decode_ref_table(&ref_bytes)?;
        let versions = read_versions_map(&self.layout, &pointers)?;

        let mut checked = 0_usize;
        let mut row_set = std::collections::BTreeSet::new();
        for row in &rows {
            let blob_bytes = self
                .layout
                .read_block_object_unchecked(Digest::from_bytes(row.address))?;
            // Unconditional address assertion (hash(raw) is always recomputed;
            // a degraded block never exempts content integrity).
            let address = block_blob_address(&blob_bytes);
            if address.as_bytes() != row.address {
                return Err(TreeSpaceError::new(
                    ErrorCode::DigestMismatch,
                    "blob content hash does not match its address",
                ));
            }
            let kind = frame_kind(&blob_bytes)?;
            // Classify the row exactly like the restore path
            // ([`restore_bucket_from_refs`]): an unknown disk version, a
            // disk/kind pair without a routable plugin, or a failing plugin
            // decode makes the row degraded — its address was verified above
            // and its canonical identity is not compared (no plugin can decode
            // it back to a canonical payload), yet it still enters the
            // leaf-set and the checked total (01 §4-5).
            let healthy = match label_disk_version(&versions, row, &kind, &blob_bytes)? {
                DiskLabel::Unknown(_) => None,
                DiskLabel::Known(disk) => self
                    .registry
                    .route_disk(disk, &kind)
                    .and_then(|plugin| plugin.decode(&blob_bytes).ok()),
            };
            let Some(canonical) = healthy else {
                row_set.insert((row.xpath.clone(), row.ref_id));
                checked += 1;
                continue;
            };
            let computed = crate::block::block_ref_id(canonical.kind.clone(), &canonical.payload);
            if computed.as_bytes() != row.ref_id {
                return Err(TreeSpaceError::new(
                    ErrorCode::DigestMismatch,
                    "envelope recomputed identity does not match the reference-table ref_id",
                ));
            }
            row_set.insert((row.xpath.clone(), row.ref_id));
            checked += 1;
        }

        let tree_bytes = self
            .layout
            .read_tree_object_unchecked(Digest::from_bytes(pointers.tree_blob))?;
        if tree_id(&tree_bytes).as_bytes() != pointers.root_tree_id {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "tree blob recomputed identity does not match the commit root tree id",
            ));
        }
        assert_tree_address(&tree_bytes, &pointers)?;
        let image = decode(&tree_bytes)?;
        let recomputed = crate::layout::tb::image_leaf_refs(&image)?;
        let recomputed_set = recomputed
            .into_iter()
            .map(|(xpath, ref_id)| {
                (
                    crate::index::canonical_xpath_bytes(&xpath),
                    ref_id.as_bytes(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        if recomputed_set != row_set {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "tree blob leaf references do not form the reference-table row set",
            ));
        }

        // 4. Pruning consistency: when the commit records pruned prefixes,
        // neither the reference-table xpaths nor the recomputed leaf set may
        // contain a leaf under a recorded prefix (the pruned tree is clean).
        if let Some(prefixes) = &pointers.pruned {
            if !prefixes.is_empty() {
                for row in &rows {
                    if crate::layout::tb::is_pruned_path(&row.xpath, prefixes) {
                        return Err(TreeSpaceError::new(
                            ErrorCode::DigestMismatch,
                            "reference-table xpath falls under a recorded pruned prefix",
                        ));
                    }
                }
                for (xpath, _) in crate::layout::tb::image_leaf_refs(&image)? {
                    let bytes = crate::index::canonical_xpath_bytes(&xpath);
                    if crate::layout::tb::is_pruned_path(&bytes, prefixes) {
                        return Err(TreeSpaceError::new(
                            ErrorCode::DigestMismatch,
                            "tree leaf falls under a recorded pruned prefix",
                        ));
                    }
                }
            }
        }
        assert_ref_table_address(&ref_bytes, &pointers)?;
        Ok(checked)
    }

    /// Runs the v4 + TB reachability GC, returning the reclaimed artifact count.
    pub fn gc(&self, keep_from_sequence: u64) -> Result<GcReport> {
        self.layout.gc(GcRequest { keep_from_sequence })
    }

    /// Returns the library root directory.
    pub fn root(&self) -> &std::path::Path {
        self.layout.root()
    }

    /// Returns the plugin routing registry backing this library instance
    /// (the boot-chain / restore / verify routes, 01 §3.2).
    ///
    /// The returned shared handle is the same `Arc` the instance routes
    /// through: callers may clone it to orchestrate compositions without
    /// bypassing the instance's route surface (01 §4.1).
    pub fn plugin_registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    /// Crate-internal access to the backing layout (validate-module seam,
    /// 数据验证系统 02 §6.3: 「经 crate 内 `pub(crate)` 接缝复用 layout」;
    /// not part of the public API).
    pub(crate) fn layout(&self) -> &L {
        &self.layout
    }
}

fn not_loaded() -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::RequiredDataMissing,
        "TbLibrary state is not loaded",
    )
}

/// Rebuilds a bucket from the committed reference-table rows (01 §4-5): the
/// ref table drives the per-row recovery — the `tb-blocks/` directory is no
/// longer scanned (a scan cannot tell a missing-plugin block apart from an
/// unreferenced orphan object).
///
/// Per row (steps numbered in the actual execution order):
///   1. read the addressed block object (`*_unchecked`); a missing object is
///      [`ErrorCode::DanglingReference`] (object missing ≠ plugin missing);
///   2. address assertion: `block_blob_address(bytes) == row.address`
///      (mismatch → [`ErrorCode::DigestMismatch`], never repaired — the hash
///      is always recomputed, degradation never exempts content integrity);
///   3. parse the block kind from the frame header (an unparseable frame is
///      [`ErrorCode::PayloadMalformed`], never degraded);
///   4. label the disk version: the version side-table row (S5) takes
///      precedence; an unknown side-table literal and a magic-probe miss both
///      label the row degraded (S6 semantics — the S4 `SchemaMismatch`
///      stubs are gone). A row without a side-table entry falls back to
///      payload-magic probing (`DiskLayout::probe_magic`);
///   5. route `registry.route_disk(disk, kind)` and decode through the
///      plugin: healthy hit → `Bucket::put_envelope`; an unknown disk
///      version, a disk/kind pair without a routable plugin, or a failing
///      plugin decode is a degraded miss → [`DegradedBlock`] with the
///      original bytes into [`Bucket::put_degraded`].
///
/// `registry` is the opening library instance's plugin routing registry (the
/// injected or default surface, 01 §3.2) — the restore path never touches the
/// process global.
///
/// A commit without a side-table pointer (legacy seven/eight-column commits,
/// or a null ninth column) leaves `versions` empty and every row falls back to
/// magic probing; the legacy "native bytes stay on the IPC/native path"
/// behavior is superseded — unknown magic is a degraded miss, not a forced
/// IPC label.
fn restore_bucket_from_refs(
    layout: &impl TbLayout,
    rows: &[RefRow],
    versions: &BTreeMap<Vec<u8>, (String, String)>,
    registry: &PluginRegistry,
) -> Result<Bucket> {
    let mut bucket = Bucket::new();
    for row in rows {
        // 1. Object read (免验通道：紧随其后的地址重算只算一次，02 §7.2)。
        let bytes = layout.read_block_object_unchecked(Digest::from_bytes(row.address))?;
        // 2. Address assertion (identity ⊥ addressing, 01 §1.11.4; unconditional,
        //    never exempted by degradation).
        if block_blob_address(&bytes).as_bytes() != row.address {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "blob content hash does not match the reference-table address",
            )
            .with_context("address", hex16(&row.address)));
        }
        // 3. Kind from the frame header (an unparseable frame is
        //    `PayloadMalformed` — never degraded).
        let kind = frame_kind(&bytes)?;
        // 4. Disk-version labeling: side-table row first, magic probe as the
        //    fallback (a commit without a side-table pointer or a row without
        //    a side-table entry — the legacy 7/8-column read path — probes).
        let label = label_disk_version(versions, row, &kind, &bytes)?;
        // 5. Route + decode; every miss degrades with the original bytes in
        //    the bucket's parallel slot (01 §4-5).
        match label {
            DiskLabel::Unknown(disk_string) => {
                bucket.put_degraded(
                    RefId::from_bytes(row.ref_id),
                    DegradedBlock {
                        kind,
                        disk: DiskVersion(disk_string),
                        raw: bytes,
                    },
                )?;
            }
            DiskLabel::Known(disk) => {
                let Some(plugin) = registry.route_disk(disk, &kind) else {
                    bucket.put_degraded(
                        RefId::from_bytes(row.ref_id),
                        DegradedBlock {
                            kind,
                            disk: DiskVersion(disk.as_str().to_owned()),
                            raw: bytes,
                        },
                    )?;
                    continue;
                };
                match plugin.decode(&bytes) {
                    Ok(envelope) => {
                        bucket.put_envelope(envelope)?;
                    }
                    Err(_) => {
                        bucket.put_degraded(
                            RefId::from_bytes(row.ref_id),
                            DegradedBlock {
                                kind,
                                disk: DiskVersion(disk.as_str().to_owned()),
                                raw: bytes,
                            },
                        )?;
                    }
                }
            }
        }
    }
    Ok(bucket)
}

/// Reads the per-commit version side-table into the `xpath → (mem_ver,
/// disk_ver)` map (01 §3-1): the ninth commit column points at the side-table;
/// an absent pointer (legacy seven/eight-column commit, or a null ninth
/// column) leaves the map empty and the row labeling falls back to magic
/// probing.
fn read_versions_map(
    layout: &impl TbLayout,
    pointers: &TbPointers,
) -> Result<BTreeMap<Vec<u8>, (String, String)>> {
    let mut versions = BTreeMap::new();
    if let Some(versions_address) = pointers.versions {
        let versions_bytes = layout.read_versions_object(Digest::from_bytes(versions_address))?;
        for row in decode_versions_table(&versions_bytes)? {
            versions.insert(row.xpath, (row.mem_ver, row.disk_ver));
        }
    }
    Ok(versions)
}

/// The disk-version labeling of one ref-table row (01 §4-5): the version
/// side-table row takes precedence and preserves its string verbatim; a row
/// without a side-table entry falls back to payload magic probing.
enum DiskLabel {
    /// A known disk layout (routeable if a plugin declares the kind).
    Known(DiskLayout),
    /// An unknown disk version — degraded with the verbatim literal
    /// (side-table string, or the `"unknown"` probe label).
    Unknown(String),
}

/// Labels the disk version of one ref-table row (01 §4-5), shared by the
/// restore path and verify so both classify degraded rows identically.
///
/// A side-table row whose `disk_ver` is not a known literal labels
/// `Unknown` with the verbatim string ([`DiskLayout::from_str`] `None`);
/// a row without a side-table entry probes the payload magic — a miss
/// (neither `ARROW1` nor `PAR1`) labels `Unknown` with the fixed
/// `"unknown"` string (there is no calibrated literal to preserve).
fn label_disk_version(
    versions: &BTreeMap<Vec<u8>, (String, String)>,
    row: &RefRow,
    kind: &BlockKind,
    bytes: &[u8],
) -> Result<DiskLabel> {
    if let Some((_, disk_ver)) = versions.get(&row.xpath) {
        return Ok(match DiskLayout::from_str(disk_ver) {
            Some(disk) => DiskLabel::Known(disk),
            None => DiskLabel::Unknown(disk_ver.clone()),
        });
    }
    let payload = frame_payload(kind, bytes)?;
    Ok(match DiskLayout::probe_magic(payload) {
        Some(disk) => DiskLabel::Known(disk),
        None => DiskLabel::Unknown("unknown".to_owned()),
    })
}

/// Locates the envelope payload region of a validated frame (after the frame
/// header) for magic probing: offset 11 for built-in frames, offset
/// `2 + name_len + 10` for registered (named) frames — mirroring the frame
/// shape already validated by [`frame_kind`].
fn frame_payload<'a>(kind: &BlockKind, object: &'a [u8]) -> Result<&'a [u8]> {
    let offset = match kind {
        BlockKind::Named(name) => {
            if object.len() < 11 {
                return Err(malformed("truncated envelope"));
            }
            2 + name.as_bytes().len() + 10
        }
        _ => 11,
    };
    object
        .get(offset..)
        .ok_or_else(|| malformed("truncated envelope"))
}

/// Parses the block kind from an envelope frame header without copying the
/// payload (routing only needs the tag; the routed plugin performs the full
/// decode). Mirrors the frame-shape validation of the legacy
/// `layout/materializer.rs` payload offset walk.
fn frame_kind(object: &[u8]) -> Result<BlockKind> {
    let Some(&first) = object.first() else {
        return Err(malformed("truncated envelope"));
    };
    if first == 5 {
        if object.len() < 12 {
            return Err(malformed("truncated named envelope"));
        }
        let name_len = object[1] as usize;
        if object.len() < 2 + name_len + 11 {
            return Err(malformed("truncated named envelope"));
        }
        let name = std::str::from_utf8(&object[2..2 + name_len])
            .map_err(|_| malformed("envelope kind name is not utf8"))?
            .to_owned();
        Ok(BlockKind::Named(std::sync::Arc::from(name)))
    } else if (1..=4).contains(&first) {
        if object.len() < 11 {
            return Err(malformed("truncated envelope"));
        }
        Ok(BlockKind::from_tag(first).expect("tag range checked above"))
    } else {
        Err(malformed("unknown block kind"))
    }
}

fn malformed(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

/// 读侧地址重算：tree bytes 的地址必须等于已提交头的 `tree_blob` 锚点。
/// 不匹配 → `Err(DigestMismatch)`（不修复，01 §4.1 / §4.4；identity 不进
/// 物理寻址，只验证，01 §8）。
pub(crate) fn assert_tree_address(tree_bytes: &[u8], pointers: &TbPointers) -> Result<()> {
    if tree_blob_address(tree_bytes).as_bytes() != pointers.tree_blob {
        return Err(TreeSpaceError::new(
            ErrorCode::DigestMismatch,
            "tree blob recomputed address does not match the committed tree_blob pointer",
        ));
    }
    Ok(())
}

/// 读侧地址重算：ref table bytes 的地址必须等于已提交头的 `refs` 锚点。
/// 不匹配 → `Err(DigestMismatch)`（不修复，01 §4.1 / §4.4；identity 不进
/// 物理寻址，只验证，01 §8）。
pub(crate) fn assert_ref_table_address(ref_bytes: &[u8], pointers: &TbPointers) -> Result<()> {
    if ref_table_address(ref_bytes).as_bytes() != pointers.refs {
        return Err(TreeSpaceError::new(
            ErrorCode::DigestMismatch,
            "reference table recomputed address does not match the committed refs pointer",
        ));
    }
    Ok(())
}
