//! Read-only, one-shot SM2 transport. No provider activation or freshness inference.
//! Objects are reopened per acquisition; no old mapping is cached across calls.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::hwinfo_sm2::{self, DecodeError, Snapshot, MAX_BUFFER_BYTES};
use std::{
    ffi::c_void,
    fmt,
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

// Minimal Windows x64 ABI declarations avoid adding a dependency or Cargo feature.
// MEMORY_BASIC_INFORMATION is verified against the pinned Windows SDK by compile probe.
type Handle = *mut c_void;
#[repr(C)]
#[derive(Default)]
struct MemoryInfo {
    base: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}
const _: () = {
    assert!(size_of::<MemoryInfo>() == 48);
    assert!(std::mem::offset_of!(MemoryInfo, region_size) == 24);
    assert!(std::mem::offset_of!(MemoryInfo, state) == 32);
    assert!(std::mem::offset_of!(MemoryInfo, protect) == 36);
    assert!(std::mem::offset_of!(MemoryInfo, kind) == 40);
};
#[link(name = "kernel32")]
extern "system" {
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(mapping: Handle, access: u32, high: u32, low: u32, size: usize)
        -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn ReleaseMutex(handle: Handle) -> i32;
    fn VirtualQuery(address: *const c_void, info: *mut MemoryInfo, size: usize) -> usize;
    fn ReadProcessMemory(
        process: Handle,
        source: *const c_void,
        dest: *mut c_void,
        size: usize,
        read: *mut usize,
    ) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetLastError() -> u32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    NotFound { operation: &'static str },
    AccessDenied { operation: &'static str },
    Windows { operation: &'static str, code: u32 },
    Timeout,
    Cancelled,
    Abandoned,
    InvalidRegion,
    MappingTooLarge,
    Allocation,
    InvalidOptions,
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HWiNFO transport: {self:?}")
    }
}
impl std::error::Error for TransportError {}
fn win_error(operation: &'static str) -> TransportError {
    // SAFETY: GetLastError takes no pointers and is read immediately after failure.
    match unsafe { GetLastError() } {
        2 => TransportError::NotFound { operation },
        5 => TransportError::AccessDenied { operation },
        code => TransportError::Windows { operation, code },
    }
}
struct OwnedHandle(Handle);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    } // unique successful Open handle
}
struct View(*mut c_void);
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.0);
        }
    } // unique successful Map view
}
struct Lock<'a> {
    handle: &'a OwnedHandle,
    held: bool,
}
impl Lock<'_> {
    fn release(mut self) -> Result<(), TransportError> {
        // SAFETY: acquired on this thread, borrowed handle outlives guard.
        if unsafe { ReleaseMutex(self.handle.0) } == 0 {
            return Err(win_error("release mutex"));
        }
        self.held = false;
        Ok(())
    }
}
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        if self.held {
            unsafe {
                ReleaseMutex(self.handle.0);
            }
        }
    }
}

