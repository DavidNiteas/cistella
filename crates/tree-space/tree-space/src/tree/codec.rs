//! A-2 tree object codec: canonical flat-entry-table serialization.
//!
//! A tree object is serialized as a single canonical Arrow IPC RecordBatch
//! whose rows form the pre-order flat entry table (see
//! `_dev/archive/树与桶模型改造/01-目标与设计.md` §5). The weak decode target is the
//! universal [`TreeImage`]; typed values encode through a derived
//! [`EncodeTree`] implementation and decode through [`project`] /
//! [`DecodeTree`].

use crate::block as blocks;
use crate::block::{Blob, BlockKind, Kv, RefId, RegisteredBlock, Sequence, Table, Value};
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::hash::canonical_digest;
use crate::ipc::encode_batch;
use crate::tree::{
    ChunkEntry, ChunkGroup, ChunkStats, Combinator, DynamicField, DynamicNode, JoinMode, Slot,
    ViewInput, ViewNode,
};
use arrow::array::{Array, ArrayRef, FixedSizeBinaryArray, StringArray, UnionArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;

const ENTRY_NODE: u8 = 1;
const ENTRY_INLINE: u8 = 2;
const ENTRY_REF: u8 = 3;
const ENTRY_CHUNK_GROUP: u8 = 4;
const ENTRY_CHUNK_ENTRY: u8 = 5;
const ENTRY_VIEW: u8 = 6;
const ENTRY_VIEW_PARAM: u8 = 7;

/// The root parent sentinel (no parent row).
const ROOT_PARENT: u64 = u64::MAX;

/// Byte-exact canonical identity of a tree object.
///
/// `TreeId = canonical_digest(b"tb-tree", [ipc_bytes])`, independent of the
/// bucket's `tb-block-*` identities. It is a distinct newtype to prevent
/// accidental confusion with [`RefId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TreeId(pub crate::ids::Digest);

impl TreeId {
    /// Builds an identity from raw 16-byte canonical-digest bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(crate::ids::Digest::from_bytes(bytes))
    }

    /// Returns the raw 16-byte identity bytes.
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0.as_bytes()
    }
}

impl std::fmt::Display for TreeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        crate::ids::Digest::fmt(&self.0, formatter)
    }
}

/// The fixed column order and Arrow types of the flat entry table.
///
/// The schema is the tree-object format version: any change to the column set
/// changes every `TreeId` (see `_dev/archive/树与桶模型改造/01-目标与设计.md` §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locator {
    /// A positional child (`name`/`key16` are null).
    Positioned,
    /// A named child (`name`).
    Named(String),
    /// A 16-byte map-key child (`key16`).
    Keyed([u8; 16]),
}

/// One child entry of a decoded tree node.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageField {
    /// The positioning of this entry under its parent.
    pub locator: Locator,
    /// The entry content.
    pub content: ImageContent,
}

/// The entry content of a decoded tree node.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageContent {
    /// A structural node (struct field, `Vec`/`BTreeMap` container).
    Node(Vec<ImageField>),
    /// An inline scalar slot (or a `ViewParam` scalar).
    Inline(Value),
    /// A block reference slot or a view input reference.
    Ref(RefId),
    /// A chunk group with its uniform block kind.
    ChunkGroup {
        /// The uniform block kind carried by the group row.
        kind: BlockKind,
        /// The ordered chunk entries and nested groups.
        children: Vec<ImageField>,
    },
    /// A single leaf chunk entry reference.
    ChunkEntry(RefId),
    /// A view node.
    View(ImageView),
}

/// Builds a named child field.
pub fn named_field(name: &str, content: ImageContent) -> ImageField {
    ImageField {
        locator: Locator::Named(name.to_owned()),
        content,
    }
}

/// Builds a keyed child field.
pub fn keyed_field(key: [u8; 16], content: ImageContent) -> ImageField {
    ImageField {
        locator: Locator::Keyed(key),
        content,
    }
}

/// Builds a positional child field.
pub fn positioned_field(content: ImageContent) -> ImageField {
    ImageField {
        locator: Locator::Positioned,
        content,
    }
}

/// A decoded view node: combinator name, ordered inputs, and named params.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageView {
    /// The combinator name (`concat`/`join`/`select` or a registered name).
    pub combinator: String,
    /// The ordered block/nested-view inputs.
    pub inputs: Vec<ImageViewInput>,
    /// The ordered named parameters.
    pub params: Vec<(String, Value)>,
}

/// One ordered view input.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageViewInput {
    /// A content-addressed block reference.
    Ref(RefId),
    /// A nested view.
    View(Box<ImageView>),
}

/// The universal decoded tree image: a root structural node.
///
/// Every valid canonical tree decodes to exactly one of these. `DynamicNode`
/// is the dynamic-shape subset of this type (named children only).
#[derive(Clone, Debug, PartialEq)]
pub struct TreeImage {
    children: Vec<ImageField>,
}

impl TreeImage {
    /// Builds a tree image from the root node's children.
    pub fn new(children: Vec<ImageField>) -> Self {
        Self { children }
    }

    /// Builds the empty tree (a root row with no children).
    pub fn new_empty() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Returns the root node's children.
    pub fn children(&self) -> &[ImageField] {
        &self.children
    }
}

/// Encodes a typed tree node instance into its tree-image children.
///
/// This trait is generated by `#[derive(TreeCodec)]`.
pub trait EncodeTree {
    /// Produces the named image children of this node.
    fn tree_children(&self) -> Result<Vec<ImageField>>;
}

/// Decodes a typed tree node instance from its tree-image children.
///
/// This trait is generated by `#[derive(TreeCodec)]`; the bucket is used to
/// materialize block leaves.
pub trait DecodeTree: Sized {
    /// Rebuilds this node from its image children, validating the expected
    /// field set, multiplicities, block kinds and locator consistency, and
    /// materializing block fields through `bucket`.
    fn from_image_children(children: &[ImageField], bucket: &Bucket) -> Result<Self>;

    /// L3 结构面校验（数据验证系统 01 §6.3）：`T` 声明的结构期望 ↔ 块内容结构，
    /// 只走**结构 + kind**——与 [`DecodeTree::from_image_children`] 相同的字段集 /
    /// 多重度 / locator / chunk-kind 校验，但叶子只验 kind / 身份面
    /// （[`check_block_leaf_identity`]），**不物化叶子 payload**（不展开叶子内容）。
    ///
    /// `#[derive(TreeCodec)]` 生成的实现即结构面专用版本；本默认实现退化为完整投影
    /// （对未覆写的手工 `DecodeTree` 实现方保持「能校验」的保守语义）。
    fn check_image_children(children: &[ImageField], bucket: &Bucket) -> Result<()> {
        Self::from_image_children(children, bucket).map(|_| ())
    }
}

