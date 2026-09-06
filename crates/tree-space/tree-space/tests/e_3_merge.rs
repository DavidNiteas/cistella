//! E-3: merge primitives (覆盖/只添加/只修改/只删除/任意组合) + scope (全树/树片段/
//! 桶块) + reachable-block enumeration.
//!
//! Asserts the protocol `交换协议/02-施工路线图.md` §7.4 obligations for
//! `tests/e_3_merge.rs`: op flag masks, the overwrite convergence, the three
//! primitive behaviors and their combination over the 02 §7.3 语义表, the
//! xpath-fragment scope (in-scope-only edits, everything else preserved), the
//! atomic positional-container rule, and `reachable_refs` for all three scopes.

use std::collections::BTreeSet;
use tree_space::block::RefId;
use tree_space::xpath::XPath;
use tree_space::{
    Blob, Bucket, EncodeTree, ImageContent, MergeFlags, MergeOp, MergeSpec, SyncScope, TreeCodec,
    TreeImage, TreeNode, Value, canonical_xpath_bytes, encode, image_leaf_refs, merge_image,
    named_field, project, reachable_refs,
};

/// Three-level named-node fixture: `/a/b/{p,q}`, `/a/c`, `/keep`, inline `/name`.
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct NodeB {
    p: Blob,
    q: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct NodeA {
    b: NodeB,
    c: Blob,
}

#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct FragSample {
    a: NodeA,
    keep: Blob,
    name: Value,
}

/// Positional-container fixture: `/spectra` is a `Vec<Blob>` (chunk group).
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct SpecSample {
    spectra: Vec<Blob>,
    note: Value,
}

fn blob(bytes: &[u8]) -> Blob {
    Blob::new(bytes.to_vec())
}

fn frag_sample(px: &[u8], qx: &[u8], cx: &[u8], kx: &[u8], name: &str) -> FragSample {
    FragSample {
        a: NodeA {
            b: NodeB {
                p: blob(px),
                q: blob(qx),
            },
            c: blob(cx),
        },
        keep: blob(kx),
        name: Value::Utf8(name.into()),
    }
}

fn frag_image(sample: &FragSample) -> TreeImage {
    TreeImage::new(sample.tree_children().unwrap())
}

fn frag_bucket(sample: &FragSample) -> Bucket {
    let mut bucket = Bucket::new();
    bucket.put(&sample.a.b.p);
    bucket.put(&sample.a.b.q);
    bucket.put(&sample.a.c);
    bucket.put(&sample.keep);
    bucket
}

fn spec_sample(spectra: &[&[u8]], note: &str) -> SpecSample {
    SpecSample {
        spectra: spectra.iter().map(|bytes| blob(bytes)).collect(),
        note: Value::Utf8(note.into()),
    }
}

fn spec_image(sample: &SpecSample) -> TreeImage {
    TreeImage::new(sample.tree_children().unwrap())
}

fn spec_bucket(sample: &SpecSample) -> Bucket {
    let mut bucket = Bucket::new();
    for spectrum in &sample.spectra {
        bucket.put(spectrum);
    }
    bucket
}

/// The union of two buckets (envelope dedup by content identity).
fn bucket_union(left: &Bucket, right: &Bucket) -> Bucket {
    let mut out = left.clone();
    for id in right.ids() {
        out.put_envelope(right.get(id).unwrap().clone()).unwrap();
    }
    out
}

/// `[a: Ref, b: Inline]` style dest/source pair shared by the op-mask tests.
fn ab_dest_image() -> TreeImage {
    TreeImage::new(vec![
        named_field("a", ImageContent::Ref(RefId::from_bytes([0x01; 16]))),
        named_field("b", ImageContent::Inline(Value::I32(1))),
    ])
}

fn ac_source_image() -> TreeImage {
    TreeImage::new(vec![
        named_field("a", ImageContent::Ref(RefId::from_bytes([0x02; 16]))),
        named_field("c", ImageContent::Inline(Value::Utf8("src".into()))),
    ])
}

fn full_tree(op: MergeOp) -> MergeSpec {
    MergeSpec {
        op,
        scope: SyncScope::FullTree,
    }
}

#[test]
fn e_3_merge_op_flags() {
    let overwrite = MergeOp::Overwrite;
    assert!(overwrite.adds() && overwrite.modifies() && overwrite.deletes());

    let add_only = MergeOp::AddOnly;
    assert!(add_only.adds() && !add_only.modifies() && !add_only.deletes());

    let modify_only = MergeOp::ModifyOnly;
    assert!(!modify_only.adds() && modify_only.modifies() && !modify_only.deletes());

    let delete_only = MergeOp::DeleteOnly;
    assert!(!delete_only.adds() && !delete_only.modifies() && delete_only.deletes());

    // Combine passes the bitmask through verbatim.
    let combined = MergeOp::Combine(MergeFlags {
        add: true,
        modify: false,
        delete: true,
    });
    assert!(combined.adds() && !combined.modifies() && combined.deletes());

    let none = MergeOp::Combine(MergeFlags::default());
    assert!(!none.adds() && !none.modifies() && !none.deletes());
}

