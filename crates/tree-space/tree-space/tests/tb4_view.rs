use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::{
    ArrowTable, Blob, Bucket, Combinator, CombinatorRegistry, JoinMode, Sequence, Value, ViewInput,
    ViewNode,
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
fn tb4_view_concat_select_and_nested_resolve_match_manual_result() {
    let left = table(&[1, 2], &["a", "b"]);
    let right = table(&[3], &["c"]);
    let mut bucket = Bucket::new();
    let left_id = bucket.put(&left);
    let right_id = bucket.put(&right);

    let concat = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(left_id), ViewInput::Ref(right_id)],
    )
    .unwrap();
    let select = ViewNode::new(
        Combinator::Select {
            columns: vec!["label".into(), "id".into()],
            rows: Some((1, 2)),
        },
        vec![ViewInput::View(Box::new(concat))],
    )
    .unwrap();
    let resolved = select.resolve(&bucket).unwrap();

    assert_eq!(
        resolved.ref_id(),
        // PL-2 M2 re-freeze: the resolved table's semantic identity.
        tree_space::RefId::from_bytes(hex("5eb4ec85a16ae8066b0d7e51afe7f030"))
    );
    let resolved_table = resolved.as_table().unwrap();
    assert_eq!(resolved_table.as_batch().schema().field(0).name(), "label");
    assert_eq!(resolved_table.as_batch().num_rows(), 2);
    assert_eq!(
        resolved_table
            .as_batch()
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0),
        "b"
    );
}

#[test]
fn tb4_view_join_inner_and_left_have_stable_row_semantics() {
    let left = table(&[1, 2], &["a", "b"]);
    let right = table(&[2, 3], &["B", "C"]);
    let mut bucket = Bucket::new();
    let left_id = bucket.put(&left);
    let right_id = bucket.put(&right);

    for (mode, expected_rows) in [(JoinMode::Inner, 1), (JoinMode::Left, 2)] {
        let view = ViewNode::new(
            Combinator::Join {
                keys: vec!["id".into()],
                mode,
            },
            vec![ViewInput::Ref(left_id), ViewInput::Ref(right_id)],
        )
        .unwrap();
        let resolved = view.resolve(&bucket).unwrap();
        let expected = match mode {
            // PL-2 M2 re-freeze: resolved-table semantic identities.
            JoinMode::Inner => "765df74e6ddcc64dc5aadf29da7d67b1",
            JoinMode::Left => "77909bc58d3bda247dfbc6ed9633dac9",
        };
        assert_eq!(
            resolved.ref_id(),
            tree_space::RefId::from_bytes(hex(expected))
        );
        let result = resolved.as_table().unwrap();
        assert_eq!(result.as_batch().num_rows(), expected_rows);
        assert_eq!(result.as_batch().num_columns(), 3);
        assert_eq!(result.as_batch().schema().field(0).name(), "id");
        assert_eq!(result.as_batch().schema().field(1).name(), "label");
        assert_eq!(result.as_batch().schema().field(2).name(), "label");
    }
}

fn hex(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

#[test]
fn tb4_view_concat_supports_sequence_blob_and_kv() {
    let mut bucket = Bucket::new();
    let sequence_a = bucket.put(&Sequence::new(vec![Value::I32(1)]));
    let sequence_b = bucket.put(&Sequence::new(vec![Value::I32(2)]));
    let sequence = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(sequence_a), ViewInput::Ref(sequence_b)],
    )
    .unwrap()
    .resolve(&bucket)
    .unwrap();
    assert_eq!(sequence.as_sequence().unwrap().values().len(), 2);

    let blob_a = bucket.put(&Blob::new(b"ab".to_vec()));
    let blob_b = bucket.put(&Blob::new(b"cd".to_vec()));
    let blob = ViewNode::new(
        Combinator::Concat,
        vec![ViewInput::Ref(blob_a), ViewInput::Ref(blob_b)],
    )
    .unwrap()
    .resolve(&bucket)
    .unwrap();
    assert_eq!(blob.as_blob().unwrap().bytes(), b"abcd");
}

#[test]
fn tb4_registry_is_open_for_extensions_but_closed_kernel_names_are_reserved() {
    let mut registry = CombinatorRegistry::new();
    assert!(
        registry
            .register(
                "identity",
                Arc::new(|blocks| {
                    Ok(Box::new(Blob::new(
                        blocks[0].as_blob().expect("blob input").bytes().to_vec(),
                    )))
                })
            )
            .is_ok()
    );
    assert!(
        registry
            .register("Concat", Arc::new(|_| Ok(Box::new(Blob::new([])))))
            .is_err()
    );
    assert!(
        registry
            .register("identity", Arc::new(|_| Ok(Box::new(Blob::new([])))))
            .is_err()
    );

    let mut bucket = Bucket::new();
    let source = bucket.put(&Blob::new(b"x".to_vec()));
    let view = ViewNode::new(
        Combinator::Registered("identity".into()),
        vec![ViewInput::Ref(source)],
    )
    .unwrap();
    let result = view.resolve_with_registry(&bucket, &registry).unwrap();
    assert_eq!(result.as_blob().unwrap().bytes(), b"x");
}

#[test]
fn tb4_registered_combinators_accept_variable_arity() {
    let mut registry = CombinatorRegistry::new();
    registry
        .register(
            "sum_sequences",
            Arc::new(|inputs| {
                let total = inputs
                    .iter()
                    .map(|block| {
                        block
                            .as_sequence()
                            .expect("sequence input")
                            .values()
                            .iter()
                            .filter_map(|value| match value {
                                Value::I32(n) => Some(*n),
                                _ => None,
                            })
                            .sum::<i32>()
                    })
                    .sum::<i32>();
                Ok(Box::new(Sequence::new(vec![Value::I32(total)])))
            }),
        )
        .unwrap();
    let mut bucket = Bucket::new();
    let a = bucket.put(&Sequence::new(vec![Value::I32(1)]));
    let b = bucket.put(&Sequence::new(vec![Value::I32(2)]));
    let c = bucket.put(&Sequence::new(vec![Value::I32(3)]));
    let view = ViewNode::new(
        Combinator::Registered("sum_sequences".into()),
        vec![ViewInput::Ref(a), ViewInput::Ref(b), ViewInput::Ref(c)],
    )
    .unwrap();
    let result = view.resolve_with_registry(&bucket, &registry).unwrap();
    assert_eq!(result.as_sequence().unwrap().values(), &[Value::I32(6)]);
}

#[test]
fn tb4_select_rejects_out_of_range_row_interval() {
    let source = table(&[1, 2, 3], &["a", "b", "c"]);
    let mut bucket = Bucket::new();
    let id = bucket.put(&source);
    for rows in [Some((3, 1)), Some((2, 2)), Some((0, 4))] {
        let view = ViewNode::new(
            Combinator::Select {
                columns: vec!["id".into()],
                rows,
            },
            vec![ViewInput::Ref(id)],
        )
        .unwrap();
        assert!(view.resolve(&bucket).is_err(), "rows {rows:?}");
    }
}
