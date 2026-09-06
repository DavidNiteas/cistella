//! E-2: permission (ownership + write gate) and access forms (mapped / full /
//! lazy) behavior.
//!
//! Asserts the protocol `交换协议/02-施工路线图.md` §6.4 obligations: holder
//! queries, runtime-entry ownership, the single-writer `require_owner` gate,
//! runtime invalidation overlays, full/lazy/block reads over a committed
//! library, zero-copy mapped reads with invalidation exposure, the disk-source
//! mapping (E-4 behavior upgrade, `02-施工路线图.md` §8.7-1), and the
//! runtime-table read dispatch.

use std::path::PathBuf;
use tree_space::block::RefId;
use tree_space::error::ErrorCode;
use tree_space::exchange::access::read_block;
use tree_space::shared::SharedRegion;
use tree_space::tree::codec::{TreeImage, encode, encode_node};
use tree_space::{
    AccessMode, Blob, Bucket, ExchangeKey, Holder, InvalidState, IpcSource, RuntimeEntry,
    RuntimeTable, Source, TreeCodec, TreeNode, Value, open_block_mapped, open_tree_mapped,
    read_block_full, read_lazy, read_tree, read_tree_full, require_owner,
};
use tree_space::{BlockRead, LazySnapshot, MappedView, OwnedSnapshot, TreeRead};

/// A small typed tree used to exercise `OwnedSnapshot::project` and the proxy
/// payload mirroring over a committed library.
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct LabSample {
    name: Value,
    block: Blob,
}

/// Builds the on-disk fixture: a fresh library, a bucket holding the sample
/// block, a committed typed tree, and the library reopened so its in-memory
/// loaded state (`tree_image`/`bucket`) is restored.
fn committed_library(root: &std::path::Path) -> (tree_space::TbLibrary, LabSample) {
    let library = tree_space::TbLibrary::create(root.join("library")).unwrap();
    let block = Blob::new(vec![9, 8, 7]);
    let fixture = LabSample {
        name: Value::Utf8("lab".into()),
        block: block.clone(),
    };
    let mut bucket = Bucket::new();
    bucket.put(&block);
    let tree_bytes = encode_node(&fixture).unwrap();
    let leaf_refs = fixture.leaf_refs();
    library
        .commit(&tree_bytes, &leaf_refs, &bucket)
        .expect("sample fixture commits");
    drop(library);
    (
        tree_space::TbLibrary::open(root.join("library")).expect("committed library reopens"),
        fixture,
    )
}

/// The only sample block id of [`committed_library`].
fn sample_block_id(library: &tree_space::TbLibrary) -> RefId {
    library
        .bucket()
        .expect("loaded bucket")
        .ids()
        .next()
        .expect("one sample block")
}

#[test]
fn e_2_holder_is_owner_only_for_own() {
    let own = Holder::Own;
    let mapped = Holder::Mapped(Source::Disk(PathBuf::from("peer-library")));
    assert!(own.is_owner());
    assert!(!mapped.is_owner());
    assert!(!own.is_mapped());
    assert!(mapped.is_mapped());
    assert!(own.source().is_none());
    let source = mapped.source().expect("mapped holder carries its source");
    assert_eq!(source, &Source::Disk(PathBuf::from("peer-library")));
}

#[test]
fn e_2_runtime_entry_is_owner() {
    assert!(RuntimeEntry::default().is_owner());
    let mapped = RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    };
    assert!(!mapped.is_owner());
}

#[test]
fn e_2_require_owner_gates_writes() {
    // Tree entry Own (the L3 default) → write allowed.
    let table = RuntimeTable::default();
    assert!(require_owner(&table, ExchangeKey::Tree).is_ok());

    // Tree entry Mapped → denied.
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    });
    assert_eq!(
        require_owner(&table, ExchangeKey::Tree).unwrap_err().code,
        ErrorCode::OwnershipDenied
    );

    // Block entry Mapped → denied.
    let block_id = RefId::from_bytes([0x11; 16]);
    table.set_block_entry(
        block_id,
        RuntimeEntry {
            holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
            ..RuntimeEntry::default()
        },
    );
    assert_eq!(
        require_owner(&table, ExchangeKey::Block(block_id))
            .unwrap_err()
            .code,
        ErrorCode::OwnershipDenied
    );

    // Absent block entry → denied ("not locally owned", §6.6).
    let absent = RefId::from_bytes([0xaa; 16]);
    assert_eq!(
        require_owner(&table, ExchangeKey::Block(absent))
            .unwrap_err()
            .code,
        ErrorCode::OwnershipDenied
    );
}

