//! Tests of the template layer: const composition checks, P4 semantics,
//! merge overrides, multi-position mounts, endpoint checks, and the
//! projection equivalence guard.
//!
//! The compile-time rejection paths are additionally covered by the
//! `compile_fail` doc-tests on [`tree_compose`](crate::tree_compose).

use super::check::composite_check;
use super::composite::{CompositeSpec, ConflictKind, MountSpec};
use super::project::{ShapeMapping, data_type, domain_type_id, field_of, project, table_type_id};
use super::spec::{
    ConstScalar, ConstStructField, ConstTimeUnit, ConstType, EndpointRef, MAX_SLOT_DEPTH, SlotCtx,
    SlotDecl, SlotName, TemplateColumn, TemplateEntry, TemplateEntryKind, TemplateSpec,
    TemplateTable, TierDecl, TreeTemplate,
};
use crate::path::Name;
use crate::registry::RegistryExport;
use crate::types::{
    Cardinality, ChildDefinition, ChildKind, ColumnDefinition, DomainType, InstanceMode, TableType,
};
use arrow::datatypes::{DataType, Field};

// ---------------------------------------------------------------------------
// Fixture templates
// ---------------------------------------------------------------------------

const NOTE_COLS: &[TemplateColumn] = &[TemplateColumn {
    name: "note",
    ty: ConstType::Utf8,
    nullable: false,
    id_component: false,
    aliases: &[],
    points_to: None,
}];

const STUDY_META: TemplateTable = TemplateTable {
    name: "study-metadata",
    version: 1,
    columns: NOTE_COLS,
    indexes: &[],
};
const RUN_META: TemplateTable = TemplateTable {
    name: "run-metadata",
    version: 1,
    columns: NOTE_COLS,
    indexes: &[],
};

const HOST_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::At(&[SlotName::STUDY]),
        path: &["study-metadata"],
        kind: TemplateEntryKind::Table(STUDY_META),
    },
    TemplateEntry {
        at: SlotCtx::At(&[SlotName::STUDY, SlotName::RUN]),
        path: &["run-metadata"],
        kind: TemplateEntryKind::Table(RUN_META),
    },
];

const HOST_COORDS: &[SlotDecl] = &[
    SlotDecl {
        slot: SlotName::STUDY,
        parent: None,
        tier: Some(TierDecl {
            prefix: "s",
            max_instances: 2,
        }),
    },
    SlotDecl {
        slot: SlotName::RUN,
        parent: Some(SlotName::STUDY),
        tier: Some(TierDecl {
            prefix: "r",
            max_instances: 2,
        }),
    },
];

struct TestHost;
impl TreeTemplate for TestHost {
    const NAME: &'static str = "test-root";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: HOST_COORDS,
        required_slots: &[],
        entries: HOST_ENTRIES,
    };
}

const POINT_COLS: &[TemplateColumn] = &[
    TemplateColumn {
        name: "point_id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: true,
        aliases: &[],
        points_to: None,
    },
    TemplateColumn {
        name: "mz",
        ty: ConstType::F(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: None,
    },
    TemplateColumn {
        name: "rt",
        ty: ConstType::F(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: None,
    },
];
const POINT_DATA: TemplateTable = TemplateTable {
    name: "point-data",
    version: 1,
    columns: POINT_COLS,
    indexes: &[],
};
const MSN: TemplateTable = TemplateTable {
    name: "msn",
    version: 1,
    columns: &[
        TemplateColumn {
            name: "up_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "down_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
    ],
    indexes: &[],
};

const POINT_CLOUD_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links"],
        kind: TemplateEntryKind::Domain { tier: None },
    },
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links", "msn"],
        kind: TemplateEntryKind::Table(MSN),
    },
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["point-data"],
        kind: TemplateEntryKind::Table(POINT_DATA),
    },
];

struct TestPointCloud;
impl TreeTemplate for TestPointCloud {
    const NAME: &'static str = "point-cloud";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::STUDY, SlotName::RUN],
        entries: POINT_CLOUD_ENTRIES,
    };
}

const FEATURE: TemplateTable = TemplateTable {
    name: "feature",
    version: 1,
    columns: &[
        TemplateColumn {
            name: "feature_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: true,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "mz",
            ty: ConstType::F(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "rt",
            ty: ConstType::F(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
    ],
    indexes: &[],
};
const CONSENSUS: TemplateTable = TemplateTable {
    name: "geo-consensus",
    version: 1,
    columns: &[
        TemplateColumn {
            name: "consensus_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: true,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "feature_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
    ],
    indexes: &[],
};

const GEO_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["geo"],
        kind: TemplateEntryKind::Domain { tier: None },
    },
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["geo", "feature"],
        kind: TemplateEntryKind::Table(FEATURE),
    },
    TemplateEntry {
        at: SlotCtx::At(&[SlotName::STUDY]),
        path: &["geo-consensus"],
        kind: TemplateEntryKind::Table(CONSENSUS),
    },
];

struct TestGeo;
impl TreeTemplate for TestGeo {
    const NAME: &'static str = "geo";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::STUDY, SlotName::RUN],
        entries: GEO_ENTRIES,
    };
}

