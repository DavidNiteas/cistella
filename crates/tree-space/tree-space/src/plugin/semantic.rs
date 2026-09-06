//! M2 semantic identity (plugin overhaul `01-目标与设计.md` §4-4): a tiny
//! non-Arrow, self-describing byte-stream fingerprint, independent of Arrow
//! metadata.
//!
//! PL-2 S1 only lands the fingerprint machinery plus the frozen golden bytes
//! (`tests/p_l_2_semantic.rs`, goldens 28–30); the Arrow plugins' `ref_id` /
//! `tree_id` formulas switch to these fingerprints in S2. Semantic values keep
//! a minimal closed family (the Arrow55 surface is all that M2 needs — the
//! family grows by explicit decision, never silently).

use crate::block::{BlockKind, Kv, RefId, Sequence, Value};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::hash::canonical_digest;
use crate::ipc::decode_batch;
use crate::tree::codec::{
    ImageContent, ImageField, ImageView, ImageViewInput, Locator, ParentKind, TreeImage,
    order_children,
};
use arrow::array::{
    Array, ArrayRef, FixedSizeBinaryArray, Float16Array, IntervalDayTimeArray,
    IntervalMonthDayNanoArray, IntervalYearMonthArray, MapArray, StructArray, UnionArray,
};
use arrow::datatypes::{DataType, Field, IntervalUnit, TimeUnit};
use arrow::record_batch::RecordBatch;
use std::cmp::Ordering;

/// A scalar semantic value: the minimal self-describing byte stream that
/// represents one scalar in a fingerprint, without relying on Arrow metadata.
///
/// The family is deliberately minimal for M2 (the Arrow55 surface). Encoding
/// is deterministic: `tag + u32 BE length prefix + payload` (a stream can be
/// decoded back exactly, and NaN / -0.0 payloads are canonicalized).
#[derive(Clone, Debug)]
pub enum SemanticValue {
    /// The null scalar.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer.
    Int(i64),
    /// An unsigned 64-bit integer.
    UInt(u64),
    /// A 64-bit IEEE float (`-0.0`/NaN canonicalized when encoding).
    Float(f64),
    /// Arbitrary bytes.
    Bytes(Vec<u8>),
    /// A UTF-8 string.
    Str(String),
    /// An ordered list of semantic values.
    List(Vec<SemanticValue>),
    // [on demand] more scalar families — M2 lands the minimum set that the
    // Arrow55 surface needs; extending the family is a separate decision.
}

// `Float(f64)` disables the derived total-order impls, so Eq/Ord are written
// by hand; floats compare by IEEE bits (deterministic total order, NaN-safe).

