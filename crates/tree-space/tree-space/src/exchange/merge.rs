//! Merge primitives for fine-grained tree edits (protocol
//! `交换协议/01-目标与设计.md` §5.4) + synchronization scope (01 §5.1) +
//! reachable-block enumeration (01 §6 枚举可达). Pure functions — no disk/IO.
//!
//! E-3 lands the behavior over the frozen E-1/E-2 types
//! (`02-施工路线图.md` §7): `SyncScope` (全树/树片段/桶块), the built-in merge
//! operations (覆盖/只添加/只修改/只删除/任意组合), `merge_image` (load whole →
//! edit by scope → write whole back) and `reachable_refs` (the pull-fetch
//! traversal base).
//!
//! Merge unit = a **field** (subtree). Fine-grained recursion enters a `Node`
//! only when its children carry stable `Named`/`Keyed` locators; `Positioned`
//! children (Vec elements, chunk entries, view inputs) and
//! `ChunkGroup`/`ChunkEntry`/`View`/`Inline`/`Ref` contents are treated as
//! atomic fields (no per-entry merge — avoids ordinal reflow, same granularity
//! as `prune_image`, 02 §7.6).

use crate::block::RefId;
use crate::error::Result;
use crate::index::canonical_xpath_bytes;
use crate::layout::tb::{hex16, image_leaf_refs, is_pruned_path};
use crate::tree::codec::{ImageContent, ImageField, Locator, TreeImage};
use crate::xpath::XPath;
use std::collections::{BTreeMap, BTreeSet};

/// Synchronization / merge scope (01 §5.1 范围可指定: 全树 / 树片段 / 桶的若干块).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncScope {
    /// The whole tree (and its reachable bucket blocks).
    FullTree,
    /// A tree fragment: the subtree under each xpath prefix. Prefix matching
    /// reuses the collision-safe canonical-byte rule
    /// ([`crate::layout::tb::is_pruned_path`]; zero new dependencies, no regex,
    /// 02 §7.6). An empty prefix list is the empty scope — nothing is touched.
    TreeFragment(Vec<XPath>),
    /// A subset of bucket blocks (bucket-level sync; has no tree merge).
    Blocks(Vec<RefId>),
}

/// The built-in merge operations (01 §5.4): overwrite (simplest), add-only,
/// modify-only, delete-only, and arbitrary combinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeOp {
    /// Overwrite: in-scope destination subtrees converge to the source ("复写").
    Overwrite,
    /// Add-only: source-only fields/leaves are inserted; existing ones untouched.
    AddOnly,
    /// Modify-only: common fields/leaves are replaced; nothing added/deleted.
    ModifyOnly,
    /// Delete-only: destination-only fields/leaves are removed; nothing added
    /// or modified.
    DeleteOnly,
    /// Arbitrary combination of the three primitive behaviors.
    Combine(MergeFlags),
}

/// Bitmask of the primitive merge behaviors (01 §5.4 "任意组合").
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct MergeFlags {
    /// Insert source-only fields/leaves.
    pub add: bool,
    /// Replace common fields/leaves with the source's.
    pub modify: bool,
    /// Remove destination-only fields/leaves.
    pub delete: bool,
}

impl MergeOp {
    /// Whether this op inserts source-only fields/leaves (02 §7.3 语义表「仅源有」).
    pub fn adds(&self) -> bool {
        match self {
            MergeOp::Overwrite | MergeOp::AddOnly => true,
            MergeOp::ModifyOnly | MergeOp::DeleteOnly => false,
            MergeOp::Combine(flags) => flags.add,
        }
    }
    /// Whether this op replaces common fields/leaves with the source's
    /// (02 §7.3 语义表「双侧都有，其他（原子字段）」).
    pub fn modifies(&self) -> bool {
        match self {
            MergeOp::Overwrite | MergeOp::ModifyOnly => true,
            MergeOp::AddOnly | MergeOp::DeleteOnly => false,
            MergeOp::Combine(flags) => flags.modify,
        }
    }
    /// Whether this op removes destination-only fields/leaves
    /// (02 §7.3 语义表「仅目的有」).
    pub fn deletes(&self) -> bool {
        match self {
            MergeOp::Overwrite | MergeOp::DeleteOnly => true,
            MergeOp::AddOnly | MergeOp::ModifyOnly => false,
            MergeOp::Combine(flags) => flags.delete,
        }
    }
}

/// A merge operation plus its scope (01 §5.4 op + xpath range).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergeSpec {
    /// The merge operation applied inside the scope.
    pub op: MergeOp,
    /// The scope the operation applies to.
    pub scope: SyncScope,
}

