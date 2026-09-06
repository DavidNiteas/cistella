//! Runtime projection of a composite template into tree-space type
//! declarations.
//!
//! This is the single direction allowed by the two-layer ruling (P5):
//! composite template (compile-time) -> [`DomainType`]/[`TableType`] values
//! (runtime). The runtime never derives a template from definition values.
//! The projection re-runs the composition checks as a defense-in-depth gate
//! for hand-built [`CompositeSpec`] values, then folds the structured
//! composite deterministically (host first, then mounts in declaration
//! order), and finally materializes the domain/table type set.
//!
//! Slot-to-nesting mapping: M1 ships the canonical coordinate-chain mapping
//! ([`SlotOrder::CoordinateChain`]) -- the coordinate system of the host
//! becomes the projected domain nesting (root -> `study` instances ->
//! `run` instances -> content) with tier instances named `{prefix}{index}`.
//! Physical path schemes of downstream crates (for example owner-outer
//! segment encodings) are persist-layer mapping concerns and do not belong to
//! this projection.

use super::check::composite_check;
use super::composite::CompositeSpec;
use super::spec::{
    ConstStructField, ConstTimeUnit, ConstType, MAX_SLOT_DEPTH, SlotCtx, SlotDecl, SlotName,
    TemplateColumn, TemplateEntry, TemplateEntryKind, TemplateTable, TierDecl, str_eq,
    table_fingerprint_eq, validate_spec_shape,
};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::TypeId;
use crate::path::Name;
use crate::registry::{RegistryExport, TypeRegistry};
use crate::types::{
    Cardinality, ChildDefinition, ChildKind, ColumnDefinition, DomainType, InstanceMode, TableType,
};
use arrow::datatypes::{DataType, Field, TimeUnit};
use std::collections::HashMap;
use xxhash_rust::xxh3::xxh3_128;

/// Identity-domain tag of projected domain types.
const DOMAIN_ID_TAG: &[u8] = b"tree-space.template.domain";
/// Identity-domain tag of projected table types.
const TABLE_ID_TAG: &[u8] = b"tree-space.template.table";

/// Derives the provisional (plan-internal) [`TypeId`] of a projected domain
/// type.
///
/// Positioning (P2-1 ruling): this is a **plan-internal identity** serving
/// plan shaping and in-crate registration self-tests only. The template
/// layer has no owner dimension and therefore cannot reproduce the
/// authoritative derivation of the downstream uni-mass-db persist typeid
/// rules (xxh3-128 over length-prefixed `[owner, table, version]`
/// components). The authoritative [`TypeId`] is re-derived at the
/// uni-mass-db projection boundary (S2, `project_definition`) under those
/// existing rules with the owner injected, and only the re-derived ids enter
/// persistence and manifest reconciliation. Formulas are not unified and
/// existing manifest checking is untouched; the S2 cross-crate equivalence
/// guard compares full values **after** the authoritative re-derivation.
///
/// Provisional formula: `xxh3_128(tag || len32(name) || name || version_le)`
/// with the template-layer domain tag. The derivation is deterministic
/// across processes; two composites that project a `(name, version)` pair
/// with identical shapes deduplicate in the [`TypeRegistry`], differing
/// shapes are a registration conflict.
pub fn domain_type_id(name: &str, version: u32) -> TypeId {
    TypeId::from_bytes(identity(DOMAIN_ID_TAG, name, version))
}

/// Derives the provisional (plan-internal) [`TypeId`] of a projected table
/// type (the same provisional formula as [`domain_type_id`] with the table
/// tag; authoritative ids are re-derived downstream with the owner injected,
/// see [`domain_type_id`]).
pub fn table_type_id(name: &str, version: u32) -> TypeId {
    TypeId::from_bytes(identity(TABLE_ID_TAG, name, version))
}

