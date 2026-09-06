//! M0 architecture gates for the Windows filesystem semantics used by tree-space.
//!
//! These tests intentionally use helper test processes: the contract concerns
//! independently opened handles, not merely multiple handles in one process.

#![cfg(windows)]

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use memmap2::MmapOptions;
use tempfile::tempdir;

const HOLDER_TARGET: &str = "TABLE_SPACE_M0_HOLDER_TARGET";
const HOLDER_READY: &str = "TABLE_SPACE_M0_HOLDER_READY";
const HOLDER_RELEASE: &str = "TABLE_SPACE_M0_HOLDER_RELEASE";
const HOLDER_OBSERVED: &str = "TABLE_SPACE_M0_HOLDER_OBSERVED";
const LOCK_TARGET: &str = "TABLE_SPACE_M0_LOCK_TARGET";
const LOCK_READY: &str = "TABLE_SPACE_M0_LOCK_READY";

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn path_from_env(name: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("missing helper environment variable `{name}`"))
}

fn wait_for(path: &Path, description: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
    panic!("timed out waiting for {description}: {}", path.display());
}

fn spawn_ignored_helper(test_name: &str, envs: &[(&str, &Path)]) -> Child {
    let exe = env::current_exe().expect("resolve integration-test executable");
    let mut command = Command::new(exe);
    command
        .arg("--ignored")
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture");
    for (key, value) in envs {
        command.env(key, value);
    }
    command.spawn().expect("spawn M0 helper process")
}

/// Helper process used by [`windows_mmap_replace_gate`].
#[test]
#[ignore]
fn mmap_holder_process() {
    let target = path_from_env(HOLDER_TARGET);
    let ready = path_from_env(HOLDER_READY);
    let release = path_from_env(HOLDER_RELEASE);
    let observed = path_from_env(HOLDER_OBSERVED);
    let file = File::open(&target).expect("open target for a read-only mapping");

    // SAFETY: `file` stays open until after the mapping is read; the test never
    // mutates the mapped bytes through this mapping.
    let mapping = unsafe { MmapOptions::new().map(&file) }.expect("map target read-only");
    assert_eq!(&mapping[..], b"old-epoch");
    fs::write(&ready, b"mapped").expect("publish mmap-holder readiness");
    wait_for(&release, "mmap holder release signal");
    fs::write(&observed, &mapping[..]).expect("publish mapped bytes after replacement attempt");
}

/// Helper process used by [`windows_sidecar_lock_release_gate`].
#[test]
#[ignore]
fn sidecar_lock_holder_process() {
    let target = path_from_env(LOCK_TARGET);
    let ready = path_from_env(LOCK_READY);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(target)
        .expect("open sidecar lock file");
    file.lock().expect("acquire exclusive sidecar lock");
    fs::write(&ready, b"locked").expect("publish sidecar-holder readiness");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

/// Verifies the actual Windows behavior when a process maps a target file and
/// another process tries `tmp -> target` replacement.
///
/// Both outcomes are valid architecture-gate observations. The design chooses
/// mapped shape C only when replacement succeeds; a blocked replacement selects
/// owned shape B for Windows writers. In either case, the old mapping must keep
/// reading the original bytes.
#[test]
fn windows_mmap_replace_gate() {
    let temp = tempdir().expect("create M0 mmap gate directory");
    let target = temp.path().join("library.umdb");
    let temporary = temp.path().join("library.umdb.tmp");
    let ready = temp.path().join("mmap-ready");
    let release = temp.path().join("mmap-release");
    let observed = temp.path().join("mmap-observed");
    fs::write(&target, b"old-epoch").expect("seed mapped target");

    let mut holder = spawn_ignored_helper(
        "mmap_holder_process",
        &[
            (HOLDER_TARGET, &target),
            (HOLDER_READY, &ready),
            (HOLDER_RELEASE, &release),
            (HOLDER_OBSERVED, &observed),
        ],
    );
    wait_for(&ready, "mmap holder readiness");

    fs::write(&temporary, b"new-epoch").expect("write replacement candidate");
    let replacement = fs::rename(&temporary, &target);
    let outcome = if replacement.is_ok() {
        "replacement-succeeds"
    } else {
        "replacement-blocked"
    };
    println!("M0 Windows mmap/rename gate: {outcome}");

    fs::write(&release, b"release").expect("release mmap holder");
    let holder_status = holder.wait().expect("wait for mmap holder");
    assert!(
        holder_status.success(),
        "mmap holder failed: {holder_status}"
    );
    assert_eq!(
        fs::read(&observed).expect("read mapped bytes after replacement attempt"),
        b"old-epoch"
    );

    match replacement {
        Ok(()) => assert_eq!(
            fs::read(&target).expect("read replacement target"),
            b"new-epoch"
        ),
        Err(error) => {
            assert_eq!(
                fs::read(&target).expect("read original target after blocked replacement"),
                b"old-epoch"
            );
            assert!(
                temporary.exists(),
                "blocked replacement must retain its temporary file"
            );
            println!("M0 Windows mmap/rename error: {error}");
        }
    }
}

/// Confirms that an exclusive sidecar lock blocks a second process and is
/// released by the operating system when the holder process terminates.
#[test]
fn windows_sidecar_lock_release_gate() {
    let temp = tempdir().expect("create M0 lock gate directory");
    let sidecar = temp.path().join("library.umdb.lock");
    let ready = temp.path().join("lock-ready");
    let mut holder = spawn_ignored_helper(
        "sidecar_lock_holder_process",
        &[(LOCK_TARGET, &sidecar), (LOCK_READY, &ready)],
    );
    wait_for(&ready, "sidecar lock holder readiness");

    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sidecar)
        .expect("open sidecar lock contender");
    assert!(
        contender.try_lock().is_err(),
        "another process must not acquire an already locked sidecar"
    );

    holder.kill().expect("terminate sidecar lock holder");
    let holder_status = holder.wait().expect("wait for terminated sidecar holder");
    assert!(
        !holder_status.success(),
        "terminated helper unexpectedly succeeded"
    );

    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if contender.try_lock().is_ok() {
            contender.unlock().expect("unlock recovered sidecar lock");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sidecar lock was not released after holder termination"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

/// Records whether append-only handles can be used for the chosen locking
/// backend. The result determines the required open mode for sidecar files.
#[test]
fn windows_append_handle_lock_gate() {
    let temp = tempdir().expect("create M0 append-lock gate directory");
    let sidecar = temp.path().join("library.umdb.lock");
    let mut append_only = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&sidecar)
        .expect("open append-only sidecar handle");
    append_only
        .write_all(b"gate")
        .expect("write through append-only handle");

    match append_only.try_lock() {
        Ok(()) => {
            println!("M0 Windows append-only lock gate: succeeds");
            append_only.unlock().expect("unlock append-only handle");
        }
        Err(error) => println!("M0 Windows append-only lock gate: blocked ({error})"),
    }
}