impl PartialEq for SemanticValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::UInt(a), Self::UInt(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for SemanticValue {}

impl PartialOrd for SemanticValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemanticValue {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::UInt(a), Self::UInt(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.total_cmp(b),
            (Self::Bytes(a), Self::Bytes(b)) => a.cmp(b),
            (Self::Str(a), Self::Str(b)) => a.cmp(b),
            (Self::List(a), Self::List(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }
}

fn rank(value: &SemanticValue) -> u8 {
    match value {
        SemanticValue::Null => 0,
        SemanticValue::Bool(_) => 1,
        SemanticValue::Int(_) => 2,
        SemanticValue::UInt(_) => 3,
        SemanticValue::Float(_) => 4,
        SemanticValue::Bytes(_) => 5,
        SemanticValue::Str(_) => 6,
        SemanticValue::List(_) => 7,
    }
}

/// The canonical NaN bit pattern used by [`SemanticValue::encode`] (the
/// platform-independent quiet NaN, frozen for byte determinism).
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

impl SemanticValue {
    /// Encodes this value as `[tag: u8][payload_len: u32 BE][payload]`.
    ///
    /// Deterministic by construction: `-0.0` and `+0.0` both encode as `+0.0`
    /// (they compare equal numerically) and every NaN payload encodes as the
    /// canonical NaN bit pattern, so identical floating-point *values* always
    /// produce identical bytes. The length prefix makes the stream
    /// self-describing: [`Self::decode`] walks it back to the exact value.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let tag = match self {
            Self::Null => 0x00,
            Self::Bool(value) => {
                body.push(u8::from(*value));
                0x01
            }
            Self::Int(value) => {
                body.extend_from_slice(&value.to_be_bytes());
                0x02
            }
            Self::UInt(value) => {
                body.extend_from_slice(&value.to_be_bytes());
                0x03
            }
            Self::Float(value) => {
                let normalized = if *value == 0.0 {
                    0.0
                } else if value.is_nan() {
                    f64::from_bits(CANONICAL_NAN_BITS)
                } else {
                    *value
                };
                body.extend_from_slice(&normalized.to_be_bytes());
                0x04
            }
            Self::Bytes(value) => {
                body.extend_from_slice(value);
                0x05
            }
            Self::Str(value) => {
                body.extend_from_slice(value.as_bytes());
                0x06
            }
            Self::List(values) => {
                for element in values {
                    body.extend_from_slice(&element.encode());
                }
                0x07
            }
        };
        let mut out = Vec::with_capacity(5 + body.len());
        out.push(tag);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Decodes one value from the front of a stream.
    ///
    /// Returns the decoded value together with the number of bytes consumed;
    /// truncated or malformed streams yield [`ErrorCode::PayloadMalformed`].
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize)> {
        let (tag, rest) = bytes
            .split_first()
            .ok_or_else(|| malformed("empty stream"))?;
        let len = u32::from_be_bytes(
            rest.get(..4)
                .ok_or_else(|| malformed("missing length prefix"))?
                .try_into()
                .unwrap(),
        ) as usize;
        let payload = rest
            .get(4..4 + len)
            .ok_or_else(|| malformed("payload truncated"))?;
        let consumed = 1 + 4 + len;
        let value = match *tag {
            0x00 => {
                if len != 0 {
                    return Err(malformed("null payload must be empty"));
                }
                Self::Null
            }
            0x01 => {
                if len != 1 {
                    return Err(malformed("bool payload must be one byte"));
                }
                Self::Bool(payload[0] != 0)
            }
            0x02 => {
                if len != 8 {
                    return Err(malformed("int payload must be eight bytes"));
                }
                Self::Int(i64::from_be_bytes(payload.try_into().unwrap()))
            }
            0x03 => {
                if len != 8 {
                    return Err(malformed("uint payload must be eight bytes"));
                }
                Self::UInt(u64::from_be_bytes(payload.try_into().unwrap()))
            }
            0x04 => {
                if len != 8 {
                    return Err(malformed("float payload must be eight bytes"));
                }
                Self::Float(f64::from_be_bytes(payload.try_into().unwrap()))
            }
            0x05 => Self::Bytes(payload.to_vec()),
            0x06 => {
                let text = std::str::from_utf8(payload).map_err(|_| malformed("utf8 payload"))?;
                Self::Str(text.to_owned())
            }
            0x07 => {
                let mut values = Vec::new();
                let mut cursor = 0;
                while cursor < len {
                    let (element, used) = Self::decode(&payload[cursor..])?;
                    values.push(element);
                    cursor += used;
                }
                Self::List(values)
            }
            other => return Err(malformed(&format!("unknown tag {other:#04x}"))),
        };
        Ok((value, consumed))
    }
}

fn malformed(message: &str) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

/// A coarse logical-type fingerprint of a table's shape (02 §6.3):
/// per-field name / logical-type tag / nullability / nested shape, **without
/// any Arrow metadata** (schema and field metadata never enter the bytes).
///
/// The tag mapping is the authoritative arrow-to-semantic type table; see the
/// type matrix in the module docs of `plugin/semantic.rs` (reported at S1
/// close, drift from it re-opens the goldens).
pub fn semantic_table(batch: &RecordBatch) -> Vec<u8> {
    let mut out = Vec::new();
    for field in batch.schema().fields() {
        encode_field(&mut out, field.as_ref());
    }
    out
}

/// Encodes one named field: `name + nullability + type shape` (children of the
/// shape recursively for nested types — Struct/Union/Map children go through
/// [`encode_field`] again, so their names stay in the fingerprint; List-family
/// element types are anonymous [`encode_shape`] values).
fn encode_field(out: &mut Vec<u8>, field: &Field) {
    push_str(out, field.name());
    out.push(u8::from(field.is_nullable()));
    encode_shape(out, field.data_type());
}

/// Encodes the logical type shape of a data type (used directly for anonymous
/// nested elements, after [`encode_field`] for named fields). Dictionary and
/// RunEndEncoded are physical encodings, so they dereference to their value
/// types — the key to cross-encoding identity.
fn encode_shape(out: &mut Vec<u8>, data_type: &DataType) {
    match data_type {
        DataType::Dictionary(_, value) => {
            encode_shape(out, value);
            return;
        }
        DataType::RunEndEncoded(_, value) => {
            encode_shape(out, value.data_type());
            return;
        }
        _ => {}
    }
    out.push(type_tag(data_type));
    match data_type {
        DataType::FixedSizeBinary(size) => out.extend_from_slice(&size.to_be_bytes()),
        DataType::Timestamp(unit, timezone) => {
            push_unit(out, *unit);
            match timezone.as_deref() {
                None => out.push(0),
                Some(timezone) => {
                    out.push(1);
                    push_str(out, timezone);
                }
            }
        }
        DataType::Time32(unit) | DataType::Time64(unit) | DataType::Duration(unit) => {
            push_unit(out, *unit)
        }
        DataType::Interval(unit) => out.push(match unit {
            IntervalUnit::YearMonth => 0,
            IntervalUnit::DayTime => 1,
            IntervalUnit::MonthDayNano => 2,
        }),
        DataType::Decimal128(precision, scale) | DataType::Decimal256(precision, scale) => {
            out.push(*precision);
            out.push(*scale as u8);
        }
        DataType::Struct(fields) => {
            for field in fields.iter() {
                encode_field(out, field.as_ref());
            }
        }
        DataType::Union(fields, mode) => {
            // The sparse/dense union mode is a physical layout detail: it does
            // not enter the logical fingerprint.
            let _ = mode;
            for (_, field) in fields.iter() {
                encode_field(out, field.as_ref());
            }
        }
        DataType::List(field)
        | DataType::LargeList(field)
        | DataType::ListView(field)
        | DataType::LargeListView(field) => {
            encode_shape(out, field.data_type());
        }
        DataType::FixedSizeList(field, size) => {
            out.extend_from_slice(&size.to_be_bytes());
            encode_shape(out, field.data_type());
        }
        DataType::Map(field, key_sorted) => {
            out.push(u8::from(*key_sorted));
            // A map entry field is guaranteed by arrow to be a struct of
            // (key, value) children; encode each entry field in order.
            if let DataType::Struct(entries) = field.data_type() {
                for entry in entries.iter() {
                    encode_field(out, entry.as_ref());
                }
            }
        }
        _ => {}
    }
}

/// The arrow DataType → semantic tag matrix (frozen by golden 29).
///
/// Explicit normalization: `Utf8`/`LargeUtf8`/`Utf8View` → `utf8`;
/// `Binary`/`LargeBinary`/`BinaryView` → `binary`;
/// `List`/`LargeList`/`ListView`/`LargeListView` → `list` (index-width and
/// view encodings are physical). Widths within the integer/float/date families
/// stay distinct (`Int8` ≠ `Int64`, `Float16` ≠ `Float64`, `Date32` ≠
/// `Date64`) — those are logical differences. Decimal precision/scale,
/// timestamp unit/timezone, fixed-size lengths, interval unit and map
/// key-sortedness are logical properties and are encoded as extra bytes
/// (see [`encode_shape`]).
fn type_tag(data_type: &DataType) -> u8 {
    match data_type {
        DataType::Null => 0x00,
        DataType::Boolean => 0x01,
        DataType::Int8 => 0x02,
        DataType::Int16 => 0x03,
        DataType::Int32 => 0x04,
        DataType::Int64 => 0x05,
        DataType::UInt8 => 0x06,
        DataType::UInt16 => 0x07,
        DataType::UInt32 => 0x08,
        DataType::UInt64 => 0x09,
        DataType::Float16 => 0x0a,
        DataType::Float32 => 0x0b,
        DataType::Float64 => 0x0c,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => 0x0d,
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => 0x0e,
        DataType::FixedSizeBinary(_) => 0x0f,
        DataType::Date32 => 0x10,
        DataType::Date64 => 0x11,
        DataType::Time32(_) => 0x12,
        DataType::Time64(_) => 0x13,
        DataType::Duration(_) => 0x14,
        DataType::Interval(_) => 0x15,
        DataType::Timestamp(_, _) => 0x16,
        DataType::Decimal128(_, _) => 0x17,
        DataType::Decimal256(_, _) => 0x18,
        DataType::List(_)
        | DataType::LargeList(_)
        | DataType::ListView(_)
        | DataType::LargeListView(_) => 0x19,
        DataType::FixedSizeList(_, _) => 0x1a,
        DataType::Struct(_) => 0x1b,
        DataType::Union(_, _) => 0x1c,
        DataType::Map(_, _) => 0x1d,
        DataType::Dictionary(_, _) | DataType::RunEndEncoded(_, _) => unreachable!(
            "Dictionary / RunEndEncoded dereference to their value types before tagging"
        ),
    }
}

fn push_unit(out: &mut Vec<u8>, unit: TimeUnit) {
    out.push(match unit {
        TimeUnit::Second => 0,
        TimeUnit::Millisecond => 1,
        TimeUnit::Microsecond => 2,
        TimeUnit::Nanosecond => 3,
    });
}

fn push_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// A deterministic walk of a tree image (02 §6.3): structural shape + leaf
/// semantic fingerprints, reusing the codec's [`order_children`] ordering so
/// that the fingerprint is independent of the physical row order.
///
/// Every node contributes a byte-count prefix; leaf scalars are encoded with
/// [`SemanticValue::encode`] and refs as raw identity bytes. The walk works on
/// canonical tree images only (the codec validates locator uniformity /
/// occupancy before `semantic_tree` is ever consulted through `TreePlugin`).
pub fn semantic_tree(image: &TreeImage) -> Vec<u8> {
    let mut out = Vec::new();
    let root = order_children(image.children(), ParentKind::Node)
        .expect("semantic_tree walks canonical tree images only");
    encode_children(&mut out, &root);
    out
}

fn encode_children<'a>(out: &mut Vec<u8>, ordered: &[&'a ImageField]) {
    out.extend_from_slice(&(ordered.len() as u32).to_be_bytes());
    for field in ordered {
        encode_field_content(out, field);
    }
}

