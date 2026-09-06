//! Memory-only layout used by bootstrap and deterministic tests.

use super::{
    GcReport, GcRequest, PublishPlan, PublishReceipt, StorageKind, StorageLayout, TableBytes,
    TableLocator, TablePayload,
};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::fault::{FaultPlan, FaultPoint};
use crate::manifest::BootstrapImage;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Thread-safe in-memory implementation of the storage boundary.
#[derive(Default)]
pub struct MemoryLayout {
    state: Mutex<Option<State>>,
    fault: FaultPlan,
}
#[derive(Clone)]
struct State {
    bootstrap: BootstrapImage,
    tables: BTreeMap<crate::ids::TableId, Vec<u8>>,
    sequence: u64,
}

impl MemoryLayout {
    /// Creates an empty memory layout.
    pub fn new() -> Self {
        Self::default()
    }
    /// Injects a deterministic fault plan for M0 fault tests.
    pub fn with_fault(mut self, fault: FaultPlan) -> Self {
        self.fault = fault;
        self
    }
}
impl StorageLayout for MemoryLayout {
    fn create(&self, bootstrap: &BootstrapImage) -> Result<()> {
        let mut state = self.state.lock().expect("memory layout lock poisoned");
        if state.is_some() {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "memory library already exists",
            ));
        }
        *state = Some(State {
            bootstrap: bootstrap.clone(),
            tables: BTreeMap::new(),
            sequence: 0,
        });
        Ok(())
    }
    fn open_bootstrap(&self) -> Result<BootstrapImage> {
        self.state
            .lock()
            .expect("memory layout lock poisoned")
            .as_ref()
            .map(|state| state.bootstrap.clone())
            .ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::BootstrapIncomplete,
                    "memory library has not been created",
                )
            })
    }
    fn load_table(&self, locator: &TableLocator) -> Result<TableBytes> {
        self.fault.hit(FaultPoint::TableLoad)?;
        let state = self.state.lock().expect("memory layout lock poisoned");
        let bytes = state
            .as_ref()
            .and_then(|state| state.tables.get(&locator.table_id))
            .cloned()
            .ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::RequiredDataMissing,
                    "memory table payload is absent",
                )
            })?;
        Ok(TableBytes {
            bytes,
            mapped: false,
        })
    }
    fn publish(&self, plan: PublishPlan) -> Result<PublishReceipt> {
        self.fault.hit(FaultPoint::BeforePayload)?;
        let mut guard = self.state.lock().expect("memory layout lock poisoned");
        let state = guard.as_mut().ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::BootstrapIncomplete,
                "memory library has not been created",
            )
        })?;
        if plan.sequence <= state.sequence {
            return Err(TreeSpaceError::new(
                ErrorCode::TargetFrozen,
                "publication sequence is not monotonic",
            ));
        }
        let mut next = state.clone();
        next.bootstrap = plan.bootstrap;
        next.sequence = plan.sequence;
        for TablePayload {
            table_id,
            content_hash: _,
            bytes,
        } in plan.table_payloads
        {
            next.tables.insert(table_id, bytes);
        }
        *state = next;
        Ok(PublishReceipt {
            sequence: plan.sequence,
            bootstrap_locator: TableLocator {
                table_id: crate::ids::TableId::from_bytes([0; 16]),
                offset: 0,
                length: 0,
                storage_kind: StorageKind::Memory,
                path: None,
            },
            table_locators: Vec::new(),
        })
    }
    fn gc(&self, _request: GcRequest) -> Result<GcReport> {
        Ok(GcReport { reclaimed: 0 })
    }
    fn kind(&self) -> StorageKind {
        StorageKind::Memory
    }
}