/// The canonical eight-column tree-object schema.
///
/// Order is fixed: `parent`, `name`, `key16`, `kind`, `value`,
/// `ref`, `block_kind`, `combinator`.
pub fn tree_schema() -> Schema {
    Schema::new(vec![
        Field::new("parent", DataType::UInt64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("key16", DataType::FixedSizeBinary(16), true),
        Field::new("kind", DataType::UInt8, false),
        Field::new("value", blocks::arrow::union_data_type(), true),
        Field::new("ref", DataType::FixedSizeBinary(16), true),
        Field::new("block_kind", DataType::Utf8, true),
        Field::new("combinator", DataType::Utf8, true),
    ])
}

/// Encodes a universal tree image into its canonical Arrow IPC bytes.
///
/// The canonical form carries no annotations and no Rust type names; leaf and
/// view structure only. `null` data slots are zero-filled before encoding.
pub fn encode(image: &TreeImage) -> Result<Vec<u8>> {
    let mut rows = Vec::new();
    rows.push(Row {
        parent: ROOT_PARENT,
        name: None,
        key16: None,
        kind: ENTRY_NODE,
        value: None,
        ref_id: None,
        block_kind: None,
        combinator: None,
    });
    emit_children(&mut rows, 0, image.children(), ParentKind::Node)?;
    build_batch(&rows)
}

/// Encodes a typed tree node instance into canonical Arrow IPC bytes.
pub fn encode_node<T: EncodeTree + ?Sized>(node: &T) -> Result<Vec<u8>> {
    let children = EncodeTree::tree_children(node)?;
    encode(&TreeImage::new(children))
}

/// Decodes canonical tree bytes into the universal image.
///
/// Fails on schema mismatch, malformed parent/kind/occupancy structure,
/// invalid sibling ordering, and closed-kernel view parameter violations.
pub fn decode(bytes: &[u8]) -> Result<TreeImage> {
    let batch = crate::ipc::decode_batch(bytes)?;
    validate_schema(batch.schema().as_ref())?;
    let rows = read_rows(&batch)?;
    validate_rows(&rows)?;
    let children = build_node_fields(&rows, &row_indices(&rows, 0))?;
    Ok(TreeImage::new(children))
}

/// Computes the canonical tree identity of encoded tree bytes.
///
/// PL-2 M2 (01 §4-4) switches the formula from the canonical-byte digest to
/// the semantic walk: `TreeId = canonical_digest(b"tb-tree",
/// [semantic_tree(decode(bytes))])`. The `tb-tree` domain is
/// generation-isolated from the `tb-block-*` and legacy `leaf-*` domains.
///
/// Bytes that do not decode to a canonical tree image keep the historical
/// byte formula as a deterministic fallback (only corrupt/error paths ever
/// trip it — every committed tree decodes, and the address assertion already
/// catches byte-level tampering).
pub fn tree_id(bytes: &[u8]) -> TreeId {
    match decode(bytes) {
        Ok(image) => TreeId(canonical_digest(
            b"tb-tree",
            [crate::plugin::semantic::semantic_tree(&image)],
        )),
        Err(_) => TreeId(canonical_digest(b"tb-tree", [bytes.to_vec()])),
    }
}

/// Projects a universal image into a typed tree node, materializing block
/// leaves through `bucket` and rejecting structural mismatches.
pub fn project<T: DecodeTree>(image: &TreeImage, bucket: &Bucket) -> Result<T> {
    T::from_image_children(image.children(), bucket)
}

/// Converts a [`DynamicNode`] into its universal tree image.
pub fn to_image(node: &DynamicNode) -> Result<TreeImage> {
    Ok(TreeImage::new(dynamic_to_fields(node)?))
}

/// Converts a universal image back into a [`DynamicNode`].
///
/// Only the dynamic-shape subset converts: every node must use named children
/// (`Vec`/`BTreeMap<[u8;16]>` housed trees and registered views with
/// parameters cannot be represented by `DynamicNode` and are rejected).
pub fn from_image(image: &TreeImage) -> Result<DynamicNode> {
    dynamic_from_children(image.children())
}

// ---------------------------------------------------------------------------
// Flat-row building (encode)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Row {
    parent: u64,
    name: Option<String>,
    key16: Option<[u8; 16]>,
    kind: u8,
    value: Option<Value>,
    ref_id: Option<[u8; 16]>,
    block_kind: Option<String>,
    combinator: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentKind {
    Node,
    Group,
}

fn emit_children(
    rows: &mut Vec<Row>,
    parent: u64,
    children: &[ImageField],
    kind: ParentKind,
) -> Result<()> {
    let ordered = order_children(children, kind)?;
    for child in ordered {
        emit_child(rows, parent, child)?;
    }
    Ok(())
}

/// Validates locator uniformity and returns the emission order: named/keyed
/// children in ascending byte order, positional children in given order.
///
/// `pub(crate)` so that the M2 semantic walker (`plugin/semantic.rs`) reuses
/// exactly the same canonical ordering as the byte codec (02 §6.3).
pub(crate) fn order_children<'a>(
    children: &'a [ImageField],
    kind: ParentKind,
) -> Result<Vec<&'a ImageField>> {
    if kind == ParentKind::Group {
        for child in children {
            if child.locator != Locator::Positioned {
                return Err(malformed("chunk group children must be positioned"));
            }
            if !matches!(
                child.content,
                ImageContent::ChunkEntry(_) | ImageContent::ChunkGroup { .. }
            ) {
                return Err(malformed(
                    "chunk group child kinds are ChunkEntry or nested ChunkGroup",
                ));
            }
        }
        return Ok(children.iter().collect());
    }
    let mut seen = None;
    for child in children {
        let category = match &child.locator {
            Locator::Positioned => 0,
            Locator::Named(_) => 1,
            Locator::Keyed(_) => 2,
        };
        match seen {
            Some(previous) if previous != category => {
                return Err(malformed(
                    "node children mix positioned/named/keyed locators",
                ));
            }
            _ => seen = Some(category),
        }
    }
    match seen {
        Some(1) => {
            let mut ordered = children
                .iter()
                .map(|child| {
                    let Locator::Named(name) = &child.locator else {
                        unreachable!()
                    };
                    (name.as_bytes().to_vec(), child)
                })
                .collect::<Vec<_>>();
            ordered.sort_by_key(|(bytes, _)| bytes.clone());

            Ok(ordered.into_iter().map(|(_, child)| child).collect())
        }
        Some(2) => {
            let mut ordered = children
                .iter()
                .map(|child| {
                    let Locator::Keyed(key) = &child.locator else {
                        unreachable!()
                    };
                    (*key, child)
                })
                .collect::<Vec<_>>();
            ordered.sort_by_key(|(key, _)| *key);

            Ok(ordered.into_iter().map(|(_, child)| child).collect())
        }
        _ => Ok(children.iter().collect()),
    }
}

fn emit_child(rows: &mut Vec<Row>, parent: u64, child: &ImageField) -> Result<()> {
    let (name, key16) = match &child.locator {
        Locator::Named(name) => (Some(name.clone()), None),
        Locator::Keyed(key) => (None, Some(*key)),
        Locator::Positioned => (None, None),
    };
    match &child.content {
        ImageContent::Node(children) => {
            let row = rows.len() as u64;
            rows.push(Row {
                parent,
                name,
                key16,
                kind: ENTRY_NODE,
                value: None,
                ref_id: None,
                block_kind: None,
                combinator: None,
            });
            emit_children(rows, row, children, ParentKind::Node)
        }
        ImageContent::Inline(value) => {
            rows.push(Row {
                parent,
                name,
                key16,
                kind: ENTRY_INLINE,
                value: Some(value.clone()),
                ref_id: None,
                block_kind: None,
                combinator: None,
            });
            Ok(())
        }
        ImageContent::Ref(id) => {
            rows.push(Row {
                parent,
                name,
                key16,
                kind: ENTRY_REF,
                value: None,
                ref_id: Some(id.as_bytes()),
                block_kind: None,
                combinator: None,
            });
            Ok(())
        }
        ImageContent::ChunkGroup { kind, children } => {
            let row = rows.len() as u64;
            rows.push(Row {
                parent,
                name,
                key16,
                kind: ENTRY_CHUNK_GROUP,
                value: None,
                ref_id: None,
                block_kind: Some(kind.as_str().into_owned()),
                combinator: None,
            });
            emit_children(rows, row, children, ParentKind::Group)
        }
        ImageContent::ChunkEntry(id) => {
            rows.push(Row {
                parent,
                name: None,
                key16: None,
                kind: ENTRY_CHUNK_ENTRY,
                value: None,
                ref_id: Some(id.as_bytes()),
                block_kind: None,
                combinator: None,
            });
            Ok(())
        }
        ImageContent::View(view) => {
            let row = rows.len() as u64;
            rows.push(Row {
                parent,
                name,
                key16,
                kind: ENTRY_VIEW,
                value: None,
                ref_id: None,
                block_kind: None,
                combinator: Some(view.combinator.clone()),
            });
            emit_view(rows, row, view)
        }
    }
}

