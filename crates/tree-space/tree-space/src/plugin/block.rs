//! Disk block plugins (`01-目标与设计.md` §4-1): a block plugin declares the
//! in-memory/on-disk layout pair it serves, the kind range it can handle, the
//! identity formula, and the direct read/write/address operations.
//!
//! PL-1 generalizes the P-IO-7.1 [`crate::layout::materializer::BlockMaterializer`]
//! double implementation into two disk plugins:
//!
//! - [`ArrowIpcBlockPlugin`] generalizes `IpcMaterializer` (`DISK = ArrowIpc`):
//!   the object is the canonical envelope frame. `encode` is byte-identical to
//!   the legacy writes, so golden 17 stays frozen.
//! - [`ArrowParquetBlockPlugin`] generalizes `ParquetMaterializer`
//!   (`DISK = ArrowParquet`, `kinds = &[Table]` only): the object is an
//!   enveloped Parquet file. The P-IO-7.2 `ArrowTable`-only constraint is kept,
//!   including the legacy temporary scratch-file path for reading Parquet.
//!
//! Identity stays orthogonal to addressing (01 §1.11.4): `ref_id` is the
//! canonical-byte formula (M1, zero golden change) and `address` is the
//! `tb-blob` full-content hash of the physical bytes.

use crate::block::{ArrowTable, Block, BlockKind, Envelope, RefId};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::hash::canonical_digest;
use crate::ids::Digest;
use crate::ipc::decode_batch;
use crate::layout::tb::block_blob_address;
use crate::plugin::semantic::{SemanticValue, semantic_table_value};
use crate::plugin::version::{DiskLayout, MemLayout};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

/// A block plugin: declares its in-memory/on-disk layout pair, the kind range
/// it handles, and the direct encode/decode/address operations (`01 §4-1`).
///
/// The companion pair `(MEMORY, DISK)` is enforced by [`crate::plugin::registry::PluginRegistry`]
/// (at most one block plugin per pair). Stable Rust cannot make a trait with
/// associated constants object-safe (E0038), so the `MEMORY` / `DISK`
/// declarations of the spec (`02 §5.4`) are presented as the equivalent
/// required query methods [`BlockPlugin::memory`] / [`BlockPlugin::disk`]
/// (deviation noted in the PL-1 S2 report). `address` defaults to the
/// immutable byte formula (`block_blob_address`) and must not change
/// ([identity stays semantic, addressing stays byte-hash, 01 §4-4]).
pub trait BlockPlugin: Send + Sync {
    /// The in-memory layout version this plugin serves (M1: always [`MemLayout::Arrow55`]).
    fn memory(&self) -> MemLayout;
    /// The declared on-disk layout version ([`DiskLayout::ArrowIpc`] /
    /// [`DiskLayout::ArrowParquet`]).
    fn disk(&self) -> DiskLayout;
    /// The block kinds this plugin can handle.
    fn kinds(&self) -> &'static [BlockKind];
    /// Probes whether a kind belongs to this plugin; defaults to
    /// `kinds().contains(kind)`.
    fn matches(&self, kind: &BlockKind) -> bool {
        self.kinds().contains(kind)
    }
    /// Computes the content identity from a kind and its canonical payload.
    ///
    /// PL-2 M2 implementation = the semantic formula
    /// (`crate::plugin::semantic::semantic_block_ref_id`, 01 §4-4); the M1
    /// byte formula was the PL-1 default (zero golden change at M1).
    fn ref_id(&self, kind: BlockKind, payload: &[u8]) -> RefId {
        crate::block::block_ref_id(kind, payload)
    }
    /// Returns the semantic value of a canonical in-memory envelope
    /// (`01 §4-1 [M2]`): the fingerprint component of the block identity
    /// formula — `block_ref_id = hash(kind.domain(), [semantic(envelope).encode()])`.
    fn semantic(&self, envelope: &Envelope) -> SemanticValue {
        crate::plugin::semantic::semantic_block_value(&envelope.kind, &envelope.payload)
    }
    /// Encodes an in-memory canonical envelope into disk bytes.
    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>>;
    /// Decodes disk bytes back into the canonical in-memory envelope.
    fn decode(&self, physical: &[u8]) -> Result<Envelope>;
    /// Computes the storage address of a physical byte string
    /// (`block_blob_address`, the immutable `tb-blob` hash formula).
    fn address(&self, physical: &[u8]) -> Digest {
        block_blob_address(physical)
    }
}

/// The Arrow IPC disk plugin (generalizes `IpcMaterializer`; golden 17 frames
/// stay byte-identical).
///
/// `encode = Envelope::encode` (the object is the canonical envelope frame
/// itself, mmap zero-copy friendly), `decode = Envelope::decode` (the payload
/// is not interpreted). Every built-in kind belongs to this plugin.
pub struct ArrowIpcBlockPlugin;

impl BlockPlugin for ArrowIpcBlockPlugin {
    fn memory(&self) -> MemLayout {
        MemLayout::Arrow55
    }

    fn disk(&self) -> DiskLayout {
        DiskLayout::ArrowIpc
    }

