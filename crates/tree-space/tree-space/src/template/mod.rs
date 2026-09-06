//! Compile-time tree template composition (the modular-subtree mechanism).
//!
//! The template layer is a pure addition on top of the tree-space kernel: it
//! lets a crate declare small tree templates as Rust constants
//! ([`TreeTemplate`]), compose them into big tree templates at compile time
//! ([`crate::tree_compose`]), and project a composite into the runtime type set
//! ([`crate::template::project()`]). It never touches instances, storage layouts, or the
//! exchange surface -- runtime tree structure stays immutable, instance
//! fusion does not exist, and the read-side tolerance of the kernel is
//! untouched.
//!
//! Semantics (work order document 01, seven rulings):
//!
//! 1. Composition happens at compile time: templates are Rust type/const
//!    items and `tree_compose!` is a declarative macro; a composite is a
//!    structured constant, never a runtime value transformation.
//! 2. Only templates fuse, never instances: no API here accepts a library
//!    handle or any instance data.
//! 3. No runtime composition: there is no runtime API that produces a new
//!    tree structure; the only runtime surface is the projection.
//! 4. Tree nesting (a leaf that is another tree) is a deferred runtime
//!    capability and is not modeled here.
//! 5. Two composition modes: orthogonal (cross-template path conflicts are
//!    compile-time errors; identical table fingerprints deduplicate, ruling
//!    P4) and merge (later mounts override earlier ones leaf-by-leaf in
//!    declaration order).
//! 6. Read-side tolerance is a kernel property (`open_multi` foreign skip)
//!    and needs no template-layer mechanism.
//! 7. Multi-position derivation: a template is pure structure; mount
//!    position and role label are injected by the composition declaration
//!    against an extensible coordinate system ([`SlotName`]; `study`/`run`
//!    now, `lib` reserved), and the same template may mount at several
//!    positions.
//!
//! Layer map: this module sits between the downstream business-structure
//! face and the runtime type registry. It adds
//! `tree-space/src/template/**` plus one `lib.rs` line and changes nothing
//! else in the kernel (zero-break constraint, work order document 02
//! section 4.3).
//!
//! Type-id note (P2-1 ruling): the ids projected via [`domain_type_id`] /
//! [`table_type_id`] are **provisional plan-internal identities** -- the
//! template layer has no owner dimension and cannot reproduce the
//! authoritative uni-mass-db persist typeid derivation. Authoritative
//! TypeIds are re-derived at the uni-mass-db projection boundary (S2) under
//! the existing typeid rules with the owner injected, before entering
//! persistence and manifest reconciliation. The cross-crate equivalence
//! guard is a structural comparison **after** that authoritative
//! re-derivation; [`TypeRegistryPlan::equivalent_to`] is the in-crate
//! structural form of the same comparison (before re-derivation), and the
//! uni-mass-db-side comparison lands in S2 where the dependency direction
//! allows it.
//!
//! Naming constraint: projected domain names are the node segment names.
//! Two domain nodes sharing `(name, version)` across different positions
//! register only when their shapes are identical (identical shapes
//! deduplicate); differing shapes are an `IdentityCollision` at
//! [`crate::registry::TypeRegistry`] registration. Template authors must
//! therefore give
//! distinct internal domain names to same-named structures placed at
//! different positions (see the geo-consensus precedent in the work-order
//! design document: a flat study-level leaf instead of a second same-named
//! domain).

pub mod check;
pub mod compose;
pub mod composite;
pub mod project;
pub mod spec;

pub use check::composite_check;
pub use composite::{CompositeSpec, ConflictKind, ConflictReport, MAX_PATH_SEGMENTS, MountSpec};
pub use project::{
    LIST_ELEMENT_NAME, ShapeMapping, SlotOrder, TypeRegistryPlan, data_type, domain_type_id,
    field_of, project, table_type_id,
};
pub use spec::{
    ConstScalar, ConstStructField, ConstTimeUnit, ConstType, EndpointRef, IndexKindConst,
    MAX_SLOT_DEPTH, SlotCtx, SlotDecl, SlotName, TemplateColumn, TemplateEntry, TemplateEntryKind,
    TemplateIndex, TemplateSpec, TemplateTable, TierDecl, TreeTemplate,
};

#[cfg(test)]
mod tests;
