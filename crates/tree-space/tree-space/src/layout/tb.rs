//! TB disk-object codec for the flat-dir layout.
//!
//! This module owns the byte-level shapes of the tree-and-bucket surface:
//!
//! - block envelope blobs (`tb-blobs/{address}.bin`, address =
//!   `canonical_digest(b"tb-blob", [envelope_bytes])`),
//! - canonical tree blobs (`tb-trees/{address}.ipc`, address =
//!   `canonical_digest(b"tb-tree-blob", [tree_bytes])`),
//! - three-column reference tables (`tb-refs/{address}.ipc`, address =
//!   `canonical_digest(b"tb-refs", [ref_table_ipc_bytes])`),
//! - the degradation objects (empty v4 tree id + empty metadata snapshot ref)
//!   that keep old readers from treating a TB directory as corrupt,
//! - the nine-column TB commit batch (P-IO-5 `tb_pruned` record + PL-1 S5
//!   `tb_versions` pointer) and its `commit_id`,
//! - the per-commit version side-table (`tb-versions/`, 01 §3-1),
//! - the canonical xpath leaf-ref walk over a decoded tree image used by verify.
//!
//! The storage-address iron rule is enforced here: semantic identities
//! (`RefId`, `TreeId`, `tb-root`/`tb-block-*` domains) never participate in
//! physical file addressing. See `_dev/树与桶管道/01-目标与设计.md` §1.9.

use crate::block::RefId;
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::hash::canonical_digest;
use crate::ids::Digest;
use crate::ipc::{decode_batch, encode_batch};
use crate::manifest::BootstrapImage;
use crate::tree::codec::{ImageContent, ImageField, ImageView, ImageViewInput, TreeImage};
use crate::xpath::XPath;
use arrow::array::{Array, BinaryArray, FixedSizeBinaryArray, StringArray, UInt64Array};
use arrow::buffer::Buffer;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Identity domain of a block envelope blob (`tb-blobs/`).
pub const BLOCK_BLOB_DOMAIN: &[u8] = b"tb-blob";
/// Identity domain of a canonical tree blob (`tb-trees/`).
pub const TREE_BLOB_DOMAIN: &[u8] = b"tb-tree-blob";
/// Identity domain of a three-column reference table (`tb-refs/`).
pub const REFS_DOMAIN: &[u8] = b"tb-refs";
/// Identity domain of a per-commit version side-table (`tb-versions/`).
pub const VERSIONS_DOMAIN: &[u8] = b"tb-versions";

/// Computes the storage address of an envelope blob.
pub fn block_blob_address(envelope_bytes: &[u8]) -> Digest {
    canonical_digest(BLOCK_BLOB_DOMAIN, [envelope_bytes.to_vec()])
}

/// Computes the storage address of a canonical tree blob.
pub fn tree_blob_address(tree_bytes: &[u8]) -> Digest {
    canonical_digest(TREE_BLOB_DOMAIN, [tree_bytes.to_vec()])
}

/// Computes the storage address of a reference-table IPC object.
pub fn ref_table_address(ref_table_ipc: &[u8]) -> Digest {
    canonical_digest(REFS_DOMAIN, [ref_table_ipc.to_vec()])
}

/// Computes the storage address of a reference-table IPC object from its rows.
pub fn ref_table_address_of_rows(rows: &[RefRow]) -> Result<Digest> {
    Ok(ref_table_address(&encode_ref_table(rows)?))
}

/// Computes the storage address of a version side-table IPC object (`tb-versions/`).
///
/// The per-commit version side-table is a tree companion: content-addressed
/// like the other TB objects, but its address is a derived redundancy of the
/// commit (referenced from the ninth column) and never enters the
/// `tb_commit_id` domain (01 §3-1 / 02 §5.7).
pub fn versions_table_address(versions_ipc: &[u8]) -> Digest {
    canonical_digest(VERSIONS_DOMAIN, [versions_ipc.to_vec()])
}

/// The identity of the canonical empty v4 tree object
/// (`tree_id([]) = canonical_digest(b"tree", [])`).
pub fn empty_tree_id() -> Digest {
    crate::layout::flat_dir::tree_id_golden(&[])
}

