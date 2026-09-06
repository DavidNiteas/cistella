//! A2-2: typed projection + derive wiring — typed encode maps onto the
//! universal image (golden 15), `project` rebuilds from image + bucket, and
//! 误装 / 结构不符 cases are rejected.

use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, TreeImage, encode_node, project};
use tree_space::{
    ArrowTable, Blob, Bucket, Kv, Sequence, TreeCodec, Value, decode, encode, named_field, tree_id,
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

#[derive(TreeCodec, Clone, Debug)]
struct Run {
    name: Value,
    note: Option<Value>,
    labels: Vec<Value>,
    grows: BTreeMap<String, Value>,
    keyed: BTreeMap<[u8; 16], Value>,
    table: ArrowTable,
    pair: Kv,
    blob: Blob,
    spectra: Vec<ArrowTable>,
}

#[derive(TreeCodec, Clone, Debug)]
struct Experiment {
    dataset: Value,
    run: Run,
}

fn fixture_table() -> ArrowTable {
    table(&[1, 2], &["a", "b"])
}

fn fixture() -> (Bucket, Experiment) {
    let table_block = fixture_table();
    let pair = Kv::try_new(vec![(Value::Utf8("k".into()), Value::Bool(true))]).unwrap();
    let blob = Blob::new(b"payload".to_vec());
    let spectra = vec![table(&[3], &["c"]), table(&[4], &["d"])];

    let mut bucket = Bucket::new();
    bucket.put(&table_block);
    bucket.put(&pair);
    bucket.put(&blob);
    for spectrum in &spectra {
        bucket.put(spectrum);
    }

    let run = Run {
        name: Value::Utf8("run-1".into()),
        note: Some(Value::I32(7)),
        labels: vec![Value::I32(1), Value::I64(2)],
        grows: BTreeMap::from([("a".into(), Value::F64(0x3ff0000000000000))]),
        keyed: BTreeMap::from([([7u8; 16], Value::Bool(true))]),
        table: table_block,
        pair,
        blob,
        spectra,
    };
    let experiment = Experiment {
        dataset: Value::Utf8("exp-1".into()),
        run,
    };
    (bucket, experiment)
}

#[test]
fn a2_2_typed_encode_decode_reencode_is_byte_stable() {
    // Golden 15 (canonical closed loop): 编码→弱解码→重编码字节相同。
    let (_, experiment) = fixture();
    let bytes = encode_node(&experiment).unwrap();
    let image = decode(&bytes).unwrap();
    let reencoded = encode(&image).unwrap();
    assert_eq!(bytes, reencoded);
}

#[test]
fn a2_2_project_rebuilds_identical_bytes() {
    // 构造→编码→弱解码→投影重建→重编码：字节相同。
    let (bucket, experiment) = fixture();
    let bytes = encode_node(&experiment).unwrap();
    let image = decode(&bytes).unwrap();
    let rebuilt: Experiment = project(&image, &bucket).unwrap();
    assert_eq!(encode_node(&rebuilt).unwrap(), bytes);
}

#[test]
fn a2_2_field_declaration_order_is_irrelevant() {
    // Golden 15: 字段声明序无关。
    #[derive(TreeCodec, Clone, Debug)]
    struct OrderedA {
        alpha: Value,
        beta: Value,
        gamma: Value,
    }
    #[derive(TreeCodec, Clone, Debug)]
    struct OrderedB {
        gamma: Value,
        alpha: Value,
        beta: Value,
    }
    let a = OrderedA {
        alpha: Value::I32(1),
        beta: Value::Utf8("b".into()),
        gamma: Value::Bool(true),
    };
    let b = OrderedB {
        gamma: Value::Bool(true),
        alpha: Value::I32(1),
        beta: Value::Utf8("b".into()),
    };
    assert_eq!(encode_node(&a).unwrap(), encode_node(&b).unwrap());
}

#[test]
fn a2_2_project_rejects_extra_image_children() {
    let (bucket, experiment) = fixture();
    let bytes = encode_node(&experiment).unwrap();
    let image = decode(&bytes).unwrap();
    // Rebuild the image with an extra ghost child that the template lacks.
    let mut children = image.children().to_vec();
    children.push(named_field("ghost", ImageContent::Inline(Value::I32(99))));
    let tampered = TreeImage::new(children);
    assert!(
        project::<Experiment>(&tampered, &bucket).is_err(),
        "undisclosed fields must be rejected"
    );
}

#[test]
fn a2_2_project_rejects_wrong_content_kind() {
    let (bucket, experiment) = fixture();
    let bytes = encode_node(&experiment).unwrap();
    let image = decode(&bytes).unwrap();
    // Change the `name` leaf from an Inline scalar into a Ref slot; the typed
    // template expects an inline value.
    let children = image
        .children()
        .iter()
        .map(|field| {
            let content = match &field.content {
                ImageContent::Node(kids) => ImageContent::Node(kids.clone()),
                ImageContent::Inline(_) => ImageContent::Node(Vec::new()),
                other => other.clone(),
            };
            tree_space::tree::codec::ImageField {
                locator: field.locator.clone(),
                content,
            }
        })
        .collect();
    let tampered = TreeImage::new(children);
    assert!(
        project::<Experiment>(&tampered, &bucket).is_err(),
        "inline field receiving a node must be rejected"
    );
}

#[test]
fn a2_2_project_rejects_block_kind_mismatch() {
    let (mut bucket, _experiment) = fixture();
    let sequence = Sequence::new(vec![Value::I32(1)]);
    let sequence_id = bucket.put(&sequence);
    // A tree whose `table` field references a Sequence block.
    let image = TreeImage::new(vec![named_field("table", ImageContent::Ref(sequence_id))]);
    #[derive(TreeCodec)]
    struct Probe {
        table: ArrowTable,
    }
    assert!(
        project::<Probe>(&image, &bucket).is_err(),
        "an expected Table field pointing at a Sequence must be rejected"
    );
}

#[test]
fn a2_2_project_rejects_missing_required_field() {
    let (bucket, _experiment) = fixture();
    let image = TreeImage::new(vec![]);
    #[derive(TreeCodec)]
    struct Probe {
        table: ArrowTable,
    }
    assert!(
        project::<Probe>(&image, &bucket).is_err(),
        "a missing required block field must be rejected"
    );
}

#[test]
fn a2_2_project_rejects_optional_field_duplicate_entries() {
    let (bucket, _experiment) = fixture();
    let image = TreeImage::new(vec![
        named_field("note", ImageContent::Inline(Value::I32(1))),
        named_field("note", ImageContent::Inline(Value::I32(2))),
    ]);
    #[derive(TreeCodec)]
    struct Probe {
        note: Option<Value>,
    }
    assert!(
        project::<Probe>(&image, &bucket).is_err(),
        "a duplicated optional entry must be rejected"
    );
}

#[test]
fn a2_2_project_rejects_container_locator_mismatch() {
    let (bucket, _experiment) = fixture();
    // `labels` is a Vec::<Value>; a container with named children instead of
    // positioned children must be rejected.
    let image = TreeImage::new(vec![named_field(
        "labels",
        ImageContent::Node(vec![named_field("0", ImageContent::Inline(Value::I32(1)))]),
    )]);
    #[derive(TreeCodec)]
    struct Probe {
        labels: Vec<Value>,
    }
    assert!(
        project::<Probe>(&image, &bucket).is_err(),
        "positioned container children are expected"
    );
}

#[test]
fn a2_2_typed_and_faithful_dynamic_mirror_share_tree_id() {
    // Golden 15 (dynamic subset): a typed tree whose shape DynamicNode can
    // carry and a faithful dynamic mirror produce the identical TreeId.
    #[derive(TreeCodec, Clone, Debug)]
    struct Inner {
        gamma: Value,
    }
    #[derive(TreeCodec, Clone, Debug)]
    struct Simple {
        alpha: Value,
        note: Option<Value>,
        child: Inner,
    }
    let typed = Simple {
        alpha: Value::I32(1),
        note: Some(Value::Utf8("n".into())),
        child: Inner {
            gamma: Value::Bool(true),
        },
    };
    let typed_id = tree_id(&encode_node(&typed).unwrap());

    let mut dynamic = tree_space::DynamicNode::new();
    dynamic.insert(
        "alpha",
        tree_space::DynamicField::Slot(tree_space::Slot::Inline(Value::I32(1))),
    );
    dynamic.insert(
        "note",
        tree_space::DynamicField::Slot(tree_space::Slot::Inline(Value::Utf8("n".into()))),
    );
    let mut child = tree_space::DynamicNode::new();
    child.insert(
        "gamma",
        tree_space::DynamicField::Slot(tree_space::Slot::Inline(Value::Bool(true))),
    );
    dynamic.insert("child", tree_space::DynamicField::Node(child));

    let image = tree_space::to_image(&dynamic).unwrap();
    let dynamic_bytes = encode(&image).unwrap();
    assert_eq!(tree_id(&dynamic_bytes), typed_id);
}
