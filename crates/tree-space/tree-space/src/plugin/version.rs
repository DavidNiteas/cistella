//! Layout-version enums (`01-目标与设计.md` §2-1/§2-9/§4-2): a closed in-memory
//! enum plus explicit on-disk strings.
//!
//! The two layout dimensions are orthogonal: [`MemLayout`] names in-memory
//! representations (closed, compile-time), while [`DiskLayout`] is expressed as
//! on-disk strings mapped back to the enum through an explicit table. Enums
//! never enter disk bytes; the strings do (`"arrow55"`, `"arrow-ipc"`,
//! `"arrow-parquet"`).

/// In-memory layout version, orthogonal to the disk layout (`01 §2-1`).
///
/// A closed, compile-time Rust enum. `Arrow55` is the only memory layout in
/// PL-1; further memory representations are appended here in later milestones
/// (e.g. polars-family in M2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MemLayout {
    /// The Arrow 55 in-memory representation.
    Arrow55,
}

impl MemLayout {
    /// The canonical on-disk string for this memory layout (`"arrow55"`).
    pub const fn as_str(self) -> &'static str {
        "arrow55"
    }
}

/// Disk layout version, orthogonal to the memory layout (`01 §2-1`).
///
/// Expressed as on-disk strings (`"arrow-ipc"` / `"arrow-parquet"`) and mapped
/// back to the enum through the explicit table in [`DiskLayout::from_str`];
/// unknown strings map to `None`. Further disk expressions are appended here in
/// later milestones (polars-family in M2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DiskLayout {
    /// Arrow IPC file format (`ARROW1` payload magic).
    ArrowIpc,
    /// Apache Parquet columnar format (`PAR1` payload magic).
    ArrowParquet,
}

impl DiskLayout {
    /// The canonical on-disk string for this disk layout
    /// (`"arrow-ipc"` / `"arrow-parquet"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            DiskLayout::ArrowIpc => "arrow-ipc",
            DiskLayout::ArrowParquet => "arrow-parquet",
        }
    }

    /// Explicit disk-string → enum mapping; `None` = unknown disk version
    /// (block side → degraded candidate, tree/boot side → hard failure).
    pub fn from_str(s: &str) -> Option<DiskLayout> {
        match s {
            "arrow-ipc" => Some(DiskLayout::ArrowIpc),
            "arrow-parquet" => Some(DiskLayout::ArrowParquet),
            _ => None,
        }
    }

    /// Probes a payload prefix for a known disk-format magic
    /// (`01 §4-2`, mirroring `layout/materializer.rs` magic dispatch).
    ///
    /// `ARROW1` → [`DiskLayout::ArrowIpc`], `PAR1` →
    /// [`DiskLayout::ArrowParquet`]; any other prefix (native/opaque bytes,
    /// unknown formats) → `None`.
    pub fn probe_magic(payload_first: &[u8]) -> Option<DiskLayout> {
        if payload_first.starts_with(b"ARROW1") {
            Some(DiskLayout::ArrowIpc)
        } else if payload_first.starts_with(b"PAR1") {
            Some(DiskLayout::ArrowParquet)
        } else {
            None
        }
    }
}
