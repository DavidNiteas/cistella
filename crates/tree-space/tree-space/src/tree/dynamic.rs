//! Runtime-constructed tree nodes, chunk groups, and persistence annotations.
//!
//! This module is deliberately independent of view resolution and disk I/O.

use crate::block::RefId;
use crate::block::{BlockKind, Value};
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::tree::{AccessOut, Slot, TreeNode, ViewNode};
use crate::xpath::{Step, XPath};
use std::collections::BTreeMap;

/// Per-chunk derived metadata. It never participates in block identity.
pub type ChunkStats = BTreeMap<String, Value>;

/// One immutable chunk reference and its optional derived statistics.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkEntry {
    /// The chunk's content reference. Nested entries have no direct reference.
    pub ref_id: Option<RefId>,
    /// Runtime kind shared by every entry in the containing group.
    pub kind: BlockKind,
    /// Derived metadata used for pruning without loading the payload.
    pub stats: ChunkStats,
    /// Optional nested chunk group for large fan-out trees.
    pub nested: Option<Box<ChunkGroup>>,
}

impl ChunkEntry {
    /// Creates a leaf chunk entry.
    pub fn new(ref_id: RefId, kind: BlockKind, stats: ChunkStats) -> Self {
        Self {
            ref_id: Some(ref_id),
            kind,
            stats,
            nested: None,
        }
    }

    /// Creates a nested group entry.
    pub fn nested(group: ChunkGroup) -> Self {
        Self {
            kind: group.kind(),
            ref_id: None,
            stats: ChunkStats::new(),
            nested: Some(Box::new(group)),
        }
    }

    /// Returns this entry's direct reference, if it is a leaf entry.
    pub fn ref_id(&self) -> Option<RefId> {
        self.ref_id
    }
}

/// An ordered, same-kind group of chunks. Groups may contain nested groups.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkGroup {
    entries: Vec<ChunkEntry>,
    kind: Option<BlockKind>,
}

impl ChunkGroup {
    /// Creates an empty group.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the group's enforced kind, if it has entries.
    pub fn kind(&self) -> BlockKind {
        self.kind.clone().unwrap_or(BlockKind::Blob)
    }

    /// Returns the number of direct entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the group has no direct entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns direct entries in append order.
    pub fn entries(&self) -> &[ChunkEntry] {
        &self.entries
    }

    /// Rebuilds a group exactly from its parts (decode path; the caller has
    /// already validated kind consistency). An empty group carries the decoded
    /// kind so that re-encoding is byte-stable.
    pub(crate) fn from_parts(kind: Option<BlockKind>, entries: Vec<ChunkEntry>) -> Self {
        Self { entries, kind }
    }

    /// Appends a leaf chunk, rejecting mixed kinds.
    pub fn push(&mut self, entry: ChunkEntry) -> Result<()> {
        self.check_kind(entry.kind.clone())?;
        self.entries.push(entry);
        Ok(())
    }

    /// Appends a reference without opening its payload.
    pub fn push_ref(&mut self, ref_id: RefId, kind: BlockKind, stats: ChunkStats) -> Result<()> {
        self.push(ChunkEntry::new(ref_id, kind, stats))
    }

    /// Appends a nested same-kind group.
    pub fn push_group(&mut self, group: ChunkGroup) -> Result<()> {
        if let Some(kind) = &group.kind {
            self.check_kind(kind.clone())?;
        }
        self.push(ChunkEntry::nested(group))
    }

    /// Selects leaf references whose direct entry statistics match `pred`.
    pub fn select(&self, pred: impl Fn(&ChunkStats) -> bool) -> Vec<RefId> {
        let mut out = Vec::new();
        self.select_into(&pred, &mut out);
        out
    }

    fn select_into(&self, pred: &dyn Fn(&ChunkStats) -> bool, out: &mut Vec<RefId>) {
        for entry in &self.entries {
            if let Some(group) = &entry.nested {
                group.select_into(pred, out);
            } else if pred(&entry.stats) {
                if let Some(id) = entry.ref_id {
                    out.push(id);
                }
            }
        }
    }