/// An owned copy only. Parsing never touches Windows objects or retains their handles.
#[derive(Debug)]
pub struct CopiedSnapshot {
    bytes: Vec<u8>,
}
impl CopiedSnapshot {
    pub fn decode(&self) -> Result<Snapshot<'_>, DecodeError> {
        hwinfo_sm2::decode(&self.bytes)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Configuration only; construction performs no OS access.
pub struct Transport {
    mapping: Vec<u16>,
    mutex: Vec<u16>,
    timeout_ms: u32,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
impl Transport {
    pub fn hwinfo(timeout_ms: u32) -> Result<Self, TransportError> {
        Self::named(
            "Global\\HWiNFO_SENS_SM2",
            "Global\\HWiNFO_SM2_MUTEX",
            timeout_ms,
        )
    }
    /// Isolated synthetic namespace. Cannot name real HWiNFO objects.
    pub fn synthetic(token: &str, timeout_ms: u32) -> Result<Self, TransportError> {
        if token.is_empty()
            || token.len() > 100
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(TransportError::InvalidOptions);
        }
        Self::named(
            &format!("Local\\Trofeo.Hwinfo.Test.{token}.Map"),
            &format!("Local\\Trofeo.Hwinfo.Test.{token}.Mutex"),
            timeout_ms,
        )
    }
    fn named(mapping: &str, mutex: &str, timeout_ms: u32) -> Result<Self, TransportError> {
        if timeout_ms > 1000 {
            return Err(TransportError::InvalidOptions);
        }
        Ok(Self {
            mapping: wide(mapping),
            mutex: wide(mutex),
            timeout_ms,
        })
    }
    /// Finite wait (0..1000 ms), cancellation polled at most every 10 ms of waiting.
    /// Suspend, scheduling, page faults and OS calls are not hard realtime bounded.
    pub fn capture(&self, cancel: &AtomicBool) -> Result<CopiedSnapshot, TransportError> {
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        // SAFETY: owned NUL-terminated names. No handle inheritance or object creation.
        let mutex = unsafe { OpenMutexW(0x0010_0001, 0, self.mutex.as_ptr()) };
        if mutex.is_null() {
            return Err(win_error("open mutex"));
        }
        let mutex = OwnedHandle(mutex);
        let start = Instant::now();
        let lock = loop {
            if cancel.load(Ordering::Acquire) {
                return Err(TransportError::Cancelled);
            }
            let remaining =
                Duration::from_millis(self.timeout_ms as u64).saturating_sub(start.elapsed());
            let ms = remaining.as_millis().min(10) as u32;
            // SAFETY: live mutex handle; finite timeout, same thread as release.
            match unsafe { WaitForSingleObject(mutex.0, ms) } {
                0 => {
                    break Lock {
                        handle: &mutex,
                        held: true,
                    }
                }
                0x80 => {
                    Lock {
                        handle: &mutex,
                        held: true,
                    }
                    .release()?;
                    return Err(TransportError::Abandoned);
                }
                0x102 if start.elapsed() < Duration::from_millis(self.timeout_ms as u64) => {
                    continue
                }
                0x102 => return Err(TransportError::Timeout),
                _ => return Err(win_error("wait mutex")),
            }
        };
        // Result is saved so every error releases explicitly before returning.
        let result = self.copy_locked(cancel);
        lock.release()?;
        result
    }
    fn copy_locked(&self, cancel: &AtomicBool) -> Result<CopiedSnapshot, TransportError> {
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        // SAFETY: valid name. Both open and view request FILE_MAP_READ only.
        let mapping = unsafe { OpenFileMappingW(4, 0, self.mapping.as_ptr()) };
        if mapping.is_null() {
            return Err(win_error("open mapping"));
        }
        let mapping = OwnedHandle(mapping);
        let base = unsafe { MapViewOfFile(mapping.0, 4, 0, 0, 0) };
        if base.is_null() {
            return Err(win_error("map view"));
        }
        let view = View(base);
        let mut length = 0usize;
        loop {
            let address = (base as usize)
                .checked_add(length)
                .ok_or(TransportError::InvalidRegion)?;
            let mut info = MemoryInfo::default();
            // SAFETY: query does not dereference address; output is correct x64 ABI.
            if unsafe { VirtualQuery(address as *const c_void, &mut info, size_of::<MemoryInfo>()) }
                != size_of::<MemoryInfo>()
            {
                return Err(win_error("query view"));
            }
            if info.allocation_base != base {
                break;
            }
            // Reject guarded, reserved, executable or otherwise unexpected regions.
            if info.base as usize != address
                || info.region_size == 0
                || info.state != 0x1000
                || info.kind != 0x40000
                || info.protect != 2
            {
                return Err(TransportError::InvalidRegion);
            }
            length = length
                .checked_add(info.region_size)
                .ok_or(TransportError::MappingTooLarge)?;
            if length > MAX_BUFFER_BYTES {
                return Err(TransportError::MappingTooLarge);
            }
        }
        if length < 44 {
            return Err(TransportError::InvalidRegion);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| TransportError::Allocation)?;
        bytes.resize(length, 0);
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        let mut read = 0;
        // SAFETY: no Rust reference into concurrently writable shared memory is made.
        // The OS checks accessibility and copies into an owned, initialized buffer.
        // Only this process's own view is read; no foreign process handle is opened.
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                view.0,
                bytes.as_mut_ptr().cast(),
                length,
                &mut read,
            )
        };
        if ok == 0 {
            return Err(win_error("copy view"));
        }
        if read != length {
            return Err(TransportError::InvalidRegion);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        Ok(CopiedSnapshot { bytes })
    }
}