fn emit_view(rows: &mut Vec<Row>, parent: u64, view: &ImageView) -> Result<()> {
    validate_view(&view.combinator, view.inputs.len(), &view.params)?;
    for input in &view.inputs {
        match input {
            ImageViewInput::Ref(id) => {
                rows.push(Row {
                    parent,
                    name: None,
                    key16: None,
                    kind: ENTRY_REF,
                    value: None,
                    ref_id: Some(id.as_bytes()),
                    block_kind: None,
                    combinator: None,
                });
            }
            ImageViewInput::View(nested) => {
                let row = rows.len() as u64;
                rows.push(Row {
                    parent,
                    name: None,
                    key16: None,
                    kind: ENTRY_VIEW,
                    value: None,
                    ref_id: None,
                    block_kind: None,
                    combinator: Some(nested.combinator.clone()),
                });
                emit_view(rows, row, nested)?;
            }
        }
    }
    for (name, value) in &view.params {
        rows.push(Row {
            parent,
            name: Some(name.clone()),
            key16: None,
            kind: ENTRY_VIEW_PARAM,
            value: Some(value.clone()),
            ref_id: None,
            block_kind: None,
            combinator: None,
        });
    }
    Ok(())
}

fn build_batch(rows: &[Row]) -> Result<Vec<u8>> {
    let parent: Vec<u64> = rows.iter().map(|row| row.parent).collect();
    let name: Vec<Option<&str>> = rows.iter().map(|row| row.name.as_deref()).collect();
    let kind: Vec<u8> = rows.iter().map(|row| row.kind).collect();
    let values: Vec<Value> = rows
        .iter()
        .map(|row| row.value.clone().unwrap_or(Value::Null))
        .collect();
    let block_kind: Vec<Option<&str>> = rows.iter().map(|row| row.block_kind.as_deref()).collect();
    let combinator: Vec<Option<&str>> = rows.iter().map(|row| row.combinator.as_deref()).collect();
    let columns: Vec<ArrayRef> = vec![
        Arc::new(arrow::array::UInt64Array::from(parent)),
        Arc::new(StringArray::from(name)),
        Arc::new(fixed_16(rows.iter().map(|row| row.key16))),
        Arc::new(arrow::array::UInt8Array::from(kind)),
        blocks::arrow::union_array(&values)?,
        Arc::new(fixed_16(rows.iter().map(|row| row.ref_id))),
        Arc::new(StringArray::from(block_kind)),
        Arc::new(StringArray::from(combinator)),
    ];
    let columns = columns
        .into_iter()
        .map(|array| crate::hash::canonicalize_null_slots(&array))
        .collect::<Vec<_>>();
    let batch = RecordBatch::try_new(Arc::new(tree_schema()), columns)
        .map_err(|error| malformed(error.to_string()))?;
    encode_batch(&batch)
}

fn fixed_16(iterator: impl Iterator<Item = Option<[u8; 16]>>) -> FixedSizeBinaryArray {
    FixedSizeBinaryArray::try_from_sparse_iter_with_size(iterator, 16)
        .expect("fixed 16-byte rows build")
}

// ---------------------------------------------------------------------------
// Flat-row reading (decode)
// ---------------------------------------------------------------------------

fn validate_schema(actual: &Schema) -> Result<()> {
    let expected = tree_schema();
    if actual.fields().len() != expected.fields().len() {
        return Err(schema(
            "tree table schema does not match the canonical column set",
        ));
    }
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(schema(
                "tree table schema does not match the canonical column set",
            ));
        }
    }
    Ok(())
}

fn read_rows(batch: &RecordBatch) -> Result<Vec<Row>> {
    if batch.num_rows() == 0 {
        return Err(malformed("tree table has no root row"));
    }
    let parent = batch.column(0);
    let name = batch.column(1);
    let key16 = batch.column(2);
    let kind = batch.column(3);
    let value = batch.column(4);
    let ref_col = batch.column(5);
    let block_kind = batch.column(6);
    let combinator = batch.column(7);
    let parent = parent
        .as_any()
        .downcast_ref::<arrow::array::UInt64Array>()
        .ok_or_else(|| malformed("parent column is not UInt64"))?;
    let name = name
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| malformed("name column is not Utf8"))?;
    let key16 = key16
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| malformed("key16 column is not FixedSizeBinary(16)"))?;
    let kind = kind
        .as_any()
        .downcast_ref::<arrow::array::UInt8Array>()
        .ok_or_else(|| malformed("kind column is not UInt8"))?;
    let ref_col = ref_col
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| malformed("ref column is not FixedSizeBinary(16)"))?;
    let block_kind = block_kind
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| malformed("block_kind column is not Utf8"))?;
    let combinator = combinator
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| malformed("combinator column is not Utf8"))?;
    let union = value
        .as_any()
        .downcast_ref::<UnionArray>()
        .ok_or_else(|| malformed("value column is not the 22-child DenseUnion"))?;
    let values = blocks::arrow::union_to_values(union)?;
    let mut rows = Vec::with_capacity(batch.num_rows());
    for index in 0..batch.num_rows() {
        rows.push(Row {
            parent: parent.value(index),
            name: if name.is_null(index) {
                None
            } else {
                Some(name.value(index).to_owned())
            },
            key16: read_fixed_16(key16, index)?,
            kind: kind.value(index),
            value: Some(values[index].clone()),
            ref_id: read_fixed_16(ref_col, index)?,
            block_kind: if block_kind.is_null(index) {
                None
            } else {
                Some(block_kind.value(index).to_owned())
            },
            combinator: if combinator.is_null(index) {
                None
            } else {
                Some(combinator.value(index).to_owned())
            },
        });
    }
    Ok(rows)
}

fn read_fixed_16(array: &FixedSizeBinaryArray, index: usize) -> Result<Option<[u8; 16]>> {
    if array.is_null(index) {
        return Ok(None);
    }
    let bytes = array.value(index);
    let value: [u8; 16] = bytes
        .try_into()
        .map_err(|_| malformed("fixed 16-byte column value has the wrong width"))?;
    Ok(Some(value))
}

