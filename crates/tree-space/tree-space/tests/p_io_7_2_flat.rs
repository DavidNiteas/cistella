//! P-IO-7.2 扁平 (02 §9.4): the `ParquetDirLayout` → `FlatDirLayout` rename
//! refactor plus the flat layout's real dual-materialization write/read path.
//!
//! The rename is a pure identifier swap — no object byte, digest formula or
//! `DESIGN_VERSION` changes — and the whole P-IO-2..6 regression suite running
//! unchanged is the zero-behavior-change proof. This file adds the 7.2 surface:
//!
//! - the `StorageKind` string value synchronised to `flat_dir` (the v4-handle
//!   side of that assertion was removed by PL-1 S7 — 02 §5.9; the TB layout
//!   `kind()` reporting stays covered by the roundtrips here);
//! - an `ArrowTable` block materialized as Parquet writes → reopens → probes
//!   `PAR1` → decodes → identity cross-checks → `project` re-encodes the
//!   original tree bytes (roundtrip);
//! - one layout mixing both materializations (ArrowTable → Parquet; Sequence /
//!   `Blob` → native IPC) addresses and restores each object correctly;
//! - `Blob`/opaque blocks stay on native bytes and refuse the Parquet
//!   materializer;
//! - the new Parquet branch address golden (01 §1.11.7) plus an explicit guard
//!   that golden 17 (IPC block address) is untouched by the rename.
//!
//! This file adds no modification to existing golden assertions.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, TreeImage, encode, encode_node, named_field};
use tree_space::{
    ArrowTable, Blob, Block, BlockKind, BlockMaterializer, Bucket, Envelope, ErrorCode,
    IpcMaterializer, Materialization, ParquetMaterializer, Sequence, TbLibrary, TreeCodec,
    TreeNode, block_blob_address, block_ref_id, image_leaf_refs, probe_block_materialization,
    select_materializer,
};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One-column `Int32` Arrow table: the same deterministic sample used by the
/// P-IO-7.1 materializer units.
fn sample_table(rows: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(rows.to_vec()))]).unwrap(),
    )
    .unwrap()
}

/// Golden-17 sample: the canonical sample Blob of P-IO-2 (payload
/// `74622d73616d706c6500` = `tb-sample\0`).
fn sample_blob() -> Blob {
    Blob::new(vec![
        0x74, 0x62, 0x2d, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x00,
    ])
}

// ---------------------------------------------------------------------------
// The rename is behaviour preserving; the StorageKind string value is synced
// ---------------------------------------------------------------------------

