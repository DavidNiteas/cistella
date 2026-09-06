//! Compile-time composition checks (const-evaluated).
//!
//! `tree_compose!` expands into one `const` assertion per composite calling
//! [`composite_check`]; a `None` result means the composite is clean, a
//! `Some` result is a compile-time error whose literal message is baked at
//! macro expansion (`concat!` + `stringify!` -- const panics accept only
//! expansion-time string literals) while the full detail stays available in
//! the generated `<Composite>::CONFLICT` diagnostic constant.
//!
//! Check pipeline (document 02, section 2.6):
//! 1. host coordinate system well-formedness,
//! 2. mount slots resolve; member `required_slots` are covered,
//! 3. `At` anchors are valid coordinate paths,
//! 4. `points_to` endpoint references resolve with matching types (both
//!    modes),
//! 5. path-claim conflicts: intra-source in both modes, cross-source only in
//!    orthogonal mode (P4: identical table fingerprints deduplicate, differing
//!    ones are errors; merge mode leaves cross-template overrides to the
//!    declaration-order fold).

use super::composite::{CompositeSpec, ConflictKind, ConflictReport, MAX_PATH_SEGMENTS};
use super::spec::{
    EndpointRef, MAX_SLOT_DEPTH, SlotCtx, SlotDecl, SlotName, TemplateEntry, TemplateEntryKind,
    TemplateSpec, TemplateTable, str_eq, str_slice_eq, table_fingerprint_eq,
};

/// Runs every check applicable to the composite's mode and returns the first
/// conflict, or `None` when the composite is clean.
pub const fn composite_check(spec: &CompositeSpec) -> Option<ConflictReport> {
    if let Some(report) = check_coordinates(spec) {
        return Some(report);
    }
    if let Some(report) = check_mount_slots(spec) {
        return Some(report);
    }
    if let Some(report) = check_anchors(spec) {
        return Some(report);
    }
    if let Some(report) = check_endpoints(spec) {
        return Some(report);
    }
    check_claims(spec)
}

/// A resolved coordinate-slot chain in a fixed buffer.
#[derive(Clone, Copy)]
struct Chain {
    slots: [SlotName; MAX_SLOT_DEPTH],
    len: usize,
}

impl Chain {
    const EMPTY: Chain = Chain {
        slots: [SlotName(""); MAX_SLOT_DEPTH],
        len: 0,
    };
}

/// Why a chain resolution failed.
#[derive(Clone, Copy)]
enum ChainErr {
    /// The slot is not declared in the coordinate system.
    UnknownSlot,
    /// The chain exceeds [`MAX_SLOT_DEPTH`] (also the cycle guard).
    TooDeep,
    /// The chain is not a valid coordinate path (an `At` anchor prefix does
    /// not match the declared parent chain).
    Illegal,
}

