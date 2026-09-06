//! Block payload materialization (P-IO-7.1 unified data interface).
//!
//! The disk object of every bucket block is the envelope-wrapped physical
//! encoding of its canonical payload. PL-1 (plugin overhaul, S2) moves the
//! real behavior into the disk plugins ([`crate::plugin::block`]):
//! [`ArrowIpcBlockPlugin`] generalizes `IpcMaterializer` and
//! [`ArrowParquetBlockPlugin`] generalizes `ParquetMaterializer`. This module
//! stays as the legacy-compatible thin layer:
//!
//! - [`Materialization`] keeps its frozen public shape and gains the explicit
//!   [`DiskLayout`] interconversion (`From` / `TryFrom`);
//! - [`IpcMaterializer`] / [`ParquetMaterializer`] are kept as zero-logic
//!   shells that forward to the plugins (they are root-level re-exports used
//!   by the P-IO-7.x tests and the exchange translator, and their deletion
//!   would break the public API);
//! - [`probe_block_materialization`] dispatches by payload magic through
//!   [`DiskLayout::probe_magic`]; the legacy "native bytes stay on the
//!   IPC/native path" fallback is preserved (unknown magic → `Ipc`);
//! - [`materializer_for`] / [`select_materializer`] stay as the
//!   layout-default + kind-constraint compatibility surface, forwarding to
//!   plugin routing internally (`TbLibrary::write_commit` call sites are
//!   unchanged).
//!
//! Identity ⊥ address (01 §1.11.4): the semantic `RefId` is computed from the
//! canonical payload and is materialization-independent, while the storage
//! address is the full-content hash of the materialized bytes. The address
//! formula `canonical_digest(b"tb-blob", [physical])` is immutable; only the
//! object bytes differ, so the same logical block gets the same `RefId` and
//! different addresses across materializations.

use crate::block::{BlockKind, Envelope};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::Digest;
use crate::plugin::block::{ArrowIpcBlockPlugin, ArrowParquetBlockPlugin, BlockPlugin};
use crate::plugin::registry::PluginRegistry;
use crate::plugin::version::DiskLayout;

/// The physical materialization format of a stored block object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Materialization {
    /// Arrow IPC file format (`ARROW1` payload magic), the default. The disk
    /// object equals the canonical envelope frame, so the bucket can be rebuilt
    /// without decoding (mmap zero-copy friendly).
    Ipc,
    /// Apache Parquet columnar format (`PAR1` payload magic), for `ArrowTable`
    /// blocks only. Reading rebuilds the table in memory.
    Parquet,
}

impl From<DiskLayout> for Materialization {
    fn from(layout: DiskLayout) -> Self {
        match layout {
            DiskLayout::ArrowIpc => Materialization::Ipc,
            DiskLayout::ArrowParquet => Materialization::Parquet,
        }
    }
}

impl TryFrom<Materialization> for DiskLayout {
    type Error = TreeSpaceError;

    fn try_from(format: Materialization) -> Result<DiskLayout> {
        match format {
            Materialization::Ipc => Ok(DiskLayout::ArrowIpc),
            Materialization::Parquet => Ok(DiskLayout::ArrowParquet),
        }
    }
}

/// A pluggable encoder/addresser/decoder of block object bytes.
///
/// The trait keeps identity separate from addressing: `address` hashes the
/// physical bytes while the semantic `RefId` stays a function of the canonical
/// payload alone (01 §1.3/§1.11.4). Kept as the legacy compatibility surface;
/// the real implementations are the disk plugins
/// ([`ArrowIpcBlockPlugin`] / [`ArrowParquetBlockPlugin`]) and every method
/// here forwards to them.
pub trait BlockMaterializer: Send + Sync {
    /// Encodes a canonical envelope into its physical object bytes.
    ///
    /// For `Ipc`, the object is the canonical envelope frame itself; for
    /// `Parquet`, the envelope payload is re-encoded as Parquet before framing.
    /// `envelope.payload` must be the canonical (materialization-independent)
    /// payload of the block.
    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>>;
    /// Computes the storage address of a physical object byte string.
    fn address(&self, physical: &[u8]) -> Digest;
    /// Decodes a physical object back into its canonical envelope.
    ///
    /// The returned envelope always carries the canonical payload, so a bucket
    /// stored in memory or a `RefId` recomputation is independent of the
    /// materialization the object arrived through.
    fn decode(&self, physical: &[u8]) -> Result<Envelope>;
}

/// The default materializer shell: forwards to [`ArrowIpcBlockPlugin`].
///
/// `encode` is the existing envelope frame (byte-identical to the pre-7.1
/// writes, so golden 17 stays frozen); `decode` extracts the envelope without
/// interpreting the payload.
pub struct IpcMaterializer;

