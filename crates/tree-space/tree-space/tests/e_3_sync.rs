//! E-3: synchronization — pull (fetch → merge → whole-tree replace) and push
//! (proxy sync through the E-2 proxy semantics).
//!
//! Asserts the protocol `交换协议/02-施工路线图.md` §7.4 obligations for
//! `tests/e_3_sync.rs`: snapshot fetch (full / fragment-trimmed bucket),
//! block fetch with dangling-reference rejection, the pull roundtrips (full
//! tree, fragment scope, each merge op), the ownership write gate and the
//! Blocks-scope rejection, `build_push_request` field/byte fidelity, and the
//! push accept / reject paths.

use std::path::PathBuf;
use std::time::Duration;
use tree_space::block::RefId;
use tree_space::shared::SharedRegion;
use tree_space::xpath::XPath;
use tree_space::{
    Blob, Bucket, ErrorCode, Holder, ImageContent, ImageField, Locator, MergeOp, MergeSpec,
    ProxyClient, ProxyPolicy, ProxyReceipt, RejectReason, RuntimeEntry, RuntimeTable, Source,
    SyncScope, TbLibrary, TreeCodec, TreeImage, TreeNode, Value, build_push_request,
    canonical_xpath_bytes, encode, encode_node, fetch_blocks, fetch_snapshot, image_leaf_refs,
    named_field, project, publish_request, pull, push, reachable_refs, receive_request,
};

/// Simple named-fields pair (one block leaf + one inline scalar).
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct SyncSample {
    name: Value,
    block: Blob,
}

/// Three-level fixture for fragment scope: `/a/b/{p,q}`, `/a/c`, `/keep`.
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct SyncNodeB {
    p: Blob,
    q: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct SyncNodeA {
    b: SyncNodeB,
    c: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct SyncFrag {
    a: SyncNodeA,
    keep: Blob,
}

/// Commits `sample` into a fresh library under `root/library`, reopens it (so
/// the in-memory loaded state answers) and returns the handle.
fn commit_sample(root: &std::path::Path, sample: &SyncSample) -> TbLibrary {
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

fn sync_frag(p: &[u8], q: &[u8], c: &[u8], k: &[u8]) -> SyncFrag {
    SyncFrag {
        a: SyncNodeA {
            b: SyncNodeB {
                p: Blob::new(p.to_vec()),
                q: Blob::new(q.to_vec()),
            },
            c: Blob::new(c.to_vec()),
        },
        keep: Blob::new(k.to_vec()),
    }
}

fn commit_frag(root: &std::path::Path, sample: &SyncFrag) -> TbLibrary {
    let library = TbLibrary::create(root.join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&sample.a.b.p);
    bucket.put(&sample.a.b.q);
    bucket.put(&sample.a.c);
    bucket.put(&sample.keep);
    let tree_bytes = encode_node(sample).unwrap();
    library
        .commit(&tree_bytes, &sample.leaf_refs(), &bucket)
        .unwrap();
    drop(library);
    TbLibrary::open(root.join("library")).unwrap()
}

/// Commits a hand-built image into `root/library` and reopens it.
fn commit_image(root: &std::path::Path, children: Vec<ImageField>, bucket: &Bucket) -> TbLibrary {
    let library = TbLibrary::create(root.join("library")).unwrap();
    let image = TreeImage::new(children);
    let tree_bytes = encode(&image).unwrap();
    let leaf_refs = image_leaf_refs(&image).unwrap();
    library.commit(&tree_bytes, &leaf_refs, bucket).unwrap();
    drop(library);
    TbLibrary::open(root.join("library")).unwrap()
}

/// Whether two buckets hold the identical envelope set, keyed by `RefId`.
fn buckets_equivalent(left: &Bucket, right: &Bucket) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ids().all(|id| left.get(id) == right.get(id))
}

/// The named child of a node, when present.
fn named_child<'a>(children: &'a [ImageField], name: &str) -> Option<&'a ImageField> {
    children
        .iter()
        .find(|field| matches!(&field.locator, Locator::Named(n) if n == name))
}

