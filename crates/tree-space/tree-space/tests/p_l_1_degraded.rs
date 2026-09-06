//! PL-1 S6: degraded blocks — `DegradedBlock` + the `Bucket` parallel slot +
//! projection/verify degraded semantics (`_dev/插件化改造/02-施工路线图.md` §5.8
//! / 01 §4-5).
//!
//! Four anchors:
//!
//! - `p_l_1_deg_projection_errs`: a library containing an unknown-disk-version
//!   block (forged payload magic + unknown side-table literal) opens fine
//!   (tree parseable, other leaves readable) and `project<T>` on the degraded
//!   leaf fails with `Err(PluginMissing{kind, disk})`;
//! - `p_l_1_deg_verify_addr_only`: verify of a library with a degraded block
//!   checks the degraded rows' addresses only (no canonical identity
//!   comparison — the identity is incomputable without the plugin, yet verify
//!   passes) and reports the degraded count through the opened bucket;
//!   tampering the degraded raw bytes turns verification into
//!   `DigestMismatch`;
//! - `p_l_1_deg_integrity_preserved`: content integrity is never exempted by
//!   degradation — an address-forged degraded object fails the open-path
//!   address recheck with `DigestMismatch`;
//! - `p_l_1_deg_partial_read`: degraded and healthy blocks coexist in one
//!   tree — the healthy leaf keeps its envelope slot and typed read path
//!   while the degraded block lives verbatim in the parallel slot.
//!
//! The fixture is hand-crafted at the layout level (a normal `TbLibrary::commit`
//! always materializes through a working plugin, so a plugin-unreachable block
//! is built by writing the block objects, ref table and version side-table
//! directly). Three degraded triggers are exercised:
//!
//! - (a) unknown side-table literal (`DiskLayout::from_str` → `None`);
//! - (b) known disk version with no routable plugin (`route_disk` miss,
//!   `ArrowParquet` × `Blob`);
//! - (c) routable plugin whose decode fails (doctored frame length field).

use std::path::{Path, PathBuf};
use tempfile::tempdir;
use tree_space::layout::flat_dir::FlatDirLayout;
use tree_space::layout::tb::hex16;
use tree_space::tree::codec::{ImageContent, TreeImage, decode_block_leaf, encode};
use tree_space::{
    Blob, Block, BlockKind, Digest, DiskVersion, ErrorCode, RefId, RefRow, TbLayout, TbLibrary,
    TreeCodec, TreeNode, VersionRow, block_blob_address, canonical_xpath_bytes, encode_ref_table,
    encode_versions_table, image_leaf_refs, named_field, ref_table_address, tb_commit_batch,
    tb_commit_id, tree_blob_address, tree_id, versions_table_address,
};

/// A committed flat library with one healthy `Blob` leaf (`normal`) and one
/// plugin-unreachable leaf (`alien`): the alien object bytes are kept on disk
/// verbatim, its ref-table row records `block_blob_address(alien_raw)`, and
/// the version side-table labels it with `alien_disk`.
struct DegradedFixture {
    root: PathBuf,
    normal_id: RefId,
    alien_id: RefId,
    alien_raw: Vec<u8>,
    alien_address: [u8; 16],
}