fn identity(tag: &[u8], name: &str, version: u32) -> [u8; 16] {
    let mut parts = Vec::with_capacity(tag.len() + name.len() + 8);
    parts.extend_from_slice(tag);
    parts.extend_from_slice(&(name.len() as u32).to_le_bytes());
    parts.extend_from_slice(name.as_bytes());
    parts.extend_from_slice(&version.to_le_bytes());
    xxh3_128(&parts).to_le_bytes()
}

/// The child field name used for the `List` element of a nested column
/// (mirrors the downstream uni-mass-db persist arrow mapping, whose S1b
/// delivery fixed the same name; the Arrow IPC round trip preserves it).
pub const LIST_ELEMENT_NAME: &str = "item";

/// Maps a [`ConstType`] to its physical Arrow data type.
///
/// `Opaque` maps to Arrow `Binary` (monorepo mapping convention); unsupported
/// bit widths and non-scalar list element types are
/// [`ErrorCode::SchemaMismatch`] errors. List mapping (both element shapes):
/// `List(Field::new("item", element, false))` -- the element slot is named
/// `item` and is not independently nullable; struct members carry their own
/// declared nullability (strictly aligned with the downstream definition
/// layer's persist mapping).
pub fn data_type(ty: ConstType) -> Result<DataType> {
    Ok(match ty {
        ConstType::Bool => DataType::Boolean,
        ConstType::I(8) => DataType::Int8,
        ConstType::I(16) => DataType::Int16,
        ConstType::I(32) => DataType::Int32,
        ConstType::I(64) => DataType::Int64,
        ConstType::U(8) => DataType::UInt8,
        ConstType::U(16) => DataType::UInt16,
        ConstType::U(32) => DataType::UInt32,
        ConstType::U(64) => DataType::UInt64,
        ConstType::F(32) => DataType::Float32,
        ConstType::F(64) => DataType::Float64,
        ConstType::Utf8 => DataType::Utf8,
        ConstType::Binary => DataType::Binary,
        ConstType::Date32 => DataType::Date32,
        ConstType::Ts { unit, tz } => DataType::Timestamp(
            time_unit(unit)?,
            if tz.is_empty() {
                None
            } else {
                Some(std::sync::Arc::from(tz))
            },
        ),
        ConstType::Dur(unit) => DataType::Duration(time_unit(unit)?),
        ConstType::Opaque(_) => DataType::Binary,
        ConstType::ListScalar(element) => DataType::List(std::sync::Arc::new(Field::new(
            LIST_ELEMENT_NAME,
            data_type(element.as_const_type())?,
            false,
        ))),
        ConstType::ListStruct(fields) => DataType::List(std::sync::Arc::new(Field::new(
            LIST_ELEMENT_NAME,
            DataType::Struct(const_struct_member_fields(fields)?.into()),
            false,
        ))),
        other => {
            return Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "unsupported logical type width",
            )
            .with_context("type", format!("{other:?}")));
        }
    })
}

/// The defensive controlled-depth guard (defense in depth; the const field
/// table already excludes nested lists at the type level).
fn nested_list_error() -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::SchemaMismatch,
        "list element carries a nested list (controlled depth: list elements must be scalar)",
    )
}

/// Maps the const struct field table of a `List(Struct)` element into Arrow
/// member fields. The const element type is a flat [`ConstScalar`], so no
/// member can carry a nested list (the guard below stays as defense in
/// depth against future variant drift).
fn const_struct_member_fields(fields: &[ConstStructField]) -> Result<Vec<Field>> {
    let mut members = Vec::with_capacity(fields.len());
    for field in fields {
        let data = data_type(field.ty.as_const_type())?;
        if matches!(data, DataType::List(_) | DataType::Struct(_)) {
            return Err(nested_list_error());
        }
        members.push(Field::new(field.name, data, field.nullable));
    }
    Ok(members)
}

fn time_unit(unit: ConstTimeUnit) -> Result<TimeUnit> {
    Ok(match unit {
        ConstTimeUnit::Second => TimeUnit::Second,
        ConstTimeUnit::Millisecond => TimeUnit::Millisecond,
        ConstTimeUnit::Microsecond => TimeUnit::Microsecond,
        ConstTimeUnit::Nanosecond => TimeUnit::Nanosecond,
    })
}

