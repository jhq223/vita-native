//! One process-wide owner for the GXM device.
use std::sync::atomic::AtomicBool;
pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);
