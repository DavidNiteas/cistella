//! A2-3: view/chunk full coverage and the tree acceptance test — view trees
//! (Join/Select/Registered + ViewParam + nested views), nested chunk groups,
//! `TreeImage ↔ DynamicNode` conversion, and the frozen golden 14 identity.

use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, ImageViewInput, TreeImage};
use tree_space::tree::{ChunkGroup, DynamicField, DynamicNode, JoinMode, Slot};
use tree_space::{
    Blob, BlockKind, Combinator, Kv, RefId, Sequence, Value, ViewInput, ViewNode, decode, encode,
    encode_node, project, to_image, tree_id, view_to_image,
};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn table(ids: &[i32], labels: &[&str]) -> tree_space::ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new("label", DataType::Utf8, false)),
    ]));
    tree_space::ArrowTable::try_new(
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
fn a2_3_view_tree_tree_id_is_frozen() {
    // Golden 14 (PL-2 M2 re-freeze): a view tree with Join (+ViewParam),
    // Select (+ViewParam), Registered, and a nested view.
    let ref_a = RefId::from_bytes(hex16("11111111111111111111111111111111"));
    let ref_b = RefId::from_bytes(hex16("22222222222222222222222222222222"));
    let select = ViewNode::new(
        Combinator::Select {
            columns: vec!["id".into()],
            rows: Some((0, 4)),
        },
        vec![ViewInput::Ref(ref_a)],
    )
    .unwrap();
    let join = ViewNode::new(
        Combinator::Join {
            keys: vec!["k".into()],
            mode: JoinMode::Inner,
        },
        vec![ViewInput::Ref(ref_a), ViewInput::View(Box::new(select))],
    )
    .unwrap();
    let registered = ViewNode::new(
        Combinator::Registered("example.slice".into()),
        vec![ViewInput::Ref(ref_b)],
    )
    .unwrap();
    let image = TreeImage::new(vec![
        named("join", ImageContent::View(view_to_image(&join))),
        named("reg", ImageContent::View(view_to_image(&registered))),
    ]);
    let bytes = encode(&image).unwrap();
    assert_eq!(
        tree_id(&bytes),
        tree_space::TreeId::from_bytes(hex16("6dfa6239c44aa13f4f30c5bbb4be92cd"))
    );
}

#[test]
fn a2_3_view_image_roundtrip_preserves_params_and_nesting() {
    let ref_a = RefId::from_bytes(hex16("01010101010101010101010101010101"));
    let nested = ViewNode::new(
        Combinator::Select {
            columns: vec!["a".into(), "b".into()],
            rows: None,
        },
        vec![ViewInput::Ref(ref_a)],
    )
    .unwrap();
    let join = ViewNode::new(
        Combinator::Join {
            keys: vec!["k1".into(), "k2".into()],
            mode: JoinMode::Left,
        },
        vec![ViewInput::View(Box::new(nested)), ViewInput::Ref(ref_a)],
    )
    .unwrap();
    let image = TreeImage::new(vec![named("v", ImageContent::View(view_to_image(&join)))]);
    let bytes = encode(&image).unwrap();
    assert_eq!(decode(&bytes).unwrap(), image);
}

#[test]
fn a2_3_registered_view_decode_is_structure() {
    let ref_b = RefId::from_bytes(hex16("22222222222222222222222222222222"));
    let registered = ViewNode::new(
        Combinator::Registered("example.slice".into()),
        vec![ViewInput::Ref(ref_b)],
    )
    .unwrap();
    let image = TreeImage::new(vec![named(
        "reg",
        ImageContent::View(view_to_image(&registered)),
    )]);
    let bytes = encode(&image).unwrap();
    let decoded = decode(&bytes).unwrap();
    let ImageContent::View(view) = &decoded.children()[0].content else {
        panic!("expected a view");
    };
    assert_eq!(view.combinator, "example.slice");
    assert_eq!(view.inputs, vec![ImageViewInput::Ref(ref_b)]);
}

#[test]
fn a2_3_nested_chunk_group_roundtrip() {
    let outer_ref = RefId::from_bytes(hex16("0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"));
    let inner_ref = RefId::from_bytes(hex16("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"));
    let mut group = ChunkGroup::new();
    group
        .push_ref(outer_ref, BlockKind::Sequence, BTreeMap::new())
        .unwrap();
    let mut nested = ChunkGroup::new();
    nested
        .push_ref(inner_ref, BlockKind::Sequence, BTreeMap::new())
        .unwrap();
    group.push_group(nested).unwrap();

    let mut dynamic = DynamicNode::new();
    dynamic.insert("chunks", DynamicField::Chunks(group));
    let image = to_image(&dynamic).unwrap();
    let bytes = encode(&image).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(encode(&decoded).unwrap(), bytes);

    // Stats never enter canonical bytes: the decoded group carries no stats.
    let back = tree_space::from_image(&decoded).unwrap();
    let DynamicField::Chunks(decoded_group) = back.get_field("chunks").expect("chunk field") else {
        panic!("expected a chunk field")
    };
    assert_eq!(decoded_group.refs(), vec![outer_ref, inner_ref]);
    assert_eq!(decoded_group.kind(), BlockKind::Sequence);
}

#[test]
fn a2_3_dynamic_image_roundtrip_is_lossless_for_the_dynamic_subset() {
    let ref_a = RefId::from_bytes(hex16("0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c"));
    let view = ViewNode::new(
        Combinator::Select {
            columns: vec!["id".into()],
            rows: None,
        },
        vec![ViewInput::Ref(ref_a)],
    )
    .unwrap();
    let mut dynamic = DynamicNode::new();
    dynamic.insert(
        "name",
        DynamicField::Slot(Slot::Inline(Value::Utf8("dyn".into()))),
    );
    dynamic.insert("blk", DynamicField::Slot(Slot::Ref(ref_a)));
    dynamic.insert("view", DynamicField::View(view));

    let image = to_image(&dynamic).unwrap();
    let bytes = encode(&image).unwrap();
    // 意象层闭环：dead-code → 互转 → 重编码字节相同。
    let back = tree_space::from_image(&decode(&bytes).unwrap()).unwrap();
    let image_again = to_image(&back).unwrap();
    assert_eq!(encode(&image_again).unwrap(), bytes);
}

#[test]
fn a2_3_acceptance_construct_encode_weakdecode_project_rebuild_equivalence() {
    // acceptance：构造 → 编码 → 弱解码 ≡ 投影重建 ≡ 手工构造。
    let table_block = table(&[1, 2], &["a", "b"]);
    let pair = Kv::try_new(vec![(Value::Utf8("k".into()), Value::Bool(true))]).unwrap();
    let blob = Blob::new(b"payload".to_vec());
    let sequence = Sequence::new(vec![Value::I32(1), Value::I64(2)]);

    let mut bucket = tree_space::Bucket::new();
    let table_id = bucket.put(&table_block);
    let table_b = table(&[3], &["c"]);
    let table_b_id = bucket.put(&table_b);
    let pair_id = bucket.put(&pair);
    let blob_id = bucket.put(&blob);
    let sequence_id = bucket.put(&sequence);

    let mut chunks = ChunkGroup::new();
    chunks
        .push_ref(table_id, BlockKind::Table, BTreeMap::new())
        .unwrap();
    chunks
        .push_ref(table_b_id, BlockKind::Table, BTreeMap::new())
        .unwrap();

    let mut dynamic = DynamicNode::new();
    dynamic.insert(
        "name",
        DynamicField::Slot(Slot::Inline(Value::Utf8("accepted".into()))),
    );
    dynamic.insert("pair", DynamicField::Slot(Slot::Ref(pair_id)));
    dynamic.insert("blob", DynamicField::Slot(Slot::Ref(blob_id)));
    dynamic.insert("seq", DynamicField::Slot(Slot::Ref(sequence_id)));
    dynamic.insert("chunks", DynamicField::Chunks(chunks));
    let view = ViewNode::new(
        Combinator::Select {
            columns: vec!["id".into()],
            rows: None,
        },
        vec![ViewInput::Ref(table_id)],
    )
    .unwrap();
    dynamic.insert("view", DynamicField::View(view));

    // 构造 → 编码 → 弱解码。
    let image = to_image(&dynamic).unwrap();
    let bytes = encode(&image).unwrap();
    let weak = decode(&bytes).unwrap();
    assert_eq!(encode(&weak).unwrap(), bytes);

    // 手工构造等价：另一个以同一数据构造的树得到相同的 TreeId。
    let mut manual = DynamicNode::new();
    manual.insert(
        "name",
        DynamicField::Slot(Slot::Inline(Value::Utf8("accepted".into()))),
    );
    manual.insert("pair", DynamicField::Slot(Slot::Ref(pair_id)));
    manual.insert("blob", DynamicField::Slot(Slot::Ref(blob_id)));
    manual.insert("seq", DynamicField::Slot(Slot::Ref(sequence_id)));
    let mut manual_chunks = ChunkGroup::new();
    manual_chunks
        .push_ref(table_id, BlockKind::Table, BTreeMap::new())
        .unwrap();
    manual_chunks
        .push_ref(table_b_id, BlockKind::Table, BTreeMap::new())
        .unwrap();
    manual.insert("chunks", DynamicField::Chunks(manual_chunks));
    let manual_view = ViewNode::new(
        Combinator::Select {
            columns: vec!["id".into()],
            rows: None,
        },
        vec![ViewInput::Ref(table_id)],
    )
    .unwrap();
    manual.insert("view", DynamicField::View(manual_view));
    assert_eq!(
        tree_id(&bytes),
        tree_id(&encode(&to_image(&manual).unwrap()).unwrap())
    );

    // 投影重建（typed）：typed 树经编码→弱解码→投影重建字节不变。
    #[derive(tree_space::TreeCodec, Clone, Debug)]
    struct Probe {
        name: Value,
        pair: Kv,
        blob: Blob,
        spectra: Vec<tree_space::ArrowTable>,
        seqs: Vec<tree_space::Sequence>,
    }
    let probe = Probe {
        name: Value::Utf8("probe".into()),
        pair: pair.clone(),
        blob: blob.clone(),
        spectra: vec![table_block.clone()],
        seqs: vec![sequence.clone()],
    };
    let probe_bytes = encode_node(&probe).unwrap();
    let probe_image = decode(&probe_bytes).unwrap();
    let rebuilt: Probe = project(&probe_image, &bucket).unwrap();
    assert_eq!(encode_node(&rebuilt).unwrap(), probe_bytes);
}

fn named(name: &str, content: ImageContent) -> tree_space::tree::codec::ImageField {
    tree_space::tree::codec::named_field(name, content)
}
