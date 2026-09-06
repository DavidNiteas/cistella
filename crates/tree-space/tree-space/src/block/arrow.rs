//! Canonical Arrow representations for the built-in non-table blocks.

use super::{Decimal128, Decimal256, DurationUnit, Time32Unit, Time64Unit, TimestampUnit, Value};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ipc::{decode_batch, encode_batch, ipc_error};
use arrow::array::{
    Array, ArrayRef, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    Decimal256Array, DurationMicrosecondArray, DurationMillisecondArray, DurationNanosecondArray,
    DurationSecondArray, FixedSizeBinaryArray, Float32Array, Float64Array, Int8Array, Int16Array,
    Int32Array, Int64Array, NullArray, StringArray, Time32MillisecondArray, Time32SecondArray,
    Time64MicrosecondArray, Time64NanosecondArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array, UnionArray,
};
use arrow::buffer::ScalarBuffer;
use arrow::datatypes::{DataType, Field, Schema, UnionFields, UnionMode, i256};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

const VALUE_CHILDREN: usize = 22;

pub(crate) fn encode_sequence(values: &[Value]) -> Result<Vec<u8>> {
    let array = values_to_array(values)?;
    encode_batch(&single_column_batch("value", array))
}

pub(crate) fn decode_sequence(bytes: &[u8]) -> Result<Vec<Value>> {
    let batch = decode_batch(bytes)?;
    if batch.num_columns() != 1 || batch.schema().field(0).name() != "value" {
        return Err(malformed(
            "sequence Arrow schema must contain one value column",
        ));
    }
    array_to_values(batch.column(0))
}

pub(crate) fn encode_kv(entries: &[(Value, Value)]) -> Result<Vec<u8>> {
    let keys = entries
        .iter()
        .map(|(key, _)| key)
        .cloned()
        .collect::<Vec<_>>();
    let values = entries
        .iter()
        .map(|(_, value)| value)
        .cloned()
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(vec![
        Field::new("key", values_data_type(&keys)?, true),
        Field::new("value", values_data_type(&values)?, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![values_to_array(&keys)?, values_to_array(&values)?],
    )
    .map_err(ipc_error)?;
    encode_batch(&batch)
}

pub(crate) fn decode_kv(bytes: &[u8]) -> Result<Vec<(Value, Value)>> {
    let batch = decode_batch(bytes)?;
    if batch.num_columns() != 2
        || batch.schema().field(0).name() != "key"
        || batch.schema().field(1).name() != "value"
    {
        return Err(malformed(
            "kv Arrow schema must contain key and value columns",
        ));
    }
    let keys = array_to_values(batch.column(0))?;
    let values = array_to_values(batch.column(1))?;
    if keys.len() != values.len() {
        return Err(malformed("kv Arrow columns have different lengths"));
    }
    Ok(keys.into_iter().zip(values).collect())
}

pub(crate) fn encode_blob(bytes: &[u8]) -> Result<Vec<u8>> {
    let array = BinaryArray::from(vec![Some(bytes)]);
    encode_batch(&single_column_batch("blob", Arc::new(array)))
}

pub(crate) fn decode_blob(bytes: &[u8]) -> Result<Vec<u8>> {
    let batch = decode_batch(bytes)?;
    if batch.num_columns() != 1 || batch.schema().field(0).name() != "blob" {
        return Err(malformed("blob Arrow schema must contain one blob column"));
    }
    let array = batch
        .column(0)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .ok_or_else(|| malformed("blob Arrow column is not Binary"))?;
    if array.len() != 1 || array.is_null(0) {
        return Err(malformed(
            "blob Arrow column must contain one non-null value",
        ));
    }
    Ok(array.value(0).to_vec())
}

fn single_column_batch(name: &str, array: ArrayRef) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![Field::new(
        name,
        array.data_type().clone(),
        true,
    )]));
    RecordBatch::try_new(schema, vec![array]).expect("valid single Arrow column")
}

fn values_data_type(values: &[Value]) -> Result<DataType> {
    if values.is_empty() {
        return Ok(DataType::Null);
    }
    let first = values.iter().find(|value| !matches!(value, Value::Null));
    let Some(first) = first else {
        return Ok(DataType::Null);
    };
    if values.iter().all(|value| compatible_typed(first, value)) {
        Ok(value_data_type(first))
    } else {
        Ok(union_data_type())
    }
}

