use super::super::{render_targets::Pool, retained::Retained};
use super::*;
use std::cell::Cell;

const MAIN_FULL: i32 = 0x80024302_u32 as i32;
const CDRAM_FULL: i32 = 0x80024309_u32 as i32;

struct Storage {
    freed: Rc<Cell<u64>>,
    bytes: u64,
    release_fault: Option<Fault>,
}
impl Drop for Storage {
    fn drop(&mut self) {
        if let Some(fault) = &self.release_fault {
            fault.record("release block", -7);
        } else {
            self.freed.set(self.freed.get() + self.bytes);
        }
    }
}
fn retain(
    pool: &RefCell<Retained<Storage>>,
    freed: &Rc<Cell<u64>>,
    bytes: u64,
    release_fault: Option<Fault>,
) -> Rc<Storage> {
    let storage = Rc::new(Storage {
        freed: freed.clone(),
        bytes,
        release_fault,
    });
    pool.borrow_mut().touch(&storage, bytes);
    storage
}

#[test]
fn exhaustion_reclaims_same_domain_idle_blocks_but_preserves_flights() {
    let pressure = Pressure::default();
    let fault = Fault::default();
    let main = Rc::new(RefCell::new(Retained::new(12)));
    let cdram = Rc::new(RefCell::new(Retained::new(8)));
    pressure.register(Domain::Main, &main);
    pressure.register(Domain::Cdram, &cdram);
    let main_freed = Rc::new(Cell::new(0));
    let cdram_freed = Rc::new(Cell::new(0));
    drop(retain(&main, &main_freed, 4, None));
    let flight = retain(&main, &main_freed, 8, None);
    drop(retain(&cdram, &cdram_freed, 8, None));
    let calls = Cell::new(0);
    let uid = pressure
        .allocate(Domain::Main, &fault, || {
            calls.set(calls.get() + 1);
            if main_freed.get() >= 4 { 12 } else { MAIN_FULL }
        })
        .unwrap();
    assert_eq!(uid, 12);
    assert_eq!(calls.get(), 2);
    assert_eq!(main_freed.get(), 4);
    assert_eq!(cdram_freed.get(), 0);
    assert_eq!(main.borrow().bytes(), 8);
    assert_eq!(flight.bytes, 8);
    drop(flight);
    assert!(matches!(
        pressure.allocate(Domain::Main, &fault, || MAIN_FULL),
        Err(Error::Sdk("sceKernelAllocMemBlock", MAIN_FULL))
    ));
    assert_eq!(main_freed.get(), 12);
    assert!(matches!(
        pressure.allocate(Domain::Cdram, &fault, || CDRAM_FULL),
        Err(Error::Sdk("sceKernelAllocMemBlock", CDRAM_FULL))
    ));
    assert_eq!(cdram_freed.get(), 8);
}