/// The reference of the canonical empty metadata snapshot: the built-in empty
/// bootstrap (nine zero-row special tables plus manifest seeds).
pub fn empty_metadata_ref() -> Result<Digest> {
    let bootstrap = BootstrapImage::built_in()?;
    crate::layout::flat_dir::metadata_ref(&bootstrap.special_tables)
}

/// Computes the nine-column TB commit identity (formula unchanged by the
/// `tb_pruned` and `tb_versions` columns: both are derived redundancies
/// excluded from the domain).
///
/// The v4 domain tag is preserved; parts are `[sequence, parent,
/// tb_root_tree_id, tb_tree_blob, tb_refs]`. The v4 `tree_id`/`metadata_ref`
/// columns of a TB commit are the constant degradation objects and therefore do
/// not need to be re-entered as parts (see `_dev/树与桶管道/01-目标与设计.md`
/// §1.9.5); the ninth `tb_versions` column is a tree-companion pointer and is
/// likewise excluded (01 §3-1, P-IO-5 `tb_pruned` precedent) — so the
/// re-frozen golden-19 id stays `ea454fc5...`.
pub fn tb_commit_id(
    sequence: u64,
    parent: [u8; 16],
    tb_root_tree_id: [u8; 16],
    tb_tree_blob: [u8; 16],
    tb_refs: [u8; 16],
) -> Digest {
    canonical_digest(
        b"commit",
        vec![
            sequence.to_le_bytes().to_vec(),
            parent.to_vec(),
            tb_root_tree_id.to_vec(),
            tb_tree_blob.to_vec(),
            tb_refs.to_vec(),
        ],
    )
}

/// One row of a three-column TB reference table.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RefRow {
    /// The canonical xpath bytes of the addressed leaf.
    pub xpath: Vec<u8>,
    /// The semantic block identity of the addressed leaf.
    pub ref_id: [u8; 16],
    /// The storage address (`tb-blobs/`) of the referenced envelope blob.
    pub address: [u8; 16],
}

/// One row of a three-column version side-table (01 §3-1).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct VersionRow {
    /// The canonical xpath bytes of the addressed leaf (aligned with
    /// [`RefRow::xpath`]).
    pub xpath: Vec<u8>,
    /// The in-memory layout version of the leaf's block (M1: always
    /// `"arrow55"`).
    pub mem_ver: String,
    /// The on-disk layout version of the leaf's block (`"arrow-ipc"` /
    /// `"arrow-parquet"`).
    pub disk_ver: String,
}

/// Returns the frozen arrow schema of a three-column TB reference table.
pub fn ref_table_schema() -> Schema {
    Schema::new(vec![
        Field::new("xpath", DataType::Binary, false),
        Field::new("ref_id", DataType::FixedSizeBinary(16), false),
        Field::new("address", DataType::FixedSizeBinary(16), false),
    ])
}

/// Returns the frozen arrow schema of a three-column version side-table
/// (01 §3-1): `xpath` / `mem_ver` / `disk_ver`, all non-null.
pub fn tb_versions_schema() -> Schema {
    Schema::new(vec![
        Field::new("xpath", DataType::Binary, false),
        Field::new("mem_ver", DataType::Utf8, false),
        Field::new("disk_ver", DataType::Utf8, false),
    ])
}

/// Encodes a reference table as one canonical IPC record batch.
///
/// Rows must already be canonically sorted (see [`derive_ref_rows`]); the
/// encoder does not re-sort them.
pub fn encode_ref_table(rows: &[RefRow]) -> Result<Vec<u8>> {
    let batch = RecordBatch::try_new(
        Arc::new(ref_table_schema()),
        vec![
            Arc::new(binary_array(rows.iter().map(|row| row.xpath.as_slice()))),
            Arc::new(fixed_array(rows.iter().map(|row| row.ref_id))),
            Arc::new(fixed_array(rows.iter().map(|row| row.address))),
        ],
    )
    .map_err(|error| corrupt(error.to_string()))?;
    encode_batch(&batch)
}

