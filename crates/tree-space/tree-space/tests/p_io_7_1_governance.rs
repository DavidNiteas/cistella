//! P-IO-7.1 统御: the unified data interface (`TbLayout` decoupling +
//! pluggable block materialization).
//!
//! Per `_dev/树与桶管道/02-施工路线图.md` §9.3 this covers:
//!
//! - materializer units: `IpcMaterializer` encode/decode/address roundtrip,
//!   `ParquetMaterializer` `ArrowTable` roundtrip with an address distinct from
//!   the IPC one;
//! - format probing: `ARROW1`/`PAR1` payload magic dispatches correctly;
//! - identity ⊥ address: the same logical block materialized as IPC and as
//!   Parquet has one `RefId` and two distinct addresses;
//! - verify through the probe-dispatched decode recomputes identity correctly,
//!   end to end on a Parquet-default library; `Blob`/opaque blocks stay on
//!   native bytes under any default.
//!
//! No new byte golden is frozen here: every equality is behavioral or
//! content-identity, so the existing golden set is untouched (the Parquet
//! address golden is P-IO-7.2's obligation, 01 §1.11.7).

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, TreeImage, encode, encode_node};
use tree_space::{
    ArrowTable, Blob, Block, BlockKind, BlockMaterializer, Bucket, Envelope, ErrorCode,
    IpcMaterializer, Materialization, ParquetMaterializer, TbLibrary, TreeCodec, TreeNode,
    block_blob_address, block_ref_id, materializer_for, named_field, probe_block_materialization,
    select_materializer,
};

fn table(ids: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(ids.to_vec()))]).unwrap(),
    )
    .unwrap()
}

fn canonical_envelope(block: &impl Block) -> Envelope {
    Envelope::new(block.kind(), block.payload())
}

fn identity(envelope: &Envelope) -> tree_space::RefId {
    block_ref_id(envelope.kind.clone(), &envelope.payload)
}

#[test]
fn p_io_7_1_ipc_materializer_roundtrip() {
    // The default materializer encodes the canonical envelope frame as-is and
    // decodes it back without interpreting the payload; its address is the
    // `tb-blob` full-content hash (golden-17 formula).
    let data = table(&[1, 2, 3]);
    let envelope = canonical_envelope(&data);
    let ipc = IpcMaterializer;
    let physical = ipc.encode(&envelope).unwrap();
    assert_eq!(physical, envelope.encode());
    assert_eq!(
        ipc.address(&physical),
        block_blob_address(&physical),
        "IPC address is the tb-blob full-content hash"
    );
    let decoded = ipc.decode(&physical).unwrap();
    assert_eq!(decoded.kind, envelope.kind);
    assert_eq!(decoded.payload, envelope.payload);
    assert_eq!(identity(&decoded), identity(&envelope));
}

#[test]
fn p_io_7_1_parquet_materializer_roundtrip_and_distinct_address() {
    // The columnar materializer rebuilds the canonical envelope from Parquet;
    // its object bytes (and therefore its address) differ from the IPC ones.
    let data = table(&[4, 5, 6]);
    let envelope = canonical_envelope(&data);
    let parquet = ParquetMaterializer;
    let physical = parquet.encode(&envelope).unwrap();
    assert_ne!(physical, envelope.encode());
    assert!(physical[11..].starts_with(b"PAR1"), "parquet payload magic");
    let decoded = parquet.decode(&physical).unwrap();
    assert_eq!(decoded.kind, envelope.kind);
    assert_eq!(identity(&decoded), identity(&envelope));

    let ipc = IpcMaterializer;
    assert_ne!(
        parquet.address(&physical),
        ipc.address(&envelope.encode()),
        "the same logical block gets a distinct physical address per materialization"
    );
}

#[test]
fn p_io_7_1_format_probe_dispatches_arrow1_and_par1() {
    // Magic dispatch: `ARROW1` payload → IPC, `PAR1` payload → Parquet.
    let data = table(&[7]);
    let envelope = canonical_envelope(&data);
    let ipc_physical = IpcMaterializer.encode(&envelope).unwrap();
    let pq_physical = ParquetMaterializer.encode(&envelope).unwrap();
    assert!(ipc_physical[11..].starts_with(b"ARROW1"));
    assert!(pq_physical[11..].starts_with(b"PAR1"));
    assert_eq!(
        probe_block_materialization(&ipc_physical).unwrap(),
        Materialization::Ipc
    );
    assert_eq!(
        probe_block_materialization(&pq_physical).unwrap(),
        Materialization::Parquet
    );
    // The dispatched materializer decodes the object for either format.
    let ipc_decoded = materializer_for(Materialization::Ipc)
        .decode(&ipc_physical)
        .unwrap();
    let pq_decoded = materializer_for(Materialization::Parquet)
        .decode(&pq_physical)
        .unwrap();
    assert_eq!(identity(&ipc_decoded), identity(&envelope));
    assert_eq!(identity(&pq_decoded), identity(&envelope));
}

#[test]
fn p_io_7_1_identity_is_materialization_independent() {
    // 01 §1.11.4 identity ⊥ address: one logical block, one `RefId`, two
    // distinct physical objects.
    let data = table(&[42]);
    let envelope = canonical_envelope(&data);
    let expected = identity(&envelope);
    let ipc_physical = IpcMaterializer.encode(&envelope).unwrap();
    let pq_physical = ParquetMaterializer.encode(&envelope).unwrap();
    assert_ne!(ipc_physical, pq_physical);

    let ipc_decoded = IpcMaterializer.decode(&ipc_physical).unwrap();
    let pq_decoded = ParquetMaterializer.decode(&pq_physical).unwrap();
    assert_eq!(identity(&ipc_decoded), expected);
    assert_eq!(identity(&pq_decoded), expected);
    assert_ne!(
        IpcMaterializer.address(&ipc_physical),
        ParquetMaterializer.address(&pq_physical),
        "different materialized bytes must address differently"
    );
}