const fn find_slot_index(coords: &[SlotDecl], name: SlotName) -> Option<usize> {
    let mut i = 0;
    while i < coords.len() {
        if coords[i].slot.const_eq(name) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Resolves the coordinate chain of a slot by walking parent pointers.
const fn coordinate_chain(coords: &[SlotDecl], slot: SlotName) -> Result<Chain, ChainErr> {
    let mut rev: [SlotName; MAX_SLOT_DEPTH] = [SlotName(""); MAX_SLOT_DEPTH];
    let mut n = 0;
    let mut cur = slot;
    loop {
        match find_slot_index(coords, cur) {
            None => return Err(ChainErr::UnknownSlot),
            Some(index) => {
                if n >= MAX_SLOT_DEPTH {
                    return Err(ChainErr::TooDeep);
                }
                rev[n] = cur;
                n += 1;
                match coords[index].parent {
                    None => break,
                    Some(parent) => cur = parent,
                }
            }
        }
    }
    let mut chain = Chain::EMPTY;
    let mut k = 0;
    while k < n {
        chain.slots[k] = rev[n - 1 - k];
        k += 1;
    }
    chain.len = n;
    Ok(chain)
}

/// Copies a static `At` chain after validating it is a real coordinate path:
/// every prefix `chain[0..=k]` must equal the coordinate chain of
/// `chain[k]`.
const fn resolve_at_chain(coords: &[SlotDecl], at: &[SlotName]) -> Result<Chain, ChainErr> {
    if at.len() > MAX_SLOT_DEPTH {
        return Err(ChainErr::TooDeep);
    }
    let mut k = 0;
    while k < at.len() {
        let full = match coordinate_chain(coords, at[k]) {
            Ok(chain) => chain,
            Err(err) => return Err(err),
        };
        if full.len != k + 1 {
            return Err(ChainErr::Illegal);
        }
        let mut p = 0;
        while p <= k {
            if !full.slots[p].const_eq(at[p]) {
                return Err(ChainErr::Illegal);
            }
            p += 1;
        }
        k += 1;
    }
    let mut chain = Chain::EMPTY;
    let mut i = 0;
    while i < at.len() {
        chain.slots[i] = at[i];
        i += 1;
    }
    chain.len = at.len();
    Ok(chain)
}

const fn source_count(spec: &CompositeSpec) -> usize {
    spec.mounts.len() + 1
}

const fn source_name(spec: &CompositeSpec, source: usize) -> &'static str {
    if source == 0 {
        spec.host_name
    } else {
        spec.mounts[source - 1].template_name
    }
}

const fn source_spec(spec: &CompositeSpec, source: usize) -> &TemplateSpec {
    if source == 0 {
        &spec.host
    } else {
        &spec.mounts[source - 1].spec
    }
}

/// Resolves the slot prefix of one entry: `At` chains are absolute; `Inherit`
/// entries of the host attach at the composite root, those of a mount at the
/// mount slot's coordinate chain.
const fn entry_prefix(
    spec: &CompositeSpec,
    source: usize,
    entry: &TemplateEntry,
) -> Result<Chain, ChainErr> {
    match entry.at {
        SlotCtx::At(chain) => resolve_at_chain(&spec.host.coordinates, chain),
        SlotCtx::Inherit => {
            if source == 0 {
                Ok(Chain::EMPTY)
            } else {
                coordinate_chain(&spec.host.coordinates, spec.mounts[source - 1].slot)
            }
        }
    }
}

const fn check_coordinates(spec: &CompositeSpec) -> Option<ConflictReport> {
    let coords = spec.host.coordinates;
    let mut i = 0;
    while i < coords.len() {
        if let Some(parent) = coords[i].parent {
            if find_slot_index(coords, parent).is_none() {
                return Some(report_simple(
                    ConflictKind::MalformedCoordinates,
                    spec.host_name,
                    spec.host_name,
                ));
            }
        }
        match coordinate_chain(coords, coords[i].slot) {
            Ok(_) => {}
            Err(ChainErr::TooDeep) => {
                return Some(report_simple(
                    ConflictKind::ChainTooDeep,
                    spec.host_name,
                    spec.host_name,
                ));
            }
            Err(_) => {
                return Some(report_simple(
                    ConflictKind::MalformedCoordinates,
                    spec.host_name,
                    spec.host_name,
                ));
            }
        }
        let mut j = i + 1;
        while j < coords.len() {
            if coords[i].slot.const_eq(coords[j].slot) {
                return Some(report_simple(
                    ConflictKind::MalformedCoordinates,
                    spec.host_name,
                    spec.host_name,
                ));
            }
            j += 1;
        }
        i += 1;
    }
    None
}

const fn check_mount_slots(spec: &CompositeSpec) -> Option<ConflictReport> {
    let mut s = 1;
    while s < source_count(spec) {
        let mount = &spec.mounts[s - 1];
        match coordinate_chain(&spec.host.coordinates, mount.slot) {
            Ok(_) => {}
            Err(ChainErr::UnknownSlot) => {
                return Some(report_simple(
                    ConflictKind::UnknownMountSlot,
                    spec.host_name,
                    mount.template_name,
                ));
            }
            Err(ChainErr::TooDeep) => {
                return Some(report_simple(
                    ConflictKind::ChainTooDeep,
                    spec.host_name,
                    mount.template_name,
                ));
            }
            Err(ChainErr::Illegal) => {
                return Some(report_simple(
                    ConflictKind::MalformedCoordinates,
                    spec.host_name,
                    mount.template_name,
                ));
            }
        }
        let mut r = 0;
        while r < mount.spec.required_slots.len() {
            if find_slot_index(&spec.host.coordinates, mount.spec.required_slots[r]).is_none() {
                return Some(report_simple(
                    ConflictKind::RequiredSlotUncovered,
                    mount.template_name,
                    spec.host_name,
                ));
            }
            r += 1;
        }
        s += 1;
    }
    None
}

const fn check_anchors(spec: &CompositeSpec) -> Option<ConflictReport> {
    let mut s = 0;
    while s < source_count(spec) {
        let entries = source_spec(spec, s).entries;
        let mut e = 0;
        while e < entries.len() {
            if let SlotCtx::At(chain) = entries[e].at {
                let origin = source_name(spec, s);
                match resolve_at_chain(&spec.host.coordinates, chain) {
                    Ok(_) => {}
                    Err(ChainErr::TooDeep) => {
                        return Some(report_simple(ConflictKind::ChainTooDeep, origin, origin));
                    }
                    Err(_) => {
                        return Some(report_simple(ConflictKind::IllegalAnchor, origin, origin));
                    }
                }
            }
            e += 1;
        }
        s += 1;
    }
    None
}

const fn check_endpoints(spec: &CompositeSpec) -> Option<ConflictReport> {
    let mut s = 0;
    while s < source_count(spec) {
        let entries = source_spec(spec, s).entries;
        let mut e = 0;
        while e < entries.len() {
            if let TemplateEntryKind::Table(table) = &entries[e].kind {
                let mut c = 0;
                while c < table.columns.len() {
                    if let Some(endpoint) = table.columns[c].points_to {
                        if let Some(report) = check_endpoint(spec, source_name(spec, s), &endpoint)
                        {
                            return Some(report);
                        }
                    }
                    c += 1;
                }
            }
            e += 1;
        }
        s += 1;
    }
    None
}

const fn check_endpoint(
    spec: &CompositeSpec,
    origin: &'static str,
    endpoint: &EndpointRef,
) -> Option<ConflictReport> {
    let mut target: Option<usize> = None;
    let mut s = 0;
    while s < source_count(spec) {
        if str_eq(source_name(spec, s), endpoint.template) {
            target = Some(s);
        }
        s += 1;
    }
    let target = match target {
        None => {
            return Some(report_with(
                ConflictKind::EndpointMissing,
                origin,
                endpoint.template,
                &Chain::EMPTY,
                endpoint.leaf,
            ));
        }
        Some(index) => index,
    };
    let entries = source_spec(spec, target).entries;
    let mut e = 0;
    let mut found: Option<&TemplateTable> = None;
    while e < entries.len() {
        if let TemplateEntryKind::Table(table) = &entries[e].kind {
            if str_slice_eq(entries[e].path, endpoint.leaf) {
                found = Some(table);
            }
        }
        e += 1;
    }
    let table = match found {
        None => {
            return Some(report_with(
                ConflictKind::EndpointMissing,
                origin,
                endpoint.template,
                &Chain::EMPTY,
                endpoint.leaf,
            ));
        }
        Some(table) => table,
    };
    let mut c = 0;
    while c < table.columns.len() {
        if str_eq(table.columns[c].name, endpoint.column) {
            if table.columns[c].ty.const_eq(endpoint.expect) {
                return None;
            }
            return Some(report_with(
                ConflictKind::EndpointTypeMismatch,
                origin,
                endpoint.template,
                &Chain::EMPTY,
                endpoint.leaf,
            ));
        }
        c += 1;
    }
    Some(report_with(
        ConflictKind::EndpointMissing,
        origin,
        endpoint.template,
        &Chain::EMPTY,
        endpoint.leaf,
    ))
}

/// Path-claim conflicts. Intra-source pairs are checked in both modes (a
/// template contradicting itself is malformed regardless of mode);
/// cross-template pairs are checked only under orthogonal composition -- merge
/// mode resolves those by the declaration-order fold in the projection.
const fn check_claims(spec: &CompositeSpec) -> Option<ConflictReport> {
    let n = source_count(spec);
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n {
            if !(spec.merge && i != j) {
                if let Some(report) = check_source_pair(spec, i, j) {
                    return Some(report);
                }
            }
            j += 1;
        }
        i += 1;
    }
    None
}

