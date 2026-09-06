//! cistella's tree-space registration layer.
//!
//! This crate is intentionally small: it registers cistella-owned Named Blocks
//! on top of `tree-space` without re-implementing tree, bucket, or persistence
//! semantics. Domain crates should depend on this crate instead of directly
//! inventing unregistered bucket layouts.

use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use arrow::array::{ArrayRef, BooleanArray, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tree_space::block::{ArrowCaps, BlockCaps, RegisteredBlock, register_block};
use tree_space::error::{ErrorCode, Result, TreeSpaceError};

/// The semantic version shared by the initial cistella registered blocks.
pub const BLOCK_SEMVER_V1: &str = "1.0.0";

/// Stable block name for the minimal literature metadata Arrow block.
pub const LITERATURE_ITEMS_BLOCK: &str = "cistella.literature.items";
/// Stable block name for opaque asset bytes plus recovery metadata.
pub const ASSET_OBJECT_BLOCK: &str = "cistella.assets.object";

/// Returns all block names registered by [`register`].
pub const fn registered_block_names() -> [&'static str; 2] {
    [LITERATURE_ITEMS_BLOCK, ASSET_OBJECT_BLOCK]
}

/// Registers the cistella Named Blocks in the process-wide tree-space registry.
///
/// The operation is idempotent. A first-call failure is cached and returned on
/// later calls, mirroring the registration style used by tree-space extension
/// crates.
pub fn register() -> Result<()> {
    static REGISTRATION: OnceLock<Result<()>> = OnceLock::new();
    REGISTRATION
        .get_or_init(|| {
            register_block::<LiteratureItemsBlock>()?;
            register_block::<AssetObjectBlock>()?;
            Ok(())
        })
        .clone()
}

/// A minimal Arrow-backed literature item table.
///
/// It is not the final literature domain model. It is the plugin-layer carrier
/// that proves cistella can register an Arrow-capable business block with a
/// stable name, version, schema, codec, and validator.
#[derive(Debug, Clone)]
pub struct LiteratureItemsBlock {
    batch: RecordBatch,
}

impl LiteratureItemsBlock {
    /// Builds a literature item block after validating the canonical schema.
    pub fn try_new(batch: RecordBatch) -> Result<Self> {
        validate_literature_schema(batch.schema().as_ref())?;
        Ok(Self { batch })
    }

    /// Returns the underlying Arrow batch.
    pub fn batch(&self) -> &RecordBatch {
        &self.batch
    }

    /// Builds a small block used by tests and downstream examples.
    pub fn example_one(id: &str, title: &str) -> Result<Self> {
        let schema = literature_items_schema();
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec![id])) as ArrayRef,
            Arc::new(StringArray::from(vec![title])) as ArrayRef,
            Arc::new(StringArray::from(vec![None::<&str>])) as ArrayRef,
            Arc::new(UInt64Array::from(vec![0_u64])) as ArrayRef,
            Arc::new(BooleanArray::from(vec![false])) as ArrayRef,
        ];
        let batch = RecordBatch::try_new(schema, columns).map_err(arrow_error)?;
        Self::try_new(batch)
    }
}

impl RegisteredBlock for LiteratureItemsBlock {
    const NAME: &'static str = LITERATURE_ITEMS_BLOCK;

    fn encode(&self) -> Vec<u8> {
        encode_batch(&self.batch).expect("validated literature batch encodes as Arrow IPC")
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let batch = decode_single_batch(bytes)?;
        Self::try_new(batch)
    }

    fn capabilities() -> BlockCaps {
        BlockCaps::Arrow(ArrowCaps {
            schema: literature_items_schema(),
            to_batch: decode_single_batch,
        })
    }
}

/// Recovery metadata for an opaque cistella asset block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetObjectMetadata {
    /// Semantic version of this opaque frame.
    pub semantic_version: String,
    /// MIME type when known, for example `application/pdf`.
    pub mime: String,
    /// Original file name captured at import time.
    pub original_name: Option<String>,
    /// Original extension without a leading dot.
    pub extension: Option<String>,
    /// Payload size in bytes.
    pub size_bytes: u64,
    /// Lower-case SHA-256 hex digest of the payload.
    pub sha256: String,
}

