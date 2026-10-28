use super::*;
use core::ffi::c_void;
use std::sync::Mutex;
static SERIAL: Mutex<()> = Mutex::new(());
unsafe extern "C" {
    fn vn_heap_init() -> i32;
    fn vn_heap_shutdown();
    fn vn_heap_malloc(size: u32) -> *mut c_void;
    fn vn_heap_memalign(alignment: u32, size: u32) -> *mut c_void;
    fn vn_heap_calloc(count: u32, size: u32) -> *mut c_void;
    fn vn_heap_realloc(pointer: *mut c_void, size: u32) -> *mut c_void;
    fn vn_heap_free(pointer: *mut c_void);
    fn vn_heap_trim() -> i32;
    fn vn_mock_fail(allocation: i32, address: i32, release: i32, mspace: i32);
    fn vn_mock_committed() -> u32;
}
struct Process;
impl Process {
    fn new() -> Self {
        // SAFETY: SERIAL excludes every other process-heap test. Host SDK calls
        // never replace the host allocator and return actual owned buffers.
        unsafe {
            vn_mock_fail(0, 0, 0, 0);
            assert_eq!(vn_heap_init(), 0);
        }
        configure_main_budget().unwrap();
        Self
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: each test has joined its workers and relinquished its heap
        // pointers. Clear deliberate failure gates before final CRT teardown.
        unsafe {
            vn_mock_fail(0, 0, 0, 0);
            vn_heap_trim();
            vn_heap_shutdown();
        }
        assert_eq!(unsafe { vn_mock_committed() }, 0);
    }
}
#[test]
fn growth_alignment_realloc_and_gpu_share_actual_kernel_commitment() {
    let _serial = SERIAL.lock().unwrap();
    let _process = Process::new();
    let initial = stats().unwrap();
    assert_eq!(initial.cpu_committed, 1 << 20);
    assert_eq!(initial.main_committed, initial.cpu_committed);
    // SAFETY: non-null returned allocations remain valid until their matching
    // free. Reads/writes are within the requested extents, including realloc.
    unsafe {
        let zero = vn_heap_malloc(0);
        assert!(!zero.is_null());
        let first = vn_heap_memalign(32768, 900_000);
        assert!(!first.is_null());
        assert_eq!(first as usize % 32768, 0);
        core::ptr::write_bytes(first, 0x37, 900_000);
        let next = vn_heap_realloc(first, 2 << 20);
        assert!(!next.is_null());
        assert_eq!(*next.cast::<u8>().add(899_999), 0x37);
        let during = stats().unwrap();
        assert_eq!(during.main_committed, vn_mock_committed());
        assert!(during.cpu_committed > initial.cpu_committed);
        let gpu = during.main_limit - during.main_committed;
        let gpu_ticket = crate::memory::MainCommit::reserve(gpu as usize).unwrap();
        assert!(!try_reserve_main(1));
        let failed = vn_heap_realloc(next, 10 << 20);
        assert!(failed.is_null());
        assert_eq!(*next.cast::<u8>().add(899_999), 0x37);
        drop(gpu_ticket);
        assert_eq!(vn_heap_realloc(next, 0), next);
        vn_heap_free(next);
        vn_heap_free(zero);
    }
    let end = stats().unwrap();
    assert_eq!(end.cpu_committed, initial.cpu_committed);
    assert_eq!(end.main_committed, end.cpu_committed);
    assert_eq!(end.live_allocations, 0);
}
#[test]
fn rollback_retains_failed_release_uid_and_refunds_only_after_physical_free() {
    let _serial = SERIAL.lock().unwrap();
    let _process = Process::new();
    // SAFETY: the SDK error gates affect only this serialized test; no failed
    // allocation is exposed. A failed free retains its complete backing block.
    unsafe {
        assert!(vn_heap_memalign(3, 4).is_null());
        assert!(vn_heap_calloc(u32::MAX, 2).is_null());
        let before = stats().unwrap().cpu_committed;
        vn_mock_fail(0, 1, 1, 0);
        assert!(vn_heap_malloc(2 << 20).is_null());
        let retired = stats().unwrap();
        assert!(retired.cpu_committed > before);
        assert_eq!(retired.main_committed, vn_mock_committed());
        assert_eq!(retired.last_error, -44);
        vn_mock_fail(0, 0, 0, 0);
        assert_eq!(vn_heap_trim(), 1);
        assert_eq!(stats().unwrap().cpu_committed, before);
        let published = vn_heap_malloc(2 << 20);
        assert!(!published.is_null());
        vn_mock_fail(0, 0, 1, 0);
        vn_heap_free(published);
        let held = stats().unwrap();
        assert_eq!(held.live_allocations, 0);
        assert!(held.cpu_committed > before);
        assert_eq!(held.main_committed, vn_mock_committed());
        vn_mock_fail(0, 0, 0, 0);
        assert_eq!(vn_heap_trim(), 1);
        assert_eq!(stats().unwrap().cpu_committed, before);
        vn_mock_fail(0, 0, 0, 1);
        assert!(vn_heap_malloc(2 << 20).is_null());
        assert_eq!(stats().unwrap().cpu_committed, before);
        vn_mock_fail(1, 0, 0, 0);
        assert!(vn_heap_malloc(2 << 20).is_null());
        assert_eq!(stats().unwrap().cpu_committed, before);
        vn_mock_fail(0, 0, 0, 0);
        let p = vn_heap_calloc(1, 64);
        assert!(!p.is_null());
        assert_eq!(*p.cast::<u8>().add(63), 0);
        vn_heap_free(p);
    }
}
#[test]
fn concurrent_allocations_and_cross_thread_free_preserve_single_owner() {
    let _serial = SERIAL.lock().unwrap();
    let _process = Process::new();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..128 {
                    // SAFETY: each worker owns its allocation. Kernel mock mutexes
                    // serialize the same production descriptor/mspace critical section.
                    unsafe {
                        let p = vn_heap_malloc(4096);
                        assert!(!p.is_null());
                        core::ptr::write_bytes(p, 0x55, 4096);
                        let address = p as usize;
                        std::thread::spawn(move || vn_heap_free(address as *mut c_void))
                            .join()
                            .unwrap();
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(stats().unwrap().live_allocations, 0);
    assert_eq!(
        stats().unwrap().main_committed,
        stats().unwrap().cpu_committed
    );
}
