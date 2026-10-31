//! Native power configuration and ordered notifications.
use super::{Error, sdk, time, wake::Wake};
use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    prelude::v1::*,
    rc::Rc,
    sync::{Arc, Mutex},
};

static CONFIGURATION: Mutex<()> = Mutex::new(());

/// Sony power configurations, expressed without a fixed GPU frequency in MHz.
///
/// The Power Service Overview, "Power Configuration Control", defines these
/// modes. Applications choose a mode explicitly and may observe the resulting
/// clocks through `clock_frequencies` when `profiling` is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationMode {
    /// Normal GPU clock with WLAN/COM available; the process startup mode.
    A,
    /// High GPU clock with WLAN/COM unavailable.
    B,
    /// High GPU clock with WLAN/COM available. Screen brightness is limited
    /// and the camera is unavailable. PlayStation TV has no brightness limit.
    C,
}

/// Request a Sony power configuration through `scePowerSetConfigurationMode`.
///
/// Calls through this function are serialized because the SDK operation is not
/// multithread safe. Call during application setup or another explicit host
/// transition; execution time varies by device. The SDK returns zero on success
/// and a negative error code on failure, retained in [`Error`].
///
/// The selected mode controls the GPU clock and the services documented by
/// [`ConfigurationMode`]. Sony does not specify a GPU frequency in MHz. Callers
/// measuring a configuration change should observe all clocks before and after
/// the request and record the request result with those observations.
pub fn request_configuration(mode: ConfigurationMode) -> Result<(), Error> {
    // This lock contains no data; recovering a poisoned guard still serializes
    // the native call without asserting that an earlier request succeeded.
    let _guard = CONFIGURATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let conf = match mode {
        ConfigurationMode::A => sdk::SCE_POWER_CONFIGURATION_MODE_A,
        ConfigurationMode::B => sdk::SCE_POWER_CONFIGURATION_MODE_B,
        ConfigurationMode::C => sdk::SCE_POWER_CONFIGURATION_MODE_C,
    };
    // SAFETY: the enum supplies a documented mode and the lock serializes calls
    // to the SDK operation. No pointer or borrowed storage crosses the boundary.
    Error::check("scePowerSetConfigurationMode", unsafe {
        sdk::scePowerSetConfigurationMode(conf as i32)
    })
    .map(|_| ())
}

/// Request the ARM CPU clock frequency in MHz through VitaSDK's power API.
///
/// Firmware validates supported frequencies; failures retain the native error
/// code. Requests are serialized with power configuration changes. A system
/// power plugin may override the request, so profiling callers should observe
/// the resulting frequency with `clock_frequencies`.
pub fn request_arm_clock_frequency(megahertz: i32) -> Result<(), Error> {
    let _guard = CONFIGURATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: this scalar request has no pointers or retained ownership. The
    // firmware validates the frequency and calls through this API are serialized.
    Error::check("scePowerSetArmClockFrequency", unsafe {
        sdk::scePowerSetArmClockFrequency(megahertz)
    })
    .map(|_| ())
}

/// Read-only clock observations. Each failed query retains its native error.
#[cfg(feature = "profiling")]
pub struct ClockFrequencies {
    /// Reported ARM clock in MHz.
    pub arm_mhz: Result<u32, Error>,
    /// Reported GPU clock in MHz.
    pub gpu_mhz: Result<u32, Error>,
    /// Reported bus clock in MHz.
    pub bus_mhz: Result<u32, Error>,
    /// Reported GPU crossbar clock in MHz.
    pub gpu_xbar_mhz: Result<u32, Error>,
}

/// Read the current hardware clock frequencies.
/// Units follow VitaSDK's psp2/power.h query contract.
#[cfg(feature = "profiling")]
pub fn clock_frequencies() -> ClockFrequencies {
    let read = |operation, value| Error::check(operation, value).map(|value| value as u32);
    // SAFETY: scalar ScePower queries, with no pointers or retained ownership.
    unsafe {
        ClockFrequencies {
            arm_mhz: read(
                "scePowerGetArmClockFrequency",
                sdk::scePowerGetArmClockFrequency(),
            ),
            gpu_mhz: read(
                "scePowerGetGpuClockFrequency",
                sdk::scePowerGetGpuClockFrequency(),
            ),
            bus_mhz: read(
                "scePowerGetBusClockFrequency",
                sdk::scePowerGetBusClockFrequency(),
            ),
            gpu_xbar_mhz: read(
                "scePowerGetGpuXbarClockFrequency",
                sdk::scePowerGetGpuXbarClockFrequency(),
            ),
        }
    }
}