/// Applies a merge spec to combine a destination tree image with a source tree
/// image, returning the merged image (01 §5.4: load whole → edit by scope →
/// write whole back; the tree's physical operation is always whole-tree
/// replacement, 02 §7.2). The returned image is validated by `encode`/`commit`
/// on replace.
///
/// Scope semantics (a field at xpath `path` is *in scope* when `path` is a
/// descendant-or-self of any prefix — `is_pruned_path` on the canonical bytes):
///
/// - `FullTree` → every field is in scope;
/// - `TreeFragment(prefixes)` → `is_pruned_path(canonical_xpath_bytes(path),
///   &prefixes.map(canonical_xpath_bytes))`;
/// - `Blocks(_)` → no tree merge (block-level sync): the destination image is
///   returned unchanged.
///
/// Merge unit = a field (subtree). At an in-scope field position (02 §7.3 语义表):
///
/// | 场景（作用域内） | Overwrite | AddOnly | ModifyOnly | DeleteOnly | Combine(f) |
/// |---|---|---|---|---|---|
/// | 双侧都有，且都是 `Node`（命名/键控） | 递归 | 递归 | 递归 | 递归 | 递归 |
/// | 双侧都有，其他（原子字段） | 取源 | 保留目的 | 取源 | 保留目的 | `f.modify` ? 取源 : 保留目的 |
/// | 仅目的有 | 删除 | 保留 | 保留 | 删除 | `f.delete` ? 删除 : 保留 |
/// | 仅源有 | 插入 | 插入 | 跳过 | 跳过 | `f.add` ? 插入 : 跳过 |
///
/// 「取源/插入」= 源字段深拷贝；「保留目的」= 目的字段克隆；「删除」= 丢弃。
/// 作用域外的字段一律保留目的（不递归、不增删改）。递归仅进入 `Node` 子节点且
/// 子节点为 `Named`/`Keyed` locator（稳定寻址）；`Positioned` 子节点（Vec 元素、
/// chunk 条目、view 输入）与 `ChunkGroup`/`ChunkEntry`/`View`/`Inline`/`Ref`
/// 内容均按原子字段处理；前缀落在这类位置型容器内部时整棵容器按 modify 语义替换
/// (02 §7.7 裁定 ⑦)。双侧 `Node` 与非 `Node` 类型不匹配时按原子字段处理。
pub fn merge_image(dest: &TreeImage, source: &TreeImage, spec: &MergeSpec) -> Result<TreeImage> {
    // Bucket-level sync has no tree merge (02 §7.3 SyncScope::Blocks).
    if matches!(spec.scope, SyncScope::Blocks(_)) {
        return Ok(dest.clone());
    }
    let filter = scope_filter(&spec.scope);
    let merged = merge_node(
        dest.children(),
        source.children(),
        spec,
        &XPath::root(),
        &filter,
    )?;
    Ok(TreeImage::new(merged))
}

/// Enumerates the reachable block set of an image, restricted to `scope`
/// (01 §6 枚举可达 + 02 §7.2 只抓范围内可达块). Drives the pull fetch:
///
/// - `FullTree` → every leaf ref of [`crate::layout::tb::image_leaf_refs`];
/// - `TreeFragment(prefixes)` → only the refs whose xpath is under a prefix;
/// - `Blocks(ids)` → the ids themselves (they *are* the scope).
pub fn reachable_refs(image: &TreeImage, scope: &SyncScope) -> Result<BTreeSet<RefId>> {
    match scope {
        SyncScope::FullTree => {
            let mut reachable = BTreeSet::new();
            for (_, id) in image_leaf_refs(image)? {
                reachable.insert(id);
            }
            Ok(reachable)
        }
        SyncScope::TreeFragment(prefixes) => {
            let prefix_bytes = prefixes
                .iter()
                .map(canonical_xpath_bytes)
                .collect::<Vec<_>>();
            let mut reachable = BTreeSet::new();
            for (xpath, id) in image_leaf_refs(image)? {
                if is_pruned_path(&canonical_xpath_bytes(&xpath), &prefix_bytes) {
                    reachable.insert(id);
                }
            }
            Ok(reachable)
        }
        SyncScope::Blocks(ids) => Ok(ids.iter().copied().collect()),
    }
}

// ---------------------------------------------------------------------------
// Scope plumbing
// ---------------------------------------------------------------------------

/// The precomputed scope shape shared by the recursive merge: `All` (FullTree),
/// `Prefixes` (canonical prefix bytes), or `None` (empty fragment / Blocks —
/// nothing is in scope).
enum ScopeFilter {
    All,
    Prefixes(Vec<Vec<u8>>),
    None,
}

fn scope_filter(scope: &SyncScope) -> ScopeFilter {
    match scope {
        SyncScope::FullTree => ScopeFilter::All,
        SyncScope::TreeFragment(prefixes) if prefixes.is_empty() => ScopeFilter::None,
        SyncScope::TreeFragment(prefixes) => {
            ScopeFilter::Prefixes(prefixes.iter().map(canonical_xpath_bytes).collect())
        }
        SyncScope::Blocks(_) => ScopeFilter::None,
    }
}