/// Hand-crafts the library at the layout level: writes the two block objects,
/// the canonical tree, the three-column ref table, the version side-table and
/// a nine-column head commit (see the module comment).
fn build_fixture(root: &Path, alien_raw: Vec<u8>, alien_disk: &str) -> DegradedFixture {
    // `create` provisions the directories + boot record; the manual writes use
    // a fresh layout instance over the same root.
    let _ = TbLibrary::create(root).unwrap();
    let layout = FlatDirLayout::new(root);

    // Healthy leaf: canonical blob envelope (IPC path).
    let normal = Blob::new(vec![0xbb]);
    let normal_id = normal.ref_id();
    let normal_raw = normal.envelope().encode();

    // Alien leaf: writer-side canonical identity (identity ⊥ addressing — the
    // tree/ref-table reference the canonical identity while the disk object is
    // the plugin-unreachable raw bytes).
    let alien_orig = Blob::new(vec![0xaa; 8]);
    let alien_id = alien_orig.ref_id();
    let alien_address = block_blob_address(&alien_raw).as_bytes();

    // Canonical tree bytes + addresses.
    let image = TreeImage::new(vec![
        named_field("normal", ImageContent::Ref(normal_id)),
        named_field("alien", ImageContent::Ref(alien_id)),
    ]);
    let tree_bytes = encode(&image).unwrap();
    let tree_address = tree_blob_address(&tree_bytes);
    let root_tree_id = tree_id(&tree_bytes).as_bytes();

    // Ref rows: canonical xpath bytes; the address column points at the
    // physical object (hash of the *alien raw* bytes for the alien leaf).
    let mut rows = Vec::new();
    for (xpath, ref_id) in image_leaf_refs(&image).unwrap() {
        let address = if ref_id == alien_id {
            alien_address
        } else {
            block_blob_address(&normal_raw).as_bytes()
        };
        rows.push(RefRow {
            xpath: canonical_xpath_bytes(&xpath),
            ref_id: ref_id.as_bytes(),
            address,
        });
    }
    rows.sort();
    let ref_bytes = encode_ref_table(&rows).unwrap();
    let refs_address = ref_table_address(&ref_bytes);

    // Version side-table: the alien leaf labeled with the supplied (known or
    // unknown) disk literal; the healthy leaf on the IPC path.
    let version_rows = rows
        .iter()
        .map(|row| VersionRow {
            xpath: row.xpath.clone(),
            mem_ver: "arrow55".to_owned(),
            disk_ver: if row.ref_id == alien_id.as_bytes() {
                alien_disk.to_owned()
            } else {
                "arrow-ipc".to_owned()
            },
        })
        .collect::<Vec<_>>();
    let versions_bytes = encode_versions_table(&version_rows).unwrap();
    let versions_address = versions_table_address(&versions_bytes);

    // Persist the objects.
    layout
        .write_block_object(
            Digest::from_bytes(block_blob_address(&normal_raw).as_bytes()),
            &normal_raw,
        )
        .unwrap();
    layout
        .write_block_object(Digest::from_bytes(alien_address), &alien_raw)
        .unwrap();
    layout.write_tree_object(tree_address, &tree_bytes).unwrap();
    layout.write_ref_object(refs_address, &ref_bytes).unwrap();
    layout
        .write_versions_object(versions_address, &versions_bytes)
        .unwrap();

    // Nine-column head commit pointing at the ref table + side-table.
    let sequence = 1_u64;
    let parent = [0_u8; 16];
    let commit_id = tb_commit_id(
        sequence,
        parent,
        root_tree_id,
        tree_address.as_bytes(),
        refs_address.as_bytes(),
    );
    let batch = tb_commit_batch(
        sequence,
        parent,
        root_tree_id,
        tree_address.as_bytes(),
        refs_address.as_bytes(),
        None,
        Some(versions_address.as_bytes()),
    )
    .unwrap();
    layout
        .write_commit_object(commit_id, &tree_space::ipc::encode_batch(&batch).unwrap())
        .unwrap();
    layout.write_committed(commit_id, sequence).unwrap();

    DegradedFixture {
        root: root.to_path_buf(),
        normal_id,
        alien_id,
        alien_raw,
        alien_address,
    }
}

/// Trigger (a): a valid built-in blob frame whose payload magic is forged to an
/// unknown literal (`FANCY1`) — the frame header still parses (kind = `Blob`),
/// the payload does not.
fn forged_blob_frame() -> Vec<u8> {
    let mut raw = Blob::new(vec![0xaa; 8]).envelope().encode();
    raw[11..17].copy_from_slice(b"FANCY1");
    raw
}

/// Trigger (b): a perfectly healthy IPC blob frame, labeled `"arrow-parquet"`
/// — a known disk version whose plugin routes `ArrowTable` only, so a `Blob`
/// has no routable plugin.
fn healthy_blob_frame() -> Vec<u8> {
    Blob::new(vec![0xaa; 8]).envelope().encode()
}

/// Trigger (c): a valid frame with a doctored payload-length field — the frame
/// header parses, but the plugin decode rejects it (length mismatch) even
/// though the side-table labels it with the routable `"arrow-ipc"`.
fn corrupt_length_blob_frame() -> Vec<u8> {
    let mut raw = Blob::new(vec![0xaa; 8]).envelope().encode();
    let len = u64::from_le_bytes(raw[3..11].try_into().unwrap());
    assert_eq!(raw.len(), 11 + len as usize);
    raw[3..11].copy_from_slice(&(len + 1).to_le_bytes());
    raw
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct Surface {
    normal: Blob,
    alien: Blob,
}

/// Unknown-disk-version block (forged magic + unknown side-table literal):
/// the library opens (tree parseable, healthy leaf readable, verify passes on
/// the address-only branch), and `project<T>` on the degraded leaf fails with
/// `Err(PluginMissing{kind, disk})`.
#[test]
fn p_l_1_deg_projection_errs() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let fixture = build_fixture(&root, forged_blob_frame(), "fancy-format");
    let library = TbLibrary::open(&fixture.root).unwrap();

    // Open succeeded and the tree is parseable; the healthy leaf restored.
    assert_eq!(library.bucket().unwrap().degraded().len(), 1);
    assert!(
        library.bucket().unwrap().get(fixture.normal_id).is_some(),
        "healthy leaf restores through the IPC plugin"
    );
    assert_eq!(
        library.verify().unwrap(),
        2,
        "degraded row counts into verify (address-only branch)"
    );

    // Typed projection reaches the healthy leaf first and then the degraded
    // leaf → PluginMissing with kind/disk context (01 §4-5 / 02 §5.8).
    let error = library.project::<Surface>().unwrap_err();
    assert_eq!(error.code, ErrorCode::PluginMissing);
    assert_eq!(error.code.as_str(), "plugin_missing");
    assert_eq!(error.context.get("kind").map(String::as_str), Some("blob"));
    assert_eq!(
        error.context.get("disk").map(String::as_str),
        Some("fancy-format")
    );
}

