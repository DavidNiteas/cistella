use std::cmp::Ordering;

/// Second-level or millisecond-resolution time-of-day units.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Time32Unit {
    /// Whole seconds.
    Second,
    /// Milliseconds.
    Millisecond,
}
/// Microsecond- or nanosecond-resolution time-of-day units.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Time64Unit {
    /// Microseconds.
    Microsecond,
    /// Nanoseconds.
    Nanosecond,
}
/// Resolution units for timestamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TimestampUnit {
    /// Whole seconds since epoch.
    Second,
    /// Milliseconds since epoch.
    Millisecond,
    /// Microseconds since epoch.
    Microsecond,
    /// Nanoseconds since epoch.
    Nanosecond,
}
/// Resolution units for monotonic durations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DurationUnit {
    /// Whole seconds.
    Second,
    /// Milliseconds.
    Millisecond,
    /// Microseconds.
    Microsecond,
    /// Nanoseconds.
    Nanosecond,
}
/// A 128-bit decimal with precision/scale, encoded as little-endian two's complement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Decimal128 {
    /// Significant decimal digits.
    pub precision: u8,
    /// Implied decimal places (may be negative).
    pub scale: i8,
    /// 16 bytes of little-endian two's complement magnitude.
    pub value: [u8; 16],
}
/// A 256-bit decimal with precision/scale, encoded as little-endian two's complement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Decimal256 {
    /// Significant decimal digits.
    pub precision: u8,
    /// Implied decimal places (may be negative).
    pub scale: i8,
    /// 32 bytes of little-endian two's complement magnitude.
    pub value: [u8; 32],
}

/// The closed runtime kind of an inline TB value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ValueKind {
    /// NULL (any null scalar).
    Null,
    /// Boolean.
    Bool,
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit float (IEEE bit pattern storage).
    F32,
    /// 64-bit float (IEEE bit pattern storage).
    F64,
    /// UTF-8 string.
    Utf8,
    /// Arbitrary bytes.
    Binary,
    /// Days since epoch.
    Date32,
    /// Milliseconds since epoch.
    Date64,
    /// Time of day with a 32-bit unit.
    Time32,
    /// Time of day with a 64-bit unit.
    Time64,
    /// Instant with unit and optional timezone.
    Timestamp,
    /// Monotonic duration with a unit.
    Duration,
    /// 128-bit decimal.
    Decimal128,
    /// 256-bit decimal.
    Decimal256,
}

/// The single scalar whitelist used by inline slots, sequence elements and kv
/// keys/values. `F32`/`F64` store IEEE bit patterns for NaN-stable identity.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The null scalar.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 8-bit integer.
    I8(i8),
    /// A signed 16-bit integer.
    I16(i16),
    /// A signed 32-bit integer.
    I32(i32),
    /// A signed 64-bit integer.
    I64(i64),
    /// An unsigned 8-bit integer.
    U8(u8),
    /// An unsigned 16-bit integer.
    U16(u16),
    /// An unsigned 32-bit integer.
    U32(u32),
    /// An unsigned 64-bit integer.
    U64(u64),
    /// A 32-bit float stored as its IEEE 754 bits.
    F32(u32),
    /// A 64-bit float stored as its IEEE 754 bits.
    F64(u64),
    /// A UTF-8 string.
    Utf8(String),
    /// Arbitrary bytes.
    Binary(Vec<u8>),
    /// Days since the UNIX epoch.
    Date32(i32),
    /// Milliseconds since the UNIX epoch.
    Date64(i64),
    /// A time of day with a 32-bit unit.
    Time32(Time32Unit, i32),
    /// A time of day with a 64-bit unit.
    Time64(Time64Unit, i64),
    /// An instant with unit and optional timezone name.
    Timestamp(TimestampUnit, Option<String>, i64),
    /// A monotonic duration with a unit.
    Duration(DurationUnit, i64),
    /// A 128-bit decimal.
    Decimal128(Decimal128),
    /// A 256-bit decimal.
    Decimal256(Decimal256),
}
impl Eq for Value {}
impl Value {
    /// Returns the value's closed scalar kind.
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Null => ValueKind::Null,
            Self::Bool(_) => ValueKind::Bool,
            Self::I8(_) => ValueKind::I8,
            Self::I16(_) => ValueKind::I16,
            Self::I32(_) => ValueKind::I32,
            Self::I64(_) => ValueKind::I64,
            Self::U8(_) => ValueKind::U8,
            Self::U16(_) => ValueKind::U16,
            Self::U32(_) => ValueKind::U32,
            Self::U64(_) => ValueKind::U64,
            Self::F32(_) => ValueKind::F32,
            Self::F64(_) => ValueKind::F64,
            Self::Utf8(_) => ValueKind::Utf8,
            Self::Binary(_) => ValueKind::Binary,
            Self::Date32(_) => ValueKind::Date32,
            Self::Date64(_) => ValueKind::Date64,
            Self::Time32(_, _) => ValueKind::Time32,
            Self::Time64(_, _) => ValueKind::Time64,
            Self::Timestamp(_, _, _) => ValueKind::Timestamp,
            Self::Duration(_, _) => ValueKind::Duration,
            Self::Decimal128(_) => ValueKind::Decimal128,
            Self::Decimal256(_) => ValueKind::Decimal256,
        }
    }

    /// Returns whether the value matches an expected scalar kind.
    pub fn is_kind(&self, expected: ValueKind) -> bool {
        self.kind() == expected
    }
}