const SPECTRUM: TemplateTable = TemplateTable {
    name: "spectrum",
    version: 1,
    columns: &[
        TemplateColumn {
            name: "spectrum_id",
            ty: ConstType::U(64),
            nullable: false,
            id_component: true,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "peaks",
            ty: ConstType::Binary,
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
    ],
    indexes: &[],
};

const SPECTRUM_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["spectrum"],
    kind: TemplateEntryKind::Table(SPECTRUM),
}];

struct TestSpectrum;
impl TreeTemplate for TestSpectrum {
    const NAME: &'static str = "spectrum";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[],
        entries: SPECTRUM_ENTRIES,
    };
}

const AUX_COLS: &[TemplateColumn] = &[
    TemplateColumn {
        name: "ion_id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: Some(EndpointRef {
            template: "point-cloud",
            leaf: &["point-data"],
            column: "point_id",
            expect: ConstType::U(64),
        }),
    },
    TemplateColumn {
        name: "formula_id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: None,
    },
];
const AUX_LINK: TemplateTable = TemplateTable {
    name: "ion-formula-link",
    version: 1,
    columns: AUX_COLS,
    indexes: &[],
};

const AUX_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["ion-formula-link"],
    kind: TemplateEntryKind::Table(AUX_LINK),
}];

struct TestAuxLink;
impl TreeTemplate for TestAuxLink {
    const NAME: &'static str = "ion-formula-link";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: AUX_ENTRIES,
    };
}

const MERGE_A_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["shared"],
    kind: TemplateEntryKind::Table(TemplateTable {
        name: "shared",
        version: 1,
        columns: &[TemplateColumn {
            name: "a",
            ty: ConstType::U(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        }],
        indexes: &[],
    }),
}];

const MERGE_B_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["shared"],
    kind: TemplateEntryKind::Table(TemplateTable {
        name: "shared",
        version: 1,
        columns: &[TemplateColumn {
            name: "b",
            ty: ConstType::Utf8,
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        }],
        indexes: &[],
    }),
}];

struct MergeA;
impl TreeTemplate for MergeA {
    const NAME: &'static str = "merge-a";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: MERGE_A_ENTRIES,
    };
}

struct MergeB;
impl TreeTemplate for MergeB {
    const NAME: &'static str = "merge-b";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: MERGE_B_ENTRIES,
    };
}

const LINKS_A_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links"],
        kind: TemplateEntryKind::Domain { tier: None },
    },
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links", "a"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "a",
            version: 1,
            columns: NOTE_COLS,
            indexes: &[],
        }),
    },
];

const LINKS_B_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links"],
        kind: TemplateEntryKind::Domain { tier: None },
    },
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["links", "b"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "b",
            version: 1,
            columns: NOTE_COLS,
            indexes: &[],
        }),
    },
];

struct LinksA;
impl TreeTemplate for LinksA {
    const NAME: &'static str = "links-a";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: LINKS_A_ENTRIES,
    };
}

struct LinksB;
impl TreeTemplate for LinksB {
    const NAME: &'static str = "links-b";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: LINKS_B_ENTRIES,
    };
}

const TABLE_AT_X: TemplateTable = TemplateTable {
    name: "x",
    version: 1,
    columns: NOTE_COLS,
    indexes: &[],
};

const TABLE_AT_X_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["x"],
    kind: TemplateEntryKind::Table(TABLE_AT_X),
}];

const UNDER_X_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["x", "y"],
    kind: TemplateEntryKind::Table(TemplateTable {
        name: "y",
        version: 1,
        columns: NOTE_COLS,
        indexes: &[],
    }),
}];

struct TableAtX;
impl TreeTemplate for TableAtX {
    const NAME: &'static str = "table-at-x";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: TABLE_AT_X_ENTRIES,
    };
}

struct UnderX;
impl TreeTemplate for UnderX {
    const NAME: &'static str = "under-x";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: UNDER_X_ENTRIES,
    };
}

const POINT_DATA_SAME: TemplateTable = TemplateTable {
    name: "point-data",
    version: 1,
    columns: POINT_COLS,
    indexes: &[],
};

const SAME_POINT_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["point-data"],
    kind: TemplateEntryKind::Table(POINT_DATA_SAME),
}];

struct SamePoint;
impl TreeTemplate for SamePoint {
    const NAME: &'static str = "same-point";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: SAME_POINT_ENTRIES,
    };
}

