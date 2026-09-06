//! P-IO-2: envelope blob channel (`tb-blocks/`), bucket restore, and the empty
//! v4 tree / empty metadata snapshot degradation objects.
//!
//! Freezes golden 16 (degradation objects) and 17 (sample envelope blob
//! address + file bytes), and covers the block-channel behaviors: filename =
//! content address hex, `exists`-skipping dedup, bucket-rebuild equivalence and
//! address-forgery detection.

use tree_space::tree::codec::TreeImage;
use tree_space::xpath::XPath;
use tree_space::{
    Blob, Block, Bucket, Digest, ImageContent, RefId, Sequence, Value, block_blob_address, encode,
    named_field,
};

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}

/// The canonical empty v4 library tree id (golden 16 first part).
const EMPTY_TREE_ID: &str = "94f7112c7db38093bcd4d7879177ac2c";
/// The canonical empty metadata snapshot reference (golden 16 second part).
const EMPTY_METADATA_REF: &str = "6e9b3f26ff015f878a4c3e1f166244dc";

/// Frozen file bytes of the sample Blob envelope (golden 17).
const SAMPLE_ENVELOPE_HEX: &str = "040100f2020000000000004152524f573100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff780000001000000000000a000c000a00090004000a00000010000000000104000800080000000400080000000400000001000000140000001000140010000e000f0004000000080010000000180000000c000000000001041000000000000000040004000400000004000000626c6f62000000000000000000000000ffffffffb8000000100000000c001a0018001700040008000c00000020000000c000000000000000000000000000000304000a0018000c00080004000a0000002c0000001000000001000000000000000000000001000000010000000000000000000000000000000000000003000000000000000000000001000000000000004000000000000000080000000000000080000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000ff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000074622d73616d706c6500000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0000000014000000000000000c00140012000c00080004000c000000640000008000000010000000000004000800080000000400080000000400000001000000140000001000140010000e000f0004000000080010000000180000000c000000000001041000000000000000040004000400000004000000626c6f620000000001000000c000000000000000c000000000000000c0000000000000000000000000000000a00000004152524f5731";
const SAMPLE_ADDRESS: &str = "d796cdd97c2ecac5dbeb3cc6ac9dc605";
/// Golden-17 semantic RefId (PL-2 M2 re-freeze: `Bytes` value of the blob).
const SAMPLE_REF_ID: &str = "d647966165bab1e26a2cbec5ec794735";

fn sample_blob() -> Blob {
    Blob::new(vec![
        0x74, 0x62, 0x2d, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x00,
    ])
}

#[test]
fn p_io_2_empty_degredation_objects_are_frozen() {
    // Golden 16 (first part): the empty v4 tree object id is the v4 tree
    // formula over zero rows.
    assert_eq!(
        tree_space::layout::tb::empty_tree_id(),
        Digest::from_bytes(hex16(EMPTY_TREE_ID))
    );
    assert_eq!(
        tree_space::layout::tb::empty_tree_id(),
        tree_space::layout::flat_dir::tree_id_golden(&[])
    );

    // Golden 16 (second part): the empty metadata snapshot reference is the v4
    // metadata formula over the built-in empty bootstrap tables.
    assert_eq!(
        tree_space::layout::tb::empty_metadata_ref().unwrap(),
        Digest::from_bytes(hex16(EMPTY_METADATA_REF))
    );
}

#[test]
fn p_io_2_sample_envelope_blob_address_is_frozen() {
    // Golden 17: the sample envelope's full frame bytes and its storage
    // address (independent of the semantic RefId).
    let bytes = sample_blob().envelope().encode();
    assert_eq!(hex(&bytes), SAMPLE_ENVELOPE_HEX);
    let address = block_blob_address(&bytes);
    assert_eq!(address.as_bytes(), hex16(SAMPLE_ADDRESS));
    assert_eq!(sample_blob().ref_id().as_bytes(), hex16(SAMPLE_REF_ID));
    assert_ne!(
        address.as_bytes(),
        hex16(SAMPLE_REF_ID),
        "storage addressing must stay independent of the semantic RefId"
    );
}

#[test]
fn p_io_2_tb_blocks_channel_persists_and_dedups_across_commits() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();

    let tree = TreeImage::new(vec![
        named_field("blob", ImageContent::Ref(sample_blob().ref_id())),
        named_field(
            "seq",
            ImageContent::Ref(Sequence::new(vec![Value::I32(5)]).ref_id()),
        ),
    ]);
    let mut bucket = Bucket::new();
    let blob_id = bucket.put(&sample_blob());
    let seq_id = bucket.put(&Sequence::new(vec![Value::I32(5)]));
    let tree_bytes = encode(&tree).unwrap();

    library
        .commit(&tree_bytes, &leaf_refs_manual(&tree), &bucket)
        .unwrap();
    // Second commit over the same block set must reuse the same two blobs.
    library
        .commit(&tree_bytes, &leaf_refs_manual(&tree), &bucket)
        .unwrap();

    assert_eq!(blob_id.as_bytes(), hex16(SAMPLE_REF_ID));
    assert_ne!(seq_id, blob_id);

    let blocks_dir = temp.path().join("library").join("tb-blocks");
    let mut files = std::fs::read_dir(&blocks_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files.sort();

    let blob_address = block_blob_address(&sample_blob().envelope().encode());
    let seq_envelope = Sequence::new(vec![Value::I32(5)]);
    let seq_address = block_blob_address(&seq_envelope.envelope().encode());

    // Filenames are the 16-byte content-hash hex (32 characters + .bin).
    let mut expected = vec![
        format!("{}.bin", blob_address),
        format!("{}.bin", seq_address),
    ];
    expected.sort();
    assert_eq!(files, expected);

    // The blob file content is the exact envelope frame bytes.
    let blob_file = blocks_dir.join(format!("{}.bin", blob_address));
    let written = std::fs::read(&blob_file).unwrap();
    assert_eq!(written, sample_blob().envelope().encode());
    assert_eq!(written, unhex(SAMPLE_ENVELOPE_HEX));
}