/// An actual application power transition reported by the SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerKind {
    /// The application is entering suspend.
    Suspending,
    /// The application has returned from suspend.
    Resumed,
}
/// Ordered transition timestamped on its native callback.
#[derive(Clone, Copy, Debug)]
pub struct PowerEvent {
    /// Native application transition.
    pub kind: PowerKind,
    /// Platform monotonic timestamp at notification, in microseconds.
    pub monotonic_micros: u64,
}
struct State {
    events: [Option<PowerEvent>; 16],
    read: usize,
    len: usize,
    overflow: bool,
    suspended: bool,
    wake: Arc<Wake>,
}
impl State {
    fn push(&mut self, kind: PowerKind) {
        let suspended = kind == PowerKind::Suspending;
        if self.suspended == suspended {
            return;
        }
        self.suspended = suspended;
        if self.len == self.events.len() {
            self.overflow = true;
        } else {
            self.events[(self.read + self.len) % self.events.len()] = Some(PowerEvent {
                kind,
                monotonic_micros: time::monotonic_micros(),
            });
            self.len += 1;
        }
        let _ = self.wake.signal();
    }
}
// SAFETY: callbacks execute on the registration thread, during callback-aware
// kernel calls. The Box outlives registration; there are no simultaneous Rust
// references to State during the native dispatch.
unsafe extern "C" fn callback(
    _id: i32,
    _count: i32,
    flags: i32,
    data: *mut std::ffi::c_void,
) -> i32 {
    let state = unsafe { &mut *(*data.cast::<UnsafeCell<State>>()).get() };
    let flags = flags as u32;
    if flags
        & (sdk::SCE_POWER_CB_SYSTEM_SUSPEND
            | sdk::SCE_POWER_CB_APP_SUSPEND
            | sdk::SCE_POWER_CB_THERMAL_SUSPEND
            | sdk::SCE_POWER_CB_LOW_BATTERY_SUSPEND)
        != 0
    {
        state.push(PowerKind::Suspending);
    }
    if flags & (sdk::SCE_POWER_CB_APP_RESUME | sdk::SCE_POWER_CB_AFTER_SYSTEM_RESUME) != 0 {
        state.push(PowerKind::Resumed);
    }
    0
}
/// Thread-affine callback registration with a bounded 16-event retained queue.
pub struct PowerEvents {
    callback: i32,
    state: Option<Box<UnsafeCell<State>>>,
    _thread: PhantomData<Rc<()>>,
}
impl PowerEvents {
    /// Register on the calling thread. That thread dispatches callbacks through
    /// `poll` and callback-aware `Wake::wait` calls until this owner is dropped.
    pub fn new(wake: Arc<Wake>) -> Result<Self, Error> {
        let state = Box::new(UnsafeCell::new(State {
            events: [None; 16],
            read: 0,
            len: 0,
            overflow: false,
            suspended: false,
            wake,
        }));
        // SAFETY: pinned Box address retained until after unregister/delete; callback ABI is exact.
        let id = Error::check("sceKernelCreateCallback", unsafe {
            sdk::sceKernelCreateCallback(
                c"app-power".as_ptr(),
                0,
                Some(callback),
                (&*state as *const UnsafeCell<State>).cast_mut().cast(),
            )
        })?;
        if let Err(error) = Error::check("scePowerRegisterCallback", unsafe {
            sdk::scePowerRegisterCallback(id)
        }) {
            if unsafe { sdk::sceKernelDeleteCallback(id) } < 0 {
                std::mem::forget(state);
            }
            return Err(error);
        }
        Ok(Self {
            callback: id,
            state: Some(state),
            _thread: PhantomData,
        })
    }
    /// Dispatch pending native callbacks, then return the next real power event.
    pub fn poll(&mut self) -> Result<Option<PowerEvent>, Error> {
        // SAFETY: !Send/!Sync keeps checking and the registered callback on their owner.
        Error::check("sceKernelCheckCallback", unsafe {
            sdk::sceKernelCheckCallback()
        })?;
        // SAFETY: no callback-aware native call occurs while State is borrowed.
        let state = unsafe { &mut *self.state.as_ref().expect("registered power context").get() };
        if state.overflow {
            return Err(Error {
                operation: "power event queue overflow",
                code: -1,
            });
        }
        if state.len == 0 {
            return Ok(None);
        }
        let event = state.events[state.read].take();
        state.read = (state.read + 1) % state.events.len();
        state.len -= 1;
        Ok(event)
    }
}
impl Drop for PowerEvents {
    fn drop(&mut self) {
        // SAFETY: disable the callback before freeing its stable context.
        unsafe {
            sdk::scePowerUnregisterCallback(self.callback);
            if sdk::sceKernelDeleteCallback(self.callback) < 0
                && let Some(state) = self.state.take()
            {
                std::mem::forget(state);
            }
        }
    }
}