    /// Returns all leaf references in tree order.
    pub fn refs(&self) -> Vec<RefId> {
        self.entries
            .iter()
            .flat_map(|entry| {
                entry
                    .nested
                    .as_deref()
                    .map_or_else(|| entry.ref_id.into_iter().collect(), ChunkGroup::refs)
            })
            .collect()
    }

    fn check_kind(&mut self, kind: BlockKind) -> Result<()> {
        match &self.kind {
            Some(expected) if expected != &kind => Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "chunk group entries must have one kind",
            )),
            None => {
                self.kind = Some(kind);
                Ok(())
            }
            Some(_) => Ok(()),
        }
    }
}

/// Legacy bridge used only by the v5 `TreeNode::set` compatibility method.
/// A runtime-constructed field in a [`DynamicNode`].
#[derive(Clone, Debug)]
pub enum DynamicField {
    /// Named child node.
    Node(DynamicNode),
    /// Inline scalar or referenced block slot.
    Slot(Slot),
    /// Ordered chunk group.
    Chunks(ChunkGroup),
    /// Deferred view node.
    View(ViewNode),
}
/// Persistence annotations carried alongside a tree instance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Persistence {
    ephemeral_prefixes: Vec<XPath>,
}

impl Persistence {
    /// Marks a subtree ephemeral. Prefixes are monotonic and deduplicated.
    pub fn mark_ephemeral(&mut self, prefix: XPath) {
        if !self.ephemeral_prefixes.contains(&prefix) {
            self.ephemeral_prefixes.push(prefix);
        }
    }
    /// Returns whether `path` is under an ephemeral prefix.
    pub fn is_ephemeral(&self, path: &XPath) -> bool {
        self.ephemeral_prefixes
            .iter()
            .any(|prefix| is_prefix(prefix, path))
    }
    /// Returns marked prefixes in insertion order.
    pub fn ephemeral_prefixes(&self) -> &[XPath] {
        &self.ephemeral_prefixes
    }
}

fn is_prefix(prefix: &XPath, path: &XPath) -> bool {
    path.steps().starts_with(prefix.steps())
}

/// A dynamic tree node with map multiplicity and no view semantics.
#[derive(Clone, Debug, Default)]
pub struct DynamicNode {
    fields: BTreeMap<String, DynamicField>,
    persistence: Persistence,
}

impl DynamicNode {
    /// Creates an empty node.
    pub fn new() -> Self {
        Self::default()
    }
    /// Inserts or replaces a named field.
    pub fn insert(&mut self, name: impl Into<String>, field: DynamicField) -> Option<DynamicField> {
        self.fields.insert(name.into(), field)
    }
    /// Returns a named field.
    pub fn get_field(&self, name: &str) -> Option<&DynamicField> {
        self.fields.get(name)
    }
    /// Returns mutable access to a named field.
    pub fn get_field_mut(&mut self, name: &str) -> Option<&mut DynamicField> {
        self.fields.get_mut(name)
    }
    /// Iterates over the named fields in key order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &DynamicField)> {
        self.fields
            .iter()
            .map(|(name, field)| (name.as_str(), field))
    }
    /// Marks a subtree ephemeral.
    pub fn mark_ephemeral(&mut self, prefix: XPath) {
        self.persistence.mark_ephemeral(prefix);
    }
    /// Returns persistence annotations.
    pub fn persistence(&self) -> &Persistence {
        &self.persistence
    }
    /// Returns mutable persistence annotations.
    pub fn persistence_mut(&mut self) -> &mut Persistence {
        &mut self.persistence
    }
}