/// Builds the Arrow field of one template column (name and nullability come
/// from the contract; the name equals the projected field name by
/// construction).
pub fn field_of(column: &TemplateColumn) -> Result<Field> {
    Ok(Field::new(
        column.name,
        data_type(column.ty)?,
        column.nullable,
    ))
}

/// The slot-to-nesting mapping of a projection.
///
/// M1 ships one canonical mapping; downstream physical schemes (owner-outer
/// segment encodings and the proposed public-root inversion) are future
/// variants of this parameter, not template-layer concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotOrder {
    /// The host coordinate system defines the projected nesting: root ->
    /// root-level slot instances -> child-slot instances -> content, with
    /// tier instances enumerated as `{prefix}{index}`.
    CoordinateChain,
}

/// The mapping parameter of [`project`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeMapping {
    /// The slot-to-nesting order.
    pub slot_order: SlotOrder,
}

impl Default for ShapeMapping {
    fn default() -> Self {
        Self {
            slot_order: SlotOrder::CoordinateChain,
        }
    }
}

/// The projection output: the complete type set of one composite, ready for
/// [`TypeRegistry`] registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeRegistryPlan {
    /// The projected domain types (root, slot levels, structural domains).
    pub domain_types: Vec<DomainType>,
    /// The projected table types (one per distinct table leaf).
    pub table_types: Vec<TableType>,
}

impl TypeRegistryPlan {
    /// Registers the plan into a fresh [`TypeRegistry`], running the
    /// registry's full structural and composition validation.
    pub fn to_registry(&self) -> Result<TypeRegistry> {
        let mut registry = TypeRegistry::new();
        for domain in &self.domain_types {
            registry.register_domain_type(domain.clone())?;
        }
        for table in &self.table_types {
            registry.register_table_type(table.clone())?;
        }
        Ok(registry)
    }

    /// Equivalence guard: registers both this plan and the expected export
    /// into fresh registries and compares the deterministic exports.
    ///
    /// `Ok(true)` = equivalent, `Ok(false)` = both register but differ,
    /// `Err` = one side fails registration (shape or composition invalid).
    /// Positioning (P2-1 ruling): this is the in-crate **structural** form of
    /// the equivalence comparison, run on the provisional plan-internal
    /// identities. The cross-crate equivalence guard (template projection vs
    /// the translate output) compares full values **after** the
    /// authoritative TypeIds are re-derived at the uni-mass-db projection
    /// boundary (S2, owner injected per the existing typeid rules); it lands
    /// downstream where the dependency direction allows it.
    pub fn equivalent_to(&self, expected: &RegistryExport) -> Result<bool> {
        let mut actual = TypeRegistry::new();
        for domain in &self.domain_types {
            actual.register_domain_type(domain.clone())?;
        }
        for table in &self.table_types {
            actual.register_table_type(table.clone())?;
        }
        let mut expected_registry = TypeRegistry::new();
        for domain in &expected.domain_types {
            expected_registry.register_domain_type(domain.clone())?;
        }
        for table in &expected.table_types {
            expected_registry.register_table_type(table.clone())?;
        }
        Ok(actual.export() == expected_registry.export())
    }
}

/// Projects a composite into its runtime type plan.
///
/// The projection validates the composite (composition checks plus shape
/// validation), folds the structured composite deterministically, and
/// materializes the type set. Orthogonal composites conflict-free at compile
/// time always project; merge composites resolve cross-template overlaps
/// here by declaration order (later claims override earlier ones; content
/// strictly under a winning table claim is covered and dropped).
pub fn project(spec: &CompositeSpec, mapping: &ShapeMapping) -> Result<TypeRegistryPlan> {
    match mapping.slot_order {
        SlotOrder::CoordinateChain => {}
    }
    validate_spec_shape(&spec.host)?;
    for mount in spec.mounts {
        validate_spec_shape(&mount.spec)?;
    }
    if let Some(report) = composite_check(spec) {
        return Err(TreeSpaceError::new(
            ErrorCode::TypeConflict,
            format!(
                "composite `{}` violates its composition rules: {report:?}",
                spec.name
            ),
        ));
    }
    let root = fold(spec)?;
    let mut plan = TypeRegistryPlan {
        domain_types: Vec::new(),
        table_types: Vec::new(),
    };
    materialize(&root, spec.host.version, &mut plan)?;
    Ok(plan)
}