impl Value {
    /// Returns the one-byte whitelist tag.
    pub fn tag(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::I8(_) => 2,
            Self::I16(_) => 3,
            Self::I32(_) => 4,
            Self::I64(_) => 5,
            Self::U8(_) => 6,
            Self::U16(_) => 7,
            Self::U32(_) => 8,
            Self::U64(_) => 9,
            Self::F32(_) => 10,
            Self::F64(_) => 11,
            Self::Utf8(_) => 12,
            Self::Binary(_) => 13,
            Self::Date32(_) => 14,
            Self::Date64(_) => 15,
            Self::Time32(_, _) => 16,
            Self::Time64(_, _) => 17,
            Self::Timestamp(_, _, _) => 18,
            Self::Duration(_, _) => 19,
            Self::Decimal128(_) => 20,
            Self::Decimal256(_) => 21,
        }
    }
    /// Encodes the canonical scalar bytes (`[tag][payload]`).
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![self.tag()];
        macro_rules! le {
            ($v:expr) => {
                o.extend_from_slice(&$v.to_le_bytes())
            };
        }
        macro_rules! len {
            ($v:expr) => {{
                let b: &[u8] = $v;
                o.extend_from_slice(&(b.len() as u64).to_le_bytes());
                o.extend_from_slice(b)
            }};
        }
        match self {
            Self::Null => {}
            Self::Bool(v) => o.push(u8::from(*v)),
            Self::I8(v) => o.push(*v as u8),
            Self::I16(v) => le!(*v),
            Self::I32(v) => le!(*v),
            Self::I64(v) => le!(*v),
            Self::U8(v) => o.push(*v),
            Self::U16(v) => le!(*v),
            Self::U32(v) => le!(*v),
            Self::U64(v) => le!(*v),
            Self::F32(v) => le!(*v),
            Self::F64(v) => le!(*v),
            Self::Utf8(v) => len!(v.as_bytes()),
            Self::Binary(v) => len!(v),
            Self::Date32(v) => le!(*v),
            Self::Date64(v) => le!(*v),
            Self::Time32(u, v) => {
                o.push(match u {
                    Time32Unit::Second => 0,
                    Time32Unit::Millisecond => 1,
                });
                le!(*v)
            }
            Self::Time64(u, v) => {
                o.push(match u {
                    Time64Unit::Microsecond => 0,
                    Time64Unit::Nanosecond => 1,
                });
                le!(*v)
            }
            Self::Timestamp(u, t, v) => {
                o.push(*u as u8);
                let tz = t.as_deref().unwrap_or("").as_bytes();
                o.push(tz.len() as u8);
                o.extend_from_slice(tz);
                le!(*v)
            }
            Self::Duration(u, v) => {
                o.push(*u as u8);
                le!(*v)
            }
            Self::Decimal128(v) => {
                o.push(v.precision);
                o.push(v.scale as u8);
                o.extend_from_slice(&v.value)
            }
            Self::Decimal256(v) => {
                o.push(v.precision);
                o.push(v.scale as u8);
                o.extend_from_slice(&v.value)
            }
        }
        o
    }
}
impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order_bytes().cmp(&other.order_bytes())
    }
}

impl Value {
    fn order_bytes(&self) -> Vec<u8> {
        match self {
            Self::Utf8(value) => {
                let mut bytes = vec![self.tag()];
                bytes.extend_from_slice(value.as_bytes());
                bytes
            }
            Self::Binary(value) => {
                let mut bytes = vec![self.tag()];
                bytes.extend_from_slice(value);
                bytes
            }
            _ => self.encode(),
        }
    }
}
impl PartialOrd for Value {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
