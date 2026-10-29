//! Caller-supplied admission for native allocation ownership.
use std::prelude::v1::*;

#[cfg(target_os = "vita")]
mod kernel;
#[cfg(target_os = "vita")]
pub use kernel::free_memory;

/// Kernel-reported currently free capacities by allocation class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeMemory {
    /// Ordinary user MAIN available for new kernel blocks.
    pub main_bytes: u64,
    /// CDRAM available for new kernel blocks.
    pub cdram_bytes: u64,
    /// Physically contiguous MAIN available for new kernel blocks.
    pub contiguous_bytes: u64,
}

/// A kernel memory query failure with its original SDK result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryError {
    /// Native operation that failed.
    pub operation: &'static str,
    /// Original signed native result.
    pub code: i32,
}
impl MemoryError {
    #[cfg(target_os = "vita")]
    fn check(operation: &'static str, code: i32) -> Result<(), Self> {
        if code < 0 {
            Err(Self { operation, code })
        } else {
            Ok(())
        }
    }
}
impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:#010x}", self.operation, self.code)
    }
}
impl std::error::Error for MemoryError {}

/// Physical page commitment, separate from suballocated payload accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhysicalUsage {
    /// Managed capacity observed before concurrent consumers start.
    pub capacity_bytes: u64,
    /// Rounded blocks and reservations awaiting kernel allocation or release.
    pub committed_bytes: u64,
}
impl PhysicalUsage {
    /// Space currently available for a new physical reservation.
    pub fn available_bytes(self) -> u64 {
        self.capacity_bytes.saturating_sub(self.committed_bytes)
    }
}

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