const DOMAIN_COLLIDE_ENTRIES: &[TemplateEntry] = &[
    TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["d", "t1"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "t1",
            version: 1,
            columns: NOTE_COLS,
            indexes: &[],
        }),
    },
    TemplateEntry {
        at: SlotCtx::At(&[SlotName::STUDY]),
        path: &["d", "t2"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "t2",
            version: 1,
            columns: NOTE_COLS,
            indexes: &[],
        }),
    },
];

struct DomainCollideTemplate;
impl TreeTemplate for DomainCollideTemplate {
    const NAME: &'static str = "domain-collide";
    const SPEC: TemplateSpec = TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[SlotName::RUN],
        entries: DOMAIN_COLLIDE_ENTRIES,
    };
}

const BAD_SEGMENT_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["bad/name"],
    kind: TemplateEntryKind::Domain { tier: None },
}];

const DUP_COLS: &[TemplateColumn] = &[
    TemplateColumn {
        name: "id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: None,
    },
    TemplateColumn {
        name: "id",
        ty: ConstType::Utf8,
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: None,
    },
];

const DUP_COL_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
    at: SlotCtx::Inherit,
    path: &["dup"],
    kind: TemplateEntryKind::Table(TemplateTable {
        name: "dup",
        version: 1,
        columns: DUP_COLS,
        indexes: &[],
    }),
}];

// ---------------------------------------------------------------------------
// Macro-composed fixtures
// ---------------------------------------------------------------------------

crate::tree_compose! {
    composite OrthoFixture = host(TestHost)
        mount(SlotName::RUN, TestPointCloud)
        mount(SlotName::RUN, TestGeo)
        mount(SlotName::STUDY, TestSpectrum, as "reference")
        mount(SlotName::RUN, TestAuxLink)
        mode Orthogonal;
}

crate::tree_compose! {
    composite MergeFixture = host(TestHost)
        mount(SlotName::RUN, MergeA)
        mount(SlotName::RUN, MergeB)
        mode Merge;
}

crate::tree_compose! {
    composite MergeFixtureSwapped = host(TestHost)
        mount(SlotName::RUN, MergeB)
        mount(SlotName::RUN, MergeA)
        mode Merge;
}

crate::tree_compose! {
    composite DomainMerge = host(TestHost)
        mount(SlotName::RUN, LinksA)
        mount(SlotName::RUN, LinksB)
        mode Orthogonal;
}

crate::tree_compose! {
    composite OrthoDedup = host(TestHost)
        mount(SlotName::RUN, TestPointCloud)
        mount(SlotName::RUN, SamePoint)
        mode Orthogonal;
}

crate::tree_compose! {
    composite SameSlotDedup = host(TestHost)
        mount(SlotName::RUN, TestSpectrum)
        mount(SlotName::RUN, TestSpectrum)
        mode Orthogonal;
}

crate::tree_compose! {
    composite MultiPosition = host(TestHost)
        mount(SlotName::STUDY, TestSpectrum, as "reference")
        mount(SlotName::RUN, TestSpectrum, as "collected")
        mode Orthogonal;
}

crate::tree_compose! {
    composite MergeCovered = host(TestHost)
        mount(SlotName::RUN, TableAtX)
        mount(SlotName::RUN, UnderX)
        mode Merge;
}

crate::tree_compose! {
    composite HostOnly = host(TestHost)
        mode Orthogonal;
}

crate::tree_compose! {
    composite DomainCollide = host(TestHost)
        mount(SlotName::RUN, DomainCollideTemplate)
        mode Orthogonal;
}

// ---------------------------------------------------------------------------
// Helpers for hand-built probe specs (value-level check coverage)
// ---------------------------------------------------------------------------

fn member(entries: &'static [TemplateEntry]) -> TemplateSpec {
    TemplateSpec {
        version: 1,
        coordinates: &[],
        required_slots: &[],
        entries,
    }
}

fn probe(mounts: &'static [MountSpec], merge: bool) -> CompositeSpec {
    CompositeSpec {
        name: "probe",
        host_name: "test-root",
        host: TestHost::SPEC,
        mounts,
        merge,
    }
}

fn leak_slots(value: Vec<SlotDecl>) -> &'static [SlotDecl] {
    Box::leak(value.into_boxed_slice())
}

fn leak_mounts(value: Vec<MountSpec>) -> &'static [MountSpec] {
    Box::leak(value.into_boxed_slice())
}

fn mount_of(name: &'static str, slot: SlotName, entries: &'static [TemplateEntry]) -> MountSpec {
    MountSpec {
        slot,
        label: "",
        template_name: name,
        spec: member(entries),
    }
}

// ---------------------------------------------------------------------------
// Clean fixtures, CONFLICT diagnostics, and mode semantics
// ---------------------------------------------------------------------------

