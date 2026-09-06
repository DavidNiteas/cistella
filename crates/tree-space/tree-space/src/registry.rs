//! Pure in-memory type and node registry.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::ids::{NodeId, PathHash, TableId, TypeId};
use crate::path::{RenameEntry, RenamePlan, TablePath};
use crate::types::{ChildDefinition, ChildKind, DomainType, InstanceMode, TableType};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Registry entry for a domain-node instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainNode {
    /// Stable node identity.
    pub node_id: NodeId,
    /// Canonical node path.
    pub path: TablePath,
    /// Domain type identity.
    pub type_id: TypeId,
    /// Parent node, absent only for roots.
    pub parent_node_id: Option<NodeId>,
    /// The parent child order, absent only for roots.
    pub child_order: Option<u32>,
    /// Whether this node may be renamed.
    pub instance_mode: InstanceMode,
    /// Fast hash, or zero after a collision.
    pub path_hash: PathHash,
    /// True if hash indexing was deliberately disabled for this path.
    pub degraded: bool,
}

/// Registry entry for a table instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableInstance {
    /// Stable physical table identity.
    pub table_id: TableId,
    /// Canonical table path.
    pub path: TablePath,
    /// Declared table type.
    pub type_id: TypeId,
    /// Type version at creation.
    pub type_version: u32,
    /// Parent node.
    pub parent_node_id: NodeId,
    /// Parent child order.
    pub child_order: u32,
    /// Whether this table may be renamed.
    pub instance_mode: InstanceMode,
    /// Fast hash, or zero after a collision.
    pub path_hash: PathHash,
    /// True if hash indexing was deliberately disabled for this path.
    pub degraded: bool,
}

/// Deterministic, IO-free registry export used by manifest and golden tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryExport {
    /// Domain definitions ordered by `TypeId`.
    pub domain_types: Vec<DomainType>,
    /// Table definitions ordered by `TypeId`.
    pub table_types: Vec<TableType>,
}