/// One node of the folded composite tree, keyed by concrete path (slot
/// segments and name segments are distinct namespaces).
struct FoldNode {
    name: String,
    is_slot: bool,
    tier: Option<TierDecl>,
    /// Whether an explicit entry claim (not a path implication) placed this
    /// node.
    explicit: bool,
    /// The source index of the last explicit claim (0 = host, 1.. = mounts).
    source: usize,
    /// `Some` for table nodes; `None` for domain nodes.
    table: Option<TemplateTable>,
    children: Vec<FoldNode>,
}

impl FoldNode {
    fn is_table(&self) -> bool {
        self.table.is_some()
    }
}

fn fold(spec: &CompositeSpec) -> Result<FoldNode> {
    let mut slot_tiers: HashMap<&'static str, Option<TierDecl>> = HashMap::new();
    for decl in spec.host.coordinates {
        if slot_tiers.insert(decl.slot.0, decl.tier).is_some() {
            return Err(project_error(
                ErrorCode::TypeConflict,
                format!("duplicate coordinate slot `{}`", decl.slot.0),
            ));
        }
    }
    let mut root = FoldNode {
        name: spec.name.to_owned(),
        is_slot: false,
        tier: None,
        explicit: true,
        source: 0,
        table: None,
        children: Vec::new(),
    };
    let mut source = 0;
    while source < spec.mounts.len() + 1 {
        insert_source(&mut root, spec, source, &slot_tiers)?;
        source += 1;
    }
    Ok(root)
}

fn insert_source(
    root: &mut FoldNode,
    spec: &CompositeSpec,
    source: usize,
    slot_tiers: &HashMap<&'static str, Option<TierDecl>>,
) -> Result<()> {
    let template = if source == 0 {
        &spec.host
    } else {
        &spec.mounts[source - 1].spec
    };
    let mount_chain = if source == 0 {
        Vec::new()
    } else {
        coordinate_chain(&spec.host.coordinates, spec.mounts[source - 1].slot)?
    };
    for entry in template.entries {
        let prefix: Vec<SlotName> = match entry.at {
            SlotCtx::Inherit => mount_chain.clone(),
            SlotCtx::At(chain) => chain.to_vec(),
        };
        insert_entry(root, entry, source, &prefix, slot_tiers, spec.merge)?;
    }
    Ok(())
}

/// Outcome of one entry insertion.
enum Inserted {
    /// The entry (or its terminal claim) changed the tree.
    Applied,
    /// The entry was dropped: merge mode covers it under a winning table
    /// claim on an ancestor path.
    Covered,
}

fn insert_entry(
    root: &mut FoldNode,
    entry: &TemplateEntry,
    source: usize,
    prefix: &[SlotName],
    slot_tiers: &HashMap<&'static str, Option<TierDecl>>,
    merge: bool,
) -> Result<Inserted> {
    let mut node = root;
    for slot in prefix {
        node = match index_of(node, slot.0, true) {
            Some(index) => &mut node.children[index],
            None => {
                node.children.push(FoldNode {
                    name: slot.0.to_owned(),
                    is_slot: true,
                    tier: slot_tiers.get(slot.0).copied().flatten(),
                    explicit: true,
                    source: 0,
                    table: None,
                    children: Vec::new(),
                });
                let last = node.children.len() - 1;
                &mut node.children[last]
            }
        };
        if node.is_table() {
            return if merge {
                Ok(Inserted::Covered)
            } else {
                Err(project_error(
                    ErrorCode::TypeConflict,
                    format!("table `{}` cannot host slotted content", node.name),
                ))
            };
        }
    }
    for index in 0..entry.path.len() {
        let segment = entry.path[index];
        if index + 1 == entry.path.len() {
            return apply_claim(node, segment, entry, source, merge);
        }
        node = match descend_or_create(node, segment, source, merge)? {
            Some(child) => child,
            None => return Ok(Inserted::Covered),
        };
    }
    Ok(Inserted::Applied)
}