fn encode_field_content(out: &mut Vec<u8>, field: &ImageField) {
    match &field.locator {
        Locator::Positioned => out.push(0x00),
        Locator::Named(name) => {
            out.push(0x01);
            push_str(out, name);
        }
        Locator::Keyed(key) => {
            out.push(0x02);
            out.extend_from_slice(key);
        }
    }
    match &field.content {
        ImageContent::Node(children) => {
            out.push(0x10);
            let ordered = order_children(children, ParentKind::Node)
                .expect("semantic_tree walks canonical tree images only");
            encode_children(out, &ordered);
        }
        ImageContent::Inline(value) => {
            out.push(0x11);
            out.extend_from_slice(&semantic_scalar(value).encode());
        }
        ImageContent::Ref(id) => {
            out.push(0x12);
            out.extend_from_slice(&id.as_bytes());
        }
        ImageContent::ChunkGroup { kind, children } => {
            out.push(0x13);
            push_str(out, kind.as_str().as_ref());
            let ordered = order_children(children, ParentKind::Group)
                .expect("semantic_tree walks canonical tree images only");
            encode_children(out, &ordered);
        }
        ImageContent::ChunkEntry(id) => {
            out.push(0x14);
            out.extend_from_slice(&id.as_bytes());
        }
        ImageContent::View(view) => {
            out.push(0x15);
            encode_view(out, view);
        }
    }
}

