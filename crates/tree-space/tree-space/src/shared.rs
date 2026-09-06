//! Cross-process zero-copy shared regions (§12 / M11).
//!
//! A [`SharedRegion`] holds Arrow IPC bytes that are published once and then
//! readable zero-copy by multiple processes. Backends:
//! - in-process `Arc` (universal fallback);
//! - Windows named file mapping (`CreateFileMappingW` / `MapViewOfFile`);
//! - Unix `memfd` (`memfd_create`) mapped read-only.
//!
//! The region stores a `u64` data-length header followed by the bytes, so an
//! opener can borrow the exact payload without knowing the mapping size.

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::slot::TableInner;
use std::sync::Arc;

/// In-process zero-copy snapshot handle (kept for compatibility).
#[derive(Clone)]
pub struct SharedSnapshot {
    inner: Arc<TableInner>,
}
impl SharedSnapshot {
    /// Creates a shared handle without copying Arrow buffers.
    pub fn new(inner: Arc<TableInner>) -> Self {
        Self { inner }
    }
    /// Borrows the immutable snapshot.
    pub fn get(&self) -> &TableInner {
        &self.inner
    }
    /// Clones only the reference count, not Arrow buffers.
    pub fn clone_handle(&self) -> Self {
        self.clone()
    }
}

/// Platform-neutral name used for a named mapping or memfd export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedRegionName(String);
impl SharedRegionName {
    /// Validates a portable region name.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(TreeSpaceError::new(
                ErrorCode::PathInvalid,
                "shared region name is invalid",
            ));
        }
        Ok(Self(value))
    }
    /// Returns the operating-system region name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A handle that lets another process open the same shared bytes.
#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionHandle {
    name: String,
}

/// A handle that lets another process open the same shared bytes.
#[cfg(unix)]
#[derive(Debug)]
pub struct RegionHandle {
    fd: std::os::unix::io::OwnedFd,
    len: usize,
}

#[cfg(unix)]
impl RegionHandle {
    /// Constructs a handle from a descriptor and byte length (e.g. after `SCM_RIGHTS`).
    pub fn from_fd(fd: std::os::unix::io::OwnedFd, len: usize) -> Self {
        Self { fd, len }
    }
    /// Returns the raw descriptor for `SCM_RIGHTS` transport.
    pub fn fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.fd.as_raw_fd()
    }
    /// Returns the shared byte length.
    pub fn len(&self) -> usize {
        self.len
    }
}

/// Arrow IPC bytes shared across processes with zero-copy reads.
#[derive(Clone)]
pub struct SharedRegion {
    backing: Arc<SharedBacking>,
}

enum SharedBacking {
    #[cfg(not(any(windows, unix)))]
    Heap(Arc<[u8]>),
    #[cfg(windows)]
    Named(Arc<platform::WindowsMapping>),
    #[cfg(unix)]
    Memfd(Arc<platform::MemfdRegion>),
}

impl SharedRegion {
    /// Publishes Arrow IPC bytes, copying them once into a shared region.
    pub fn publish(bytes: Vec<u8>) -> Result<Self> {
        #[cfg(windows)]
        {
            let name = format!("tree-space-shm-{}-{}", std::process::id(), unique_counter());
            let mapping = platform::WindowsMapping::create(&name, &bytes)?;
            return Ok(Self {
                backing: Arc::new(SharedBacking::Named(Arc::new(mapping))),
            });
        }
        #[cfg(unix)]
        {
            let region = platform::MemfdRegion::create(&bytes)?;
            return Ok(Self {
                backing: Arc::new(SharedBacking::Memfd(Arc::new(region))),
            });
        }
        #[cfg(not(any(windows, unix)))]
        {
            Ok(Self {
                backing: Arc::new(SharedBacking::Heap(bytes.into())),
            })
        }
    }

    /// Borrows the shared Arrow IPC bytes without copying.
    pub fn bytes(&self) -> &[u8] {
        match &*self.backing {
            #[cfg(not(any(windows, unix)))]
            SharedBacking::Heap(bytes) => bytes,
            #[cfg(windows)]
            SharedBacking::Named(mapping) => mapping.as_slice(),
            #[cfg(unix)]
            SharedBacking::Memfd(region) => region.as_slice(),
        }
    }