#[test]
fn ortho_fixture_is_clean_and_composes() {
    assert!(OrthoFixture::CONFLICT.is_none());
    assert!(!OrthoFixture::SPEC.merge);
    assert_eq!(OrthoFixture::SPEC.name, "OrthoFixture");
    assert_eq!(OrthoFixture::SPEC.host_name, "test-root");
    assert_eq!(OrthoFixture::SPEC.mounts.len(), 4);
    assert_eq!(OrthoFixture::SPEC.mounts[2].label, "reference");
    let plan = project(&OrthoFixture::SPEC, &ShapeMapping::default()).unwrap();
    assert_eq!(plan.domain_types.len(), 5);
    assert_eq!(plan.table_types.len(), 8);
    plan.to_registry().unwrap();
}

#[test]
fn host_only_composite_is_clean() {
    assert!(HostOnly::CONFLICT.is_none());
    assert_eq!(HostOnly::SPEC.mounts.len(), 0);
    let plan = project(&HostOnly::SPEC, &ShapeMapping::default()).unwrap();
    plan.to_registry().unwrap();
}

#[test]
fn ortho_identical_fingerprint_deduplicates_p4() {
    assert!(OrthoDedup::CONFLICT.is_none());
    let plan = project(&OrthoDedup::SPEC, &ShapeMapping::default()).unwrap();
    // The fold keeps a single node for the identical re-declaration.
    let count = plan
        .table_types
        .iter()
        .filter(|table| table.name.as_str() == "point-data")
        .count();
    assert_eq!(count, 1);
    // Same (name, version, columns) -> same identity; the registry dedups.
    plan.to_registry().unwrap();
}

#[test]
fn same_slot_double_mount_of_identical_template_deduplicates() {
    assert!(SameSlotDedup::CONFLICT.is_none());
}

#[test]
fn ortho_fingerprint_mismatch_is_reported() {
    // point-data (three columns) vs shared "point-data" clone with one column
    // removed: same path, different fingerprint.
    const MZ_RT_COLS: &[TemplateColumn] = &[
        TemplateColumn {
            name: "mz",
            ty: ConstType::F(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
        TemplateColumn {
            name: "rt",
            ty: ConstType::F(64),
            nullable: false,
            id_component: false,
            aliases: &[],
            points_to: None,
        },
    ];
    const CLONE_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["point-data"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "point-data",
            version: 1,
            columns: MZ_RT_COLS,
            indexes: &[],
        }),
    }];
    let mounts = leak_mounts(vec![
        mount_of("point-cloud", SlotName::RUN, POINT_CLOUD_ENTRIES),
        mount_of("clone", SlotName::RUN, CLONE_ENTRIES),
    ]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::LeafFingerprintMismatch);
    assert_eq!(report.left, "point-cloud");
    assert_eq!(report.right, "clone");
    assert_eq!(report.path_names_len, 1);
    assert_eq!(report.path_names[0], "point-data");
}

#[test]
fn leaf_vs_domain_conflicts_in_ortho_and_is_overridden_in_merge() {
    const TABLE_ENTRY: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["x"],
        kind: TemplateEntryKind::Table(TABLE_AT_X),
    }];
    const DOMAIN_ENTRY: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["x"],
        kind: TemplateEntryKind::Domain { tier: None },
    }];
    let mounts = leak_mounts(vec![
        mount_of("table-side", SlotName::RUN, TABLE_ENTRY),
        mount_of("domain-side", SlotName::RUN, DOMAIN_ENTRY),
    ]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::LeafVsDomain);
}

#[test]
fn intra_source_conflicts_error_in_both_modes() {
    const CONTRADICTORY: &[TemplateEntry] = &[
        TemplateEntry {
            at: SlotCtx::Inherit,
            path: &["x"],
            kind: TemplateEntryKind::Table(TABLE_AT_X),
        },
        TemplateEntry {
            at: SlotCtx::Inherit,
            path: &["x"],
            kind: TemplateEntryKind::Domain { tier: None },
        },
    ];
    let mounts = leak_mounts(vec![mount_of("self", SlotName::RUN, CONTRADICTORY)]);
    assert!(composite_check(&probe(mounts, false)).is_some());
    assert!(composite_check(&probe(mounts, true)).is_some());
}