/// Encodes a version side-table as one canonical IPC record batch (01 §3-1).
///
/// Rows must already be sorted by canonical xpath bytes ascending (the writer
/// derives them straight from the sorted reference-table rows); the encoder
/// does not re-sort them (the decoder validates the ascending order).
pub fn encode_versions_table(rows: &[VersionRow]) -> Result<Vec<u8>> {
    let batch = RecordBatch::try_new(
        Arc::new(tb_versions_schema()),
        vec![
            Arc::new(binary_array(rows.iter().map(|row| row.xpath.as_slice()))),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.mem_ver.clone())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.disk_ver.clone())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|error| corrupt(error.to_string()))?;
    encode_batch(&batch)
}

fn binary_array<'a>(values: impl Iterator<Item = &'a [u8]>) -> BinaryArray {
    let values = values.collect::<Vec<_>>();
    let mut offsets = Vec::with_capacity(values.len() + 1);
    offsets.push(0_i32);
    let mut total = 0_u32;
    for value in &values {
        total += value.len() as u32;
        offsets.push(total as i32);
    }
    let data: Vec<u8> = values
        .iter()
        .flat_map(|value| value.iter().copied())
        .collect();
    BinaryArray::new(
        arrow::buffer::OffsetBuffer::new(arrow::buffer::ScalarBuffer::from(offsets)),
        Buffer::from(data),
        None,
    )
}

fn fixed_array(values: impl Iterator<Item = [u8; 16]>) -> FixedSizeBinaryArray {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        FixedSizeBinaryArray::new(16, Buffer::from(Vec::<u8>::new()), None)
    } else {
        FixedSizeBinaryArray::try_from_iter(values.into_iter().map(|value| value.to_vec()))
            .expect("16-byte values are valid fixed binary")
    }
}

/// Decodes a reference-table IPC object, validating schema, nulls and the
/// canonical ascending xpath order.
pub fn decode_ref_table(bytes: &[u8]) -> Result<Vec<RefRow>> {
    let batch = decode_batch(bytes)?;
    let expected = ref_table_schema();
    let actual = batch.schema();
    if actual.fields().len() != expected.fields().len() {
        return Err(schema(
            "reference table schema does not match the canonical column set",
        ));
    }
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(schema(
                "reference table schema does not match the canonical column set",
            ));
        }
    }
    if batch.num_columns() != 3 {
        return Err(schema("reference table must have exactly three columns"));
    }
    let xpaths = batch
        .column(0)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| schema("reference table xpath column is not Binary"))?;
    let ref_ids = batch
        .column(1)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| schema("reference table ref_id column is not FixedSizeBinary(16)"))?;
    let addresses = batch
        .column(2)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| schema("reference table address column is not FixedSizeBinary(16)"))?;
    if ref_ids.value_length() != 16 || addresses.value_length() != 16 {
        return Err(schema(
            "reference table id/address columns are not 16 bytes",
        ));
    }
    let mut rows = Vec::with_capacity(batch.num_rows());
    let mut previous: Option<&[u8]> = None;
    for index in 0..batch.num_rows() {
        if xpaths.is_null(index) || ref_ids.is_null(index) || addresses.is_null(index) {
            return Err(schema("reference table rows cannot be null"));
        }
        let xpath = xpaths.value(index);
        if previous.is_some_and(|previous| previous >= xpath) {
            return Err(schema(
                "reference table xpath rows are not strictly ascending",
            ));
        }
        previous = Some(xpath);
        let ref_id: [u8; 16] = ref_ids
            .value(index)
            .try_into()
            .map_err(|_| schema("reference table ref_id has the wrong width"))?;
        let address: [u8; 16] = addresses
            .value(index)
            .try_into()
            .map_err(|_| schema("reference table address has the wrong width"))?;
        rows.push(RefRow {
            xpath: xpath.to_vec(),
            ref_id,
            address,
        });
    }
    Ok(rows)
}

