//! Compile-time shape vocabulary of the tree template system.
//!
//! Every template is a [`TreeTemplate`] implementor whose shape is a
//! [`TemplateSpec`] constant: a flat entry list of domains, tiered domains and
//! table leaves, optionally anchored to coordinate slots. The shape carries
//! column contracts (`name` / [`ConstType`] / nullability / id-component /
//! aliases / index declarations) but never instance data, so orthogonal and
//! merge composition can be judged entirely at compile time (work order
//! document 02, sections 1 and 2).
//!
//! Design authority: the modular-subtree work order under
//! `crates/tree-space/_dev` (document 01 for the seven semantic rulings,
//! document 02 for this mechanism).

use crate::error::{ErrorCode, Result, TreeSpaceError};

/// Maximum depth of a resolved coordinate-slot chain.
///
/// At anchors and mount chains longer than this are rejected
/// ([`crate::template::ConflictKind::ChainTooDeep`]); the bound doubles as the cycle guard for
/// malformed coordinate systems.
pub const MAX_SLOT_DEPTH: usize = 8;

/// A coordinate-slot name of the mount coordinate system.
///
/// A newtype over `&'static str` (not a closed enum) so downstream hosts can
/// declare their own slots; two slots are the same slot when their strings are
/// byte-equal. `STUDY` / `RUN` are the current coordinate system, `LIB` is the
/// reserved future coordinate (documented but not mapped by any runtime
/// projection yet).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotName(pub &'static str);

impl SlotName {
    /// The study level (current coordinate system, root level).
    pub const STUDY: SlotName = SlotName("study");
    /// The run level (current coordinate system, under study).
    pub const RUN: SlotName = SlotName("run");
    /// The library-resource level (reserved future coordinate, document 05
    /// R2.5; registered here so compositions can already name it).
    pub const LIB: SlotName = SlotName("lib");

    /// Compile-time byte equality of two slot names.
    pub const fn const_eq(self, other: SlotName) -> bool {
        str_eq(self.0, other.0)
    }
}

/// Compile-time equality of two string slices.
pub(crate) const fn str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Compile-time equality of two `&'static str` slices.
pub(crate) const fn str_slice_eq(a: &[&'static str], b: &[&'static str]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !str_eq(a[i], b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// The logical time unit of timestamp and duration columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstTimeUnit {
    /// Seconds.
    Second,
    /// Milliseconds.
    Millisecond,
    /// Microseconds.
    Microsecond,
    /// Nanoseconds.
    Nanosecond,
}

const fn unit_eq(a: ConstTimeUnit, b: ConstTimeUnit) -> bool {
    matches!(
        (a, b),
        (ConstTimeUnit::Second, ConstTimeUnit::Second)
            | (ConstTimeUnit::Millisecond, ConstTimeUnit::Millisecond)
            | (ConstTimeUnit::Microsecond, ConstTimeUnit::Microsecond)
            | (ConstTimeUnit::Nanosecond, ConstTimeUnit::Nanosecond)
    )
}

/// One member of a const struct shape carried by [`ConstType::ListStruct`]
/// (the compile-time mirror of the downstream definition layer's
/// `StructShape` member).
///
/// Controlled depth is enforced at the type level: the member type is the
/// flat [`ConstScalar`] (a list can never appear inside a struct shape), so
/// nesting can never recurse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstStructField {
    /// The member name (unique within its shape; checked by the projection's
    /// Arrow mapping).
    pub name: &'static str,
    /// The scalar member type.
    pub ty: ConstScalar,
    /// Whether the member may be null inside the struct.
    pub nullable: bool,
}

