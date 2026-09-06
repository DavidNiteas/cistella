//! Arrow 55 IPC-only serialization helpers.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use arrow::record_batch::RecordBatch;
use std::io::Cursor;

/// Encodes one or more record batches as an Arrow IPC file.
pub fn encode_batches(batches: &[RecordBatch]) -> Result<Vec<u8>> {
    let first = batches.first().ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "cannot encode zero Arrow batches",
        )
    })?;
    let mut bytes = Vec::new();
    {
        let mut writer = FileWriter::try_new(&mut bytes, &first.schema()).map_err(ipc_error)?;
        for batch in batches {
            writer.write(batch).map_err(ipc_error)?;
        }
        writer.finish().map_err(ipc_error)?;
    }
    Ok(bytes)
}

/// Decodes an Arrow IPC file and returns every batch in file order.
///
/// Malformed bytes are reported as [`ErrorCode::PayloadMalformed`]. Arrow's
/// IPC reader may unwind on corrupt length fields instead of returning an
/// error, so the decode is panic-guarded: the M2 semantic identity formulas
/// (PL-2, `plugin::semantic`) run this over untrusted / damaged payloads and
/// must see a stable error, never a panic.
pub fn decode_batches(bytes: &[u8]) -> Result<Vec<RecordBatch>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = FileReader::try_new(Cursor::new(bytes), None).map_err(ipc_error)?;
        reader.map(|batch| batch.map_err(ipc_error)).collect()
    }))
    .map_err(|_| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "Arrow 55 IPC decode panicked on malformed bytes",
        )
    })?
}

/// Encodes exactly one record batch.
pub fn encode_batch(batch: &RecordBatch) -> Result<Vec<u8>> {
    encode_batches(std::slice::from_ref(batch))
}

/// Decodes exactly one record batch.
pub fn decode_batch(bytes: &[u8]) -> Result<RecordBatch> {
    let mut batches = decode_batches(bytes)?;
    if batches.len() != 1 {
        return Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "expected exactly one Arrow IPC batch",
        ));
    }
    Ok(batches.remove(0))
}

/// Converts an Arrow error into the stable malformed-payload category.
pub fn ipc_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "Arrow 55 IPC decode or encode failure",
    )
    .with_context("detail", error.to_string())
}
