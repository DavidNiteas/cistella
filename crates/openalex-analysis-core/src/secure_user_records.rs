//! Handle-rooted authority-record I/O.  This module deliberately accepts only
//! fixed directory components and UUID-derived one-component file names.
//!
//! On Windows every authority operation is rooted at the directory handle
//! captured when the Vault is opened.  We never validate a pathname and then
//! call an ordinary path API for the same object: child names are opened with
//! `NtCreateFile` relative to a verified parent handle with `OBJ_DONT_REPARSE`.

use crate::{CoreError, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct SecureVaultRoot {
    #[cfg(windows)]
    handle: Arc<File>,
    /// A hash of the manifest vault_id. It is stable across a portable copy,
    /// does not disclose the manifest value in the kernel object name, and
    /// prevents unrelated Vaults from sharing a CAS mutex.
    lock_namespace: Arc<str>,
}

#[cfg(windows)]
impl SecureVaultRoot {
    pub(crate) fn open(path: &std::path::Path, vault_id: &str) -> Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        // Opening a directory with std::fs::File::open is rejected by Windows
        // (ERROR_ACCESS_DENIED).  This is the one initial-path operation owned
        // by Vault::open; after it succeeds every Notes operation is relative to
        // this retained directory handle and never resolves root_path() again.
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_handle(raw as RawHandle) };
        verify_directory(&file)?;
        Ok(Self {
            handle: Arc::new(file),
            lock_namespace: Arc::from(vault_lock_namespace(vault_id)),
        })
    }
}
#[cfg(not(windows))]
impl SecureVaultRoot {
    pub(crate) fn open(_: &std::path::Path, _: &str) -> Result<Self> {
        // There is intentionally no ordinary-Path fallback. Unix support is
        // compiled separately below; unsupported targets fail closed.
        Err(CoreError::UnsafeUserRecordPath)
    }
}

fn vault_lock_namespace(vault_id: &str) -> String {
    let digest = Sha256::digest(vault_id.as_bytes());
    format!("{digest:x}")
}

pub(crate) struct SecureUserRecords<'a> {
    root: &'a SecureVaultRoot,
    leaf: &'static str,
}

impl<'a> SecureUserRecords<'a> {
    pub(crate) fn notes(root: &'a SecureVaultRoot) -> Self {
        Self {
            root,
            leaf: "notes",
        }
    }
    pub(crate) fn annotations(root: &'a SecureVaultRoot) -> Self {
        Self {
            root,
            leaf: "annotations",
        }
    }

    pub(crate) fn read(
        &self,
        id: Uuid,
        extension: &'static str,
        kind: &'static str,
    ) -> Result<Vec<u8>> {
        platform::read(self.root, self.leaf, &file_name(id, extension), kind, id)
    }
    pub(crate) fn replace(
        &self,
        id: Uuid,
        extension: &'static str,
        payload: &[u8],
        prefix: &'static str,
    ) -> Result<()> {
        platform::replace(
            self.root,
            self.leaf,
            &file_name(id, extension),
            payload,
            prefix,
        )
    }
    pub(crate) fn delete(
        &self,
        id: Uuid,
        extension: &'static str,
        kind: &'static str,
    ) -> Result<()> {
        platform::delete(self.root, self.leaf, &file_name(id, extension), kind, id)
    }
    pub(crate) fn list(&self, extension: &'static str, kind: &'static str) -> Result<Vec<Uuid>> {
        platform::list(self.root, self.leaf, extension, kind)
    }
    /// Serializes compare-and-swap mutations for one Note ID.  This is an
    /// operating-system mutex rather than a filesystem lock file: it has no
    /// path to resolve, is released by the kernel if the holder crashes, and
    /// therefore cannot become a persistent authority-record artifact.
    pub(crate) fn with_note_cas_lock<T>(
        &self,
        note_id: Uuid,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        platform::with_note_cas_lock(
            self.root,
            self.leaf,
            self.root.lock_namespace.as_ref(),
            note_id,
            operation,
        )
    }
}

fn file_name(id: Uuid, extension: &str) -> String {
    format!("{id}.{extension}")
}
fn not_found(kind: &str, id: Uuid) -> CoreError {
    match kind {
        "note" => CoreError::NoteNotFound(id.to_string()),
        _ => CoreError::AnnotationNotFound(id.to_string()),
    }
}