/// The scalar element type of a `List(scalar)` column.
///
/// A flat mirror of the scalar `ConstType` variants: the list variants are
/// excluded by construction, so a scalar list element can never carry a
/// nested list (controlled depth at the type level).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstScalar {
    /// Boolean.
    Bool,
    /// Signed integer with a bit width (8/16/32/64).
    I(u8),
    /// Unsigned integer with a bit width (8/16/32/64).
    U(u8),
    /// Float with a bit width (32/64).
    F(u8),
    /// UTF-8 text.
    Utf8,
    /// Opaque bytes.
    Binary,
    /// Days since the Unix epoch.
    Date32,
    /// Timestamp with a time unit and a fixed timezone (`""` = none).
    Ts {
        /// The time unit.
        unit: ConstTimeUnit,
        /// The fixed timezone, or `""` for none.
        tz: &'static str,
    },
    /// Duration with a time unit.
    Dur(ConstTimeUnit),
    /// A semantically typed opaque payload (projected as Arrow `Binary`).
    Opaque(&'static str),
}

impl ConstScalar {
    /// Widens this flat scalar back into its [`ConstType`] equivalent (used
    /// by the Arrow mapping to reuse one conversion path).
    pub const fn as_const_type(self) -> ConstType {
        match self {
            ConstScalar::Bool => ConstType::Bool,
            ConstScalar::I(bits) => ConstType::I(bits),
            ConstScalar::U(bits) => ConstType::U(bits),
            ConstScalar::F(bits) => ConstType::F(bits),
            ConstScalar::Utf8 => ConstType::Utf8,
            ConstScalar::Binary => ConstType::Binary,
            ConstScalar::Date32 => ConstType::Date32,
            ConstScalar::Ts { unit, tz } => ConstType::Ts { unit, tz },
            ConstScalar::Dur(unit) => ConstType::Dur(unit),
            ConstScalar::Opaque(tag) => ConstType::Opaque(tag),
        }
    }
}

/// The const-friendly logical column type of a template column.
///
/// This is the compile-time mirror of the downstream definition layer's
/// logical scalar type: heap-carrying variants (`String` payloads) use
/// `&'static str` instead so templates stay const-constructible. The runtime
/// projection maps this to an Arrow `Field` (see `crate::template::project`);
/// `Opaque` maps to Arrow `Binary` per the monorepo mapping convention.
///
/// List support (uni-mass-db MVP redesign S2): [`ConstType::ListScalar`]
/// carries `List(scalar)` and [`ConstType::ListStruct`] carries
/// `List(Struct{...})` with an inline field table. Both mirror the
/// downstream `LogicalDataType` list semantics: the Arrow element slot is
/// named `item` and is not independently nullable; struct members carry
/// their own nullability and must be scalar (controlled depth).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstType {
    /// Boolean.
    Bool,
    /// Signed integer with a bit width (8/16/32/64).
    I(u8),
    /// Unsigned integer with a bit width (8/16/32/64).
    U(u8),
    /// Float with a bit width (32/64).
    F(u8),
    /// UTF-8 text.
    Utf8,
    /// Opaque bytes.
    Binary,
    /// Days since the Unix epoch.
    Date32,
    /// Timestamp with a time unit and a fixed timezone (`""` = none).
    Ts {
        /// The time unit.
        unit: ConstTimeUnit,
        /// The fixed timezone, or `""` for none.
        tz: &'static str,
    },
    /// Duration with a time unit.
    Dur(ConstTimeUnit),
    /// A semantically typed opaque payload (projected as Arrow `Binary`).
    Opaque(&'static str),
    /// `List(scalar)`: a list whose element slot carries one scalar value of
    /// the given flat scalar type ([`ConstScalar`] excludes nested lists by
    /// construction).
    ListScalar(ConstScalar),
    /// `List(Struct{...})`: a list whose element slot carries an inline
    /// struct with the given field table. Every member is a flat
    /// [`ConstScalar`] (controlled depth at the type level, mirroring the
    /// downstream `StructShape` semantics).
    ListStruct(&'static [ConstStructField]),
}

impl ConstType {
    /// Compile-time structural equality of two logical types.
    pub const fn const_eq(self, other: ConstType) -> bool {
        match (self, other) {
            (Self::Bool, Self::Bool) => true,
            (Self::I(a), Self::I(b)) => a == b,
            (Self::U(a), Self::U(b)) => a == b,
            (Self::F(a), Self::F(b)) => a == b,
            (Self::Utf8, Self::Utf8) => true,
            (Self::Binary, Self::Binary) => true,
            (Self::Date32, Self::Date32) => true,
            (Self::Ts { unit: au, tz: at }, Self::Ts { unit: bu, tz: bt }) => {
                unit_eq(au, bu) && str_eq(at, bt)
            }
            (Self::Dur(a), Self::Dur(b)) => unit_eq(a, b),
            (Self::Opaque(a), Self::Opaque(b)) => str_eq(a, b),
            (Self::ListScalar(a), Self::ListScalar(b)) => scalar_eq(a, b),
            (Self::ListStruct(a), Self::ListStruct(b)) => const_struct_fields_eq(a, b),
            _ => false,
        }
    }
}

/// Compile-time equality of two flat scalar values.
const fn scalar_eq(a: ConstScalar, b: ConstScalar) -> bool {
    match (a, b) {
        (ConstScalar::I(x), ConstScalar::I(y))
        | (ConstScalar::U(x), ConstScalar::U(y))
        | (ConstScalar::F(x), ConstScalar::F(y)) => x == y,
        (ConstScalar::Ts { unit: au, tz: at }, ConstScalar::Ts { unit: bu, tz: bt }) => {
            unit_eq(au, bu) && str_eq(at, bt)
        }
        (ConstScalar::Dur(x), ConstScalar::Dur(y)) => unit_eq(x, y),
        (ConstScalar::Opaque(x), ConstScalar::Opaque(y)) => str_eq(x, y),
        (ConstScalar::Bool, ConstScalar::Bool)
        | (ConstScalar::Binary, ConstScalar::Binary)
        | (ConstScalar::Date32, ConstScalar::Date32)
        | (ConstScalar::Utf8, ConstScalar::Utf8) => true,
        _ => false,
    }
}

/// Compile-time structural equality of two const struct field tables.
const fn const_struct_fields_eq(a: &[ConstStructField], b: &[ConstStructField]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !str_eq(a[i].name, b[i].name)
            || !scalar_eq(a[i].ty, b[i].ty)
            || a[i].nullable != b[i].nullable
        {
            return false;
        }
        i += 1;
    }
    true
}