#[test]
fn e_2_mark_invalid_overlays_runtime_event() {
    // Runtime event on the tree after recording a mapped view.
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    });
    table
        .mark_invalid(ExchangeKey::Tree, InvalidState::UpstreamChanged)
        .unwrap();
    assert_eq!(table.tree_entry().invalid, InvalidState::UpstreamChanged);

    // Runtime event on a present block entry.
    let block_id = RefId::from_bytes([0x22; 16]);
    table.set_block_entry(block_id, RuntimeEntry::default());
    table
        .mark_invalid(ExchangeKey::Block(block_id), InvalidState::UpstreamDead)
        .unwrap();
    assert_eq!(
        table.block_entry(block_id).unwrap().invalid,
        InvalidState::UpstreamDead
    );

    // An absent block entry cannot be invalidated (dangling reference).
    let absent = RefId::from_bytes([0xbb; 16]);
    assert_eq!(
        table
            .mark_invalid(ExchangeKey::Block(absent), InvalidState::UpstreamChanged)
            .unwrap_err()
            .code,
        ErrorCode::DanglingReference
    );
}

#[test]
fn e_2_read_tree_full_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let (library, fixture) = committed_library(temp.path());
    let snapshot = read_tree_full(&library).unwrap();

    // The snapshot image/bucket equal the loaded library state (owned copy).
    assert_eq!(snapshot.image, *library.tree_image().unwrap());
    assert_eq!(
        snapshot,
        OwnedSnapshot {
            image: library.tree_image().unwrap().clone(),
            bucket: library.bucket().unwrap().clone(),
        }
    );

    // The snapshot projects into the typed tree the library committed.
    let projected: LabSample = snapshot.project().unwrap();
    assert_eq!(projected, fixture);
}

#[test]
fn e_2_read_block_full_copies_single_block() {
    let temp = tempfile::tempdir().unwrap();
    let (library, _fixture) = committed_library(temp.path());
    let bucket = library.bucket().unwrap();
    let id = sample_block_id(&library);

    let envelope = read_block_full(&library, id).unwrap();
    assert_eq!(envelope, *bucket.get(id).unwrap());

    let unknown = RefId::from_bytes([0xee; 16]);
    assert_eq!(
        read_block_full(&library, unknown).unwrap_err().code,
        ErrorCode::DanglingReference
    );
}

#[test]
fn e_2_open_tree_mapped_zero_copy_roundtrip() {
    let tree_bytes = encode(&TreeImage::new_empty()).unwrap();
    let region = SharedRegion::publish(tree_bytes.clone()).unwrap();
    let view = open_tree_mapped(Source::Ipc(IpcSource::new(region.export().unwrap()))).unwrap();

    // Zero-copy: the mapped bytes are exactly the published canonical bytes.
    assert_eq!(view.bytes().unwrap(), tree_bytes.as_slice());
    assert_eq!(view.invalid_state(), InvalidState::Valid);
}

#[test]
fn e_2_mapped_view_invalidation_exposure() {
    let tree_bytes = encode(&TreeImage::new_empty()).unwrap();
    let region = SharedRegion::publish(tree_bytes.clone()).unwrap();
    let view = open_tree_mapped(Source::Ipc(IpcSource::new(region.export().unwrap()))).unwrap();

    // Upstream changed: reads still succeed (current view, 01 §7 case 1).
    view.mark_invalid(InvalidState::UpstreamChanged);
    assert_eq!(view.invalid_state(), InvalidState::UpstreamChanged);
    assert_eq!(view.bytes().unwrap(), tree_bytes.as_slice());

    // Upstream dead: reads are refused (may hold no data, 01 §7 case 2).
    view.mark_invalid(InvalidState::UpstreamDead);
    assert_eq!(view.invalid_state(), InvalidState::UpstreamDead);
    assert_eq!(
        view.bytes().unwrap_err().code,
        ErrorCode::RequiredDataMissing
    );
}