/// Decodes a version side-table IPC object, validating the deep schema,
/// non-null rows and the canonical ascending xpath order (01 §3-1).
pub fn decode_versions_table(bytes: &[u8]) -> Result<Vec<VersionRow>> {
    let batch = decode_batch(bytes)?;
    let expected = tb_versions_schema();
    let actual = batch.schema();
    if actual.fields().len() != expected.fields().len() {
        return Err(schema(
            "version side-table schema does not match the canonical column set",
        ));
    }
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(schema(
                "version side-table schema does not match the canonical column set",
            ));
        }
    }
    if batch.num_columns() != 3 {
        return Err(schema("version side-table must have exactly three columns"));
    }
    let xpaths = batch
        .column(0)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| schema("version side-table xpath column is not Binary"))?;
    let mem_vers = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| schema("version side-table mem_ver column is not Utf8"))?;
    let disk_vers = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| schema("version side-table disk_ver column is not Utf8"))?;
    let mut rows = Vec::with_capacity(batch.num_rows());
    let mut previous: Option<&[u8]> = None;
    for index in 0..batch.num_rows() {
        if xpaths.is_null(index) || mem_vers.is_null(index) || disk_vers.is_null(index) {
            return Err(schema("version side-table rows cannot be null"));
        }
        let xpath = xpaths.value(index);
        if previous.is_some_and(|previous| previous >= xpath) {
            return Err(schema(
                "version side-table xpath rows are not strictly ascending",
            ));
        }
        previous = Some(xpath);
        rows.push(VersionRow {
            xpath: xpath.to_vec(),
            mem_ver: mem_vers.value(index).to_owned(),
            disk_ver: disk_vers.value(index).to_owned(),
        });
    }
    Ok(rows)
}

/// Derives the canonical reference rows for a commit-form tree.
///
/// For every `(xpath, ref_id)` leaf the envelope is resolved from `bucket` and
/// the blob address computed from its full bytes. A leaf whose block is absent
/// from the bucket is an error (a tree must not reference an unpersisted
/// block). Rows are deduplicated on `(xpath, ref_id)` and sorted by canonical
/// xpath bytes ascending, then by `ref_id` for a total order.
///
/// This is the canonical (Arrow IPC) derivation: addresses are the full-content
/// hash of the enveloped canonical payload. Materialization-aware derivation
/// uses [`derive_ref_rows_with_addresses`].
pub fn derive_ref_rows(leaf_refs: &[(XPath, RefId)], bucket: &Bucket) -> Result<Vec<RefRow>> {
    derive_ref_rows_with_addresses(leaf_refs, bucket, |ref_id| {
        let envelope = bucket.get(ref_id).expect("checked present above");
        block_blob_address(&envelope.encode()).as_bytes()
    })
}

/// Like [`derive_ref_rows`], but with the physical address of each leaf
/// supplied by the caller.
///
/// The caller computes the address from the materialized object bytes (01
/// §1.11.4: the address column points at the physical object), while the leaf
/// set and the existence check against `bucket` stay unchanged. The public
/// [`derive_ref_rows`] is the IPC canonical form of this function.
pub(crate) fn derive_ref_rows_with_addresses(
    leaf_refs: &[(XPath, RefId)],
    bucket: &Bucket,
    address_for: impl Fn(RefId) -> [u8; 16],
) -> Result<Vec<RefRow>> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for (xpath, ref_id) in leaf_refs {
        bucket.get(*ref_id).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "tree leaf references a block that is absent from the bucket",
            )
            .with_context("ref_id", ref_id.to_string())
        })?;
        let xpath_bytes = crate::index::canonical_xpath_bytes(xpath);
        let address = address_for(*ref_id);
        if seen.insert((xpath_bytes.clone(), ref_id.as_bytes())) {
            rows.push(RefRow {
                xpath: xpath_bytes,
                ref_id: ref_id.as_bytes(),
                address,
            });
        }
    }
    rows.sort();
    Ok(rows)
}

/// The refusal reason reported when a leaf blob referenced by a reference table
/// cannot be resolved.
fn corrupt(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, message.into())
}

fn schema(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}

/// Recomputes the `(XPath, RefId)` leaf set of a decoded tree image by
/// structural traversal.
///
/// Named children become `Field` steps, positional children `Index` steps,
/// chunk-group children use the flattened ordinal (matching `ChunkGroup::refs`
/// / the `DynamicNode` and type-derive leaf walks), and view inputs use their
/// input ordinal. Named children, `String` map keys and `[u8; 16]` keys all
/// derive a `Field` step (bytes16 keys use their lowercase 32-char hex field
/// name) — because the typed leaf walk derives the same steps, the recompute
/// is exact for every leaf class (see `_dev/树与桶管道/01-目标与设计.md` §2).
pub fn image_leaf_refs(image: &TreeImage) -> Result<Vec<(XPath, RefId)>> {
    let mut out = Vec::new();
    walk_node(image.children(), XPath::root(), &mut out)?;
    Ok(out)
}

