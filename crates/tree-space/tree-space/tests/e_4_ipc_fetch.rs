//! E-4: cross-process IPC fetch (协议 `交换协议/02-施工路线图.md` §8.4) — the
//! 01 §5.2 grab over a shared snapshot region (same-process model, 02 §8.6).
//!
//! Asserts the §8.4 obligations for `tests/e_4_ipc_fetch.rs`: the
//! `publish_snapshot → SharedRegion::open → receive_snapshot` wire roundtrip,
//! `fetch_snapshot_ipc` (full tree + fragment-trimmed bucket), the block-level
//! `fetch_blocks_ipc` with dangling-reference rejection, and the lazy
//! `LazyIpcSnapshot` (tree up front, per-block zero-copy slices + on-demand
//! decode + cache).

use tree_space::block::RefId;
use tree_space::shared::SharedRegion;
use tree_space::tree::codec::encode_node;
use tree_space::xpath::XPath;
use tree_space::{
    Blob, Bucket, ErrorCode, IpcSource, LazyIpcSnapshot, OwnedSnapshot, Source, SyncScope,
    TbLibrary, TreeCodec, TreeNode, Value, fetch_blocks_ipc, fetch_snapshot_ipc, publish_snapshot,
    reachable_refs, receive_snapshot,
};

/// A small typed tree with one block leaf (full-tree fixtures).
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct IpcSample {
    name: Value,
    block: Blob,
}

