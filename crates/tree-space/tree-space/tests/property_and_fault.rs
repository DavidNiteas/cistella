use proptest::prelude::*;
use tree_space::ErrorCode;
use tree_space::fault::{FaultPlan, FaultPoint};
use tree_space::layout::{FlatDirLayout, PublishPlan, SingleFileLayout, StorageLayout};
use tree_space::manifest::BootstrapImage;
use tree_space::path::TablePath;
use tree_space::shared::{SharedRegionName, SharedSnapshot};
use tree_space::slot::{Residency, TableInner, TableSlot};

proptest! {
    #[test]
    fn path_roundtrip_is_canonical(segments in prop::collection::vec("[A-Za-z][A-Za-z0-9_-]{0,8}", 1..6)) {
        let text = segments.join("/");
        let path = TablePath::parse(&text).unwrap();
        prop_assert_eq!(path.to_string(), text);
        prop_assert_eq!(TablePath::parse(path.to_string()).unwrap(), path);
    }
}

#[test]
fn fault_plan_is_predictable_and_one_shot() {
    let plan = FaultPlan::new();
    plan.fail(FaultPoint::BeforeTrailer, 1);
    let error = plan.hit(FaultPoint::BeforeTrailer).unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageCorrupt);
    assert!(plan.hit(FaultPoint::BeforeTrailer).is_ok());
}

#[test]
fn m11_shared_snapshot_keeps_arrow_buffers_alive() {
    let slot = TableSlot::new_loaded(TableInner {
        batches: vec![],
        residency: Residency::Mapped,
        digest: None,
    });
    let shared = SharedSnapshot::new(slot.snapshot().unwrap());
    let clone = shared.clone_handle();
    assert!(clone.get().batches.is_empty());
    slot.unload().unwrap();
    assert!(shared.get().batches.is_empty());
    assert_eq!(
        SharedRegionName::new("tree-space-test").unwrap().as_str(),
        "tree-space-test"
    );
}

#[test]
fn flat_dir_fault_before_manifest_keeps_old_head() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    let image = BootstrapImage::built_in().unwrap();
    let plan = FaultPlan::new();
    plan.fail(FaultPoint::BeforeManifest, 1);
    let layout = FlatDirLayout::new(&root).with_fault(plan);
    layout.create(&image).unwrap();
    let error = layout
        .publish(PublishPlan {
            bootstrap: image.clone(),
            sequence: 1,
            table_payloads: vec![],
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageCorrupt);
    // The visibility point was never switched: the old head remains fully readable.
    assert_eq!(layout.open_bootstrap().unwrap().manifest, image.manifest);
}

#[test]
fn single_file_fault_before_trailer_keeps_old_head() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let image = BootstrapImage::built_in().unwrap();
    let plan = FaultPlan::new();
    plan.fail(FaultPoint::BeforeTrailer, 1);
    let layout = SingleFileLayout::new(&path).with_fault(plan);
    layout.create(&image).unwrap();
    let error = layout
        .publish(PublishPlan {
            bootstrap: image.clone(),
            sequence: 1,
            table_payloads: vec![],
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageCorrupt);
    assert_eq!(layout.open_bootstrap().unwrap().manifest, image.manifest);
}