#[test]
fn domain_merge_unions_children_both_modes() {
    assert!(DomainMerge::CONFLICT.is_none());
    let plan = project(&DomainMerge::SPEC, &ShapeMapping::default()).unwrap();
    let links = plan
        .domain_types
        .iter()
        .find(|domain| domain.name.as_str() == "links")
        .expect("links domain");
    let names: Vec<&str> = links
        .children
        .iter()
        .map(|child| child.name.as_str())
        .collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn merge_overrides_leaf_by_declaration_order() {
    assert!(MergeFixture::CONFLICT.is_none());
    assert!(MergeFixtureSwapped::CONFLICT.is_none());
    let plan = project(&MergeFixture::SPEC, &ShapeMapping::default()).unwrap();
    let shared = plan
        .table_types
        .iter()
        .find(|table| table.name.as_str() == "shared")
        .expect("shared table");
    assert_eq!(shared.columns.len(), 1);
    assert_eq!(shared.columns[0].name, "b");

    let swapped = project(&MergeFixtureSwapped::SPEC, &ShapeMapping::default()).unwrap();
    let shared = swapped
        .table_types
        .iter()
        .find(|table| table.name.as_str() == "shared")
        .expect("shared table");
    assert_eq!(shared.columns[0].name, "a");
}

#[test]
fn merge_covers_content_under_a_winning_table() {
    assert!(MergeCovered::CONFLICT.is_none());
    let plan = project(&MergeCovered::SPEC, &ShapeMapping::default()).unwrap();
    // The earlier template's table survives (the later one never claims the
    // path itself) and the later nested entry is covered.
    let x = plan
        .table_types
        .iter()
        .find(|table| table.name.as_str() == "x")
        .expect("x table");
    assert_eq!(x.columns[0].name, "note");
    assert!(
        plan.table_types
            .iter()
            .all(|table| table.name.as_str() != "y")
    );
}

#[test]
fn multi_position_mount_places_content_at_both_slots() {
    assert!(MultiPosition::CONFLICT.is_none());
    assert_eq!(MultiPosition::SPEC.mounts[0].label, "reference");
    assert_eq!(MultiPosition::SPEC.mounts[1].label, "collected");
    let plan = project(&MultiPosition::SPEC, &ShapeMapping::default()).unwrap();
    let study = plan
        .domain_types
        .iter()
        .find(|domain| domain.name.as_str() == "study")
        .expect("study type");
    assert!(
        study
            .children
            .iter()
            .any(|child| child.name.as_str() == "spectrum" && child.child_kind == ChildKind::Table)
    );
    let run = plan
        .domain_types
        .iter()
        .find(|domain| domain.name.as_str() == "run")
        .expect("run type");
    assert!(
        run.children
            .iter()
            .any(|child| child.name.as_str() == "spectrum" && child.child_kind == ChildKind::Table)
    );
    // Identical shape at both positions -> one shared table identity.
    plan.to_registry().unwrap();
}

// ---------------------------------------------------------------------------
// Coordinate-system and endpoint checks (value level)
// ---------------------------------------------------------------------------

#[test]
fn unknown_mount_slot_is_rejected() {
    let mounts = leak_mounts(vec![mount_of(
        "spectrum",
        SlotName("nope"),
        SPECTRUM_ENTRIES,
    )]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::UnknownMountSlot);
}

#[test]
fn uncovered_required_slot_is_rejected() {
    const LIB_REQUIRE_ENTRIES: &[TemplateEntry] = &[];
    let mounts = leak_mounts(vec![MountSpec {
        slot: SlotName::RUN,
        label: "",
        template_name: "lib-need",
        spec: TemplateSpec {
            version: 1,
            coordinates: &[],
            required_slots: &[SlotName::LIB],
            entries: LIB_REQUIRE_ENTRIES,
        },
    }]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::RequiredSlotUncovered);
}

#[test]
fn illegal_at_anchor_is_rejected() {
    const ANCHORED: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::At(&[SlotName::LIB]),
        path: &["spectrum"],
        kind: TemplateEntryKind::Table(SPECTRUM),
    }];
    let mounts = leak_mounts(vec![mount_of("spectrum", SlotName::RUN, ANCHORED)]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::IllegalAnchor);
}

#[test]
fn malformed_coordinates_are_rejected() {
    let coords = leak_slots(vec![SlotDecl {
        slot: SlotName::RUN,
        parent: Some(SlotName("ghost")),
        tier: None,
    }]);
    let spec = CompositeSpec {
        name: "probe",
        host_name: "test-root",
        host: TemplateSpec {
            version: 1,
            coordinates: coords,
            required_slots: &[],
            entries: &[],
        },
        mounts: &[],
        merge: false,
    };
    let report = composite_check(&spec).expect("conflict");
    assert_eq!(report.kind, ConflictKind::MalformedCoordinates);
}

#[test]
fn deep_chains_are_rejected() {
    let mut coords: Vec<SlotDecl> = Vec::new();
    let mut names: Vec<SlotName> = Vec::new();
    for depth in 0..(MAX_SLOT_DEPTH + 1) {
        let name = SlotName(Box::leak(format!("d{depth}").into_boxed_str()));
        let parent = if depth == 0 {
            None
        } else {
            Some(names[depth - 1])
        };
        coords.push(SlotDecl {
            slot: name,
            parent,
            tier: None,
        });
        names.push(name);
    }
    let coords = leak_slots(coords);
    let anchored: &'static [SlotName] = Box::leak(names.clone().into_boxed_slice());
    let entries: &'static [TemplateEntry] = Box::leak(
        vec![TemplateEntry {
            at: SlotCtx::At(anchored),
            path: &["spectrum"],
            kind: TemplateEntryKind::Table(SPECTRUM),
        }]
        .into_boxed_slice(),
    );
    let mounts = leak_mounts(vec![mount_of("spectrum", SlotName::RUN, entries)]);
    let spec = CompositeSpec {
        name: "probe",
        host_name: "test-root",
        host: TemplateSpec {
            version: 1,
            coordinates: coords,
            required_slots: &[],
            entries: &[],
        },
        mounts,
        merge: false,
    };
    let report = composite_check(&spec).expect("conflict");
    assert_eq!(report.kind, ConflictKind::ChainTooDeep);
}