/// The const placeholder of an index declaration on a template table.
///
/// Tree-space `TableType` has no index concept; the declaration is carried for
/// downstream definition-layer adapters (R7: policy never enters templates or
/// definitions, it lives in runtime configuration). `Custom` is the escape
/// hatch for kinds this layer does not name yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexKindConst {
    /// Hash (equality) index.
    Hash,
    /// Ordered (range) index.
    Ordered,
    /// 2D rect (box) index.
    Rect,
    /// N-dimensional R-tree over points.
    Rtree,
    /// Hilbert spatial index.
    Hilbert,
    /// An unnamed index kind, carried verbatim for downstream adapters.
    Custom(&'static str),
}

const fn index_kind_eq(a: IndexKindConst, b: IndexKindConst) -> bool {
    match (a, b) {
        (IndexKindConst::Hash, IndexKindConst::Hash) => true,
        (IndexKindConst::Ordered, IndexKindConst::Ordered) => true,
        (IndexKindConst::Rect, IndexKindConst::Rect) => true,
        (IndexKindConst::Rtree, IndexKindConst::Rtree) => true,
        (IndexKindConst::Hilbert, IndexKindConst::Hilbert) => true,
        (IndexKindConst::Custom(x), IndexKindConst::Custom(y)) => str_eq(x, y),
        _ => false,
    }
}

/// One declared index of a template table (see [`IndexKindConst`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemplateIndex {
    /// The index kind.
    pub kind: IndexKindConst,
    /// The indexed columns in declaration order.
    pub columns: &'static [&'static str],
}

const fn index_eq(a: &TemplateIndex, b: &TemplateIndex) -> bool {
    index_kind_eq(a.kind, b.kind) && str_slice_eq(a.columns, b.columns)
}