fn validate_rows(rows: &[Row]) -> Result<()> {
    for (index, row) in rows.iter().enumerate() {
        if !(ENTRY_NODE..=ENTRY_VIEW_PARAM).contains(&row.kind) {
            return Err(malformed("invalid entry kind tag"));
        }
        if row.name.is_some() && row.key16.is_some() {
            return Err(malformed("name and key16 are mutually exclusive"));
        }
        if index == 0 {
            if row.parent != ROOT_PARENT {
                return Err(malformed("the first row must be the root sentinel row"));
            }
            if row.kind != ENTRY_NODE {
                return Err(malformed("the root row must be a Node"));
            }
            if row.name.is_some() || row.key16.is_some() {
                return Err(malformed("the root row has no locator"));
            }
        } else {
            if row.parent == ROOT_PARENT {
                return Err(malformed(
                    "the parent sentinel is only allowed on the root row",
                ));
            }
            if row.parent >= index as u64 {
                return Err(malformed("a child row must follow its parent row"));
            }
            let parent = row.parent as usize;
            if !matches!(
                rows[parent].kind,
                ENTRY_NODE | ENTRY_CHUNK_GROUP | ENTRY_VIEW
            ) {
                return Err(malformed("row parent cannot host children"));
            }
        }
        match row.kind {
            ENTRY_NODE => {
                if row.ref_id.is_some() || row.block_kind.is_some() || row.combinator.is_some() {
                    return Err(malformed("Node row carries unexpected columns"));
                }
            }
            ENTRY_INLINE => {
                if row.ref_id.is_some() || row.block_kind.is_some() || row.combinator.is_some() {
                    return Err(malformed("Inline row carries unexpected columns"));
                }
            }
            ENTRY_REF => {
                if row.ref_id.is_none() {
                    return Err(malformed("Ref row is missing its reference"));
                }
                if row.block_kind.is_some() || row.combinator.is_some() {
                    return Err(malformed("Ref row carries unexpected columns"));
                }
            }
            ENTRY_CHUNK_GROUP => {
                if row.block_kind.is_none() {
                    return Err(malformed("ChunkGroup row is missing its block kind"));
                }
                if row.ref_id.is_some() || row.combinator.is_some() {
                    return Err(malformed("ChunkGroup row carries unexpected columns"));
                }
            }
            ENTRY_CHUNK_ENTRY => {
                if row.ref_id.is_none() {
                    return Err(malformed("ChunkEntry row is missing its reference"));
                }
                if row.name.is_some() || row.key16.is_some() {
                    return Err(malformed("ChunkEntry rows are positional"));
                }
                if row.block_kind.is_some() || row.combinator.is_some() {
                    return Err(malformed("ChunkEntry row carries unexpected columns"));
                }
            }
            ENTRY_VIEW => {
                if row.combinator.is_none() {
                    return Err(malformed("View row is missing its combinator"));
                }
                if row.ref_id.is_some() || row.block_kind.is_some() {
                    return Err(malformed("View row carries unexpected columns"));
                }
            }
            ENTRY_VIEW_PARAM => {
                if row.name.is_none() {
                    return Err(malformed("ViewParam row is missing its parameter name"));
                }
                if row.key16.is_some() || row.ref_id.is_some() {
                    return Err(malformed("ViewParam row carries unexpected columns"));
                }
                if row.block_kind.is_some() || row.combinator.is_some() {
                    return Err(malformed("ViewParam row carries unexpected columns"));
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

fn row_indices(rows: &[Row], parent: usize) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, row)| {
            if row.parent as usize == parent {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

fn build_node_fields(rows: &[Row], indices: &[usize]) -> Result<Vec<ImageField>> {
    if indices.is_empty() {
        return Ok(Vec::new());
    }
    let mut category = None;
    for &index in indices {
        let row = &rows[index];
        let local = if row.key16.is_some() {
            2
        } else if row.name.is_some() {
            1
        } else {
            0
        };
        match category {
            Some(previous) if previous != local => {
                return Err(malformed(
                    "node children mix positioned/named/keyed locators",
                ));
            }
            _ => category = Some(local),
        }
    }
    match category {
        Some(0) => {
            for &index in indices {
                if !matches!(rows[index].kind, ENTRY_NODE | ENTRY_INLINE) {
                    return Err(malformed(
                        "positioned node children are Nodes or Inline values",
                    ));
                }
            }
        }
        Some(1) => {
            let mut previous: Option<Vec<u8>> = None;
            for &index in indices {
                if !matches!(
                    rows[index].kind,
                    ENTRY_NODE | ENTRY_INLINE | ENTRY_REF | ENTRY_CHUNK_GROUP | ENTRY_VIEW
                ) {
                    return Err(malformed("named node child has an invalid kind"));
                }
                let name = rows[index].name.clone().unwrap();
                if previous
                    .as_ref()
                    .is_some_and(|previous| previous.as_slice() >= name.as_bytes())
                {
                    return Err(malformed("named siblings must be strictly increasing"));
                }
                previous = Some(name.as_bytes().to_vec());
            }
        }
        Some(2) => {
            let mut previous: Option<[u8; 16]> = None;
            for &index in indices {
                if !matches!(
                    rows[index].kind,
                    ENTRY_NODE | ENTRY_INLINE | ENTRY_REF | ENTRY_CHUNK_GROUP | ENTRY_VIEW
                ) {
                    return Err(malformed("keyed node child has an invalid kind"));
                }
                let key = rows[index].key16.unwrap();
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(malformed("keyed siblings must be strictly increasing"));
                }
                previous = Some(key);
            }
        }
        _ => unreachable!(),
    }
    indices
        .iter()
        .map(|&index| build_field(rows, index))
        .collect()
}

fn build_group_fields(
    rows: &[Row],
    indices: &[usize],
    outer: &BlockKind,
) -> Result<Vec<ImageField>> {
    let mut fields = Vec::with_capacity(indices.len());
    for &index in indices {
        let row = &rows[index];
        if row.name.is_some() || row.key16.is_some() {
            return Err(malformed("chunk group children must be positioned"));
        }
        match row.kind {
            ENTRY_CHUNK_GROUP => {
                let nested = parse_block_kind(row.block_kind.as_deref().unwrap());
                if &nested != outer {
                    return Err(malformed(
                        "nested chunk group kind does not match its parent",
                    ));
                }
                fields.push(build_field(rows, index)?);
            }
            ENTRY_CHUNK_ENTRY => fields.push(build_field(rows, index)?),
            _ => {
                return Err(malformed(
                    "chunk group children are ChunkEntry or nested ChunkGroup",
                ));
            }
        }
    }
    Ok(fields)
}

fn build_field(rows: &[Row], index: usize) -> Result<ImageField> {
    let row = &rows[index];
    let locator = if let Some(key) = row.key16 {
        Locator::Keyed(key)
    } else if let Some(name) = &row.name {
        Locator::Named(name.clone())
    } else {
        Locator::Positioned
    };
    let content = match row.kind {
        ENTRY_NODE => ImageContent::Node(build_node_fields(rows, &row_indices(rows, index))?),
        ENTRY_INLINE => {
            ensure_leaf(rows, index)?;
            ImageContent::Inline(row.value.clone().unwrap_or(Value::Null))
        }
        ENTRY_REF => {
            ensure_leaf(rows, index)?;
            ImageContent::Ref(RefId::from_bytes(
                row.ref_id
                    .ok_or_else(|| malformed("Ref row is missing its reference"))?,
            ))
        }
        ENTRY_CHUNK_GROUP => {
            let kind = parse_block_kind(
                row.block_kind
                    .as_deref()
                    .ok_or_else(|| malformed("ChunkGroup row is missing its block kind"))?,
            );
            ImageContent::ChunkGroup {
                kind: kind.clone(),
                children: build_group_fields(rows, &row_indices(rows, index), &kind)?,
            }
        }
        ENTRY_CHUNK_ENTRY => {
            ensure_leaf(rows, index)?;
            ImageContent::ChunkEntry(RefId::from_bytes(
                row.ref_id
                    .ok_or_else(|| malformed("ChunkEntry row is missing its reference"))?,
            ))
        }
        ENTRY_VIEW => ImageContent::View(build_view(rows, index)?),
        ENTRY_VIEW_PARAM => {
            return Err(malformed("ViewParam rows only appear under View rows"));
        }
        _ => return Err(malformed("invalid entry kind tag")),
    };
    Ok(ImageField { locator, content })
}

fn ensure_leaf(rows: &[Row], index: usize) -> Result<()> {
    if !row_indices(rows, index).is_empty() {
        return Err(malformed("leaf rows cannot have children"));
    }
    Ok(())
}

fn build_view(rows: &[Row], index: usize) -> Result<ImageView> {
    let combinator = rows[index]
        .combinator
        .clone()
        .ok_or_else(|| malformed("View row is missing its combinator"))?;
    let mut inputs = Vec::new();
    let mut params = Vec::new();
    for child_index in row_indices(rows, index) {
        let child = &rows[child_index];
        if child.key16.is_some() {
            return Err(malformed("View children use name parameters, not key16"));
        }
        match child.kind {
            ENTRY_VIEW_PARAM => {
                let name = child
                    .name
                    .clone()
                    .ok_or_else(|| malformed("ViewParam row is missing its parameter name"))?;
                params.push((name, child.value.clone().unwrap_or(Value::Null)));
            }
            ENTRY_REF => {
                if child.name.is_some() {
                    return Err(malformed("view input references are positional"));
                }
                inputs.push(ImageViewInput::Ref(RefId::from_bytes(
                    child
                        .ref_id
                        .ok_or_else(|| malformed("view input reference is missing"))?,
                )));
            }
            ENTRY_VIEW => {
                if child.name.is_some() {
                    return Err(malformed("nested view inputs are positional"));
                }
                inputs.push(ImageViewInput::View(Box::new(build_view(
                    rows,
                    child_index,
                )?)));
            }
            _ => {
                return Err(malformed(
                    "view children are params, inputs, or nested views",
                ));
            }
        }
    }
    validate_view(&combinator, inputs.len(), &params)?;
    Ok(ImageView {
        combinator,
        inputs,
        params,
    })
}

fn parse_block_kind(name: &str) -> BlockKind {
    match name {
        "table" => BlockKind::Table,
        "sequence" => BlockKind::Sequence,
        "kv" => BlockKind::Kv,
        "blob" => BlockKind::Blob,
        other => BlockKind::Named(Arc::<str>::from(other)),
    }
}

/// Validates the closed-kernel combinator invariants. Registered combinators
/// accept any scalar-shaped parameters.
fn validate_view(combinator: &str, inputs: usize, params: &[(String, Value)]) -> Result<()> {
    match combinator {
        "concat" => {
            if !params.is_empty() {
                return Err(malformed("Concat views take no parameters"));
            }
            if inputs < 1 {
                return Err(malformed("Concat views need at least one input"));
            }
        }
        "join" => {
            let mut mode_seen = false;
            for (name, value) in params {
                match name.as_str() {
                    "key" => {
                        if !matches!(value, Value::Utf8(_)) {
                            return Err(malformed("Join key parameters must be Utf8"));
                        }
                    }
                    "mode" => {
                        if mode_seen {
                            return Err(malformed("Join takes at most one mode parameter"));
                        }
                        mode_seen = true;
                        let ok = match value {
                            Value::Utf8(mode) => mode == "inner" || mode == "left",
                            _ => false,
                        };
                        if !ok {
                            return Err(malformed("Join mode must be \"inner\" or \"left\""));
                        }
                    }
                    _ => return Err(malformed("unknown Join view parameter")),
                }
            }
            if inputs != 2 {
                return Err(malformed("Join views take exactly two inputs"));
            }
        }
        "select" => {
            let mut offset_seen = false;
            let mut len_seen = false;
            for (name, value) in params {
                match name.as_str() {
                    "column" => {
                        if !matches!(value, Value::Utf8(_)) {
                            return Err(malformed("Select column parameters must be Utf8"));
                        }
                    }
                    "rows_offset" => {
                        if offset_seen {
                            return Err(malformed("Select rows_offset appears more than once"));
                        }
                        offset_seen = true;
                        if !matches!(value, Value::U64(_)) {
                            return Err(malformed("Select rows_offset must be UInt64"));
                        }
                    }
                    "rows_len" => {
                        if len_seen {
                            return Err(malformed("Select rows_len appears more than once"));
                        }
                        len_seen = true;
                        if !matches!(value, Value::U64(_)) {
                            return Err(malformed("Select rows_len must be UInt64"));
                        }
                    }
                    _ => return Err(malformed("unknown Select view parameter")),
                }
            }
            if offset_seen != len_seen {
                return Err(malformed(
                    "Select rows_offset and rows_len must both appear",
                ));
            }
            if inputs != 1 {
                return Err(malformed("Select views take exactly one input"));
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed projection helpers (used by the TreeCodec derive)
// ---------------------------------------------------------------------------

/// Builds a unique by-name map of the children of a named-field node.
///
/// Positioned and keyed children, and duplicate names, are structural
/// mismatches for a typed node.
pub fn named_child_map<'a>(
    children: &'a [ImageField],
) -> Result<BTreeMap<&'a str, &'a ImageField>> {
    let mut map = BTreeMap::new();
    for child in children {
        let Locator::Named(name) = &child.locator else {
            return Err(schema("positioned or keyed child under a named-field node"));
        };
        if map.insert(name.as_str(), child).is_some() {
            return Err(schema("duplicate named child"));
        }
    }
    Ok(map)
}

/// Removes and returns the child for a required field.
pub fn require_child<'a>(
    map: &mut BTreeMap<&'a str, &'a ImageField>,
    name: &str,
) -> Result<&'a ImageField> {
    map.remove(name)
        .ok_or_else(|| schema(format!("missing required field '{name}'")))
}

/// Removes and returns the child for an optional field.
pub fn take_child<'a>(
    map: &mut BTreeMap<&'a str, &'a ImageField>,
    name: &str,
) -> Result<Option<&'a ImageField>> {
    Ok(map.remove(name))
}

/// Returns the children of a Node image field.
pub fn expect_node<'a>(field: &'a ImageField, what: &str) -> Result<&'a [ImageField]> {
    expect_node_of(&field.content, what)
}

/// Returns the children of a Node image content.
pub fn expect_node_of<'a>(content: &'a ImageContent, what: &str) -> Result<&'a [ImageField]> {
    match content {
        ImageContent::Node(children) => Ok(children),
        _ => Err(schema(format!("field '{what}' is not a node"))),
    }
}