#[test]
fn p_io_7_2_flat_dir_rename_preserves_default_library_behavior() {
    // The default `TbLibrary` is now `FlatDirLayout`; a Blob round-trips at its
    // golden-17 address exactly as before the rename.
    let golden_address = "d796cdd97c2ecac5dbeb3cc6ac9dc605";
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let blob = sample_blob();
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let root = temp.path().join("library");
    let obj = std::fs::read(root.join("tb-blocks").join(format!("{golden_address}.bin"))).unwrap();
    assert_eq!(
        obj,
        blob.envelope().encode(),
        "golden-17 frame bytes unchanged"
    );
    assert_eq!(
        probe_block_materialization(&obj).unwrap(),
        Materialization::Ipc
    );
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Dual materialization on the flat layout's real write/read path
// ---------------------------------------------------------------------------

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct SingleTable {
    data: ArrowTable,
}

#[test]
fn p_io_7_2_flat_parquet_roundtrip_bytes_equivalent() {
    // An `ArrowTable` under a parquet layout default writes an enveloped PAR1
    // object; reopen probes it, decodes it, and `project` reproduces the
    // original tree bytes (byte-equivalent).
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create_with_materialization(&root, Materialization::Parquet).unwrap();
    let data = sample_table(&[1, 2, 3]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    let tree = SingleTable { data: data.clone() };
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();

    // The on-disk object is the enveloped Parquet frame at its own address.
    let address = block_blob_address(&enveloped_parquet(&tree.data));
    let obj = std::fs::read(root.join("tb-blocks").join(format!("{address}.bin"))).unwrap();
    assert_eq!(
        probe_block_materialization(&obj).unwrap(),
        Materialization::Parquet
    );

    // Reopen with no parquet hint: probe → decode → identity → project.
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    let restored = reopened.bucket().unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(
        restored.get(data.ref_id()).unwrap().payload,
        data.envelope().payload,
        "a parquet-restored table carries the canonical envelope payload"
    );
    let projected: SingleTable = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);
    assert_eq!(projected.data.ref_id(), data.ref_id());
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Mixed {
    table_a: ArrowTable,
    table_b: ArrowTable,
    seq: Sequence,
    blob: Blob,
}

#[test]
fn p_io_7_2_flat_mixed_materialization_roundtrip() {
    // One layout, both materializations: ArrowTable blocks are Parquet under
    // the parquet default while `Sequence`/`Blob` stay native IPC (01 §1.11.3).
    // Reopen must locate/probe/decode every object independently.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create_with_materialization(&root, Materialization::Parquet).unwrap();
    let table_a = sample_table(&[1, 2]);
    let table_b = sample_table(&[3, 4]);
    let seq = Sequence::new(vec![tree_space::Value::I32(7)]);
    let blob = Blob::new(vec![9, 8, 7]);
    let mut bucket = Bucket::new();
    bucket.put(&table_a);
    bucket.put(&table_b);
    bucket.put(&seq);
    bucket.put(&blob);
    let tree = Mixed {
        table_a: table_a.clone(),
        table_b: table_b.clone(),
        seq: seq.clone(),
        blob: blob.clone(),
    };
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();

    // Each object is addressed by its own materialized bytes and probed to its
    // own format: tables PAR1, sequence/blob ARROW1 (native).
    let blocks_dir = root.join("tb-blocks");
    assert_eq!(std::fs::read_dir(&blocks_dir).unwrap().count(), 4);
    for (block, expected) in [
        (&table_a as &dyn Block, Materialization::Parquet),
        (&table_b as &dyn Block, Materialization::Parquet),
        (&seq as &dyn Block, Materialization::Ipc),
        (&blob as &dyn Block, Materialization::Ipc),
    ] {
        let block_obj = block.envelope().encode();
        let physical = if expected == Materialization::Parquet {
            enveloped_parquet(block)
        } else {
            block_obj.clone()
        };
        let address = block_blob_address(&physical);
        let obj = std::fs::read(blocks_dir.join(format!("{address}.bin"))).unwrap();
        assert_eq!(
            probe_block_materialization(&obj).unwrap(),
            expected,
            "block kind {:?} probes {expected:?}",
            block.kind()
        );
    }

    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 4);
    let restored = reopened.bucket().unwrap();
    assert_eq!(restored.len(), 4);
    let projected: Mixed = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);
    assert_eq!(projected.table_a.ref_id(), table_a.ref_id());
    assert_eq!(projected.table_b.ref_id(), table_b.ref_id());
    assert_eq!(projected.seq.envelope().payload, seq.envelope().payload);
    assert_eq!(projected.blob.envelope().payload, blob.envelope().payload);
}

