//! Portable UTF-8 names, paths, hashes, and deterministic rename plans.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::PathHash;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use xxhash_rust::xxh3::xxh3_128;

const RESERVED: &[&str] = &[
    "manifest",
    "dt_types",
    "dt_children",
    "tt_types",
    "tt_columns",
    "tt_composition",
    "table-metadata",
    "domain-metadata",
    "column-digests",
    ".",
    "..",
];

/// One portable tree-space path segment.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Name(String);

impl Name {
    /// Validates and constructs a UTF-8 name of 1 through 64 bytes.
    ///
    /// Names may contain arbitrary UTF-8 text. Only the structural characters
    /// that would break path addressing are rejected: `/` (the path
    /// separator), `%` (percent-encoding escape), and ASCII control bytes.
    /// The reserved ABI names listed in [`RESERVED`] are rejected separately.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && !value.contains('/')
            && !value.contains('%')
            && !value.bytes().any(|byte| byte.is_ascii_control());
        if !valid {
            return Err(
                TreeSpaceError::new(ErrorCode::PathInvalid, "invalid tree-space name")
                    .with_context("name", value),
            );
        }
        if RESERVED.contains(&value.as_str()) {
            return Err(
                TreeSpaceError::new(ErrorCode::NameReserved, "reserved tree-space name")
                    .with_context("name", value),
            );
        }
        Ok(Self(value))
    }

    /// Returns the validated segment text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Name {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Canonical slash-delimited tree-space path.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct TablePath(Vec<Name>);

impl TablePath {
    /// Parses a canonical path with no empty, dot, or encoded segments.
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref();
        if value.is_empty() || value.starts_with('/') || value.ends_with('/') || value.contains('%')
        {
            return Err(
                TreeSpaceError::new(ErrorCode::PathInvalid, "invalid table path")
                    .with_context("path", value),
            );
        }
        let segments = value
            .split('/')
            .map(Name::new)
            .collect::<Result<Vec<_>>>()?;
        Ok(Self(segments))
    }

    /// Constructs a path from already-validated segments.
    pub fn from_segments(segments: Vec<Name>) -> Result<Self> {
        if segments.is_empty() {
            return Err(TreeSpaceError::new(
                ErrorCode::PathInvalid,
                "path has no segments",
            ));
        }
        Ok(Self(segments))
    }

    /// Returns canonical UTF-8 path text.
    pub fn as_str(&self) -> String {
        self.0
            .iter()
            .map(Name::as_str)
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Returns the final path segment.
    pub fn name(&self) -> &Name {
        self.0.last().expect("validated path has a segment")
    }

    /// Returns whether this path is equal to or lies beneath `prefix`.
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// Replaces a prefix; callers use this for deterministic subtree rename planning.
    pub fn replace_prefix(&self, old: &Self, new: &Self) -> Option<Self> {
        self.starts_with(old).then(|| {
            let mut segments = new.0.clone();
            segments.extend_from_slice(&self.0[old.0.len()..]);
            Self(segments)
        })
    }

    /// Returns the canonical path hash input specified by the v3 ABI.
    pub fn hash(&self, format_major: u16) -> PathHash {
        let mut input = b"tree-space/path\0".to_vec();
        input.extend_from_slice(&format_major.to_le_bytes());
        input.extend_from_slice(self.as_str().as_bytes());
        PathHash(xxh3_128(&input))
    }
}

impl Ord for TablePath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}
impl PartialOrd for TablePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Display for TablePath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}

/// One atomically published logical path change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameEntry {
    /// Original path.
    pub old_path: TablePath,
    /// Replacement path.
    pub new_path: TablePath,
    /// Hash of the original path.
    pub old_hash: PathHash,
    /// Hash of the replacement path.
    pub new_hash: PathHash,
}

/// A sorted, all-or-nothing subtree rename plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenamePlan {
    /// Ordered path changes, sorted by original path.
    pub entries: Vec<RenameEntry>,
}