/// Returns the inline value of an Inline image field.
pub fn expect_inline<'a>(field: &'a ImageField, what: &str) -> Result<&'a Value> {
    expect_inline_of(&field.content, what)
}

/// Returns the inline value of an Inline image content.
pub fn expect_inline_of<'a>(content: &'a ImageContent, what: &str) -> Result<&'a Value> {
    match content {
        ImageContent::Inline(value) => Ok(value),
        _ => Err(schema(format!("field '{what}' is not an inline value"))),
    }
}

/// Returns the reference of a Ref image field.
pub fn expect_ref(field: &ImageField, what: &str) -> Result<RefId> {
    expect_ref_of(&field.content, what)
}

/// Returns the reference of a Ref image content.
pub fn expect_ref_of(content: &ImageContent, what: &str) -> Result<RefId> {
    match content {
        ImageContent::Ref(id) => Ok(*id),
        _ => Err(schema(format!("field '{what}' is not a block reference"))),
    }
}

/// Returns the kind and children of a ChunkGroup image field.
pub fn expect_chunk_group<'a>(
    field: &'a ImageField,
    what: &str,
) -> Result<(&'a BlockKind, &'a [ImageField])> {
    match &field.content {
        ImageContent::ChunkGroup { kind, children } => Ok((kind, children)),
        _ => Err(schema(format!("field '{what}' is not a chunk group"))),
    }
}