#[test]
fn p_io_7_2_blob_and_opaque_never_accept_parquet() {
    // `Blob` and opaque registered content are zero-copy native blocks: the
    // Parquet materializer refuses them, the unified selector always returns
    // the IPC materializer, and under a parquet default the written object is
    // the native envelope at the IPC address.
    let blob_env = Envelope::new(BlockKind::Blob, vec![1, 2, 3]);
    let error = ParquetMaterializer.encode(&blob_env).unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
    let named = BlockKind::Named("reg".into());
    let selected = select_materializer(Materialization::Parquet, &named);
    let native = IpcMaterializer.encode(&blob_env).unwrap();
    assert_eq!(
        selected.encode(&blob_env).unwrap(),
        native,
        "opaque registered blocks never follow a parquet default"
    );

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let library = TbLibrary::create_with_materialization(&root, Materialization::Parquet).unwrap();
    let blob = Blob::new(vec![0xaa]);
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(&tree_bytes, &image_leaf_refs(&image).unwrap(), &bucket)
        .unwrap();
    let written = std::fs::read(root.join("tb-blocks").join(format!(
        "{}.bin",
        block_blob_address(&blob.envelope().encode())
    )))
    .unwrap();
    assert_eq!(written, blob.envelope().encode());
    assert_eq!(
        probe_block_materialization(&written).unwrap(),
        Materialization::Ipc
    );
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Golden obligations (01 §1.11.7)
// ---------------------------------------------------------------------------

/// Frozen storage address of the sample ArrowTable envelope when materialized
/// as Parquet (new 7.2 golden). Computed as
/// `block_blob_address(ParquetMaterializer.encode(sample_table(&[1, 2, 3]).envelope()))`.
const PARQUET_ADDRESS: &str = "efaf05e1754df32a1ebec16a0a5a881d";
/// Frozen semantic identity of the same sample ArrowTable (materialization
/// independent; PL-2 M2 re-freeze — the semantic table fingerprint, identical
/// across the IPC and Parquet materializations).
const SAMPLE_TABLE_REF_ID: &str = "4ee1fd39d16713d6aedcbb37056c6b65";
/// Golden 17: the frozen IPC block address of the P-IO-2 sample Blob.
const GOLDEN_17_BLOB_ADDRESS: &str = "d796cdd97c2ecac5dbeb3cc6ac9dc605";
/// Golden-17 semantic RefId (PL-2 M2 re-freeze).
const GOLDEN_17_BLOB_REF_ID: &str = "d647966165bab1e26a2cbec5ec794735";

#[test]
fn p_io_7_2_parquet_block_address_is_frozen() {
    // The same logical block keeps one semantic `RefId` across materializations
    // but its storage address follows the physical bytes (01 §1.11.4): the
    // Parquet address is frozen here, distinct from the IPC one.
    let data = sample_table(&[1, 2, 3]);
    let envelope = data.envelope();
    let ref_id = block_ref_id(envelope.kind.clone(), &envelope.payload);
    assert_eq!(hex(&ref_id.as_bytes()), SAMPLE_TABLE_REF_ID);

    let physical = ParquetMaterializer.encode(&envelope).unwrap();
    assert_eq!(
        probe_block_materialization(&physical).unwrap(),
        Materialization::Parquet
    );
    let address = block_blob_address(&physical);
    assert_eq!(hex(&address.as_bytes()), PARQUET_ADDRESS);

    // Decode restores the canonical envelope (identity is preserved).
    let decoded = ParquetMaterializer.decode(&physical).unwrap();
    let decoded_id = block_ref_id(decoded.kind.clone(), &decoded.payload);
    assert_eq!(decoded_id, ref_id);

    // Identity ⊥ address: the IPC bytes of the same logical block address
    // differently.
    let ipc_physical = IpcMaterializer.encode(&envelope).unwrap();
    let ipc_address = block_blob_address(&ipc_physical);
    assert_ne!(ipc_address, address);
    assert_ne!(hex(&ipc_address.as_bytes()), PARQUET_ADDRESS);
}

#[test]
fn p_io_7_2_golden_17_ipc_block_address_unchanged() {
    // Explicit regression guard: the rename must not disturb golden 17 — the
    // P-IO-2 sample Blob still frames to the same bytes and the same address.
    let blob = sample_blob();
    let bytes = blob.envelope().encode();
    assert_eq!(
        block_blob_address(&bytes).as_bytes(),
        hex16(GOLDEN_17_BLOB_ADDRESS)
    );
    assert_eq!(blob.ref_id().as_bytes(), hex16(GOLDEN_17_BLOB_REF_ID));
    assert_ne!(
        block_blob_address(&bytes).as_bytes(),
        blob.ref_id().as_bytes(),
        "storage addressing stays independent of the semantic RefId"
    );
}

/// The unique envelope frame for `block` under the Parquet materializer.
fn enveloped_parquet(block: &dyn Block) -> Vec<u8> {
    let envelope = Envelope::new(block.kind(), block.payload());
    ParquetMaterializer.encode(&envelope).unwrap()
}