const fn check_source_pair(spec: &CompositeSpec, i: usize, j: usize) -> Option<ConflictReport> {
    let entries_i = source_spec(spec, i).entries;
    let entries_j = source_spec(spec, j).entries;
    let mut a = 0;
    while a < entries_i.len() {
        let mut b = if i == j { a + 1 } else { 0 };
        while b < entries_j.len() {
            if let Some(report) = check_entry_pair(spec, i, &entries_i[a], j, &entries_j[b]) {
                return Some(report);
            }
            b += 1;
        }
        a += 1;
    }
    None
}

const fn check_entry_pair(
    spec: &CompositeSpec,
    i: usize,
    entry_a: &TemplateEntry,
    j: usize,
    entry_b: &TemplateEntry,
) -> Option<ConflictReport> {
    let chain_a = match entry_prefix(spec, i, entry_a) {
        Ok(chain) => chain,
        Err(_) => {
            return Some(report_simple(
                ConflictKind::IllegalAnchor,
                source_name(spec, i),
                source_name(spec, j),
            ));
        }
    };
    let chain_b = match entry_prefix(spec, j, entry_b) {
        Ok(chain) => chain,
        Err(_) => {
            return Some(report_simple(
                ConflictKind::IllegalAnchor,
                source_name(spec, i),
                source_name(spec, j),
            ));
        }
    };
    let left = source_name(spec, i);
    let right = source_name(spec, j);
    match path_rel(&chain_a, entry_a.path, &chain_b, entry_b.path) {
        PathRel::Disjoint => None,
        PathRel::Equal => match (&entry_a.kind, &entry_b.kind) {
            (TemplateEntryKind::Table(table_a), TemplateEntryKind::Table(table_b)) => {
                if table_fingerprint_eq(table_a, table_b) {
                    None
                } else {
                    Some(report_with(
                        ConflictKind::LeafFingerprintMismatch,
                        left,
                        right,
                        &chain_a,
                        entry_a.path,
                    ))
                }
            }
            (TemplateEntryKind::Table(_), TemplateEntryKind::Domain { .. })
            | (TemplateEntryKind::Domain { .. }, TemplateEntryKind::Table(_)) => Some(report_with(
                ConflictKind::LeafVsDomain,
                left,
                right,
                &chain_a,
                entry_a.path,
            )),
            (TemplateEntryKind::Domain { .. }, TemplateEntryKind::Domain { .. }) => None,
        },
        PathRel::AShorter => match &entry_a.kind {
            // The prefix side must be a domain claim; a table there would have
            // to contain the other entry.
            TemplateEntryKind::Domain { .. } => None,
            TemplateEntryKind::Table(_) => Some(report_with(
                ConflictKind::LeafVsDomain,
                left,
                right,
                &chain_a,
                entry_a.path,
            )),
        },
        PathRel::BShorter => match &entry_b.kind {
            TemplateEntryKind::Domain { .. } => None,
            TemplateEntryKind::Table(_) => Some(report_with(
                ConflictKind::LeafVsDomain,
                left,
                right,
                &chain_b,
                entry_b.path,
            )),
        },
    }
}