fn encode_view(out: &mut Vec<u8>, view: &ImageView) {
    push_str(out, &view.combinator);
    out.extend_from_slice(&(view.inputs.len() as u32).to_be_bytes());
    for input in &view.inputs {
        match input {
            ImageViewInput::Ref(id) => {
                out.push(0x00);
                out.extend_from_slice(&id.as_bytes());
            }
            ImageViewInput::View(nested) => {
                out.push(0x01);
                encode_view(out, nested);
            }
        }
    }
    // View parameters are a logical name → value map: sort by name so that the
    // insertion order never changes the fingerprint.
    let mut params = view.params.clone();
    params.sort_by(|(a, _), (b, _)| a.cmp(b));
    out.extend_from_slice(&(params.len() as u32).to_be_bytes());
    for (name, value) in params {
        push_str(out, &name);
        out.extend_from_slice(&semantic_scalar(&value).encode());
    }
}

/// Maps an inline TB scalar ([`Value`]) onto the minimal semantic family.
///
/// The integer/boolean/string/bytes/binary families map 1:1; the temporal and
/// decimal families keep their logical metadata as a tagged list (unit / tz /
/// precision·scale), because the same raw number with a different unit is a
/// different logical value. IEEE bit patterns (`F32`/`F64`) convert to the
/// numeric `Float` value; encoding canonicalizes `-0.0`/NaN afterwards.
fn semantic_scalar(value: &Value) -> SemanticValue {
    match value {
        Value::Null => SemanticValue::Null,
        Value::Bool(value) => SemanticValue::Bool(*value),
        Value::I8(value) => SemanticValue::Int(*value as i64),
        Value::I16(value) => SemanticValue::Int(*value as i64),
        Value::I32(value) => SemanticValue::Int(*value as i64),
        Value::I64(value) => SemanticValue::Int(*value),
        Value::U8(value) => SemanticValue::UInt(*value as u64),
        Value::U16(value) => SemanticValue::UInt(*value as u64),
        Value::U32(value) => SemanticValue::UInt(*value as u64),
        Value::U64(value) => SemanticValue::UInt(*value),
        Value::F32(bits) => SemanticValue::Float(f32::from_bits(*bits) as f64),
        Value::F64(bits) => SemanticValue::Float(f64::from_bits(*bits)),
        Value::Utf8(value) => SemanticValue::Str(value.clone()),
        Value::Binary(value) => SemanticValue::Bytes(value.clone()),
        Value::Date32(value) => SemanticValue::List(vec![
            SemanticValue::Str("date32".into()),
            SemanticValue::Int(*value as i64),
        ]),
        Value::Date64(value) => SemanticValue::List(vec![
            SemanticValue::Str("date64".into()),
            SemanticValue::Int(*value),
        ]),
        Value::Time32(unit, value) => SemanticValue::List(vec![
            SemanticValue::Str("time32".into()),
            SemanticValue::UInt(*unit as u8 as u64),
            SemanticValue::Int(*value as i64),
        ]),
        Value::Time64(unit, value) => SemanticValue::List(vec![
            SemanticValue::Str("time64".into()),
            SemanticValue::UInt(*unit as u8 as u64),
            SemanticValue::Int(*value),
        ]),
        Value::Timestamp(unit, timezone, value) => SemanticValue::List(vec![
            SemanticValue::Str("timestamp".into()),
            SemanticValue::UInt(*unit as u8 as u64),
            match timezone {
                Some(timezone) => SemanticValue::Str(timezone.clone()),
                None => SemanticValue::Null,
            },
            SemanticValue::Int(*value),
        ]),
        Value::Duration(unit, value) => SemanticValue::List(vec![
            SemanticValue::Str("duration".into()),
            SemanticValue::UInt(*unit as u8 as u64),
            SemanticValue::Int(*value),
        ]),
        Value::Decimal128(value) => SemanticValue::List(vec![
            SemanticValue::Str("decimal128".into()),
            SemanticValue::UInt(value.precision as u64),
            SemanticValue::Int(value.scale as i64),
            SemanticValue::Bytes(value.value.to_vec()),
        ]),
        Value::Decimal256(value) => SemanticValue::List(vec![
            SemanticValue::Str("decimal256".into()),
            SemanticValue::UInt(value.precision as u64),
            SemanticValue::Int(value.scale as i64),
            SemanticValue::Bytes(value.value.to_vec()),
        ]),
    }
}

