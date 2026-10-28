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

/// Physical allocation class used to select the caller's ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryClass {
    /// An allocation within the process heap; arena commitment is counted separately.
    Heap,
    /// A separately allocated ordinary MAIN kernel block.
    MainKernel,
    /// A separately allocated physically contiguous MAIN kernel block.
    ContiguousKernel,
}

/// Supplies reservations from the caller's existing physical memory ledger.
pub trait MemoryBudget: Send + Sync {
    /// Reserve actual rounded bytes before native allocation begins.
    fn reserve(
        &self,
        class: MemoryClass,
        bytes: u64,
    ) -> Result<Box<dyn MemoryReservation>, MemoryBudgetError>;
}

/// Allocation-free physical MAIN commitment shared with the optional process
/// allocator. Logical subsystem reservations remain independent. Keep this
/// unique owner until successful kernel release; retain it on native failure.
#[cfg(any(
    all(target_os = "vita", feature = "graphics"),
    all(test, feature = "process-allocator")
))]
#[derive(Default)]
pub(crate) struct MainCommit(u32);
#[cfg(any(
    all(target_os = "vita", feature = "graphics"),
    all(test, feature = "process-allocator")
))]
impl MainCommit {
    pub(crate) fn reserve(bytes: usize) -> Option<Self> {
        #[cfg(all(feature = "process-allocator", any(target_os = "vita", test)))]
        {
            let bytes = u32::try_from(bytes).ok()?;
            crate::allocator::try_reserve_main(bytes).then_some(Self(bytes))
        }
        #[cfg(not(all(feature = "process-allocator", any(target_os = "vita", test))))]
        {
            let _ = bytes;
            Some(Self(0))
        }
    }
}
#[cfg(any(
    all(target_os = "vita", feature = "graphics"),
    all(test, feature = "process-allocator")
))]
impl Drop for MainCommit {
    fn drop(&mut self) {
        #[cfg(all(feature = "process-allocator", any(target_os = "vita", test)))]
        if self.0 != 0 {
            crate::allocator::refund_main(self.0);
        }
        #[cfg(not(all(feature = "process-allocator", any(target_os = "vita", test))))]
        {
            let _ = self.0;
        }
    }
}