/// Opaque asset bytes plus recovery metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetObjectBlock {
    metadata: AssetObjectMetadata,
    bytes: Vec<u8>,
}

impl AssetObjectBlock {
    /// Builds an opaque asset object and derives size/hash metadata.
    pub fn new(
        bytes: impl Into<Vec<u8>>,
        mime: impl Into<String>,
        original_name: Option<String>,
        extension: Option<String>,
    ) -> Result<Self> {
        let bytes = bytes.into();
        let mime = mime.into();
        if mime.trim().is_empty() {
            return Err(invalid_manifest("asset MIME type must not be empty"));
        }
        let metadata = AssetObjectMetadata {
            semantic_version: BLOCK_SEMVER_V1.to_string(),
            mime,
            original_name,
            extension,
            size_bytes: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        };
        Ok(Self { metadata, bytes })
    }

    /// Returns recovery metadata.
    pub fn metadata(&self) -> &AssetObjectMetadata {
        &self.metadata
    }

    /// Returns the raw opaque payload.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl RegisteredBlock for AssetObjectBlock {
    const NAME: &'static str = ASSET_OBJECT_BLOCK;

    fn encode(&self) -> Vec<u8> {
        encode_asset_frame(&self.metadata, &self.bytes)
            .expect("validated asset metadata encodes as JSON")
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let (metadata, payload) = decode_asset_frame(bytes)?;
        validate_asset_metadata(&metadata, &payload)?;
        Ok(Self {
            metadata,
            bytes: payload,
        })
    }

    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}

/// The canonical schema for the initial literature item Arrow block.
pub fn literature_items_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("item_id", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("primary_doi", DataType::Utf8, true),
        Field::new("updated_at_epoch_ms", DataType::UInt64, false),
        Field::new("deleted", DataType::Boolean, false),
    ]))
}

fn validate_literature_schema(schema: &Schema) -> Result<()> {
    if schema == literature_items_schema().as_ref() {
        Ok(())
    } else {
        Err(TreeSpaceError::new(
            ErrorCode::SchemaMismatch,
            "literature item block schema does not match cistella v1",
        ))
    }
}

fn encode_batch(batch: &RecordBatch) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut writer =
            StreamWriter::try_new(&mut out, batch.schema().as_ref()).map_err(arrow_error)?;
        writer.write(batch).map_err(arrow_error)?;
        writer.finish().map_err(arrow_error)?;
    }
    Ok(out)
}

fn decode_single_batch(bytes: &[u8]) -> Result<RecordBatch> {
    let cursor = Cursor::new(bytes);
    let reader = StreamReader::try_new(cursor, None).map_err(arrow_error)?;
    let batches = reader
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(arrow_error)?;
    match batches.as_slice() {
        [batch] => Ok(batch.clone()),
        [] => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "Arrow block must contain one record batch, got zero",
        )),
        _ => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "Arrow block must contain exactly one record batch",
        )),
    }
}

const ASSET_MAGIC: &[u8; 8] = b"CISTAOB1";

fn encode_asset_frame(metadata: &AssetObjectMetadata, payload: &[u8]) -> Result<Vec<u8>> {
    let metadata_bytes = serde_json::to_vec(metadata).map_err(json_error)?;
    let metadata_len: u32 = metadata_bytes
        .len()
        .try_into()
        .map_err(|_| invalid_manifest("asset metadata is too large"))?;
    let mut out = Vec::with_capacity(12 + metadata_bytes.len() + payload.len());
    out.extend_from_slice(ASSET_MAGIC);
    out.extend_from_slice(&metadata_len.to_le_bytes());
    out.extend_from_slice(&metadata_bytes);
    out.extend_from_slice(payload);
    Ok(out)
}

