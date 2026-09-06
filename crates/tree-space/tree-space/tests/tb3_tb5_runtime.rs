//! TB-3/TB-5 runtime-only acceptance coverage.

use std::collections::BTreeMap;
use tree_space::block::{Blob as BlockBlob, Block, BlockKind, Value};
use tree_space::tree::{ChunkGroup, DynamicField, DynamicNode, Slot, memory_gc_roots};
use tree_space::xpath::XPath;
use tree_space::{RefId, TreeNode};

fn stats(value: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("score".to_string(), Value::I64(value))])
}

#[test]
fn chunk_group_enforces_kind_and_selects_without_payload_access() {
    let first = BlockBlob::new(vec![1, 2, 3]);
    let second = BlockBlob::new(vec![4, 5]);
    let first_id = first.ref_id();
    let second_id = second.ref_id();
    let mut group = ChunkGroup::new();
    group.push_ref(first_id, BlockKind::Blob, stats(1)).unwrap();
    group
        .push_ref(second_id, BlockKind::Blob, stats(2))
        .unwrap();

    assert_eq!(
        group.select(|s| s.get("score") == Some(&Value::I64(2))),
        vec![second_id]
    );
    assert_eq!(group.refs(), vec![first_id, second_id]);
    assert!(
        group
            .push_ref(
                RefId::from_bytes([9; 16]),
                BlockKind::Sequence,
                BTreeMap::new()
            )
            .is_err()
    );
}

#[test]
fn nested_chunks_preserve_order_and_leaf_refs() {
    let a = BlockBlob::new(vec![1]);
    let b = BlockBlob::new(vec![2]);
    let a_id = a.ref_id();
    let b_id = b.ref_id();
    let mut nested = ChunkGroup::new();
    nested
        .push_ref(b_id, BlockKind::Blob, BTreeMap::new())
        .unwrap();
    let mut root = ChunkGroup::new();
    root.push_ref(a_id, BlockKind::Blob, BTreeMap::new())
        .unwrap();
    root.push_group(nested).unwrap();
    assert_eq!(root.refs(), vec![a_id, b_id]);
    assert_eq!(root.leaf_refs().len(), 2);
}

#[test]
fn dynamic_node_navigates_chunks_and_collects_absolute_refs() {
    let blob = BlockBlob::new(vec![7, 8]);
    let id = blob.ref_id();
    let mut chunks = ChunkGroup::new();
    chunks
        .push_ref(id, BlockKind::Blob, BTreeMap::new())
        .unwrap();
    let mut node = DynamicNode::new();
    node.insert("chunks", DynamicField::Chunks(chunks));

    assert!(matches!(
        node.get(&XPath::parse("/chunks[0]").unwrap()).unwrap(),
        tree_space::tree::AccessOut::Ref(found) if found == id
    ));
    assert_eq!(
        node.leaf_refs(),
        vec![(XPath::parse("/chunks[0]").unwrap(), id)]
    );
}

#[test]
fn persistence_is_monotonic_and_does_not_change_refs() {
    let blob = BlockBlob::new(vec![11]);
    let id = blob.ref_id();
    let mut node = DynamicNode::new();
    node.insert("payload", DynamicField::Slot(Slot::Ref(id)));
    let before = node.leaf_refs();
    let prefix = XPath::parse("/payload").unwrap();
    node.mark_ephemeral(prefix.clone());
    node.mark_ephemeral(prefix.clone());
    assert!(node.persistence().is_ephemeral(&prefix));
    assert!(
        node.persistence()
            .is_ephemeral(&XPath::parse("/payload/child").unwrap())
    );
    assert_eq!(node.persistence().ephemeral_prefixes().len(), 1);
    assert_eq!(node.leaf_refs(), before);
    assert_eq!(before[0].1, id);
    let cloned = node.clone();
    assert_eq!(cloned.persistence(), node.persistence());
    assert_eq!(cloned.leaf_refs(), before);
}

#[test]
fn memory_gc_roots_include_ephemeral_subtrees() {
    let keep = BlockBlob::new(vec![1]);
    let ephemeral = BlockBlob::new(vec![2]);
    let keep_id = keep.ref_id();
    let ephemeral_id = ephemeral.ref_id();
    let mut node = DynamicNode::new();
    node.insert("keep", DynamicField::Slot(Slot::Ref(keep_id)));
    node.insert("temporary", DynamicField::Slot(Slot::Ref(ephemeral_id)));
    node.mark_ephemeral(XPath::parse("/temporary").unwrap());
    let roots = memory_gc_roots([&node as &dyn TreeNode]);
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&keep_id));
    assert!(roots.contains(&ephemeral_id));
}

#[test]
fn dynamic_set_accepts_tb_slot() {
    let mut node = DynamicNode::new();
    let block = BlockBlob::new(vec![3]);
    let id = block.ref_id();
    node.set(&XPath::parse("/payload").unwrap(), Slot::Ref(id))
        .unwrap();
    assert_eq!(node.leaf_refs()[0].1, id);
}