/// A cross-tree endpoint declaration of an auxiliary link column (R3').
///
/// Declaring it never creates a template-definition-time dependency: the
/// reference is inert while the auxiliary template stands alone and is checked
/// only when a composite actually forms (`points_to` endpoint existence and
/// type match, both composition modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EndpointRef {
    /// The `TreeTemplate::NAME` of the template that must participate.
    pub template: &'static str,
    /// The template-internal path of the referenced table entry.
    pub leaf: &'static [&'static str],
    /// The referenced column name.
    pub column: &'static str,
    /// The required logical type of the referenced column.
    pub expect: ConstType,
}

const fn endpoint_eq(a: &EndpointRef, b: &EndpointRef) -> bool {
    str_eq(a.template, b.template)
        && str_slice_eq(a.leaf, b.leaf)
        && str_eq(a.column, b.column)
        && a.expect.const_eq(b.expect)
}

/// One compile-time column contract of a template table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemplateColumn {
    /// The column name (must equal the projected Arrow field name).
    pub name: &'static str,
    /// The logical type.
    pub ty: ConstType,
    /// Physical nullability.
    pub nullable: bool,
    /// Whether the column belongs to the table's content-id input set (R8).
    pub id_component: bool,
    /// Extra access names of the same physical column (R1.5 alias mechanism;
    /// carried for downstream adapters, not materialized by tree-space).
    pub aliases: &'static [&'static str],
    /// Cross-tree endpoint declaration (R3'); inert until composition.
    pub points_to: Option<EndpointRef>,
}

const fn column_eq(a: &TemplateColumn, b: &TemplateColumn) -> bool {
    str_eq(a.name, b.name)
        && a.ty.const_eq(b.ty)
        && a.nullable == b.nullable
        && a.id_component == b.id_component
        && str_slice_eq(a.aliases, b.aliases)
        && match (a.points_to, b.points_to) {
            (None, None) => true,
            (Some(x), Some(y)) => endpoint_eq(&x, &y),
            _ => false,
        }
}

/// A table leaf of a template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemplateTable {
    /// The table (leaf) name; the last path segment of its entry.
    pub name: &'static str,
    /// The table schema version.
    pub version: u32,
    /// The column contracts in declaration order.
    pub columns: &'static [TemplateColumn],
    /// The index declarations (downstream payload; see [`TemplateIndex`]).
    pub indexes: &'static [TemplateIndex],
}

/// Structural fingerprint equality of two table declarations (P4 gate: the
/// same leaf may be re-declared across templates only when fingerprints are
/// identical).
pub(crate) const fn table_fingerprint_eq(a: &TemplateTable, b: &TemplateTable) -> bool {
    if !str_eq(a.name, b.name) || a.version != b.version || a.columns.len() != b.columns.len() {
        return false;
    }
    let mut i = 0;
    while i < a.columns.len() {
        if !column_eq(&a.columns[i], &b.columns[i]) {
            return false;
        }
        i += 1;
    }
    if a.indexes.len() != b.indexes.len() {
        return false;
    }
    let mut j = 0;
    while j < a.indexes.len() {
        if !index_eq(&a.indexes[j], &b.indexes[j]) {
            return false;
        }
        j += 1;
    }
    true
}

/// The instance-enumeration parameters of a tiered level (mirror of the
/// downstream `FamilyTier` shape): instances are named `{prefix}{index}` for
/// `index` in `0..max_instances`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierDecl {
    /// The instance-segment prefix (`s`, `r`, ...).
    pub prefix: &'static str,
    /// The maximum number of instances under one parent.
    pub max_instances: u32,
}

/// The kind of a template entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TemplateEntryKind {
    /// A domain node. `tier` declares an instance-enumerated sub-level; in M1
    /// only coordinate slots carry tiers (set by the host's coordinate
    /// system), so plain domain entries use `None`.
    Domain {
        /// The tier parameters, when the domain is an instance-enumerated level.
        tier: Option<TierDecl>,
    },
    /// A table leaf.
    Table(TemplateTable),
}

