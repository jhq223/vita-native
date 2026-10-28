//! Same-domain reclamation at the kernel allocation boundary.
use super::{
    Error,
    fault::{Fault, sdk},
    types::Result,
};
use std::{
    cell::RefCell,
    prelude::v1::*,
    rc::{Rc, Weak},
    sync::Arc,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Domain {
    Main,
    Cdram,
}

pub(super) trait IdlePool {
    /// Release exclusively cached owners and return their physical capacity.
    fn release_idle(&self) -> u64;
}

struct RegisteredPool {
    domain: Domain,
    pool: Weak<dyn IdlePool>,
}
#[derive(Clone, Default)]
pub(super) struct Pressure {
    pools: Rc<RefCell<Vec<RegisteredPool>>>,
    main_budget: Option<Arc<dyn crate::memory::MemoryBudget>>,
}

impl Pressure {
    pub fn new(main_budget: Option<Arc<dyn crate::memory::MemoryBudget>>) -> Self {
        Self {
            main_budget,
            ..Self::default()
        }
    }
    pub fn register<T: IdlePool + 'static>(&self, domain: Domain, pool: &Rc<T>) {
        let pool: Rc<dyn IdlePool> = pool.clone();
        self.pools.borrow_mut().push(RegisteredPool {
            domain,
            pool: Rc::downgrade(&pool),
        });
    }

    fn reclaim(&self, domain: Domain, fault: &Fault) -> Result<u64> {
        let mut released = 0;
        for owner in self.pools.borrow().iter() {
            if owner.domain == domain
                && let Some(pool) = owner.pool.upgrade()
            {
                released += pool.release_idle();
                fault.check()?;
            }
        }
        Ok(released)
    }
    pub fn reserve(
        &self,
        domain: Domain,
        bytes: u64,
        fault: &Fault,
    ) -> Result<Option<Box<dyn crate::memory::MemoryReservation>>> {
        if domain != Domain::Main {
            return Ok(None);
        }
        let Some(budget) = &self.main_budget else {
            return Ok(None);
        };
        match budget.reserve(bytes) {
            Ok(ticket) => Ok(Some(ticket)),
            Err(first) => {
                if self.reclaim(domain, fault)? == 0 {
                    return Err(Error::Admission(first));
                }
                budget.reserve(bytes).map(Some).map_err(Error::Admission)
            }
        }
    }

    pub fn allocate(
        &self,
        domain: Domain,
        fault: &Fault,
        mut allocate: impl FnMut() -> i32,
    ) -> Result<i32> {
        let first = allocate();
        if first >= 0 {
            return Ok(first);
        }
        // Sony kernel/error.h: exhausted physical pages, contiguous page units,
        // and the CDRAM-specific physical-page error. Other SDK failures retain
        // their original result without touching caches.
        let exhausted = match first as u32 {
            0x80024302 | 0x80024303 => true,
            0x80024309 => domain == Domain::Cdram,
            _ => false,
        };
        if exhausted && self.reclaim(domain, fault)? > 0 {
            let next = allocate();
            sdk("sceKernelAllocMemBlock", next)?;
            return Ok(next);
        }
        Err(Error::Sdk("sceKernelAllocMemBlock", first))
    }
}

#[cfg(test)]
mod tests;
