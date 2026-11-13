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
    AddressOverflow,
    PayloadTooLarge { required: usize },
    Header(DecodeError),
    HeaderChanged,
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
        // Stage one: fixed stack storage, only the supported header bytes.
        let mut prefix = [0u8; 48];
        view.copy_into(&mut prefix[..44])?;
        let header_len = hwinfo_sm2::header_size(&prefix[..44]).map_err(TransportError::Header)?;
        if header_len == 48 {
            view.copy_into(&mut prefix[..48])?;
        }
        let layout = hwinfo_sm2::validate_header(&prefix[..header_len], usize::MAX)
            .map_err(TransportError::Header)?;
        let length = layout.required_len;
        if length > MAX_BUFFER_BYTES {
            return Err(TransportError::PayloadTooLarge { required: length });
        }
        // Retain wire offsets: the bounded prefix includes any section gaps.
        // Check its entire readable extent before payload allocation/copy.
        view.validate_range(length)?;
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| TransportError::Allocation)?;
        bytes.resize(length, 0);
        view.copy_into(&mut bytes)?;
        if bytes.get(..header_len) != Some(&prefix[..header_len]) {
            return Err(TransportError::HeaderChanged);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        Ok(CopiedSnapshot { bytes })
    }
}

impl View {
    // Only walk the requested bounded prefix, not all trailing mapping capacity.
    fn validate_range(&self, length: usize) -> Result<(), TransportError> {
        let base = self.0 as usize;
        let end = base
            .checked_add(length)
            .ok_or(TransportError::AddressOverflow)?;
        let mut cursor = base;
        while cursor < end {
            let mut info = MemoryInfo::default();
            // SAFETY: VirtualQuery inspects an address; output has checked x64 ABI.
            if unsafe { VirtualQuery(cursor as *const c_void, &mut info, size_of::<MemoryInfo>()) }
                != size_of::<MemoryInfo>()
            {
                return Err(win_error("query view"));
            }
            if info.allocation_base != self.0
                || info.base as usize != cursor
                || info.region_size == 0
                || info.state != 0x1000
                || info.kind != 0x40000
                || info.protect != 2
            {
                return Err(TransportError::InvalidRegion);
            }
            cursor = cursor
                .checked_add(info.region_size)
                .ok_or(TransportError::AddressOverflow)?;
        }
        Ok(())
    }
    fn copy_into(&self, bytes: &mut [u8]) -> Result<(), TransportError> {
        self.validate_range(bytes.len())?;
        let mut read = 0;
        // SAFETY: own live view, OS-checked source, initialized exclusively owned
        // destination. No Rust reference is made into shared memory.
        if unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                self.0,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &mut read,
            )
        } == 0
        {
            return Err(win_error("copy view"));
        }
        if read != bytes.len() {
            return Err(TransportError::InvalidRegion);
        }
        Ok(())
    }
}