    fn kinds(&self) -> &'static [BlockKind] {
        &BlockKind::ALL
    }

    // The IPC plugin is the universal envelope container: it matches every
    // kind, including dynamically-registered `Named` blocks. A static slice
    // cannot enumerate dynamic names, so `matches` overrides the
    // `kinds().contains(kind)` default to `true` (the tagged envelope framing,
    // `tag = 5` for named kinds, is handled by `Envelope::encode`/`decode`;
    // see `materializer::payload_offset`).
    fn matches(&self, _kind: &BlockKind) -> bool {
        true
    }

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>> {
        Ok(envelope.encode())
    }

    fn decode(&self, physical: &[u8]) -> Result<Envelope> {
        Envelope::decode(physical)
    }
}

/// The Arrow Parquet disk plugin (generalizes `ParquetMaterializer`;
/// `ArrowTable` only, P-IO-7.2 constraint kept).
///
/// `encode` parses the canonical payload into the Arrow batch, writes it as an
/// enveloped Parquet file, and `decode` rebuilds the canonical envelope from
/// the Parquet rows. Non-table blocks are refused with `SchemaMismatch`.
pub struct ArrowParquetBlockPlugin;

impl BlockPlugin for ArrowParquetBlockPlugin {
    fn memory(&self) -> MemLayout {
        MemLayout::Arrow55
    }

    fn disk(&self) -> DiskLayout {
        DiskLayout::ArrowParquet
    }

    fn kinds(&self) -> &'static [BlockKind] {
        &[BlockKind::Table]
    }

    // The Parquet object payload is the physical Parquet bytes, not the
    // canonical IPC payload: resolve it back into the Arrow batch so that the
    // same logical table identities identically across materializations
    // (P-IO-7.2 cross-encoding anchor, 02 §6.4). Unresolvable payloads fall
    // back to the byte-value formula through the shared entry point.
    fn ref_id(&self, kind: BlockKind, payload: &[u8]) -> RefId {
        match read_parquet_bytes(payload) {
            Ok(batch) => match semantic_table_value(&batch) {
                Some(value) => RefId(canonical_digest(&kind.domain(), [value.encode()])),
                None => crate::block::block_ref_id(kind, payload),
            },
            Err(_) => crate::block::block_ref_id(kind, payload),
        }
    }

    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>> {
        if envelope.kind != BlockKind::Table {
            return Err(schema_mismatch(
                "parquet materialization is available for ArrowTable blocks only",
            ));
        }
        let batch = decode_batch(&envelope.payload)?;
        let parquet = write_parquet_bytes(&batch)?;
        Ok(Envelope::new(BlockKind::Table, parquet).encode())
    }

    fn decode(&self, physical: &[u8]) -> Result<Envelope> {
        let envelope = Envelope::decode(physical)?;
        if envelope.kind != BlockKind::Table {
            return Err(schema_mismatch(
                "parquet object carries a non-table block kind",
            ));
        }
        let batch = read_parquet_bytes(&envelope.payload)?;
        let table = ArrowTable::try_new(batch)?;
        Ok(Envelope::new(BlockKind::Table, table.payload()))
    }
}

/// Writes one record batch as a Parquet file in memory.
///
/// `into_inner` flushes the buffered row groups and writes the file footer
/// (it is the single call that both finalizes the bytes and returns them).
fn write_parquet_bytes(batch: &RecordBatch) -> Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ArrowWriter::try_new(cursor, batch.schema(), None).map_err(parquet_error)?;
    writer.write(batch).map_err(parquet_error)?;
    let cursor = writer.into_inner().map_err(parquet_error)?;
    Ok(cursor.into_inner())
}

/// Reads a Parquet file from memory back into a single record batch.
///
/// Mirrors the layout's `read_parquet` and the legacy materializer: zero-row
/// files (schema only) rebuild the empty batch from the embedded Arrow schema,
/// and multiple row groups are concatenated into the single-batch contract.
/// The parquet reader requires a `ChunkReader` (`File`/`bytes`), so the
/// in-memory payload is materialized to a unique scratch file that is always
/// removed again; the crate keeps zero new third-party dependencies. The
/// scratch path shape is preserved from the legacy `ParquetMaterializer`.
fn read_parquet_bytes(bytes: &[u8]) -> Result<RecordBatch> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "tree-space-{}-{:?}-{:016x}.parquet",
        std::process::id(),
        std::thread::current().id(),
        SERIAL.fetch_add(1, Ordering::Relaxed),
    ));
    if let Err(error) = std::fs::write(&path, bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(parquet_io(error));
    }
    let result = (|| {
        let file = std::fs::File::open(&path).map_err(parquet_io)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_error)?;
        let embedded_schema = builder.schema().clone();
        let reader = builder.build().map_err(parquet_error)?;
        let batches = reader
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(parquet_error)?;
        if batches.is_empty() {
            return Ok(RecordBatch::new_empty(embedded_schema));
        }
        if batches.len() == 1 {
            return Ok(batches.into_iter().next().expect("checked length"));
        }
        let schema = batches[0].schema();
        arrow::compute::concat_batches(&schema, &batches).map_err(parquet_error)
    })();
    let _ = std::fs::remove_file(&path);
    result
}

fn schema_mismatch(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}

fn parquet_io(error: std::io::Error) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::StorageCorrupt, "parquet scratch I/O failed")
        .with_context("detail", error.to_string())
}

fn parquet_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "Parquet Arrow conversion failed",
    )
    .with_context("detail", error.to_string())
}