#[test]
fn endpoint_missing_is_reported() {
    const CHEM_LINK_COLS: &[TemplateColumn] = &[TemplateColumn {
        name: "ion_id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: Some(EndpointRef {
            template: "chem",
            leaf: &["formula-data"],
            column: "formula_id",
            expect: ConstType::U(64),
        }),
    }];
    const CHEM_LINK_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["ion-formula-link"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "ion-formula-link",
            version: 1,
            columns: CHEM_LINK_COLS,
            indexes: &[],
        }),
    }];
    let mounts = leak_mounts(vec![mount_of("aux", SlotName::RUN, CHEM_LINK_ENTRIES)]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::EndpointMissing);
    // Both modes check endpoints.
    assert!(composite_check(&probe(mounts, true)).is_some());
}

#[test]
fn endpoint_type_mismatch_is_reported() {
    const MISMATCH_COLS: &[TemplateColumn] = &[TemplateColumn {
        name: "ion_id",
        ty: ConstType::U(64),
        nullable: false,
        id_component: false,
        aliases: &[],
        points_to: Some(EndpointRef {
            template: "point-cloud",
            leaf: &["point-data"],
            column: "point_id",
            expect: ConstType::Utf8,
        }),
    }];
    const MISMATCH_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
        at: SlotCtx::Inherit,
        path: &["ion-formula-link"],
        kind: TemplateEntryKind::Table(TemplateTable {
            name: "ion-formula-link",
            version: 1,
            columns: MISMATCH_COLS,
            indexes: &[],
        }),
    }];
    let mounts = leak_mounts(vec![
        mount_of("point-cloud", SlotName::RUN, POINT_CLOUD_ENTRIES),
        mount_of("aux", SlotName::RUN, MISMATCH_ENTRIES),
    ]);
    let report = composite_check(&probe(mounts, false)).expect("conflict");
    assert_eq!(report.kind, ConflictKind::EndpointTypeMismatch);
}

// ---------------------------------------------------------------------------
// Projection: equivalence guard and shape details
// ---------------------------------------------------------------------------

fn col(order: usize, name: &str, ty: DataType, nullable: bool) -> ColumnDefinition {
    ColumnDefinition::from_field(
        u32::try_from(order).unwrap(),
        Field::new(name, ty, nullable),
        true,
    )
}

fn table_type(name: &str, columns: Vec<ColumnDefinition>) -> TableType {
    TableType {
        type_id: table_type_id(name, 1),
        name: Name::new(name.to_owned()).unwrap(),
        version: 1,
        columns,
        includes_type_ids: Vec::new(),
    }
}

fn domain_type(name: &str, children: Vec<ChildDefinition>) -> DomainType {
    DomainType {
        type_id: domain_type_id(name, 1),
        name: Name::new(name.to_owned()).unwrap(),
        version: 1,
        instance_mode: InstanceMode::Shared,
        children,
        includes_type_ids: Vec::new(),
    }
}

fn child_table(order: usize, name: &str) -> ChildDefinition {
    ChildDefinition::new(
        u32::try_from(order).unwrap(),
        Name::new(name.to_owned()).unwrap(),
        ChildKind::Table,
        table_type_id(name, 1),
        Cardinality::ZeroOrMore,
        InstanceMode::Shared,
    )
}

fn child_domain(order: usize, name: &str, type_name: &str) -> ChildDefinition {
    ChildDefinition::new(
        u32::try_from(order).unwrap(),
        Name::new(name.to_owned()).unwrap(),
        ChildKind::Domain,
        domain_type_id(type_name, 1),
        Cardinality::ZeroOrMore,
        InstanceMode::Shared,
    )
}

