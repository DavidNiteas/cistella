//! `tree-space` v4 storage, registry, and verification runtime.
//!
//! The protocol authority is documented in [`_dev/README.md`](../_dev/README.md);
//! the tree-and-bucket model and tree runtime contract in
//! [`_dev/archive/树与桶模型改造/01-目标与设计.md`](../_dev/archive/树与桶模型改造/01-目标与设计.md).
//! Persistent table payloads and metadata use Arrow 55 IPC; the only fixed
//! non-Arrow bytes are the documented single-file header and trailer.

#![deny(missing_docs)]

pub mod block;
pub mod bucket;
pub mod domain;
pub mod error;
pub mod exchange;
pub mod fault;
pub mod hash;
pub mod ids;
pub mod index;
pub mod ipc;
pub mod layout;
pub mod leaf;
pub mod lock;
pub mod manifest;
pub mod metadata;
pub mod path;
pub mod perf;
/// Versioned plugin system (layout-version enums; plugin traits in later PL-1
/// stages).
pub mod plugin;
pub mod registry;
pub mod shared;
pub mod slot;
pub mod special;
pub mod template;
pub mod transform;
pub mod tree;

pub mod tb_library;

pub mod types;
pub mod validate;
pub mod xpath;

pub use block::{
    ArrowCaps, ArrowTable, Blob, Block, BlockCaps, BlockDesc, BlockKind, Decimal128, Decimal256,
    DurationUnit, Envelope, Kv, RefId, RegisteredBlock, Sequence, Time32Unit, Time64Unit,
    TimestampUnit, Value, ValueKind, block_caps, block_ref_id, read_block, register_block,
    validate_block_name,
};
pub use bucket::Bucket;
pub use exchange::access::{
    BlockRead, LazySnapshot, MappedView, OwnedSnapshot, TreeRead, open_block_mapped,
    open_tree_mapped, read_block_full, read_lazy, read_tree, read_tree_full,
};
pub use exchange::{
    AccessMode, ExchangeKey, Holder, IndexPolicySetting, InvalidState, IoBlobSetting, IpcSource,
    RuntimeConfig, RuntimeEntry, RuntimeTable, Source, TreeAccess,
};
// NOTE: the runtime table-dispatched block reader stays reachable at
// `exchange::access::read_block` — the bare `read_block` name at the crate root
// is already taken by the frozen `block::read_block` registry entry, and
// renaming either would break the existing public API (E-1 zero-break rule).
pub use exchange::merge::{MergeFlags, MergeOp, MergeSpec, SyncScope, merge_image, reachable_refs};
pub use exchange::ownership::require_owner;
pub use exchange::proxy::{
    ProxyClient, ProxyPolicy, ProxyReceipt, ProxyRequest, RejectReason, await_receipt,
    handle_proxy_request, publish_receipt, publish_request, receive_request,
};
pub use exchange::sync::{build_push_request, fetch_blocks, fetch_snapshot, pull, push};
pub use exchange::translator::{
    LayoutTranslator, LazyIpcSnapshot, MappedBacking, MappedObject, TbChannel, detect_disk_layout,
    fetch_blocks_ipc, fetch_snapshot_ipc, map_block_disk, map_ref_disk, map_tree_disk,
    persist_block, publish_snapshot, receive_snapshot,
};

pub use domain::{SpatialDomain, TableDomain, assert_leaf, assert_tree_node};
pub use error::{ErrorCode, Result, TreeSpaceError};
pub use fault::{FaultPlan, FaultPoint};
pub use ids::{Digest, NodeId, PathHash, SchemaVersion, TableId, TypeId};
pub use index::{
    XPathHash, XPathIndex, canonical_xpath_bytes, xpath_from_canonical_bytes, xpath_hash,
};
pub use layout::materializer::{
    BlockMaterializer, IpcMaterializer, ParquetMaterializer, materializer_for,
    probe_block_materialization, select_materializer,
};
pub use layout::tb::{
    RefRow, TbPointers, VersionRow, block_blob_address, decode_ref_table, decode_versions_table,
    derive_ref_rows, empty_metadata_ref, empty_tree_id, encode_ref_table, encode_versions_table,
    image_leaf_refs, is_pruned_path, prune_image, ref_table_address, ref_table_address_of_rows,
    ref_table_schema, tb_commit_batch, tb_commit_id, tb_commit_schema, tb_versions_schema,
    tree_blob_address, versions_table_address,
};
pub use layout::{
    GcReport, GcRequest, Materialization, PublishPlan, PublishReceipt, StorageKind, StorageLayout,
    TableBytes, TableLocator, TbLayout,
};
pub use manifest::{BootstrapImage, Manifest, ManifestValue};
pub use metadata::{
    ColumnDigestRecord, DomainMetadata, TableMetadata, table_id_for_path, table_metadata_map,
};
pub use path::{Name, RenameEntry, RenamePlan, TablePath};
pub use plugin::block::{ArrowIpcBlockPlugin, ArrowParquetBlockPlugin, BlockPlugin};
pub use plugin::boot::{BOOT_VERSION, BootRecord};
pub use plugin::degraded::{DegradedBlock, DiskVersion};
pub use plugin::registry::{IdentityMemConverter, MemoryConverter, PluginRegistry};
pub use plugin::semantic::{
    SemanticValue, semantic_block_ref_id, semantic_table, semantic_table_value, semantic_tree,
};
pub use plugin::tree::{Arrow55TreePlugin, TreePlugin};
pub use plugin::version::{DiskLayout, MemLayout};
pub use registry::{DomainNode, RegistryExport, TableInstance, TypeRegistry};
pub use slot::{Residency, SlotState, TableInner, TableSlot};
pub use tb_library::{SingleFileTbLibrary, TbCommitReceipt, TbLibrary};
pub use tree::codec::{
    DecodeBlock, DecodeTree, EncodeTree, ImageContent, ImageField, ImageView, ImageViewInput,
    Locator, TreeId, TreeImage, check_block_leaf_identity, decode, decode_block_leaf, encode,
    encode_node, from_image, keyed_field, named_field, positioned_field, project, to_image,
    tree_id, tree_schema, view_to_image,
};
pub use tree::{
    ChunkEntry, ChunkGroup, ChunkStats, Combinator, CombinatorRegistry, DynamicField, DynamicNode,
    FieldMeta, FieldTarget, JoinMode, LeafSchemaMeta, LeafTarget, Multiplicity, NodeMeta,
    Persistence, RegisteredResolver, Slot, TreeInstance, TreeNode, TreeNodeMeta, ViewInput,
    ViewNode, memory_gc_roots,
};
pub use tree_space_derive::{TreeCodec, TreeInstance, TreeNode};
pub use types::{
    Cardinality, ChildDefinition, ChildKind, ColumnDefinition, DomainType, InstanceMode, TableType,
};
pub use validate::{
    ValidateDepth, ValidateEntry, ValidateReport, ValidateRequest, ValidateSubject, validate,
};

/// The architecture generation implemented by this crate.
///
/// `v4` is the stable disk-protocol base (tables, commit-tree layouts). The TB
/// tree-and-bucket model (see
/// `_dev/archive/树与桶模型改造/01-目标与设计.md`) layers *in-memory* tree semantics on
/// top of it as a strict superset; it does not change the persisted protocol
/// version.
pub const DESIGN_VERSION: &str = "v4";
/// The only supported Arrow IPC encoding major version.
pub const ARROW_IPC_MAJOR: u16 = 55;
/// The only currently accepted digest algorithm identifier.
pub const DIGEST_ALGORITHM: &str = "ts3-xxh3-128-arrow-ipc-001";