/// The slot anchor of a template entry (document 02 section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotCtx {
    /// The entry follows the mount position of its template.
    Inherit,
    /// The entry is absolutely anchored at the given coordinate-slot chain
    /// (for example a study-level consensus table inside a run-mounted geo
    /// template). The chain must be a valid path of the composite's
    /// coordinate system.
    At(&'static [SlotName]),
}

/// One entry of a template shape: a domain or a table at a template-internal
/// path, with a slot anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemplateEntry {
    /// The slot anchor.
    pub at: SlotCtx,
    /// The template-internal path segments below the resolved anchor.
    pub path: &'static [&'static str],
    /// The entry kind.
    pub kind: TemplateEntryKind,
}

/// One declared coordinate slot of a host template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotDecl {
    /// The slot being declared.
    pub slot: SlotName,
    /// The parent slot (`None` for a root-level slot).
    pub parent: Option<SlotName>,
    /// The instance-enumeration parameters (`None` = a plain structural
    /// level that appears once under its parent).
    pub tier: Option<TierDecl>,
}

/// The compile-time shape of one template.
///
/// Member templates leave `coordinates` empty and declare their slot needs in
/// `required_slots`; the host template of a composite declares the coordinate
/// system. `version` is the template's schema version: a composite projects
/// its domain types with the host's version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemplateSpec {
    /// The template schema version (the host's version stamps projected
    /// domain types).
    pub version: u32,
    /// The declared coordinate system (host templates only; empty for members).
    pub coordinates: &'static [SlotDecl],
    /// The slots this template requires the composite's host to declare.
    pub required_slots: &'static [SlotName],
    /// The shape entries.
    pub entries: &'static [TemplateEntry],
}

/// The identity trait of a tree template: a stable name plus a const shape.
///
/// Every template -- business tree, auxiliary tree, or the public-root host --
/// is a plain implementor with no template-definition-time dependencies
/// (R3'): auxiliary link columns declare inert
/// [`TemplateColumn::points_to`] references that are checked only when a
/// composite actually forms.
pub trait TreeTemplate: 'static + Sync {
    /// The stable template name (referenced by `points_to` endpoints and
    /// named in conflict reports).
    const NAME: &'static str;
    /// The const shape of the template.
    const SPEC: TemplateSpec;
}

/// Rejects a template shape that is structurally unusable for projection:
/// slot chains deeper than [`MAX_SLOT_DEPTH`], entry names that are not valid
/// tree-space names, or table entries with duplicate column names.
pub(crate) fn validate_spec_shape(spec: &TemplateSpec) -> Result<()> {
    if spec.coordinates.len() > MAX_SLOT_DEPTH {
        return Err(err("coordinate system exceeds the maximum slot depth"));
    }
    for entry in spec.entries {
        if entry.path.is_empty() {
            return Err(err("template entry path must not be empty"));
        }
        if entry.path.len() > MAX_SLOT_DEPTH {
            return Err(err("template entry path exceeds the maximum depth"));
        }
        for segment in entry.path {
            crate::path::Name::new(*segment).map_err(|_| {
                err("template entry path segment is not a valid tree-space name")
                    .with_context("segment", (*segment).to_owned())
            })?;
        }
        if let TemplateEntryKind::Table(table) = &entry.kind {
            crate::path::Name::new(table.name).map_err(|_| {
                err("table name is not a valid tree-space name")
                    .with_context("table", table.name.to_owned())
            })?;
            let mut i = 0;
            while i < table.columns.len() {
                let mut j = i + 1;
                while j < table.columns.len() {
                    if str_eq(table.columns[i].name, table.columns[j].name) {
                        return Err(err("template table has duplicate column names")
                            .with_context("table", table.name.to_owned())
                            .with_context("column", table.columns[i].name.to_owned()));
                    }
                    j += 1;
                }
                i += 1;
            }
        }
    }
    Ok(())
}

fn err(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::TypeConflict, message)
}