fn descend_or_create<'a>(
    node: &'a mut FoldNode,
    segment: &'static str,
    source: usize,
    merge: bool,
) -> Result<Option<&'a mut FoldNode>> {
    if let Some(index) = index_of(node, segment, false) {
        let existing = &mut node.children[index];
        if existing.is_table() {
            return if merge {
                Ok(None)
            } else {
                Err(project_error(
                    ErrorCode::TypeConflict,
                    format!("table `{}` cannot host nested content", existing.name),
                ))
            };
        }
        return Ok(Some(existing));
    }
    node.children.push(FoldNode {
        name: segment.to_owned(),
        is_slot: false,
        tier: None,
        explicit: false,
        source,
        table: None,
        children: Vec::new(),
    });
    let last = node.children.len() - 1;
    Ok(Some(&mut node.children[last]))
}

/// Applies the terminal claim of an entry at `node`'s child named `segment`.
fn apply_claim(
    node: &mut FoldNode,
    segment: &'static str,
    entry: &TemplateEntry,
    source: usize,
    merge: bool,
) -> Result<Inserted> {
    let existing = index_of(node, segment, false);
    match (&entry.kind, existing) {
        (TemplateEntryKind::Table(table), Some(index)) => {
            let existing = &mut node.children[index];
            if existing.is_table() {
                let same = table_fingerprint_eq(table, existing.table.as_ref().expect("table"));
                if same {
                    return Ok(Inserted::Applied);
                }
                if !merge {
                    return Err(project_error(
                        ErrorCode::TypeConflict,
                        format!(
                            "orthogonal violation: table `{}` re-declared with a different fingerprint",
                            segment
                        ),
                    ));
                }
                existing.table = Some(*table);
                existing.explicit = true;
                existing.source = source;
                Ok(Inserted::Applied)
            } else if merge {
                existing.table = Some(*table);
                existing.tier = None;
                existing.children.clear();
                existing.explicit = true;
                existing.source = source;
                Ok(Inserted::Applied)
            } else {
                Err(project_error(
                    ErrorCode::TypeConflict,
                    format!(
                        "orthogonal violation: `{}` is a domain and a table at the same path",
                        segment
                    ),
                ))
            }
        }
        (TemplateEntryKind::Table(table), None) => {
            node.children.push(FoldNode {
                name: segment.to_owned(),
                is_slot: false,
                tier: None,
                explicit: true,
                source,
                table: Some(*table),
                children: Vec::new(),
            });
            Ok(Inserted::Applied)
        }
        (TemplateEntryKind::Domain { tier }, Some(index)) => {
            let existing = &mut node.children[index];
            if existing.is_table() {
                if !merge {
                    return Err(project_error(
                        ErrorCode::TypeConflict,
                        format!(
                            "orthogonal violation: `{}` is a table and a domain at the same path",
                            segment
                        ),
                    ));
                }
                existing.table = None;
                existing.tier = *tier;
                existing.children.clear();
                existing.explicit = true;
                existing.source = source;
                return Ok(Inserted::Applied);
            }
            if tier.is_some() {
                existing.tier = *tier;
            }
            existing.explicit = true;
            if source > existing.source {
                existing.source = source;
            }
            Ok(Inserted::Applied)
        }
        (TemplateEntryKind::Domain { tier }, None) => {
            node.children.push(FoldNode {
                name: segment.to_owned(),
                is_slot: false,
                tier: *tier,
                explicit: true,
                source,
                table: None,
                children: Vec::new(),
            });
            Ok(Inserted::Applied)
        }
    }
}