fn walk_node(
    children: &[crate::tree::codec::ImageField],
    parent: XPath,
    out: &mut Vec<(XPath, RefId)>,
) -> Result<()> {
    for (index, field) in children.iter().enumerate() {
        let path = match &field.locator {
            crate::tree::codec::Locator::Named(name) => parent.clone().field(name),
            crate::tree::codec::Locator::Keyed(key) => parent.clone().field(hex16(key)),
            crate::tree::codec::Locator::Positioned => parent.clone().index(index),
        };
        walk_content(field, path, out)?;
    }
    Ok(())
}

fn walk_content(
    field: &crate::tree::codec::ImageField,
    path: XPath,
    out: &mut Vec<(XPath, RefId)>,
) -> Result<()> {
    match &field.content {
        ImageContent::Node(children) => walk_node(children, path, out),
        ImageContent::Inline(_) => Ok(()),
        ImageContent::Ref(id) => {
            out.push((path, *id));
            Ok(())
        }
        ImageContent::ChunkGroup { children, .. } => {
            let mut flat = 0_usize;
            walk_group(children, path, &mut flat, out)
        }
        ImageContent::ChunkEntry(id) => {
            // A ChunkEntry is only meaningful inside a ChunkGroup; the group
            // walker already flattens it. A stray direct entry is malformed.
            out.push((path, *id));
            Ok(())
        }
        ImageContent::View(view) => walk_view(view, path, out),
    }
}

fn walk_group(
    children: &[crate::tree::codec::ImageField],
    path: XPath,
    flat: &mut usize,
    out: &mut Vec<(XPath, RefId)>,
) -> Result<()> {
    for child in children {
        if child.locator != crate::tree::codec::Locator::Positioned {
            return Err(schema("chunk group children must be positioned"));
        }
        match &child.content {
            ImageContent::ChunkEntry(id) => {
                out.push((path.clone().index(*flat), *id));
                *flat += 1;
            }
            ImageContent::ChunkGroup {
                children: nested, ..
            } => {
                walk_group(nested, path.clone(), flat, out)?;
            }
            _ => {
                return Err(schema(
                    "chunk group children are chunk entries or nested groups",
                ));
            }
        }
    }
    Ok(())
}