/// Verify on a degraded block checks the address only (the canonical identity
/// is incomputable without the plugin — yet verification passes), reports the
/// degraded count through the opened bucket, and tampering the raw bytes turns
/// the address re-check into `DigestMismatch`. Both the unknown-literal trigger
/// (a) and the no-routable-plugin trigger (b) are covered.
#[test]
fn p_l_1_deg_verify_addr_only() {
    for (disk_label, alien_raw) in [
        ("fancy-format", forged_blob_frame()),
        ("arrow-parquet", healthy_blob_frame()),
    ] {
        let temp = tempdir().unwrap();
        let root = temp.path().join("library");
        let fixture = build_fixture(&root, alien_raw, disk_label);
        let library = TbLibrary::open(&fixture.root).unwrap();

        // One degraded block, observed through the public parallel slot.
        assert_eq!(
            library.bucket().unwrap().degraded().len(),
            1,
            "degraded count for label {disk_label}"
        );
        assert_eq!(
            library
                .bucket()
                .unwrap()
                .degraded_get(fixture.alien_id)
                .unwrap()
                .disk,
            DiskVersion(disk_label.to_owned()),
            "the side-table literal is preserved verbatim"
        );

        // The degraded row is address-verified only: its canonical identity is
        // not computable (forged magic / no plugin), yet verify passes and
        // counts the row — the identity surface was necessarily not compared.
        assert_eq!(
            library.verify().unwrap(),
            2,
            "degraded row counted, address still verified ({disk_label})"
        );

        // Tampering the degraded raw bytes breaks the address re-check even
        // though the row stays plugin-unreachable.
        let mut tampered = fixture.alien_raw.clone();
        tampered.push(0xcc);
        std::fs::write(
            fixture
                .root
                .join("tb-blocks")
                .join(format!("{}.bin", hex16(&fixture.alien_address))),
            tampered,
        )
        .unwrap();
        let error = library.verify().unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::DigestMismatch,
            "degraded rows are still hash-verified ({disk_label})"
        );
    }
}

/// Content integrity is never exempted by degradation: an address-forged
/// degraded object fails the open-path address recheck with
/// `DigestMismatch`, even though no plugin could route it anyway.
#[test]
fn p_l_1_deg_integrity_preserved() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let fixture = build_fixture(&root, forged_blob_frame(), "fancy-format");

    // Replace the alien object bytes under its recorded address: the new bytes
    // hash to a different address, so the restore path rejects the row.
    let mut tampered = fixture.alien_raw.clone();
    tampered.push(0xcc);
    std::fs::write(
        fixture
            .root
            .join("tb-blocks")
            .join(format!("{}.bin", hex16(&fixture.alien_address))),
        tampered,
    )
    .unwrap();

    let error = match TbLibrary::open(&fixture.root) {
        Ok(_) => panic!("an address-forged degraded object must fail the open"),
        Err(error) => error,
    };
    assert_eq!(
        error.code,
        ErrorCode::DigestMismatch,
        "hash(raw) is recomputed for the would-be degraded block and never waived"
    );
}

/// Degraded and healthy blocks coexist in one tree: the healthy leaf keeps its
/// envelope slot and typed read path; the degraded block (here trigger (c) — a
/// routable plugin whose decode fails) lives verbatim in the parallel slot.
#[test]
fn p_l_1_deg_partial_read() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let fixture = build_fixture(&root, corrupt_length_blob_frame(), "arrow-ipc");
    let library = TbLibrary::open(&fixture.root).unwrap();
    let bucket = library.bucket().unwrap();

    // The decode-failure block degrades with kind parsed from the frame header,
    // disk label and the original bytes preserved verbatim.
    assert_eq!(bucket.degraded().len(), 1);
    let degraded = bucket.degraded_get(fixture.alien_id).unwrap();
    assert_eq!(degraded.kind, BlockKind::Blob);
    assert_eq!(degraded.disk, DiskVersion("arrow-ipc".to_owned()));
    assert_eq!(degraded.raw, fixture.alien_raw, "original disk bytes kept");

    // The healthy leaf is untouched: envelope slot + typed leaf read path.
    assert!(bucket.get(fixture.normal_id).is_some());
    assert!(
        bucket.get(fixture.alien_id).is_none(),
        "the degraded identity stays out of the healthy envelope slot"
    );
    let normal: Blob = decode_block_leaf(fixture.normal_id, &BlockKind::Blob, bucket).unwrap();
    assert_eq!(normal.bytes(), &[0xbb]);

    // Whole-library verify still counts both rows.
    assert_eq!(library.verify().unwrap(), 2);
}
