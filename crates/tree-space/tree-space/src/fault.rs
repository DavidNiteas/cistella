//! Deterministic fault injection points used by platform and e2e tests.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A publication stage at which a test may request a controlled failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum FaultPoint {
    /// Before writing an Arrow payload.
    BeforePayload,
    /// After writing a payload but before metadata publication.
    AfterPayload,
    /// During metadata replacement.
    MetadataReplace,
    /// Before the manifest visibility point.
    BeforeManifest,
    /// Before writing a single-file trailer.
    BeforeTrailer,
    /// During a table load.
    TableLoad,
}
/// A one-shot or repeating deterministic fault plan.
#[derive(Default)]
pub struct FaultPlan {
    remaining: Mutex<BTreeMap<FaultPoint, usize>>,
}
impl FaultPlan {
    /// Creates an empty plan.
    pub fn new() -> Self {
        Self::default()
    }
    /// Injects `count` failures at the selected point.
    pub fn fail(&self, point: FaultPoint, count: usize) {
        self.remaining
            .lock()
            .expect("fault plan lock poisoned")
            .insert(point, count);
    }
    /// Consumes one planned failure, if any.
    pub fn hit(&self, point: FaultPoint) -> Result<()> {
        let mut remaining = self.remaining.lock().expect("fault plan lock poisoned");
        if let Some(count) = remaining.get_mut(&point) {
            if *count > 0 {
                *count -= 1;
                return Err(TreeSpaceError::new(
                    ErrorCode::StorageCorrupt,
                    "deterministic fault injection",
                )
                .with_context("fault_point", format!("{point:?}")));
            }
        }
        Ok(())
    }
}
