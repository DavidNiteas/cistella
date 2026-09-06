//! TS-B1-2 B1-T3: composable registry lifecycle (`_dev/注册面注入/02-施工路线图.md`
//! §6.3 表 B1-T3 / 01 §3.4 / §4.1).
//!
//! The same `Arc<PluginRegistry>` flows through create and open — the two ends
//! of the library lifecycle share one registry surface, so the whole chain
//! (create → put/commit → open → verify → project) routes self-consistently.
//! Composability is the caller's orchestration: the library never
//! re-orchestrates (01 §3.4). Three orchestrations:
//!
//! - full built-in base (`PluginRegistry::new()`) shared across create/open —
//!   healthy chain end-to-end, `project` restores exactly the committed
//!   encoding;
//! - degraded orchestration (built-in tree plugin, no block plugin) — the
//!   restore path degrades both rows per the existing semantics on the
//!   injected surface, `verify` keeps counting them, and projection fails
//!   with `PluginMissing` exactly like the frozen degraded surface (01 §3.4);
//! - two-surface coexistence: a minimal test-local `RegisteredBlock` (named
//!   block, process-level `block::registry`, 02 §7-6 — no stage-1 bridge
//!   `indexed-table` dependency) composes with the injected registry: the
//!   named frame routes healthy end-to-end because the IPC plugin matches
//!   every kind including dynamically-named ones (01 §3.4 note, 02 §6.2).
//!
//! Key assertion (01 §3.4 / §4.1): `plugin_registry()` at both ends returns
//! the exact injected allocation (`Arc::ptr_eq`) — the same `Arc` routes the
//! whole chain.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::{
    Arrow55TreePlugin, ArrowTable, Blob, BlockCaps, BlockKind, Bucket, DiskVersion, ErrorCode,
    PluginRegistry, RegisteredBlock, TbLibrary, TreeCodec, TreeNode, encode_node, image_leaf_refs,
    register_block,
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

/// Commits the standard two-leaf fixture through `library`, returning the
/// committed tree's canonical bytes (the projection equivalence anchor).
fn commit_fixture(library: &TbLibrary) -> Vec<u8> {
    let (bucket, tree) = fixture();
    let tree_bytes = encode_node(&tree).unwrap();
    library
        .commit(&tree_bytes, &tree.leaf_refs(), &bucket)
        .unwrap();
    tree_bytes
}

/// An opaque named (registered) block, defined test-locally per 02 §7-6: the
/// two-surface coexistence sample never pulls the stage-1 bridge's
/// `indexed-table` dependency into this work order.
///
/// A named block enters the tree through the dynamic image path (a leaf
/// `ImageContent::Ref(ref_id)` under a `<name>` field — the `TbLibrary::commit`
/// input surface is tree-bytes + leaf refs, so the typed `TreeNode` derive is
/// not involved; its leaf-type whitelist covers the built-in kinds only).
#[derive(Clone, Debug, PartialEq, Eq)]
struct BenchV2(Vec<u8>);