#[test]
fn e_3_fetch_snapshot_full_tree() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("source".into()),
        block: Blob::new(vec![1, 2, 3]),
    };
    let source = commit_sample(temp.path(), &sample);
    let snapshot = fetch_snapshot(&source, &SyncScope::FullTree).unwrap();
    // Whole tree image.
    assert_eq!(&snapshot.image, source.tree_image().unwrap());
    // FullTree bucket = the source's whole bucket.
    assert!(buckets_equivalent(
        &snapshot.bucket,
        source.bucket().unwrap()
    ));
}

#[test]
fn e_3_fetch_snapshot_fragment_trims_bucket() {
    let temp = tempfile::tempdir().unwrap();
    let source = commit_frag(temp.path(), &sync_frag(b"p", b"q", b"c", b"k"));
    let scope = SyncScope::TreeFragment(vec![XPath::parse("/a/b").unwrap()]);
    let snapshot = fetch_snapshot(&source, &scope).unwrap();

    // The image is always whole (the tree is never splittable, 01 §1).
    assert_eq!(&snapshot.image, source.tree_image().unwrap());
    // The bucket is trimmed to the in-scope reachable set (02 §7.2).
    let expected = reachable_refs(source.tree_image().unwrap(), &scope).unwrap();
    assert_eq!(expected.len(), 2);
    assert_eq!(snapshot.bucket.len(), expected.len());
    for id in &expected {
        assert!(snapshot.bucket.get(*id).is_some());
    }
    // Out-of-scope leaves (/a/c, /keep) are not fetched.
    let prefix = canonical_xpath_bytes(&XPath::parse("/a/b").unwrap());
    for (xpath, id) in image_leaf_refs(source.tree_image().unwrap()).unwrap() {
        if !canonical_xpath_bytes(&xpath).starts_with(&prefix) {
            assert!(
                snapshot.bucket.get(id).is_none(),
                "out-of-scope leaf fetched: {xpath}"
            );
        }
    }
}

#[test]
fn e_3_fetch_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("source".into()),
        block: Blob::new(vec![9, 8, 7]),
    };
    let source = commit_sample(temp.path(), &sample);
    let id = source.bucket().unwrap().ids().next().unwrap();

    let fetched = fetch_blocks(&source, &[id]).unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched.get(id), source.bucket().unwrap().get(id));

    // An unknown id → dangling reference (no partial success).
    let unknown = RefId::from_bytes([0xee; 16]);
    let error = fetch_blocks(&source, &[id, unknown]).unwrap_err();
    assert_eq!(error.code, ErrorCode::DanglingReference);
}

#[test]
fn e_3_pull_full_tree_overwrite_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let src_sample = SyncSample {
        name: Value::Utf8("source".into()),
        block: Blob::new(vec![1, 2, 3]),
    };
    let dest_sample = SyncSample {
        name: Value::Utf8("dest".into()),
        block: Blob::new(vec![7, 7]),
    };
    let source = commit_sample(&temp.path().join("src"), &src_sample);
    let dest = commit_sample(&temp.path().join("dest"), &dest_sample);

    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::FullTree,
    };
    let receipt = pull(&source, &dest, &RuntimeTable::default(), &spec).unwrap();
    assert!(receipt.sequence > 0);

    // commit only persists — the observable effect is on re-open (02 §7.6).
    drop(source);
    drop(dest);
    let dest_re = TbLibrary::open(temp.path().join("dest/library")).unwrap();
    let src_re = TbLibrary::open(temp.path().join("src/library")).unwrap();
    // 抓取 + merge + 整棵替换 roundtrip: dest converges to source.
    assert_eq!(dest_re.tree_image().unwrap(), src_re.tree_image().unwrap());
    // The destination persisted the source's reachable blocks. The dest's own
    // former blocks remain orphaned on disk until GC (01 §6 / layout GC — a
    // commit never deletes), so the bucket is a superset, never a subset.
    for id in src_re.bucket().unwrap().ids() {
        assert!(
            dest_re.bucket().unwrap().get(id).is_some(),
            "source block {id} missing from the pulled destination"
        );
    }
    // The reopened destination materializes the very source sample.
    let projected: SyncSample =
        project(dest_re.tree_image().unwrap(), dest_re.bucket().unwrap()).unwrap();
    assert_eq!(projected, src_sample);
    assert!(dest_re.verify().unwrap() >= 1);
}

