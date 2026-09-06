//! v5 domain tags: marker traits orthogonal to tree structure.
//!
//! A domain tag is a compile-time property label attached to a leaf type via
//! `impl SomeDomain for T`. It carries no runtime state and does not affect
//! content addressing; it only exists to express shared properties through
//! trait bounds. `Leaf` itself is the built-in "leaf domain" that every leaf
//! necessarily satisfies.

use crate::block::Block;

/// Tags a leaf as a table-like value ("table domain").
///
/// The built-in [`crate::block::Table`] implements this; users may attach it
/// to their own leaf types to express that they behave like tables.
pub trait TableDomain: Block {}

/// Tags a leaf as carrying spatial semantics ("spatial domain").
///
/// Marker only; spatial query support lives up-stack (for example the
/// `table-index` crate), not here.
pub trait SpatialDomain: Block {}

impl TableDomain for crate::block::Table {}

/// Static assertion helper: requires `T: Leaf` at compile time.
pub fn assert_leaf<T: Block>() {}

/// Static assertion helper: requires `T: TreeNode` at compile time.
pub fn assert_tree_node<T: crate::TreeNode>() {}