/// Requires every child to be positioned.
pub fn check_positioned(children: &[ImageField], what: &str) -> Result<()> {
    if children
        .iter()
        .all(|child| child.locator == Locator::Positioned)
    {
        Ok(())
    } else {
        Err(schema(format!(
            "field '{what}' expects positioned children"
        )))
    }
}

/// Requires every child to be named.
pub fn check_named(children: &[ImageField], what: &str) -> Result<()> {
    if children
        .iter()
        .all(|child| matches!(child.locator, Locator::Named(_)))
    {
        Ok(())
    } else {
        Err(schema(format!("field '{what}' expects named children")))
    }
}

/// Requires every child to be keyed.
pub fn check_keyed(children: &[ImageField], what: &str) -> Result<()> {
    if children
        .iter()
        .all(|child| matches!(child.locator, Locator::Keyed(_)))
    {
        Ok(())
    } else {
        Err(schema(format!(
            "field '{what}' expects 16-byte keyed children"
        )))
    }
}

/// Splits a named child into its name and content.
pub fn split_named_field<'a>(
    field: &'a ImageField,
    what: &str,
) -> Result<(&'a str, &'a ImageContent)> {
    match &field.locator {
        Locator::Named(name) => Ok((name, &field.content)),
        _ => Err(schema(format!("field '{what}' expects named keys"))),
    }
}

/// Splits a keyed child into its 16-byte key and content.
pub fn split_keyed_field<'a>(
    field: &'a ImageField,
    what: &str,
) -> Result<(&'a [u8; 16], &'a ImageContent)> {
    match &field.locator {
        Locator::Keyed(key) => Ok((key, &field.content)),
        _ => Err(schema(format!("field '{what}' expects 16-byte keys"))),
    }
}

/// Validates that the image's chunk-group kind is the template's expectation.
pub fn check_chunk_kind(actual: &BlockKind, expected: &BlockKind, what: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(schema(format!(
            "field '{what}' chunk group kind does not match the typed template"
        )))
    }
}

/// Flattens a chunk group into its leaf references, validating nested-group
/// kind consistency and entry shape.
pub fn chunk_refs(kind: &BlockKind, children: &[ImageField], what: &str) -> Result<Vec<RefId>> {
    let mut out = Vec::new();
    for child in children {
        if child.locator != Locator::Positioned {
            return Err(schema(format!(
                "field '{what}' chunk children must be positional"
            )));
        }
        match &child.content {
            ImageContent::ChunkEntry(id) => out.push(*id),
            ImageContent::ChunkGroup {
                kind: inner,
                children: nested,
            } => {
                if inner != kind {
                    return Err(schema(format!(
                        "field '{what}' nested chunk group kind mismatch"
                    )));
                }
                out.extend(chunk_refs(inner, nested, what)?);
            }
            _ => {
                return Err(schema(format!(
                    "field '{what}' has a non-chunk group child"
                )));
            }
        }
    }
    Ok(out)
}

/// A concrete block leaf type that typed projection can reconstruct.
///
/// Built-in block types decode with their fixed codecs; registered block types
/// decode through their adapters.
pub trait DecodeBlock: Sized {
    /// Reconstructs the value from canonical payload bytes.
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self>;
}

impl DecodeBlock for Table {
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self> {
        if kind != &BlockKind::Table {
            return Err(schema("expected a Table block"));
        }
        Table::try_read_blob(payload)
    }
}

impl DecodeBlock for Sequence {
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self> {
        if kind != &BlockKind::Sequence {
            return Err(schema("expected a Sequence block"));
        }
        Sequence::decode(payload)
    }
}

impl DecodeBlock for Kv {
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self> {
        if kind != &BlockKind::Kv {
            return Err(schema("expected a Kv block"));
        }
        Kv::decode(payload)
    }
}

impl DecodeBlock for Blob {
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self> {
        if kind != &BlockKind::Blob {
            return Err(schema("expected a Blob block"));
        }
        Ok(Blob::new(blocks::arrow::decode_blob(payload)?))
    }
}

impl<T: RegisteredBlock + 'static> DecodeBlock for T {
    fn decode_block(kind: &BlockKind, payload: &[u8]) -> Result<Self> {
        if kind != &BlockKind::Named(Arc::<str>::from(T::NAME)) {
            return Err(schema("expected a registered block"));
        }
        T::decode(payload)
    }
}

/// Materializes a block leaf through the bucket, verifying the expected kind.
///
/// A leaf whose identity sits in the bucket's degraded slot (01 §4-5) reports
/// [`ErrorCode::PluginMissing`] with the degraded block's `kind`/`disk`
/// context — a missing plugin means the leaf cannot be materialized (§2-4).
pub fn decode_block_leaf<B: DecodeBlock>(
    id: RefId,
    expected: &BlockKind,
    bucket: &Bucket,
) -> Result<B> {
    let envelope = match bucket.get(id) {
        Some(envelope) => envelope,
        None => return Err(missing_block_error(id, bucket)),
    };
    if &envelope.kind != expected {
        return Err(schema(
            "block kind does not match the typed tree expectation",
        ));
    }
    B::decode_block(expected, &envelope.payload)
}

/// L3 结构面求证（数据验证系统 01 §6.3）：叶子只验 kind / 身份面，不物化
/// payload——与 [`decode_block_leaf`] 相同的 envelope + kind 校验，但**不调用**
/// `B::decode_block`（不展开叶子内容）。由 `#[derive(TreeCodec)]` 生成的
/// `DecodeTree::check_image_children` 在每个叶子位消费。
pub fn check_block_leaf_identity(id: RefId, expected: &BlockKind, bucket: &Bucket) -> Result<()> {
    let envelope = match bucket.get(id) {
        Some(envelope) => envelope,
        None => return Err(missing_block_error(id, bucket)),
    };
    if &envelope.kind != expected {
        return Err(schema(
            "block kind does not match the typed tree expectation",
        ));
    }
    Ok(())
}

/// The leaf materialization error for an identity absent from the healthy
/// envelope slot: a degraded leaf (missing plugin, 01 §4-5) is
/// [`ErrorCode::PluginMissing`] with its kind/disk context; any other missing
/// identity stays [`ErrorCode::DanglingReference`].
fn missing_block_error(id: RefId, bucket: &Bucket) -> TreeSpaceError {
    if let Some(degraded) = bucket.degraded_get(id) {
        return TreeSpaceError::new(
            ErrorCode::PluginMissing,
            "block plugin missing for degraded leaf",
        )
        .with_context("kind", degraded.kind.as_str().as_ref())
        .with_context("disk", degraded.disk.0.clone());
    }
    TreeSpaceError::new(
        ErrorCode::DanglingReference,
        "tree block reference is missing",
    )
}

// ---------------------------------------------------------------------------
// Dynamic node conversion
// ---------------------------------------------------------------------------

