//! PL-1 S2: plugin traits + arrow55 pluginization (`_dev/插件化改造/02-施工路线图.md`
//! §5.4).
//!
//! Five anchors:
//!
//! - `p_l_1_plugin_ipc_byte_identical`: `ArrowIpcBlockPlugin` encodes the
//!   golden-17 sample envelope to byte-identical IPC frames (zero-byte-change
//!   proof of the S2 hard boundary);
//! - `p_l_1_plugin_parquet_table_only`: the Parquet plugin refuses every
//!   non-`Table` kind with `SchemaMismatch` (P-IO-7.2 constraint kept);
//! - `p_l_1_plugin_pair_conflict`: a second plugin for the same `(MEMORY,
//!   DISK)` pair is a `TypeConflict`; an undeclared pair fails `validate_pair`
//!   with `SchemaMismatch`; the built-in pairs always pass;
//! - `p_l_1_plugin_route`: disk-version + kind routing through the registry
//!   (including the kind-constrained miss = degraded candidate);
//! - `p_l_1_tree_plugin_wraps_codec`: `Arrow55TreePlugin` equals the
//!   `tree::codec` encode/decode/tree_id paths on a sample tree.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::TreeImage;
use tree_space::{
    Arrow55TreePlugin, ArrowIpcBlockPlugin, ArrowParquetBlockPlugin, ArrowTable, Blob, Block,
    BlockKind, BlockPlugin, DiskLayout, ErrorCode, ImageContent, MemLayout, PluginRegistry, RefId,
    TreePlugin, Value, named_field, tree_blob_address,
};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Frozen golden-17 file bytes of the P-IO-2 sample Blob envelope
/// (`tests/p_io_2_blocks.rs` line 41).
const SAMPLE_ENVELOPE_HEX: &str = "040100f2020000000000004152524f573100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff780000001000000000000a000c000a00090004000a00000010000000000104000800080000000400080000000400000001000000140000001000140010000e000f0004000000080010000000180000000c000000000001041000000000000000040004000400000004000000626c6f62000000000000000000000000ffffffffb8000000100000000c001a0018001700040008000c00000020000000c000000000000000000000000000000304000a0018000c00080004000a0000002c0000001000000001000000000000000000000001000000010000000000000000000000000000000000000003000000000000000000000001000000000000004000000000000000080000000000000080000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000ff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000074622d73616d706c6500000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0000000014000000000000000c00140012000c00080004000c000000640000008000000010000000000004000800080000000400080000000400000001000000140000001000140010000e000f0004000000080010000000180000000c000000000001041000000000000000040004000400000004000000626c6f620000000001000000c000000000000000c000000000000000c0000000000000000000000000000000a00000004152524f5731";
/// Frozen golden-17 storage address of the sample blob (P-IO-2).
const SAMPLE_ADDRESS: &str = "d796cdd97c2ecac5dbeb3cc6ac9dc605";
/// Frozen sample-blob semantic identity (golden-17 anchor, `tb-block-blob`;
/// PL-2 M2 re-freeze to the semantic formula).
const SAMPLE_REF_ID: &str = "d647966165bab1e26a2cbec5ec794735";

/// The golden-17 sample blob (`b"tb-sample\0"`), byte-for-byte as in P-IO-2.
fn sample_blob() -> Blob {
    Blob::new(vec![
        0x74, 0x62, 0x2d, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x00,
    ])
}

fn sample_table(ids: &[i32]) -> ArrowTable {
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

#[test]
fn p_l_1_plugin_ipc_byte_identical() {
    // S2 hard boundary: `ArrowIpcBlockPlugin` encodes golden-17 sample frames
    // byte-identically and reproduces the frozen address/identity formulas.
    let blob = sample_blob();
    let envelope = blob.envelope();
    let plugin = ArrowIpcBlockPlugin;

    assert_eq!(plugin.memory(), MemLayout::Arrow55);
    assert_eq!(plugin.disk(), DiskLayout::ArrowIpc);

    let physical = plugin.encode(&envelope).unwrap();
    assert_eq!(
        hex(&physical),
        SAMPLE_ENVELOPE_HEX,
        "golden 17 IPC frame stays byte-identical through the plugin"
    );
    assert_eq!(plugin.address(&physical).as_bytes(), hex16(SAMPLE_ADDRESS));
    assert_eq!(
        plugin.ref_id(envelope.kind.clone(), &envelope.payload),
        RefId::from_bytes(hex16(SAMPLE_REF_ID))
    );
    assert_ne!(
        plugin.address(&physical).as_bytes(),
        hex16(SAMPLE_REF_ID),
        "identity stays orthogonal to addressing"
    );

    let decoded = plugin.decode(&physical).unwrap();
    assert_eq!(decoded.kind, envelope.kind);
    assert_eq!(decoded.payload, envelope.payload);
}

#[test]
fn p_l_1_plugin_parquet_table_only() {
    // P-IO-7.2 constraint: the Parquet plugin serves `ArrowTable` only.
    let plugin = ArrowParquetBlockPlugin;
    assert_eq!(plugin.memory(), MemLayout::Arrow55);
    assert_eq!(plugin.disk(), DiskLayout::ArrowParquet);
    assert!(plugin.matches(&BlockKind::Table));
    assert!(!plugin.matches(&BlockKind::Blob));

    let blob_env = Blob::new(vec![1, 2, 3]).envelope();
    let error = plugin.encode(&blob_env).unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaMismatch);
    let error = plugin.decode(&blob_env.encode()).unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaMismatch);

    // ArrowTable positive roundtrip: enveloped `PAR1` payload, canonical
    // envelope restored.
    let envelope = sample_table(&[1, 2, 3]).envelope();
    let physical = plugin.encode(&envelope).unwrap();
    assert_ne!(physical, envelope.encode());
    assert!(physical[11..].starts_with(b"PAR1"), "parquet payload magic");
    let decoded = plugin.decode(&physical).unwrap();
    assert_eq!(decoded.kind, BlockKind::Table);
    assert_eq!(decoded.payload, envelope.payload);
}