fn walk_view(view: &ImageView, path: XPath, out: &mut Vec<(XPath, RefId)>) -> Result<()> {
    for (index, input) in view.inputs.iter().enumerate() {
        match input {
            ImageViewInput::Ref(id) => out.push((path.clone().index(index), *id)),
            ImageViewInput::View(nested) => walk_view(nested, path.clone().index(index), out)?,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ephemeral pruning
// ---------------------------------------------------------------------------

/// Whether the given canonical xpath bytes fall under any ephemeral prefix.
///
/// A canonical xpath is the length-prefixed concatenation of its steps, so byte
/// prefixing is exactly step prefixing (`/a` never matches `/ab` because the
/// name length prefix differs; `[1]` never matches `[12]` because the index is
/// fixed-width little-endian). See 01 §1.9.8 (b)1.
pub fn is_pruned_path(xpath_bytes: &[u8], prefixes: &[Vec<u8>]) -> bool {
    prefixes
        .iter()
        .any(|prefix| xpath_bytes.starts_with(prefix))
}

/// Prunes a tree's root image fields under the ephemeral prefixes.
///
/// A field whose xpath (computed exactly as in [`image_leaf_refs`]) falls under
/// any prefix is removed together with its whole subtree. The observed leaf
/// set is recomputed from the pruned form by callers via [`image_leaf_refs`].
/// Chunk groups and views are pruned as a single field unit (the derive's
/// ephemeral unit is the whole `Vec<Block>` container / view field);
/// entry-level pruning is not performed because removing individual entries
/// re-flows their ordinal xpaths, which would make a recorded prefix
/// ambiguous. Returns the surviving fields and the canonical bytes of the
/// maximally pruned field xpaths (the `tb_pruned` record), sorted ascending.
pub fn prune_image(
    fields: Vec<ImageField>,
    prefixes: &[Vec<u8>],
) -> (Vec<ImageField>, Vec<Vec<u8>>) {
    let mut kept = Vec::new();
    let mut pruned = BTreeSet::new();
    pruned_walk_node(fields, XPath::root(), prefixes, &mut kept, &mut pruned);
    let mut recorded = pruned.into_iter().collect::<Vec<_>>();
    recorded.sort();
    (kept, recorded)
}

fn pruned_walk_node(
    children: Vec<ImageField>,
    parent: XPath,
    prefixes: &[Vec<u8>],
    out: &mut Vec<ImageField>,
    pruned: &mut BTreeSet<Vec<u8>>,
) {
    for (index, field) in children.into_iter().enumerate() {
        let path = match &field.locator {
            crate::tree::codec::Locator::Named(name) => parent.clone().field(name),
            crate::tree::codec::Locator::Keyed(key) => parent.clone().field(hex16(key)),
            crate::tree::codec::Locator::Positioned => parent.clone().index(index),
        };
        let path_bytes = crate::index::canonical_xpath_bytes(&path);
        if is_pruned_path(&path_bytes, prefixes) {
            pruned.insert(path_bytes);
            continue;
        }
        pruned_walk_content(field, path, prefixes, out, pruned);
    }
}

fn pruned_walk_content(
    field: ImageField,
    path: XPath,
    prefixes: &[Vec<u8>],
    out: &mut Vec<ImageField>,
    pruned: &mut BTreeSet<Vec<u8>>,
) {
    match field.content {
        ImageContent::Node(children) => {
            let mut kids = Vec::new();
            pruned_walk_node(children, path, prefixes, &mut kids, pruned);
            out.push(ImageField {
                locator: field.locator,
                content: ImageContent::Node(kids),
            });
        }
        ImageContent::ChunkGroup { kind, children } => {
            out.push(ImageField {
                locator: field.locator,
                content: ImageContent::ChunkGroup { kind, children },
            });
        }
        ImageContent::View(view) => {
            out.push(ImageField {
                locator: field.locator,
                content: ImageContent::View(view),
            });
        }
        _ => out.push(field),
    }
}

/// The three TB pointer columns of a TB commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TbPointers {
    /// The root `TreeId` (semantic identity) of the committed tree.
    pub root_tree_id: [u8; 16],
    /// The `tb-trees/` storage address of the canonical tree blob.
    pub tree_blob: [u8; 16],
    /// The `tb-refs/` storage address of the three-column reference table.
    pub refs: [u8; 16],
    /// The recorded pruned canonical xpath prefixes (`tb_pruned` column).
    ///
    /// `None` = the column is null (no pruning was recorded — a legacy
    /// seven-column commit or a full-tree commit), `Some(list)` = the column is
    /// non-null (pruning was enabled; the list may be empty when nothing was
    /// pruned in this commit).
    pub pruned: Option<Vec<Vec<u8>>>,
    /// The `tb-versions/` storage address of the per-commit version side-table
    /// (01 §3-1, ninth `tb_versions` column).
    ///
    /// `None` = the column is null (a legacy seven- or eight-column commit, or
    /// a commit written without a side-table pointer) — the open path falls
    /// back to per-object magic probing for every leaf.
    pub versions: Option<[u8; 16]>,
}

/// Reads the appended TB columns of a commit batch.
///
/// A four-column batch (pure v4 commit) yields `None`. A batch with any of the
/// appended columns present must carry the three pointers, each as non-null
/// `FixedSizeBinary(16)`; an optional fourth appended column `tb_pruned` must
/// be `List<Binary>` (nullable) and an optional fifth appended column
/// `tb_versions` must be `FixedSizeBinary(16)` (nullable). A seven-column batch
/// (legacy TB commit) is readable and yields `pruned = None`; an eight-column
/// batch adds the pruned list and yields `versions = None`; a nine-column batch
/// (PL-1 S5) adds the versions pointer. Five or six appended columns, a missing
/// list column on an eight/nine-column batch, a wrong list type, or more than
/// nine columns are half-extended/incorrectly typed/over-extended commits and
/// rejected.
pub fn decode_tb_commit_pointers(batch: &RecordBatch) -> Result<Option<TbPointers>> {
    if batch.num_columns() < 5 {
        return Ok(None);
    }
    if batch.num_columns() < 7 {
        return Err(schema("commit has a partially extended TB column set"));
    }
    if batch.num_columns() > 9 {
        return Err(schema(
            "commit carries an unexpected TB extension beyond tb_versions",
        ));
    }
    let mut values = [None::<[u8; 16]>; 3];
    for (local, column_index) in [4_usize, 5, 6].into_iter().enumerate() {
        let column = batch.column(column_index);
        let array = column
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| schema("commit TB append column is not FixedSizeBinary(16)"))?;
        if array.value_length() != 16 {
            return Err(schema("commit TB append column is not 16 bytes wide"));
        }
        if array.is_null(0) {
            return Err(schema("commit TB append column must be non-null"));
        }
        let value: [u8; 16] = array
            .value(0)
            .try_into()
            .map_err(|_| schema("commit TB append value has the wrong width"))?;
        values[local] = Some(value);
    }
    let mut pointers = TbPointers {
        root_tree_id: values[0].expect("filled"),
        tree_blob: values[1].expect("filled"),
        refs: values[2].expect("filled"),
        pruned: None,
        versions: None,
    };
    if batch.num_columns() < 8 {
        return Ok(Some(pointers));
    }
    pointers.pruned = decode_tb_pruned_column(batch.column(7))?;
    if batch.num_columns() < 9 {
        return Ok(Some(pointers));
    }
    pointers.versions = decode_tb_versions_column(batch.column(8))?;
    Ok(Some(pointers))
}

/// Decodes the `tb_versions: FixedSizeBinary(16)` nullable column of a
/// nine-column commit: `null` → `None`, a non-null 16-byte value → `Some`.
fn decode_tb_versions_column(column: &arrow::array::ArrayRef) -> Result<Option<[u8; 16]>> {
    let array = column
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| schema("commit tb_versions column is not FixedSizeBinary(16)"))?;
    if array.value_length() != 16 {
        return Err(schema("commit tb_versions column is not 16 bytes wide"));
    }
    if array.is_null(0) {
        return Ok(None);
    }
    let value: [u8; 16] = array
        .value(0)
        .try_into()
        .map_err(|_| schema("commit tb_versions value has the wrong width"))?;
    Ok(Some(value))
}