impl BlockMaterializer for IpcMaterializer {
    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>> {
        ArrowIpcBlockPlugin.encode(envelope)
    }
    fn address(&self, physical: &[u8]) -> Digest {
        ArrowIpcBlockPlugin.address(physical)
    }
    fn decode(&self, physical: &[u8]) -> Result<Envelope> {
        ArrowIpcBlockPlugin.decode(physical)
    }
}

/// The optional columnar materializer shell: forwards to
/// [`ArrowParquetBlockPlugin`] (`ArrowTable` only).
///
/// `encode` parses the canonical payload into the Arrow batch, writes it as an
/// enveloped Parquet file, and `decode` rebuilds the canonical envelope from
/// the Parquet rows. Non-table blocks are refused (01 §1.11.3: parquet is
/// `ArrowTable` only).
pub struct ParquetMaterializer;

impl BlockMaterializer for ParquetMaterializer {
    fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>> {
        ArrowParquetBlockPlugin.encode(envelope)
    }
    fn address(&self, physical: &[u8]) -> Digest {
        ArrowParquetBlockPlugin.address(physical)
    }
    fn decode(&self, physical: &[u8]) -> Result<Envelope> {
        ArrowParquetBlockPlugin.decode(physical)
    }
}

/// Probes a block object's payload magic and dispatches it to a materializer.
///
/// Reads the envelope frame to locate the payload, then delegates the magic
/// dispatch to [`DiskLayout::probe_magic`]: `ARROW1` →
/// [`Materialization::Ipc`], `PAR1` → [`Materialization::Parquet`]. Payloads
/// with neither magic are native bytes (opaque registered content) and stay on
/// the IPC/native path — the legacy fallback is preserved (`Ok(Ipc)`).
pub fn probe_block_materialization(object: &[u8]) -> Result<Materialization> {
    let payload = payload_offset(object)?;
    match DiskLayout::probe_magic(payload) {
        Some(DiskLayout::ArrowIpc) => Ok(Materialization::Ipc),
        Some(DiskLayout::ArrowParquet) => Ok(Materialization::Parquet),
        None => Ok(Materialization::Ipc),
    }
}

/// Returns the shared materializer shell for a format.
///
/// Both shells are zero-sized, so this returns a `'static` reference without
/// allocation; every method forwards to the corresponding disk plugin.
pub fn materializer_for(format: Materialization) -> &'static dyn BlockMaterializer {
    match format {
        Materialization::Ipc => &IpcMaterializer,
        Materialization::Parquet => &ParquetMaterializer,
    }
}

/// The unified materialization selection entry point (01 §1.11.3).
///
/// Thin compatibility surface over plugin routing: the layout-configured
/// default disk version is combined with the block kind constraint through
/// [`PluginRegistry::route_disk`] (`ArrowTable` blocks may be Parquet when the
/// layout default is [`Materialization::Parquet`]; every other kind — `Blob`,
/// `Sequence`, `Kv`, opaque registered kinds — stays on native bytes/the IPC
/// plugin regardless of the default, matching the legacy match exactly). This
/// is a whole-layout policy, not a per-block API knob.
pub fn select_materializer(
    default: Materialization,
    kind: &BlockKind,
) -> &'static dyn BlockMaterializer {
    let disk = DiskLayout::try_from(default).expect("ipc/parquet map to the two M1 disk layouts");
    match PluginRegistry::global().route_disk(disk, kind) {
        Some(plugin) => match plugin.disk() {
            DiskLayout::ArrowIpc => &IpcMaterializer,
            DiskLayout::ArrowParquet => &ParquetMaterializer,
        },
        // Legacy fallback: kinds outside the plugin's declared range (and the
        // IPC default path) keep native bytes on the IPC materializer.
        None => &IpcMaterializer,
    }
}

/// Locates the envelope payload (after the frame header) for magic probing.
fn payload_offset(object: &[u8]) -> Result<&[u8]> {
    let Some(&first) = object.first() else {
        return Err(malformed("truncated envelope"));
    };
    if first == 5 {
        if object.len() < 12 {
            return Err(malformed("truncated named envelope"));
        }
        let name_len = object[1] as usize;
        let offset = 2 + name_len + 10;
        if object.len() < offset {
            return Err(malformed("truncated named envelope"));
        }
        Ok(&object[offset..])
    } else if (1..=4).contains(&first) {
        if object.len() < 11 {
            return Err(malformed("truncated envelope"));
        }
        Ok(&object[11..])
    } else {
        Err(malformed("unknown block kind"))
    }
}

fn malformed(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}