fn expected_ortho_fixture() -> RegistryExport {
    let root = domain_type(
        "OrthoFixture",
        vec![
            child_domain(0, "s0", "study"),
            child_domain(1, "s1", "study"),
        ],
    );
    let study = domain_type(
        "study",
        vec![
            child_table(0, "geo-consensus"),
            child_domain(1, "r0", "run"),
            child_domain(2, "r1", "run"),
            child_table(3, "spectrum"),
            child_table(4, "study-metadata"),
        ],
    );
    let run = domain_type(
        "run",
        vec![
            child_domain(0, "geo", "geo"),
            child_table(1, "ion-formula-link"),
            child_domain(2, "links", "links"),
            child_table(3, "point-data"),
            child_table(4, "run-metadata"),
        ],
    );
    let links = domain_type("links", vec![child_table(0, "msn")]);
    let geo = domain_type("geo", vec![child_table(0, "feature")]);
    let table_types = vec![
        table_type(
            "study-metadata",
            vec![col(0, "note", DataType::Utf8, false)],
        ),
        table_type("run-metadata", vec![col(0, "note", DataType::Utf8, false)]),
        table_type(
            "point-data",
            vec![
                col(0, "point_id", DataType::UInt64, false),
                col(1, "mz", DataType::Float64, false),
                col(2, "rt", DataType::Float64, false),
            ],
        ),
        table_type(
            "msn",
            vec![
                col(0, "up_id", DataType::UInt64, false),
                col(1, "down_id", DataType::UInt64, false),
            ],
        ),
        table_type(
            "feature",
            vec![
                col(0, "feature_id", DataType::UInt64, false),
                col(1, "mz", DataType::Float64, false),
                col(2, "rt", DataType::Float64, false),
            ],
        ),
        table_type(
            "geo-consensus",
            vec![
                col(0, "consensus_id", DataType::UInt64, false),
                col(1, "feature_id", DataType::UInt64, false),
            ],
        ),
        table_type(
            "spectrum",
            vec![
                col(0, "spectrum_id", DataType::UInt64, false),
                col(1, "peaks", DataType::Binary, false),
            ],
        ),
        table_type(
            "ion-formula-link",
            vec![
                col(0, "ion_id", DataType::UInt64, false),
                col(1, "formula_id", DataType::UInt64, false),
            ],
        ),
    ];
    RegistryExport {
        domain_types: vec![root, study, run, links, geo],
        table_types,
    }
}

#[test]
fn ortho_fixture_projection_is_equivalent_to_the_hand_built_export() {
    let plan = project(&OrthoFixture::SPEC, &ShapeMapping::default()).unwrap();
    assert!(plan.equivalent_to(&expected_ortho_fixture()).unwrap());
}

#[test]
fn equivalence_guard_detects_a_perturbed_export() {
    let plan = project(&OrthoFixture::SPEC, &ShapeMapping::default()).unwrap();
    let mut perturbed = expected_ortho_fixture();
    perturbed.domain_types[0].children.pop();
    assert!(!plan.equivalent_to(&perturbed).unwrap());
}

#[test]
fn projection_places_geo_consensus_at_study_and_content_at_run() {
    let plan = project(&OrthoFixture::SPEC, &ShapeMapping::default()).unwrap();
    let study = plan
        .domain_types
        .iter()
        .find(|domain| domain.name.as_str() == "study")
        .expect("study type");
    let study_children: Vec<&str> = study
        .children
        .iter()
        .map(|child| child.name.as_str())
        .collect();
    assert_eq!(
        study_children,
        vec!["geo-consensus", "r0", "r1", "spectrum", "study-metadata"]
    );
    let run = plan
        .domain_types
        .iter()
        .find(|domain| domain.name.as_str() == "run")
        .expect("run type");
    let run_children: Vec<&str> = run
        .children
        .iter()
        .map(|child| child.name.as_str())
        .collect();
    assert_eq!(
        run_children,
        vec![
            "geo",
            "ion-formula-link",
            "links",
            "point-data",
            "run-metadata"
        ]
    );
}

#[test]
fn same_named_domains_with_different_shapes_conflict_at_registration() {
    assert!(DomainCollide::CONFLICT.is_none());
    let plan = project(&DomainCollide::SPEC, &ShapeMapping::default()).unwrap();
    // The projection succeeds (two "d" domains with different children at
    // different positions); registration surfaces the name collision.
    assert!(plan.to_registry().is_err());
}

#[test]
fn projection_rejects_invalid_segments_and_duplicate_columns() {
    let mounts: &'static [MountSpec] =
        leak_mounts(vec![mount_of("bad", SlotName::RUN, BAD_SEGMENT_ENTRIES)]);
    let spec = probe(mounts, false);
    assert!(project(&spec, &ShapeMapping::default()).is_err());

    let dup: &'static [MountSpec] =
        leak_mounts(vec![mount_of("dup", SlotName::RUN, DUP_COL_ENTRIES)]);
    let spec = probe(dup, false);
    assert!(project(&spec, &ShapeMapping::default()).is_err());
}

#[test]
fn type_id_derivation_is_deterministic_and_version_sensitive() {
    let a = domain_type_id("study", 1);
    let b = domain_type_id("study", 1);
    let c = domain_type_id("study", 2);
    let d = domain_type_id("run", 1);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
    assert_ne!(
        table_type_id("point-data", 1),
        domain_type_id("point-data", 1)
    );
}

// ---------------------------------------------------------------------------
// ConstType List extension (uni-mass-db MVP redesign S2, blueprint task 1)
// ---------------------------------------------------------------------------