/// Whether `path` lies inside the scope (descendant-or-self of a prefix, 02
/// §7.3): the decision predicate for per-field merge actions.
fn in_scope(filter: &ScopeFilter, path: &XPath) -> bool {
    match filter {
        ScopeFilter::All => true,
        ScopeFilter::None => false,
        ScopeFilter::Prefixes(prefixes) => is_pruned_path(&canonical_xpath_bytes(path), prefixes),
    }
}

/// Whether the subtree under `path` is touched by the scope: `path` itself or
/// any descendant is in scope (descendant-or-self of a prefix), or `path` is an
/// ancestor of an in-scope path (ancestor-or-self). This is the recursion entry
/// predicate: both a prefix's descendants (e.g. `/a/b/p` under prefix `/a/b`)
/// and the ancestors leading down to a prefix (e.g. `/a` under prefix `/a/b`)
/// are recursed into, while unrelated subtrees are kept untouched.
fn touched(filter: &ScopeFilter, path: &XPath) -> bool {
    match filter {
        ScopeFilter::All => true,
        ScopeFilter::None => false,
        ScopeFilter::Prefixes(prefixes) => {
            let bytes = canonical_xpath_bytes(path);
            prefixes
                .iter()
                .any(|prefix| prefix.starts_with(&bytes) || bytes.starts_with(prefix))
        }
    }
}

// ---------------------------------------------------------------------------
// Recursive merge
// ---------------------------------------------------------------------------

/// Merges two nodes' children (the merge unit is a field, 02 §7.3). When the
/// children carry stable `Named`/`Keyed` locators the merge recurses
/// fine-grained by key; otherwise (positioned/mixed children, e.g. a root
/// positional container) the node is an atomic container whose whole children
/// list follows the op's modify rule inside the shadow.
fn merge_node(
    dest: &[ImageField],
    source: &[ImageField],
    spec: &MergeSpec,
    path: &XPath,
    filter: &ScopeFilter,
) -> Result<Vec<ImageField>> {
    if fine_grained(dest, source) {
        merge_children(dest, source, spec, path, filter)
    } else if touched(filter, path) && spec.op.modifies() {
        // Positional/mixed-children node pair: the whole container is the
        // atomic unit (02 §7.6/§7.7 ⑦); a prefix inside it swaps it whole.
        Ok(source.to_vec())
    } else {
        Ok(dest.to_vec())
    }
}

/// Merges a pair of same-named/same-keyed fields at `path`.
///
/// Untouched fields (neither in scope nor on the way to an in-scope field) are
/// kept from the destination. `Node` pairs with stable children recurse when
/// touched; container pairs (positioned `Node`/`ChunkGroup`/`View`) swap whole
/// on modify ops; atomic fields (`Inline`/`Ref`/`ChunkEntry` and mismatched
/// pairs) follow the op's modify rule only when in scope.
fn merge_field(
    dest: &ImageField,
    source: &ImageField,
    spec: &MergeSpec,
    path: &XPath,
    filter: &ScopeFilter,
) -> Result<ImageField> {
    match (&dest.content, &source.content) {
        (ImageContent::Node(dc), ImageContent::Node(sc)) if fine_grained(dc, sc) => {
            if touched(filter, path) {
                let children = merge_node(dc, sc, spec, path, filter)?;
                Ok(ImageField {
                    locator: dest.locator.clone(),
                    content: ImageContent::Node(children),
                })
            } else {
                Ok(dest.clone())
            }
        }
        (dc, sc) if container_pair(dc, sc) => {
            // Positional/mixed-children containers (Node/ChunkGroup/View) are
            // atomic units; a prefix inside them swaps the whole container
            // (02 §7.6, §7.7 裁定 ⑦).
            if touched(filter, path) && spec.op.modifies() {
                Ok(source.clone())
            } else {
                Ok(dest.clone())
            }
        }
        _ => {
            // Atomic fields: source value converges only when in scope.
            if in_scope(filter, path) && spec.op.modifies() {
                Ok(source.clone())
            } else {
                Ok(dest.clone())
            }
        }
    }
}