#[test]
fn e_2_mapped_disk_source_maps_committed_tree() {
    // E-4 behavior upgrade (02 §8.7-1): `Source::Disk` is no longer rejected —
    // it maps the committed tree blob of a real on-disk library (01 §7 base).
    let temp = tempfile::tempdir().unwrap();
    let (library, _fixture) = committed_library(temp.path());
    let root = temp.path().join("library");
    let expected_tree = encode(library.tree_image().unwrap()).unwrap();
    drop(library);

    // Tree mapped read over a disk source: the committed tree blob bytes,
    // valid invalidation state.
    let view = open_tree_mapped(Source::Disk(root.clone())).unwrap();
    assert_eq!(view.bytes().unwrap(), expected_tree.as_slice());
    assert_eq!(view.invalid_state(), InvalidState::Valid);

    // Block mapped read entry over a disk source maps the same committed tree
    // (a bare library path locates only the committed tree; block addresses
    // come from the reference table via `map_block_disk`, 02 §8.2-3).
    let view = open_block_mapped(Source::Disk(root)).unwrap();
    assert_eq!(view.bytes().unwrap(), expected_tree.as_slice());

    // A nonexistent path still probes as ambiguous.
    assert!(matches!(
        open_tree_mapped(Source::Disk(PathBuf::from("peer-library"))),
        Err(e) if e.code == ErrorCode::MappingAmbiguous
    ));
}

#[test]
fn e_2_read_lazy_defers_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let (library, _fixture) = committed_library(temp.path());
    let id = sample_block_id(&library);
    let owned = library.bucket().unwrap().get(id).unwrap().clone();

    let lazy = read_lazy(&library).unwrap();
    assert_eq!(lazy.image(), library.tree_image().unwrap());

    // No on-demand copy has happened yet.
    assert!(!lazy.is_materialized(id));
    // First access copies from the source library bucket into the reader.
    let envelope = lazy.block(id).unwrap();
    assert_eq!(envelope, owned);
    assert!(lazy.is_materialized(id));
    // Cache hit: a second access returns an equal copy without re-reading.
    assert_eq!(lazy.block(id).unwrap(), envelope);

    // Unknown id → dangling reference.
    let unknown = RefId::from_bytes([0xff; 16]);
    assert_eq!(
        lazy.block(unknown).unwrap_err().code,
        ErrorCode::DanglingReference
    );
}

#[test]
fn e_2_dispatch_read_tree_and_block() {
    let temp = tempfile::tempdir().unwrap();
    let (library, _fixture) = committed_library(temp.path());
    let id = sample_block_id(&library);
    let bucket = library.bucket().unwrap();

    // Tree dispatched as Mapped: the recorded source region is mapped.
    let tree_bytes = encode(&TreeImage::new_empty()).unwrap();
    let region = SharedRegion::publish(tree_bytes.clone()).unwrap();
    let ipc = IpcSource::new(region.export().unwrap());
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Ipc(ipc.clone())),
        access: AccessMode::Mapped,
        source: Some(Source::Ipc(ipc)),
        invalid: InvalidState::Valid,
    });
    let read = read_tree(&library, &table).unwrap();
    assert!(matches!(
        read,
        TreeRead::Mapped(view) if view.bytes().unwrap() == tree_bytes.as_slice()
    ));

    // Tree dispatched as Full (the L1/L3 default): owned snapshot.
    let read = read_tree(&library, &RuntimeTable::default()).unwrap();
    assert!(
        matches!(read, TreeRead::Full(snapshot) if snapshot.image == *library.tree_image().unwrap())
    );

    // Block dispatched as Mapped: the recorded source region is mapped.
    let block_payload = vec![1_u8, 2, 3];
    let block_region = SharedRegion::publish(block_payload.clone()).unwrap();
    let block_ipc = IpcSource::new(block_region.export().unwrap());
    let mut table = RuntimeTable::default();
    table.set_block_entry(
        id,
        RuntimeEntry {
            holder: Holder::Mapped(Source::Ipc(block_ipc.clone())),
            access: AccessMode::Mapped,
            source: Some(Source::Ipc(block_ipc)),
            invalid: InvalidState::Valid,
        },
    );
    let read = read_block(&library, &table, id).unwrap();
    assert!(matches!(
        read,
        BlockRead::Mapped(view) if view.bytes().unwrap() == block_payload.as_slice()
    ));

    // Block dispatched as Full (default entry): owned envelope copy.
    let mut table = RuntimeTable::default();
    table.set_block_entry(id, RuntimeEntry::default());
    let read = read_block(&library, &table, id).unwrap();
    assert!(matches!(
        read,
        BlockRead::Full(envelope) if envelope == *bucket.get(id).unwrap()
    ));
}

#[test]
fn e_2_mapped_and_lazy_views_are_send_sync() {
    // Closing criterion (02 §6.5): the invalidation flag / copy cache is
    // guarded by a Mutex, so a mapping handle may cross threads.
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<MappedView>();
    assert_sync::<MappedView>();
    assert_send::<LazySnapshot<'static>>();
    assert_sync::<LazySnapshot<'static>>();
}
