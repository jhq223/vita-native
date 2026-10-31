//! Thread-owned development PMON observations, in raw cycle-counter ticks.
use crate::app::{Error, sdk};
use std::{cell::Cell, marker::PhantomData, prelude::v1::*, rc::Rc, sync::Mutex};

const SELF: i32 = sdk::SCE_PERF_ARM_PMON_THREAD_ID_SELF as i32;
// Sony libperf.h names counter 31 SCE_PERF_ARM_PMON_CYCLE_COUNTER.
const CYCLE_COUNTER: u32 = 31;

std::thread_local! {
    static OWNED: Cell<bool> = const { Cell::new(false) };
}

struct Module {
    users: usize,
    loaded_here: bool,
}
static MODULE: Mutex<Module> = Mutex::new(Module {
    users: 0,
    loaded_here: false,
});

fn acquire_module() -> Result<(), Error> {
    let mut module = MODULE.lock().map_err(|_| Error {
        operation: "PMON module ownership poisoned",
        code: -1,
    })?;
    if module.users == 0 {
        // SAFETY: scalar module query; this mutex serializes module ownership.
        let loaded = unsafe { sdk::sceSysmoduleIsLoaded(sdk::SCE_SYSMODULE_PERF) };
        if loaded as u32 == sdk::SCE_SYSMODULE_ERROR_UNLOADED {
            Error::check("sceSysmoduleLoadModule PERF", unsafe {
                sdk::sceSysmoduleLoadModule(sdk::SCE_SYSMODULE_PERF)
            })?;
            module.loaded_here = true;
        } else {
            Error::check("sceSysmoduleIsLoaded PERF", loaded)?;
            module.loaded_here = false;
        }
    }
    module.users += 1;
    Ok(())
}

fn release_module() -> Result<(), Error> {
    let mut module = MODULE.lock().map_err(|_| Error {
        operation: "PMON module ownership poisoned",
        code: -1,
    })?;
    if module.users == 1 && module.loaded_here {
        // SAFETY: the last native owner has stopped its thread's counters.
        // Failed unload retains the lease so an explicit retry remains valid.
        Error::check("sceSysmoduleUnloadModule PERF", unsafe {
            sdk::sceSysmoduleUnloadModule(sdk::SCE_SYSMODULE_PERF)
        })?;
    }
    module.users -= 1;
    if module.users == 0 {
        module.loaded_here = false;
    }
    Ok(())
}

/// One running observation of a thread's 32-bit PMON cycle counter.
///
/// `raw_cycle_ticks` wraps modulo 2^32. Pair readings from the same live `Pmon`
/// owner and bound the interval to less than a full wrap. Sony documents saved
/// thread-context counters, but exposes no cycle-divider contract here; retain
/// raw ticks until device validation establishes their interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PmonSample {
    /// Native identity of the thread that owns the counter.
    pub thread_id: i32,
    /// Unscaled counter 31, read while the thread's PMON remains started.
    pub raw_cycle_ticks: u32,
}

/// Exclusive PMON owner for the calling thread, available only with `profiling`.
///
/// The owner is neither `Send` nor `Sync`. A second owner on the same thread is
/// rejected before reset. Each thread may own its own counters; PERF module
/// loading is shared across owners. External PMON users on an owned thread must
/// coordinate with this owner because reset affects that thread's counters.
///
/// Sony specifies counter save/restore when a thread leaves and reenters RUNNING.
/// Live short-scope behavior, migration and the cycle divider still require a
/// device self-test. Reads include PMON query overhead and supply raw ticks.
/// Explicit `stop` reports native failures and can be retried. Drop attempts
/// cleanup; a failed stop retains the thread reservation and loaded module.
pub struct Pmon {
    thread_id: i32,
    reserved: bool,
    active: bool,
    module: bool,
    _thread: PhantomData<Rc<()>>,
}

impl Pmon {
    /// Reserve the current thread, load PERF if necessary, reset and start PMON.
    /// This resets all of this thread's PMON counters once, before observation.
    pub fn start() -> Result<Self, Error> {
        if OWNED.with(|owned| owned.replace(true)) {
            return Err(Error {
                operation: "PMON thread already owned",
                code: -1,
            });
        }
        let mut owner = Self {
            thread_id: 0,
            reserved: true,
            active: false,
            module: false,
            _thread: PhantomData,
        };
        // SAFETY: scalar caller identity, retained only for observation matching.
        owner.thread_id = Error::check("sceKernelGetThreadId", unsafe {
            sdk::sceKernelGetThreadId()
        })?;
        acquire_module()?;
        owner.module = true;
        // SAFETY: this !Send owner reserved the calling thread before any reset.
        Error::check("scePerfArmPmonReset", unsafe {
            sdk::scePerfArmPmonReset(SELF)
        })?;
        // An unsuccessful start is also stopped during cleanup, retaining the
        // reservation if the native state cannot be confirmed stopped.
        owner.active = true;
        Error::check("scePerfArmPmonStart", unsafe {
            sdk::scePerfArmPmonStart(SELF)
        })?;
        Ok(owner)
    }

    /// Read counter 31 without resetting or stopping it, locking, or allocating.
    /// A native failure retains its operation and original error code.
    pub fn read(&self) -> Result<PmonSample, Error> {
        if !self.active {
            return Err(Error {
                operation: "PMON owner is stopped",
                code: -1,
            });
        }
        let mut raw_cycle_ticks = 0;
        // SAFETY: exclusive stack output; SELF denotes this !Send owner's
        // running thread. The SDK retains no output pointer after the call.
        Error::check("scePerfArmPmonGetCounterValue", unsafe {
            sdk::scePerfArmPmonGetCounterValue(SELF, CYCLE_COUNTER, &mut raw_cycle_ticks)
        })?;
        Ok(PmonSample {
            thread_id: self.thread_id,
            raw_cycle_ticks,
        })
    }

    /// Stop this thread's PMON and release its PERF module lease.
    /// Stop failure keeps the reservation and lease for retry. Unload failure
    /// retains only the module lease; repeated calls retry the remaining work.
    pub fn stop(&mut self) -> Result<(), Error> {
        if self.active {
            // SAFETY: this owner remains on the thread whose counters it started.
            Error::check("scePerfArmPmonStop", unsafe {
                sdk::scePerfArmPmonStop(SELF)
            })?;
            self.active = false;
        }
        if self.reserved {
            OWNED.with(|owned| owned.set(false));
            self.reserved = false;
        }
        if self.module {
            release_module()?;
            self.module = false;
        }
        Ok(())
    }
}

impl Drop for Pmon {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            std::eprintln!("native PMON cleanup: {error}");
        }
    }
}