/// Decodes the `tb_pruned: List<Binary>` column of an eight-column commit.
fn decode_tb_pruned_column(column: &arrow::array::ArrayRef) -> Result<Option<Vec<Vec<u8>>>> {
    let expected = DataType::List(Arc::new(Field::new_list_field(DataType::Binary, true)));
    if column.data_type() != &expected {
        return Err(schema("commit tb_pruned column is not List<Binary>"));
    }
    let list = column
        .as_any()
        .downcast_ref::<arrow::array::ListArray>()
        .ok_or_else(|| schema("commit tb_pruned column is not a list"))?;
    if list.is_null(0) {
        return Ok(None);
    }
    let values = list.value(0);
    let binaries = values
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| schema("commit tb_pruned list element is not Binary"))?;
    let prefixes = (0..binaries.len())
        .map(|index| binaries.value(index).to_vec())
        .collect::<Vec<_>>();
    Ok(Some(prefixes))
}

/// Returns the frozen Arrow schema of a TB commit batch (nine columns).
///
/// The ninth column `tb_versions` points at the per-commit version side-table
/// address (01 §3-1); it is nullable so the commit bytes stay readable for
/// every earlier column count (a legacy seven/eight-column commit decodes with
/// `versions = None`).
pub fn tb_commit_schema() -> Schema {
    Schema::new(vec![
        Field::new("sequence", DataType::UInt64, false),
        Field::new("parent", DataType::FixedSizeBinary(16), false),
        Field::new("tree_id", DataType::FixedSizeBinary(16), false),
        Field::new("metadata_ref", DataType::Utf8, false),
        Field::new("tb_root_tree_id", DataType::FixedSizeBinary(16), true),
        Field::new("tb_tree_blob", DataType::FixedSizeBinary(16), true),
        Field::new("tb_refs", DataType::FixedSizeBinary(16), true),
        Field::new(
            "tb_pruned",
            DataType::List(Arc::new(Field::new_list_field(DataType::Binary, true))),
            true,
        ),
        Field::new("tb_versions", DataType::FixedSizeBinary(16), true),
    ])
}

