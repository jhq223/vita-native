//! Display-queue synchronization, independent of offscreen scene completion.
use super::{
    fault::{Fault, sdk},
    types::Result,
};
use std::prelude::v1::*;
#[cfg(all(test, not(target_os = "vita")))]
use tests::native::{SceGxmSyncObject, sceGxmSyncObjectCreate, sceGxmSyncObjectDestroy};
#[cfg(target_os = "vita")]
use vitasdk_sys::{SceGxmSyncObject, sceGxmSyncObjectCreate, sceGxmSyncObjectDestroy};

#[derive(Clone, Copy)]
pub(super) enum SurfaceKind {
    Offscreen,
    Display,
}

pub(super) struct DisplaySync {
    raw: *mut SceGxmSyncObject,
    fault: Fault,
}
impl DisplaySync {
    pub fn new(kind: SurfaceKind, fault: &Fault) -> Result<Self> {
        fault.check()?;
        let mut sync = Self {
            raw: core::ptr::null_mut(),
            fault: fault.clone(),
        };
        // libgxm Overview, Scene Dependencies and Synchronization with Display:
        // scenes are ordered independently; only scanout needs a display sync.
        if matches!(kind, SurfaceKind::Display) {
            // SAFETY: writable output; successful creation transfers ownership.
            sdk("sceGxmSyncObjectCreate", unsafe {
                sceGxmSyncObjectCreate(&mut sync.raw)
            })?;
        }
        Ok(sync)
    }
    pub fn raw(&self) -> *mut SceGxmSyncObject {
        self.raw
    }
}
impl Drop for DisplaySync {
    fn drop(&mut self) {
        if self.raw.is_null() || self.fault.get().is_some() {
            return;
        }
        // SAFETY: framebuffer leases and the display queue drain/detach boundary
        // keep this object owned until fragment and display users have retired.
        // A failure poisons the device before framebuffer storage can be released.
        let _ = self.fault.command("sceGxmSyncObjectDestroy", unsafe {
            sceGxmSyncObjectDestroy(self.raw)
        });
    }
}

#[cfg(all(test, not(target_os = "vita")))]
mod tests {
    use super::*;
    use crate::graphics::Error;
    use std::{cell::RefCell, rc::Rc};

    // Only the two SDK entry points are replaced. Surface selection, ownership,
    // original error codes and the shared terminal fault use production code.
    #[allow(non_snake_case)]
    pub(super) mod native {
        use super::*;
        pub struct SceGxmSyncObject {
            _allocation: u8,
        }
        #[derive(Default)]
        pub struct Calls {
            pub creates: usize,
            pub destroys: usize,
            pub live: usize,
            pub peak: usize,
            pub create_error: i32,
            pub destroy_error: i32,
        }
        thread_local! { pub static CALLS: RefCell<Calls> = RefCell::default(); }
        pub unsafe fn sceGxmSyncObjectCreate(out: *mut *mut SceGxmSyncObject) -> i32 {
            CALLS.with_borrow_mut(|c| {
                c.creates += 1;
                if c.create_error < 0 {
                    return c.create_error;
                }
                // Model the three native slots required by the display contract.
                // Any attempt to allocate for an offscreen surface fails this test.
                if c.live == 3 {
                    return -2141519849;
                }
                c.live += 1;
                c.peak = c.peak.max(c.live);
                unsafe {
                    out.write(Box::into_raw(Box::new(SceGxmSyncObject { _allocation: 0 })));
                }
                0
            })
        }
        pub unsafe fn sceGxmSyncObjectDestroy(raw: *mut SceGxmSyncObject) -> i32 {
            CALLS.with_borrow_mut(|c| {
                c.destroys += 1;
                if c.destroy_error < 0 {
                    return c.destroy_error;
                }
                c.live -= 1;
                unsafe {
                    drop(Box::from_raw(raw));
                }
                0
            })
        }
        pub fn reset() {
            CALLS.with_borrow_mut(|c| *c = Calls::default());
        }
    }

    #[test]
    fn offscreen_churn_and_delayed_readers_keep_exactly_three_display_objects() {
        native::reset();
        let fault = Fault::default();
        let screens: Vec<_> = (0..3)
            .map(|_| Rc::new(DisplaySync::new(SurfaceKind::Display, &fault).unwrap()))
            .collect();
        let display_queue = screens.clone();
        let mut delayed_fragments = Vec::new();
        for _ in 0..2000 {
            let offscreen = Rc::new(DisplaySync::new(SurfaceKind::Offscreen, &fault).unwrap());
            assert!(offscreen.raw().is_null());
            delayed_fragments.push(offscreen.clone());
            drop(offscreen);
        }
        native::CALLS.with_borrow(|c| {
            assert_eq!((c.creates, c.live, c.peak, c.destroys), (3, 3, 3, 0));
        });
        delayed_fragments.clear();
        drop(screens);
        native::CALLS.with_borrow(|c| assert_eq!(c.live, 3));
        drop(display_queue);
        native::CALLS.with_borrow(|c| assert_eq!((c.live, c.destroys), (0, 3)));
        eprintln!(
            "display sync: 2000 offscreen surfaces, 2000 delayed owners, 3 creates/3 peak/3 destroys; previous policy 2003 creates/2003 peak/2003 destroys"
        );
    }

    #[test]
    fn display_creation_failure_preserves_error_and_releases_earlier_objects() {
        native::reset();
        let fault = Fault::default();
        let first = DisplaySync::new(SurfaceKind::Display, &fault).unwrap();
        native::CALLS.with_borrow_mut(|c| c.create_error = -2141519849);
        let failure = DisplaySync::new(SurfaceKind::Display, &fault);
        assert!(matches!(
            failure,
            Err(Error::Sdk("sceGxmSyncObjectCreate", -2141519849))
        ));
        assert!(fault.get().is_none());
        drop(first);
        native::CALLS.with_borrow(|c| assert_eq!((c.live, c.destroys), (0, 1)));
    }

    #[test]
    fn destroy_failure_latches_terminal_fault_and_blocks_more_native_calls() {
        native::reset();
        let fault = Fault::default();
        let sync = DisplaySync::new(SurfaceKind::Display, &fault).unwrap();
        let second = DisplaySync::new(SurfaceKind::Display, &fault).unwrap();
        let raw = [sync.raw(), second.raw()];
        native::CALLS.with_borrow_mut(|c| c.destroy_error = -17);
        drop(sync);
        let failure = fault.get().unwrap();
        assert_eq!(
            (failure.operation, failure.code),
            ("sceGxmSyncObjectDestroy", -17)
        );
        drop(second);
        assert!(matches!(
            DisplaySync::new(SurfaceKind::Display, &fault),
            Err(Error::Terminal(_))
        ));
        native::CALLS.with_borrow(|c| assert_eq!((c.creates, c.live, c.destroys), (2, 2, 1)));
        // Test-only teardown of quarantined mock handles after observing the fault.
        for p in raw {
            unsafe {
                drop(Box::from_raw(p));
            }
        }
    }
}
