//! TB tree runtime: node metadata, xpath navigation, tree instances and
//! temporary (view) nodes. Tree positions hold scalar values inline or block
//! references; block payloads live in [`crate::Bucket`].

pub mod meta;

pub use meta::{FieldMeta, FieldTarget, LeafSchemaMeta, LeafTarget, Multiplicity, NodeMeta};
pub use slot::Slot;

use crate::block::RefId;
use crate::block::Value;
use crate::error::Result;
use crate::xpath::XPath;

/// Static per-node metadata, delivered by a dedicated trait so that the
/// (navigation) [`TreeNode`] trait stays dyn-compatible.
pub trait TreeNodeMeta: 'static {
    /// The static metadata table of this node type.
    const META: &'static NodeMeta;

    /// Collects the ephemeral xpath prefixes declared by this node's template
    /// fields, as absolute xpaths from a tree rooted at `Self`.
    ///
    /// Every field annotated `#[tree_space(ephemeral)]` contributes its xpath
    /// at its depth on the template DAG; ephemeral fields do not descend into
    /// their node target, while non-ephemeral node fields recurse into their
    /// sub-node metadata (see `_dev/树与桶管道/01-目标与设计.md` §1.9.8 (a)5).
    /// Call this on the committed tree's root node type only; nested node
    /// types' own prefixes are folded in by the root traversal.
    fn ephemeral_prefixes() -> Vec<crate::xpath::XPath> {
        let mut out = Vec::new();
        collect_ephemeral_prefixes(Self::META, &crate::xpath::XPath::root(), &mut out);
        out
    }
}

/// Traverses a node's metadata DAG collecting ephemeral field prefixes.
fn collect_ephemeral_prefixes(meta: &'static NodeMeta, prefix: &XPath, out: &mut Vec<XPath>) {
    for field in meta.fields {
        let path = prefix.clone().field(field.name);
        if field.ephemeral {
            out.push(path);
            continue;
        }
        if let FieldTarget::Node(target) = &field.target {
            collect_ephemeral_prefixes(target, &path, out);
        }
    }
}

/// The node trait of TB trees.
///
/// Every node struct `#[derive(TreeNode)]s` this trait. The derive supplies
/// the static metadata table (via [`TreeNodeMeta`]) and the xpath navigation
/// entry points; the compile-time guarantee that the tree is an acyclic,
/// statically shaped DAG comes from Rust's own type system.
///
/// The trait is dyn-compatible (no `Self: Sized` supertrait or method), so the
/// weakly typed bridge can hold `Box<dyn TreeNode>`. The derived `get` may
/// `Box::new(self.field.clone())` — cloning happens on the concrete type,
/// outside this trait.
pub trait TreeNode: 'static {
    /// Navigates one xpath and returns the targeted leaf or sub-node.
    fn get(&self, xpath: &XPath) -> Result<AccessOut>;

    /// Navigates as far as possible, returning the reached value and the
    /// unconsumed suffix of the xpath.
    ///
    /// Unlike [`TreeNode::get`], this never fails: on an unreachable step it
    /// returns the deepest reachable value together with the remaining steps
    /// that could not be consumed. This is the best-effort bridge used during
    /// instance construction/de-collection, where the walker needs to stop at
    /// the frontier without losing the unconsumed address.
    fn get_best_effort(&self, xpath: &XPath) -> (AccessOut, XPath);

    /// Writes an inline value or block reference into the targeted slot.
    ///
    /// This is the weakly typed write bridge. Implementations store the value
    /// according to the field's target: inline fields accept
    /// `Slot::Inline(Value)`, dynamic nodes store the slot directly, and typed
    /// block fields are not writable through the bridge (see
    /// `_dev/archive/树与桶模型改造/01-目标与设计.md` §4.2.3).
    fn set(&mut self, xpath: &XPath, v: Slot) -> Result<()>;

    /// Collects every leaf reference in this subtree, with its absolute xpath.
    fn leaf_refs(&self) -> Vec<(XPath, RefId)>;
}