fn decode_asset_frame(bytes: &[u8]) -> Result<(AssetObjectMetadata, Vec<u8>)> {
    if bytes.len() < 12 || &bytes[..8] != ASSET_MAGIC {
        return Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "asset object frame magic is missing or invalid",
        ));
    }
    let metadata_len = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("four-byte metadata length slice"),
    ) as usize;
    let metadata_end = 12_usize.checked_add(metadata_len).ok_or_else(|| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "asset metadata length overflows",
        )
    })?;
    if metadata_end > bytes.len() {
        return Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "asset metadata length exceeds frame length",
        ));
    }
    let metadata = serde_json::from_slice(&bytes[12..metadata_end]).map_err(json_error)?;
    Ok((metadata, bytes[metadata_end..].to_vec()))
}

fn validate_asset_metadata(metadata: &AssetObjectMetadata, payload: &[u8]) -> Result<()> {
    if metadata.semantic_version != BLOCK_SEMVER_V1 {
        return Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "asset object semantic version is not supported",
        )
        .with_context("version", metadata.semantic_version.clone()));
    }
    if metadata.mime.trim().is_empty() {
        return Err(invalid_manifest("asset MIME type must not be empty"));
    }
    if metadata.size_bytes != payload.len() as u64 {
        return Err(TreeSpaceError::new(
            ErrorCode::DigestMismatch,
            "asset object size does not match metadata",
        ));
    }
    let actual = sha256_hex(payload);
    if metadata.sha256 != actual {
        return Err(TreeSpaceError::new(
            ErrorCode::DigestMismatch,
            "asset object SHA-256 does not match metadata",
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn arrow_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "Arrow IPC payload is malformed",
    )
    .with_context("detail", error.to_string())
}

fn json_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, "JSON payload is malformed")
        .with_context("detail", error.to_string())
}

fn invalid_manifest(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::ManifestInvalid, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_space::block::{BlockCaps, block_caps};

    #[test]
    fn register_is_idempotent_and_exposes_caps() {
        register().unwrap();
        register().unwrap();

        match block_caps(LITERATURE_ITEMS_BLOCK).unwrap() {
            BlockCaps::Arrow(caps) => {
                assert_eq!(caps.schema.as_ref(), literature_items_schema().as_ref())
            }
            BlockCaps::Opaque => panic!("literature items must be Arrow-capable"),
        }
        assert!(matches!(
            block_caps(ASSET_OBJECT_BLOCK),
            Some(BlockCaps::Opaque)
        ));
    }

    #[test]
    fn literature_arrow_block_roundtrips() {
        let block = LiteratureItemsBlock::example_one("lit-1", "A paper").unwrap();
        let encoded = block.encode();
        let decoded = LiteratureItemsBlock::decode(&encoded).unwrap();

        assert_eq!(decoded.batch().num_rows(), 1);
        assert_eq!(
            decoded.batch().schema().as_ref(),
            literature_items_schema().as_ref()
        );
    }

    #[test]
    fn literature_schema_mismatch_is_rejected() {
        let schema = Arc::new(Schema::new(vec![Field::new("bad", DataType::Utf8, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["x"])) as ArrayRef],
        )
        .unwrap();
        let err = LiteratureItemsBlock::try_new(batch).unwrap_err();
        assert_eq!(err.code, ErrorCode::SchemaMismatch);
    }

    #[test]
    fn opaque_asset_roundtrips_and_validates_recovery_metadata() {
        let block = AssetObjectBlock::new(
            b"pdf bytes".to_vec(),
            "application/pdf",
            Some("paper.pdf".to_string()),
            Some("pdf".to_string()),
        )
        .unwrap();
        let encoded = block.encode();
        let decoded = AssetObjectBlock::decode(&encoded).unwrap();

        assert_eq!(decoded.bytes(), b"pdf bytes");
        assert_eq!(decoded.metadata().mime, "application/pdf");
        assert_eq!(
            decoded.metadata().original_name.as_deref(),
            Some("paper.pdf")
        );
        assert_eq!(decoded.metadata().size_bytes, 9);
    }

    #[test]
    fn opaque_asset_digest_mismatch_is_rejected() {
        let block = AssetObjectBlock::new(b"abc".to_vec(), "text/plain", None, None).unwrap();
        let mut encoded = block.encode();
        let last = encoded.last_mut().unwrap();
        *last ^= 0x01;

        let err = AssetObjectBlock::decode(&encoded).unwrap_err();
        assert_eq!(err.code, ErrorCode::DigestMismatch);
    }
}
