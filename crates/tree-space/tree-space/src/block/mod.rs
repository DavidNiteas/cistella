//! TB data blocks: typed containers with canonical, content-addressed bytes.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::Digest;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

pub(crate) mod arrow;
mod registry;
mod table;
mod value;
pub use registry::{ArrowCaps, BlockCaps, RegisteredBlock, block_caps, register_block};
pub use table::Table;
pub use value::{
    Decimal128, Decimal256, DurationUnit, Time32Unit, Time64Unit, TimestampUnit, Value, ValueKind,
};

/// Content-addressed reference to a TB block.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RefId(
    /// The raw 16-byte identity digest.
    pub Digest,
);

impl std::fmt::Display for RefId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl RefId {
    /// Builds a reference from raw 16-byte identity bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Digest::from_bytes(bytes))
    }

    /// Returns the raw 16-byte identity bytes.
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0.as_bytes()
    }
}

/// A closed built-in or registered TB block kind.
///
/// The four built-in kinds (`table`/`sequence`/`kv`/`blob`) are always Arrow
/// encoded. Additional kinds are registered under a stable name (see
/// [`RegisteredBlock`]) and may be opaque or Arrow via [`BlockCaps`].
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum BlockKind {
    /// An Arrow IPC table block.
    Table,
    /// A sequence of self-describing scalar values.
    Sequence,
    /// A canonical-ordered key/value block.
    Kv,
    /// An arbitrary byte block.
    Blob,
    /// A registered block kind, identified by its stable registration name.
    Named(Arc<str>),
}
impl BlockKind {
    /// Every built-in kind, in tag order.
    pub const ALL: [Self; 4] = [Self::Table, Self::Sequence, Self::Kv, Self::Blob];
    /// Returns the stable kind name; built-ins use the fixed lowercase names.
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::Table => Cow::Borrowed("table"),
            Self::Sequence => Cow::Borrowed("sequence"),
            Self::Kv => Cow::Borrowed("kv"),
            Self::Blob => Cow::Borrowed("blob"),
            Self::Named(name) => Cow::Owned(name.to_string()),
        }
    }
    /// Returns the one-byte envelope tag (built-ins 1–4, registered kinds 5).
    pub fn tag(&self) -> u8 {
        match self {
            Self::Table => 1,
            Self::Sequence => 2,
            Self::Kv => 3,
            Self::Blob => 4,
            Self::Named(_) => 5,
        }
    }
    /// Returns the identity-domain prefix for the kind.
    pub fn domain(&self) -> Vec<u8> {
        match self {
            Self::Table => b"tb-block-table".to_vec(),
            Self::Sequence => b"tb-block-sequence".to_vec(),
            Self::Kv => b"tb-block-kv".to_vec(),
            Self::Blob => b"tb-block-blob".to_vec(),
            Self::Named(name) => {
                let mut out = b"tb-block-".to_vec();
                out.extend_from_slice(name.as_bytes());
                out
            }
        }
    }
    /// Resolves a one-byte envelope tag back to a built-in kind.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Table),
            2 => Some(Self::Sequence),
            3 => Some(Self::Kv),
            4 => Some(Self::Blob),
            _ => None,
        }
    }
}

/// Validates a registered block name against the AB-1 name rules.
///
/// Names must be non-empty UTF-8 of at most 64 bytes, free of ASCII control
/// characters and `/`/`%`, and must not start with the reserved `tb-block-`
/// domain prefix.
pub fn validate_block_name(name: &str) -> Result<()> {
    let invalid = name.is_empty()
        || name.len() > 64
        || name.starts_with("tb-block-")
        || name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'/' || byte == b'%');
    if invalid {
        return Err(TreeSpaceError::new(
            ErrorCode::NameReserved,
            "invalid registered block name",
        ));
    }
    Ok(())
}

/// A static description of a TB block kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDesc {
    /// The block kind.
    pub kind: BlockKind,
    /// Stable descriptor name.
    pub name: Cow<'static, str>,
}