impl TreeNode for DynamicNode {
    fn get(&self, xpath: &XPath) -> Result<AccessOut> {
        let (out, rest) = self.get_best_effort(xpath);
        if rest.is_root() {
            Ok(out)
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::XpathUnreachable,
                "xpath is not reachable",
            ))
        }
    }
    fn get_best_effort(&self, xpath: &XPath) -> (AccessOut, XPath) {
        if xpath.is_root() {
            return (AccessOut::Node(Box::new(self.clone())), XPath::root());
        }
        let Some((Step::Field(name), rest)) = xpath.take_first() else {
            return (AccessOut::Node(Box::new(self.clone())), xpath.clone());
        };
        let Some(field) = self.fields.get(&name) else {
            return (AccessOut::Node(Box::new(self.clone())), xpath.clone());
        };
        field_get(field, &rest)
    }
    fn set(&mut self, xpath: &XPath, value: Slot) -> Result<()> {
        let Some((Step::Field(name), rest)) = xpath.take_first() else {
            return Err(TreeSpaceError::new(
                ErrorCode::XpathUnreachable,
                "dynamic set requires a field path",
            ));
        };
        if !rest.is_root() {
            return Err(TreeSpaceError::new(
                ErrorCode::XpathUnreachable,
                "dynamic set only supports direct slots",
            ));
        }
        self.fields.insert(name, DynamicField::Slot(value));
        Ok(())
    }
    fn leaf_refs(&self) -> Vec<(XPath, RefId)> {
        self.fields
            .iter()
            .flat_map(|(name, field)| field_refs(field, XPath::root().field(name)))
            .collect()
    }
}

fn field_get(field: &DynamicField, rest: &XPath) -> (AccessOut, XPath) {
    match field {
        DynamicField::Node(node) => node.get_best_effort(rest),
        DynamicField::Slot(slot) => match slot {
            Slot::Inline(value) => (AccessOut::Value(value.clone()), rest.clone()),
            Slot::Ref(id) => (AccessOut::Ref(*id), rest.clone()),
        },
        DynamicField::Chunks(group) => {
            let Some((Step::Index(index), suffix)) = rest.take_first() else {
                return (AccessOut::Node(Box::new(group.clone())), rest.clone());
            };
            let Some(entry) = group.entries.get(index) else {
                return (AccessOut::Node(Box::new(group.clone())), rest.clone());
            };
            if let Some(nested) = &entry.nested {
                (AccessOut::Node(Box::new((**nested).clone())), suffix)
            } else {
                (
                    AccessOut::Ref(entry.ref_id.expect("leaf chunk entry")),
                    suffix,
                )
            }
        }
        DynamicField::View(view) => (AccessOut::Node(Box::new(view.clone())), rest.clone()),
    }
}

fn field_refs(field: &DynamicField, path: XPath) -> Vec<(XPath, RefId)> {
    match field {
        DynamicField::Node(node) => node
            .leaf_refs()
            .into_iter()
            .map(|(suffix, id)| (path.join(&suffix), id))
            .collect(),
        DynamicField::Slot(slot) => slot
            .ref_id()
            .into_iter()
            .map(|id| (path.clone(), id))
            .collect(),
        DynamicField::Chunks(group) => group
            .refs()
            .into_iter()
            .enumerate()
            .map(|(i, id)| (path.clone().index(i), id))
            .collect(),
        DynamicField::View(view) => view
            .refs()
            .into_iter()
            .map(|(suffix, id)| (path.join(&suffix), id))
            .collect(),
    }
}

impl TreeNode for ChunkGroup {
    fn get(&self, xpath: &XPath) -> Result<AccessOut> {
        let (out, rest) = self.get_best_effort(xpath);
        if rest.is_root() {
            Ok(out)
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::XpathUnreachable,
                "chunk xpath is not reachable",
            ))
        }
    }
    fn get_best_effort(&self, xpath: &XPath) -> (AccessOut, XPath) {
        if xpath.is_root() {
            return (AccessOut::Node(Box::new(self.clone())), XPath::root());
        }
        field_get(&DynamicField::Chunks(self.clone()), xpath)
    }
    fn set(&mut self, _xpath: &XPath, _value: Slot) -> Result<()> {
        Err(TreeSpaceError::new(
            ErrorCode::XpathUnreachable,
            "chunk groups are append-only",
        ))
    }
    fn leaf_refs(&self) -> Vec<(XPath, RefId)> {
        self.refs()
            .into_iter()
            .enumerate()
            .map(|(i, id)| (XPath::root().index(i), id))
            .collect()
    }
}

/// Collects the memory-GC roots from all live tree instances, including ephemeral paths.
pub fn memory_gc_roots<'a>(instances: impl IntoIterator<Item = &'a dyn TreeNode>) -> Vec<RefId> {
    let mut roots = std::collections::BTreeSet::new();
    for instance in instances {
        roots.extend(instance.leaf_refs().into_iter().map(|(_, id)| id));
    }
    roots.into_iter().collect()
}
