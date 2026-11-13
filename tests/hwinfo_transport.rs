#![cfg(all(windows, target_pointer_width = "64"))]
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use trofeo_lcd::{
    hwinfo_sm2::{DecodeError, MAX_BUFFER_BYTES},
    hwinfo_transport::{Transport, TransportError},
    sensor_policy::Status,
};
type Handle = *mut c_void;
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        file: Handle,
        attributes: *const c_void,
        protect: u32,
        high: u32,
        low: u32,
        name: *const u16,
    ) -> Handle;
    fn CreateMutexW(attributes: *const c_void, owner: i32, name: *const u16) -> Handle;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn MapViewOfFile(mapping: Handle, access: u32, high: u32, low: u32, size: usize)
        -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn ReleaseMutex(handle: Handle) -> i32;
    fn GetLastError() -> u32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}
#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    token: String,
    mutex: Handle,
    mapping: Handle,
}
impl Fixture {
    fn new(size: usize) -> Self {
        let token = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mutex = unsafe {
            CreateMutexW(
                std::ptr::null(),
                0,
                wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Mutex")).as_ptr(),
            )
        };
        assert!(!mutex.is_null());
        assert_ne!(unsafe { GetLastError() }, 183, "test namespace collision");
        let mut this = Self {
            token,
            mutex,
            mapping: std::ptr::null_mut(),
        };
        this.mapping = unsafe {
            CreateFileMappingW(
                -1isize as Handle,
                std::ptr::null(),
                4,
                0,
                size as u32,
                this.map_name().as_ptr(),
            )
        };
        assert!(!this.mapping.is_null());
        assert_ne!(unsafe { GetLastError() }, 183, "test namespace collision");
        this
    }
    fn map_name(&self) -> Vec<u16> {
        wide(&format!("Local\\Trofeo.Hwinfo.Test.{}.Map", self.token))
    }
    fn mutex_name(&self) -> Vec<u16> {
        wide(&format!("Local\\Trofeo.Hwinfo.Test.{}.Mutex", self.token))
    }
    fn transport(&self, timeout: u32) -> Transport {
        Transport::synthetic(&self.token, timeout).unwrap()
    }
    fn write(&self, data: &[u8]) {
        unsafe {
            assert_eq!(WaitForSingleObject(self.mutex, 1000), 0);
            let view = MapViewOfFile(self.mapping, 2, 0, 0, data.len());
            assert!(!view.is_null());
            std::ptr::copy_nonoverlapping(data.as_ptr(), view.cast(), data.len());
            assert_ne!(UnmapViewOfFile(view), 0);
            assert_ne!(ReleaseMutex(self.mutex), 0);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            if !self.mapping.is_null() {
                CloseHandle(self.mapping);
            }
            CloseHandle(self.mutex);
        }
    }
}
fn header(time: i64) -> Vec<u8> {
    let mut data = vec![0u8; 48];
    for (at, value) in [
        (0, 0x48576953u32),
        (4, 2),
        (8, 1),
        (20, 48),
        (24, 392),
        (32, 48),
        (36, 460),
    ] {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    data[12..20].copy_from_slice(&time.to_le_bytes());
    data
}
fn capture(f: &Fixture) -> trofeo_lcd::hwinfo_transport::CopiedSnapshot {
    f.transport(100).capture(&AtomicBool::new(false)).unwrap()
}

#[test]
fn owned_copy_decodes_after_publisher_destruction() {
    let f = Fixture::new(48);
    f.write(&header(17));
    let copy = capture(&f);
    assert!(copy.bytes().len() >= 48); // VirtualQuery reports page-rounded accessible extent.
    drop(f);
    let parsed = copy.decode().unwrap();
    assert_eq!(parsed.header.poll_time, 17);
    assert_eq!(parsed.status(), Status::Unverified);
}
#[test]
fn decoder_error_is_separate_and_lock_released() {
    let f = Fixture::new(48);
    assert!(matches!(
        capture(&f).decode(),
        Err(DecodeError::Signature(0))
    ));
    f.write(&header(9));
    assert!(capture(&f).decode().is_ok());
}
#[test]
fn invalid_offsets_cannot_escape_mapping() {
    let f = Fixture::new(48);
    let mut data = header(0);
    data[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    f.write(&data);
    assert!(capture(&f).decode().is_err());
}
#[test]
fn oversized_mapping_rejected_and_objects_released() {
    let f = Fixture::new(MAX_BUFFER_BYTES + 4096);
    assert_eq!(
        f.transport(10)
            .capture(&AtomicBool::new(false))
            .unwrap_err(),
        TransportError::MappingTooLarge
    );
    assert_eq!(unsafe { WaitForSingleObject(f.mutex, 0) }, 0);
    unsafe {
        ReleaseMutex(f.mutex);
    }
}
#[test]
fn reserved_uncommitted_mapping_rejected() {
    let mut f = Fixture::new(48);
    unsafe {
        CloseHandle(f.mapping);
    }
    f.mapping = unsafe {
        CreateFileMappingW(
            -1isize as Handle,
            std::ptr::null(),
            0x04000004,
            0,
            4096,
            f.map_name().as_ptr(),
        )
    };
    assert!(!f.mapping.is_null());
    assert_eq!(
        f.transport(10)
            .capture(&AtomicBool::new(false))
            .unwrap_err(),
        TransportError::InvalidRegion
    );
}
#[test]
fn missing_mutex_is_not_created() {
    let f = Fixture::new(48);
    let token = f.token.clone();
    drop(f);
    assert_eq!(
        Transport::synthetic(&token, 0)
            .unwrap()
            .capture(&AtomicBool::new(false))
            .unwrap_err(),
        TransportError::NotFound {
            operation: "open mutex"
        }
    );
}
#[test]
fn missing_mapping_is_distinct_and_mutex_is_released() {
    let mut f = Fixture::new(48);
    unsafe {
        CloseHandle(f.mapping);
    }
    f.mapping = std::ptr::null_mut();
    assert_eq!(
        f.transport(0).capture(&AtomicBool::new(false)).unwrap_err(),
        TransportError::NotFound {
            operation: "open mapping"
        }
    );
    assert_eq!(unsafe { WaitForSingleObject(f.mutex, 0) }, 0);
    unsafe {
        ReleaseMutex(f.mutex);
    }
}
// Publisher holds the test mutex on a different thread; readiness is handshaken.
fn hold(token: String, abandon: bool) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker = thread::spawn(move || unsafe {
        let h = OpenMutexW(
            0x00100001,
            0,
            wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Mutex")).as_ptr(),
        );
        assert!(!h.is_null());
        assert_eq!(WaitForSingleObject(h, 1000), 0);
        ready_tx.send(()).unwrap();
        stop_rx.recv().unwrap();
        if !abandon {
            assert_ne!(ReleaseMutex(h), 0);
        }
        CloseHandle(h);
    });
    ready_rx.recv().unwrap();
    (stop_tx, worker)
}
#[test]
fn timeout_is_bounded_and_not_a_decode_failure() {
    let f = Fixture::new(48);
    let (stop, worker) = hold(f.token.clone(), false);
    let start = Instant::now();
    assert_eq!(
        f.transport(30)
            .capture(&AtomicBool::new(false))
            .unwrap_err(),
        TransportError::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    stop.send(()).unwrap();
    worker.join().unwrap();
}
#[test]
fn cancellation_before_open_and_during_wait() {
    let f = Fixture::new(48);
    assert_eq!(
        f.transport(0).capture(&AtomicBool::new(true)).unwrap_err(),
        TransportError::Cancelled
    );
    let (stop, worker) = hold(f.token.clone(), false);
    let cancel_value = AtomicBool::new(false);
    let cancel = &cancel_value;
    thread::scope(|scope| {
        scope.spawn(move || {
            thread::sleep(Duration::from_millis(25));
            cancel.store(true, Ordering::Release);
        });
        assert_eq!(
            f.transport(1000).capture(cancel).unwrap_err(),
            TransportError::Cancelled
        );
    });
    stop.send(()).unwrap();
    worker.join().unwrap();
}
#[test]
fn abandoned_mutex_rejected_and_released() {
    let f = Fixture::new(48);
    f.write(&header(0));
    let (stop, worker) = hold(f.token.clone(), true);
    stop.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(
        f.transport(100)
            .capture(&AtomicBool::new(false))
            .unwrap_err(),
        TransportError::Abandoned
    );
    // A different thread must be able to acquire it (same-thread mutexes recurse).
    let (stop, worker) = hold(f.token.clone(), false);
    stop.send(()).unwrap();
    worker.join().unwrap();
}
#[test]
fn waits_for_publisher_and_reads_completed_update() {
    let f = Fixture::new(48);
    f.write(&header(10));
    let (ready_tx, ready_rx) = mpsc::channel();
    let token = f.token.clone();
    let worker = thread::spawn(move || unsafe {
        let mutex = OpenMutexW(
            0x00100001,
            0,
            wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Mutex")).as_ptr(),
        );
        assert_eq!(WaitForSingleObject(mutex, 1000), 0);
        let mapping = OpenFileMappingW(
            2,
            0,
            wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Map")).as_ptr(),
        );
        let view = MapViewOfFile(mapping, 2, 0, 0, 48);
        assert!(!view.is_null());
        let data = header((1i64 << 40) | 20);
        std::ptr::copy_nonoverlapping(data.as_ptr(), view.cast(), 16);
        ready_tx.send(()).unwrap();
        thread::sleep(Duration::from_millis(50));
        std::ptr::copy_nonoverlapping(data.as_ptr().add(16), view.cast::<u8>().add(16), 32);
        UnmapViewOfFile(view);
        CloseHandle(mapping);
        ReleaseMutex(mutex);
        CloseHandle(mutex);
    });
    ready_rx.recv().unwrap();
    assert_eq!(
        f.transport(1000)
            .capture(&AtomicBool::new(false))
            .unwrap()
            .decode()
            .unwrap()
            .header
            .poll_time,
        (1i64 << 40) | 20
    );
    worker.join().unwrap();
}
#[test]
fn reopen_observes_recreated_mapping_without_cached_data() {
    let mut f = Fixture::new(48);
    f.write(&header(1));
    let transport = f.transport(100);
    assert_eq!(
        transport
            .capture(&AtomicBool::new(false))
            .unwrap()
            .decode()
            .unwrap()
            .header
            .poll_time,
        1
    );
    unsafe {
        CloseHandle(f.mapping);
    }
    f.mapping = std::ptr::null_mut();
    assert!(matches!(
        transport.capture(&AtomicBool::new(false)),
        Err(TransportError::NotFound { .. })
    ));
    f.mapping = unsafe {
        CreateFileMappingW(
            -1isize as Handle,
            std::ptr::null(),
            4,
            0,
            8192,
            f.map_name().as_ptr(),
        )
    };
    assert!(!f.mapping.is_null());
    assert_ne!(unsafe { GetLastError() }, 183);
    f.write(&header(2));
    assert_eq!(
        transport
            .capture(&AtomicBool::new(false))
            .unwrap()
            .decode()
            .unwrap()
            .header
            .poll_time,
        2
    );
}
#[test]
fn repeated_open_close_leaves_no_named_objects_or_views() {
    let f = Fixture::new(48);
    f.write(&header(0));
    for _ in 0..200 {
        assert!(capture(&f).decode().is_ok());
    }
    let map = f.map_name();
    let mutex = f.mutex_name();
    drop(f);
    unsafe {
        let m = OpenFileMappingW(4, 0, map.as_ptr());
        if !m.is_null() {
            CloseHandle(m);
            panic!("leaked mapping/view");
        }
        assert_eq!(GetLastError(), 2);
        let m = OpenMutexW(0x00100001, 0, mutex.as_ptr());
        if !m.is_null() {
            CloseHandle(m);
            panic!("leaked mutex");
        }
        assert_eq!(GetLastError(), 2);
    }
}
#[test]
fn option_validation_prevents_infinite_wait_and_namespace_escape() {
    assert!(matches!(
        Transport::synthetic("bad\\name", 1),
        Err(TransportError::InvalidOptions)
    ));
    assert!(matches!(
        Transport::synthetic("valid", u32::MAX),
        Err(TransportError::InvalidOptions)
    ));
    assert!(matches!(
        Transport::synthetic("", 1),
        Err(TransportError::InvalidOptions)
    ));
}

#[test]
fn access_denied_is_distinct_for_test_mutex_and_mapping() {
    // DACL exists only on newly created isolated test objects; no existing ACL is modified.
    for deny_mutex in [true, false] {
        let mut f = Fixture::new(48);
        let mut descriptor = std::ptr::null_mut();
        unsafe {
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide("D:(D;;GA;;;WD)").as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut()
                ),
                0
            );
            let attributes = SecurityAttributes {
                length: std::mem::size_of::<SecurityAttributes>() as u32,
                descriptor,
                inherit: 0,
            };
            if deny_mutex {
                CloseHandle(f.mutex);
                f.mutex = CreateMutexW(
                    (&attributes as *const SecurityAttributes).cast(),
                    0,
                    f.mutex_name().as_ptr(),
                );
                assert!(!f.mutex.is_null());
            } else {
                CloseHandle(f.mapping);
                f.mapping = CreateFileMappingW(
                    -1isize as Handle,
                    (&attributes as *const SecurityAttributes).cast(),
                    4,
                    0,
                    4096,
                    f.map_name().as_ptr(),
                );
                assert!(!f.mapping.is_null());
            }
            LocalFree(descriptor);
        }
        assert_eq!(
            f.transport(10)
                .capture(&AtomicBool::new(false))
                .unwrap_err(),
            TransportError::AccessDenied {
                operation: if deny_mutex {
                    "open mutex"
                } else {
                    "open mapping"
                }
            }
        );
    }
}