/// Pure-memory registry with deterministic export and collision-safe indexes.
#[derive(Clone)]
pub struct TypeRegistry {
    domain_types: BTreeMap<TypeId, DomainType>,
    table_types: BTreeMap<TypeId, TableType>,
    nodes: BTreeMap<NodeId, DomainNode>,
    tables: BTreeMap<TableId, TableInstance>,
    paths: BTreeMap<TablePath, PathTarget>,
    hash_index: HashMap<PathHash, TablePath>,
    path_hash: Arc<dyn Fn(&TablePath) -> PathHash + Send + Sync>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathTarget {
    Node(NodeId),
    Table(TableId),
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeRegistry {
    /// Creates an empty registry using the v3 XXH3 path hash.
    pub fn new() -> Self {
        Self::with_path_hasher(Arc::new(|path| path.hash(3)))
    }

    /// Creates an empty registry with a controlled path hasher for collision tests.
    pub fn with_path_hasher(path_hash: Arc<dyn Fn(&TablePath) -> PathHash + Send + Sync>) -> Self {
        Self {
            domain_types: BTreeMap::new(),
            table_types: BTreeMap::new(),
            nodes: BTreeMap::new(),
            tables: BTreeMap::new(),
            paths: BTreeMap::new(),
            hash_index: HashMap::new(),
            path_hash,
        }
    }

    /// Registers a domain type after complete structural and composition validation.
    pub fn register_domain_type(&mut self, definition: DomainType) -> Result<()> {
        validate_domain_shape(&definition)?;
        if let Some(existing) = self.domain_types.get(&definition.type_id) {
            if existing != &definition {
                return Err(type_conflict(&definition.name.to_string()));
            }
            return Ok(());
        }
        if self.domain_types.values().any(|existing| {
            existing.name == definition.name
                && existing.version == definition.version
                && existing != &definition
        }) {
            return Err(type_conflict(&definition.name.to_string()));
        }
        let id = definition.type_id;
        self.domain_types.insert(id, definition);
        if let Err(error) = self.validate_domain_composition() {
            self.domain_types.remove(&id);
            return Err(error);
        }
        Ok(())
    }

    /// Registers a table type after complete structural and composition validation.
    pub fn register_table_type(&mut self, definition: TableType) -> Result<()> {
        validate_table_shape(&definition)?;
        if let Some(existing) = self.table_types.get(&definition.type_id) {
            if existing != &definition {
                return Err(type_conflict(&definition.name.to_string()));
            }
            return Ok(());
        }
        if self.table_types.values().any(|existing| {
            existing.name == definition.name
                && existing.version == definition.version
                && existing != &definition
        }) {
            return Err(type_conflict(&definition.name.to_string()));
        }
        let id = definition.type_id;
        self.table_types.insert(id, definition);
        if let Err(error) = self.validate_table_composition() {
            self.table_types.remove(&id);
            return Err(error);
        }
        Ok(())
    }

    /// Exports definitions in stable identity order without performing I/O.
    pub fn export(&self) -> RegistryExport {
        RegistryExport {
            domain_types: self.domain_types.values().cloned().collect(),
            table_types: self.table_types.values().cloned().collect(),
        }
    }

    /// Returns a registered domain type.
    pub fn domain_type(&self, type_id: TypeId) -> Result<&DomainType> {
        self.domain_types
            .get(&type_id)
            .ok_or_else(|| missing_type(type_id))
    }

    /// Returns a registered table type.
    pub fn table_type(&self, type_id: TypeId) -> Result<&TableType> {
        self.table_types
            .get(&type_id)
            .ok_or_else(|| missing_type(type_id))
    }

    /// Returns the deterministic transitive child expansion of a domain type.
    pub fn expanded_children(&self, type_id: TypeId) -> Result<Vec<ChildDefinition>> {
        let mut visited = BTreeSet::new();
        self.expand_domain(type_id, &mut visited)
    }

    /// Returns the deterministic transitive column expansion of a table type.
    pub fn expanded_columns(&self, type_id: TypeId) -> Result<Vec<crate::types::ColumnDefinition>> {
        let mut visited = BTreeSet::new();
        self.expand_table(type_id, &mut visited)
    }

    /// Creates a root domain node.
    pub fn create_root(&mut self, path: TablePath, type_id: TypeId) -> Result<NodeId> {
        let ty = self.domain_type(type_id)?;
        let node = DomainNode {
            node_id: NodeId::new(),
            path,
            type_id,
            parent_node_id: None,
            child_order: None,
            instance_mode: ty.instance_mode,
            path_hash: PathHash::DEGRADED,
            degraded: false,
        };
        self.insert_node(node)
    }

    /// Creates a child domain node after validating parent type, name, and cardinality.
    pub fn create_child_node(
        &mut self,
        parent_node_id: NodeId,
        path: TablePath,
        type_id: TypeId,
    ) -> Result<NodeId> {
        let parent = self.nodes.get(&parent_node_id).cloned().ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::DanglingReference, "parent node does not exist")
        })?;
        let child = self.match_child(&parent, &path, type_id, ChildKind::Domain)?;
        let node = DomainNode {
            node_id: NodeId::new(),
            path,
            type_id,
            parent_node_id: Some(parent_node_id),
            child_order: Some(child.child_order),
            instance_mode: child.instance_mode,
            path_hash: PathHash::DEGRADED,
            degraded: false,
        };
        self.insert_node(node)
    }

    /// Registers a leaf table instance under a domain node.
    pub fn create_table(
        &mut self,
        parent_node_id: NodeId,
        path: TablePath,
        type_id: TypeId,
    ) -> Result<TableId> {
        let parent = self.nodes.get(&parent_node_id).cloned().ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::DanglingReference, "parent node does not exist")
        })?;
        let child = self.match_child(&parent, &path, type_id, ChildKind::Table)?;
        let version = self.table_type(type_id)?.version;
        let table = TableInstance {
            table_id: TableId::new(),
            path,
            type_id,
            type_version: version,
            parent_node_id,
            child_order: child.child_order,
            instance_mode: child.instance_mode,
            path_hash: PathHash::DEGRADED,
            degraded: false,
        };
        self.insert_table(table)
    }

    /// Looks up a node or table by full canonical path; never trusts a hash alone.
    pub fn resolve_path(
        &self,
        path: &TablePath,
    ) -> Option<(Option<&DomainNode>, Option<&TableInstance>)> {
        match self.paths.get(path) {
            Some(PathTarget::Node(id)) => Some((self.nodes.get(id), None)),
            Some(PathTarget::Table(id)) => Some((None, self.tables.get(id))),
            None => None,
        }
    }

    /// Looks up by hash only after checking the complete path supplied by the caller.
    pub fn resolve_hash(
        &self,
        hash: PathHash,
        full_path: &TablePath,
    ) -> Option<(Option<&DomainNode>, Option<&TableInstance>)> {
        (hash != PathHash::DEGRADED && self.hash_index.get(&hash) == Some(full_path))
            .then(|| self.resolve_path(full_path))
            .flatten()
    }

    /// Validates all ExactlyOne and OneOrMore definitions for the registry state.
    pub fn validate_cardinality(&self) -> Result<()> {
        for node in self.nodes.values() {
            for child in self.expanded_children(node.type_id)? {
                let count = self.child_count(node.node_id, &child);
                if !child.cardinality.accepts(count) {
                    return Err(TreeSpaceError::new(
                        ErrorCode::CardinalityViolation,
                        "required child count is not satisfied",
                    )
                    .with_context("path", node.path.to_string())
                    .with_context("child", child.name.to_string()));
                }
            }
        }
        Ok(())
    }

    /// Plans a deterministic logical subtree rename without modifying registry state.
    pub fn plan_rename(&self, source: &TablePath, destination: &TablePath) -> Result<RenamePlan> {
        let source_target = self.paths.get(source).ok_or_else(|| {
            TreeSpaceError::new(ErrorCode::PathInvalid, "rename source does not exist")
        })?;
        let exclusive = match source_target {
            PathTarget::Node(id) => self.nodes[id].instance_mode,
            PathTarget::Table(id) => self.tables[id].instance_mode,
        };
        if exclusive == InstanceMode::Exclusive {
            return Err(TreeSpaceError::new(
                ErrorCode::OwnershipDenied,
                "exclusive instance cannot be renamed",
            )
            .with_context("path", source.to_string()));
        }
        let mut entries = self
            .paths
            .keys()
            .filter(|path| path.starts_with(source))
            .map(|old_path| {
                let new_path = old_path
                    .replace_prefix(source, destination)
                    .expect("prefix was filtered");
                let old_hash = (self.path_hash)(old_path);
                let new_hash = (self.path_hash)(&new_path);
                RenameEntry {
                    old_path: old_path.clone(),
                    new_path,
                    old_hash,
                    new_hash,
                }
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.old_path.cmp(&b.old_path));
        let old_paths = entries
            .iter()
            .map(|entry| entry.old_path.clone())
            .collect::<BTreeSet<_>>();
        if entries.iter().any(|entry| {
            self.paths.contains_key(&entry.new_path) && !old_paths.contains(&entry.new_path)
        }) {
            return Err(TreeSpaceError::new(
                ErrorCode::IdentityCollision,
                "rename destination conflicts with existing path",
            )
            .with_context("path", destination.to_string()));
        }
        Ok(RenamePlan { entries })
    }

    /// Applies a previously validated rename plan atomically in memory.
    pub fn apply_rename(&mut self, plan: &RenamePlan) -> Result<()> {
        let snapshot = self.clone();
        for entry in &plan.entries {
            let target = self.paths.remove(&entry.old_path).ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::DanglingReference,
                    "rename plan source disappeared",
                )
            })?;
            match target {
                PathTarget::Node(id) => {
                    self.nodes
                        .get_mut(&id)
                        .expect("path index points to node")
                        .path = entry.new_path.clone()
                }
                PathTarget::Table(id) => {
                    self.tables
                        .get_mut(&id)
                        .expect("path index points to table")
                        .path = entry.new_path.clone()
                }
            }
            self.paths.insert(entry.new_path.clone(), target);
        }
        self.rebuild_hash_index();
        if let Err(error) = self.ensure_unique_paths() {
            *self = snapshot;
            return Err(error);
        }
        Ok(())
    }

    /// Returns domain nodes in deterministic path order.
    pub fn nodes(&self) -> Vec<&DomainNode> {
        self.nodes.values().collect()
    }

    /// Returns table instances in deterministic path order.
    pub fn tables(&self) -> Vec<&TableInstance> {
        self.tables.values().collect()
    }

    fn insert_node(&mut self, mut node: DomainNode) -> Result<NodeId> {
        if self.paths.contains_key(&node.path) {
            return Err(TreeSpaceError::new(
                ErrorCode::IdentityCollision,
                "path already registered",
            )
            .with_context("path", node.path.to_string()));
        }
        self.assign_hash(&node.path, &mut node.path_hash, &mut node.degraded);
        let id = node.node_id;
        self.paths.insert(node.path.clone(), PathTarget::Node(id));
        self.nodes.insert(id, node);
        Ok(id)
    }

    fn insert_table(&mut self, mut table: TableInstance) -> Result<TableId> {
        if self.paths.contains_key(&table.path) {
            return Err(TreeSpaceError::new(
                ErrorCode::IdentityCollision,
                "path already registered",
            )
            .with_context("path", table.path.to_string()));
        }
        self.assign_hash(&table.path, &mut table.path_hash, &mut table.degraded);
        let id = table.table_id;
        self.paths.insert(table.path.clone(), PathTarget::Table(id));
        self.tables.insert(id, table);
        Ok(id)
    }

    fn assign_hash(&mut self, path: &TablePath, hash: &mut PathHash, degraded: &mut bool) {
        let candidate = (self.path_hash)(path);
        if candidate == PathHash::DEGRADED || self.hash_index.contains_key(&candidate) {
            *hash = PathHash::DEGRADED;
            *degraded = true;
        } else {
            self.hash_index.insert(candidate, path.clone());
            *hash = candidate;
        }
    }

    fn rebuild_hash_index(&mut self) {
        self.hash_index.clear();
        let entries = self.paths.keys().cloned().collect::<Vec<_>>();
        for path in entries {
            let candidate = (self.path_hash)(&path);
            let duplicate =
                candidate == PathHash::DEGRADED || self.hash_index.contains_key(&candidate);
            match self.paths[&path] {
                PathTarget::Node(id) => {
                    let node = self.nodes.get_mut(&id).expect("node exists");
                    node.path_hash = if duplicate {
                        PathHash::DEGRADED
                    } else {
                        candidate
                    };
                    node.degraded = duplicate;
                }
                PathTarget::Table(id) => {
                    let table = self.tables.get_mut(&id).expect("table exists");
                    table.path_hash = if duplicate {
                        PathHash::DEGRADED
                    } else {
                        candidate
                    };
                    table.degraded = duplicate;
                }
            }
            if !duplicate {
                self.hash_index.insert(candidate, path);
            }
        }
    }

    fn ensure_unique_paths(&self) -> Result<()> {
        if self.paths.len() == self.nodes.len() + self.tables.len() {
            Ok(())
        } else {
            Err(TreeSpaceError::new(
                ErrorCode::IdentityCollision,
                "path registry is inconsistent",
            ))
        }
    }

    fn match_child(
        &self,
        parent: &DomainNode,
        path: &TablePath,
        type_id: TypeId,
        kind: ChildKind,
    ) -> Result<ChildDefinition> {
        let child = self
            .expanded_children(parent.type_id)?
            .into_iter()
            .find(|child| {
                child.child_kind == kind
                    && child.child_type_id == type_id
                    && child.name == *path.name()
            })
            .ok_or_else(|| {
                TreeSpaceError::new(
                    ErrorCode::TypeConflict,
                    "child does not match parent type definition",
                )
                .with_context("path", path.to_string())
            })?;
        let count = self.child_count(parent.node_id, &child);
        if !child.cardinality.permits_additional(count) {
            return Err(TreeSpaceError::new(
                ErrorCode::CardinalityViolation,
                "maximum child count reached",
            )
            .with_context("path", parent.path.to_string()));
        }
        Ok(child)
    }

    fn child_count(&self, parent: NodeId, child: &ChildDefinition) -> usize {
        let nodes = self
            .nodes
            .values()
            .filter(|node| {
                node.parent_node_id == Some(parent) && node.child_order == Some(child.child_order)
            })
            .count();
        let tables = self
            .tables
            .values()
            .filter(|table| {
                table.parent_node_id == parent && table.child_order == child.child_order
            })
            .count();
        match child.child_kind {
            ChildKind::Domain => nodes,
            ChildKind::Table => tables,
        }
    }

    fn validate_domain_composition(&self) -> Result<()> {
        for id in self.domain_types.keys().copied().collect::<Vec<_>>() {
            self.expand_domain(id, &mut BTreeSet::new())?;
        }
        Ok(())
    }
    fn validate_table_composition(&self) -> Result<()> {
        for id in self.table_types.keys().copied().collect::<Vec<_>>() {
            self.expand_table(id, &mut BTreeSet::new())?;
        }
        Ok(())
    }

    fn expand_domain(
        &self,
        id: TypeId,
        visiting: &mut BTreeSet<TypeId>,
    ) -> Result<Vec<ChildDefinition>> {
        if !visiting.insert(id) {
            return Err(TreeSpaceError::new(
                ErrorCode::CompositionCycle,
                "domain composition cycle",
            )
            .with_context("type", id.to_string()));
        }
        let definition = self.domain_type(id)?;
        let mut out = definition.children.clone();
        for include in &definition.includes_type_ids {
            out.extend(self.expand_domain(*include, visiting)?);
        }
        visiting.remove(&id);
        out.sort_by(|left, right| {
            (left.child_kind, &left.name, left.child_type_id).cmp(&(
                right.child_kind,
                &right.name,
                right.child_type_id,
            ))
        });
        let mut names = BTreeMap::new();
        for child in &out {
            if let Some(previous) =
                names.insert(child.name.clone(), (child.child_kind, child.child_type_id))
            {
                if previous != (child.child_kind, child.child_type_id) {
                    return Err(TreeSpaceError::new(
                        ErrorCode::TypeConflict,
                        "composition has incompatible child name",
                    )
                    .with_context("child", child.name.to_string()));
                }
            }
        }
        Ok(out)
    }

    fn expand_table(
        &self,
        id: TypeId,
        visiting: &mut BTreeSet<TypeId>,
    ) -> Result<Vec<crate::types::ColumnDefinition>> {
        if !visiting.insert(id) {
            return Err(TreeSpaceError::new(
                ErrorCode::CompositionCycle,
                "table composition cycle",
            )
            .with_context("type", id.to_string()));
        }
        let definition = self.table_type(id)?;
        let mut out = definition.columns.clone();
        for include in &definition.includes_type_ids {
            out.extend(self.expand_table(*include, visiting)?);
        }
        visiting.remove(&id);
        out.sort_by(|left, right| {
            (left.col_order, &left.name).cmp(&(right.col_order, &right.name))
        });
        let mut names = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for column in &out {
            if !names.insert(column.name.clone()) || !orders.insert(column.col_order) {
                return Err(TreeSpaceError::new(
                    ErrorCode::TypeConflict,
                    "composition has duplicate table column",
                )
                .with_context("column", column.name.to_string()));
            }
        }
        Ok(out)
    }
}

