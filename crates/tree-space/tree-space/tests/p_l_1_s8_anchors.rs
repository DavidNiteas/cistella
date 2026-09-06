//! PL-1 S8 cross-point anchors (02 §5.10 / 04 全量收口): the whole-chain
//! assertions that span two or more of S1–S7 and were not yet covered under
//! their §5.10 name by a single test.
//!
//! - `p_l_1_boot_unknown_tree_version`: a committed flat library whose boot
//!   record carries an unknown tree disk version (`"nope"`) fails
//!   `TbLibrary::open` at bootstrap step 1 with `BootstrapIncomplete`. The
//!   `decode_boot` unit rejection of that record is already covered by S3
//!   (`p_l_1_boot_bad_or_missing`); this anchor proves the open-path end to
//!   end through the boot chain (S4 step 1), complementing S3's
//!   `p_l_1_open_order_boot_first` (which covers the missing-boot variant).
//! - `p_l_1_route_positive_negative`: the version-routing matrix in one
//!   place — magic positives (`ARROW1` → IPC, `PAR1` → Parquet) and negatives
//!   (unknown magic → `None` = degraded candidate), the kind-constrained miss
//!   (Parquet × Blob → no routable plugin = degraded candidate) and the
//!   routing positives through the registry. The detailed owners remain S1
//!   (`p_l_1_ver_probe_magic`), S2 (`p_l_1_plugin_route`) and S6
//!   (`p_l_1_deg_projection_errs` / `p_l_1_deg_verify_addr_only` — the
//!   open-path degradation of an unknown-magic / kind-miss leaf lives there).

use tree_space::{Block, BlockKind, DiskLayout, PluginRegistry, TbLayout};

#[test]
fn p_l_1_boot_unknown_tree_version() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("library");
    // A healthy library first: `create` writes the canonical four-column boot
    // record, a commit makes the head readable, and the control open runs the
    // full chain to completion.
    let library = tree_space::TbLibrary::create(&root).unwrap();
    let blob = tree_space::Blob::new(vec![7]);
    let mut bucket = tree_space::Bucket::new();
    bucket.put(&blob);
    let image = tree_space::TreeImage::new(vec![tree_space::named_field(
        "blob",
        tree_space::ImageContent::Ref(blob.ref_id()),
    )]);
    let tree_bytes = tree_space::encode(&image).unwrap();
    let leaves = tree_space::image_leaf_refs(&image).unwrap();
    library.commit(&tree_bytes, &leaves, &bucket).unwrap();
    assert!(tree_space::TbLibrary::open(&root).is_ok(), "control open");

    // Overwrite the `tb-boot/boot.ipc` channel with the same record whose
    // `tree_disk_version` is an unknown literal (S3's unit-level "nope"
    // record). The S4 boot chain's step 1 — decode + parse the version pair —
    // must fail the open with BootstrapIncomplete before any commit reading.
    let alien = tree_space::plugin::boot::BootRecord {
        boot_version: tree_space::plugin::boot::BOOT_VERSION,
        library_id: "tree-space".to_owned(),
        tree_mem_version: "arrow55".to_owned(),
        tree_disk_version: "nope".to_owned(),
    };
    tree_space::layout::flat_dir::FlatDirLayout::new(&root)
        .write_boot(&tree_space::plugin::boot::encode_boot(&alien).unwrap())
        .unwrap();

    let error = match tree_space::TbLibrary::open(&root) {
        Ok(_) => panic!("an unknown boot tree disk version must fail the open"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::BootstrapIncomplete);
    assert!(
        error.message.contains("version"),
        "the failure is the step-1 boot parse: {}",
        error.message
    );
    // The commit was complete and healthy — later-chain error categories must
    // not leak (mirrors `p_l_1_open_order_boot_first`'s non-leak asserts).
    assert_ne!(error.code, tree_space::ErrorCode::DanglingReference);
    assert_ne!(error.code, tree_space::ErrorCode::DigestMismatch);
    assert_ne!(error.code, tree_space::ErrorCode::SchemaMismatch);
}

#[test]
fn p_l_1_route_positive_negative() {
    let registry = PluginRegistry::global();

    // Positive magic → disk layout (S1 `DiskLayout::probe_magic`), and each
    // positive disk version routes its declared kinds through the registry
    // (S2 `route_disk`).
    assert_eq!(
        DiskLayout::probe_magic(b"ARROW1\x00\x00\x00\x00"),
        Some(DiskLayout::ArrowIpc)
    );
    assert_eq!(
        DiskLayout::probe_magic(b"PAR1\x00\x00\x00\x00MARK"),
        Some(DiskLayout::ArrowParquet)
    );
    let ipc = registry.route_disk(DiskLayout::ArrowIpc, &BlockKind::Blob);
    assert!(
        ipc.is_some(),
        "ArrowIpc routes native kinds to the IPC plugin"
    );
    assert_eq!(ipc.expect("checked").disk(), DiskLayout::ArrowIpc);
    let parquet = registry.route_disk(DiskLayout::ArrowParquet, &BlockKind::Table);
    assert!(
        parquet.is_some(),
        "ArrowParquet routes Table to the Parquet plugin"
    );
    assert_eq!(parquet.expect("checked").disk(), DiskLayout::ArrowParquet);

    // Negative magic → `None` (degraded candidate; the S6 ref-driven restore
    // labels such rows `DiskVersion("unknown")` and parks the bytes in the
    // bucket's degraded slot — `p_l_1_deg_projection_errs` is the e2e owner).
    for opaque in [
        b"".as_slice(),
        b"native-bytes",
        b"ARROW",
        b"xARROW1",
        b"FANCY1\x00\x00",
    ] {
        assert_eq!(
            DiskLayout::probe_magic(opaque),
            None,
            "opaque magic {opaque:?} is a degraded candidate, not a layout"
        );
    }

    // Kind-constrained negative: `ArrowParquet` × `Blob` has no routable
    // plugin — the route miss is the degraded candidate (S6 trigger (b)).
    assert!(
        registry
            .route_disk(DiskLayout::ArrowParquet, &BlockKind::Blob)
            .is_none(),
        "kind constraint: a Blob under ArrowParquet is a degraded candidate"
    );
}
