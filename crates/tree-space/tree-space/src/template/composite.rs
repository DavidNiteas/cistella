//! Structured composite specifications and conflict reports.
//!
//! A composite is a structured constant -- the host template plus the ordered
//! mount list -- never a folded flat shape. Folding happens deterministically
//! in the runtime projection (host first, then mounts in declaration order);
//! compile-time checks operate on the structured form directly. Composites do
//! not implement [`TreeTemplate`](crate::template::TreeTemplate); a larger
//! composition re-declares its members in a new `tree_compose!` invocation.

use super::spec::{MAX_SLOT_DEPTH, SlotName, TemplateSpec};

/// Maximum number of name segments materialized in a [`ConflictReport`].
///
/// Paths longer than this are truncated in the report (the checks themselves
/// compare the full paths; the report is a diagnostic).
pub const MAX_PATH_SEGMENTS: usize = 16;

/// One mount declaration of a composite: which template attaches where, with
/// which role label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MountSpec {
    /// The coordinate slot this template's `Inherit` content attaches to.
    pub slot: SlotName,
    /// The role label of the `as` clause (empty when omitted). Compile-time
    /// trace of the semantic role injected by the mount position (semantic 7).
    pub label: &'static str,
    /// The mounted template's `TreeTemplate::NAME`.
    pub template_name: &'static str,
    /// The mounted template's shape.
    pub spec: TemplateSpec,
}

/// The structured composite specification produced by `tree_compose!`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompositeSpec {
    /// The composite name (the macro's `stringify!` of its identifier).
    pub name: &'static str,
    /// The host template's `TreeTemplate::NAME`.
    pub host_name: &'static str,
    /// The host template shape; it owns the composite's coordinate system.
    pub host: TemplateSpec,
    /// The ordered mount list (declaration order = merge fold order).
    pub mounts: &'static [MountSpec],
    /// `true` = merge mode (later cross-template claims override earlier
    /// ones); `false` = orthogonal mode (cross-template conflicts are
    /// compile-time errors, identical fingerprints deduplicate per P4).
    pub merge: bool,
}

/// The kind of the first conflict found in a composite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConflictKind {
    /// Two claims declare the same path as tables with different structural
    /// fingerprints under orthogonal composition (or within one template).
    LeafFingerprintMismatch,
    /// One claim declares a path as a table while another claim (explicit or
    /// implied by a deeper path) declares the same path as a domain.
    LeafVsDomain,
    /// A mount references a slot the host coordinate system does not declare.
    UnknownMountSlot,
    /// A member template requires a slot the host coordinate system does not
    /// declare.
    RequiredSlotUncovered,
    /// An `At` anchor is not a valid coordinate path of the host coordinate
    /// system.
    IllegalAnchor,
    /// The host coordinate system is malformed (unknown parent slot,
    /// duplicate slot, or exceeding the slot-depth bound).
    MalformedCoordinates,
    /// A slot chain exceeds [`MAX_SLOT_DEPTH`].
    ChainTooDeep,
    /// An endpoint reference points at a `(template, leaf, column)` absent
    /// from the composite.
    EndpointMissing,
    /// An endpoint reference resolves but the referenced column type differs
    /// from the declared expectation.
    EndpointTypeMismatch,
}

/// The diagnostic report of the first conflict found in a composite.
///
/// Built by the compile-time checks; `None` reports mean the composite is
/// clean. Paths are materialized into fixed buffers and truncated at
/// [`MAX_SLOT_DEPTH`] / [`MAX_PATH_SEGMENTS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConflictReport {
    /// The conflict kind.
    pub kind: ConflictKind,
    /// The left source (`host_name` or the mounted template name).
    pub left: &'static str,
    /// The right source (`host_name` or the mounted template name).
    pub right: &'static str,
    /// The slot segments of the conflicting path (truncated).
    pub path_slots: [SlotName; MAX_SLOT_DEPTH],
    /// The number of valid entries in [`ConflictReport::path_slots`].
    pub path_slots_len: usize,
    /// The name segments of the conflicting path (truncated).
    pub path_names: [&'static str; MAX_PATH_SEGMENTS],
    /// The number of valid entries in [`ConflictReport::path_names`].
    pub path_names_len: usize,
}