/// One concrete path segment: a coordinate slot or a plain name.
///
/// Slot and name segments live in different namespaces and never compare
/// equal, so a domain named like a slot cannot alias the slot level.
#[derive(Clone, Copy)]
enum Seg {
    S(SlotName),
    N(&'static str),
}

const fn seg_at(chain: &Chain, names: &[&'static str], index: usize) -> Seg {
    if index < chain.len {
        Seg::S(chain.slots[index])
    } else {
        Seg::N(names[index - chain.len])
    }
}

const fn seg_eq(a: Seg, b: Seg) -> bool {
    match (a, b) {
        (Seg::S(x), Seg::S(y)) => x.const_eq(y),
        (Seg::N(x), Seg::N(y)) => str_eq(x, y),
        _ => false,
    }
}

/// The relation of two concrete paths.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathRel {
    Equal,
    AShorter,
    BShorter,
    Disjoint,
}

const fn path_rel(
    chain_a: &Chain,
    names_a: &[&'static str],
    chain_b: &Chain,
    names_b: &[&'static str],
) -> PathRel {
    let len_a = chain_a.len + names_a.len();
    let len_b = chain_b.len + names_b.len();
    let min = if len_a < len_b { len_a } else { len_b };
    let mut i = 0;
    while i < min {
        if !seg_eq(seg_at(chain_a, names_a, i), seg_at(chain_b, names_b, i)) {
            return PathRel::Disjoint;
        }
        i += 1;
    }
    if len_a < len_b {
        PathRel::AShorter
    } else if len_a > len_b {
        PathRel::BShorter
    } else {
        PathRel::Equal
    }
}

const fn report_simple(
    kind: ConflictKind,
    left: &'static str,
    right: &'static str,
) -> ConflictReport {
    ConflictReport {
        kind,
        left,
        right,
        path_slots: [SlotName(""); MAX_SLOT_DEPTH],
        path_slots_len: 0,
        path_names: [""; MAX_PATH_SEGMENTS],
        path_names_len: 0,
    }
}

const fn report_with(
    kind: ConflictKind,
    left: &'static str,
    right: &'static str,
    chain: &Chain,
    names: &[&'static str],
) -> ConflictReport {
    let mut report = report_simple(kind, left, right);
    let mut k = 0;
    while k < chain.len && k < MAX_SLOT_DEPTH {
        report.path_slots[k] = chain.slots[k];
        k += 1;
    }
    report.path_slots_len = if chain.len > MAX_SLOT_DEPTH {
        MAX_SLOT_DEPTH
    } else {
        chain.len
    };
    let mut m = 0;
    while m < names.len() && m < MAX_PATH_SEGMENTS {
        report.path_names[m] = names[m];
        m += 1;
    }
    report.path_names_len = if names.len() > MAX_PATH_SEGMENTS {
        MAX_PATH_SEGMENTS
    } else {
        names.len()
    };
    report
}