/// Three-level fixture for fragment scope: `/a/b/{p,q}`, `/a/c`, `/keep`.
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct IpcFragB {
    p: Blob,
    q: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct IpcFragA {
    b: IpcFragB,
    c: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct IpcFrag {
    a: IpcFragA,
    keep: Blob,
}

/// Commits `sample` into a fresh library under `root/library` and reopens it.
fn commit_sample(root: &std::path::Path, sample: &IpcSample) -> TbLibrary {
    let library = TbLibrary::create(root.join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&sample.block);
    let tree_bytes = encode_node(sample).unwrap();
    library
        .commit(&tree_bytes, &sample.leaf_refs(), &bucket)
        .unwrap();
    drop(library);
    TbLibrary::open(root.join("library")).unwrap()
}

/// Publishes the library's full snapshot, returning the carrier region (kept
/// alive to model the publishing peer) plus its IPC source.
fn publish_library(library: &TbLibrary) -> (SharedRegion, Source) {
    let snapshot = OwnedSnapshot::read_full(library).unwrap();
    let (region, handle) = publish_snapshot(&snapshot).unwrap();
    (region, Source::Ipc(IpcSource::new(handle)))
}

#[test]
fn e_4_publish_receive_snapshot_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let sample = IpcSample {
        name: Value::Utf8("wire".into()),
        block: Blob::new(vec![9, 8, 7]),
    };
    let library = commit_sample(temp.path(), &sample);
    let snapshot = OwnedSnapshot::read_full(&library).unwrap();

    // publish_snapshot → export → open → receive_snapshot: the image and the
    // bucket come back equal to the source snapshot.
    let (region, handle) = publish_snapshot(&snapshot).unwrap();
    let opened = SharedRegion::open(handle).unwrap();
    let decoded = receive_snapshot(&opened).unwrap();
    assert_eq!(decoded, snapshot);
    drop(opened);
    drop(region);
}

#[test]
fn e_4_fetch_snapshot_ipc_full_tree() {
    let temp = tempfile::tempdir().unwrap();
    let sample = IpcSample {
        name: Value::Utf8("full".into()),
        block: Blob::new(vec![1, 2, 3]),
    };
    let library = commit_sample(temp.path(), &sample);
    let expected = OwnedSnapshot::read_full(&library).unwrap();
    let (region, source) = publish_library(&library);

    let fetched = fetch_snapshot_ipc(source.clone(), &SyncScope::FullTree).unwrap();
    assert_eq!(fetched, expected);
    drop(region);
}

#[test]
fn e_4_fetch_snapshot_ipc_fragment_trims() {
    let temp = tempfile::tempdir().unwrap();
    let sample = IpcFrag {
        a: IpcFragA {
            b: IpcFragB {
                p: Blob::new(b"p".to_vec()),
                q: Blob::new(b"q".to_vec()),
            },
            c: Blob::new(b"c".to_vec()),
        },
        keep: Blob::new(b"k".to_vec()),
    };
    let library = {
        let created = TbLibrary::create(temp.path().join("library")).unwrap();
        let mut bucket = Bucket::new();
        bucket.put(&sample.a.b.p);
        bucket.put(&sample.a.b.q);
        bucket.put(&sample.a.c);
        bucket.put(&sample.keep);
        let tree_bytes = encode_node(&sample).unwrap();
        created
            .commit(&tree_bytes, &sample.leaf_refs(), &bucket)
            .unwrap();
        drop(created);
        TbLibrary::open(temp.path().join("library")).unwrap()
    };
    let (region, source) = publish_library(&library);

    let scope = SyncScope::TreeFragment(vec![XPath::parse("/a/b").unwrap()]);
    let fetched = fetch_snapshot_ipc(source, &scope).unwrap();
    // The image is always whole (the tree is never splittable, 01 §1).
    assert_eq!(&fetched.image, library.tree_image().unwrap());
    // The bucket is trimmed to the in-scope reachable set (reachable_refs).
    let reachable = reachable_refs(library.tree_image().unwrap(), &scope).unwrap();
    assert_eq!(reachable.len(), 2);
    assert_eq!(fetched.bucket.len(), reachable.len());
    for id in &reachable {
        assert!(
            fetched.bucket.get(*id).is_some(),
            "in-scope leaf {id} missing from the fetched bucket"
        );
    }
    drop(region);
}

#[test]
fn e_4_fetch_blocks_ipc() {
    let temp = tempfile::tempdir().unwrap();
    let sample = IpcSample {
        name: Value::Utf8("blocks".into()),
        block: Blob::new(vec![5, 6, 7]),
    };
    let library = commit_sample(temp.path(), &sample);
    let id = library.bucket().unwrap().ids().next().unwrap();
    let (region, source) = publish_library(&library);

    let fetched = fetch_blocks_ipc(source.clone(), &[id]).unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched.get(id), library.bucket().unwrap().get(id));

    // Unknown id → dangling reference (no partial success).
    let unknown = RefId::from_bytes([0xee; 16]);
    assert_eq!(
        fetch_blocks_ipc(source.clone(), &[unknown])
            .unwrap_err()
            .code,
        ErrorCode::DanglingReference
    );
    drop(region);
}

#[test]
fn e_4_lazy_ipc_defers_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let sample = IpcSample {
        name: Value::Utf8("lazy".into()),
        block: Blob::new(vec![4, 4, 4]),
    };
    let library = commit_sample(temp.path(), &sample);
    let id = library.bucket().unwrap().ids().next().unwrap();
    let envelope = library.bucket().unwrap().get(id).unwrap().clone();
    let (region, source) = publish_library(&library);

    let lazy = LazyIpcSnapshot::new(source).unwrap();
    // Tree up front, no block decoded yet.
    assert_eq!(lazy.image(), library.tree_image().unwrap());
    assert!(!lazy.is_materialized(id));
    // Zero-copy slice == the envelope frame bytes.
    assert_eq!(lazy.block_bytes(id).unwrap(), envelope.encode().as_slice());
    // On-demand decode + cache.
    assert_eq!(lazy.block(id).unwrap(), envelope);
    assert!(lazy.is_materialized(id));
    assert_eq!(lazy.block(id).unwrap(), envelope, "cache hit");

    // Unknown id → dangling reference.
    let unknown = RefId::from_bytes([0xff; 16]);
    assert_eq!(
        lazy.block_bytes(unknown).unwrap_err().code,
        ErrorCode::DanglingReference
    );
    drop(region);
}