impl BlockDesc {
    /// Returns the descriptor for a kind.
    pub fn for_kind(kind: BlockKind) -> Self {
        let name = kind.as_str();
        Self { kind, name }
    }
}

/// A canonical TB block. The payload is the sole identity input.
pub trait Block: Send + Sync + std::fmt::Debug {
    /// Returns the closed runtime kind.
    fn kind(&self) -> BlockKind;
    /// Writes canonical payload bytes to an existing buffer.
    fn write_payload(&self, out: &mut Vec<u8>);
    /// Returns canonical payload bytes as a convenience allocation.
    fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_payload(&mut out);
        out
    }
    /// Returns the content identity derived from kind and payload.
    ///
    /// M2 semantic formula (01 §4-4): forwards to [`block_ref_id`], the single
    /// identity-formula entry point shared by the plugins and the bucket.
    fn content_hash(&self) -> RefId {
        let mut payload = Vec::new();
        self.write_payload(&mut payload);
        block_ref_id(self.kind(), &payload)
    }
    /// Returns the static block description.
    fn describe(&self) -> BlockDesc {
        BlockDesc::for_kind(self.kind())
    }
    /// Returns this value as a table when its kind is `Table`.
    fn as_table(&self) -> Option<&Table> {
        None
    }
    /// Returns this value as a sequence when its kind is `Sequence`.
    fn as_sequence(&self) -> Option<&Sequence> {
        None
    }
    /// Returns this value as a key/value block when its kind is `Kv`.
    fn as_kv(&self) -> Option<&Kv> {
        None
    }
    /// Returns this value as a byte block when its kind is `Blob`.
    fn as_blob(&self) -> Option<&Blob> {
        None
    }
    /// Compatibility spelling for content identity.
    fn ref_id(&self) -> RefId {
        self.content_hash()
    }
    /// Builds the in-memory storage envelope.
    fn envelope(&self) -> Envelope {
        let mut payload = Vec::new();
        self.write_payload(&mut payload);
        Envelope::new(self.kind(), payload)
    }
}
/// The in-memory storage envelope. Envelope framing is not part of RefId.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope {
    /// The closed block kind.
    pub kind: BlockKind,
    /// The framing version.
    pub version: u16,
    /// The canonical block payload bytes.
    pub payload: Vec<u8>,
}
impl Envelope {
    /// The current framing version used by built-in kinds.
    pub const VERSION: u16 = 1;
    /// The framing version used by registered (named) kinds.
    pub const NAMED_VERSION: u16 = 2;
    /// Creates an envelope at the current framing version.
    pub fn new(kind: BlockKind, payload: Vec<u8>) -> Self {
        let version = if matches!(kind, BlockKind::Named(_)) {
            Self::NAMED_VERSION
        } else {
            Self::VERSION
        };
        Self {
            kind,
            version,
            payload,
        }
    }
    /// Encodes the frame: `[kind tag][version u16 LE][len u64 LE][payload]`
    /// for built-ins, or `[5][name_len u8][name][version 2 u16 LE][len][payload]`
    /// for registered kinds.
    pub fn encode(&self) -> Vec<u8> {
        match &self.kind {
            BlockKind::Named(name) => {
                let mut out = Vec::with_capacity(12 + name.len() + self.payload.len());
                out.push(5);
                out.push(name.len() as u8);
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(&Self::NAMED_VERSION.to_le_bytes());
                out.extend_from_slice(&(self.payload.len() as u64).to_le_bytes());
                out.extend_from_slice(&self.payload);
                out
            }
            _ => {
                let mut out = Vec::with_capacity(11 + self.payload.len());
                out.push(self.kind.tag());
                out.extend_from_slice(&self.version.to_le_bytes());
                out.extend_from_slice(&(self.payload.len() as u64).to_le_bytes());
                out.extend_from_slice(&self.payload);
                out
            }
        }
    }
    /// Decodes an envelope frame, rejecting truncation, unknown kinds,
    /// unsupported versions, and trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let Some(&first) = bytes.first() else {
            return Err(malformed("truncated envelope"));
        };
        if first == 5 {
            if bytes.len() < 12 {
                return Err(malformed("truncated named envelope"));
            }
            let name_len = bytes[1] as usize;
            if bytes.len() < 2 + name_len + 11 {
                return Err(malformed("truncated named envelope"));
            }
            let name = std::str::from_utf8(&bytes[2..2 + name_len])
                .map_err(|_| malformed("envelope kind name is not utf8"))?
                .to_owned();
            validate_block_name(&name)?;
            let offset = 2 + name_len;
            let version = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
            if version != Self::NAMED_VERSION {
                return Err(malformed("unsupported named envelope version"));
            }
            let len = usize::try_from(u64::from_le_bytes(
                bytes[offset + 2..offset + 10].try_into().unwrap(),
            ))
            .map_err(|_| malformed("envelope length does not fit platform usize"))?;
            if bytes.len() != offset + 10 + len {
                return Err(malformed("invalid envelope length"));
            }
            Ok(Self {
                kind: BlockKind::Named(Arc::<str>::from(name)),
                version,
                payload: bytes[offset + 10..].to_vec(),
            })
        } else {
            let kind = BlockKind::from_tag(first).ok_or_else(|| malformed("unknown block kind"))?;
            if bytes.len() < 11 {
                return Err(malformed("truncated envelope"));
            }
            let version = u16::from_le_bytes([bytes[1], bytes[2]]);
            let len = usize::try_from(u64::from_le_bytes(bytes[3..11].try_into().unwrap()))
                .map_err(|_| malformed("envelope length does not fit platform usize"))?;
            if version != Self::VERSION {
                return Err(malformed("unsupported envelope version"));
            }
            if bytes.len() != 11 + len {
                return Err(malformed("invalid envelope length"));
            }
            Ok(Self {
                kind,
                version,
                payload: bytes[11..].to_vec(),
            })
        }
    }
}
fn type_mismatch(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::SchemaMismatch, message)
}
fn malformed(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

/// A sequence block with value-level self-describing elements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sequence {
    values: Vec<Value>,
}
impl Sequence {
    /// Creates a sequence from an ordered list of scalar values.
    pub fn new(values: impl Into<Vec<Value>>) -> Self {
        Self {
            values: values.into(),
        }
    }
    /// Returns the ordered elements.
    pub fn values(&self) -> &[Value] {
        &self.values
    }
    /// Consumes the sequence, returning the ordered elements.
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// Validates all elements against a tree-provided scalar kind.
    pub fn validate_kind(&self, expected: ValueKind) -> Result<()> {
        if self.values.iter().all(|value| value.is_kind(expected)) {
            Ok(())
        } else {
            Err(type_mismatch("sequence element kind does not match schema"))
        }
    }

