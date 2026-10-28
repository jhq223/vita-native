//! Caller-supplied admission for native allocation ownership.
use std::prelude::v1::*;

/// An opaque owned accounting lease, released through its ordinary destructor.
pub trait MemoryReservation: Send {}
impl<T: Send> MemoryReservation for T {}

/// Admission failure with its original explanation and recovery class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryBudgetError {
    /// Releasing an idle owner may make the reservation possible.
    Capacity(String),
    /// A failure that cannot be resolved by freeing capacity.
    Other(String),
}
impl From<MemoryBudgetError> for String {
    fn from(error: MemoryBudgetError) -> Self {
        match error {
            MemoryBudgetError::Capacity(message) | MemoryBudgetError::Other(message) => message,
        }
    }
}

/// Supplies reservations from the caller's existing physical memory ledger.
pub trait MemoryBudget: Send + Sync {
    /// Reserve actual rounded bytes before native allocation begins.
    fn reserve(&self, bytes: u64) -> Result<Box<dyn MemoryReservation>, MemoryBudgetError>;
}
