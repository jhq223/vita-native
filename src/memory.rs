//! Caller-supplied admission for native allocation ownership.
use std::prelude::v1::*;

/// An opaque owned accounting lease, released through its ordinary destructor.
pub trait MemoryReservation: Send {}
impl<T: Send> MemoryReservation for T {}

/// Supplies reservations from the caller's existing physical memory ledger.
pub trait MemoryBudget: Send + Sync {
    /// Reserve actual rounded bytes before native allocation begins.
    fn reserve(&self, bytes: u64) -> Result<Box<dyn MemoryReservation>, String>;
}
