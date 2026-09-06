//! The `tree_compose!` composition macro.
//!
//! Expands one composite declaration into: a marker struct, its structured
//! [`crate::template::CompositeSpec`] constant, a `CONFLICT` diagnostic constant, and one
//! compile-time assertion running the full check pipeline of the selected
//! mode. Composites never fold at compile time and never implement
//! [`TreeTemplate`](crate::template::TreeTemplate); a larger composition
//! re-declares its members.

/// Composes tree templates into a composite, checked at compile time.
///
/// Syntax:
///
/// ```text
/// tree_compose! {
///     (doc attributes)* (visibility)? composite NAME = host(HOST_TYPE)
///         mount(SLOT, TEMPLATE_TYPE (, as ROLE_LABEL)?)*
///         mode (Orthogonal | Merge);
/// }
/// ```
///
/// - `host` owns the composite's coordinate system (its
///   [`TemplateSpec::coordinates`](crate::template::TemplateSpec::coordinates));
///   every mount slot and every `At` anchor is validated against it.
/// - `mount` attaches a member template: `Inherit` content lands under the
///   slot's coordinate chain; `as` records the compile-time role label
///   (semantic role injected by the mount position, never materialized into
///   data).
/// - `Orthogonal` makes every cross-template path conflict a compile-time
///   error, except re-declarations of the same table with identical
///   structural fingerprints, which deduplicate (ruling P4). `Merge` lets
///   later mounts override earlier ones leaf-by-leaf in declaration order.
/// - Under both modes the checks cover: mount slot validity, member
///   `required_slots` coverage, `At` anchor legality, and `points_to`
///   endpoint existence plus type match.
///
/// The expansion is pure addition: one marker struct, one `CompositeSpec`
/// constant, one `CONFLICT` diagnostic, and one `const` assertion. When the
/// assertion fails the literal message names the composite; the generated
/// `CONFLICT` constant carries the first [`crate::template::ConflictReport`].
///
/// # Example (clean composition)
///
/// ```no_run
/// use tree_space::template::{
///     ConstType, SlotCtx, SlotDecl, SlotName, TemplateColumn, TemplateEntry, TemplateEntryKind,
///     TemplateSpec, TemplateTable, TierDecl, TreeTemplate,
/// };
/// use tree_space::tree_compose;
///
/// const COLS: &[TemplateColumn] = &[TemplateColumn {
///     name: "id",
///     ty: ConstType::U(64),
///     nullable: false,
///     id_component: true,
///     aliases: &[],
///     points_to: None,
/// }];
/// const TABLE: TemplateTable = TemplateTable {
///     name: "point-data",
///     version: 1,
///     columns: COLS,
///     indexes: &[],
/// };
/// const ENTRIES: &[TemplateEntry] = &[TemplateEntry {
///     at: SlotCtx::Inherit,
///     path: &["point-data"],
///     kind: TemplateEntryKind::Table(TABLE),
/// }];
///
/// struct Host;
/// impl TreeTemplate for Host {
///     const NAME: &'static str = "host";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[SlotDecl {
///             slot: SlotName::RUN,
///             parent: None,
///             tier: Some(TierDecl { prefix: "r", max_instances: 4 }),
///         }],
///         required_slots: &[],
///         entries: &[],
///     };
/// }
/// struct PointCloud;
/// impl TreeTemplate for PointCloud {
///     const NAME: &'static str = "point-cloud";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[],
///         required_slots: &[SlotName::RUN],
///         entries: ENTRIES,
///     };
/// }
///
/// tree_compose! {
///     pub composite Mvp = host(Host)
///         mount(SlotName::RUN, PointCloud)
///         mode Orthogonal;
/// }
/// const _: () = assert!(Mvp::CONFLICT.is_none());
/// ```
///
/// # Compile-time rejection: orthogonal violation
///
/// Two member templates claim the same table path with different column
/// sets; under `Orthogonal` the composite does not compile.
///
/// ```compile_fail
/// use tree_space::template::{
///     ConstType, SlotCtx, SlotDecl, SlotName, TemplateColumn, TemplateEntry, TemplateEntryKind,
///     TemplateSpec, TemplateTable, TierDecl, TreeTemplate,
/// };
/// use tree_space::tree_compose;
///
/// const COLS_A: &[TemplateColumn] = &[TemplateColumn {
///     name: "id",
///     ty: ConstType::U(64),
///     nullable: false,
///     id_component: true,
///     aliases: &[],
///     points_to: None,
/// }];
/// const COLS_B: &[TemplateColumn] = &[
///     TemplateColumn {
///         name: "id",
///         ty: ConstType::U(64),
///         nullable: false,
///         id_component: true,
///         aliases: &[],
///         points_to: None,
///     },
///     TemplateColumn {
///         name: "extra",
///         ty: ConstType::Utf8,
///         nullable: true,
///         id_component: false,
///         aliases: &[],
///         points_to: None,
///     },
/// ];
/// const ENTRY_A: &[TemplateEntry] = &[TemplateEntry {
///     at: SlotCtx::Inherit,
///     path: &["shared"],
///     kind: TemplateEntryKind::Table(TemplateTable {
///         name: "shared",
///         version: 1,
///         columns: COLS_A,
///         indexes: &[],
///     }),
/// }];
/// const ENTRY_B: &[TemplateEntry] = &[TemplateEntry {
///     at: SlotCtx::Inherit,
///     path: &["shared"],
///     kind: TemplateEntryKind::Table(TemplateTable {
///         name: "shared",
///         version: 1,
///         columns: COLS_B,
///         indexes: &[],
///     }),
/// }];
///
/// struct Host;
/// impl TreeTemplate for Host {
///     const NAME: &'static str = "host";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[SlotDecl {
///             slot: SlotName::RUN,
///             parent: None,
///             tier: Some(TierDecl { prefix: "r", max_instances: 4 }),
///         }],
///         required_slots: &[],
///         entries: &[],
///     };
/// }
/// struct A;
/// impl TreeTemplate for A {
///     const NAME: &'static str = "a";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[],
///         required_slots: &[],
///         entries: ENTRY_A,
///     };
/// }
/// struct B;
/// impl TreeTemplate for B {
///     const NAME: &'static str = "b";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[],
///         required_slots: &[],
///         entries: ENTRY_B,
///     };
/// }
///
/// tree_compose! {
///     composite Bad = host(Host)
///         mount(SlotName::RUN, A)
///         mount(SlotName::RUN, B)
///         mode Orthogonal;
/// }
/// ```
///
/// # Compile-time rejection: endpoint missing
///
/// The auxiliary template declares a `points_to` endpoint at a template that
/// does not participate in the composite (R3': the check belongs to the
/// composition, never to the template definition).
///
/// ```compile_fail
/// use tree_space::template::{
///     ConstType, EndpointRef, SlotCtx, SlotDecl, SlotName, TemplateColumn, TemplateEntry,
///     TemplateEntryKind, TemplateSpec, TemplateTable, TierDecl, TreeTemplate,
/// };
/// use tree_space::tree_compose;
///
/// const LINK_COLS: &[TemplateColumn] = &[TemplateColumn {
///     name: "ion_id",
///     ty: ConstType::U(64),
///     nullable: false,
///     id_component: false,
///     aliases: &[],
///     points_to: Some(EndpointRef {
///         template: "point-cloud",
///         leaf: &["point-data"],
///         column: "point_id",
///         expect: ConstType::U(64),
///     }),
/// }];
/// const LINK_ENTRIES: &[TemplateEntry] = &[TemplateEntry {
///     at: SlotCtx::Inherit,
///     path: &["ion-formula-link"],
///     kind: TemplateEntryKind::Table(TemplateTable {
///         name: "ion-formula-link",
///         version: 1,
///         columns: LINK_COLS,
///         indexes: &[],
///     }),
/// }];
///
/// struct Host;
/// impl TreeTemplate for Host {
///     const NAME: &'static str = "host";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[SlotDecl {
///             slot: SlotName::RUN,
///             parent: None,
///             tier: Some(TierDecl { prefix: "r", max_instances: 4 }),
///         }],
///         required_slots: &[],
///         entries: &[],
///     };
/// }
/// struct IonFormulaLink;
/// impl TreeTemplate for IonFormulaLink {
///     const NAME: &'static str = "ion-formula-link";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[],
///         required_slots: &[SlotName::RUN],
///         entries: LINK_ENTRIES,
///     };
/// }
///
/// tree_compose! {
///     composite Aux = host(Host)
///         mount(SlotName::RUN, IonFormulaLink)
///         mode Orthogonal;
/// }
/// ```
///
/// # Compile-time rejection: slot illegal
///
/// The mount references a coordinate slot the host does not declare.
///
/// ```compile_fail
/// use tree_space::template::{
///     SlotCtx, SlotDecl, SlotName, TemplateColumn, TemplateEntry, TemplateEntryKind,
///     TemplateSpec, TemplateTable, TierDecl, TreeTemplate, ConstType,
/// };
/// use tree_space::tree_compose;
///
/// const COLS: &[TemplateColumn] = &[TemplateColumn {
///     name: "id",
///     ty: ConstType::U(64),
///     nullable: false,
///     id_component: true,
///     aliases: &[],
///     points_to: None,
/// }];
/// const ENTRIES: &[TemplateEntry] = &[TemplateEntry {
///     at: SlotCtx::Inherit,
///     path: &["spectrum"],
///     kind: TemplateEntryKind::Table(TemplateTable {
///         name: "spectrum",
///         version: 1,
///         columns: COLS,
///         indexes: &[],
///     }),
/// }];
///
/// struct Host;
/// impl TreeTemplate for Host {
///     const NAME: &'static str = "host";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[SlotDecl {
///             slot: SlotName::RUN,
///             parent: None,
///             tier: Some(TierDecl { prefix: "r", max_instances: 4 }),
///         }],
///         required_slots: &[],
///         entries: &[],
///     };
/// }
/// struct Spectrum;
/// impl TreeTemplate for Spectrum {
///     const NAME: &'static str = "spectrum";
///     const SPEC: TemplateSpec = TemplateSpec {
///         version: 1,
///         coordinates: &[],
///         required_slots: &[SlotName::LIB],
///         entries: ENTRIES,
///     };
/// }
///
/// tree_compose! {
///     composite WrongSlot = host(Host)
///         mount(SlotName::LIB, Spectrum)
///         mode Orthogonal;
/// }
/// ```
#[macro_export]
macro_rules! tree_compose {
    (@mode Orthogonal) => {
        false
    };
    (@mode Merge) => {
        true
    };
    (@label) => {
        ""
    };
    (@label $label:expr) => {
        $label
    };
    (
        $(#[$meta:meta])*
        $vis:vis composite $name:ident = host($host:ty)
        $( mount($slot:expr, $ty:ty $(, as $label:expr)?) )*
        mode $mode:ident;
    ) => {
        $(#[$meta])*
        #[doc = concat!(
            "Composite tree template `",
            stringify!($name),
            "`: a structured host-plus-mounts constant with compile-time",
            " composition checks (no runtime recombination)."
        )]
        #[derive(Debug, Clone, Copy)]
        $vis struct $name;

        impl $name {
            #[doc = "The structured composite specification: host plus ordered mounts."]
            pub const SPEC: $crate::template::CompositeSpec = $crate::template::CompositeSpec {
                name: stringify!($name),
                host_name: <$host as $crate::template::TreeTemplate>::NAME,
                host: <$host as $crate::template::TreeTemplate>::SPEC,
                mounts: &[
                    $(
                        $crate::template::MountSpec {
                            slot: $slot,
                            label: $crate::tree_compose!(@label $($label)?),
                            template_name: <$ty as $crate::template::TreeTemplate>::NAME,
                            spec: <$ty as $crate::template::TreeTemplate>::SPEC,
                        }
                    ),*
                ],
                merge: $crate::tree_compose!(@mode $mode),
            };

            #[doc = "The first conflict found in this composite, or `None` when it is clean."]
            pub const CONFLICT: Option<$crate::template::ConflictReport> =
                $crate::template::composite_check(&Self::SPEC);
        }

        const _: () = assert!(
            $crate::template::composite_check(&$name::SPEC).is_none(),
            concat!(
                "tree_compose: composite `",
                stringify!($name),
                "` violates its composition rules (see <",
                stringify!($name),
                ">::CONFLICT for the first conflict report)"
            )
        );
    };
}
