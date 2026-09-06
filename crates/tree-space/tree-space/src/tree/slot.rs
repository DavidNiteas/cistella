//! TB tree slots: inline scalar values or content-addressed block references.

use crate::block::{RefId, Value};

/// The non-generic TB slot model.
///
/// Scalar values are stored inline. Block payloads are represented only by
/// their content reference; payload bytes live in [`crate::Bucket`].
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    /// An inline scalar value.
    Inline(Value),
    /// A reference to a bucket block.
    Ref(RefId),
}

impl Slot {
    /// Returns the referenced block identity, if this is an external slot.
    pub const fn ref_id(&self) -> Option<RefId> {
        match self {
            Self::Inline(_) => None,
            Self::Ref(id) => Some(*id),
        }
    }

    /// Returns whether the slot is an external block reference.
    pub const fn is_ref(&self) -> bool {
        matches!(self, Self::Ref(_))
    }

    /// Returns the inline value, if present.
    pub const fn inline_value(&self) -> Option<&Value> {
        match self {
            Self::Inline(value) => Some(value),
            Self::Ref(_) => None,
        }
    }
}
