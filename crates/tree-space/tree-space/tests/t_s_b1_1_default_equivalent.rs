//! TS-B1-1 B1-T1: default construction equivalence (`_dev/注册面注入/02-施工路线图.md`
//! §5.3 表 B1-T1 / 01 §3.3).
//!
//! The default constructors (`create` / `open`) delegate to the injected ones
//! with `Arc::new(PluginRegistry::new())`, which is content-identical to the
//! former process-global surface. Anchors:
//!
//! - default `create` + `commit` produces the same receipt, and `verify` the
//!   same count, as `create_with_registry(root, PluginRegistry::new())`;
//! - default `open` restores the same bucket as
//!   `open_with_registry(root, PluginRegistry::new())`;
//! - `plugin_registry()` exposes the instance `Arc` as-is (`Arc::ptr_eq` for
//!   the injected one), and the default instance's registry behaves like a
//!   fresh `PluginRegistry::new()` on the built-in route surface.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::{
    ArrowTable, Blob, Bucket, DiskLayout, MemLayout, PluginRegistry, TbLibrary, TreeCodec,
    TreeNode, encode_node,
};

/// One-column `Int32` table block.
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

/// A two-leaf tree (one IPC table block + one blob block), with the bucket
/// holding the same blocks.
#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Surface {
    table: ArrowTable,
    blob: Blob,
}

fn fixture() -> (Bucket, Surface) {
    let table_block = table(&[1, 2]);
    let blob_block = Blob::new(vec![0xbb]);
    let mut bucket = Bucket::new();
    bucket.put(&table_block);
    bucket.put(&blob_block);
    (
        bucket,
        Surface {
            table: table_block,
            blob: blob_block,
        },
    )
}

#[test]
fn t_s_b1_1_default_equivalent() {
    // --- Create-side: default `create` ≡ injected `create_with_registry(new())`.
    let temp = tempfile::tempdir().unwrap();
    let default_root = temp.path().join("default");
    let injected_root = temp.path().join("injected");

    let default_lib = TbLibrary::create(&default_root).unwrap();
    let injected_lib =
        TbLibrary::create_with_registry(&injected_root, Arc::new(PluginRegistry::new())).unwrap();

    // Same input → same output: the receipt is content-addressed, so equal
    // receipts are the byte-equivalence proof of the whole commit path.
    let (bucket_a, tree_a) = fixture();
    let (bucket_b, tree_b) = fixture();
    let receipt_a = default_lib
        .commit(
            &encode_node(&tree_a).unwrap(),
            &tree_a.leaf_refs(),
            &bucket_a,
        )
        .unwrap();
    let receipt_b = injected_lib
        .commit(
            &encode_node(&tree_b).unwrap(),
            &tree_b.leaf_refs(),
            &bucket_b,
        )
        .unwrap();
    assert_eq!(
        receipt_a, receipt_b,
        "default and injected-with-new commits route identically (same receipt)"
    );
    assert_eq!(
        default_lib.verify().unwrap(),
        injected_lib.verify().unwrap(),
        "verify counts agree (2 healthy leaves)"
    );
    assert_eq!(default_lib.verify().unwrap(), 2);

    // --- Open-side: default `open` ≡ `open_with_registry(root, new())`.
    let default_reopened = TbLibrary::open(&default_root).unwrap();
    let injected_reopened =
        TbLibrary::open_with_registry(&default_root, Arc::new(PluginRegistry::new())).unwrap();
    assert_eq!(
        default_reopened.bucket().unwrap().ids().collect::<Vec<_>>(),
        injected_reopened
            .bucket()
            .unwrap()
            .ids()
            .collect::<Vec<_>>(),
        "open and open_with_registry(new()) restore the same bucket"
    );
    assert!(default_reopened.bucket().unwrap().degraded().is_empty());
    assert_eq!(
        default_reopened.verify().unwrap(),
        injected_reopened.verify().unwrap()
    );

    // --- `plugin_registry()`: the injected Arc is stored as-is; the default
    // instance behaves like a fresh `PluginRegistry::new()`.
    let registry = Arc::new(PluginRegistry::new());
    let arc_lib =
        TbLibrary::create_with_registry(temp.path().join("held"), Arc::clone(&registry)).unwrap();
    assert!(
        Arc::ptr_eq(arc_lib.plugin_registry(), &registry),
        "the injected Arc is the exact allocation the instance routes through"
    );

    let default_registry = default_lib.plugin_registry();
    assert!(
        default_registry
            .route_disk(DiskLayout::ArrowIpc, &tree_space::BlockKind::Blob)
            .is_some(),
        "default registry routes the built-in IPC path"
    );
    assert!(
        default_registry
            .route_disk(DiskLayout::ArrowParquet, &tree_space::BlockKind::Table)
            .is_some(),
        "default registry routes the built-in parquet path"
    );
    assert!(
        default_registry
            .route_disk(DiskLayout::ArrowParquet, &tree_space::BlockKind::Blob)
            .is_none(),
        "default registry keeps the kind-constrained parquet miss (degraded candidate)"
    );
    assert!(
        default_registry.tree_plugin(DiskLayout::ArrowIpc).is_some(),
        "default registry routes the boot tree plugin"
    );
    assert!(
        default_registry
            .converter(MemLayout::Arrow55, MemLayout::Arrow55)
            .is_some(),
        "default registry keeps the Arrow55 → Arrow55 identity converter"
    );
}
