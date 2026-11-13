use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
type Handle = *mut c_void;
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        f: Handle,
        a: *const c_void,
        p: u32,
        h: u32,
        l: u32,
        n: *const u16,
    ) -> Handle;
    fn CreateMutexW(a: *const c_void, o: i32, n: *const u16) -> Handle;
    fn MapViewOfFile(h: Handle, a: u32, x: u32, y: u32, n: usize) -> *mut c_void;
    fn UnmapViewOfFile(p: *const c_void) -> i32;
    fn CloseHandle(h: Handle) -> i32;
    fn WaitForSingleObject(h: Handle, t: u32) -> u32;
    fn ReleaseMutex(h: Handle) -> i32;
    fn GetLastError() -> u32;
}
struct Owned(Handle);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub struct Mapping {
    pub token: String,
    map: Owned,
    mutex: Owned,
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
impl Mapping {
    pub fn new() -> Self {
        unsafe {
            let token = format!(
                "pipeline-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let mutex = Owned(CreateMutexW(
                std::ptr::null(),
                0,
                wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Mutex")).as_ptr(),
            ));
            assert!(!mutex.0.is_null());
            assert_ne!(GetLastError(), 183);
            let map = Owned(CreateFileMappingW(
                -1isize as Handle,
                std::ptr::null(),
                4,
                0,
                4096,
                wide(&format!("Local\\Trofeo.Hwinfo.Test.{token}.Map")).as_ptr(),
            ));
            assert!(!map.0.is_null());
            assert_ne!(GetLastError(), 183);
            Self { token, map, mutex }
        }
    }
    pub fn write(&self, bytes: &[u8]) {
        unsafe {
            assert!(bytes.len() <= 4096);
            assert_eq!(WaitForSingleObject(self.mutex.0, 1000), 0);
            let p = MapViewOfFile(self.map.0, 2, 0, 0, 4096);
            if p.is_null() {
                ReleaseMutex(self.mutex.0);
                panic!("test mapping view failed");
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast(), bytes.len());
            let unmap = UnmapViewOfFile(p);
            let release = ReleaseMutex(self.mutex.0);
            assert_ne!(unmap, 0);
            assert_ne!(release, 0);
        }
    }
    pub fn read(&self) -> Vec<u8> {
        trofeo_lcd::hwinfo_transport::Transport::synthetic(&self.token, 100)
            .unwrap()
            .capture(&AtomicBool::new(false))
            .unwrap()
            .bytes()
            .to_vec()
    }
}
pub fn bytes(poll: i64, renamed: bool, reordered: bool, missing: bool) -> Vec<u8> {
    let mut b = vec![0; 48 + 2 * 392 + 2 * 460];
    for (at, n) in [
        (0, u32::from_le_bytes(*b"HWiS")),
        (4, 2),
        (8, 1),
        (20, 48),
        (24, 392),
        (28, 2),
        (32, 832),
        (36, 460),
        (40, 2),
        (44, 2000),
    ] {
        b[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    b[12..20].copy_from_slice(&poll.to_le_bytes());
    for i in 0..2 {
        let sensor = 48 + i * 392;
        let id = if reordered { 2 - i } else { i + 1 };
        b[sensor..sensor + 4].copy_from_slice(&(id as u32).to_le_bytes());
        let at = 832 + i * 460;
        for (off, n) in [(0, 1u32), (4, i as u32), (8, if missing { 9 } else { 3 })] {
            b[at + off..at + off + 4].copy_from_slice(&n.to_le_bytes());
        }
        b[at + 284..at + 292].copy_from_slice(&(40.0 + id as f64).to_le_bytes());
        b[at + 444] = b'C';
        let label = if renamed {
            b"Renamed".as_slice()
        } else {
            b"Synthetic".as_slice()
        };
        b[at + 316..at + 316 + label.len()].copy_from_slice(label);
    }
    b
}
