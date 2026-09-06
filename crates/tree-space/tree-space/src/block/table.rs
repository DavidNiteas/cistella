//! Arrow RecordBatch TB block.

use super::{Block, BlockKind, RefId};
use crate::error::Result;
use crate::hash::canonicalize_null_slots;
use crate::ipc::{decode_batch, encode_batch};
use arrow::array::ArrayRef;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

/// A TB table block wrapping an Arrow [`RecordBatch`].
#[derive(Clone, Debug)]
pub struct Table {
    batch: RecordBatch,
}

impl Table {
    /// Wraps a record batch; schema and arrays are kept as-is.
    pub fn try_new(batch: RecordBatch) -> Result<Self> {
        Ok(Self { batch })
    }

    /// Returns the wrapped record batch.
    pub fn as_batch(&self) -> &RecordBatch {
        &self.batch
    }

    /// Consumes the block, returning the record batch.
    pub fn into_batch(self) -> RecordBatch {
        self.batch
    }

    /// Returns a clone of the batch as an `Arc`.
    pub fn batch_arc(&self) -> Arc<RecordBatch> {
        Arc::new(self.batch.clone())
    }

    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let columns: Vec<ArrayRef> = self
            .batch
            .columns()
            .iter()
            .map(canonicalize_null_slots)
            .collect();
        let batch = RecordBatch::try_new_with_options(
            self.batch.schema(),
            columns,
            &arrow::record_batch::RecordBatchOptions::new(),
        )
        .expect("a valid table block reconstructs with its own schema");
        encode_batch(&batch).expect("a valid table block encodes as Arrow IPC")
    }

    /// Decodes a table block from canonical Arrow IPC bytes.
    pub fn try_read_blob(bytes: &[u8]) -> Result<Self> {
        Self::try_new(decode_batch(bytes)?)
    }

    /// Decodes a table block, panicking on malformed canonical bytes.
    pub fn read_blob(bytes: &[u8]) -> Self {
        Self::try_read_blob(bytes).expect("canonical table payload must decode")
    }
}

impl Default for Table {
    fn default() -> Self {
        use arrow::array::Int64Array;
        use arrow::datatypes::{DataType, Field, Schema};
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
            vec![Arc::new(Int64Array::from(Vec::<i64>::new()))],
        )
        .expect("empty table block");
        Self { batch }
    }
}

impl Block for Table {
    fn kind(&self) -> BlockKind {
        BlockKind::Table
    }

    fn write_payload(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.canonical_bytes());
    }
    fn as_table(&self) -> Option<&Table> {
        Some(self)
    }
}

impl Table {
    /// Computes the table identity from the canonical payload (M2 semantic
    /// formula through the single `block_ref_id` entry point, 01 §4-4).
    pub fn ref_id(&self) -> RefId {
        let mut payload = Vec::new();
        self.write_payload(&mut payload);
        crate::block::block_ref_id(BlockKind::Table, &payload)
    }
}
