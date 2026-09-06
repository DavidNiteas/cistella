//! AB-2: registered (named) block kinds, envelope v2, and capability tiers.

use arrow::array::Int64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::{
    Block, BlockCaps, BlockKind, Bucket, Envelope, ErrorCode, RegisteredBlock, Sequence, Value,
    block_caps, block_ref_id, read_block, register_block, validate_block_name,
};

/// An opaque registered block: payload is the raw bytes, no Arrow projection.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpaqueBlock(Vec<u8>);

impl RegisteredBlock for OpaqueBlock {
    const NAME: &'static str = "org.example.ab2.bench";
    fn encode(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        Ok(Self(bytes.to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}

fn metrics_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        "count",
        DataType::Int64,
        false,
    )]))
}

fn metrics_to_batch(bytes: &[u8]) -> Result<RecordBatch, tree_space::TreeSpaceError> {
    let batch = tree_space::ipc::decode_batch(bytes)?;
    Ok(batch)
}

/// An Arrow registered block: payload is canonical IPC of a single Int64 column.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetricsBlock(Vec<i64>);

impl RegisteredBlock for MetricsBlock {
    const NAME: &'static str = "org.example.ab2.metrics";
    fn encode(&self) -> Vec<u8> {
        tree_space::ipc::encode_batch(
            &RecordBatch::try_new(
                metrics_schema(),
                vec![Arc::new(Int64Array::from(self.0.clone()))],
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        let batch = metrics_to_batch(bytes)?;
        let array = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("metrics payload contains an Int64 column");
        Ok(Self(array.values().to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Arrow(tree_space::ArrowCaps {
            schema: metrics_schema(),
            to_batch: metrics_to_batch,
        })
    }
}

#[test]
fn ab2_opaque_block_enters_bucket_and_roundtrips() {
    register_block::<OpaqueBlock>().unwrap();
    let block = OpaqueBlock(b"AB2-OPAQUE".to_vec());
    let mut bucket = Bucket::new();
    let id = bucket.put(&block);
    assert_eq!(bucket.put(&block), id);
    assert_eq!(bucket.len(), 1);
    bucket.verify_all().unwrap();

    let restored = bucket.read(id).unwrap();
    assert_eq!(
        restored.kind(),
        BlockKind::Named("org.example.ab2.bench".into())
    );
    assert_eq!(
        OpaqueBlock::decode(&restored.payload()).unwrap(),
        block,
        "strong-typed path rebuilds the registered value from canonical bytes"
    );

    let computed = block_ref_id(
        BlockKind::Named("org.example.ab2.bench".into()),
        &block.encode(),
    );
    assert_eq!(computed, id);
    assert!(matches!(
        block_caps("org.example.ab2.bench").unwrap(),
        BlockCaps::Opaque
    ));
}

#[test]
fn ab2_arrow_block_roundtrips_and_exposes_caps() {
    register_block::<MetricsBlock>().unwrap();
    let block = MetricsBlock(vec![1, 2, 3]);
    let mut bucket = Bucket::new();
    let id = bucket.put(&block);
    bucket.verify_all().unwrap();
    let restored = bucket.read(id).unwrap();
    assert_eq!(MetricsBlock::decode(&restored.payload()).unwrap(), block);
    let caps = block_caps("org.example.ab2.metrics").unwrap();
    match caps {
        BlockCaps::Arrow(caps) => {
            let batch = (caps.to_batch)(&block.encode()).unwrap();
            assert_eq!(batch.num_rows(), 3);
        }
        BlockCaps::Opaque => panic!("metrics block must be Arrow"),
    }
}

#[test]
fn ab2_named_envelope_v2_roundtrips() {
    register_block::<EnvelopeBlock>().unwrap();
    let block = EnvelopeBlock(b"v2".to_vec());
    let env = block.envelope();
    assert_eq!(env.kind.tag(), 5);
    let bytes = env.encode();
    assert_eq!(
        bytes[0], 5,
        "named envelope carries the registered kind tag"
    );
    let decoded = Envelope::decode(&bytes).unwrap();
    assert_eq!(decoded, env);
    let named = read_block(env.kind.clone(), &env.payload).unwrap();
    assert_eq!(named.kind(), env.kind);
    assert_eq!(EnvelopeBlock::decode(&named.payload()).unwrap(), block);
}

#[test]
fn ab2_unregistered_name_is_a_type_not_found() {
    let kind = BlockKind::Named("org.example.ab2.nope".into());
    let err = read_block(kind, b"x").unwrap_err();
    assert_eq!(err.code, ErrorCode::TypeNotFound);
}

#[test]
fn ab2_builtin_names_cannot_be_overridden_and_duplicates_are_rejected() {
    register_block::<SoloBlock>().unwrap();
    assert_eq!(
        register_block::<SoloBlock>().unwrap_err().code,
        ErrorCode::TypeConflict,
        "duplicate registration rejected"
    );
    assert_eq!(
        register_block::<BuiltinishBlock>().unwrap_err().code,
        ErrorCode::TypeConflict,
        "built-in name override rejected"
    );
}

#[test]
fn ab2_block_name_rules_are_enforced() {
    assert!(validate_block_name("ok.org.example").is_ok());
    assert!(validate_block_name("").is_err());
    assert!(validate_block_name(&"x".repeat(65)).is_err());
    assert!(validate_block_name("a/b").is_err());
    assert!(validate_block_name("tb-block-x").is_err());
    assert!(validate_block_name("a b").is_ok());
}

#[test]
fn ab2_opaque_identity_is_frozen_for_empty_payload() {
    register_block::<EmptyBlock>().unwrap();
    let block = EmptyBlock;
    let id = block_ref_id(
        BlockKind::Named("org.example.ab2.empty".into()),
        &block.encode(),
    );
    // PL-2 M2 re-freeze (02 §6.4): registered blocks map their raw payload as
    // a byte semantic value (deviation reported at S2 — registered kinds carry
    // no declarable generic semantics).
    assert_eq!(id.to_string(), "386083eca7ae526afee46a9d15af70d3");
    assert_eq!(block.ref_id(), id);
}

/// A sequence with a null element still carries an Arrow IPC payload.
#[test]
fn ab2_sequence_with_nulls_is_canonical() {
    let sequence = Sequence::new(vec![Value::I32(1), Value::Null, Value::I32(3)]);
    let decoded = Sequence::decode(&sequence.encode()).unwrap();
    assert_eq!(decoded.values().len(), 3);
}

/// An opaque registered block used by the envelope-v2 test.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EnvelopeBlock(Vec<u8>);

impl RegisteredBlock for EnvelopeBlock {
    const NAME: &'static str = "org.example.ab2.env";
    fn encode(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        Ok(Self(bytes.to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}

/// An opaque registered block used by the empty-payload identity test.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EmptyBlock;

impl RegisteredBlock for EmptyBlock {
    const NAME: &'static str = "org.example.ab2.empty";
    fn encode(&self) -> Vec<u8> {
        Vec::new()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        assert!(bytes.is_empty(), "empty registered block payload");
        Ok(Self)
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}

/// A registered block name used only by the duplicate-rejection test.
#[derive(Debug)]
struct SoloBlock;
impl RegisteredBlock for SoloBlock {
    const NAME: &'static str = "org.example.ab2.solo";
    fn encode(&self) -> Vec<u8> {
        Vec::new()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        assert!(bytes.is_empty());
        Ok(Self)
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}

#[derive(Debug)]
struct BuiltinishBlock(Vec<u8>);
impl RegisteredBlock for BuiltinishBlock {
    const NAME: &'static str = "table";
    fn encode(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn decode(bytes: &[u8]) -> Result<Self, tree_space::TreeSpaceError> {
        Ok(Self(bytes.to_vec()))
    }
    fn capabilities() -> BlockCaps {
        BlockCaps::Opaque
    }
}
