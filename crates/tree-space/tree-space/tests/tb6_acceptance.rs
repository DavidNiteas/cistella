use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::{ChunkGroup, DynamicField, DynamicNode, memory_gc_roots};
use tree_space::xpath::XPath;
use tree_space::{
    ArrowTable, Blob, BlockKind, Bucket, Combinator, Kv, Sequence, Value, ViewInput, ViewNode,
};

fn table(ids: &[i32], labels: &[&str]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new("label", DataType::Utf8, false)),
    ]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(ids.to_vec())),
                Arc::new(StringArray::from(labels.to_vec())),
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn tb6_acceptance_template_put_chunk_view_gc_verify_rebuild() {
    let table_a = table(&[1], &["a"]);
    let table_b = table(&[2], &["b"]);
    let sequence = Sequence::new(vec![Value::Null, Value::I32(7)]);
    let kv = Kv::try_new(vec![(Value::Utf8("k".into()), Value::Bool(true))]).unwrap();
    let blob = Blob::new(b"payload".to_vec());
    let mut bucket = Bucket::new();
    let ids = [
        bucket.put(&table_a),
        bucket.put(&sequence),
        bucket.put(&kv),
        bucket.put(&blob),
    ];
    let table_b_id = bucket.put(&table_b);

    let mut chunks = ChunkGroup::new();
    chunks
        .push_ref(ids[0], BlockKind::Table, Default::default())
        .unwrap();
    let before = bucket.get(ids[0]).unwrap().encode();
    chunks
        .push_ref(table_b_id, BlockKind::Table, Default::default())
        .unwrap();
    assert_eq!(bucket.get(ids[0]).unwrap().encode(), before);

    let view = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(ids[0]), ViewInput::Ref(table_b_id)],
    )
    .unwrap();
    let resolved = view.resolve(&bucket).unwrap();
    let manual = table(&[1, 2], &["a", "b"]);
    assert_eq!(resolved.ref_id(), manual.ref_id());

    let mut node = DynamicNode::new();
    node.insert("chunks", DynamicField::Chunks(chunks));
    node.mark_ephemeral(XPath::parse("/chunks").unwrap());
    let roots = memory_gc_roots([&node as &dyn tree_space::TreeNode]);
    assert!(roots.contains(&ids[0]) && roots.contains(&table_b_id));
    bucket.verify_all().unwrap();

    let snapshot: Vec<_> = bucket.ids().collect();
    let mut rebuilt = Bucket::new();
    for id in snapshot {
        rebuilt
            .put_envelope(bucket.get(id).unwrap().clone())
            .unwrap();
    }
    rebuilt.verify_all().unwrap();
    assert_eq!(
        rebuilt.ids().collect::<Vec<_>>(),
        bucket.ids().collect::<Vec<_>>()
    );
}