/// The weakly typed result of [`TreeNode::get`].
///
/// Block leaf positions are always answered by their identity ([`AccessOut::Ref`]);
/// the bridge never carries block payloads (see
/// `_dev/archive/树与桶模型改造/01-目标与设计.md` §4.2.2).
pub enum AccessOut {
    /// An inline TB scalar value.
    Value(Value),
    /// A sub-node (type-erased).
    Node(Box<dyn TreeNode>),
    /// A block leaf answered by identity (payload lives in the bucket and is
    /// materialized explicitly via [`crate::Bucket::get`]).
    Ref(RefId),
}

impl std::fmt::Debug for AccessOut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(value) => formatter.debug_tuple("Value").field(value).finish(),
            Self::Node(_) => formatter.write_str("Node(..)"),
            Self::Ref(id) => formatter.debug_tuple("Ref").field(&id).finish(),
        }
    }
}

/// A tree instance is a statically shaped, lightly held structure: it carries
/// topology (cardinalities) and leaf references, never leaf payloads.
pub trait TreeInstance: TreeNode {
    /// Constructs a structurally complete empty instance: every field is its
    /// default (`Option` = `none`, containers = empty, leaves = their
    /// `Default`). Requires every field type to implement `Default`.
    fn new_empty() -> Self
    where
        Self: Sized;
}

/// Uniform "value → weak outcome" conversion used by derive-generated `get`.
pub trait LeafToOut {
    /// Converts this leaf-shaped value into a weakly typed access outcome.
    fn to_out(&self) -> AccessOut;
}

impl LeafToOut for crate::block::Value {
    fn to_out(&self) -> AccessOut {
        AccessOut::Value(self.clone())
    }
}

macro_rules! impl_block_to_out {
    ($ty:ty) => {
        impl LeafToOut for $ty {
            fn to_out(&self) -> AccessOut {
                // Block leaves answer by identity; the payload is never
                // cloned onto the bridge (see
                // `_dev/archive/树与桶模型改造/01-目标与设计.md` §4.2.1).
                AccessOut::Ref(LeafRefOf::leaf_ref(self))
            }
        }
        impl LeafRefOf for $ty {
            fn leaf_ref(&self) -> RefId {
                crate::block::Block::ref_id(self)
            }
        }
    };
}

impl_block_to_out!(crate::block::Table);
impl_block_to_out!(crate::block::Sequence);
impl_block_to_out!(crate::block::Kv);
impl_block_to_out!(crate::block::Blob);

impl LeafToOut for Slot {
    fn to_out(&self) -> AccessOut {
        match self {
            Slot::Inline(value) => AccessOut::Value(value.clone()),
            Slot::Ref(id) => AccessOut::Ref(*id),
        }
    }
}

/// Uniform "leaf-shaped value → content reference" conversion used by
/// derive-generated `leaf_refs`.
pub trait LeafRefOf {
    /// Returns the content reference of this leaf-shaped value.
    fn leaf_ref(&self) -> RefId;
}

impl LeafRefOf for Slot {
    fn leaf_ref(&self) -> RefId {
        self.ref_id()
            .expect("inline values have no block reference")
    }
}

pub mod codec;
mod dynamic;
mod slot;
mod view;

pub use codec::{
    DecodeBlock, DecodeTree, EncodeTree, ImageContent, ImageField, ImageView, ImageViewInput,
    Locator, TreeId, TreeImage, check_chunk_kind, check_keyed, check_named, check_positioned,
    chunk_refs, decode, decode_block_leaf, encode, encode_node, expect_chunk_group, expect_inline,
    expect_inline_of, expect_node, expect_node_of, expect_ref, expect_ref_of, from_image,
    image_to_view, keyed_field, named_child_map, named_field, positioned_field, project,
    require_child, split_keyed_field, split_named_field, take_child, to_image, tree_id,
    tree_schema, view_to_image,
};
pub use dynamic::{
    ChunkEntry, ChunkGroup, ChunkStats, DynamicField, DynamicNode, Persistence, memory_gc_roots,
};
pub use view::{
    Combinator, CombinatorRegistry, JoinMode, RegisteredResolver, ViewInput, ViewNode, resolve,
    resolve_with_registry,
};
