//! Type declarations and Arrow Field IPC helpers.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{SchemaVersion, TypeId};
use crate::path::Name;
use arrow::datatypes::{Field, Schema};
use arrow::ipc::convert::{IpcSchemaEncoder, fb_to_schema};
use arrow::ipc::root_as_schema;

/// Number of instances permitted for one domain child definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Cardinality {
    /// Exactly one child must exist at publication.
    ExactlyOne,
    /// Zero or one child may exist.
    ZeroOrOne,
    /// Any number of children may exist.
    ZeroOrMore,
    /// One or more children must exist at publication.
    OneOrMore,
}

impl Cardinality {
    /// Returns whether `count` satisfies this cardinality.
    pub const fn accepts(self, count: usize) -> bool {
        match self {
            Self::ExactlyOne => count == 1,
            Self::ZeroOrOne => count <= 1,
            Self::ZeroOrMore => true,
            Self::OneOrMore => count >= 1,
        }
    }

    /// Returns whether another child may be added before publication.
    pub const fn permits_additional(self, count: usize) -> bool {
        match self {
            Self::ExactlyOne | Self::ZeroOrOne => count == 0,
            Self::ZeroOrMore | Self::OneOrMore => true,
        }
    }
}

/// Ownership and naming semantics for a child definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum InstanceMode {
    /// The registry creates the single child and it cannot be renamed.
    Exclusive,
    /// A caller supplies a validated name and may use logical rename.
    Shared,
}

/// Domain child kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ChildKind {
    /// A child domain node.
    Domain,
    /// A leaf table instance.
    Table,
}

/// One ordered child definition in a domain type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildDefinition {
    /// Continuous logical order beginning at zero.
    pub child_order: u32,
    /// Logical child name.
    pub name: Name,
    /// Whether this describes a node or a table.
    pub child_kind: ChildKind,
    /// Domain or table type identity.
    pub child_type_id: TypeId,
    /// Number of instances permitted below the parent node.
    pub cardinality: Cardinality,
    /// Exclusive or shared creation semantics.
    pub instance_mode: InstanceMode,
}

impl ChildDefinition {
    /// Creates an ordered domain child definition.
    pub fn new(
        child_order: u32,
        name: Name,
        child_kind: ChildKind,
        child_type_id: TypeId,
        cardinality: Cardinality,
        instance_mode: InstanceMode,
    ) -> Self {
        Self {
            child_order,
            name,
            child_kind,
            child_type_id,
            cardinality,
            instance_mode,
        }
    }
}

/// Immutable definition of a domain-node shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainType {
    /// Stable type identity.
    pub type_id: TypeId,
    /// Portable type name.
    pub name: Name,
    /// Persisted type version.
    pub version: SchemaVersion,
    /// Default ownership semantics for instances of this type.
    pub instance_mode: InstanceMode,
    /// Ordered direct children.
    pub children: Vec<ChildDefinition>,
    /// Domain types whose children are transitively included.
    pub includes_type_ids: Vec<TypeId>,
}

/// One declared physical column in a table type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnDefinition {
    /// Continuous logical column order beginning at zero.
    pub col_order: u32,
    /// Portable column name. Must equal the Arrow `Field` name (checked by the
    /// registry); unlike paths, column names are not restricted to ASCII.
    pub name: String,
    /// Arrow `Field` encoded as an Arrow IPC Schema FlatBuffer containing one field.
    pub field_ipc: Vec<u8>,
    /// Physical nullability requirement.
    pub nullable: bool,
    /// Whether the column participates in Merkle digests.
    pub in_hash: bool,
}

impl ColumnDefinition {
    /// Creates a column definition and encodes its Arrow Field through Arrow 55 IPC.
    ///
    /// The column name is taken from the field itself, so the declaration and
    /// the persisted Arrow schema can never disagree.
    pub fn from_field(col_order: u32, field: Field, in_hash: bool) -> Self {
        let name = field.name().to_owned();
        let nullable = field.is_nullable();
        Self {
            col_order,
            name,
            field_ipc: encode_field_ipc(&field),
            nullable,
            in_hash,
        }
    }

    /// Decodes and validates the one-field Arrow schema encoding.
    pub fn field(&self) -> Result<Field> {
        decode_field_ipc(&self.field_ipc)
    }
}

/// Immutable definition of a table shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableType {
    /// Stable type identity.
    pub type_id: TypeId,
    /// Portable type name.
    pub name: Name,
    /// Persisted type version.
    pub version: SchemaVersion,
    /// Direct columns before composition expansion.
    pub columns: Vec<ColumnDefinition>,
    /// Table types whose columns are transitively included.
    pub includes_type_ids: Vec<TypeId>,
}

/// Encodes a Field using Arrow 55 IPC schema FlatBuffer bytes.
pub fn encode_field_ipc(field: &Field) -> Vec<u8> {
    let schema = Schema::new(vec![field.clone()]);
    IpcSchemaEncoder::new()
        .schema_to_fb(&schema)
        .finished_data()
        .to_vec()
}

/// Decodes a Field encoded by [`encode_field_ipc`].
pub fn decode_field_ipc(bytes: &[u8]) -> Result<Field> {
    let raw = root_as_schema(bytes).map_err(|error| {
        TreeSpaceError::new(ErrorCode::PayloadMalformed, "invalid Arrow IPC Field bytes")
            .with_context("detail", error.to_string())
    })?;
    let schema = fb_to_schema(raw);
    if schema.fields().len() != 1 {
        return Err(TreeSpaceError::new(
            ErrorCode::SchemaMismatch,
            "field IPC must contain exactly one field",
        ));
    }
    Ok(schema.field(0).clone())
}
