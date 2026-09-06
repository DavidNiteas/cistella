//! TS-B1-1 B1-T2: the injected registry is effective (`_dev/注册面注入/02-施工路线图.md`
//! §5.3 表 B1-T2 / 01 §3.4).
//!
//! The write side stays on the untouched `select_materializer` surface
//! (redline), so the same fixture commits under any injected registry; the
//! read/verify side routes through the instance registry and reproduces the
//! existing bootstrap/degraded semantics on the injected surface:
//!
//! - `PluginRegistry::empty()` (no tree plugin) + boot touch →
//!   `BootstrapIncomplete` (the tree structure is unparseable without the
//!   boot tree plugin — same semantics, orchestratable registry);
//! - a registry with the tree plugin only (no block plugins) → every row
//!   degrades to [`DegradedBlock`] (classification/count-grade assertion) and
//!   `verify` still counts the rows (address-only branch);
//! - a partial registry (tree plugin + IPC block plugin) → the IPC-served
//!   blocks restore healthy: the partial composition is orchestratable.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::{
    Arrow55TreePlugin, ArrowIpcBlockPlugin, ArrowTable, Blob, Bucket, DiskVersion, ErrorCode,
    PluginRegistry, TbLibrary, TreeCodec, TreeNode, encode_node,
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

/// Commits the standard two-leaf fixture through `library`.
fn commit_fixture(library: &TbLibrary) {
    let (bucket, tree) = fixture();
    library
        .commit(&encode_node(&tree).unwrap(), &tree.leaf_refs(), &bucket)
        .unwrap();
}

#[test]
fn t_s_b1_2_injected_effective() {
    // --- Scenario 1: `empty()` registry — no tree plugin means the boot
    // chain cannot parse the tree structure:
    // BootstrapIncomplete on the injected surface (01 §3.4).
    {
        let registry = Arc::new(PluginRegistry::empty());
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let library = TbLibrary::create_with_registry(&root, Arc::clone(&registry)).unwrap();
        commit_fixture(&library);

        let error = match TbLibrary::open_with_registry(&root, registry) {
            Ok(_) => panic!("an empty-registry reopen must fail the boot chain"),
            Err(error) => error,
        };
        assert_eq!(
            error.code,
            ErrorCode::BootstrapIncomplete,
            "a registry without the boot tree plugin reproduces the existing bootstrap failure"
        );
    }

    // --- Scenario 2: tree plugin only (no block plugins) — every restored
    // row degrades to the parallel slot; verify keeps counting them
    // (classification/count-grade assertion, 02 §7-5).
    {
        let mut registry = PluginRegistry::empty();
        registry
            .register_tree(Arc::new(Arrow55TreePlugin))
            .expect("first tree plugin registration is accepted");
        let registry = Arc::new(registry);

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let library = TbLibrary::create_with_registry(&root, Arc::clone(&registry)).unwrap();
        commit_fixture(&library);

        let reopened = TbLibrary::open_with_registry(&root, registry).unwrap();
        let restored = reopened.bucket().unwrap();
        assert_eq!(
            restored.len(),
            0,
            "no healthy envelope slot without block plugins"
        );
        assert_eq!(
            restored.degraded().len(),
            2,
            "both rows degrade to the parallel slot (count-grade)"
        );
        let kinds = restored
            .degraded()
            .values()
            .map(|block| block.kind.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let expected = std::iter::once(tree_space::BlockKind::Blob)
            .chain(std::iter::once(tree_space::BlockKind::Table))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            kinds, expected,
            "degraded rows keep their frame-header kind classification"
        );
        for block in restored.degraded().values() {
            assert_eq!(
                block.disk,
                DiskVersion("arrow-ipc".to_owned()),
                "degraded rows keep the side-table disk label"
            );
        }
        assert_eq!(
            reopened.verify().unwrap(),
            2,
            "degraded rows still count into verify (address-only branch)"
        );
    }

    // --- Scenario 3: partial composition — tree plugin + IPC block plugin:
    // the IPC-served blocks restore healthy on the injected partial surface
    // (the composition is orchestratable, 01 §3.4).
    {
        let mut registry = PluginRegistry::empty();
        registry
            .register_tree(Arc::new(Arrow55TreePlugin))
            .expect("first tree plugin registration is accepted");
        registry
            .register(Arc::new(ArrowIpcBlockPlugin))
            .expect("first block plugin registration is accepted");
        let registry = Arc::new(registry);

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let library = TbLibrary::create_with_registry(&root, Arc::clone(&registry)).unwrap();
        commit_fixture(&library);

        let reopened = TbLibrary::open_with_registry(&root, registry).unwrap();
        let restored = reopened.bucket().unwrap();
        assert_eq!(
            restored.degraded().len(),
            0,
            "the registered IPC block plugin serves the fixture rows — no degradation"
        );
        assert_eq!(
            restored.len(),
            2,
            "both leaves restore healthy on the partial surface"
        );
        assert_eq!(
            reopened.verify().unwrap(),
            2,
            "verify counts both healthy rows"
        );
    }
}