#[test]
fn e_3_merge_overwrite_full_tree_converges() {
    // Full-tree overwrite = simplest merge (复写): the result converges to the
    // source image, dest-only /b is deleted, source-only /c inserted.
    let spec = full_tree(MergeOp::Overwrite);
    let merged = merge_image(&ab_dest_image(), &ac_source_image(), &spec).unwrap();
    assert_eq!(merged, ac_source_image());
    assert_eq!(
        encode(&merged).unwrap(),
        encode(&ac_source_image()).unwrap()
    );
}

#[test]
fn e_3_merge_add_only() {
    let spec = full_tree(MergeOp::AddOnly);
    let merged = merge_image(&ab_dest_image(), &ac_source_image(), &spec).unwrap();
    // Common /a keeps the dest value; dest-only /b is kept; source-only /c inserted.
    let expected = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(RefId::from_bytes([0x01; 16]))),
        named_field("b", ImageContent::Inline(Value::I32(1))),
        named_field("c", ImageContent::Inline(Value::Utf8("src".into()))),
    ]);
    assert_eq!(merged, expected);
    assert_eq!(encode(&merged).unwrap(), encode(&expected).unwrap());
}

#[test]
fn e_3_merge_modify_only() {
    let spec = full_tree(MergeOp::ModifyOnly);
    let merged = merge_image(&ab_dest_image(), &ac_source_image(), &spec).unwrap();
    // Common /a (Ref) takes the source value; dest-only /b kept; source-only /c skipped.
    let expected = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(RefId::from_bytes([0x02; 16]))),
        named_field("b", ImageContent::Inline(Value::I32(1))),
    ]);
    assert_eq!(merged, expected);
}

#[test]
fn e_3_merge_delete_only() {
    let spec = full_tree(MergeOp::DeleteOnly);
    let merged = merge_image(&ab_dest_image(), &ac_source_image(), &spec).unwrap();
    // Common /a keeps dest; dest-only /b deleted; source-only /c skipped.
    let expected = TreeImage::new(vec![named_field(
        "a",
        ImageContent::Ref(RefId::from_bytes([0x01; 16])),
    )]);
    assert_eq!(merged, expected);
}

#[test]
fn e_3_merge_combine_add_modify() {
    let spec = full_tree(MergeOp::Combine(MergeFlags {
        add: true,
        modify: true,
        delete: false,
    }));
    let merged = merge_image(&ab_dest_image(), &ac_source_image(), &spec).unwrap();
    // add+modify: /a takes source, /b retained (delete off), /c inserted.
    let expected = TreeImage::new(vec![
        named_field("a", ImageContent::Ref(RefId::from_bytes([0x02; 16]))),
        named_field("b", ImageContent::Inline(Value::I32(1))),
        named_field("c", ImageContent::Inline(Value::Utf8("src".into()))),
    ]);
    assert_eq!(merged, expected);
}

#[test]
fn e_3_merge_scope_fragment_only_touches_in_scope() {
    let dest = frag_sample(b"dp", b"dq", b"dc", b"dk", "dest-sample");
    let source = frag_sample(b"sp", b"sq", b"sc", b"sk", "source-sample");
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::TreeFragment(vec![XPath::parse("/a/b").unwrap()]),
    };
    let merged = merge_image(&frag_image(&dest), &frag_image(&source), &spec).unwrap();
    let union = bucket_union(&frag_bucket(&dest), &frag_bucket(&source));
    let projected: FragSample = project(&merged, &union).unwrap();

    // Only the /a/b subtree converges to the source; /a/c, /keep and /name stay
    // byte-identical to the destination.
    assert_eq!(projected.a.b.p, source.a.b.p);
    assert_eq!(projected.a.b.q, source.a.b.q);
    assert_eq!(projected.a.c, dest.a.c);
    assert_eq!(projected.keep, dest.keep);
    assert_eq!(projected.name, dest.name);
    assert_ne!(projected.a.b.p, dest.a.b.p);

    // A prefix at an ancestor position still recurses into deeper nodes:
    // prefix `/a` scopes /a/b/p, /a/b/q and /a/c (descendants), while /keep
    // and /name stay out of scope.
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::TreeFragment(vec![XPath::parse("/a").unwrap()]),
    };
    let merged = merge_image(&frag_image(&dest), &frag_image(&source), &spec).unwrap();
    let projected: FragSample = project(&merged, &union).unwrap();
    assert_eq!(projected.a.b.p, source.a.b.p);
    assert_eq!(projected.a.b.q, source.a.b.q);
    assert_eq!(projected.a.c, source.a.c);
    assert_eq!(projected.keep, dest.keep);
    assert_eq!(projected.name, dest.name);
}

