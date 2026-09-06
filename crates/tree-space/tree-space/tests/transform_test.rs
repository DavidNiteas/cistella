//! M10 transform rule application and coverage.

use arrow::array::{Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::path::Name;
use tree_space::transform::{ConversionPlan, ConversionRule, FunctionRegistry};
use tree_space::types::encode_field_ipc;

fn sample() -> RecordBatch {
    RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int32, false),
            Field::new("b", DataType::Utf8, false),
            Field::new("c", DataType::Int32, false),
        ])),
        vec![
            Arc::new(Int32Array::from(vec![1, 2, 3])),
            Arc::new(StringArray::from(vec!["x", "y", "z"])),
            Arc::new(Int32Array::from(vec![4, 5, 6])),
        ],
    )
    .unwrap()
}

#[test]
fn rename_and_drop_produce_expected_schema() {
    let batch = sample();
    let plan = ConversionPlan {
        dry_run: false,
        rules: vec![
            ConversionRule::RenameColumn {
                from: Name::new("a").unwrap(),
                to: Name::new("x").unwrap(),
            },
            ConversionRule::DropColumn {
                column: Name::new("b").unwrap(),
            },
        ],
    };
    let out = plan.apply(&[batch]).unwrap();
    let schema = out[0].schema();
    let names = schema
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["x", "c"]);
    assert_eq!(out[0].num_rows(), 3);
}

#[test]
fn convert_type_casts_column() {
    let batch = sample();
    let plan = ConversionPlan {
        dry_run: false,
        rules: vec![ConversionRule::ConvertType {
            column: Name::new("a").unwrap(),
            target: DataType::Int64,
        }],
    };
    let out = plan.apply(&[batch]).unwrap();
    assert_eq!(out[0].schema().field(0).data_type(), &DataType::Int64);
}

#[test]
fn coverage_rejects_missing_column() {
    let batch = sample();
    let plan = ConversionPlan {
        dry_run: false,
        rules: vec![ConversionRule::DropColumn {
            column: Name::new("nope").unwrap(),
        }],
    };
    let report = plan.coverage(batch.schema().as_ref());
    assert!(!report.executable);
    assert!(plan.apply(&[batch]).is_err());
}

#[test]
fn move_rewrites_subtree_prefix() {
    let plan = ConversionPlan {
        dry_run: false,
        rules: vec![ConversionRule::Move {
            from: "root/old".to_owned(),
            to: "root/new".to_owned(),
        }],
    };
    let path = tree_space::path::TablePath::parse("root/old/points").unwrap();
    let moved = plan.move_path(&path).unwrap();
    assert_eq!(moved.to_string(), "root/new/points");
}

#[test]
fn map_function_applies_registered_fn() {
    let batch = sample();
    let field = Field::new("a", DataType::Int32, false);
    let mut funcs = FunctionRegistry::new();
    funcs.register("double", |array: arrow::array::ArrayRef| {
        let input = array.as_any().downcast_ref::<Int32Array>().unwrap();
        let doubled = input
            .iter()
            .map(|value| value.map(|v| v * 2))
            .collect::<Int32Array>();
        Ok(Arc::new(doubled))
    });
    let plan = ConversionPlan {
        dry_run: false,
        rules: vec![ConversionRule::MapFunction {
            name: "double".to_owned(),
            input_schema_ipc: encode_field_ipc(&field),
            output_schema_ipc: encode_field_ipc(&field),
        }],
    };
    let out = plan.apply_with(&[batch], &funcs).unwrap();
    let values = out[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap();
    assert_eq!(values.value(0), 2);
    assert_eq!(values.value(1), 4);
    assert_eq!(values.value(2), 6);
}