    /// Consumes the sequence after validating its scalar element kind.
    pub fn into_typed(self, expected: ValueKind) -> Result<Vec<Value>> {
        self.validate_kind(expected)?;
        Ok(self.values)
    }
    /// Encodes the canonical payload (`[version=1][count u64 LE][elements]`).
    pub fn encode(&self) -> Vec<u8> {
        arrow::encode_sequence(&self.values).expect("sequence values encode as Arrow IPC")
    }
    /// Decodes a sequence payload with defensive bound checks.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self::new(arrow::decode_sequence(bytes)?))
    }
}
impl Block for Sequence {
    fn kind(&self) -> BlockKind {
        BlockKind::Sequence
    }
    fn write_payload(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.encode());
    }
    fn as_sequence(&self) -> Option<&Sequence> {
        Some(self)
    }
}

/// A key/value block with canonical key ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Kv {
    entries: Vec<(Value, Value)>,
}
impl Kv {
    /// Builds a kv block from unordered entries, rejecting null/duplicate keys.
    pub fn try_new(entries: impl IntoIterator<Item = (Value, Value)>) -> Result<Self> {
        Self::from_entries(entries.into_iter().collect())
    }
    /// Builds and canonical-sorts entries, rejecting null/duplicate keys.
    pub fn from_entries(mut entries: Vec<(Value, Value)>) -> Result<Self> {
        let mut seen = BTreeSet::new();
        for (key, _) in &entries {
            if matches!(key, Value::Null) || !seen.insert(key.encode()) {
                return Err(malformed("null or duplicate key"));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self { entries })
    }
    /// Returns the canonical-ordered entries.
    pub fn entries(&self) -> &[(Value, Value)] {
        &self.entries
    }

    /// Validates key and value kinds against tree-provided expectations.
    pub fn validate_kinds(&self, key: ValueKind, value: ValueKind) -> Result<()> {
        if self
            .entries
            .iter()
            .all(|(k, v)| k.is_kind(key) && v.is_kind(value))
        {
            Ok(())
        } else {
            Err(type_mismatch("kv key or value kind does not match schema"))
        }
    }

    /// Consumes the map after validating key and value kinds.
    pub fn into_typed(self, key: ValueKind, value: ValueKind) -> Result<Vec<(Value, Value)>> {
        self.validate_kinds(key, value)?;
        Ok(self.entries)
    }
    /// Encodes the canonical payload (`[version=1][count u64 LE][pairs]`).
    pub fn encode(&self) -> Vec<u8> {
        arrow::encode_kv(&self.entries).expect("kv entries encode as Arrow IPC")
    }
    /// Decodes a kv payload, enforcing sorting, uniqueness, and bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let entries = arrow::decode_kv(bytes)?;
        let mut previous = None;
        for (key, _) in &entries {
            if matches!(key, Value::Null) {
                return Err(malformed("null key"));
            }
            if previous.as_ref().is_some_and(|value: &Value| value >= key) {
                return Err(malformed("keys are not sorted or are duplicated"));
            }
            previous = Some(key.clone());
        }
        Ok(Self { entries })
    }
}
impl Block for Kv {
    fn kind(&self) -> BlockKind {
        BlockKind::Kv
    }
    fn write_payload(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&arrow::encode_kv(&self.entries).expect("kv encodes as Arrow IPC"));
    }
    fn as_kv(&self) -> Option<&Kv> {
        Some(self)
    }
}