#[test]
fn e_3_merge_scope_out_of_scope_preserves_dest() {
    // An empty prefix list is the empty scope: nothing is in scope, so the
    // merged tree equals the destination even under Overwrite.
    let dest = frag_image(&frag_sample(b"dp", b"dq", b"dc", b"dk", "dest"));
    let source = frag_image(&frag_sample(b"sp", b"sq", b"sc", b"sk", "source"));
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::TreeFragment(Vec::new()),
    };
    let merged = merge_image(&dest, &source, &spec).unwrap();
    assert_eq!(merged, dest);
    assert_eq!(encode(&merged).unwrap(), encode(&dest).unwrap());
}

#[test]
fn e_3_merge_blocks_scope_is_noop() {
    // Bucket-level scope has no tree merge (02 §7.3): the image is returned
    // unchanged whatever the op.
    let dest = frag_image(&frag_sample(b"dp", b"dq", b"dc", b"dk", "dest"));
    let source = frag_image(&frag_sample(b"sp", b"sq", b"sc", b"sk", "source"));
    let spec = MergeSpec {
        op: MergeOp::Overwrite,
        scope: SyncScope::Blocks(vec![RefId::from_bytes([0x01; 16])]),
    };
    let merged = merge_image(&dest, &source, &spec).unwrap();
    assert_eq!(merged, dest);
    assert_eq!(encode(&merged).unwrap(), encode(&dest).unwrap());
}

#[test]
fn e_3_merge_positional_container_atomic() {
    // `Vec<Blob>` (chunk group) is an atomic field: a different entry count is
    // NOT merged per entry; the whole container subtree is replaced (02 §7.6).
    let dest = spec_sample(&[b"d1", b"d2"], "dest");
    let source = spec_sample(&[b"s1"], "source");
    let spec = full_tree(MergeOp::Overwrite);
    let merged = merge_image(&spec_image(&dest), &spec_image(&source), &spec).unwrap();
    let union = bucket_union(&spec_bucket(&dest), &spec_bucket(&source));
    let projected: SpecSample = project(&merged, &union).unwrap();
    assert_eq!(projected.spectra, source.spectra);
    assert_ne!(projected.spectra, dest.spectra);
    assert_eq!(projected.note, source.note);
}

#[test]
fn e_3_reachable_refs_full_tree() {
    let sample = frag_sample(b"p", b"q", b"c", b"k", "sample");
    let image = frag_image(&sample);
    let all = image_leaf_refs(&image)
        .unwrap()
        .into_iter()
        .map(|(_, id)| id)
        .collect::<BTreeSet<_>>();
    let reachable = reachable_refs(&image, &SyncScope::FullTree).unwrap();
    assert_eq!(reachable, all);
    assert_eq!(reachable.len(), 4);
}

#[test]
fn e_3_reachable_refs_fragment() {
    let sample = frag_sample(b"p", b"q", b"c", b"k", "sample");
    let image = frag_image(&sample);
    let prefix = XPath::parse("/a/b").unwrap();
    let prefix_bytes = canonical_xpath_bytes(&prefix);
    let expected = image_leaf_refs(&image)
        .unwrap()
        .into_iter()
        .filter(|(xpath, _)| canonical_xpath_bytes(xpath).starts_with(&prefix_bytes))
        .map(|(_, id)| id)
        .collect::<BTreeSet<_>>();
    let reachable = reachable_refs(&image, &SyncScope::TreeFragment(vec![prefix])).unwrap();
    assert_eq!(reachable, expected);
    // /a/b covers exactly the two inner leaves; /a/c and /keep are excluded.
    assert_eq!(expected.len(), 2);
    let all = image_leaf_refs(&image)
        .unwrap()
        .into_iter()
        .map(|(_, id)| id)
        .collect::<BTreeSet<_>>();
    assert_eq!(all.len(), 4);
    assert!(expected.is_subset(&all) && !expected.is_empty());
}

#[test]
fn e_3_reachable_refs_blocks() {
    let sample = frag_sample(b"p", b"q", b"c", b"k", "sample");
    let image = frag_image(&sample);
    let ids = vec![
        RefId::from_bytes([0xaa; 16]),
        RefId::from_bytes([0xbb; 16]),
        RefId::from_bytes([0xaa; 16]), // duplicate input dedups
    ];
    let expected = ids.iter().copied().collect::<BTreeSet<_>>();
    // Blocks scope returns the ids themselves — they ARE the scope, no tree walk.
    let reachable = reachable_refs(&image, &SyncScope::Blocks(ids)).unwrap();
    assert_eq!(reachable, expected);
    assert_eq!(expected.len(), 2);
}