impl RegisteredBlock for BenchV2 {
    const NAME: &'static str = "org.example.b1.t3.bench";
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

#[test]
fn t_s_b1_2_injected_lifecycle() {
    // --- Orchestration 1: full built-in base (`new()`) — the same `Arc`
    // flows through create and open, and the whole chain comes back healthy.
    {
        let registry = Arc::new(PluginRegistry::new());
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");

        // Create-side injection.
        let library = TbLibrary::create_with_registry(&root, Arc::clone(&registry)).unwrap();
        assert!(
            Arc::ptr_eq(library.plugin_registry(), &registry),
            "the create-side instance holds the injected allocation as its route surface"
        );

        // Bake the tree + blocks and commit (the write side stays on the
        // untouched `select_materializer` redline — same as B1-T1/T2).
        let committed_bytes = commit_fixture(&library);

        // Open-side injection with the same `Arc` — one registry across the
        // lifecycle ends (§3.4 completeness claim).
        let reopened = TbLibrary::open_with_registry(&root, Arc::clone(&registry)).unwrap();
        assert!(
            Arc::ptr_eq(reopened.plugin_registry(), &registry),
            "the open-side instance holds the exact same injected allocation"
        );

        // Whole-chain routing self-consistency: restore, verify, project.
        let restored = reopened.bucket().unwrap();
        assert_eq!(
            restored.degraded().len(),
            0,
            "no degradation on the full built-in surface"
        );
        assert_eq!(restored.len(), 2, "both leaves restore healthy");
        assert_eq!(
            reopened.verify().unwrap(),
            2,
            "verify counts both healthy rows"
        );
        let projected: Surface = reopened.project().unwrap();
        assert_eq!(
            encode_node(&projected).unwrap(),
            committed_bytes,
            "project restores exactly the bytes committed through the create side"
        );
    }

    // --- Orchestration 2: degraded composition (built-in tree plugin, no
    // block plugin) — the degraded-block path expresses on the injected
    // surface per the existing semantics, and verify keeps counting the rows
    // (classification/count-grade, 02 §7-5).
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

        // The same `Arc` reopens: the tree is parseable, the rows degrade.
        let reopened = TbLibrary::open_with_registry(&root, Arc::clone(&registry)).unwrap();
        assert!(
            Arc::ptr_eq(reopened.plugin_registry(), &registry),
            "the degraded orchestration also keeps one Arc across lifecycle ends"
        );

        let restored = reopened.bucket().unwrap();
        assert_eq!(
            restored.len(),
            0,
            "no healthy envelope slot without block plugins"
        );
        assert_eq!(
            restored.degraded().len(),
            2,
            "both rows degrade to the parallel slot under the composition"
        );
        let kinds = restored
            .degraded()
            .values()
            .map(|block| block.kind.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let expected = std::iter::once(BlockKind::Blob)
            .chain(std::iter::once(BlockKind::Table))
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

        // Projection on the degraded leaves fails per the frozen degraded
        // semantics (01 §3.4 / §4-5): `PluginMissing` with kind/disk context —
        // the first degraded leaf in canonical projection order reports
        // (`table` or `blob`; both rows degrade under this orchestration).
        let error = reopened.project::<Surface>().unwrap_err();
        assert_eq!(error.code, ErrorCode::PluginMissing);
        assert!(
            matches!(
                error.context.get("kind").map(String::as_str),
                Some("blob") | Some("table")
            ),
            "the degraded leaf reports its own kind: {:?}",
            error.context.get("kind")
        );
        assert_eq!(
            error.context.get("disk").map(String::as_str),
            Some("arrow-ipc")
        );
    }

    // --- Orchestration 3: two-surface coexistence — a test-local
    // `RegisteredBlock` (process-level `block::registry`) composes with the
    // injected registry surface (block plugins): the named frame routes
    // healthy end-to-end because the IPC plugin matches every kind including
    // dynamically-named ones (01 §3.4 note — the two surfaces are orthogonal,
    // never interchangeable; 02 §6.2).
    {
        register_block::<BenchV2>()
            .expect("the test-local named block registers into the process-level table");
        let registry = Arc::new(PluginRegistry::new());
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");

        let bench = BenchV2(b"B1T3-BENCH".to_vec());
        let blob = Blob::new(vec![0xbb]);
        let mut bucket = Bucket::new();
        let bench_id = bucket.put(&bench);
        let blob_id = bucket.put(&blob);

        // The dynamic image path: a `<name>` leaf carries `ImageContent::Ref`
        // (the typed derive's leaf whitelist covers built-in kinds only, so a
        // named block enters the tree through the image surface — the same
        // commit entry the typed trees use, `TbLibrary::commit`).
        let image = tree_space::tree::codec::TreeImage::new(vec![
            tree_space::tree::codec::named_field(
                "bench",
                tree_space::tree::codec::ImageContent::Ref(bench_id),
            ),
            tree_space::tree::codec::named_field(
                "blob",
                tree_space::tree::codec::ImageContent::Ref(blob_id),
            ),
        ]);
        let tree_bytes = tree_space::tree::codec::encode(&image).unwrap();
        let leaf_refs = image_leaf_refs(&image).unwrap();

        let library = TbLibrary::create_with_registry(&root, Arc::clone(&registry)).unwrap();
        assert!(Arc::ptr_eq(library.plugin_registry(), &registry));
        library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

        // Same `Arc` across the named-block lifecycle too.
        let reopened = TbLibrary::open_with_registry(&root, Arc::clone(&registry)).unwrap();
        assert!(
            Arc::ptr_eq(reopened.plugin_registry(), &registry),
            "one Arc across the named-block lifecycle too"
        );

        // The named frame restores healthy through the injected IPC plugin —
        // the process-level registration (block::registry) and the injected
        // registry (PluginRegistry) are orthogonal and both working.
        let restored = reopened.bucket().unwrap();
        assert_eq!(
            restored.degraded().len(),
            0,
            "the named frame routes healthy through the injected IPC plugin"
        );
        assert_eq!(restored.len(), 2, "both leaves restore healthy");
        let restored_kinds = restored
            .ids()
            .map(|id| restored.get(id).expect("restored id").kind.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            restored_kinds.contains(&BlockKind::Named("org.example.b1.t3.bench".into())),
            "the named leaf keeps its registered kind through the route"
        );
        assert!(
            restored_kinds.contains(&BlockKind::Blob),
            "the built-in leaf keeps its built-in kind through the route"
        );
        assert_eq!(
            reopened.verify().unwrap(),
            2,
            "named frames count into verify like any other healthy row"
        );

        // Read-side whole chain: the reopened tree decodes to the same leaf
        // ref set (tree bytes are content-addressed — equality of ref sets is
        // the read-side round trip proof).
        let decoded = tree_space::tree::codec::decode(&tree_bytes).unwrap();
        let decoded_refs = image_leaf_refs(&decoded).unwrap();
        let mut expected = leaf_refs
            .iter()
            .map(|(_, id)| id.as_bytes().to_owned())
            .collect::<Vec<_>>();
        let mut actual = decoded_refs
            .iter()
            .map(|(_, id)| id.as_bytes().to_owned())
            .collect::<Vec<_>>();
        expected.sort();
        actual.sort();
        assert_eq!(
            actual, expected,
            "the reopened tree decodes to the committed named/blob leaf set"
        );
    }
}
