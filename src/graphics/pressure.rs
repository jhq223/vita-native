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
pub(super) struct Pressure(Rc<RefCell<Vec<RegisteredPool>>>);

impl Pressure {
    pub fn register<T: IdlePool + 'static>(&self, domain: Domain, pool: &Rc<T>) {
        let pool: Rc<dyn IdlePool> = pool.clone();
        self.0.borrow_mut().push(RegisteredPool {
            domain,
            pool: Rc::downgrade(&pool),
        });
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
        if exhausted {
            let mut released = 0;
            for owner in self.0.borrow().iter() {
                if owner.domain == domain
                    && let Some(pool) = owner.pool.upgrade()
                {
                    released += pool.release_idle();
                    fault.check()?;
                }
            }
            if released > 0 {
                let next = allocate();
                sdk("sceKernelAllocMemBlock", next)?;
                return Ok(next);
            }
        }
        Err(Error::Sdk("sceKernelAllocMemBlock", first))
    }
}

#[cfg(test)]
mod tests;