// ---------------------------------------------------------------------------
// Block semantic identity (02 §6.4, S2): `RefId = hash(domain, [semantic_*])`
// ---------------------------------------------------------------------------
//
// The block identity formula switches from the M1 canonical-byte formula to
// the semantic formula at PL-2 S2 (01 §4-4):
//
// - `Table` blocks hash the resolved batch's logical shape plus its per-column
//   data values (the identity stays content-addressed — two tables with the
//   same schema but different data are different blocks — while the logical
//   values are physical-encoding independent, so the same logical table
//   identities identically across Arrow IPC / Arrow Parquet / a future Polars
//   encoding);
// - `Blob` blocks hash their byte payload as a `Bytes` value;
// - `Sequence` / `Kv` blocks hash their decoded element structures;
// - registered (`Named`) blocks carry no declarable generic semantics, so they
//   hash their raw payload as a byte value (S2 deviation, reported).
//
// The domain tag stays the existing `kind.domain()` (`tb-block-*` family); the
// address formula (`block_blob_address`) is untouched.

/// The semantic value of a block's canonical payload: the fingerprint
/// component of the block identity formula.
///
/// `RefId = canonical_digest(kind.domain(), [value.encode()])`. Decode failures
/// (corrupt / non-canonical payloads) fall back to the raw payload as a byte
/// value, which keeps the identity deterministic on every input.
pub fn semantic_block_value(kind: &BlockKind, payload: &[u8]) -> SemanticValue {
    match kind {
        BlockKind::Table => match decode_batch(payload) {
            Ok(batch) => semantic_table_value(&batch).unwrap_or_else(|| byte_value(payload)),
            Err(_) => byte_value(payload),
        },
        BlockKind::Sequence => match Sequence::decode(payload) {
            Ok(sequence) => SemanticValue::List(
                sequence
                    .values()
                    .iter()
                    .map(semantic_scalar)
                    .collect::<Vec<_>>(),
            ),
            Err(_) => byte_value(payload),
        },
        BlockKind::Kv => match Kv::decode(payload) {
            Ok(kv) => SemanticValue::List(
                kv.entries()
                    .iter()
                    .map(|(key, value)| {
                        SemanticValue::List(vec![semantic_scalar(key), semantic_scalar(value)])
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(_) => byte_value(payload),
        },
        BlockKind::Blob => match crate::block::arrow::decode_blob(payload) {
            Ok(bytes) => SemanticValue::Bytes(bytes),
            Err(_) => byte_value(payload),
        },
        BlockKind::Named(_) => byte_value(payload),
    }
}

/// Computes the semantic `RefId` of a kind and its canonical payload (the M2
/// formula, 02 §6.4). `crate::block::block_ref_id` forwards here.
pub fn semantic_block_ref_id(kind: BlockKind, payload: &[u8]) -> RefId {
    RefId(canonical_digest(
        &kind.domain(),
        [semantic_block_value(&kind, payload).encode()],
    ))
}

/// The semantic value of a table batch: logical shape (`semantic_table`) plus
/// each column's ordered data values.
///
/// `None` when a column carries a physical type the semantic walker cannot
/// resolve (the raw-payload byte fallback then keeps the identity
/// deterministic and content-addressed).
pub fn semantic_table_value(batch: &RecordBatch) -> Option<SemanticValue> {
    let mut parts = vec![SemanticValue::Bytes(semantic_table(batch))];
    for column in batch.columns() {
        parts.push(SemanticValue::List(semantic_column(column).ok()?));
    }
    Some(SemanticValue::List(parts))
}

fn byte_value(payload: &[u8]) -> SemanticValue {
    SemanticValue::Bytes(payload.to_vec())
}

/// Converts one arrow column into its ordered row values (rows are ordered, so
/// the semantic fingerprint is row-order sensitive for tables — matching the
/// leaf-value sensitivity of `semantic_tree`).
fn semantic_column(array: &ArrayRef) -> Result<Vec<SemanticValue>> {
    let mut values = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        values.push(semantic_slot(array, index)?);
    }
    Ok(values)
}

/// Reads one logical slot of an arrow array into a [`SemanticValue`].
///
/// Physical encodings (dictionary, run-end-encoded) dereference to their value
/// type first, so the fingerprint is independent of the physical layout — the
/// key to cross-encoding identity. Scalar families follow the frozen
/// [`semantic_scalar`] mapping; nested types recurse.
fn semantic_slot(array: &ArrayRef, index: usize) -> Result<SemanticValue> {
    if array.is_null(index) {
        return Ok(SemanticValue::Null);
    }
    match array.data_type() {
        DataType::Dictionary(_, value) => {
            let decoded = cast_to(array, value.as_ref())?;
            semantic_slot(&decoded, index)
        }
        DataType::RunEndEncoded(_, value) => {
            let decoded = cast_to(array, value.data_type())?;
            semantic_slot(&decoded, index)
        }
        DataType::Float16 => Ok(SemanticValue::Float(
            array
                .as_any()
                .downcast_ref::<Float16Array>()
                .ok_or_else(|| type_error("Float16 array"))?
                .value(index)
                .to_f32() as f64,
        )),
        DataType::FixedSizeBinary(_) => Ok(SemanticValue::Bytes(
            array
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .ok_or_else(|| type_error("FixedSizeBinary array"))?
                .value(index)
                .to_vec(),
        )),
        DataType::Interval(unit) => interval_value(array, index, *unit),
        DataType::List(_) => list_value::<i32>(array, index),
        DataType::LargeList(_) => list_value::<i64>(array, index),
        DataType::ListView(_) => list_view_value::<i32>(array, index),
        DataType::LargeListView(_) => list_view_value::<i64>(array, index),
        DataType::FixedSizeList(_, _) => fixed_list_value(array, index),
        DataType::Struct(_) => struct_value(array, index),
        DataType::Union(_, _) => union_value(array, index),
        DataType::Map(_, _) => map_value(array, index),
        // Scalar families route through the canonical Value bridge (covers
        // every tag of `block::Value`, including the parameterized temporal /
        // decimal kinds), keeping the frozen scalar mapping.
        _ => {
            let value = crate::block::arrow::array_value(array, index)?;
            Ok(semantic_scalar(&value))
        }
    }
}

fn cast_to(array: &ArrayRef, target: &DataType) -> Result<ArrayRef> {
    arrow::compute::cast(array, target).map_err(|error| {
        TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "semantic dereference cast failed",
        )
        .with_context("detail", error.to_string())
    })
}

fn type_error(expected: &str) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        format!("semantic slot expected {expected}"),
    )
}