fn values_to_array(values: &[Value]) -> Result<ArrayRef> {
    let first = values.iter().find(|value| !matches!(value, Value::Null));
    if first.is_none()
        || values
            .iter()
            .all(|value| compatible_typed(first.unwrap(), value))
    {
        return typed_array(values, first);
    }
    union_array(values)
}

fn compatible_typed(first: &Value, value: &Value) -> bool {
    match (first, value) {
        (_, Value::Null) => true,
        (Value::Bool(_), Value::Bool(_))
        | (Value::I8(_), Value::I8(_))
        | (Value::I16(_), Value::I16(_))
        | (Value::I32(_), Value::I32(_))
        | (Value::I64(_), Value::I64(_))
        | (Value::U8(_), Value::U8(_))
        | (Value::U16(_), Value::U16(_))
        | (Value::U32(_), Value::U32(_))
        | (Value::U64(_), Value::U64(_))
        | (Value::F32(_), Value::F32(_))
        | (Value::F64(_), Value::F64(_))
        | (Value::Utf8(_), Value::Utf8(_))
        | (Value::Binary(_), Value::Binary(_))
        | (Value::Date32(_), Value::Date32(_))
        | (Value::Date64(_), Value::Date64(_)) => true,
        (Value::Time32(a, _), Value::Time32(b, _)) => a == b,
        (Value::Time64(a, _), Value::Time64(b, _)) => a == b,
        (Value::Timestamp(au, at, _), Value::Timestamp(bu, bt, _)) => au == bu && at == bt,
        (Value::Duration(a, _), Value::Duration(b, _)) => a == b,
        (Value::Decimal128(a), Value::Decimal128(b)) => {
            a.precision == b.precision && a.scale == b.scale
        }
        (Value::Decimal256(a), Value::Decimal256(b)) => {
            a.precision == b.precision && a.scale == b.scale
        }
        _ => false,
    }
}

fn value_data_type(value: &Value) -> DataType {
    match value {
        Value::Null => DataType::Null,
        Value::Bool(_) => DataType::Boolean,
        Value::I8(_) => DataType::Int8,
        Value::I16(_) => DataType::Int16,
        Value::I32(_) => DataType::Int32,
        Value::I64(_) => DataType::Int64,
        Value::U8(_) => DataType::UInt8,
        Value::U16(_) => DataType::UInt16,
        Value::U32(_) => DataType::UInt32,
        Value::U64(_) => DataType::UInt64,
        Value::F32(_) => DataType::Float32,
        Value::F64(_) => DataType::Float64,
        Value::Utf8(_) => DataType::Utf8,
        Value::Binary(_) => DataType::Binary,
        Value::Date32(_) => DataType::Date32,
        Value::Date64(_) => DataType::Date64,
        Value::Time32(unit, _) => match unit {
            Time32Unit::Second => DataType::Time32(arrow::datatypes::TimeUnit::Second),
            Time32Unit::Millisecond => DataType::Time32(arrow::datatypes::TimeUnit::Millisecond),
        },
        Value::Time64(unit, _) => match unit {
            Time64Unit::Microsecond => DataType::Time64(arrow::datatypes::TimeUnit::Microsecond),
            Time64Unit::Nanosecond => DataType::Time64(arrow::datatypes::TimeUnit::Nanosecond),
        },
        Value::Timestamp(unit, timezone, _) => DataType::Timestamp(
            timestamp_unit(*unit),
            timezone.clone().map(Arc::<str>::from),
        ),
        Value::Duration(unit, _) => DataType::Duration(duration_unit(*unit)),
        Value::Decimal128(value) => DataType::Decimal128(value.precision, value.scale),
        Value::Decimal256(value) => DataType::Decimal256(value.precision, value.scale),
    }
}

