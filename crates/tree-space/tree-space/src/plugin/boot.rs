//! Boot block (`01-目标与设计.md` §4-3): the frozen Arrow 55 single-batch
//! four-column record, independent of the plugin system.
//!
//! The boot record is *write-once* at library creation and carries the library
//! id (human-readable) plus the tree's memory/disk layout-version pair. Its
//! schema, channel naming and fixed file name are frozen — any later change
//! must declare an explicit replacement (01 §7).

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ipc::{decode_batch, encode_batch};
use crate::plugin::version::DiskLayout;
use arrow::array::{Array, StringArray, UInt16Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

/// The only supported boot record version in PL-1.
pub const BOOT_VERSION: u16 = 1;

/// A boot record: the strong-typed write/read surface of the boot block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootRecord {
    /// The boot format version (`BOOT_VERSION`).
    pub boot_version: u16,
    /// The library id (human-readable, e.g. `"tree-space"`).
    pub library_id: String,
    /// The tree's in-memory layout version string (e.g. `"arrow55"`).
    pub tree_mem_version: String,
    /// The tree's on-disk layout version string (e.g. `"arrow-ipc"`).
    pub tree_disk_version: String,
}

/// The frozen boot Arrow schema: four columns
/// `boot_version u16 / library_id utf8 / tree_mem_version utf8 /
/// tree_disk_version utf8`, all non-nullable.
pub fn boot_schema() -> Schema {
    Schema::new(vec![
        Field::new("boot_version", DataType::UInt16, false),
        Field::new("library_id", DataType::Utf8, false),
        Field::new("tree_mem_version", DataType::Utf8, false),
        Field::new("tree_disk_version", DataType::Utf8, false),
    ])
}

/// Encodes a boot record as one canonical Arrow IPC batch (single row).
pub fn encode_boot(record: &BootRecord) -> Result<Vec<u8>> {
    let batch = RecordBatch::try_new(
        Arc::new(boot_schema()),
        vec![
            Arc::new(UInt16Array::from(vec![record.boot_version])),
            Arc::new(StringArray::from(vec![record.library_id.as_str()])),
            Arc::new(StringArray::from(vec![record.tree_mem_version.as_str()])),
            Arc::new(StringArray::from(vec![record.tree_disk_version.as_str()])),
        ],
    )
    .map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "cannot build the boot record batch",
        )
        .with_context("detail", error.to_string())
    })?;
    encode_batch(&batch)
}

/// Decodes boot bytes back into a [`BootRecord`], deeply validating the schema
/// (column names, types, nullability), the single-row shape, the supported
/// boot version and the known tree disk version. *Any* violation is a hard
/// [`ErrorCode::BootstrapIncomplete`] failure — including a structurally broken
/// payload that does not decode as Arrow IPC at all (01 §4-3: boot broken,
/// unsupported version, or unknown tree disk version → `BootstrapIncomplete`;
/// no degradation for the tree spine).
pub fn decode_boot(bytes: &[u8]) -> Result<BootRecord> {
    let batch = decode_batch(bytes).map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::BootstrapIncomplete,
            "boot payload is not a valid single Arrow IPC batch",
        )
        .with_context("detail", error.message)
    })?;
    let expected = boot_schema();
    let actual = batch.schema();
    if actual.fields().len() != expected.fields().len() {
        return Err(boot_invalid(
            "boot schema does not match the frozen four-column layout",
        ));
    }
    for (index, expected_field) in expected.fields().iter().enumerate() {
        let actual_field = &actual.fields()[index];
        if actual_field.name() != expected_field.name()
            || actual_field.data_type() != expected_field.data_type()
            || actual_field.is_nullable() != expected_field.is_nullable()
        {
            return Err(boot_invalid(
                "boot schema does not match the frozen four-column layout",
            ));
        }
    }
    if batch.num_rows() != 1 {
        return Err(boot_invalid("boot record must be exactly one row"));
    }
    let boot_version = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt16Array>()
        .ok_or_else(|| boot_invalid("boot_version column is not u16"))?;
    let library_id = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| boot_invalid("library_id column is not utf8"))?;
    let tree_mem_version = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| boot_invalid("tree_mem_version column is not utf8"))?;
    let tree_disk_version = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| boot_invalid("tree_disk_version column is not utf8"))?;
    if boot_version.is_null(0)
        || library_id.is_null(0)
        || tree_mem_version.is_null(0)
        || tree_disk_version.is_null(0)
    {
        return Err(boot_invalid("boot record columns cannot be null"));
    }
    let record = BootRecord {
        boot_version: boot_version.value(0),
        library_id: library_id.value(0).to_owned(),
        tree_mem_version: tree_mem_version.value(0).to_owned(),
        tree_disk_version: tree_disk_version.value(0).to_owned(),
    };
    if record.boot_version != BOOT_VERSION {
        return Err(boot_invalid("boot version is not supported")
            .with_context("boot_version", record.boot_version.to_string()));
    }
    if DiskLayout::from_str(&record.tree_disk_version).is_none() {
        return Err(boot_invalid("tree disk version is not a known disk layout")
            .with_context("tree_disk_version", record.tree_disk_version.clone()));
    }
    Ok(record)
}

fn boot_invalid(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::BootstrapIncomplete, message)
}