fn list_value<Offset: arrow::array::OffsetSizeTrait>(
    array: &ArrayRef,
    index: usize,
) -> Result<SemanticValue> {
    let list = array
        .as_any()
        .downcast_ref::<arrow::array::GenericListArray<Offset>>()
        .ok_or_else(|| type_error("list array"))?;
    let child = list.value(index);
    Ok(SemanticValue::List(semantic_column(&child)?))
}

fn list_view_value<Offset: arrow::array::OffsetSizeTrait>(
    array: &ArrayRef,
    index: usize,
) -> Result<SemanticValue> {
    let view = array
        .as_any()
        .downcast_ref::<arrow::array::GenericListViewArray<Offset>>()
        .ok_or_else(|| type_error("list-view array"))?;
    let child = view.value(index);
    Ok(SemanticValue::List(semantic_column(&child)?))
}

fn fixed_list_value(array: &ArrayRef, index: usize) -> Result<SemanticValue> {
    let list = array
        .as_any()
        .downcast_ref::<arrow::array::FixedSizeListArray>()
        .ok_or_else(|| type_error("fixed-size list array"))?;
    let child = list.value(index);
    Ok(SemanticValue::List(semantic_column(&child)?))
}

fn struct_value(array: &ArrayRef, index: usize) -> Result<SemanticValue> {
    let struct_array = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| type_error("struct array"))?;
    let mut fields = Vec::with_capacity(struct_array.num_columns());
    for column in struct_array.columns() {
        fields.push(semantic_slot(column, index)?);
    }
    Ok(SemanticValue::List(fields))
}

