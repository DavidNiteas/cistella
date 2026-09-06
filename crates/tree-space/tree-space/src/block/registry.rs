//! Process-wide registry of registered (non-built-in) TB block kinds.
//!
//! The four built-in kinds are decoded by the fixed canonical codecs in
//! [`super`]. Additional kinds register under a stable name; the registry is
//! process-global so that any envelope or `RefId` can be weakly decoded without
//! carrying a registry through every call. Built-in names cannot be overridden
//! and duplicate names are rejected.

use super::{Block, BlockKind, validate_block_name};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

/// The Arrow-facing capabilities of a registered block.
#[derive(Clone, Debug)]
pub struct ArrowCaps {
    /// The canonical Arrow schema of the block payload.
    pub schema: Arc<Schema>,
    /// Decodes a canonical payload into a single record batch.
    pub to_batch: fn(&[u8]) -> Result<RecordBatch>,
}

/// The capability tier of a registered block kind.
#[derive(Clone, Debug)]
pub enum BlockCaps {
    /// Opaque bytes; the block enjoys bucket-level benefits only.
    Opaque,
    /// Arrow encoded; the block enjoys content-level verify, sharing, and
    /// polars projection through its [`ArrowCaps`].
    Arrow(ArrowCaps),
}

/// A user-registered block kind.
///
/// Implementing this trait and calling [`register_block::<T>()`] gives a kind
/// a stable name, a canonical payload codec, and a capability tier. The
/// blanket [`Block`] impl lets registered blocks enter the bucket and compute
/// identities like any built-in block; the strong-typed path reconstructs `T`
/// from canonical payload bytes via [`RegisteredBlock::decode`].
pub trait RegisteredBlock: Send + Sync + std::fmt::Debug {
    /// The stable registration name (see [`super::validate_block_name`]).
    const NAME: &'static str;
    /// Encodes the canonical payload bytes (the sole identity input).
    fn encode(&self) -> Vec<u8>;
    /// Reconstructs the value from canonical payload bytes.
    fn decode(bytes: &[u8]) -> Result<Self>
    where
        Self: Sized;
    /// The capability tier of this kind.
    fn capabilities() -> BlockCaps;
}

impl<T: RegisteredBlock + 'static> Block for T {
    fn kind(&self) -> BlockKind {
        BlockKind::Named(Arc::<str>::from(T::NAME))
    }
    fn write_payload(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.encode());
    }
}

#[derive(Clone, Copy)]
struct Entry {
    decode: fn(&[u8]) -> Result<Box<dyn Block>>,
    caps: fn() -> BlockCaps,
}

static REGISTRY: LazyLock<Mutex<BTreeMap<&'static str, Entry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Registers a registered block kind under its trait name.
///
/// Rejects invalid names, built-in names, and duplicate registrations.
pub fn register_block<T: RegisteredBlock + 'static>() -> Result<()> {
    validate_block_name(T::NAME)?;
    if BlockKind::ALL
        .iter()
        .any(|kind| kind.as_str().as_ref() == T::NAME)
    {
        return Err(TreeSpaceError::new(
            ErrorCode::TypeConflict,
            "built-in block kind name cannot be overridden",
        ));
    }
    let mut registry = REGISTRY.lock().unwrap();
    if registry.contains_key(T::NAME) {
        return Err(TreeSpaceError::new(
            ErrorCode::TypeConflict,
            "registered block kind name already exists",
        ));
    }
    registry.insert(
        T::NAME,
        Entry {
            decode: decode_erased::<T>,
            caps: T::capabilities,
        },
    );
    Ok(())
}

/// Returns the capability tier of a registered kind, if it is registered.
pub fn block_caps(name: &str) -> Option<BlockCaps> {
    let registry = REGISTRY.lock().unwrap();
    registry.get(name).map(|entry| (entry.caps)())
}

/// Weakly decodes a registered block by name.
pub(crate) fn decode_named(name: &str, bytes: &[u8]) -> Result<Box<dyn Block>> {
    let registry = REGISTRY.lock().unwrap();
    let entry = registry.get(name).copied().ok_or_else(|| {
        TreeSpaceError::new(ErrorCode::TypeNotFound, "registered block kind is missing")
            .with_context("kind", name.to_string())
    })?;
    (entry.decode)(bytes)
}

fn decode_erased<T: RegisteredBlock + 'static>(bytes: &[u8]) -> Result<Box<dyn Block>> {
    Ok(Box::new(T::decode(bytes)?))
}