/// Fine-grained child merge over a keyed map (Named by name / Keyed by 16-byte
/// key). Dest-only fields are deleted only when in scope and the op deletes;
/// source-only fields are inserted only when in scope and the op adds.
fn merge_children(
    dest: &[ImageField],
    source: &[ImageField],
    spec: &MergeSpec,
    parent: &XPath,
    filter: &ScopeFilter,
) -> Result<Vec<ImageField>> {
    match (children_shape(dest), children_shape(source)) {
        (ChildrenShape::Named, ChildrenShape::Named)
        | (ChildrenShape::Named, ChildrenShape::Empty)
        | (ChildrenShape::Empty, ChildrenShape::Named) => merge_map_children(
            dest,
            source,
            spec,
            parent,
            filter,
            |field| match &field.locator {
                Locator::Named(name) => Some(name.clone()),
                _ => None,
            },
            |parent, name| parent.clone().field(name.clone()),
        ),
        (ChildrenShape::Keyed, ChildrenShape::Keyed)
        | (ChildrenShape::Keyed, ChildrenShape::Empty)
        | (ChildrenShape::Empty, ChildrenShape::Keyed) => merge_map_children(
            dest,
            source,
            spec,
            parent,
            filter,
            |field| match &field.locator {
                Locator::Keyed(key) => Some(*key),
                _ => None,
            },
            |parent, key| parent.clone().field(hex16(key)),
        ),
        (ChildrenShape::Empty, ChildrenShape::Empty) => Ok(Vec::new()),
        _ => unreachable!("merge_children is only called under fine_grained"),
    }
}

/// The keyed-map merge over both sides' fields (02 §7.3 语义表 rows
/// 「仅目的有」/「仅源有」plus per-common-field `merge_field`).
fn merge_map_children<K: Ord + Clone>(
    dest: &[ImageField],
    source: &[ImageField],
    spec: &MergeSpec,
    parent: &XPath,
    filter: &ScopeFilter,
    key_of: impl Fn(&ImageField) -> Option<K>,
    path_for: impl Fn(&XPath, &K) -> XPath,
) -> Result<Vec<ImageField>> {
    let mut dest_map = BTreeMap::new();
    for field in dest {
        if let Some(key) = key_of(field) {
            dest_map.insert(key, field);
        }
    }
    let mut source_map = BTreeMap::new();
    for field in source {
        if let Some(key) = key_of(field) {
            source_map.insert(key, field);
        }
    }

    let mut out = Vec::new();
    // Common and dest-only fields, in destination order (encode re-sorts
    // named/keyed children, so the order is informational only).
    for (key, dest_field) in &dest_map {
        let path = path_for(parent, key);
        match source_map.get(key) {
            Some(source_field) => {
                out.push(merge_field(dest_field, source_field, spec, &path, filter)?)
            }
            None => {
                if !(in_scope(filter, &path) && spec.op.deletes()) {
                    out.push((*dest_field).clone());
                }
            }
        }
    }
    // Source-only fields inserted only in scope (02 §7.3 语义表「仅源有」).
    for (key, source_field) in &source_map {
        if dest_map.contains_key(key) {
            continue;
        }
        let path = path_for(parent, key);
        if in_scope(filter, &path) && spec.op.adds() {
            out.push((*source_field).clone());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Child-shape classification
// ---------------------------------------------------------------------------

/// The uniform locator category of a node's children, when it has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildrenShape {
    Empty,
    Named,
    Keyed,
    Positioned,
    Mixed,
}

fn children_shape(children: &[ImageField]) -> ChildrenShape {
    let mut category = None;
    for field in children {
        let field_category = match &field.locator {
            Locator::Named(_) => ChildrenShape::Named,
            Locator::Keyed(_) => ChildrenShape::Keyed,
            Locator::Positioned => ChildrenShape::Positioned,
        };
        match category {
            None => category = Some(field_category),
            Some(previous) if previous != field_category => return ChildrenShape::Mixed,
            _ => {}
        }
    }
    category.unwrap_or(ChildrenShape::Empty)
}

/// Whether the two children lists merge fine-grained (Named/Keyed pairs, or
/// trivially empty) — the recursion entry condition of 02 §7.3.
fn fine_grained(dest: &[ImageField], source: &[ImageField]) -> bool {
    matches!(
        (children_shape(dest), children_shape(source)),
        (ChildrenShape::Named, ChildrenShape::Named)
            | (ChildrenShape::Named, ChildrenShape::Empty)
            | (ChildrenShape::Empty, ChildrenShape::Named)
            | (ChildrenShape::Keyed, ChildrenShape::Keyed)
            | (ChildrenShape::Keyed, ChildrenShape::Empty)
            | (ChildrenShape::Empty, ChildrenShape::Keyed)
            | (ChildrenShape::Empty, ChildrenShape::Empty)
    )
}

/// Whether both contents are structural containers (positioned/mixed-children
/// `Node`, `ChunkGroup`, or `View`) treated as atomic units (02 §7.6).
fn container_pair(dest: &ImageContent, source: &ImageContent) -> bool {
    matches!(
        (dest, source),
        (ImageContent::Node(_), ImageContent::Node(_))
            | (
                ImageContent::ChunkGroup { .. },
                ImageContent::ChunkGroup { .. }
            )
            | (ImageContent::View(_), ImageContent::View(_))
    )
}