fn dynamic_to_fields(node: &DynamicNode) -> Result<Vec<ImageField>> {
    let mut fields = Vec::new();
    for (name, field) in node.fields() {
        let content = match field {
            DynamicField::Node(child) => ImageContent::Node(dynamic_to_fields(child)?),
            DynamicField::Slot(slot) => match slot {
                Slot::Inline(value) => ImageContent::Inline(value.clone()),
                Slot::Ref(id) => ImageContent::Ref(*id),
            },
            DynamicField::Chunks(group) => ImageContent::ChunkGroup {
                kind: group.kind(),
                children: group_to_fields(group)?,
            },
            DynamicField::View(view) => ImageContent::View(view_to_image(view)),
        };
        fields.push(named_field(name, content));
    }
    Ok(fields)
}

fn group_to_fields(group: &ChunkGroup) -> Result<Vec<ImageField>> {
    let mut fields = Vec::new();
    for entry in group.entries() {
        if let Some(nested) = &entry.nested {
            fields.push(positioned_field(ImageContent::ChunkGroup {
                kind: nested.kind(),
                children: group_to_fields(nested)?,
            }));
        } else {
            fields.push(positioned_field(ImageContent::ChunkEntry(
                entry
                    .ref_id
                    .ok_or_else(|| malformed("chunk entry has no reference"))?,
            )));
        }
    }
    Ok(fields)
}

fn dynamic_from_children(children: &[ImageField]) -> Result<DynamicNode> {
    let mut node = DynamicNode::new();
    for (name, field) in named_child_map(children)? {
        let content = match &field.content {
            ImageContent::Node(children) => DynamicField::Node(dynamic_from_children(children)?),
            ImageContent::Inline(value) => DynamicField::Slot(Slot::Inline(value.clone())),
            ImageContent::Ref(id) => DynamicField::Slot(Slot::Ref(*id)),
            ImageContent::ChunkGroup { kind, children } => {
                DynamicField::Chunks(group_from_image(kind, children)?)
            }
            ImageContent::ChunkEntry(_) => {
                return Err(schema("a chunk entry cannot be a dynamic named field"));
            }
            ImageContent::View(view) => DynamicField::View(image_to_view(view)?),
        };
        node.insert(name.to_owned(), content);
    }
    Ok(node)
}

fn group_from_image(kind: &BlockKind, children: &[ImageField]) -> Result<ChunkGroup> {
    let mut entries = Vec::new();
    for child in children {
        if child.locator != Locator::Positioned {
            return Err(schema("chunk group children must be positioned"));
        }
        match &child.content {
            ImageContent::ChunkEntry(id) => {
                entries.push(ChunkEntry::new(*id, kind.clone(), ChunkStats::new()));
            }
            ImageContent::ChunkGroup {
                kind: inner,
                children,
            } => {
                if inner != kind {
                    return Err(schema("nested chunk group kind mismatch"));
                }
                entries.push(ChunkEntry::nested(group_from_image(inner, children)?));
            }
            _ => {
                return Err(schema(
                    "chunk group child is not a chunk entry or nested group",
                ));
            }
        }
    }
    Ok(ChunkGroup::from_parts(Some(kind.clone()), entries))
}

/// Converts a model view node into its universal image form.
pub fn view_to_image(view: &ViewNode) -> ImageView {
    let inputs = view
        .inputs()
        .iter()
        .map(|input| match input {
            ViewInput::Ref(id) => ImageViewInput::Ref(*id),
            ViewInput::View(nested) => ImageViewInput::View(Box::new(view_to_image(nested))),
        })
        .collect::<Vec<_>>();
    let (combinator, params) = match view.combinator() {
        Combinator::Concat => ("concat".to_owned(), Vec::new()),
        Combinator::Join { keys, mode } => {
            let mut params = keys
                .iter()
                .map(|key| ("key".to_owned(), Value::Utf8(key.clone())))
                .collect::<Vec<_>>();
            params.push((
                "mode".to_owned(),
                Value::Utf8(
                    match mode {
                        JoinMode::Inner => "inner",
                        JoinMode::Left => "left",
                    }
                    .to_owned(),
                ),
            ));
            ("join".to_owned(), params)
        }
        Combinator::Select { columns, rows } => {
            let mut params = columns
                .iter()
                .map(|column| ("column".to_owned(), Value::Utf8(column.clone())))
                .collect::<Vec<_>>();
            if let Some((offset, length)) = rows {
                params.push(("rows_offset".to_owned(), Value::U64(*offset as u64)));
                params.push(("rows_len".to_owned(), Value::U64(*length as u64)));
            }
            ("select".to_owned(), params)
        }
        Combinator::Registered(name) => (name.clone(), Vec::new()),
    };
    ImageView {
        combinator,
        inputs,
        params,
    }
}

fn utf8_param(value: &Value, what: &str) -> Result<String> {
    match value {
        Value::Utf8(value) => Ok(value.clone()),
        _ => Err(malformed(format!("{what} parameter must be Utf8"))),
    }
}

fn u64_param(value: &Value, what: &str) -> Result<u64> {
    match value {
        Value::U64(value) => Ok(*value),
        _ => Err(malformed(format!("{what} parameter must be UInt64"))),
    }
}