/// An arbitrary byte block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Blob(
    /// The raw byte payload.
    pub Vec<u8>,
);
impl Blob {
    /// Creates a blob from arbitrary bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }
    /// Returns the raw byte payload.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl Block for Blob {
    fn kind(&self) -> BlockKind {
        BlockKind::Blob
    }
    fn write_payload(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&arrow::encode_blob(&self.0).expect("blob encodes as Arrow IPC"));
    }
    fn as_blob(&self) -> Option<&Blob> {
        Some(self)
    }
}

pub use table::Table as ArrowTable;

/// Weak, type-erased block decoding. Built-in kinds use the fixed canonical
/// codecs; registered kinds are decoded through the process registry.
pub fn read_block(kind: BlockKind, payload: &[u8]) -> Result<Box<dyn Block>> {
    match kind {
        BlockKind::Table => Ok(Box::new(Table::try_read_blob(payload)?)),
        BlockKind::Sequence => Ok(Box::new(Sequence::decode(payload)?)),
        BlockKind::Kv => Ok(Box::new(Kv::decode(payload)?)),
        BlockKind::Blob => Ok(Box::new(Blob::new(arrow::decode_blob(payload)?))),
        BlockKind::Named(name) => registry::decode_named(&name, payload),
    }
}

/// Computes the TB identity for a kind and canonical payload.
///
/// PL-2 M2 (01 §4-4) switches the formula from the canonical-byte digest to
/// the semantic formula: `RefId = hash(kind.domain(), [semantic 指纹])`
/// (see `plugin::semantic::semantic_block_ref_id`). The domain tag
/// (`tb-block-*`) is unchanged; the storage-address formula
/// (`block_blob_address`) keeps hashing raw bytes.
pub fn block_ref_id(kind: BlockKind, payload: &[u8]) -> RefId {
    crate::plugin::semantic::semantic_block_ref_id(kind, payload)
}

#[cfg(test)]
mod tests;
