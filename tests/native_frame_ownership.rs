//! Host lifecycle checks execute the production pool against an in-memory kernel
//! allocator. SDK mapping and DMA remain covered by Vita target compilation.
use vita_native::memory;
#[allow(dead_code)]
#[path = "../src/app/memory.rs"]
mod layout;
mod app {
    pub mod memory {
        pub use crate::layout::{MemoryBudget, MemoryKind, MemoryLayout, MemoryReservation};
        pub struct MemoryBlock {
            bytes: Box<[u8]>,
            _ticket: Box<dyn MemoryReservation>,
        }
        impl MemoryBlock {
            pub fn allocate(
                layout: MemoryLayout,
                budget: &dyn MemoryBudget,
            ) -> Result<Self, String> {
                let ticket = budget.reserve(layout.bytes())?;
                crate::allocation_boundary()?;
                Ok(Self {
                    bytes: vec![0; layout.bytes() as usize].into_boxed_slice(),
                    _ticket: ticket,
                })
            }
            pub fn pointer(&self) -> *mut std::ffi::c_void {
                self.bytes.as_ptr().cast_mut().cast()
            }
        }
    }
}
#[path = "../src/video/owned.rs"]
mod owned;
use owned::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct Budget(Arc<AtomicUsize>);
struct Ticket(Arc<AtomicUsize>, usize);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::SeqCst);
    }
}
impl layout::MemoryBudget for Budget {
    fn reserve(&self, bytes: u64) -> Result<Box<dyn layout::MemoryReservation>, String> {
        self.0.fetch_add(bytes as usize, Ordering::SeqCst);
        Ok(Box::new(Ticket(self.0.clone(), bytes as usize)))
    }
}
#[test]
fn sealed_readers_and_simulated_flight_delay_pool_reuse_without_extra_copy() {
    let budget = Arc::new(Budget::default());
    let pool = Nv12Pool::new([960, 544], 1 << 20, budget.clone()).unwrap();
    let metadata = budget.0.load(Ordering::SeqCst);
    let mut writable = pool.acquire().unwrap().unwrap();
    assert_eq!(writable.layout().bytes, 783360);
    writable.planes_mut().0[0] = 73;
    let shared = writable.seal();
    let address = shared.pointer();
    let flight = shared.clone();
    let reader = std::thread::spawn(move || {
        assert_eq!(shared.plane(0)[0], 73);
        shared
    });
    drop(reader.join().unwrap());
    assert!(pool.acquire().unwrap().is_none());
    assert_eq!(budget.0.load(Ordering::SeqCst), (1 << 20) + metadata);
    drop(flight);
    let again = pool.acquire().unwrap().unwrap().seal();
    assert_eq!(again.pointer(), address);
    assert_eq!(again.plane(0)[0], 73);
    assert_eq!(again.strides(), [960, 960]);
    assert_eq!(again.capacity(), 1 << 20);
    assert_ne!(again.id(), 0);
    drop(again);
    drop(pool);
    assert_eq!(budget.0.load(Ordering::SeqCst), 0);
}
#[test]
fn cancellation_releases_idle_blocks_but_keeps_sealed_storage_and_padding_valid() {
    let budget = Arc::new(Budget::default());
    let pool = Nv12Pool::new([10, 6], 2 << 20, budget.clone()).unwrap();
    let metadata = budget.0.load(Ordering::SeqCst);
    let a = pool.acquire().unwrap().unwrap();
    let b = pool.acquire().unwrap().unwrap().seal();
    assert_eq!(b.strides(), [16, 16]);
    assert_eq!(b.plane(0), &[0; 96]);
    assert_eq!(b.plane(1), &[0; 48]);
    drop(a);
    drop(pool);
    assert_eq!(budget.0.load(Ordering::SeqCst), (1 << 20) + metadata);
    assert_eq!(b.size(), [10, 6]);
    drop(b);
    assert_eq!(budget.0.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "host copy cost measurement"]
fn owned_nv12_copy_measurement() {
    let budget = Arc::new(Budget::default());
    let pool = Nv12Pool::new([960, 544], 1 << 20, budget).unwrap();
    drop(pool.acquire().unwrap());
    let source = vec![93u8; 783360];
    let mut cpu = source.clone();
    let mut gpu = source.clone();
    let start = std::time::Instant::now();
    for _ in 0..300 {
        cpu.copy_from_slice(std::hint::black_box(&source));
        gpu.copy_from_slice(std::hint::black_box(&cpu));
        std::hint::black_box(&gpu);
    }
    let copied = start.elapsed();
    let start = std::time::Instant::now();
    for _ in 0..300 {
        let mut block = pool.acquire().unwrap().unwrap();
        let (y, uv) = block.planes_mut();
        y.copy_from_slice(&source[..522240]);
        uv.copy_from_slice(&source[522240..]);
        let frame = block.seal();
        std::hint::black_box(frame.plane(0));
    }
    eprintln!(
        "300 x 960x544 NV12 host: two copies {:?}, owned single copy {:?}; copied payload 470016000 -> 235008000 bytes",
        copied,
        start.elapsed()
    );
}

// Pause the host kernel allocator after budget admission, before allocating/clearing
// storage. Thread-local injection keeps unrelated ownership tests independent.
struct AllocationGate {
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<bool>,
}
thread_local! {
    static ALLOCATION_GATE: std::cell::RefCell<Option<AllocationGate>> = const {
        std::cell::RefCell::new(None)
    };
}
fn allocation_boundary() -> Result<(), String> {
    if let Some(gate) = ALLOCATION_GATE.with_borrow_mut(Option::take) {
        gate.entered.send(()).unwrap();
        if !gate.resume.recv().unwrap() {
            return Err("controlled physical allocation failure".into());
        }
    }
    Ok(())
}
type PausedAcquire = (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<bool>,
    std::thread::JoinHandle<Result<Option<Nv12Buffer>, String>>,
);
fn pause_next_acquire(pool: Arc<Nv12Pool>) -> PausedAcquire {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ALLOCATION_GATE.with_borrow_mut(|slot| {
            *slot = Some(AllocationGate {
                entered: entered_tx,
                resume: resume_rx,
            })
        });
        pool.acquire()
    });
    (entered_rx, resume_tx, worker)
}