fn typed_array(values: &[Value], first: Option<&Value>) -> Result<ArrayRef> {
    let Some(first) = first else {
        return Ok(Arc::new(NullArray::new(values.len())));
    };
    macro_rules! primitive {
        ($array:ty, $variant:pat, $value:expr) => {{
            let data = values
                .iter()
                .map(|value| match value {
                    Value::Null => None,
                    $variant => Some($value),
                    _ => None,
                })
                .collect::<Vec<_>>();
            Arc::new(<$array>::from(data)) as ArrayRef
        }};
    }
    Ok(match first {
        Value::Null => Arc::new(NullArray::new(values.len())) as ArrayRef,
        Value::Bool(_) => primitive!(BooleanArray, Value::Bool(value), *value),
        Value::I8(_) => primitive!(Int8Array, Value::I8(value), *value),
        Value::I16(_) => primitive!(Int16Array, Value::I16(value), *value),
        Value::I32(_) => primitive!(Int32Array, Value::I32(value), *value),
        Value::I64(_) => primitive!(Int64Array, Value::I64(value), *value),
        Value::U8(_) => primitive!(UInt8Array, Value::U8(value), *value),
        Value::U16(_) => primitive!(UInt16Array, Value::U16(value), *value),
        Value::U32(_) => primitive!(UInt32Array, Value::U32(value), *value),
        Value::U64(_) => primitive!(UInt64Array, Value::U64(value), *value),
        Value::F32(_) => primitive!(Float32Array, Value::F32(value), f32::from_bits(*value)),
        Value::F64(_) => primitive!(Float64Array, Value::F64(value), f64::from_bits(*value)),
        Value::Utf8(_) => Arc::new(StringArray::from(
            values
                .iter()
                .map(|value| match value {
                    Value::Utf8(value) => Some(value.as_str()),
                    Value::Null => None,
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )) as ArrayRef,
        Value::Binary(_) => Arc::new(BinaryArray::from(
            values
                .iter()
                .map(|value| match value {
                    Value::Binary(value) => Some(value.as_slice()),
                    Value::Null => None,
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )) as ArrayRef,
        Value::Date32(_) => primitive!(Date32Array, Value::Date32(value), *value),
        Value::Date64(_) => primitive!(Date64Array, Value::Date64(value), *value),
        Value::Time32(Time32Unit::Second, _) => primitive!(
            Time32SecondArray,
            Value::Time32(Time32Unit::Second, value),
            *value
        ),
        Value::Time32(Time32Unit::Millisecond, _) => primitive!(
            Time32MillisecondArray,
            Value::Time32(Time32Unit::Millisecond, value),
            *value
        ),
        Value::Time64(Time64Unit::Microsecond, _) => primitive!(
            Time64MicrosecondArray,
            Value::Time64(Time64Unit::Microsecond, value),
            *value
        ),
        Value::Time64(Time64Unit::Nanosecond, _) => primitive!(
            Time64NanosecondArray,
            Value::Time64(Time64Unit::Nanosecond, value),
            *value
        ),
        Value::Timestamp(unit, timezone, _) => timestamp_array(values, *unit, timezone.clone())?,
        Value::Duration(unit, _) => duration_array(values, *unit)?,
        Value::Decimal128(value) => decimal128_array(values, value.precision, value.scale)?,
        Value::Decimal256(value) => decimal256_array(values, value.precision, value.scale)?,
    })
}

/// The fixed 22-child DenseUnion `DataType` used by the value bridge.
///
/// The child order is frozen as the `Value` tag order (see
/// `_dev/archive/树与桶模型改造/01-目标与设计.md` §2.4.1). Exposed crate-wide for the
/// A-2 tree codec's always-union value column.
pub(crate) fn union_data_type() -> DataType {
    let fields = (0..VALUE_CHILDREN)
        .map(|id| Field::new(format!("v{id}"), union_child_type(id), true))
        .collect::<Vec<_>>();
    DataType::Union(
        UnionFields::new((0..VALUE_CHILDREN as i8).collect::<Vec<_>>(), fields),
        UnionMode::Dense,
    )
}

fn union_child_type(id: usize) -> DataType {
    match id {
        0 => DataType::Null,
        1 => DataType::Boolean,
        2 => DataType::Int8,
        3 => DataType::Int16,
        4 => DataType::Int32,
        5 => DataType::Int64,
        6 => DataType::UInt8,
        7 => DataType::UInt16,
        8 => DataType::UInt32,
        9 => DataType::UInt64,
        10 => DataType::Float32,
        11 => DataType::Float64,
        12 => DataType::Utf8,
        13 => DataType::Binary,
        14 => DataType::Date32,
        15 => DataType::Date64,
        16 => DataType::FixedSizeBinary(5),
        17 => DataType::FixedSizeBinary(9),
        18 => DataType::Binary,
        19 => DataType::FixedSizeBinary(9),
        20 => DataType::FixedSizeBinary(18),
        21 => DataType::FixedSizeBinary(34),
        _ => unreachable!(),
    }
}

/// Builds a 22-child DenseUnion array over the supplied scalars. Exposed
/// crate-wide for the always-union value column of the A-2 tree codec.
pub(crate) fn union_array(values: &[Value]) -> Result<ArrayRef> {
    let mut children = (0..VALUE_CHILDREN)
        .map(|id| empty_child_array(id))
        .collect::<Vec<_>>();
    let mut type_ids = Vec::with_capacity(values.len());
    let mut offsets = Vec::with_capacity(values.len());
    let mut child_values = (0..VALUE_CHILDREN)
        .map(|_| Vec::<Value>::new())
        .collect::<Vec<_>>();
    for value in values {
        let id = value.tag() as usize;
        type_ids.push(id as i8);
        offsets.push(child_values[id].len() as i32);
        child_values[id].push(value.clone());
    }
    for id in 0..VALUE_CHILDREN {
        if !child_values[id].is_empty() {
            children[id] = union_child_array(id, &child_values[id])?;
        }
    }
    let fields = (0..VALUE_CHILDREN)
        .map(|id| Field::new(format!("v{id}"), union_child_type(id), true))
        .collect::<Vec<_>>();
    let array = UnionArray::try_new(
        UnionFields::new((0..VALUE_CHILDREN as i8).collect::<Vec<_>>(), fields),
        ScalarBuffer::from(type_ids),
        Some(ScalarBuffer::from(offsets)),
        children,
    )
    .map_err(ipc_error)?;
    Ok(Arc::new(array))
}

/// Builds an empty child array for a union child id. Exposed crate-wide for
/// the A-2 tree codec's value-column reader and its unit tests.
pub(crate) fn empty_child_array(id: usize) -> ArrayRef {
    match id {
        0 => Arc::new(NullArray::new(0)),
        1 => Arc::new(BooleanArray::from(Vec::<Option<bool>>::new())),
        2 => Arc::new(Int8Array::from(Vec::<Option<i8>>::new())),
        3 => Arc::new(Int16Array::from(Vec::<Option<i16>>::new())),
        4 => Arc::new(Int32Array::from(Vec::<Option<i32>>::new())),
        5 => Arc::new(Int64Array::from(Vec::<Option<i64>>::new())),
        6 => Arc::new(UInt8Array::from(Vec::<Option<u8>>::new())),
        7 => Arc::new(UInt16Array::from(Vec::<Option<u16>>::new())),
        8 => Arc::new(UInt32Array::from(Vec::<Option<u32>>::new())),
        9 => Arc::new(UInt64Array::from(Vec::<Option<u64>>::new())),
        10 => Arc::new(Float32Array::from(Vec::<Option<f32>>::new())),
        11 => Arc::new(Float64Array::from(Vec::<Option<f64>>::new())),
        12 => Arc::new(StringArray::from(Vec::<Option<&str>>::new())),
        13 => Arc::new(BinaryArray::from(Vec::<Option<&[u8]>>::new())),
        14 => Arc::new(Date32Array::from(Vec::<Option<i32>>::new())),
        15 => Arc::new(Date64Array::from(Vec::<Option<i64>>::new())),
        16 | 17 | 19 | 20 | 21 => Arc::new(
            FixedSizeBinaryArray::try_from_sparse_iter_with_size(
                std::iter::empty::<Option<&[u8]>>(),
                match id {
                    16 => 5,
                    17 | 19 => 9,
                    20 => 18,
                    21 => 34,
                    _ => unreachable!(),
                },
            )
            .expect("valid empty fixed binary child"),
        ),
        18 => Arc::new(BinaryArray::from(Vec::<Option<&[u8]>>::new())),
        _ => unreachable!(),
    }
}

fn union_child_array(id: usize, values: &[Value]) -> Result<ArrayRef> {
    match id {
        0 => Ok(Arc::new(NullArray::new(values.len()))),
        1..=15 => typed_array(values, values.first()),
        16 | 17 | 19 | 20 | 21 => {
            let width = match id {
                16 => 5,
                17 | 19 => 9,
                20 => 18,
                21 => 34,
                _ => unreachable!(),
            };
            let bytes = values
                .iter()
                .map(|value| parameter_bytes(value))
                .collect::<Result<Vec<_>>>()?;
            FixedSizeBinaryArray::try_from_sparse_iter_with_size(
                bytes.iter().map(|value| Some(value.as_slice())),
                width,
            )
            .map(|array| Arc::new(array) as ArrayRef)
            .map_err(ipc_error)
        }
        18 => {
            let bytes = values
                .iter()
                .map(|value| parameter_bytes(value))
                .collect::<Result<Vec<_>>>()?;
            Ok(Arc::new(BinaryArray::from(
                bytes
                    .iter()
                    .map(|value| Some(value.as_slice()))
                    .collect::<Vec<_>>(),
            )))
        }
        _ => Err(malformed("invalid union child id")),
    }
}

fn parameter_bytes(value: &Value) -> Result<Vec<u8>> {
    let encoded = value.encode();
    Ok(encoded.get(1..).unwrap_or_default().to_vec())
}

/// Returns `Value` elements from an Arrow array, transparently unwrapping a
/// DenseUnion when present. Exposed crate-wide for the A-2 tree codec.
pub(crate) fn array_to_values(array: &ArrayRef) -> Result<Vec<Value>> {
    if let Some(union) = array.as_any().downcast_ref::<UnionArray>() {
        return union_to_values(union);
    }
    let mut values = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        values.push(if array.is_null(index) {
            Value::Null
        } else {
            array_value(array, index)?
        });
    }
    Ok(values)
}

/// Reads a DenseUnion array back into `Value` elements.
///
/// Out-of-range type ids and out-of-range child offsets are reported as
/// malformed payloads instead of panicking (canonical encoders never produce
/// them, but untrusted bytes may). Exposed crate-wide for the A-2 tree codec.
pub(crate) fn union_to_values(array: &UnionArray) -> Result<Vec<Value>> {
    (0..array.len())
        .map(|index| {
            let id = array.type_id(index);
            if id < 0 || id as u8 >= VALUE_CHILDREN as u8 {
                return Err(malformed("value union type id out of range"));
            }
            let id_usize = id as usize;
            let child = array.child(id);
            let offset = array.value_offset(index);
            if offset >= child.len() {
                return Err(malformed("value union offset out of range"));
            }
            if id == 0 {
                Ok(Value::Null)
            } else if child.is_null(offset) {
                Ok(Value::Null)
            } else if id_usize <= 15 {
                array_value(child, offset)
            } else {
                parameter_value(id_usize, child, offset)
            }
        })
        .collect()
}

/// Reads one scalar from a non-union Arrow array slot. Exposed crate-wide for
/// the A-2 tree codec's value-column reader.
pub(crate) fn array_value(array: &ArrayRef, index: usize) -> Result<Value> {
    macro_rules! primitive {
        ($ty:ty, $variant:ident) => {
            Ok(Value::$variant(
                array
                    .as_any()
                    .downcast_ref::<$ty>()
                    .ok_or_else(|| malformed("Arrow type mismatch"))?
                    .value(index),
            ))
        };
    }
    match array.data_type() {
        DataType::Boolean => Ok(Value::Bool(
            array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index),
        )),
        DataType::Int8 => primitive!(Int8Array, I8),
        DataType::Int16 => primitive!(Int16Array, I16),
        DataType::Int32 => primitive!(Int32Array, I32),
        DataType::Int64 => primitive!(Int64Array, I64),
        DataType::UInt8 => primitive!(UInt8Array, U8),
        DataType::UInt16 => primitive!(UInt16Array, U16),
        DataType::UInt32 => primitive!(UInt32Array, U32),
        DataType::UInt64 => primitive!(UInt64Array, U64),
        DataType::Float32 => Ok(Value::F32(
            array
                .as_any()
                .downcast_ref::<Float32Array>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_bits(),
        )),
        DataType::Float64 => Ok(Value::F64(
            array
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_bits(),
        )),
        DataType::Utf8 => Ok(Value::Utf8(
            array
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_owned(),
        )),
        DataType::Binary => Ok(Value::Binary(
            array
                .as_any()
                .downcast_ref::<BinaryArray>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_vec(),
        )),
        DataType::Date32 => primitive!(Date32Array, Date32),
        DataType::Date64 => primitive!(Date64Array, Date64),
        DataType::Time32(unit) => match unit {
            arrow::datatypes::TimeUnit::Second => Ok(Value::Time32(
                Time32Unit::Second,
                array
                    .as_any()
                    .downcast_ref::<Time32SecondArray>()
                    .ok_or_else(|| malformed("Arrow type mismatch"))?
                    .value(index),
            )),
            arrow::datatypes::TimeUnit::Millisecond => Ok(Value::Time32(
                Time32Unit::Millisecond,
                array
                    .as_any()
                    .downcast_ref::<Time32MillisecondArray>()
                    .ok_or_else(|| malformed("Arrow type mismatch"))?
                    .value(index),
            )),
            _ => Err(malformed("invalid Time32 unit")),
        },
        DataType::Time64(unit) => match unit {
            arrow::datatypes::TimeUnit::Microsecond => Ok(Value::Time64(
                Time64Unit::Microsecond,
                array
                    .as_any()
                    .downcast_ref::<Time64MicrosecondArray>()
                    .ok_or_else(|| malformed("Arrow type mismatch"))?
                    .value(index),
            )),
            arrow::datatypes::TimeUnit::Nanosecond => Ok(Value::Time64(
                Time64Unit::Nanosecond,
                array
                    .as_any()
                    .downcast_ref::<Time64NanosecondArray>()
                    .ok_or_else(|| malformed("Arrow type mismatch"))?
                    .value(index),
            )),
            _ => Err(malformed("invalid Time64 unit")),
        },
        DataType::Timestamp(unit, timezone) => {
            timestamp_value(array, index, *unit, timezone.clone())
        }
        DataType::Duration(unit) => duration_value(array, index, *unit),
        DataType::Decimal128(precision, scale) => Ok(Value::Decimal128(Decimal128 {
            precision: *precision,
            scale: *scale,
            value: array
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_le_bytes(),
        })),
        DataType::Decimal256(precision, scale) => Ok(Value::Decimal256(Decimal256 {
            precision: *precision,
            scale: *scale,
            value: array
                .as_any()
                .downcast_ref::<Decimal256Array>()
                .ok_or_else(|| malformed("Arrow type mismatch"))?
                .value(index)
                .to_le_bytes(),
        })),
        DataType::Null => Ok(Value::Null),
        _ => Err(malformed("unsupported Arrow value type")),
    }
}

/// Reads one parameter-bag scalar (union children 16–21) from an Arrow array
/// slot. Exposed crate-wide for the A-2 tree codec's value-column reader.
pub(crate) fn parameter_value(id: usize, array: &ArrayRef, index: usize) -> Result<Value> {
    let bytes = if id == 18 {
        array
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or_else(|| malformed("Arrow parameter bag type mismatch"))?
            .value(index)
            .to_vec()
    } else {
        array
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| malformed("Arrow parameter bag type mismatch"))?
            .value(index)
            .to_vec()
    };
    decode_parameter(id, &bytes)
}

fn decode_parameter(id: usize, bytes: &[u8]) -> Result<Value> {
    let mut cursor = ParameterCursor { bytes, offset: 0 };
    match id {
        16 => {
            let unit = match cursor.take(1)?[0] {
                0 => Time32Unit::Second,
                1 => Time32Unit::Millisecond,
                _ => return Err(malformed("invalid Time32 parameter bag")),
            };
            Ok(Value::Time32(
                unit,
                i32::from_le_bytes(cursor.take(4)?.try_into().unwrap()),
            ))
        }
        17 => {
            let unit = match cursor.take(1)?[0] {
                0 => Time64Unit::Microsecond,
                1 => Time64Unit::Nanosecond,
                _ => return Err(malformed("invalid Time64 parameter bag")),
            };
            Ok(Value::Time64(
                unit,
                i64::from_le_bytes(cursor.take(8)?.try_into().unwrap()),
            ))
        }
        18 => {
            let unit = match cursor.take(1)?[0] {
                0 => TimestampUnit::Second,
                1 => TimestampUnit::Millisecond,
                2 => TimestampUnit::Microsecond,
                3 => TimestampUnit::Nanosecond,
                _ => return Err(malformed("invalid Timestamp parameter bag")),
            };
            let tz_len = cursor.take(1)?[0] as usize;
            let tz = if tz_len == 0 {
                None
            } else {
                Some(
                    String::from_utf8(cursor.take(tz_len)?.to_vec())
                        .map_err(|_| malformed("invalid timezone"))?,
                )
            };
            Ok(Value::Timestamp(
                unit,
                tz,
                i64::from_le_bytes(cursor.take(8)?.try_into().unwrap()),
            ))
        }
        19 => {
            let unit = match cursor.take(1)?[0] {
                0 => DurationUnit::Second,
                1 => DurationUnit::Millisecond,
                2 => DurationUnit::Microsecond,
                3 => DurationUnit::Nanosecond,
                _ => return Err(malformed("invalid Duration parameter bag")),
            };
            Ok(Value::Duration(
                unit,
                i64::from_le_bytes(cursor.take(8)?.try_into().unwrap()),
            ))
        }
        20 => {
            let precision = cursor.take(1)?[0];
            let scale = cursor.take(1)?[0] as i8;
            let mut value = [0; 16];
            value.copy_from_slice(cursor.take(16)?);
            Ok(Value::Decimal128(Decimal128 {
                precision,
                scale,
                value,
            }))
        }
        21 => {
            let precision = cursor.take(1)?[0];
            let scale = cursor.take(1)?[0] as i8;
            let mut value = [0; 32];
            value.copy_from_slice(cursor.take(32)?);
            Ok(Value::Decimal256(Decimal256 {
                precision,
                scale,
                value,
            }))
        }
        _ => Err(malformed("invalid parameter bag type id")),
    }
}

struct ParameterCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ParameterCursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| malformed("parameter bag length overflow"))?;
        if end > self.bytes.len() {
            return Err(malformed("truncated parameter bag"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
}

fn timestamp_unit(unit: TimestampUnit) -> arrow::datatypes::TimeUnit {
    match unit {
        TimestampUnit::Second => arrow::datatypes::TimeUnit::Second,
        TimestampUnit::Millisecond => arrow::datatypes::TimeUnit::Millisecond,
        TimestampUnit::Microsecond => arrow::datatypes::TimeUnit::Microsecond,
        TimestampUnit::Nanosecond => arrow::datatypes::TimeUnit::Nanosecond,
    }
}

fn duration_unit(unit: DurationUnit) -> arrow::datatypes::TimeUnit {
    match unit {
        DurationUnit::Second => arrow::datatypes::TimeUnit::Second,
        DurationUnit::Millisecond => arrow::datatypes::TimeUnit::Millisecond,
        DurationUnit::Microsecond => arrow::datatypes::TimeUnit::Microsecond,
        DurationUnit::Nanosecond => arrow::datatypes::TimeUnit::Nanosecond,
    }
}

fn timestamp_array(
    values: &[Value],
    unit: TimestampUnit,
    timezone: Option<String>,
) -> Result<ArrayRef> {
    macro_rules! make {
        ($ty:ty) => {{
            let data = values
                .iter()
                .map(|value| match value {
                    Value::Timestamp(_, _, value) => Some(*value),
                    Value::Null => None,
                    _ => None,
                })
                .collect::<Vec<_>>();
            let array = <$ty>::from(data).with_timezone_opt(timezone.map(Arc::<str>::from));
            Arc::new(array) as ArrayRef
        }};
    }
    Ok(match unit {
        TimestampUnit::Second => make!(TimestampSecondArray),
        TimestampUnit::Millisecond => make!(TimestampMillisecondArray),
        TimestampUnit::Microsecond => make!(TimestampMicrosecondArray),
        TimestampUnit::Nanosecond => make!(TimestampNanosecondArray),
    })
}

fn duration_array(values: &[Value], unit: DurationUnit) -> Result<ArrayRef> {
    macro_rules! make {
        ($ty:ty) => {
            Arc::new(<$ty>::from(
                values
                    .iter()
                    .map(|value| match value {
                        Value::Duration(_, value) => Some(*value),
                        Value::Null => None,
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )) as ArrayRef
        };
    }
    Ok(match unit {
        DurationUnit::Second => make!(DurationSecondArray),
        DurationUnit::Millisecond => make!(DurationMillisecondArray),
        DurationUnit::Microsecond => make!(DurationMicrosecondArray),
        DurationUnit::Nanosecond => make!(DurationNanosecondArray),
    })
}

fn decimal128_array(values: &[Value], precision: u8, scale: i8) -> Result<ArrayRef> {
    let array = Decimal128Array::from(
        values
            .iter()
            .map(|value| match value {
                Value::Decimal128(value) => Some(i128::from_le_bytes(value.value)),
                Value::Null => None,
                _ => None,
            })
            .collect::<Vec<_>>(),
    )
    .with_precision_and_scale(precision, scale)
    .map_err(ipc_error)?;
    Ok(Arc::new(array))
}

fn decimal256_array(values: &[Value], precision: u8, scale: i8) -> Result<ArrayRef> {
    let array = Decimal256Array::from(
        values
            .iter()
            .map(|value| match value {
                Value::Decimal256(value) => Some(i256_from_le(value.value)),
                Value::Null => None,
                _ => None,
            })
            .collect::<Vec<_>>(),
    )
    .with_precision_and_scale(precision, scale)
    .map_err(ipc_error)?;
    Ok(Arc::new(array))
}

fn i256_from_le(value: [u8; 32]) -> i256 {
    i256::from_le_bytes(value)
}

fn timestamp_value(
    array: &ArrayRef,
    index: usize,
    unit: arrow::datatypes::TimeUnit,
    timezone: Option<Arc<str>>,
) -> Result<Value> {
    let value = match unit {
        arrow::datatypes::TimeUnit::Second => array
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .ok_or_else(|| malformed("Arrow timestamp type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Millisecond => array
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .ok_or_else(|| malformed("Arrow timestamp type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Microsecond => array
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .ok_or_else(|| malformed("Arrow timestamp type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Nanosecond => array
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .ok_or_else(|| malformed("Arrow timestamp type mismatch"))?
            .value(index),
    };
    let unit = match unit {
        arrow::datatypes::TimeUnit::Second => TimestampUnit::Second,
        arrow::datatypes::TimeUnit::Millisecond => TimestampUnit::Millisecond,
        arrow::datatypes::TimeUnit::Microsecond => TimestampUnit::Microsecond,
        arrow::datatypes::TimeUnit::Nanosecond => TimestampUnit::Nanosecond,
    };
    Ok(Value::Timestamp(
        unit,
        timezone.map(|value| value.to_string()),
        value,
    ))
}

fn duration_value(
    array: &ArrayRef,
    index: usize,
    unit: arrow::datatypes::TimeUnit,
) -> Result<Value> {
    let value = match unit {
        arrow::datatypes::TimeUnit::Second => array
            .as_any()
            .downcast_ref::<DurationSecondArray>()
            .ok_or_else(|| malformed("Arrow duration type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Millisecond => array
            .as_any()
            .downcast_ref::<DurationMillisecondArray>()
            .ok_or_else(|| malformed("Arrow duration type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Microsecond => array
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .ok_or_else(|| malformed("Arrow duration type mismatch"))?
            .value(index),
        arrow::datatypes::TimeUnit::Nanosecond => array
            .as_any()
            .downcast_ref::<DurationNanosecondArray>()
            .ok_or_else(|| malformed("Arrow duration type mismatch"))?
            .value(index),
    };
    let unit = match unit {
        arrow::datatypes::TimeUnit::Second => DurationUnit::Second,
        arrow::datatypes::TimeUnit::Millisecond => DurationUnit::Millisecond,
        arrow::datatypes::TimeUnit::Microsecond => DurationUnit::Microsecond,
        arrow::datatypes::TimeUnit::Nanosecond => DurationUnit::Nanosecond,
    };
    Ok(Value::Duration(unit, value))
}

fn malformed(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}