/// Locates a direct child by name and namespace (`slot` = coordinate-slot
/// node, otherwise a name node). Returns the child index so call sites can
/// re-borrow after deciding between mutation and descent.
fn index_of(node: &FoldNode, name: &str, slot: bool) -> Option<usize> {
    let mut index = 0;
    while index < node.children.len() {
        let child = &node.children[index];
        if child.is_slot == slot && str_eq(&child.name, name) {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn coordinate_chain(coords: &[SlotDecl], slot: SlotName) -> Result<Vec<SlotName>> {
    let mut rev = Vec::new();
    let mut cur = slot;
    loop {
        let decl = coords
            .iter()
            .find(|decl| decl.slot.const_eq(cur))
            .ok_or_else(|| {
                project_error(
                    ErrorCode::TypeNotFound,
                    format!("coordinate slot `{}` is not declared", cur.0),
                )
            })?;
        rev.push(cur);
        match decl.parent {
            None => break,
            Some(parent) => cur = parent,
        }
        if rev.len() > MAX_SLOT_DEPTH {
            return Err(project_error(
                ErrorCode::TypeConflict,
                "coordinate chain exceeds the maximum slot depth",
            ));
        }
    }
    rev.reverse();
    Ok(rev)
}

/// Materializes the folded tree into domain and table types (post-order).
fn materialize(node: &FoldNode, version: u32, plan: &mut TypeRegistryPlan) -> Result<TypeId> {
    let mut children: Vec<(String, ChildKind, TypeId)> = Vec::new();
    for child in &node.children {
        if child.is_table() {
            let table = child.table.as_ref().expect("table node");
            let type_id = table_type_id(table.name, table.version);
            plan.table_types.push(TableType {
                type_id,
                name: Name::new(table.name)?,
                version: table.version,
                columns: columns_of(table)?,
                includes_type_ids: Vec::new(),
            });
            children.push((table.name.to_owned(), ChildKind::Table, type_id));
        } else {
            let child_id = materialize(child, version, plan)?;
            match child.tier {
                Some(tier) => {
                    let mut index = 0;
                    while index < tier.max_instances {
                        children.push((
                            format!("{}{}", tier.prefix, index),
                            ChildKind::Domain,
                            child_id,
                        ));
                        index += 1;
                    }
                }
                None => children.push((child.name.clone(), ChildKind::Domain, child_id)),
            }
        }
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));
    let mut window = 1;
    while window < children.len() {
        if children[window].0 == children[window - 1].0 {
            return Err(project_error(
                ErrorCode::IdentityCollision,
                format!(
                    "projected child name `{}` collides under `{}` (slot and name segments project into one namespace)",
                    children[window].0, node.name
                ),
            ));
        }
        window += 1;
    }
    let mut definitions = Vec::with_capacity(children.len());
    for (order, (name, kind, type_id)) in children.iter().enumerate() {
        definitions.push(ChildDefinition::new(
            u32::try_from(order).expect("child order fits u32"),
            Name::new(name.clone())?,
            *kind,
            *type_id,
            Cardinality::ZeroOrMore,
            InstanceMode::Shared,
        ));
    }
    let type_id = domain_type_id(&node.name, version);
    plan.domain_types.push(DomainType {
        type_id,
        name: Name::new(node.name.clone())?,
        version,
        instance_mode: InstanceMode::Shared,
        children: definitions,
        includes_type_ids: Vec::new(),
    });
    Ok(type_id)
}

fn columns_of(table: &TemplateTable) -> Result<Vec<ColumnDefinition>> {
    let mut out = Vec::with_capacity(table.columns.len());
    for (order, column) in table.columns.iter().enumerate() {
        let field = field_of(column)?;
        // in_hash = true mirrors the downstream translate bridge: every
        // projected column participates in the tree-space Merkle digest.
        out.push(ColumnDefinition::from_field(
            u32::try_from(order).expect("column order fits u32"),
            field,
            true,
        ));
    }
    Ok(out)
}

fn project_error(code: ErrorCode, message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(code, message)
}