#[test]
fn final_gpu_reader_recycles_while_another_physical_allocation_is_paused() {
    let budget = Arc::new(Budget::default());
    let pool = Arc::new(Nv12Pool::new([960, 544], 2 << 20, budget.clone()).unwrap());
    let frame = pool.acquire().unwrap().unwrap().seal();
    let address = frame.pointer();
    let gpu_reader = frame.clone();
    drop(frame);
    let (entered, resume, allocation) = pause_next_acquire(pool.clone());
    entered.recv().unwrap();
    let (recycled_tx, recycled_rx) = std::sync::mpsc::channel();
    let recycle_pool = pool.clone();
    let recycle = std::thread::spawn(move || {
        drop(gpu_reader);
        let reused = recycle_pool.acquire().unwrap().unwrap();
        recycled_tx.send(reused.seal()).unwrap();
    });
    // A timeout only detects deadlock in the old lock boundary. Ordering comes
    // from the allocator handshake; no sleep or elapsed-time performance claim.
    let reused = recycled_rx.recv_timeout(std::time::Duration::from_secs(5));
    resume.send(true).unwrap();
    let allocated = allocation.join().unwrap().unwrap().unwrap();
    recycle.join().unwrap();
    let reused = reused.expect("GPU reader recycling waited for physical allocation");
    assert_eq!(reused.pointer(), address);
    assert!(pool.acquire().unwrap().is_none());
    drop((reused, allocated, pool));
    assert_eq!(budget.0.load(Ordering::SeqCst), 0);
}

#[test]
fn in_progress_admission_is_bounded_and_failed_allocation_restores_the_slot() {
    let budget = Arc::new(Budget::default());
    let pool = Arc::new(Nv12Pool::new([10, 6], 1 << 20, budget.clone()).unwrap());
    let metadata = budget.0.load(Ordering::SeqCst);
    let (entered, resume, allocation) = pause_next_acquire(pool.clone());
    entered.recv().unwrap();
    assert_eq!(budget.0.load(Ordering::SeqCst), metadata + (1 << 20));
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let concurrent_pool = pool.clone();
    let concurrent = std::thread::spawn(move || {
        result_tx
            .send(concurrent_pool.acquire().unwrap().is_none())
            .unwrap();
    });
    let bounded = result_rx.recv_timeout(std::time::Duration::from_secs(5));
    resume.send(false).unwrap();
    let failure = allocation.join().unwrap();
    concurrent.join().unwrap();
    assert!(bounded.expect("concurrent admission waited for physical allocation"));
    assert!(matches!(failure, Err(e) if e == "controlled physical allocation failure"));
    assert_eq!(budget.0.load(Ordering::SeqCst), metadata);
    let replacement = pool.acquire().unwrap().unwrap().seal();
    assert_eq!(replacement.plane(0), &[0; 96]);
    assert_eq!(replacement.plane(1), &[0; 48]);
    assert!(pool.acquire().unwrap().is_none());
    drop((replacement, pool));
    assert_eq!(budget.0.load(Ordering::SeqCst), 0);
}