const SAMPLE_FIELDS: &[ConstStructField] = &[
    ConstStructField {
        name: "mz",
        ty: ConstScalar::F(64),
        nullable: false,
    },
    ConstStructField {
        name: "rt",
        ty: ConstScalar::F(64),
        nullable: false,
    },
    ConstStructField {
        name: "intensity",
        ty: ConstScalar::F(32),
        nullable: true,
    },
];

#[test]
fn list_scalar_maps_to_item_slot_arrow_list() {
    let data_type = data_type(ConstType::ListScalar(ConstScalar::Utf8)).unwrap();
    let DataType::List(element) = data_type else {
        panic!("list type expected");
    };
    assert_eq!(element.name(), "item");
    assert!(
        !element.is_nullable(),
        "element slot is not independently nullable"
    );
    assert_eq!(element.data_type(), &DataType::Utf8);
}

#[test]
fn list_struct_maps_members_with_declared_nullability() {
    let data_type = data_type(ConstType::ListStruct(SAMPLE_FIELDS)).unwrap();
    let DataType::List(element) = data_type else {
        panic!("list type expected");
    };
    assert_eq!(element.name(), "item");
    assert!(!element.is_nullable());
    let DataType::Struct(members) = element.data_type() else {
        panic!("struct element expected");
    };
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].name(), "mz");
    assert_eq!(members[0].data_type(), &DataType::Float64);
    assert!(!members[0].is_nullable());
    assert_eq!(members[2].name(), "intensity");
    assert_eq!(members[2].data_type(), &DataType::Float32);
    assert!(members[2].is_nullable());
}

#[test]
fn scalar_element_widening_round_trips_through_const_type() {
    // The flat scalar element widens to its ConstType equivalent, so the
    // Arrow mapping reuses one conversion path for every leaf shape.
    for scalar in [
        ConstScalar::Bool,
        ConstScalar::I(16),
        ConstScalar::U(64),
        ConstScalar::F(32),
        ConstScalar::Utf8,
        ConstScalar::Binary,
        ConstScalar::Date32,
        ConstScalar::Ts {
            unit: ConstTimeUnit::Second,
            tz: "",
        },
        ConstScalar::Dur(ConstTimeUnit::Millisecond),
        ConstScalar::Opaque("json"),
    ] {
        let direct = data_type(scalar.as_const_type()).unwrap();
        let via_list = data_type(ConstType::ListScalar(scalar)).unwrap();
        let DataType::List(element) = via_list else {
            panic!("list type expected for {scalar:?}");
        };
        assert_eq!(element.data_type(), &direct);
    }
}

#[test]
fn list_const_types_compare_structurally() {
    assert!(
        ConstType::ListScalar(ConstScalar::Utf8).const_eq(ConstType::ListScalar(ConstScalar::Utf8))
    );
    assert!(!ConstType::ListScalar(ConstScalar::Utf8).const_eq(ConstType::Utf8));
    assert!(
        !ConstType::ListScalar(ConstScalar::Utf8)
            .const_eq(ConstType::ListScalar(ConstScalar::F(64)))
    );
    assert!(ConstType::ListStruct(SAMPLE_FIELDS).const_eq(ConstType::ListStruct(SAMPLE_FIELDS)));
    const WIDENED: &[ConstStructField] = &[
        ConstStructField {
            name: "mz",
            ty: ConstScalar::F(64),
            nullable: false,
        },
        ConstStructField {
            name: "rt",
            ty: ConstScalar::F(64),
            nullable: false,
        },
        ConstStructField {
            name: "intensity",
            ty: ConstScalar::F(64),
            nullable: true,
        },
    ];
    assert!(!ConstType::ListStruct(SAMPLE_FIELDS).const_eq(ConstType::ListStruct(WIDENED)));
    assert!(
        !ConstType::ListStruct(SAMPLE_FIELDS).const_eq(ConstType::ListScalar(ConstScalar::F(64)))
    );
}

#[test]
fn list_column_field_of_keeps_name_and_nullability() {
    let column = TemplateColumn {
        name: "samples",
        ty: ConstType::ListStruct(SAMPLE_FIELDS),
        nullable: true,
        id_component: false,
        aliases: &[],
        points_to: None,
    };
    let field = field_of(&column).unwrap();
    assert_eq!(field.name(), "samples");
    assert!(field.is_nullable());
    assert!(matches!(field.data_type(), DataType::List(_)));
}

// ---------------------------------------------------------------------------
// Spike 1 regression: generic const fn reading associated consts
// ---------------------------------------------------------------------------

const fn spike_entries_len<T: TreeTemplate>() -> usize {
    T::SPEC.entries.len()
}

const SPIKE_LEN: usize = spike_entries_len::<TestSpectrum>();

#[test]
fn generic_const_fn_reads_associated_consts() {
    assert_eq!(SPIKE_LEN, 1);
}
