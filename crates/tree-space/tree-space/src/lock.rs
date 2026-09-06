//! std::fs::File sidecar and single-file lock adaptation (MSRV 1.95).

use crate::error::{ErrorCode, Result, TreeSpaceError};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// An acquired exclusive file lock whose release is owned by the OS and file handle.
#[derive(Debug)]
pub struct FileLock {
    file: File,
    /// Path opened for the lock; sidecars are never renamed or deleted by tree-space.
    pub path: PathBuf,
}

impl FileLock {
    /// Acquires a non-blocking exclusive lock from a read/write, non-append handle.
    pub fn try_exclusive(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(lock_io)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(lock_io)?;
        file.try_lock().map_err(|error| {
            TreeSpaceError::new(ErrorCode::LockUnavailable, "exclusive lock is unavailable")
                .with_context("path", path.display().to_string())
                .with_context("detail", error.to_string())
        })?;
        Ok(Self { file, path })
    }

    /// Releases this lock early; dropping also releases it.
    pub fn unlock(self) -> Result<()> {
        self.file.unlock().map_err(lock_io)
    }
}

fn lock_io(error: std::io::Error) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::LockUnavailable,
        "cannot open or release lock file",
    )
    .with_context("detail", error.to_string())
}
