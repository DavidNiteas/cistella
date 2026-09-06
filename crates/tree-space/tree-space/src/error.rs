//! Stable, machine-readable error categories.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

/// Stable error category exposed by every public operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ErrorCode {
    /// A table path violates the portable path grammar.
    PathInvalid,
    /// A name is reserved for the tree-space ABI.
    NameReserved,
    /// A requested type definition does not exist.
    TypeNotFound,
    /// Type definitions conflict.
    TypeConflict,
    /// A composition graph contains a cycle.
    CompositionCycle,
    /// A child occurrence violates cardinality.
    CardinalityViolation,
    /// Physical Arrow schema differs from its declared type.
    SchemaMismatch,
    /// Manifest data is malformed or incomplete.
    ManifestInvalid,
    /// Bootstrap cannot produce a complete immutable snapshot.
    BootstrapIncomplete,
    /// An identity or hash collision cannot be represented safely.
    IdentityCollision,
    /// A target cannot be modified in its current state.
    TargetFrozen,
    /// The caller lacks ownership of a write target.
    OwnershipDenied,
    /// The required fs4 lock cannot be acquired.
    LockUnavailable,
    /// Atomic replacement was blocked and no in-place fallback was used.
    ReplaceBlocked,
    /// A payload cannot be parsed as the required Arrow IPC data.
    PayloadMalformed,
    /// A stored digest does not match canonical content bytes.
    DigestMismatch,
    /// A layout contains corrupt bytes or invalid offsets.
    StorageCorrupt,
    /// A physical locator is ambiguous.
    MappingAmbiguous,
    /// A conversion or verification input is required but missing.
    RequiredDataMissing,
    /// Conversion rules leave more than one valid interpretation.
    ConversionAmbiguous,
    /// A metadata reference has no corresponding object.
    DanglingReference,
    /// A projected leaf is a degraded block with no routable plugin
    /// (01 §4-5: missing plugin ⇒ the leaf cannot be materialized).
    PluginMissing,
    /// A tree xpath does not address any existing node or leaf.
    XpathUnreachable,
    /// The layout does not support this operation.
    Unsupported,
}

impl ErrorCode {
    /// Returns the stable lower-case wire code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PathInvalid => "path_invalid",
            Self::NameReserved => "name_reserved",
            Self::TypeNotFound => "type_not_found",
            Self::TypeConflict => "type_conflict",
            Self::CompositionCycle => "composition_cycle",
            Self::CardinalityViolation => "cardinality_violation",
            Self::SchemaMismatch => "schema_mismatch",
            Self::ManifestInvalid => "manifest_invalid",
            Self::BootstrapIncomplete => "bootstrap_incomplete",
            Self::IdentityCollision => "identity_collision",
            Self::TargetFrozen => "target_frozen",
            Self::OwnershipDenied => "ownership_denied",
            Self::LockUnavailable => "lock_unavailable",
            Self::ReplaceBlocked => "replace_blocked",
            Self::PayloadMalformed => "payload_malformed",
            Self::DigestMismatch => "digest_mismatch",
            Self::StorageCorrupt => "storage_corrupt",
            Self::MappingAmbiguous => "mapping_ambiguous",
            Self::RequiredDataMissing => "required_data_missing",
            Self::ConversionAmbiguous => "conversion_ambiguous",
            Self::DanglingReference => "dangling_reference",
            Self::PluginMissing => "plugin_missing",
            Self::XpathUnreachable => "xpath_unreachable",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Contextual, stable error value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeSpaceError {
    /// Stable category used for programmatic handling.
    pub code: ErrorCode,
    /// Stable contextual values such as `path`, `type`, `key`, or `offset`.
    pub context: BTreeMap<String, String>,
    /// Human-readable explanation that is not used as a semantic category.
    pub message: String,
}

impl TreeSpaceError {
    /// Creates an error with a code and display message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            context: BTreeMap::new(),
            message: message.into(),
        }
    }

    /// Adds stable context to the error.
    #[must_use]
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }
}

impl Display for TreeSpaceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for TreeSpaceError {}

/// Result type used by tree-space internals and public APIs.
pub type Result<T> = std::result::Result<T, TreeSpaceError>;
