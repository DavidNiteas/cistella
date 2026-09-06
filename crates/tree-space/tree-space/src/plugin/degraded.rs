//! Degraded-block residency for plugin-unreachable blocks
//! (`01-目标与设计.md` §4-5 / 02 §5.8): a block whose disk version is unknown
//! or has no routable plugin is kept as its original disk bytes instead of
//! failing the whole library read.

use crate::block::BlockKind;

/// The on-disk layout-version indicator of a missing-plugin block
/// (01 §4-5): the calibrated literal is preserved verbatim — an unknown
/// literal also stays verbatim and never participates in routing or pairing.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DiskVersion(
    /// The disk-version string (`"arrow-ipc"` / `"arrow-parquet"` or an
    /// unknown literal exactly as labeled).
    pub String,
);

/// The degraded residency of a missing-plugin block (01 §4-5): the original
/// disk object bytes are kept — not a [`crate::block::Blob`], because `Blob`
/// is an Arrow IPC wrapper, not raw bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegradedBlock {
    /// The block kind parsed from the object's frame header. A header that
    /// cannot be parsed is [`crate::error::ErrorCode::PayloadMalformed`] —
    /// never degraded.
    pub kind: BlockKind,
    /// The disk-version label (side-table / magic-probe calibrated; unknown
    /// literals verbatim).
    pub disk: DiskVersion,
    /// The original disk object bytes.
    pub raw: Vec<u8>,
}