#[cfg(windows)]
fn verify_directory(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileAttributeTagInfo,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0
        || info.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
            != FILE_ATTRIBUTE_DIRECTORY
    {
        return Err(CoreError::UnsafeUserRecordPath);
    }
    Ok(())
}
#[cfg(windows)]
fn verify_regular(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileAttributeTagInfo,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0
        || info.FileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
    {
        return Err(CoreError::UnsafeUserRecordPath);
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
        FILE_RENAME_INFORMATION, FILE_RENAME_REPLACE_IF_EXISTS, FILE_SYNCHRONOUS_IO_NONALERT,
        FileRenameInformation, NtCreateFile, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::{
        HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileDispositionInfo, SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    fn open_relative(
        parent: &File,
        name: &str,
        directory: bool,
        disposition: u32,
        write: bool,
    ) -> Result<File> {
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        let mut unicode = UNICODE_STRING {
            Length: (wide.len() * 2) as u16,
            MaximumLength: (wide.len() * 2) as u16,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle() as HANDLE,
            ObjectName: &mut unicode,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut ios = IO_STATUS_BLOCK::default();
        let mut raw: HANDLE = std::ptr::null_mut();
        let access = if write {
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE
        } else {
            FILE_GENERIC_READ
        };
        let options = FILE_OPEN_REPARSE_POINT
            | FILE_SYNCHRONOUS_IO_NONALERT
            | if directory { FILE_DIRECTORY_FILE } else { 0 };
        let status = unsafe {
            NtCreateFile(
                &mut raw,
                access,
                &attributes,
                &mut ios,
                std::ptr::null(),
                if directory { 0 } else { FILE_ATTRIBUTE_NORMAL },
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                disposition,
                options,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            // Preserve the one missing-record path required by get_* semantics;
            // every other native open failure is a security refusal rather than
            // a reason to resume through an ordinary Path API.
            const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
            const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003Au32 as i32;
            return if matches!(
                status,
                STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND
            ) {
                Err(
                    std::io::Error::new(std::io::ErrorKind::NotFound, "relative record missing")
                        .into(),
                )
            } else {
                Err(CoreError::UnsafeUserRecordPath)
            };
        }
        let file = unsafe { File::from_raw_handle(raw as RawHandle) };
        if directory {
            verify_directory(&file)?
        } else {
            verify_regular(&file)?
        }
        Ok(file)
    }
    fn dirs(root: &SecureVaultRoot, leaf: &str) -> Result<File> {
        let user = open_relative(root.handle.as_ref(), "user", true, FILE_OPEN_IF, true)?;
        open_relative(&user, leaf, true, FILE_OPEN_IF, true)
    }
    pub(super) fn read(
        root: &SecureVaultRoot,
        leaf: &str,
        name: &str,
        kind: &str,
        id: Uuid,
    ) -> Result<Vec<u8>> {
        let dir = dirs(root, leaf)?;
        let mut f = match open_relative(&dir, name, false, FILE_OPEN, false) {
            Ok(f) => f,
            Err(e) => {
                return if matches!(e, CoreError::Io(_)) {
                    Err(not_found(kind, id))
                } else {
                    Err(e)
                };
            }
        };
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    pub(super) fn replace(
        root: &SecureVaultRoot,
        leaf: &str,
        name: &str,
        payload: &[u8],
        prefix: &str,
    ) -> Result<()> {
        let dir = dirs(root, leaf)?;
        let temp = format!("{prefix}.{}.tmp", Uuid::new_v4());
        let mut f = open_relative(&dir, &temp, false, FILE_CREATE, true)?;
        let outcome = (|| {
            f.write_all(payload)?;
            f.sync_all()?;
            // Commit by native directory-handle-relative rename.  No complete
            // source or target pathname is ever handed to MoveFileEx.
            rename_relative(&f, &dir, name)
        })();
        if outcome.is_err() {
            let _ = mark_delete(&f);
        }
        outcome
    }
    fn rename_relative(file: &File, dir: &File, name: &str) -> Result<()> {
        // NtSetInformationFile preserves RootDirectory semantics.  The Win32
        // SetFileInformationByHandle wrapper rejected a relative target here
        // with ERROR_INVALID_PARAMETER, which would tempt a path-based rename.
        // Keep the commit entirely handle-relative instead.
        let wide: Vec<u16> = name.encode_utf16().collect();
        let size = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName) + wide.len() * 2;
        let mut bytes = vec![0u8; size];
        let record = bytes.as_mut_ptr() as *mut FILE_RENAME_INFORMATION;
        unsafe {
            (*record).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS;
            (*record).RootDirectory = dir.as_raw_handle() as HANDLE;
            (*record).FileNameLength = (wide.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                (*record).FileName.as_mut_ptr(),
                wide.len(),
            );
        }
        let mut ios = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtSetInformationFile(
                file.as_raw_handle() as HANDLE,
                &mut ios,
                record as *const _,
                size as u32,
                FileRenameInformation,
            )
        };
        if status < 0 {
            Err(std::io::Error::from_raw_os_error(status).into())
        } else {
            Ok(())
        }
    }
    fn mark_delete(file: &File) -> Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                FileDispositionInfo,
                &disposition as *const _ as *const _,
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error().into())
        } else {
            Ok(())
        }
    }
    pub(super) fn with_note_cas_lock<T>(
        _root: &SecureVaultRoot,
        _: &str,
        vault_namespace: &str,
        note_id: Uuid,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        use windows_sys::Win32::Foundation::{
            CloseHandle, HANDLE, LocalFree, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        };
        use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
        use windows_sys::Win32::System::Threading::{
            CreateMutexW, ReleaseMutex, WaitForSingleObject,
        };

        // The lock is a Global named kernel mutex: unlike Local\\, the same
        // object is visible to independent processes in different Terminal
        // Services sessions. The name contains only a SHA-256 namespace of the
        // manifest vault_id plus the note UUID, never an absolute path.
        let name: Vec<u16> = format!("Global\\cistella-note-cas-{vault_namespace}-{note_id}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // Use an explicit owner-only SDDL DACL rather than granting every
        // authenticated user GA. The owner is the current Windows account, so
        // that account's processes in every session can share the Global mutex
        // while unrelated users cannot preemptively hold it for a DoS. Failure
        // to create the Global object is terminal and never falls back to a
        // Local or unlocked update.
        let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut security_descriptor_size = 0u32;
        let sddl: Vec<u16> = "D:(A;;GA;;;OW)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut security_descriptor,
                &mut security_descriptor_size,
            )
        };
        if converted == 0 || security_descriptor.is_null() {
            return Err(CoreError::NoteCasLockUnavailable(
                "failed to build mutex security descriptor".to_string(),
            ));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security_descriptor,
            bInheritHandle: 0,
        };
        let mutex = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
        unsafe { LocalFree(security_descriptor as _) };
        if mutex.is_null() {
            return Err(CoreError::NoteCasLockUnavailable(format!(
                "failed to create Global CAS mutex: {}",
                std::io::Error::last_os_error()
            )));
        }

        const LOCK_WAIT_MS: u32 = 30_000;

        // The rendezvous hook is compiled only for tests.  In product builds
        // (Debug or Release) this call site disappears entirely.
        #[cfg(test)]
        let acquired = cas_test_hook::probe_and_signal(mutex, LOCK_WAIT_MS)?;
        #[cfg(not(test))]
        let acquired: Option<u32> = None;

        let wait = acquired.unwrap_or_else(|| unsafe { WaitForSingleObject(mutex, LOCK_WAIT_MS) });
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe { CloseHandle(mutex) };
            let reason = if wait == WAIT_TIMEOUT {
                "CAS mutex wait timed out".to_string()
            } else if wait == WAIT_FAILED {
                format!("CAS mutex wait failed: {}", std::io::Error::last_os_error())
            } else {
                format!("CAS mutex wait returned unexpected status: {wait}")
            };
            return Err(CoreError::NoteCasLockUnavailable(reason));
        }
        struct MutexGuard(HANDLE);
        impl Drop for MutexGuard {
            fn drop(&mut self) {
                unsafe {
                    ReleaseMutex(self.0);
                    CloseHandle(self.0);
                }
            }
        }
        let _guard = MutexGuard(mutex);
        operation()
    }

    #[cfg(test)]
    mod cas_test_hook {
        use super::*;
        use windows_sys::Win32::Foundation::{
            CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
        use windows_sys::Win32::System::Threading::{
            EVENT_MODIFY_STATE, OpenEventW, ReleaseMutex, SetEvent, WaitForSingleObject,
        };

        struct TestHookEvents {
            lock_held: HANDLE,
            waiter_attempted: HANDLE,
            commit_barrier: HANDLE,
        }

        impl Drop for TestHookEvents {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.lock_held);
                    CloseHandle(self.waiter_attempted);
                    CloseHandle(self.commit_barrier);
                }
            }
        }

        /// Rendezvous hook used only by the M1 independent-process CAS tests.
        /// Returns `Some(probe_result)` if this process is the holder that
        /// acquired the mutex during the zero-time probe, or `None` if it is the
        /// waiter.  When no hook variables are present it returns `Ok(None)` so
        /// ordinary unit tests pay no overhead.
        pub(super) fn probe_and_signal(mutex: HANDLE, lock_wait_ms: u32) -> Result<Option<u32>> {
            let hook_marker = std::env::var("CISTELLA_M1_CAS_CHILD_PROCESS").ok();
            let hook_token = std::env::var("CISTELLA_M1_CAS_TEST_TOKEN").ok();
            let hook_requested = hook_marker.as_deref() == Some("1");
            let hook_enabled = hook_requested
                && std::env::var("CISTELLA_M1_CAS_TEST_HOOK").ok().as_deref() == Some("1")
                && hook_token
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .is_some();
            if hook_requested && !hook_enabled {
                return Err(CoreError::NoteCasLockUnavailable(
                    "invalid M1 CAS test-only activation token".to_string(),
                ));
            }
            let hook_events = if hook_enabled {
                let open = |variable: &str| -> Result<HANDLE> {
                    let event_name = std::env::var(variable).map_err(|_| {
                        CoreError::NoteCasLockUnavailable(format!(
                            "missing CAS test hook variable {variable}"
                        ))
                    })?;
                    let wide: Vec<u16> = event_name
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    let handle =
                        unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, 0, wide.as_ptr()) };
                    if handle.is_null() {
                        Err(CoreError::NoteCasLockUnavailable(format!(
                            "failed to open CAS test hook event {variable}: {}",
                            std::io::Error::last_os_error()
                        )))
                    } else {
                        Ok(handle)
                    }
                };
                Some(TestHookEvents {
                    lock_held: open("CISTELLA_M1_CAS_HOOK_LOCK_HELD")?,
                    waiter_attempted: open("CISTELLA_M1_CAS_HOOK_WAITER_ATTEMPTED")?,
                    commit_barrier: open("CISTELLA_M1_CAS_HOOK_COMMIT_BARRIER")?,
                })
            } else {
                None
            };

            if let Some(events) = hook_events.as_ref() {
                // A zero-time probe deterministically identifies the holder
                // without sleeping or relying on process scheduling; the other
                // process reports that it attempted the same mutex before
                // entering the bounded wait.
                let probe = unsafe { WaitForSingleObject(mutex, 0) };
                if probe == WAIT_OBJECT_0 || probe == WAIT_ABANDONED {
                    if unsafe { SetEvent(events.lock_held) } == 0 {
                        unsafe { CloseHandle(mutex) };
                        return Err(CoreError::NoteCasLockUnavailable(
                            "failed to signal CAS test lock-held event".to_string(),
                        ));
                    }
                    let barrier =
                        unsafe { WaitForSingleObject(events.commit_barrier, lock_wait_ms) };
                    if barrier != WAIT_OBJECT_0 {
                        unsafe {
                            ReleaseMutex(mutex);
                            CloseHandle(mutex);
                        }
                        return Err(CoreError::NoteCasLockUnavailable(
                            "CAS test commit barrier was not released".to_string(),
                        ));
                    }
                    Ok(Some(probe))
                } else if probe == WAIT_TIMEOUT {
                    if unsafe { SetEvent(events.waiter_attempted) } == 0 {
                        unsafe { CloseHandle(mutex) };
                        return Err(CoreError::NoteCasLockUnavailable(
                            "failed to signal CAS test waiter event".to_string(),
                        ));
                    }
                    Ok(None)
                } else {
                    unsafe { CloseHandle(mutex) };
                    Err(CoreError::NoteCasLockUnavailable(format!(
                        "CAS test mutex probe failed: {probe}"
                    )))
                }
            } else {
                Ok(None)
            }
        }
    }

    pub(super) fn delete(
        root: &SecureVaultRoot,
        leaf: &str,
        name: &str,
        kind: &str,
        id: Uuid,
    ) -> Result<()> {
        let dir = dirs(root, leaf)?;
        let f = match open_relative(&dir, name, false, FILE_OPEN, true) {
            Ok(f) => f,
            Err(CoreError::Io(_)) => return Err(not_found(kind, id)),
            Err(error) => return Err(error),
        };
        mark_delete(&f)
    }
    pub(super) fn list(
        root: &SecureVaultRoot,
        leaf: &str,
        extension: &str,
        _: &str,
    ) -> Result<Vec<Uuid>> {
        use windows_sys::Wdk::Storage::FileSystem::{
            FileDirectoryInformation, NtQueryDirectoryFile,
        };
        let dir = dirs(root, leaf)?;
        let mut buffer = vec![0u8; 65536];
        let mut restart = true;
        let mut ids = Vec::new();
        loop {
            let mut ios = IO_STATUS_BLOCK::default();
            let status = unsafe {
                NtQueryDirectoryFile(
                    dir.as_raw_handle() as HANDLE,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null(),
                    &mut ios,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
                    FileDirectoryInformation,
                    false,
                    std::ptr::null(),
                    restart,
                )
            };
            restart = false;
            if status < 0 {
                const STATUS_NO_MORE_FILES: i32 = 0x8000_0006u32 as i32;
                if status == STATUS_NO_MORE_FILES {
                    break;
                }
                return Err(CoreError::UnsafeUserRecordPath);
            }
            let returned_len = ios.Information.min(buffer.len());
            for name in parse_directory_information(&buffer, returned_len)? {
                if let Some(stem) = name.strip_suffix(&format!(".{extension}")) {
                    ids.push(Uuid::parse_str(stem).map_err(|_| CoreError::UnsafeUserRecordPath)?);
                }
            }
        }
        for id in &ids {
            let _ = open_relative(&dir, &file_name(*id, extension), false, FILE_OPEN, false)?;
        }
        ids.sort();
        Ok(ids)
    }

    /// Parse the variable-length records returned by NtQueryDirectoryFile.
    ///
    /// The native buffer is untrusted input at this boundary: the kernel call
    /// reports the valid byte count in IO_STATUS_BLOCK.Information, and every
    /// field used below is read only after the containing record and field
    /// ranges have been checked.  Keeping this parser byte-based means there is
    /// no unchecked `from_raw_parts` or typed dereference of a variable-length
    /// record.
    fn parse_directory_information(buffer: &[u8], information: usize) -> Result<Vec<String>> {
        use windows_sys::Wdk::Storage::FileSystem::FILE_DIRECTORY_INFORMATION;

        let returned_len = information.min(buffer.len());
        let file_name_offset = std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
        let minimum_record_header = file_name_offset;
        let mut offset = 0usize;
        let mut names = Vec::new();

        while offset < returned_len {
            let remaining = returned_len
                .checked_sub(offset)
                .ok_or(CoreError::UnsafeUserRecordPath)?;
            if remaining < minimum_record_header {
                return Err(CoreError::UnsafeUserRecordPath);
            }

            let next = read_u32(buffer, offset)?;
            let file_name_length = read_u32(
                buffer,
                offset
                    .checked_add(std::mem::offset_of!(
                        FILE_DIRECTORY_INFORMATION,
                        FileNameLength
                    ))
                    .ok_or(CoreError::UnsafeUserRecordPath)?,
            )? as usize;
            let record_len = if next == 0 {
                remaining
            } else {
                let next = next as usize;
                if next < minimum_record_header {
                    return Err(CoreError::UnsafeUserRecordPath);
                }
                let end = offset
                    .checked_add(next)
                    .ok_or(CoreError::UnsafeUserRecordPath)?;
                if end > returned_len {
                    return Err(CoreError::UnsafeUserRecordPath);
                }
                next
            };
            if record_len < minimum_record_header {
                return Err(CoreError::UnsafeUserRecordPath);
            }
            if file_name_length % 2 != 0 || file_name_length > record_len - file_name_offset {
                return Err(CoreError::UnsafeUserRecordPath);
            }
            let name_start = offset
                .checked_add(file_name_offset)
                .ok_or(CoreError::UnsafeUserRecordPath)?;
            let name_end = name_start
                .checked_add(file_name_length)
                .ok_or(CoreError::UnsafeUserRecordPath)?;
            let record_end = offset
                .checked_add(record_len)
                .ok_or(CoreError::UnsafeUserRecordPath)?;
            if name_end > record_end || name_end > returned_len {
                return Err(CoreError::UnsafeUserRecordPath);
            }
            let units = buffer[name_start..name_end]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
            names.push(
                String::from_utf16(units.collect::<Vec<_>>().as_slice())
                    .map_err(|_| CoreError::UnsafeUserRecordPath)?,
            );

            if next == 0 {
                offset = record_end;
                break;
            }
            offset = record_end;
        }
        if offset != returned_len && returned_len != 0 {
            return Err(CoreError::UnsafeUserRecordPath);
        }
        Ok(names)
    }

    fn read_u32(buffer: &[u8], offset: usize) -> Result<u32> {
        let end = offset
            .checked_add(std::mem::size_of::<u32>())
            .ok_or(CoreError::UnsafeUserRecordPath)?;
        let bytes = buffer
            .get(offset..end)
            .ok_or(CoreError::UnsafeUserRecordPath)?;
        Ok(u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| CoreError::UnsafeUserRecordPath)?,
        ))
    }

    #[cfg(test)]
    mod directory_parser_tests {
        use super::parse_directory_information;
        use windows_sys::Wdk::Storage::FileSystem::FILE_DIRECTORY_INFORMATION;

        fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
            buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn record(name: &str, next: u32, record_len: usize) -> Vec<u8> {
            let name_bytes: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let file_name_offset = std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
            let file_name_length_offset =
                std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileNameLength);
            let mut buffer = vec![0u8; record_len];
            put_u32(&mut buffer, 0, next);
            put_u32(
                &mut buffer,
                file_name_length_offset,
                name_bytes.len() as u32,
            );
            buffer[file_name_offset..file_name_offset + name_bytes.len()]
                .copy_from_slice(&name_bytes);
            buffer
        }

        #[test]
        fn parser_accepts_valid_record_and_clamps_to_information() {
            let record = record("valid.md", 0, 128);
            let mut returned = record.clone();
            returned.extend_from_slice(&[0xA5; 64]);
            let names = parse_directory_information(&returned, record.len()).unwrap();
            assert_eq!(names, vec!["valid.md"]);
        }

        #[test]
        fn parser_rejects_truncated_header_and_short_information() {
            assert!(parse_directory_information(&[0u8; 16], 16).is_err());
            let record = record("valid.md", 0, 128);
            assert!(parse_directory_information(&record, 32).is_err());
        }

        #[test]
        fn parser_rejects_invalid_next_entry_boundaries() {
            let file_name_offset = std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
            let mut too_small = record("valid.md", (file_name_offset - 1) as u32, 128);
            assert!(parse_directory_information(&too_small, too_small.len()).is_err());
            put_u32(&mut too_small, 0, 256);
            assert!(parse_directory_information(&too_small, too_small.len()).is_err());
        }

        #[test]
        fn parser_rejects_invalid_filename_lengths() {
            let file_name_offset = std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
            let file_name_length_offset =
                std::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileNameLength);
            let mut odd = record("valid.md", 0, 128);
            put_u32(&mut odd, file_name_length_offset, 3);
            assert!(parse_directory_information(&odd, odd.len()).is_err());

            let mut beyond_record = record("", 0, file_name_offset + 2);
            put_u32(&mut beyond_record, file_name_length_offset, 4);
            assert!(parse_directory_information(&beyond_record, beyond_record.len()).is_err());
        }
    }
}

