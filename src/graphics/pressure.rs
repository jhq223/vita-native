//! Same-domain reclamation at the kernel allocation boundary.
mod physical;
use super::{
    Error,
    fault::{Fault, sdk},
    types::Result,
};
pub(super) use physical::Commit;
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
    main: Rc<physical::Pool>,
    cdram: Rc<physical::Pool>,
    main_budget: Option<Arc<dyn crate::memory::MemoryBudget>>,
}

impl Pressure {
    pub fn new(main_budget: Option<Arc<dyn crate::memory::MemoryBudget>>) -> Self {
        Self {
            main_budget,
            ..Self::default()
        }
    }
    #[cfg(target_os = "vita")]
    pub fn initialize_main(&self) -> Result<()> {
        #[cfg(feature = "process-allocator")]
        let capacity = crate::allocator::main_usage().capacity_bytes;
        #[cfg(not(feature = "process-allocator"))]
        let capacity = crate::memory::free_memory()
            .map_err(|e| Error::Sdk(e.operation, e.code))?
            .main_bytes;
        self.main.initialize(capacity);
        Ok(())
    }
    #[cfg(target_os = "vita")]
    pub fn initialize_cdram(&self) -> Result<()> {
        // libgxm has already allocated its parameter buffer. No managed CDRAM
        // blocks exist yet, so the free count is the entire shared capacity.
        let free = crate::memory::free_memory().map_err(|e| Error::Sdk(e.operation, e.code))?;
        self.cdram.initialize(free.cdram_bytes);
        Ok(())
    }
    pub fn usage(&self, domain: Domain) -> crate::memory::PhysicalUsage {
        match domain {
            Domain::Main => {
                #[cfg(all(target_os = "vita", feature = "process-allocator"))]
                {
                    crate::allocator::main_usage()
                }
                #[cfg(not(all(target_os = "vita", feature = "process-allocator")))]
                {
                    self.main.usage()
                }
            }
            Domain::Cdram => self.cdram.usage(),
        }
    }
    pub fn commit(&self, domain: Domain, bytes: u64, fault: &Fault) -> Result<Commit> {
        let pool = match domain {
            Domain::Main => &self.main,
            Domain::Cdram => &self.cdram,
        };
        if let Some(ticket) = pool.reserve(bytes) {
            return Ok(ticket);
        }
        if self.reclaim(domain, fault)? > 0
            && let Some(ticket) = pool.reserve(bytes)
        {
            return Ok(ticket);
        }
        Err(Error::Budget {
            domain: match domain {
                Domain::Main => "physical MAIN",
                Domain::Cdram => "physical CDRAM",
            },
            requested: bytes,
            limit: pool.usage().available_bytes(),
        })
    }
    #[cfg(target_os = "vita")]
    pub fn block_size(
        &self,
        domain: Domain,
        bytes: usize,
        quantum: usize,
        page: usize,
        fault: &Fault,
    ) -> Result<usize> {
        let preferred = bytes
            .max(quantum)
            .checked_next_multiple_of(page)
            .ok_or(Error::Invalid("native block size"))?;
        if preferred as u64 > self.usage(domain).available_bytes() {
            self.reclaim(domain, fault)?;
        }
        // Retain the minimum rounded request when capacity is exhausted: the
        // actual reservation then reports the original physical-domain failure.
        Ok(
            super::suballoc::block_size(bytes, quantum, page, self.usage(domain).available_bytes())
                .unwrap_or(
                    bytes
                        .checked_next_multiple_of(page)
                        .ok_or(Error::Invalid("native block size"))?,
                ),
        )
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
        match budget.reserve(crate::memory::MemoryClass::MainKernel, bytes) {
            Ok(ticket) => Ok(Some(ticket)),
            Err(first @ crate::memory::MemoryBudgetError::Capacity(_)) => {
                if self.reclaim(domain, fault)? == 0 {
                    return Err(Error::Admission(first));
                }
                budget
                    .reserve(crate::memory::MemoryClass::MainKernel, bytes)
                    .map(Some)
                    .map_err(Error::Admission)
            }
            Err(other) => Err(Error::Admission(other)),
        }
    }

    #[cfg(target_os = "vita")]
    pub fn commit_main(
        &self,
        domain: Domain,
        bytes: usize,
        fault: &Fault,
    ) -> Result<crate::memory::MainCommit> {
        if domain == Domain::Cdram {
            return Ok(Default::default());
        }
        if let Some(ticket) = crate::memory::MainCommit::reserve(bytes) {
            return Ok(ticket);
        }
        if self.reclaim(domain, fault)? > 0 {
            if let Some(ticket) = crate::memory::MainCommit::reserve(bytes) {
                return Ok(ticket);
            }
        }
        Err(Error::Budget {
            domain: "physical MAIN",
            requested: bytes as u64,
            limit: self.usage(Domain::Main).available_bytes(),
        })
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