/// Builds the nine-column TB commit batch.
///
/// The v4 `tree_id`/`metadata_ref` columns carry the constant degradation
/// objects so that an old reader sees an empty library instead of corruption.
/// `pruned` distinguishes the null column (`None`, a full-tree commit) from a
/// non-null list (`Some`, pruning enabled — possibly an empty list when
/// nothing was pruned). `versions` is the nullable ninth column: `Some`
/// records the per-commit version side-table address (01 §3-1); `None` writes
/// null (no side-table pointer — the open path probes every leaf).
pub fn tb_commit_batch(
    sequence: u64,
    parent: [u8; 16],
    tb_root_tree_id: [u8; 16],
    tb_tree_blob: [u8; 16],
    tb_refs: [u8; 16],
    pruned: Option<&[Vec<u8>]>,
    versions: Option<[u8; 16]>,
) -> Result<RecordBatch> {
    let empty_tree = empty_tree_id().as_bytes();
    let empty_meta = empty_metadata_ref()?.as_bytes();
    RecordBatch::try_new(
        Arc::new(tb_commit_schema()),
        vec![
            Arc::new(UInt64Array::from(vec![sequence])),
            Arc::new(fixed_array(vec![parent].into_iter())),
            Arc::new(fixed_array(vec![empty_tree].into_iter())),
            Arc::new(StringArray::from(vec![hex16(&empty_meta)])),
            Arc::new(fixed_array(vec![tb_root_tree_id].into_iter())),
            Arc::new(fixed_array(vec![tb_tree_blob].into_iter())),
            Arc::new(fixed_array(vec![tb_refs].into_iter())),
            pruned_array(pruned),
            versions_array(versions),
        ],
    )
    .map_err(|error| schema(error.to_string()))
}

/// Builds the `tb_pruned` column array: `null` for `None`, a non-null list for
/// `Some`.
fn pruned_array(pruned: Option<&[Vec<u8>]>) -> std::sync::Arc<dyn arrow::array::Array> {
    let field = Arc::new(Field::new_list_field(DataType::Binary, true));
    match pruned {
        None => Arc::new(arrow::array::ListArray::new_null(field, 1)),
        Some(prefixes) => {
            let child = binary_array(prefixes.iter().map(|prefix| prefix.as_slice()));
            let offsets =
                arrow::buffer::OffsetBuffer::new(arrow::buffer::ScalarBuffer::from(vec![
                    0_i32,
                    prefixes.len() as i32,
                ]));
            Arc::new(arrow::array::ListArray::new(
                field,
                offsets,
                Arc::new(child),
                None,
            ))
        }
    }
}

/// Builds the `tb_versions` column array: `null` for `None`, a non-null
/// 16-byte value for `Some`.
fn versions_array(versions: Option<[u8; 16]>) -> std::sync::Arc<dyn arrow::array::Array> {
    match versions {
        None => Arc::new(FixedSizeBinaryArray::new_null(16, 1)),
        Some(value) => Arc::new(fixed_array(vec![value].into_iter())),
    }
}

/// Sweeps `tb-blobs/{address}.bin` files that are not in the reachable set.
pub(crate) fn sweep_tb_blobs(dir: &std::path::Path, keep: &BTreeSet<[u8; 16]>) -> Result<u64> {
    let mut reclaimed = 0;
    if !dir.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(dir).map_err(|error| io(error))? {
        let entry = entry.map_err(|error| io(error))?;
        if !entry.file_type().map_err(|error| io(error))?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".bin") else {
            continue;
        };
        let Some(address) = crate::layout::flat_dir::hex_to_bytes(stem) else {
            continue;
        };
        if !keep.contains(&address) {
            std::fs::remove_file(entry.path()).map_err(|error| io(error))?;
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

/// Encodes a 16-byte map key as its lowercase 32-char hex field name
/// (`{:02x}`, matching the file-name hex convention of the layout).
pub fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Decodes a 32-char hex field name back into its 16-byte map key.
pub fn parse_hex16(name: &str) -> Option<[u8; 16]> {
    if name.len() != 32 {
        return None;
    }
    let mut out = [0_u8; 16];
    for (index, chunk) in name.as_bytes().chunks(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out[index] = (high << 4) | low;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn io(error: std::io::Error) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, "tb sweep I/O failed")
        .with_context("detail", error.to_string())
}