#[cfg(all(windows, test))]
mod m1_cas_tests {
    use crate::{CoreError, NOTES_RELATIVE_DIR, NoteDraft, Vault, VaultOpenOptions};
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Child, Command, ExitStatus},
        time::{Duration, Instant},
    };
    use uuid::Uuid;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0},
        Storage::FileSystem::SYNCHRONIZE,
        System::Threading::{
            CreateEventW, EVENT_MODIFY_STATE, OpenEventW, SetEvent, WaitForSingleObject,
        },
    };

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temporary directory");
        path
    }

    fn write_vault(root: &Path) -> Vault {
        fs::write(
            root.join("manifest.json"),
            r#"{
  "format_version": "0.1.0",
  "vault_id": "work-order-11-m1",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": { "name": "test", "entity": "sources", "snapshot_date": null, "input_path": "fixture" },
  "tables": {}
}"#,
        )
        .expect("write manifest");
        Vault::open(root, VaultOpenOptions::default()).expect("open minimum Vault")
    }

    struct RendezvousEvent(HANDLE);

    impl Drop for RendezvousEvent {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn rendezvous_event_name(run_id: Uuid, label: &str) -> String {
        format!("Local\\cistella-m1-rendezvous-{run_id}-{label}")
    }

    fn wide_name(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn create_rendezvous_event(name: &str) -> RendezvousEvent {
        let wide = wide_name(name);
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        assert!(
            !handle.is_null(),
            "create rendezvous event {name:?}: error {}",
            unsafe { GetLastError() }
        );
        RendezvousEvent(handle)
    }

    fn open_rendezvous_event(name: &str) -> RendezvousEvent {
        let wide = wide_name(name);
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, 0, wide.as_ptr()) };
        assert!(
            !handle.is_null(),
            "open rendezvous event {name:?}: error {}",
            unsafe { GetLastError() }
        );
        RendezvousEvent(handle)
    }

    fn signal_rendezvous(event: &RendezvousEvent, label: &str) {
        assert_ne!(unsafe { SetEvent(event.0) }, 0, "signal rendezvous {label}");
    }

    fn wait_rendezvous(event: &RendezvousEvent, label: &str) {
        const RENDEZVOUS_TIMEOUT_MS: u32 = 30_000;
        let status = unsafe { WaitForSingleObject(event.0, RENDEZVOUS_TIMEOUT_MS) };
        assert_eq!(
            status, WAIT_OBJECT_0,
            "wait for rendezvous {label} returned {status}"
        );
    }

    struct ProcessCasCleanup {
        root: PathBuf,
        ipc: PathBuf,
        children: Vec<Child>,
    }

    impl ProcessCasCleanup {
        fn new(root: PathBuf, ipc: PathBuf) -> Self {
            Self {
                root,
                ipc,
                children: Vec::new(),
            }
        }

        fn push(&mut self, child: Child) {
            self.children.push(child);
        }

        /// Wait for every child without ever blocking the parent indefinitely.
        /// A timeout or polling error first terminates and reaps all children.
        fn wait_all(&mut self, timeout: Duration) -> std::io::Result<Vec<ExitStatus>> {
            let deadline = Instant::now() + timeout;
            let mut statuses: Vec<Option<ExitStatus>> = vec![None; self.children.len()];
            loop {
                let mut all_done = true;
                for (index, child) in self.children.iter_mut().enumerate() {
                    if statuses[index].is_some() {
                        continue;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => statuses[index] = Some(status),
                        Ok(None) => all_done = false,
                        Err(error) => {
                            self.terminate_and_reap()?;
                            return Err(error);
                        }
                    }
                }
                if all_done {
                    return Ok(statuses
                        .into_iter()
                        .map(|status| status.expect("completed child has an exit status"))
                        .collect());
                }
                if Instant::now() >= deadline {
                    self.terminate_and_reap()?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out waiting for CAS child processes",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// Terminate every child and poll until all are reaped.  Returns Ok only
        /// after every handle has returned `Ok(Some(_))`.
        fn terminate_and_reap(&mut self) -> std::io::Result<()> {
            const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);
            let deadline = Instant::now() + TOTAL_TIMEOUT;

            for child in &mut self.children {
                let _ = child.kill();
            }

            loop {
                let mut all_done = true;
                for child in &mut self.children {
                    match child.try_wait() {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => all_done = false,
                    }
                }
                if all_done {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "failed to reap all CAS child processes within bounded timeout",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn all_reaped(&mut self) -> std::io::Result<bool> {
            let mut all_done = true;
            for child in &mut self.children {
                match child.try_wait() {
                    Ok(Some(_)) => {}
                    Ok(None) => all_done = false,
                    Err(error) => return Err(error),
                }
            }
            Ok(all_done)
        }
    }

    impl Drop for ProcessCasCleanup {
        fn drop(&mut self) {
            // Only delete the temporary directories after we have proven that
            // every child process has exited.  This prevents cleaning up while a
            // child still holds handles or files inside root/ipc.
            if self.terminate_and_reap().is_ok() {
                let _ = fs::remove_dir_all(&self.ipc);
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }

    #[test]
    fn m1_note_revision_cas_child_process() {
        let Ok(root) = std::env::var("CISTELLA_M1_CAS_CHILD_ROOT") else {
            return;
        };
        let note_id = Uuid::parse_str(
            &std::env::var("CISTELLA_M1_CAS_CHILD_NOTE_ID").expect("child note id"),
        )
        .expect("valid child note id");
        let revision = std::env::var("CISTELLA_M1_CAS_CHILD_REVISION").expect("child revision");
        let result_path = PathBuf::from(
            std::env::var("CISTELLA_M1_CAS_CHILD_RESULT").expect("child result path"),
        );
        let title = std::env::var("CISTELLA_M1_CAS_CHILD_TITLE").expect("child title");
        let body = std::env::var("CISTELLA_M1_CAS_CHILD_BODY").expect("child body");
        let ready_name = std::env::var("CISTELLA_M1_CAS_CHILD_READY").expect("child ready event");
        let start_name = std::env::var("CISTELLA_M1_CAS_CHILD_START").expect("child start event");

        let vault = Vault::open(root, VaultOpenOptions::default()).expect("open child Vault");
        let ready = open_rendezvous_event(&ready_name);
        let start = open_rendezvous_event(&start_name);
        signal_rendezvous(&ready, "child ready");
        wait_rendezvous(&start, "shared start");

        let result = vault.update_note(note_id, &revision, title, body);
        let marker = match result {
            Ok(_) => "ok".to_string(),
            Err(CoreError::NoteConflict { .. }) => "conflict".to_string(),
            Err(error) => panic!("child CAS failed with unexpected error: {error:?}"),
        };
        if std::env::var("CISTELLA_M1_CAS_CHILD_HANG_AFTER_BARRIER")
            .ok()
            .as_deref()
            == Some("1")
        {
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        fs::write(result_path, marker).expect("write child CAS result outside Vault");
    }

    #[test]
    fn m1_note_revision_cas_is_atomic_across_independent_processes() {
        let root = temp_dir("cistella-work-order-11-m1-process-cas");
        let ipc = temp_dir("cistella-work-order-11-m1-process-ipc");
        let result_a = ipc.join("child-a.result");
        let result_b = ipc.join("child-b.result");
        let mut cleanup = ProcessCasCleanup::new(root.clone(), ipc.clone());

        let result = (|| {
            let vault = write_vault(&root);
            let note = vault
                .create_note(NoteDraft {
                    item_id: Uuid::new_v4(),
                    title: "before process CAS".to_string(),
                    markdown_body: "before".to_string(),
                })
                .expect("create process CAS note");
            let run_id = Uuid::new_v4();
            let start_name = rendezvous_event_name(run_id, "start");
            let ready_a_name = rendezvous_event_name(run_id, "ready-a");
            let ready_b_name = rendezvous_event_name(run_id, "ready-b");
            let lock_held_name = rendezvous_event_name(run_id, "lock-held");
            let waiter_attempted_name = rendezvous_event_name(run_id, "waiter-attempted");
            let commit_barrier_name = rendezvous_event_name(run_id, "commit-barrier");
            let _start = create_rendezvous_event(&start_name);
            let ready_a = create_rendezvous_event(&ready_a_name);
            let ready_b = create_rendezvous_event(&ready_b_name);
            let lock_held = create_rendezvous_event(&lock_held_name);
            let waiter_attempted = create_rendezvous_event(&waiter_attempted_name);
            let commit_barrier = create_rendezvous_event(&commit_barrier_name);

            let exe = std::env::current_exe().expect("current test executable");
            let common = [
                (
                    "CISTELLA_M1_CAS_CHILD_ROOT",
                    root.to_string_lossy().into_owned(),
                ),
                ("CISTELLA_M1_CAS_CHILD_NOTE_ID", note.note_id.to_string()),
                ("CISTELLA_M1_CAS_CHILD_REVISION", note.revision.clone()),
                ("CISTELLA_M1_CAS_CHILD_START", start_name.clone()),
                ("CISTELLA_M1_CAS_CHILD_PROCESS", "1".to_string()),
                ("CISTELLA_M1_CAS_TEST_HOOK", "1".to_string()),
                ("CISTELLA_M1_CAS_TEST_TOKEN", run_id.to_string()),
                ("CISTELLA_M1_CAS_HOOK_LOCK_HELD", lock_held_name.clone()),
                (
                    "CISTELLA_M1_CAS_HOOK_WAITER_ATTEMPTED",
                    waiter_attempted_name.clone(),
                ),
                (
                    "CISTELLA_M1_CAS_HOOK_COMMIT_BARRIER",
                    commit_barrier_name.clone(),
                ),
            ];
            let mut command_a = Command::new(&exe);
            command_a
                .args([
                    "secure_user_records::m1_cas_tests::m1_note_revision_cas_child_process",
                    "--exact",
                    "--nocapture",
                ])
                .envs(common.iter().map(|(key, value)| (*key, value)))
                .env("CISTELLA_M1_CAS_CHILD_READY", &ready_a_name)
                .env("CISTELLA_M1_CAS_CHILD_RESULT", &result_a)
                .env("CISTELLA_M1_CAS_CHILD_TITLE", "process writer A")
                .env("CISTELLA_M1_CAS_CHILD_BODY", "process body A");
            let mut command_b = Command::new(&exe);
            command_b
                .args([
                    "secure_user_records::m1_cas_tests::m1_note_revision_cas_child_process",
                    "--exact",
                    "--nocapture",
                ])
                .envs(common.iter().map(|(key, value)| (*key, value)))
                .env("CISTELLA_M1_CAS_CHILD_READY", &ready_b_name)
                .env("CISTELLA_M1_CAS_CHILD_RESULT", &result_b)
                .env("CISTELLA_M1_CAS_CHILD_TITLE", "process writer B")
                .env("CISTELLA_M1_CAS_CHILD_BODY", "process body B");
            cleanup.push(command_a.spawn().expect("spawn CAS child A"));
            cleanup.push(command_b.spawn().expect("spawn CAS child B"));

            wait_rendezvous(&ready_a, "child A ready");
            wait_rendezvous(&ready_b, "child B ready");
            signal_rendezvous(&_start, "shared start");

            wait_rendezvous(&lock_held, "CAS holder owns lock");
            wait_rendezvous(&waiter_attempted, "CAS waiter attempted lock");
            signal_rendezvous(&commit_barrier, "release holder commit barrier");

            let statuses = cleanup
                .wait_all(Duration::from_secs(5))
                .expect("wait CAS children");
            assert!(
                statuses.iter().all(ExitStatus::success),
                "CAS child exited unsuccessfully: {statuses:?}"
            );
            let marker_a = fs::read_to_string(&result_a).expect("read child A marker");
            let marker_b = fs::read_to_string(&result_b).expect("read child B marker");
            assert!(
                (marker_a == "ok" && marker_b == "conflict")
                    || (marker_a == "conflict" && marker_b == "ok"),
                "independent processes must have exactly one CAS winner: {marker_a:?}, {marker_b:?}"
            );
            let final_note = vault
                .get_note(note.note_id)
                .expect("read final process CAS note");
            assert!(
                (final_note.title == "process writer A" && marker_a == "ok")
                    || (final_note.title == "process writer B" && marker_b == "ok"),
                "final authority record must be the winning process"
            );
            let names: Vec<_> = fs::read_dir(root.join(NOTES_RELATIVE_DIR))
                .expect("read notes directory")
                .map(|entry| {
                    entry
                        .expect("read note entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(names, vec![format!("{}.md", note.note_id)]);
            Ok::<(), String>(())
        })();
        result.expect("Note CAS must serialize without temporary or IPC artifacts");
    }

    #[test]
    fn m1_process_cas_timeout_kills_reaps_and_cleans_up() {
        let root = temp_dir("cistella-work-order-11-m1-cas-timeout");
        let ipc = temp_dir("cistella-work-order-11-m1-cas-timeout-ipc");
        let result_a = ipc.join("child-a.result");
        let result_b = ipc.join("child-b.result");
        let mut cleanup = ProcessCasCleanup::new(root.clone(), ipc.clone());

        let result = (|| {
            let vault = write_vault(&root);
            let note = vault
                .create_note(NoteDraft {
                    item_id: Uuid::new_v4(),
                    title: "before timeout".to_string(),
                    markdown_body: "before".to_string(),
                })
                .expect("create timeout note");
            let run_id = Uuid::new_v4();
            let start_name = rendezvous_event_name(run_id, "timeout-start");
            let ready_a_name = rendezvous_event_name(run_id, "timeout-ready-a");
            let ready_b_name = rendezvous_event_name(run_id, "timeout-ready-b");
            let lock_held_name = rendezvous_event_name(run_id, "timeout-lock-held");
            let waiter_attempted_name = rendezvous_event_name(run_id, "timeout-waiter-attempted");
            let commit_barrier_name = rendezvous_event_name(run_id, "timeout-commit-barrier");
            let _start = create_rendezvous_event(&start_name);
            let ready_a = create_rendezvous_event(&ready_a_name);
            let ready_b = create_rendezvous_event(&ready_b_name);
            let lock_held = create_rendezvous_event(&lock_held_name);
            let waiter_attempted = create_rendezvous_event(&waiter_attempted_name);
            let commit_barrier = create_rendezvous_event(&commit_barrier_name);

            let exe = std::env::current_exe().expect("current test executable");
            let common = [
                (
                    "CISTELLA_M1_CAS_CHILD_ROOT",
                    root.to_string_lossy().into_owned(),
                ),
                ("CISTELLA_M1_CAS_CHILD_NOTE_ID", note.note_id.to_string()),
                ("CISTELLA_M1_CAS_CHILD_REVISION", note.revision.clone()),
                ("CISTELLA_M1_CAS_CHILD_START", start_name.clone()),
                ("CISTELLA_M1_CAS_CHILD_PROCESS", "1".to_string()),
                ("CISTELLA_M1_CAS_TEST_HOOK", "1".to_string()),
                ("CISTELLA_M1_CAS_TEST_TOKEN", run_id.to_string()),
                ("CISTELLA_M1_CAS_HOOK_LOCK_HELD", lock_held_name.clone()),
                (
                    "CISTELLA_M1_CAS_HOOK_WAITER_ATTEMPTED",
                    waiter_attempted_name.clone(),
                ),
                (
                    "CISTELLA_M1_CAS_HOOK_COMMIT_BARRIER",
                    commit_barrier_name.clone(),
                ),
            ];
            let mut command_a = Command::new(&exe);
            command_a
                .args([
                    "secure_user_records::m1_cas_tests::m1_note_revision_cas_child_process",
                    "--exact",
                    "--nocapture",
                ])
                .envs(common.iter().map(|(key, value)| (*key, value)))
                .env("CISTELLA_M1_CAS_CHILD_READY", &ready_a_name)
                .env("CISTELLA_M1_CAS_CHILD_RESULT", &result_a)
                .env("CISTELLA_M1_CAS_CHILD_TITLE", "timeout writer A")
                .env("CISTELLA_M1_CAS_CHILD_BODY", "timeout body A")
                .env("CISTELLA_M1_CAS_CHILD_HANG_AFTER_BARRIER", "1");
            let mut command_b = Command::new(&exe);
            command_b
                .args([
                    "secure_user_records::m1_cas_tests::m1_note_revision_cas_child_process",
                    "--exact",
                    "--nocapture",
                ])
                .envs(common.iter().map(|(key, value)| (*key, value)))
                .env("CISTELLA_M1_CAS_CHILD_READY", &ready_b_name)
                .env("CISTELLA_M1_CAS_CHILD_RESULT", &result_b)
                .env("CISTELLA_M1_CAS_CHILD_TITLE", "timeout writer B")
                .env("CISTELLA_M1_CAS_CHILD_BODY", "timeout body B");
            cleanup.push(command_a.spawn().expect("spawn hanging CAS child"));
            cleanup.push(command_b.spawn().expect("spawn exiting CAS child"));

            wait_rendezvous(&ready_a, "timeout child A ready");
            wait_rendezvous(&ready_b, "timeout child B ready");
            signal_rendezvous(&_start, "timeout children start");
            wait_rendezvous(&lock_held, "timeout CAS holder owns lock");
            wait_rendezvous(&waiter_attempted, "timeout CAS waiter attempted lock");
            signal_rendezvous(&commit_barrier, "timeout release commit barrier");

            let started = Instant::now();
            let wait_result = cleanup.wait_all(Duration::from_millis(750));
            assert!(
                matches!(&wait_result, Err(error) if error.kind() == std::io::ErrorKind::TimedOut),
                "one hanging child must produce a bounded timeout: {wait_result:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "fault-injection wait must remain short"
            );

            // P1: we must confirm every child has exited before Drop deletes the
            // temporary directories.  terminate_and_reap only returns Ok when all
            // handles have been reaped.
            cleanup
                .terminate_and_reap()
                .expect("all children must be reaped before cleanup");
            assert!(
                cleanup.all_reaped().expect("check child reap status"),
                "both child handles must be confirmed exited before cleanup"
            );
            Ok::<(), String>(())
        })();

        drop(cleanup);
        assert!(
            !root.exists(),
            "temporary Vault must be cleaned after timeout"
        );
        assert!(
            !ipc.exists(),
            "external IPC directory must be cleaned after timeout"
        );
        assert!(!result_a.exists(), "IPC result A must not survive cleanup");
        assert!(!result_b.exists(), "IPC result B must not survive cleanup");
        result.expect("timeout cleanup must complete without panic");
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    // Explicitly fail closed until the rustix/openat implementation is built on
    // a Unix target; never reintroduce a normal Path fallback.
    pub(super) fn read(_: &SecureVaultRoot, _: &str, _: &str, _: &str, _: Uuid) -> Result<Vec<u8>> {
        Err(CoreError::UnsafeUserRecordPath)
    }
    pub(super) fn replace(_: &SecureVaultRoot, _: &str, _: &str, _: &[u8], _: &str) -> Result<()> {
        Err(CoreError::UnsafeUserRecordPath)
    }
    pub(super) fn delete(_: &SecureVaultRoot, _: &str, _: &str, _: &str, _: Uuid) -> Result<()> {
        Err(CoreError::UnsafeUserRecordPath)
    }
    pub(super) fn list(_: &SecureVaultRoot, _: &str, _: &str, _: &str) -> Result<Vec<Uuid>> {
        Err(CoreError::UnsafeUserRecordPath)
    }
    pub(super) fn with_note_cas_lock<T>(
        _: &SecureVaultRoot,
        _: &str,
        _: &str,
        _: Uuid,
        _: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        Err(CoreError::UnsafeUserRecordPath)
    }
}