#[test]
fn p_io_7_1_selection_follows_default_and_kind() {
    // 01 §1.11.3 materialization selection: the layout default plus the kind
    // constraint, applied as one unified entry (no per-block knobs).
    let table_env = canonical_envelope(&table(&[1]));
    let blob_env = canonical_envelope(&Blob::new(vec![9]));
    let table_parquet = select_materializer(Materialization::Parquet, &BlockKind::Table)
        .encode(&table_env)
        .unwrap();
    assert_eq!(
        table_parquet,
        ParquetMaterializer.encode(&table_env).unwrap(),
        "ArrowTable follows a parquet layout default"
    );
    let blob_native = select_materializer(Materialization::Parquet, &BlockKind::Blob)
        .encode(&blob_env)
        .unwrap();
    assert_eq!(
        blob_native,
        IpcMaterializer.encode(&blob_env).unwrap(),
        "Blob/opaque stays on native bytes under any default"
    );
    let table_ipc = select_materializer(Materialization::Ipc, &BlockKind::Table)
        .encode(&table_env)
        .unwrap();
    assert_eq!(
        table_ipc,
        IpcMaterializer.encode(&table_env).unwrap(),
        "an IPC layout default keeps ArrowTable on IPC"
    );
}

#[test]
fn p_io_7_1_parquet_materializer_rejects_non_table() {
    // 01 §1.11.3: parquet materialization is `ArrowTable` only.
    let blob_env = canonical_envelope(&Blob::new(vec![1, 2, 3]));
    let error = ParquetMaterializer.encode(&blob_env).unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Surface {
    data: ArrowTable,
    blob: Blob,
}

#[test]
fn p_io_7_1_verify_reports_identity_after_probe_dispatch() {
    // End to end on a Parquet-default library: the `ArrowTable` block is
    // written as enveloped Parquet (probed `PAR1`), the `Blob` stays native
    // (probed `ARROW1`); reopen drops the write-time default and probes each
    // object, and verify/identity recomputation is clean.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create_with_materialization(
        temp.path().join("library"),
        Materialization::Parquet,
    )
    .unwrap();
    let data = table(&[1, 2, 3]);
    let blob = Blob::new(vec![9, 8, 7]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&blob);
    let surface = Surface {
        data: data.clone(),
        blob: blob.clone(),
    };
    let tree_bytes = encode_node(&surface).unwrap();
    library
        .commit(&tree_bytes, &surface.leaf_refs(), &bucket)
        .unwrap();

    let root = temp.path().join("library");
    let blocks = std::fs::read_dir(root.join("tb-blocks")).unwrap().count();
    assert_eq!(blocks, 2);
    let native_addr = block_blob_address(&blob.envelope().encode());
    let native_path = root.join("tb-blocks").join(format!("{native_addr}.bin"));
    assert!(
        native_path.exists(),
        "a Blob under a parquet default still lands at its native IPC address"
    );
    let native_bytes = std::fs::read(&native_path).unwrap();
    assert_eq!(
        probe_block_materialization(&native_bytes).unwrap(),
        Materialization::Ipc
    );

    // Reopen with no parquet hint and verify through the probe-dispatched
    // decode; identity recomputation must match the committed RefIds.
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 2);
    let restored = reopened.bucket().unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(
        restored.get(data.ref_id()).unwrap().payload,
        data.envelope().payload,
        "a table restored from parquet carries the canonical envelope payload"
    );
    assert_eq!(
        restored.get(blob.ref_id()).unwrap().payload,
        blob.envelope().payload,
        "a Blob restores byte-identical native bytes"
    );
    let projected: Surface = reopened.project().unwrap();
    assert_eq!(encode_node(&projected).unwrap(), tree_bytes);
    assert_eq!(projected.data.ref_id(), data.ref_id());
}

#[test]
fn p_io_7_1_ipc_default_library_writes_identical_native_bytes() {
    // The default (IPC) write path is byte-identical to the pre-7.1 writes: a
    // Blob object is the canonical envelope frame at its golden-17 address and
    // verify stays clean after the decoupling.
    let temp = tempfile::tempdir().unwrap();
    let library = TbLibrary::create(temp.path().join("library")).unwrap();
    let blob = Blob::new(b"tb-sample".to_vec());
    let mut bucket = Bucket::new();
    bucket.put(&blob);
    let image = TreeImage::new(vec![named_field("blob", ImageContent::Ref(blob.ref_id()))]);
    let tree_bytes = encode(&image).unwrap();
    library
        .commit(
            &tree_bytes,
            &tree_space::layout::tb::image_leaf_refs(&image).unwrap(),
            &bucket,
        )
        .unwrap();
    let root = temp.path().join("library");
    let address = block_blob_address(&blob.envelope().encode());
    let obj_bytes = std::fs::read(root.join("tb-blocks").join(format!("{address}.bin"))).unwrap();
    assert_eq!(obj_bytes, blob.envelope().encode());
    assert_eq!(
        probe_block_materialization(&obj_bytes).unwrap(),
        Materialization::Ipc
    );
    let reopened = TbLibrary::open(&root).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
}
