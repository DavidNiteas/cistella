//! PL-1 S3 boot block + tb-boot channel (02 §5.5).
//!
//! Covers the boot write/reopen roundtrip across both disk layouts, the
//! negative surface (missing boot, unsupported boot version, unknown tree disk
//! version → `BootstrapIncomplete`) and the frozen boot schema bytes.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::convert::IpcSchemaEncoder;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tempfile::tempdir;
use tree_space::layout::flat_dir::FlatDirLayout;
use tree_space::layout::single_file::SingleFileLayout;
use tree_space::plugin::boot::{BOOT_VERSION, BootRecord, boot_schema, decode_boot, encode_boot};
use tree_space::{
    ArrowTable, Blob, Bucket, ErrorCode, SingleFileTbLibrary, TbLayout, TbLibrary, TreeCodec,
    TreeNode, encode_node,
};

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

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Surface {
    data: ArrowTable,
    blob: Blob,
}

#[test]
fn p_l_1_boot_create_write_reopen() {
    // FlatDir: `create` writes `<root>/tb-boot/boot.ipc`; a fresh layout reads
    // the same record back through `read_boot` + `decode_boot`.
    let temp = tempdir().unwrap();
    let root = temp.path().join("flat");
    TbLibrary::create(&root).unwrap();
    assert!(root.join("tb-boot").join("boot.ipc").exists());
    let layout = FlatDirLayout::new(&root);
    let flat = decode_boot(&layout.read_boot().unwrap()).unwrap();
    assert_eq!(flat.boot_version, BOOT_VERSION);
    assert_eq!(flat.library_id, "tree-space");
    assert_eq!(flat.tree_mem_version, "arrow55");
    assert_eq!(flat.tree_disk_version, "arrow-ipc");

    // SingleFile: the boot object rides the stage/epoch pipeline, so it becomes
    // readable once the first commit flushes the pending batch (the container
    // has no side channel; this stage does not go through `TbLibrary::open`).
    let temp = tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let library = TbLibrary::<SingleFileLayout>::create(&path).unwrap();
    let data = sample_table(&[7]);
    let blob = Blob::new(vec![0xbb]);
    let mut bucket = Bucket::new();
    bucket.put(&data);
    bucket.put(&blob);
    let tree = Surface {
        data: data.clone(),
        blob: blob.clone(),
    };
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();
    let layout = SingleFileLayout::new(&path);
    let single = decode_boot(&layout.read_boot().unwrap()).unwrap();
    assert_eq!(
        single, flat,
        "single-file boot record equals the flat-dir one"
    );
}

#[test]
fn p_l_1_boot_bad_or_missing() {
    // Missing boot file → `BootstrapIncomplete` from `read_boot`.
    let temp = tempdir().unwrap();
    let root = temp.path().join("flat");
    TbLibrary::create(&root).unwrap();
    let layout = FlatDirLayout::new(&root);
    std::fs::remove_file(root.join("tb-boot").join("boot.ipc")).unwrap();
    let missing = layout.read_boot().unwrap_err();
    assert_eq!(missing.code, ErrorCode::BootstrapIncomplete);

    // Tampered boot_version → `BootstrapIncomplete` from `decode_boot`.
    let bad_version = BootRecord {
        boot_version: BOOT_VERSION - 1,
        library_id: "tree-space".to_owned(),
        tree_mem_version: "arrow55".to_owned(),
        tree_disk_version: "arrow-ipc".to_owned(),
    };
    let unsupported_version = decode_boot(&encode_boot(&bad_version).unwrap()).unwrap_err();
    assert_eq!(unsupported_version.code, ErrorCode::BootstrapIncomplete);

    // Unknown tree disk version → `BootstrapIncomplete` from `decode_boot`.
    let unknown_disk = BootRecord {
        boot_version: BOOT_VERSION,
        library_id: "tree-space".to_owned(),
        tree_mem_version: "arrow55".to_owned(),
        tree_disk_version: "nope".to_owned(),
    };
    let bad_disk = decode_boot(&encode_boot(&unknown_disk).unwrap()).unwrap_err();
    assert_eq!(bad_disk.code, ErrorCode::BootstrapIncomplete);

    // End to end: tampered bytes written back into the flat boot channel make
    // `read_boot` + `decode_boot` fail with `BootstrapIncomplete`.
    std::fs::write(
        root.join("tb-boot").join("boot.ipc"),
        encode_boot(&unknown_disk).unwrap(),
    )
    .unwrap();
    let bytes = layout.read_boot().unwrap();
    let e2e = decode_boot(&bytes).unwrap_err();
    assert_eq!(e2e.code, ErrorCode::BootstrapIncomplete);

    // End to end: structurally broken bytes (not Arrow IPC at all) written back
    // into the boot channel → `BootstrapIncomplete` from `decode_boot` (no
    // `PayloadMalformed` passthrough; 01 §4-3 boot-broken hard failure).
    std::fs::write(
        root.join("tb-boot").join("boot.ipc"),
        b"this is not an arrow ipc file".to_vec(),
    )
    .unwrap();
    let bytes = layout.read_boot().unwrap();
    let broken = decode_boot(&bytes).unwrap_err();
    assert_eq!(broken.code, ErrorCode::BootstrapIncomplete);

    // SingleFile: `read_boot` on a container without a boot object
    // (a never-created file) → `BootstrapIncomplete`.
    let temp = tempdir().unwrap();
    let path = temp.path().join("never-created.umdb");
    let layout = SingleFileLayout::new(&path);
    let missing_sf = layout.read_boot().unwrap_err();
    assert_eq!(missing_sf.code, ErrorCode::BootstrapIncomplete);
}

#[test]
fn p_l_1_boot_schema_frozen() {
    // The boot layout is frozen (01 §4-3 / 02 §4.4): any change to the
    // four-column schema shows up as a changed IPC schema byte sequence.
    let bytes = IpcSchemaEncoder::new()
        .schema_to_fb(&boot_schema())
        .finished_data()
        .to_vec();
    assert_eq!(
        hex(&bytes),
        "0c0000000800080000000400080000000400000004000000c8000000840000003c0000000400000098ffffff140000000c000000000000050c0000000000000088ffffff11000000747265655f6469736b5f76657273696f6e000000ccffffff140000000c000000000000050c00000000000000bcffffff10000000747265655f6d656d5f76657273696f6e0000000010001400100000000f0004000000080010000000180000000c00000000000005100000000000000004000400040000000a0000006c6962726172795f6964000010001600100000000f0004000000080010000000180000001c000000000000021800000000000600080004000600000010000000000000000c000000626f6f745f76657273696f6e00000000"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