    /// Exports a handle that another process can [`open`](Self::open).
    ///
    /// A process-local `Heap` region cannot be exported; named/memfd regions can.
    pub fn export(&self) -> Result<RegionHandle> {
        match &*self.backing {
            #[cfg(windows)]
            SharedBacking::Named(mapping) => Ok(RegionHandle {
                name: mapping.name.clone(),
            }),
            #[cfg(unix)]
            SharedBacking::Memfd(region) => Ok(RegionHandle {
                fd: region.export_fd(),
                len: region.len(),
            }),
            #[cfg(not(any(windows, unix)))]
            SharedBacking::Heap(_) => Err(TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "a process-local heap region cannot be exported across processes",
            )),
        }
    }

    /// Opens a shared region by handle with zero-copy reading.
    pub fn open(handle: RegionHandle) -> Result<Self> {
        #[cfg(windows)]
        {
            let mapping = platform::WindowsMapping::open(&handle.name)?;
            Ok(Self {
                backing: Arc::new(SharedBacking::Named(Arc::new(mapping))),
            })
        }
        #[cfg(unix)]
        {
            let region = unsafe { platform::MemfdRegion::open(handle.fd, handle.len) }?;
            Ok(Self {
                backing: Arc::new(SharedBacking::Memfd(Arc::new(region))),
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = handle;
            Err(TreeSpaceError::new(
                ErrorCode::PayloadMalformed,
                "cross-process sharing is unavailable on this platform",
            ))
        }
    }
}

fn unique_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A custodian holding a shared region alive (§12 supplied-chain semantics).
///
/// The active custodian keeps the region's backing reachable; when it is
/// dropped, the next reader in the [`CustodianChain`] takes over. With no
/// holders the kernel reclaims the region (Windows named section / Unix memfd).
#[derive(Clone)]
pub struct SharedCustodian {
    region: SharedRegion,
}

impl SharedCustodian {
    /// Assumes custody of a region.
    pub fn new(region: SharedRegion) -> Self {
        Self { region }
    }
    /// Borrows the shared bytes.
    pub fn bytes(&self) -> &[u8] {
        self.region.bytes()
    }
    /// Returns a clone of the region (does not copy bytes).
    pub fn region(&self) -> SharedRegion {
        self.region.clone()
    }
}

/// A reader-order custodian chain (§12: oldest reader takes over on holder death).
#[derive(Default)]
pub struct CustodianChain {
    holders: std::sync::Mutex<std::collections::VecDeque<SharedCustodian>>,
}

impl CustodianChain {
    /// Creates an empty chain.
    pub fn new() -> Self {
        Self::default()
    }
    /// Appends a reader to the end of the chain (read order).
    pub fn add_reader(&self, region: SharedRegion) {
        self.holders
            .lock()
            .expect("custodian chain poisoned")
            .push_back(SharedCustodian::new(region));
    }
    /// Promotes the oldest reader to custodian, if any (takeover on holder death).
    pub fn take_over(&self) -> Option<SharedCustodian> {
        self.holders
            .lock()
            .expect("custodian chain poisoned")
            .pop_front()
    }
    /// Number of readers awaiting potential takeover.
    pub fn pending_readers(&self) -> usize {
        self.holders.lock().expect("custodian chain poisoned").len()
    }
}

/// Unix SCM_RIGHTS fd-passing transport for memfd custody (§12).
///
/// This sends a duplicated descriptor over a Unix socket so the receiver can
/// mmap the same physical pages. It is Unix-only and compiled under `cfg(unix)`;
/// the Windows equivalent uses named file mappings opened by name.
#[cfg(unix)]
pub mod scm {
    use std::io;
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;

    /// A control-message buffer aligned for `cmsghdr` access.
    #[repr(align(8))]
    struct Control([u8; 64]);

    fn cmsg_len() -> libc::socklen_t {
        unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as libc::c_uint) }
    }

    /// Sends `fd` over `stream` using `SCM_RIGHTS`.
    pub fn send_fd(stream: &UnixStream, fd: RawFd) -> io::Result<()> {
        let fds = [fd];
        let mut control = Control([0_u8; 64]);
        let mut iov = std::io::IoSlice::new(&[1_u8]);
        unsafe {
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = (&mut iov) as *mut _ as *mut libc::iovec;
            message.msg_iovlen = 1;
            message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
            message.msg_controllen = control.0.len();
            let cmsg = libc::CMSG_FIRSTHDR(&message);
            if cmsg.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "no control message space",
                ));
            }
            (*cmsg).cmsg_len = cmsg_len() as usize;
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(cmsg) as *mut libc::c_int,
                1,
            );
            message.msg_controllen = (*cmsg).cmsg_len as usize;
            if libc::sendmsg(stream.as_raw_fd(), &message, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Receives a descriptor sent via [`send_fd`].
    pub fn recv_fd(stream: &UnixStream) -> io::Result<RawFd> {
        let mut received: RawFd = -1;
        let mut control = Control([0_u8; 64]);
        let mut byte = 0_u8;
        let mut iov = std::io::IoSliceMut::new(std::slice::from_mut(&mut byte));
        unsafe {
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = (&mut iov) as *mut _ as *mut libc::iovec;
            message.msg_iovlen = 1;
            message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
            message.msg_controllen = control.0.len();
            if libc::recvmsg(stream.as_raw_fd(), &mut message, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            let cmsg = libc::CMSG_FIRSTHDR(&message);
            if !cmsg.is_null()
                && (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SCM_RIGHTS
            {
                std::ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(cmsg) as *const libc::c_int,
                    &mut received,
                    1,
                );
            }
        }
        if received < 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "no descriptor received",
            ));
        }
        Ok(received)
    }
}