#[test]
fn p_l_1_plugin_pair_conflict() {
    let mut registry = PluginRegistry::new();

    // A second block plugin / tree plugin for the same (Arrow55, ArrowIpc)
    // pair is a TypeConflict.
    let error = registry
        .register(Arc::new(ArrowIpcBlockPlugin))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TypeConflict);
    let error = registry
        .register_tree(Arc::new(Arrow55TreePlugin))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TypeConflict);

    // An undeclared pair fails validate_pair; the built-in pairs pass.
    let empty = PluginRegistry::empty();
    let error = empty
        .validate_pair(MemLayout::Arrow55, DiskLayout::ArrowIpc)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaMismatch);

    assert!(
        registry
            .validate_pair(MemLayout::Arrow55, DiskLayout::ArrowIpc)
            .is_ok()
    );
    assert!(
        registry
            .validate_pair(MemLayout::Arrow55, DiskLayout::ArrowParquet)
            .is_ok()
    );
}

#[test]
fn p_l_1_plugin_route() {
    let registry = PluginRegistry::global();

    let ipc = registry.route_disk(DiskLayout::ArrowIpc, &BlockKind::Table);
    assert!(ipc.is_some(), "ArrowIpc routes Table to the IPC plugin");
    assert_eq!(ipc.expect("checked").disk(), DiskLayout::ArrowIpc);

    let parquet = registry.route_disk(DiskLayout::ArrowParquet, &BlockKind::Table);
    assert!(
        parquet.is_some(),
        "ArrowParquet routes Table to the Parquet plugin"
    );
    assert_eq!(parquet.expect("checked").disk(), DiskLayout::ArrowParquet);

    assert!(
        registry
            .route_disk(DiskLayout::ArrowParquet, &BlockKind::Blob)
            .is_none(),
        "kind constraint: a Blob under ArrowParquet is a degraded candidate"
    );
    assert!(
        registry
            .route_disk(DiskLayout::ArrowIpc, &BlockKind::Blob)
            .is_some(),
        "the IPC plugin keeps native kinds routable"
    );
    let named = BlockKind::Named(std::sync::Arc::<str>::from("custom"));
    let named_route = registry.route_disk(DiskLayout::ArrowIpc, &named);
    assert!(
        named_route.is_some(),
        "the IPC plugin is the universal envelope container: a named registered block must route to it"
    );
    assert_eq!(
        named_route.expect("checked").disk(),
        DiskLayout::ArrowIpc,
        "a named kind under ArrowIpc routes to the IPC plugin, not a degraded candidate"
    );

    let tree = registry.tree_plugin(DiskLayout::ArrowIpc);
    assert!(tree.is_some(), "the built-in tree pair routes ArrowIpc");
    assert!(registry.tree_plugin(DiskLayout::ArrowParquet).is_none());
}

#[test]
fn p_l_1_tree_plugin_wraps_codec() {
    let plugin = Arrow55TreePlugin;
    assert_eq!(plugin.memory(), MemLayout::Arrow55);
    assert_eq!(plugin.disk(), DiskLayout::ArrowIpc);

    let image = TreeImage::new(vec![named_field(
        "answer",
        ImageContent::Inline(Value::I32(42)),
    )]);
    let bytes = plugin.encode(&image).unwrap();
    assert_eq!(
        bytes,
        tree_space::tree::codec::encode(&image).unwrap(),
        "encode equals the tree::codec path"
    );
    assert_eq!(
        plugin.decode(&bytes).unwrap(),
        image,
        "decode roundtrips the same tree image"
    );
    assert_eq!(
        plugin.tree_id(&bytes).as_bytes(),
        tree_space::tree::codec::tree_id(&bytes).as_bytes(),
        "tree_id equals the tree::codec formula"
    );
    assert_eq!(
        plugin.address(&bytes),
        tree_blob_address(&bytes),
        "address is the immutable tb-tree-blob hash"
    );
}
