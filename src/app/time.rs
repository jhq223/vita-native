//! Monotonic process time and UTC time, in explicit units.
use super::{Error, sdk};
use std::{prelude::v1::*, time::Duration};

#[cfg(feature = "profiling")]
pub use crate::thread_runtime::ThreadRuntime;
#[cfg(feature = "profiling")]
mod pmon;
#[cfg(feature = "profiling")]
pub use pmon::{Pmon, PmonSample};

/// Observe the caller's cumulative CPU execution and current scheduling state.
/// `run_micros` excludes time while descheduled. Query work is included at paired
/// observation boundaries; callers retain their own wall-clock measurements.
#[cfg(feature = "profiling")]
pub fn thread_runtime() -> Result<ThreadRuntime, Error> {
    crate::thread_runtime::read().map_err(|(operation, code)| Error { operation, code })
}

/// Monotonic microseconds; this clock remains distinct from the story clock.
pub fn monotonic_micros() -> u64 {
    // SAFETY: scalar kernel query.
    unsafe { sdk::sceKernelGetProcessTimeWide() }
}
/// UTC milliseconds since 1970-01-01.
pub fn unix_millis() -> Result<i64, Error> {
    let mut tick = sdk::SceRtcTick { tick: 0 };
    // SAFETY: exclusive initialized output, filled by the synchronous SDK call.
    Error::check("sceRtcGetCurrentTick", unsafe {
        sdk::sceRtcGetCurrentTick(&mut tick)
    })?;
    const EPOCH: i128 = 62_135_596_800_000_000;
    i64::try_from((i128::from(tick.tick) - EPOCH) / 1000).map_err(|_| Error {
        operation: "RTC timestamp overflow",
        code: -1,
    })
}
/// Sleep using the kernel's relative timeout, independently of the wall clock.
pub fn sleep(duration: Duration) -> Result<(), Error> {
    let mut remaining = duration.as_nanos().div_ceil(1000);
    while remaining > 0 {
        let micros = remaining.min(u128::from(u32::MAX)) as u32;
        // SAFETY: documented relative microsecond duration.
        Error::check("sceKernelDelayThread", unsafe {
            sdk::sceKernelDelayThread(micros)
        })?;
        remaining -= u128::from(micros);
    }
    Ok(())
}
