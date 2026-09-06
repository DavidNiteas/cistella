//! Compile-time node metadata consumed by the tree runtime.
//!
//! These types are generated *into* by `#[derive(TreeNode)]` from the
//! `tree-space-derive` crate; they are the runtime reflection of a node's
//! structure (fields, multiplicities, child/leaf targets).

use crate::block::BlockKind;

/// The multiplicity of a node field, inferred from its Rust type by the derive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Multiplicity {
    /// Exactly one child/leaf (`T`).
    Single,
    /// At most one child/leaf (`Option<T>`), i.e. nullable.
    Optional,
    /// Any number of ordered children/leaves (`Vec<T>`); xpath `[i]`.
    Sequence,
    /// Any number of keyed children/leaves (`BTreeMap<K, T>`); xpath `[key]`.
    Map,
}

/// The target of a field: a sub-node or a leaf.
#[derive(Clone, Debug)]
pub enum FieldTarget {
    /// A sub-node; its metadata is the referenced node type's `META`.
    Node(&'static NodeMeta),
    /// A leaf; carries its kind and a static schema descriptor.
    /// An inline TB scalar value.
    Inline {
        /// A static scalar descriptor.
        schema: &'static LeafSchemaMeta,
    },
    /// A TB block field with a closed block kind and static descriptor.
    Block {
        /// The block runtime kind.
        kind: BlockKind,
        /// A static opaque block descriptor name.
        schema: &'static LeafSchemaMeta,
    },
}

/// The runtime target category of a leaf position.
#[derive(Clone, Debug)]
pub enum LeafTarget {
    /// An inline scalar value.
    Value,
    /// An external block reference.
    Block(BlockKind),
}

/// Static descriptor of a leaf field's schema.
#[derive(Clone, Copy, Debug)]
pub struct LeafSchemaMeta {
    /// A stable human-readable schema name (for example `"Table"`).
    pub name: &'static str,
}

/// Static metadata of one struct field.
#[derive(Clone, Debug)]
pub struct FieldMeta {
    /// The field name (xpath step text).
    pub name: &'static str,
    /// The inferred multiplicity.
    pub multiplicity: Multiplicity,
    /// The field target (node or leaf).
    pub target: FieldTarget,
    /// Whether the field is annotated `#[tree_space(ephemeral)]` — the field's
    /// subtree is not persisted and reopens as the field type's `Default`
    /// (see `_dev/树与桶管道/01-目标与设计.md` §1.9.8 (a)).
    pub ephemeral: bool,
}

/// Static metadata of a node struct.
#[derive(Clone, Copy, Debug)]
pub struct NodeMeta {
    /// The Rust type name of the node.
    pub type_name: &'static str,
    /// Ordered field metadata, in declaration order.
    pub fields: &'static [FieldMeta],
}