fn union_value(array: &ArrayRef, index: usize) -> Result<SemanticValue> {
    let union = array
        .as_any()
        .downcast_ref::<UnionArray>()
        .ok_or_else(|| type_error("union array"))?;
    let type_id = union.type_id(index);
    let physical = union.value_offset(index) as usize;
    let child = union.child(type_id);
    semantic_slot(child, physical)
}

fn map_value(array: &ArrayRef, index: usize) -> Result<SemanticValue> {
    let map = array
        .as_any()
        .downcast_ref::<MapArray>()
        .ok_or_else(|| type_error("map array"))?;
    let offsets = map.value_offsets();
    let offset = offsets[index] as usize;
    let len = (offsets[index + 1] - offsets[index]) as usize;
    let keys = map.keys();
    let values = map.values();
    let mut entries = Vec::with_capacity(len);
    for entry in 0..len {
        entries.push(SemanticValue::List(vec![
            semantic_slot(keys, offset + entry)?,
            semantic_slot(values, offset + entry)?,
        ]));
    }
    Ok(SemanticValue::List(entries))
}

fn interval_value(array: &ArrayRef, index: usize, unit: IntervalUnit) -> Result<SemanticValue> {
    match unit {
        IntervalUnit::YearMonth => {
            let value = array
                .as_any()
                .downcast_ref::<IntervalYearMonthArray>()
                .ok_or_else(|| type_error("interval year-month array"))?
                .value(index);
            Ok(SemanticValue::List(vec![
                SemanticValue::Str("interval".into()),
                SemanticValue::UInt(0),
                SemanticValue::Int(value as i64),
            ]))
        }
        IntervalUnit::DayTime => {
            let value = array
                .as_any()
                .downcast_ref::<IntervalDayTimeArray>()
                .ok_or_else(|| type_error("interval day-time array"))?
                .value(index);
            Ok(SemanticValue::List(vec![
                SemanticValue::Str("interval".into()),
                SemanticValue::UInt(1),
                SemanticValue::Int(value.days as i64),
                SemanticValue::Int(value.milliseconds as i64),
            ]))
        }
        IntervalUnit::MonthDayNano => {
            let value = array
                .as_any()
                .downcast_ref::<IntervalMonthDayNanoArray>()
                .ok_or_else(|| type_error("interval month-day-nano array"))?
                .value(index);
            Ok(SemanticValue::List(vec![
                SemanticValue::Str("interval".into()),
                SemanticValue::UInt(2),
                SemanticValue::Int(value.months as i64),
                SemanticValue::Int(value.days as i64),
                SemanticValue::Int(value.nanoseconds),
            ]))
        }
    }
}