fn validate_domain_shape(definition: &DomainType) -> Result<()> {
    let mut names = BTreeSet::new();
    for (expected, child) in definition.children.iter().enumerate() {
        if child.child_order != expected as u32 || !names.insert(child.name.clone()) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "child order must be unique and continuous",
            )
            .with_context("type", definition.name.to_string()));
        }
    }
    Ok(())
}
fn validate_table_shape(definition: &TableType) -> Result<()> {
    let mut names = BTreeSet::new();
    for (expected, column) in definition.columns.iter().enumerate() {
        if column.col_order != expected as u32 || !names.insert(column.name.clone()) {
            return Err(TreeSpaceError::new(
                ErrorCode::TypeConflict,
                "column order must be unique and continuous",
            )
            .with_context("type", definition.name.to_string()));
        }
        let field = column.field()?;
        if field.name() != column.name.as_str() || field.is_nullable() != column.nullable {
            return Err(TreeSpaceError::new(
                ErrorCode::SchemaMismatch,
                "column Field IPC disagrees with declaration",
            )
            .with_context("column", column.name.to_string()));
        }
    }
    Ok(())
}
fn missing_type(id: TypeId) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::TypeNotFound, "type is not registered")
        .with_context("type", id.to_string())
}
fn type_conflict(name: &str) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::TypeConflict,
        "same name and version has different structure",
    )
    .with_context("type", name)
}
