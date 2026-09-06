//! In-memory content-addressed bucket for TB envelopes.
use crate::block::{Block, Envelope, RefId, block_ref_id, read_block};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::plugin::degraded::DegradedBlock;
use std::collections::{BTreeMap, BTreeSet};

/// An in-memory bucket of TB envelopes keyed by content identity.
///
/// Insertion is content-addressed (identical payload+kind deduplicates), reads
/// and verification are identity-checked, and GC retains only the supplied
/// root set. The bucket never interprets blob bytes.
///
/// A parallel [`Self::degraded`] slot holds plugin-unreachable blocks as their
/// original disk bytes (01 §4-5): the restore path routes every ref-table row
/// through the plugin registry and degrades the misses instead of failing the
/// whole library read.
#[derive(Clone, Debug, Default)]
pub struct Bucket {
    objects: BTreeMap<RefId, Envelope>,
    degraded: BTreeMap<RefId, DegradedBlock>,
}
impl Bucket {
    /// Creates an empty bucket.
    pub fn new() -> Self {
        Self::default()
    }
    /// Returns the number of distinct stored envelopes.
    pub fn len(&self) -> usize {
        self.objects.len()
    }
    /// Returns whether the bucket holds no envelopes.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
    /// Stores a block (deduplicated by identity) and returns its `RefId`.
    pub fn put<B: Block + ?Sized>(&mut self, block: &B) -> RefId {
        let id = block.ref_id();
        self.objects.entry(id).or_insert_with(|| block.envelope());
        id
    }
    /// Stores an already-framed envelope, recomputing its identity.
    pub fn put_envelope(&mut self, envelope: Envelope) -> Result<RefId> {
        let id = block_ref_id(envelope.kind.clone(), &envelope.payload);
        self.objects.entry(id).or_insert(envelope);
        Ok(id)
    }
    /// Returns the stored envelope for an identity, if present.
    pub fn get(&self, id: RefId) -> Option<&Envelope> {
        self.objects.get(&id)
    }
    /// Stores a degraded block in the parallel slot (01 §4-5), keyed by its
    /// `RefId` like the healthy envelope map.
    ///
    /// A `RefId` already occupied by a healthy envelope is rejected with
    /// [`ErrorCode::TypeConflict`] (the restored row state must never claim
    /// both readings of one identity); a repeated degraded insert deduplicates
    /// to the first value, mirroring [`Self::put_envelope`].
    pub fn put_degraded(&mut self, id: RefId, block: DegradedBlock) -> Result<()> {
        if self.objects.contains_key(&id) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "a healthy envelope already occupies the identity",
            ));
        }
        self.degraded.entry(id).or_insert(block);
        Ok(())
    }
    /// Returns the parallel degraded-block slot, keyed by `RefId`.
    pub fn degraded(&self) -> &BTreeMap<RefId, DegradedBlock> {
        &self.degraded
    }
    /// Returns the degraded block for an identity, if present.
    pub fn degraded_get(&self, id: RefId) -> Option<&DegradedBlock> {
        self.degraded.get(&id)
    }
    /// Weakly decodes the block for an identity, or reports a dangling ref.
    pub fn read(&self, id: RefId) -> Result<Box<dyn Block>> {
        let e = self.objects.get(&id).ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::DanglingReference, "bucket reference is missing")
        })?;
        read_block(e.kind.clone(), &e.payload)
    }
    /// Verifies that one stored envelope recomputes to its identity.
    pub fn verify(&self, id: RefId) -> Result<()> {
        let e = self.objects.get(&id).ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::DanglingReference, "bucket reference is missing")
        })?;
        if block_ref_id(e.kind.clone(), &e.payload) != id {
            return Err(TreeSpaceError::new(
                ErrorCode::DigestMismatch,
                "bucket object digest mismatch",
            ));
        }
        Ok(())
    }
    /// Verifies every stored envelope against its identity.
    pub fn verify_all(&self) -> Result<()> {
        for id in self.objects.keys() {
            self.verify(*id)?;
        }
        Ok(())
    }
    /// Removes envelopes unreachable from `roots`, returning how many were dropped.
    pub fn gc(&mut self, roots: impl IntoIterator<Item = RefId>) -> usize {
        let roots = roots.into_iter().collect::<BTreeSet<_>>();
        let before = self.objects.len();
        self.objects.retain(|id, _| roots.contains(id));
        before - self.objects.len()
    }
    /// Iterates over all stored identities.
    pub fn ids(&self) -> impl Iterator<Item = RefId> + '_ {
        self.objects.keys().copied()
    }
}