#[test]
fn e_3_pull_fragment_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let src_frag = sync_frag(b"sp", b"sq", b"sc", b"sk");
    let dest_frag = sync_frag(b"dp", b"dq", b"dc", b"dk");
    let source = commit_frag(&temp.path().join("src"), &src_frag);
    let dest = commit_frag(&temp.path().join("dest"), &dest_frag);

    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::TreeFragment(vec![XPath::parse("/a/b").unwrap()]),
    };
    pull(&source, &dest, &RuntimeTable::default(), &spec).unwrap();
    drop(source);
    drop(dest);

    let dest_re = TbLibrary::open(temp.path().join("dest/library")).unwrap();
    let projected: SyncFrag =
        project(dest_re.tree_image().unwrap(), dest_re.bucket().unwrap()).unwrap();
    // Only the in-scope /a/b subtree converged to the source; everything else
    // kept the destination's values.
    assert_eq!(projected.a.b.p, src_frag.a.b.p);
    assert_eq!(projected.a.b.q, src_frag.a.b.q);
    assert_eq!(projected.a.c, dest_frag.a.c);
    assert_eq!(projected.keep, dest_frag.keep);
    assert_eq!(dest_re.verify().unwrap(), 4);
}

#[test]
fn e_3_pull_merge_op_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let b1 = Blob::new(vec![1]);
    let b2 = Blob::new(vec![2]);

    // Source image: [a: Ref(b2), c: Inline(2)].
    let mut src_bucket = Bucket::new();
    let id2 = src_bucket.put(&b2);
    let source = commit_image(
        &temp.path().join("src"),
        vec![
            named_field("a", ImageContent::Ref(id2)),
            named_field("c", ImageContent::Inline(Value::I32(2))),
        ],
        &src_bucket,
    );

    // Per-op fresh destination image: [a: Ref(b1), b: Inline(1)].
    let mut dest_bucket = Bucket::new();
    let id1 = dest_bucket.put(&b1);
    for (op, expected_a, expect_b, expect_c) in [
        (MergeOp::AddOnly, id1, true, true),
        (MergeOp::ModifyOnly, id2, true, false),
        (MergeOp::DeleteOnly, id1, false, false),
    ] {
        let op_root = temp.path().join(format!("op-{op:?}"));
        let dest = commit_image(
            &op_root,
            vec![
                named_field("a", ImageContent::Ref(id1)),
                named_field("b", ImageContent::Inline(Value::I32(1))),
            ],
            &dest_bucket,
        );
        let spec = MergeSpec {
            op,
            scope: SyncScope::FullTree,
        };
        pull(&source, &dest, &RuntimeTable::default(), &spec).unwrap();
        drop(dest);

        let reopened = TbLibrary::open(op_root.join("library")).unwrap();
        let image = reopened.tree_image().unwrap();
        let leaves = image_leaf_refs(image).unwrap();
        // 02 §7.3 语义表 leaf-set changes per op.
        assert_eq!(leaves.len(), 1, "{op:?}");
        assert_eq!(leaves[0].1, expected_a, "{op:?}");
        // Inline fields: destination-only /b deletion and source-only /c
        // insertion follow the op's flags.
        assert_eq!(
            named_child(image.children(), "b").is_some(),
            expect_b,
            "{op:?}"
        );
        assert_eq!(
            named_child(image.children(), "c").is_some(),
            expect_c,
            "{op:?}"
        );
    }
}

#[test]
fn e_3_pull_requires_owner() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("src".into()),
        block: Blob::new(vec![1]),
    };
    let source = commit_sample(&temp.path().join("src"), &sample);
    let dest = commit_sample(&temp.path().join("dest"), &sample);

    // The destination table marks the tree as mapped from a peer → the write
    // gate refuses the pull before any fetch/merge (01 §3.1).
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    });
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::FullTree,
    };
    let error = pull(&source, &dest, &table, &spec).unwrap_err();
    assert_eq!(error.code, ErrorCode::OwnershipDenied);
}

#[test]
fn e_3_pull_blocks_scope_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("src".into()),
        block: Blob::new(vec![1]),
    };
    let source = commit_sample(&temp.path().join("src"), &sample);
    let dest = commit_sample(&temp.path().join("dest"), &sample);
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::Blocks(vec![RefId::from_bytes([0x01; 16])]),
    };
    let error = pull(&source, &dest, &RuntimeTable::default(), &spec).unwrap_err();
    assert_eq!(error.code, ErrorCode::PayloadMalformed);
}

