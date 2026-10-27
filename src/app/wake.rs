//! A retained kernel wake token: one waiter, any number of notifiers.
use super::{Error, sdk};
use std::{prelude::v1::*, sync::Arc, time::Duration};

/// Owned, thread-safe kernel event flag retaining one coalesced notification.
pub struct Wake(i32);
/// Outcome of a native relative wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// A retained notification was consumed.
    Notified,
    /// The relative timeout elapsed.
    TimedOut,
}
impl Wake {
    /// Create an unsignaled native event shared by its owner and notifiers.
    pub fn new() -> Result<Arc<Self>, Error> {
        // SAFETY: constant name; SDK does not retain the optional parameter pointer.
        let id = Error::check("sceKernelCreateEventFlag", unsafe {
            sdk::sceKernelCreateEventFlag(c"worker-wake".as_ptr(), 0, 0, std::ptr::null_mut())
        })?;
        Ok(Arc::new(Self(id)))
    }
    /// Retains a notification delivered before the next wait.
    pub fn signal(&self) -> Result<(), Error> {
        // SAFETY: Arc retains this event flag through each concurrent operation.
        Error::check("sceKernelSetEventFlag", unsafe {
            sdk::sceKernelSetEventFlag(self.0, 1)
        })?;
        Ok(())
    }
    /// Consume one retained signal or wait until notification/relative timeout.
    /// Callback-aware waiting also dispatches this thread's native power callbacks.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<WaitResult, Error> {
        let mut micros = timeout.map_or(0, |v| {
            v.as_nanos().div_ceil(1000).min(u128::from(u32::MAX)) as u32
        });
        let limit = if timeout.is_some() {
            &mut micros
        } else {
            std::ptr::null_mut()
        };
        // SAFETY: event is owned, timeout is local, matching and clearing are atomic.
        let result = unsafe {
            sdk::sceKernelWaitEventFlagCB(
                self.0,
                1,
                sdk::SCE_EVENT_WAITCLEAR_PAT,
                std::ptr::null_mut(),
                limit,
            )
        };
        if result as u32 == sdk::SCE_KERNEL_ERROR_WAIT_TIMEOUT {
            return Ok(WaitResult::TimedOut);
        }
        Error::check("sceKernelWaitEventFlagCB", result)?;
        Ok(WaitResult::Notified)
    }
}
impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: the final Arc is dropped only after all waiters/notifiers release it.
        unsafe {
            sdk::sceKernelDeleteEventFlag(self.0);
        }
    }
}