#[cfg(any(windows, unix))]
mod platform {
    #![allow(unsafe_code)]

    #[cfg(windows)]
    use super::io_error;
    #[cfg(unix)]
    use super::io_error_io;

    #[cfg(windows)]
    pub struct WindowsMapping {
        handle: windows_sys::Win32::Foundation::HANDLE,
        view: *mut u8,
        size: usize,
        pub name: String,
    }

    #[cfg(windows)]
    unsafe impl Send for WindowsMapping {}
    #[cfg(windows)]
    unsafe impl Sync for WindowsMapping {}

    #[cfg(windows)]
    impl WindowsMapping {
        pub fn create(name: &str, data: &[u8]) -> Result<Self, crate::error::TreeSpaceError> {
            use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Memory::{
                CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
                PAGE_READWRITE,
            };
            let total = 8 + data.len();
            let name_u16: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let handle: HANDLE = unsafe {
                CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    (total >> 32) as u32,
                    total as u32,
                    name_u16.as_ptr(),
                )
            };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(io_error("create named mapping"));
            }
            let view: MEMORY_MAPPED_VIEW_ADDRESS =
                unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, total) };
            if view.Value.is_null() {
                return Err(io_error("map view of named mapping"));
            }
            let base = view.Value as *mut u8;
            unsafe {
                std::ptr::copy_nonoverlapping(&(data.len() as u64).to_le_bytes()[0], base, 8);
                std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(8), data.len());
            }
            Ok(Self {
                handle,
                view: base,
                size: total,
                name: name.to_owned(),
            })
        }

        pub fn open(name: &str) -> Result<Self, crate::error::TreeSpaceError> {
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::System::Memory::{
                FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
            };
            let name_u16: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let handle: HANDLE =
                unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name_u16.as_ptr()) };
            if handle.is_null() {
                return Err(io_error("open named mapping"));
            }
            let view: MEMORY_MAPPED_VIEW_ADDRESS =
                unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
            if view.Value.is_null() {
                return Err(io_error("map view of opened named mapping"));
            }
            let len = unsafe { (view.Value as *const u64).read_unaligned() } as usize;
            Ok(Self {
                handle,
                view: view.Value as *mut u8,
                size: 8 + len,
                name: name.to_owned(),
            })
        }

        pub fn as_slice(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.view.add(8), self.size - 8) }
        }
    }

    #[cfg(windows)]
    impl Drop for WindowsMapping {
        fn drop(&mut self) {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Memory::UnmapViewOfFile;
            unsafe {
                UnmapViewOfFile(
                    windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: self.view as _,
                    },
                );
                CloseHandle(self.handle);
            }
        }
    }

    #[cfg(unix)]
    pub struct MemfdRegion {
        map: memmap2::Mmap,
        _file: std::fs::File,
        len: usize,
    }

    #[cfg(unix)]
    unsafe impl Send for MemfdRegion {}
    #[cfg(unix)]
    unsafe impl Sync for MemfdRegion {}

    #[cfg(unix)]
    impl MemfdRegion {
        pub fn create(data: &[u8]) -> Result<Self, crate::error::TreeSpaceError> {
            use std::io::Write;
            use std::os::unix::io::AsRawFd;
            let total = 8 + data.len();
            let mfd = memfd::MemfdOptions::new()
                .create("tree-space-shm")
                .map_err(|error| io_error_io(error))?;
            mfd.as_file()
                .set_len(total as u64)
                .and_then(|_| {
                    mfd.as_file()
                        .write_all(&(data.len() as u64).to_le_bytes())?;
                    mfd.as_file().write_all(data)
                })
                .and_then(|_| mfd.as_file().sync_all())
                .map_err(|error| io_error_io(error))?;
            let file = mfd.into_file();
            let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|error| io_error_io(error))?;
            let _ = file.as_raw_fd();
            Ok(Self {
                map,
                _file: file,
                len: total,
            })
        }

        pub unsafe fn open(
            fd: std::os::unix::io::OwnedFd,
            len: usize,
        ) -> Result<Self, crate::error::TreeSpaceError> {
            let file = std::fs::File::from(fd);
            let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|error| io_error_io(error))?;
            Ok(Self {
                map,
                _file: file,
                len,
            })
        }

        pub fn as_slice(&self) -> &[u8] {
            &self.map[8..self.len]
        }

        pub fn len(&self) -> usize {
            self.len
        }

        pub fn export_fd(&self) -> std::os::unix::io::OwnedFd {
            use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
            let dup = unsafe { libc::dup(self._file.as_raw_fd()) };
            if dup < 0 {
                panic!("failed to duplicate memfd for export");
            }
            unsafe { OwnedFd::from_raw_fd(dup) }
        }
    }
}

#[cfg(any(windows, unix))]
fn io_error(message: &str) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
        .with_context("detail", std::io::Error::last_os_error().to_string())
}

#[cfg(unix)]
fn io_error_io(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, "M11 shared region I/O failed")
        .with_context("detail", error.to_string())
}
