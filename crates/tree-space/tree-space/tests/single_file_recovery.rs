//! M4 single-file container recovery: truncation, corrupt trailer, forged prev pointer.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use tree_space::layout::{PublishPlan, SingleFileLayout, StorageLayout};
use tree_space::manifest::BootstrapImage;

const TRAILER_LEN: u64 = 64;

fn fresh(path: &std::path::Path) -> SingleFileLayout {
    let layout = SingleFileLayout::new(path);
    layout.create(&BootstrapImage::built_in().unwrap()).unwrap();
    layout
}

fn read_bytes(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn write_at(path: &std::path::Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn truncation_recovers_to_last_valid_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let layout = fresh(&path);
    // Append one more epoch so there is a previous valid trailer to fall back to.
    layout
        .publish(PublishPlan {
            bootstrap: BootstrapImage::built_in().unwrap(),
            sequence: 1,
            table_payloads: vec![],
        })
        .unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    // Truncate partway through the final trailer.
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(len - 8).unwrap();
    // open must recover the previous complete epoch rather than erroring.
    assert!(layout.open_bootstrap().is_ok());
}

#[test]
fn corrupt_trailer_is_skipped_for_previous_valid() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let layout = fresh(&path);
    layout
        .publish(PublishPlan {
            bootstrap: BootstrapImage::built_in().unwrap(),
            sequence: 1,
            table_payloads: vec![],
        })
        .unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    // Flip bytes in the final trailer's checksum region.
    let mut corrupt = read_bytes(&path);
    let tail = (len - TRAILER_LEN + 60) as usize;
    for byte in &mut corrupt[tail..tail + 4] {
        *byte ^= 0xff;
    }
    std::fs::write(&path, &corrupt).unwrap();
    assert!(layout.open_bootstrap().is_ok());
}

#[test]
fn forged_prev_pointer_does_not_break_latest_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("library.umdb");
    let layout = fresh(&path);
    layout
        .publish(PublishPlan {
            bootstrap: BootstrapImage::built_in().unwrap(),
            sequence: 1,
            table_payloads: vec![],
        })
        .unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    // Corrupt the previous-trailer-offset field (bytes 36..44 of the trailer).
    write_at(
        &path,
        len - TRAILER_LEN + 36,
        &[0xde, 0xad, 0xbe, 0xef, 0x00, 0x00, 0x00, 0x00],
    );
    // Discovery must not trust the forged prev pointer; the valid latest trailer wins.
    assert!(layout.open_bootstrap().is_ok());
    // And a subsequent append must still work off the structurally-scanned tail.
    let _ = read_bytes(&path);
    layout
        .publish(PublishPlan {
            bootstrap: BootstrapImage::built_in().unwrap(),
            sequence: 2,
            table_payloads: vec![],
        })
        .unwrap();
    assert!(layout.open_bootstrap().is_ok());
}
