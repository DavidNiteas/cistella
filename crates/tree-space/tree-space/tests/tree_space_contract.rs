//! Legacy v4 contract closure (PL-1 S7, superseded by the v4-face removal in
//! the legacy-hygiene work order): the v4 table-space assertions were first
//! pruned to the deprecation gate, and the retired `Library` / `VerifyLevel`
//! faces are now deleted outright. The single surviving case asserts the
//! layout-independent `lock` face, which the TB face still depends on and
//! which must stay live. See the S7 report for the per-assertion deletion
//! ledger.

use tree_space::ErrorCode;

#[test]
fn m7_lock_backend_distinguishes_contention() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("x.lock");
    let first = tree_space::lock::FileLock::try_exclusive(&path).unwrap();
    let error = tree_space::lock::FileLock::try_exclusive(&path).unwrap_err();
    assert_eq!(error.code, ErrorCode::LockUnavailable);
    drop(first);
}