#[test]
fn success_unrelated_errors_and_empty_caches_do_not_retry() {
    let pressure = Pressure::default();
    let fault = Fault::default();
    let pool = Rc::new(RefCell::new(Retained::new(8)));
    let freed = Rc::new(Cell::new(0));
    pressure.register(Domain::Main, &pool);
    drop(retain(&pool, &freed, 8, None));
    for code in [21, -1, CDRAM_FULL] {
        let calls = Cell::new(0);
        let result = pressure.allocate(Domain::Main, &fault, || {
            calls.set(calls.get() + 1);
            code
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(freed.get(), 0);
        if code >= 0 {
            assert_eq!(result.unwrap(), code);
        } else {
            assert!(
                matches!(result, Err(Error::Sdk("sceKernelAllocMemBlock", actual)) if actual == code)
            );
        }
    }
    drop(pool);
    let calls = Cell::new(0);
    assert!(
        pressure
            .allocate(Domain::Main, &fault, || {
                calls.set(calls.get() + 1);
                MAIN_FULL
            })
            .is_err()
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn release_fault_stops_reclamation_and_second_allocation() {
    let pressure = Pressure::default();
    let fault = Fault::default();
    let pool = Rc::new(RefCell::new(Retained::new(8)));
    let later = Rc::new(RefCell::new(Retained::new(8)));
    let freed = Rc::new(Cell::new(0));
    pressure.register(Domain::Main, &pool);
    pressure.register(Domain::Main, &later);
    drop(retain(&pool, &freed, 8, Some(fault.clone())));
    drop(retain(&later, &freed, 8, None));
    let calls = Cell::new(0);
    assert!(matches!(
        pressure.allocate(Domain::Main, &fault, || {
            calls.set(calls.get() + 1);
            MAIN_FULL
        }),
        Err(Error::Terminal(_))
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(freed.get(), 0);
    assert_eq!(later.borrow().bytes(), 8);
}

#[test]
fn scheduling_target_allocation_can_reclaim_its_own_pool() {
    let pressure = Pressure::default();
    let fault = Fault::default();
    let pool = Rc::new(RefCell::new(Pool::<Storage>::new(8192)));
    let freed = Rc::new(Cell::new(0));
    pressure.register(Domain::Main, &pool);
    drop(
        Pool::get_or_create(&pool, [128, 128], 4096, |_| {
            Ok(Rc::new(Storage {
                freed: freed.clone(),
                bytes: 4096,
                release_fault: None,
            }))
        })
        .unwrap(),
    );
    let target = Pool::get_or_create(&pool, [256, 256], 4096, |_| {
        pressure.allocate(Domain::Main, &fault, || {
            if freed.get() == 4096 { 2 } else { MAIN_FULL }
        })?;
        Ok(Rc::new(Storage {
            freed: freed.clone(),
            bytes: 4096,
            release_fault: None,
        }))
    })
    .unwrap();
    assert_eq!(freed.get(), 4096);
    assert!(!pool.borrow().contains([128, 128]).unwrap());
    assert!(pool.borrow().contains([256, 256]).unwrap());
    assert_eq!(target.bytes, 4096);
}

#[derive(Default)]
struct BudgetState {
    used: u64,
    calls: usize,
}
struct TestBudget {
    limit: u64,
    state: std::sync::Arc<std::sync::Mutex<BudgetState>>,
}
struct TestTicket {
    bytes: u64,
    state: std::sync::Arc<std::sync::Mutex<BudgetState>>,
}
impl Drop for TestTicket {
    fn drop(&mut self) {
        self.state.lock().unwrap().used -= self.bytes;
    }
}
impl crate::memory::MemoryBudget for TestBudget {
    fn reserve(
        &self,
        class: crate::memory::MemoryClass,
        bytes: u64,
    ) -> std::result::Result<
        Box<dyn crate::memory::MemoryReservation>,
        crate::memory::MemoryBudgetError,
    > {
        assert_eq!(class, crate::memory::MemoryClass::MainKernel);
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        if state.used + bytes > self.limit {
            return Err(crate::memory::MemoryBudgetError::Capacity(
                "MAIN limit".into(),
            ));
        }
        state.used += bytes;
        Ok(Box::new(TestTicket {
            bytes,
            state: self.state.clone(),
        }))
    }
}
struct BudgetOwner {
    _ticket: Box<dyn crate::memory::MemoryReservation>,
}
#[test]
fn main_budget_reclaims_idle_only_and_preserves_inflight_and_cdram() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(BudgetState::default()));
    let budget = std::sync::Arc::new(TestBudget {
        limit: 12,
        state: state.clone(),
    });
    let pressure = Pressure::new(Some(budget));
    let fault = Fault::default();
    let idle = Rc::new(RefCell::new(Retained::new(12)));
    pressure.register(Domain::Main, &idle);
    let idle_owner = Rc::new(BudgetOwner {
        _ticket: pressure.reserve(Domain::Main, 4, &fault).unwrap().unwrap(),
    });
    idle.borrow_mut().touch(&idle_owner, 4);
    drop(idle_owner);
    let flight = pressure.reserve(Domain::Main, 8, &fault).unwrap().unwrap();
    let cdram = pressure.reserve(Domain::Cdram, 100, &fault).unwrap();
    assert!(cdram.is_none());
    let new = pressure.reserve(Domain::Main, 4, &fault).unwrap().unwrap();
    assert_eq!(state.lock().unwrap().used, 12);
    assert_eq!(state.lock().unwrap().calls, 4);
    assert!(
        matches!(pressure.reserve(Domain::Main, 1, &fault), Err(Error::Admission(crate::memory::MemoryBudgetError::Capacity(message))) if message == "MAIN limit")
    );
    assert_eq!(state.lock().unwrap().calls, 5);
    drop((flight, new));
    assert_eq!(state.lock().unwrap().used, 0);
}

struct PermanentBudget;
impl crate::memory::MemoryBudget for PermanentBudget {
    fn reserve(
        &self,
        _: crate::memory::MemoryClass,
        _: u64,
    ) -> std::result::Result<
        Box<dyn crate::memory::MemoryReservation>,
        crate::memory::MemoryBudgetError,
    > {
        Err(crate::memory::MemoryBudgetError::Other(
            "injected failure".into(),
        ))
    }
}
#[test]
fn non_capacity_budget_failure_does_not_evict_idle_owners() {
    let pressure = Pressure::new(Some(std::sync::Arc::new(PermanentBudget)));
    let fault = Fault::default();
    let idle = Rc::new(RefCell::new(Retained::new(4)));
    pressure.register(Domain::Main, &idle);
    let freed = Rc::new(Cell::new(0));
    drop(retain(&idle, &freed, 4, None));
    assert!(
        matches!(pressure.reserve(Domain::Main, 4, &fault), Err(Error::Admission(crate::memory::MemoryBudgetError::Other(message))) if message == "injected failure")
    );
    assert_eq!(freed.get(), 0);
    assert_eq!(idle.borrow().bytes(), 4);
}
