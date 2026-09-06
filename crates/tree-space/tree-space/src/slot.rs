//! Immutable table snapshots, lazy load de-duplication, and generation waits.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::Digest;
use arrow::record_batch::RecordBatch;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

/// The residency representation of an immutable table snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Residency {
    /// All buffers are process-owned and writable after cloning.
    OwnedFull,
    /// Columns are copied to process memory only when requested.
    OwnedLazyColumns,
    /// Bytes originate from a read-only mapping and must materialize before writes.
    Mapped,
}

/// Immutable data visible to a reader that has resolved a table.
#[derive(Clone)]
pub struct TableInner {
    /// Fully schema-validated Arrow batches.
    pub batches: Vec<RecordBatch>,
    /// Storage representation used to load this snapshot.
    pub residency: Residency,
    /// Digest produced before publication.
    pub digest: Option<Digest>,
}

impl TableInner {
    /// Creates a full owned snapshot.
    pub fn owned(batches: Vec<RecordBatch>, digest: Option<Digest>) -> Self {
        Self {
            batches,
            residency: Residency::OwnedFull,
            digest,
        }
    }

    /// Copies a non-owned view into a publishable owned representation.
    pub fn materialize(&self) -> Self {
        Self {
            batches: self.batches.clone(),
            residency: Residency::OwnedFull,
            digest: self.digest,
        }
    }
}

/// State-machine state of a table slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotState {
    /// No resident snapshot.
    Unloaded,
    /// A single thread is loading the physical bytes.
    Loading,
    /// An immutable snapshot is resident and unmodified.
    LoadedClean,
    /// A writer owns the slot and is preparing a replacement.
    Writing,
    /// The resident value was modified but not yet published.
    LoadedDirty,
}

/// A table slot with immutable reader snapshots and serialized writers.
pub struct TableSlot {
    current: RwLock<Option<Arc<TableInner>>>,
    state: Mutex<SlotState>,
    changed: Condvar,
    generation: AtomicU64,
    write_mu: Mutex<()>,
}

impl Default for TableSlot {
    fn default() -> Self {
        Self::new_unloaded()
    }
}

impl TableSlot {
    /// Creates an unloaded slot.
    pub fn new_unloaded() -> Self {
        Self {
            current: RwLock::new(None),
            state: Mutex::new(SlotState::Unloaded),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
            write_mu: Mutex::new(()),
        }
    }

    /// Returns the current state-machine state.
    pub fn state(&self) -> SlotState {
        *self.state.lock().expect("slot state lock poisoned")
    }

    /// Marks a clean snapshot dirty while retaining the immutable reader view.
    pub fn mark_dirty(&self) -> Result<()> {
        let mut state = self.state.lock().expect("slot state lock poisoned");
        if *state != SlotState::LoadedClean {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "only clean snapshots can become dirty",
            ));
        }
        *state = SlotState::LoadedDirty;
        Ok(())
    }
    /// Creates an already loaded clean slot.
    pub fn new_loaded(value: TableInner) -> Self {
        Self {
            current: RwLock::new(Some(Arc::new(value))),
            state: Mutex::new(SlotState::LoadedClean),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
            write_mu: Mutex::new(()),
        }
    }

    /// Returns the currently published immutable snapshot without queue semantics.
    pub fn snapshot(&self) -> Option<Arc<TableInner>> {
        self.current
            .read()
            .expect("slot current lock poisoned")
            .clone()
    }

    /// Returns the current slot generation.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Loads at most once while concurrent readers wait for the first loader.
    pub fn load_once<F>(&self, loader: F) -> Result<Arc<TableInner>>
    where
        F: FnOnce() -> Result<TableInner>,
    {
        let mut state = self.state.lock().expect("slot state lock poisoned");
        loop {
            match *state {
                SlotState::LoadedClean | SlotState::LoadedDirty | SlotState::Writing => {
                    return self.snapshot().ok_or_else(|| {
                        TreeSpaceError::new(
                            ErrorCode::BootstrapIncomplete,
                            "loaded slot has no snapshot",
                        )
                    });
                }
                SlotState::Loading => {
                    state = self.changed.wait(state).expect("slot state lock poisoned")
                }
                SlotState::Unloaded => {
                    *state = SlotState::Loading;
                    break;
                }
            }
        }
        drop(state);
        let result = loader().map(Arc::new);
        let mut state = self.state.lock().expect("slot state lock poisoned");
        match result {
            Ok(value) => {
                *self.current.write().expect("slot current lock poisoned") = Some(value.clone());
                *state = SlotState::LoadedClean;
                self.changed.notify_all();
                Ok(value)
            }
            Err(error) => {
                *state = SlotState::Unloaded;
                self.changed.notify_all();
                Err(error)
            }
        }
    }

    /// Publishes a fully materialized value, then advances generation and wakes waiters.
    pub fn publish(&self, value: TableInner) -> Result<u64> {
        if value.residency != Residency::OwnedFull {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "writes require OwnedFull materialization",
            ));
        }
        let _writer = self.write_mu.lock().expect("slot write lock poisoned");
        {
            let mut state = self.state.lock().expect("slot state lock poisoned");
            *state = SlotState::Writing;
        }
        *self.current.write().expect("slot current lock poisoned") = Some(Arc::new(value));
        let generation = self.generation.fetch_add(1, Ordering::Release) + 1;
        let mut state = self.state.lock().expect("slot state lock poisoned");
        *state = SlotState::LoadedClean;
        self.changed.notify_all();
        Ok(generation)
    }

    /// Waits until a publication at or beyond `required_generation` is visible.
    pub fn wait(&self, required_generation: u64) -> Result<Arc<TableInner>> {
        let mut state = self.state.lock().expect("slot state lock poisoned");
        while self.generation() < required_generation {
            state = self.changed.wait(state).expect("slot state lock poisoned");
        }
        drop(state);
        self.snapshot().ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::RequiredDataMissing,
                "generation completed without a snapshot",
            )
        })
    }

    /// Drops a clean resident snapshot without invalidating snapshots already borrowed by readers.
    pub fn unload(&self) -> Result<()> {
        let mut state = self.state.lock().expect("slot state lock poisoned");
        if *state != SlotState::LoadedClean {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "only clean slots can be unloaded",
            ));
        }
        *self.current.write().expect("slot current lock poisoned") = None;
        *state = SlotState::Unloaded;
        Ok(())
    }
}