/// Converts a universal view image back into a model view node.
///
/// Registered views with parameters cannot be represented by [`ViewNode`] and
/// are rejected.
pub fn image_to_view(image: &ImageView) -> Result<ViewNode> {
    validate_view(&image.combinator, image.inputs.len(), &image.params)?;
    let combinator = match image.combinator.as_str() {
        "concat" => Combinator::Concat,
        "join" => {
            let mut keys = Vec::new();
            let mut mode = JoinMode::Inner;
            for (name, value) in &image.params {
                match name.as_str() {
                    "key" => keys.push(utf8_param(value, "Join key")?),
                    "mode" => {
                        mode = match utf8_param(value, "Join mode")?.as_str() {
                            "inner" => JoinMode::Inner,
                            "left" => JoinMode::Left,
                            _ => {
                                return Err(malformed("Join mode must be \"inner\" or \"left\""));
                            }
                        };
                    }
                    _ => unreachable!("join parameters validated"),
                }
            }
            Combinator::Join { keys, mode }
        }
        "select" => {
            let mut columns = Vec::new();
            let mut offset = None;
            let mut length = None;
            for (name, value) in &image.params {
                match name.as_str() {
                    "column" => columns.push(utf8_param(value, "Select column")?),
                    "rows_offset" => offset = Some(u64_param(value, "rows_offset")?),
                    "rows_len" => length = Some(u64_param(value, "rows_len")?),
                    _ => unreachable!("select parameters validated"),
                }
            }
            let rows = match (offset, length) {
                (Some(offset), Some(length)) => Some((offset as usize, length as usize)),
                (None, None) => None,
                _ => unreachable!("select rows validated"),
            };
            Combinator::Select { columns, rows }
        }
        other => {
            if !image.params.is_empty() {
                return Err(schema(
                    "registered view parameters cannot be represented by a ViewNode",
                ));
            }
            Combinator::Registered(other.to_owned())
        }
    };
    let inputs = image
        .inputs
        .iter()
        .map(|input| match input {
            ImageViewInput::Ref(id) => Ok(ViewInput::Ref(*id)),
            ImageViewInput::View(nested) => Ok(ViewInput::View(Box::new(image_to_view(nested)?))),
        })
        .collect::<Result<Vec<_>>>()?;
    ViewNode::new(combinator, inputs)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn malformed(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

fn schema(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Time32Unit, TimestampUnit};

    fn row(
        parent: u64,
        name: Option<&str>,
        kind: u8,
        value: Option<Value>,
        ref_id: Option<[u8; 16]>,
        block_kind: Option<&str>,
        combinator: Option<&str>,
    ) -> Row {
        Row {
            parent,
            name: name.map(str::to_owned),
            key16: None,
            kind,
            value,
            ref_id,
            block_kind: block_kind.map(str::to_owned),
            combinator: combinator.map(str::to_owned),
        }
    }

    fn tree_bytes(rows: Vec<Row>) -> Vec<u8> {
        build_batch(&rows).unwrap()
    }

    fn root_row() -> Row {
        row(ROOT_PARENT, None, ENTRY_NODE, None, None, None, None)
    }

    /// Builds a value-union column carrying arbitrary type ids (used to craft
    /// malformed type-id cases that cannot pass `UnionArray::try_new`).
    fn union_with_ids(ids: Vec<i8>) -> ArrayRef {
        let len = ids.len();
        let DataType::Union(fields, _mode) = tree_schema().field(4).data_type().clone() else {
            unreachable!("the value column is always the union")
        };
        let children = (0..22)
            .map(|id| blocks::arrow::empty_child_array(id))
            .collect::<Vec<_>>();
        Arc::new(unsafe {
            UnionArray::new_unchecked(
                fields,
                arrow::buffer::ScalarBuffer::from(ids),
                Some(arrow::buffer::ScalarBuffer::from(vec![0; len] as Vec<i32>)),
                children,
            )
        })
    }

    fn fixed_16_rows(rows: usize) -> ArrayRef {
        Arc::new(fixed_16((0..rows).map(|_| None::<[u8; 16]>)))
    }

    fn name_col(rows: usize) -> ArrayRef {
        Arc::new(StringArray::from(vec![None::<&str>; rows]))
    }

    #[test]
    fn a2_1_value_union_type_id_out_of_range_is_rejected() {
        let columns = vec![
            Arc::new(arrow::array::UInt64Array::from(vec![u64::MAX])) as ArrayRef,
            name_col(1),
            fixed_16_rows(1),
            Arc::new(arrow::array::UInt8Array::from(vec![ENTRY_INLINE])) as ArrayRef,
            union_with_ids(vec![50]),
            fixed_16_rows(1),
            name_col(1),
            name_col(1),
        ];
        let batch = RecordBatch::try_new(Arc::new(tree_schema()), columns).unwrap();
        let bytes = encode_batch(&batch).unwrap();
        let error = decode(&bytes).unwrap_err();
        assert!(
            error.code == ErrorCode::PayloadMalformed,
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn a2_1_parameter_bag_values_roundtrip() {
        let image = TreeImage::new(vec![named_field(
            "time",
            ImageContent::Inline(Value::Time32(Time32Unit::Millisecond, 822)),
        )]);
        assert_eq!(decode(&encode(&image).unwrap()).unwrap(), image);

        let image = TreeImage::new(vec![named_field(
            "stamp",
            ImageContent::Inline(Value::Timestamp(
                TimestampUnit::Nanosecond,
                Some("UTC".into()),
                7,
            )),
        )]);
        assert_eq!(decode(&encode(&image).unwrap()).unwrap(), image);
    }

    #[test]
    fn a2_3_view_decode_rejects_wrong_closed_kernel_arity() {
        let rows = vec![
            root_row(),
            row(0, Some("v"), ENTRY_VIEW, None, None, None, Some("join")),
            row(1, None, ENTRY_REF, None, Some([0xab; 16]), None, None),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "Join views take exactly two inputs"
        );
    }

    #[test]
    fn a2_3_view_decode_rejects_unknown_closed_kernel_param() {
        let rows = vec![
            root_row(),
            row(0, Some("v"), ENTRY_VIEW, None, None, None, Some("select")),
            row(1, None, ENTRY_REF, None, Some([0xab; 16]), None, None),
            row(
                1,
                Some("bogus"),
                ENTRY_VIEW_PARAM,
                Some(Value::Utf8("x".into())),
                None,
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "unknown Select parameter must be rejected"
        );
    }

    #[test]
    fn a2_3_view_decode_rejects_concat_with_params() {
        let rows = vec![
            root_row(),
            row(0, Some("v"), ENTRY_VIEW, None, None, None, Some("concat")),
            row(1, None, ENTRY_REF, None, Some([0xab; 16]), None, None),
            row(
                1,
                Some("p"),
                ENTRY_VIEW_PARAM,
                Some(Value::U64(9)),
                None,
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "Concat takes no parameters"
        );
    }

    #[test]
    fn a2_3_group_decode_rejects_missing_block_kind() {
        let rows = vec![
            root_row(),
            row(0, Some("chunks"), ENTRY_CHUNK_GROUP, None, None, None, None),
            row(
                1,
                None,
                ENTRY_CHUNK_ENTRY,
                None,
                Some([0xab; 16]),
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "ChunkGroup requires a block kind"
        );
    }

    #[test]
    fn a2_3_group_decode_rejects_nested_kind_mismatch() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("chunks"),
                ENTRY_CHUNK_GROUP,
                None,
                None,
                Some("table"),
                None,
            ),
            row(1, None, ENTRY_CHUNK_GROUP, None, None, Some("blob"), None),
            row(
                2,
                None,
                ENTRY_CHUNK_ENTRY,
                None,
                Some([0xab; 16]),
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "nested chunk group kinds must agree"
        );
    }

    #[test]
    fn a2_3_leaf_row_with_children_is_rejected() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("x"),
                ENTRY_INLINE,
                Some(Value::I32(1)),
                None,
                None,
                None,
            ),
            row(1, None, ENTRY_INLINE, Some(Value::I32(2)), None, None, None),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "inline leaves cannot have children"
        );
    }

    #[test]
    fn a2_3_view_param_outside_view_parent_is_rejected() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("p"),
                ENTRY_VIEW_PARAM,
                Some(Value::U64(9)),
                None,
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "ViewParam rows only appear under View rows"
        );
    }

    #[test]
    fn a2_1_decode_rejects_mixed_named_and_positioned_children() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("named"),
                ENTRY_INLINE,
                Some(Value::I32(1)),
                None,
                None,
                None,
            ),
            row(0, None, ENTRY_INLINE, Some(Value::I32(2)), None, None, None),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "Node children cannot mix locator kinds"
        );
    }

    #[test]
    fn a2_1_decode_rejects_ref_row_without_reference() {
        let rows = vec![
            root_row(),
            row(0, Some("blk"), ENTRY_REF, None, None, None, None),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "Ref rows must carry a reference"
        );
    }

    #[test]
    fn a2_3_decode_rejects_chunk_entry_with_name() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("chunks"),
                ENTRY_CHUNK_GROUP,
                None,
                None,
                Some("table"),
                None,
            ),
            row(
                1,
                Some("oops"),
                ENTRY_CHUNK_ENTRY,
                None,
                Some([0xab; 16]),
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "ChunkEntry rows are positional"
        );
    }

    #[test]
    fn a2_3_decode_rejects_parent_pointing_at_a_leaf_row() {
        let rows = vec![
            root_row(),
            row(
                0,
                Some("x"),
                ENTRY_INLINE,
                Some(Value::I32(1)),
                None,
                None,
                None,
            ),
            row(
                1,
                Some("child"),
                ENTRY_INLINE,
                Some(Value::I32(2)),
                None,
                None,
                None,
            ),
        ];
        assert!(
            decode(&tree_bytes(rows)).is_err(),
            "only node/group/view rows can host children"
        );
    }
}
