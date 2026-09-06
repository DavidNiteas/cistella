//! TB-7: `#[derive(TreeNode)]` contract on real node structs.
//!
//! Exercises the derive's codegen end-to-end: block leaves answer `get` by
//! identity (`AccessOut::Ref`, never payload on the bridge), `leaf_refs`
//! collects every block reference, and the weak `set` bridge writes inline
//! scalar values while rejecting writes into typed block fields.

use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;
use tree_space::tree::{AccessOut, Slot};
use tree_space::xpath::XPath;
use tree_space::{ArrowTable, Blob, Block, Kv, Sequence, TreeNode, Value};

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

#[derive(tree_space::TreeNode, Clone, Debug)]
struct Run {
    name: Value,
    grows: BTreeMap<String, Value>,
    table: ArrowTable,
    sequence: Sequence,
    pair: Kv,
    blob: Blob,
    spectra: Vec<ArrowTable>,
}

#[derive(tree_space::TreeNode, Clone, Debug)]
struct Experiment {
    dataset: Value,
    run: Run,
}

fn fixture() -> Experiment {
    Experiment {
        dataset: Value::Utf8("exp-1".into()),
        run: Run {
            name: Value::I32(7),
            grows: BTreeMap::from([("a".into(), Value::F64(0x3ff0000000000000))]),
            table: table(&[1], &["a"]),
            sequence: Sequence::new(vec![Value::I32(1), Value::I32(2)]),
            pair: Kv::try_new(vec![(Value::Utf8("k".into()), Value::Bool(true))]).unwrap(),
            blob: Blob::new(b"payload".to_vec()),
            spectra: vec![table(&[1], &["a"]), table(&[2], &["b"])],
        },
    }
}

#[test]
fn tb7_derive_get_answers_block_positions_by_identity() {
    let exp = fixture();
    let table_id = exp.run.table.ref_id();
    let sequence_id = exp.run.sequence.ref_id();
    let pair_id = exp.run.pair.ref_id();
    let blob_id = exp.run.blob.ref_id();
    let spectrum_id = exp.run.spectra[0].ref_id();

    for (path, expected) in [
        ("/run/table", table_id),
        ("/run/sequence", sequence_id),
        ("/run/pair", pair_id),
        ("/run/blob", blob_id),
        ("/run/spectra[0]", spectrum_id),
    ] {
        match exp.get(&XPath::parse(path).unwrap()).unwrap() {
            AccessOut::Ref(id) => assert_eq!(id, expected, "path {path}"),
            other => panic!("path {path}: expected Ref, got {other:?}"),
        }
    }
}

#[test]
fn tb7_derive_get_answers_inline_and_node_positions() {
    let exp = fixture();
    match exp.get(&XPath::parse("/dataset").unwrap()).unwrap() {
        AccessOut::Value(value) => assert_eq!(value, Value::Utf8("exp-1".into())),
        other => panic!("expected Value, got {other:?}"),
    }
    match exp.get(&XPath::parse("/run/name").unwrap()).unwrap() {
        AccessOut::Value(value) => assert_eq!(value, Value::I32(7)),
        other => panic!("expected Value, got {other:?}"),
    }
    match exp.get(&XPath::parse("/run/grows/a").unwrap()).unwrap() {
        AccessOut::Value(value) => assert_eq!(value, Value::F64(0x3ff0000000000000)),
        other => panic!("expected Value, got {other:?}"),
    }
    match exp.get(&XPath::parse("/run").unwrap()).unwrap() {
        AccessOut::Node(_) => {}
        other => panic!("expected Node, got {other:?}"),
    }
    assert!(exp.get(&XPath::parse("/run/missing").unwrap()).is_err());
}

#[test]
fn tb7_derive_leaf_refs_collect_every_block_reference() {
    let exp = fixture();
    let refs = exp.leaf_refs();
    let expected = vec![
        (
            XPath::root().field("run").field("table"),
            exp.run.table.ref_id(),
        ),
        (
            XPath::root().field("run").field("sequence"),
            exp.run.sequence.ref_id(),
        ),
        (
            XPath::root().field("run").field("pair"),
            exp.run.pair.ref_id(),
        ),
        (
            XPath::root().field("run").field("blob"),
            exp.run.blob.ref_id(),
        ),
        (
            XPath::root().field("run").field("spectra").index(0),
            exp.run.spectra[0].ref_id(),
        ),
        (
            XPath::root().field("run").field("spectra").index(1),
            exp.run.spectra[1].ref_id(),
        ),
    ];
    assert_eq!(refs.len(), expected.len());
    for (path, id) in expected {
        assert!(refs.contains(&(path.clone(), id)), "missing {path}: {id}");
    }
}

#[test]
fn tb7_derive_set_writes_inline_values_and_rejects_block_writes() {
    let mut exp = fixture();

    exp.set(
        &XPath::parse("/run/name").unwrap(),
        Slot::Inline(Value::I32(99)),
    )
    .unwrap();
    assert_eq!(exp.run.name, Value::I32(99));

    exp.set(
        &XPath::parse("/dataset").unwrap(),
        Slot::Inline(Value::Utf8("renamed".into())),
    )
    .unwrap();
    assert_eq!(exp.dataset, Value::Utf8("renamed".into()));

    assert!(
        exp.set(
            &XPath::parse("/run/table").unwrap(),
            Slot::Inline(Value::I32(1)),
        )
        .is_err(),
        "typed block field must not be writable through the weak bridge"
    );
    assert!(
        exp.set(
            &XPath::parse("/run/sequence").unwrap(),
            Slot::Ref(exp.run.table.ref_id()),
        )
        .is_err(),
        "typed block field must reject identity writes too"
    );
    assert!(
        exp.set(
            &XPath::parse("/run/name").unwrap(),
            Slot::Ref(exp.run.table.ref_id()),
        )
        .is_err(),
        "inline field must reject a block reference (fixed inline policy)"
    );
    assert!(
        exp.set(
            &XPath::parse("/run/grows/a").unwrap(),
            Slot::Inline(Value::I32(1)),
        )
        .is_err(),
        "container field is not writable through the weak bridge"
    );
}
