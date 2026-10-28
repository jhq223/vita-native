//! Per-submission notifications and precise retirement of their original owners.
use super::types::Result;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    prelude::v1::*,
    rc::Rc,
};

pub(super) struct Completion<T> {
    pub address: *mut u32,
    pub value: u32,
    pub done: Cell<bool>,
    pub leases: RefCell<Vec<T>>,
    pub staging: usize,
}
impl<T> Completion<T> {
    pub fn poll(&self) -> bool {
        if !self.done.get() && unsafe { core::ptr::read_volatile(self.address) } == self.value {
            self.done.set(true);
            self.leases.borrow_mut().clear();
        }
        self.done.get()
    }
}
pub(super) struct Flight<T> {
    pub slot: usize,
    pub token: Rc<Completion<T>>,
}

pub(super) fn collect<T>(
    flights: &mut VecDeque<Flight<T>>,
    free: &mut Vec<usize>,
    check: impl Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    let mut i = 0;
    while i < flights.len() {
        if flights[i].token.poll() {
            // Owner destruction can latch a native fault. Keep the slot reserved
            // when cleanup fails rather than allowing notification reuse.
            check()?;
            let flight = flights.remove(i).expect("flight");
            free.push(flight.slot);
        } else {
            i += 1;
        }
    }
    check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{fault::Fault, suballoc::FreeList};
    use std::ops::Range;

    // Exact production free-list allocation is returned only by the final lease.
    struct Allocation {
        free: Rc<RefCell<FreeList>>,
        range: Range<usize>,
    }
    impl Drop for Allocation {
        fn drop(&mut self) {
            self.free.borrow_mut().release(self.range.clone());
        }
    }
    fn allocate(free: &Rc<RefCell<FreeList>>, bytes: usize) -> Rc<Allocation> {
        let range = free.borrow_mut().take(bytes, 1).unwrap();
        Rc::new(Allocation {
            free: free.clone(),
            range,
        })
    }
    fn completion<T>(notification: &Cell<u32>, value: u32, leases: Vec<T>) -> Rc<Completion<T>> {
        Rc::new(Completion {
            address: notification.as_ptr(),
            value,
            done: Cell::new(false),
            leases: RefCell::new(leases),
            staging: 0,
        })
    }

    #[test]
    fn device_poll_retires_early_scenes_after_final_fence_and_reopens_idle_capacity() {
        let memory = Rc::new(RefCell::new(FreeList::new(16)));
        let notifications = [Cell::new(0), Cell::new(0)];
        let first = completion(&notifications[0], 1, std::vec![allocate(&memory, 8)]);
        let last = completion(&notifications[1], 2, std::vec![allocate(&memory, 8)]);
        let mut flights = VecDeque::from([
            Flight {
                slot: 0,
                token: first,
            },
            Flight {
                slot: 1,
                token: last.clone(),
            },
        ]);
        let mut slots = Vec::new();
        notifications[0].set(1);
        notifications[1].set(2);
        // This is Fence::poll's production observation: only its own token retires.
        assert!(last.poll());
        assert_eq!(last.staging, 0);
        assert_eq!(memory.borrow().bytes(), 8);
        assert!(memory.borrow_mut().take(16, 1).is_none());
        // The image now remains unchanged: ordinary device poll, with no new
        // scene, allocation attempt, finish, or memory accounting observation.
        collect(&mut flights, &mut slots, || Ok(())).unwrap();
        assert!(flights.is_empty());
        assert_eq!(slots, [0, 1]);
        let next = allocate(&memory, 16);
        assert_eq!(next.range, 0..16);
        // A caller-owned fence remains complete after its slot is reused.
        notifications[1].set(99);
        assert!(last.poll());
        drop(next);
        assert_eq!(memory.borrow().bytes(), 16);
    }

    #[test]
    fn each_scene_or_transfer_notification_preserves_its_own_delayed_readers() {
        let memory = Rc::new(RefCell::new(FreeList::new(16)));
        let notifications = [Cell::new(1), Cell::new(0), Cell::new(3)];
        let shared = allocate(&memory, 8);
        let first = completion(&notifications[0], 1, std::vec![shared.clone()]);
        let transfer = completion(&notifications[1], 2, std::vec![shared]);
        let last = completion(&notifications[2], 3, std::vec![allocate(&memory, 8)]);
        let mut flights = VecDeque::from([
            Flight {
                slot: 0,
                token: first,
            },
            Flight {
                slot: 1,
                token: transfer.clone(),
            },
            Flight {
                slot: 2,
                token: last.clone(),
            },
        ]);
        let mut slots = Vec::new();
        assert!(last.poll());
        collect(&mut flights, &mut slots, || Ok(())).unwrap();
        assert_eq!(slots, [0, 2]);
        assert_eq!(flights.len(), 1);
        assert!(!transfer.done.get());
        assert_eq!(transfer.leases.borrow().len(), 1);
        assert_eq!(memory.borrow().bytes(), 8);
        notifications[1].set(2);
        collect(&mut flights, &mut slots, || Ok(())).unwrap();
        assert_eq!(slots, [0, 2, 1]);
        assert!(flights.is_empty());
        assert_eq!(memory.borrow().bytes(), 16);
    }

    #[test]
    fn cleanup_failure_keeps_notification_capacity_reserved_and_later_owners_intact() {
        struct Poison(Fault);
        impl Drop for Poison {
            fn drop(&mut self) {
                self.0.record("test native cleanup", -17);
            }
        }
        let fault = Fault::default();
        let notifications = [Cell::new(1), Cell::new(2)];
        let first = completion(&notifications[0], 1, std::vec![Poison(fault.clone())]);
        let later = completion(&notifications[1], 2, std::vec![Poison(fault.clone())]);
        let mut flights = VecDeque::from([
            Flight {
                slot: 0,
                token: first,
            },
            Flight {
                slot: 1,
                token: later.clone(),
            },
        ]);
        let mut slots = Vec::new();
        assert!(collect(&mut flights, &mut slots, || fault.check()).is_err());
        assert!(slots.is_empty());
        assert_eq!(flights.len(), 2);
        assert!(!later.done.get());
        assert_eq!(later.leases.borrow().len(), 1);
        assert_eq!(fault.get().unwrap().code, -17);
    }
}