#[test]
fn e_3_build_push_request_matches_library() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("push-me".into()),
        block: Blob::new(vec![3, 1, 4]),
    };
    let library = commit_sample(&temp.path().join("src"), &sample);
    let target = Source::Disk(PathBuf::from("owner-library"));
    let request = build_push_request(&library, 7, target.clone()).unwrap();

    assert_eq!(request.request_id, 7);
    assert_eq!(request.target, target);
    assert_eq!(
        request.tree_bytes,
        encode(library.tree_image().unwrap()).unwrap()
    );
    // leaf_refs equal the image's canonical leaf walk (order-insensitive).
    let expected = image_leaf_refs(library.tree_image().unwrap()).unwrap();
    let mut left = expected
        .iter()
        .map(|(xpath, id)| (canonical_xpath_bytes(xpath), *id))
        .collect::<Vec<_>>();
    left.sort();
    let mut right = request
        .leaf_refs
        .iter()
        .map(|(xpath, id)| (canonical_xpath_bytes(xpath), *id))
        .collect::<Vec<_>>();
    right.sort();
    assert_eq!(left, right);
    // The bucket mirrors the library's loaded bucket.
    assert!(buckets_equivalent(
        &request.bucket,
        library.bucket().unwrap()
    ));
}

#[test]
fn e_3_build_push_request_byte_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("wire".into()),
        block: Blob::new(vec![5, 6]),
    };
    let library = commit_sample(&temp.path().join("src"), &sample);
    let request = build_push_request(&library, 42, Source::Disk(PathBuf::from("owner"))).unwrap();

    // E-2 proxy wire framing roundtrips the built request byte-for-byte.
    let (region, handle) = publish_request(&request).unwrap();
    let decoded = receive_request(&SharedRegion::open(handle).unwrap()).unwrap();
    assert_eq!(decoded, request);
    drop(region);
}

#[test]
fn e_3_push_accepts_owned() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("push".into()),
        block: Blob::new(vec![8, 8, 8]),
    };
    let source = commit_sample(&temp.path().join("src"), &sample);
    // The owner starts as a fresh created library (no TB commit yet).
    let owner = TbLibrary::create(temp.path().join("owner")).unwrap();
    let request = build_push_request(&source, 99, Source::Disk(PathBuf::from("owner"))).unwrap();

    let client = ProxyClient::with_timeout(Duration::from_secs(1));
    let receipt = push(
        &client,
        &owner,
        &RuntimeTable::default(),
        ProxyPolicy::Allow,
        request,
    );
    assert_eq!(receipt, ProxyReceipt::Accepted { request_id: 99 });
    // The proxy write committed through the owner's write path (disk-visible).
    assert!(owner.verify().unwrap() >= 1);
    drop(source);
}

#[test]
fn e_3_push_rejects_not_owner_and_policy() {
    let temp = tempfile::tempdir().unwrap();
    let sample = SyncSample {
        name: Value::Utf8("push".into()),
        block: Blob::new(vec![2]),
    };
    let source = commit_sample(&temp.path().join("src"), &sample);
    let owner = TbLibrary::create(temp.path().join("owner")).unwrap();
    let client = ProxyClient::with_timeout(Duration::from_secs(1));

    // Mapped (non-owner) destination → Rejected(NotOwner) through the push entry.
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    });
    let request = build_push_request(&source, 100, Source::Disk(PathBuf::from("peer"))).unwrap();
    let receipt = push(&client, &owner, &table, ProxyPolicy::Allow, request);
    assert_eq!(
        receipt,
        ProxyReceipt::Rejected {
            request_id: 100,
            reason: RejectReason::NotOwner,
        }
    );

    // Own destination but the owner's policy refuses → Rejected(ProxyRefused).
    let request = build_push_request(&source, 101, Source::Disk(PathBuf::from("owner"))).unwrap();
    let receipt = push(
        &client,
        &owner,
        &RuntimeTable::default(),
        ProxyPolicy::Deny,
        request,
    );
    assert_eq!(
        receipt,
        ProxyReceipt::Rejected {
            request_id: 101,
            reason: RejectReason::ProxyRefused,
        }
    );
}