#[test]
fn p_io_2_bucket_restore_rebuilds_identical_envelope_set() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();

    let mut bucket = Bucket::new();
    bucket.put(&sample_blob());
    bucket.put(&Sequence::new(vec![Value::I32(5), Value::Utf8("x".into())]));

    let tree = TreeImage::new(vec![
        named_field("blob", ImageContent::Ref(sample_blob().ref_id())),
        named_field(
            "seq",
            ImageContent::Ref(Sequence::new(vec![Value::I32(5), Value::Utf8("x".into())]).ref_id()),
        ),
        named_field("inline", ImageContent::Inline(Value::I64(9))),
    ]);
    let tree_bytes = encode(&tree).unwrap();
    library
        .commit(&tree_bytes, &leaf_refs_manual(&tree), &bucket)
        .unwrap();

    let reopened = tree_space::TbLibrary::open(temp.path().join("library")).unwrap();
    let restored = reopened.bucket().unwrap();
    assert_eq!(restored.len(), bucket.len());
    for id in bucket.ids() {
        assert_eq!(restored.get(id).unwrap(), bucket.get(id).unwrap());
    }
    let mut restored_ids = restored.ids().collect::<Vec<_>>();
    restored_ids.sort();
    let mut original_ids = bucket.ids().collect::<Vec<_>>();
    original_ids.sort();
    assert_eq!(restored_ids, original_ids);
}

#[test]
fn p_io_2_bucket_restore_rejects_address_forgery() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let mut bucket = Bucket::new();
    bucket.put(&sample_blob());
    let tree = TreeImage::new(vec![named_field(
        "blob",
        ImageContent::Ref(sample_blob().ref_id()),
    )]);
    let tree_bytes = encode(&tree).unwrap();
    library
        .commit(&tree_bytes, &leaf_refs_manual(&tree), &bucket)
        .unwrap();

    // Forge the blob: keep the correct address name but replace the content
    // bytes, so the file no longer hashes to its own name.
    let blocks_dir = temp.path().join("library").join("tb-blocks");
    let address = block_blob_address(&sample_blob().envelope().encode());
    let blob_path = blocks_dir.join(format!("{address}.bin"));
    let forged = Blob::new(vec![0xff; 32]).envelope().encode();
    assert_ne!(block_blob_address(&forged).as_bytes(), address.as_bytes());
    std::fs::write(&blob_path, &forged).unwrap();
    let forgery = block_blob_address(&forged);
    assert_ne!(forgery.as_bytes(), address.as_bytes());

    let error = match tree_space::TbLibrary::open(temp.path().join("library")) {
        Ok(_) => panic!("forged blob address must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, tree_space::ErrorCode::DigestMismatch);
}

#[test]
fn p_io_2_ref_table_derivation_reuses_block_wholes() {
    // The reference table is derived straight from the tree's leaf references:
    // the same block referenced from two paths yields two rows; the envelope
    // is not reinterpreted, so the derived addresses are the content-hash ones.
    let mut bucket = Bucket::new();
    let blob_id = bucket.put(&sample_blob());
    let seq_id = bucket.put(&Sequence::new(vec![Value::I32(5)]));

    let leaf_refs = vec![
        (XPath::root().field("a"), blob_id),
        (XPath::root().field("b"), blob_id),
        (XPath::root().field("seq"), seq_id),
    ];
    let rows = tree_space::layout::tb::derive_ref_rows(&leaf_refs, &bucket).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].address,
        block_blob_address(&sample_blob().envelope().encode()).as_bytes()
    );
    assert_eq!(rows[0].ref_id, blob_id.as_bytes());
    // Same RefId on two xpaths stays two rows.
    assert_eq!(
        rows[0].xpath,
        tree_space::index::canonical_xpath_bytes(&XPath::root().field("a"))
    );
    assert_eq!(
        rows[1].xpath,
        tree_space::index::canonical_xpath_bytes(&XPath::root().field("b"))
    );
    // Rows are sorted by canonical xpath bytes ascending, then by ref_id.
    assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));

    // A leaf pointing at a block absent from the bucket is a derivation error.
    let dangling = blob_id;
    let missing = Bucket::new();
    let error =
        tree_space::layout::tb::derive_ref_rows(&[(XPath::root().field("x"), dangling)], &missing)
            .unwrap_err();
    assert_eq!(error.code, tree_space::ErrorCode::DanglingReference);
}

fn leaf_refs_manual(image: &TreeImage) -> Vec<(XPath, RefId)> {
    // For the tiny P-IO-2 images we walk with the universal image helper; it is
    // also the same walk the reopen/verify path uses.
    tree_space::layout::tb::image_leaf_refs(image).unwrap()
}
