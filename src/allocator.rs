//! Growing process heap over independent system mspaces. Enabling the feature
//! changes the executable's C/newlib/Rust allocation ABI. Retain the link anchor
//! before CRT startup and configure capacity before graphics or session/media workers start.

/// Current ownership, including arena headers, rounding and failed releases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct HeapStats {
    /// Kernel bytes held by CPU arenas, including the warm bootstrap arena.
    pub cpu_committed: u32,
    /// Maximum simultaneous CPU arena commitment.
    pub cpu_peak: u32,
    /// CPU arenas and separately admitted ordinary MAIN graphics blocks.
    pub main_committed: u32,
    /// Shared physical admission ceiling observed before device construction.
    pub main_limit: u32,
    /// Kernel blocks still owned, including failed-release blocks.
    pub arena_count: u32,
    /// Live C/Rust allocation objects (an in-place realloc retains one object).
    pub live_allocations: u32,
    /// Retained bootstrap capacity; additional empty arenas are released.
    pub warm_bytes: u32,
    /// Physical release failures, each retaining its original ownership.
    pub release_failures: u32,
    /// In-use bytes reported by the system mspaces.
    pub in_use: u32,
    /// Last original kernel failure, or zero before any failure.
    pub last_error: i32,
}
unsafe extern "C" {
    fn vita_native_process_heap_anchor();
    fn vn_heap_configure() -> i32;
    fn vn_main_capacity() -> u32;
    fn vn_main_committed() -> u32;
    fn vn_heap_stats(out: *mut HeapStats) -> i32;
    #[cfg(any(all(target_os = "vita", feature = "graphics"), test))]
    fn vn_main_reserve(bytes: u32) -> i32;
    #[cfg(any(all(target_os = "vita", feature = "graphics"), test))]
    fn vn_main_refund(bytes: u32);
}
/// An allocator configuration failure preserving its native code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapError(pub i32);
impl core::fmt::Display for HeapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "process heap: {:#010x}", self.0)
    }
}
impl std::error::Error for HeapError {}
/// Reobserve actual free MAIN plus already committed managed storage. Call once
/// before creating graphics/session/media workers; the retained kernel blocks and loaded program
/// already reduce free MAIN. Subsequent unmanaged SDK/thread allocations may
/// still fail at the kernel boundary. This is independent of VM-local quotas.
pub fn configure_main_budget() -> Result<(), HeapError> {
    // SAFETY: retaining the anchor selects the complete matching C allocation
    // family. CRT initialization precedes this call; it allocates no Rust data.
    unsafe { vita_native_process_heap_anchor() };
    // SAFETY: configuration serializes with all process heap operations and has
    // no callback into Rust. The caller keeps the documented cold-start boundary.
    let code = unsafe { vn_heap_configure() };
    if code < 0 {
        Err(HeapError(code))
    } else {
        Ok(())
    }
}
/// Read shared MAIN counters through allocation-free atomic loads.
/// Capacity is fixed at the configured cold-start boundary; concurrent
/// commitments may change immediately after this observation.
pub fn main_usage() -> crate::memory::PhysicalUsage {
    // SAFETY: these functions only read the allocator's 32-bit atomics.
    unsafe {
        crate::memory::PhysicalUsage {
            capacity_bytes: vn_main_capacity().into(),
            committed_bytes: vn_main_committed().into(),
        }
    }
}

/// Observe actual committed ownership without allocating diagnostic storage.
pub fn stats() -> Result<HeapStats, HeapError> {
    let mut out = HeapStats::default();
    // SAFETY: the output is the C record's exact repr(C) layout and stays live;
    // the allocator copies it under its native mutex before returning.
    let code = unsafe { vn_heap_stats(&mut out) };
    if code < 0 {
        Err(HeapError(code))
    } else {
        Ok(out)
    }
}
#[cfg(any(all(target_os = "vita", feature = "graphics"), test))]
pub(crate) fn try_reserve_main(bytes: u32) -> bool {
    // SAFETY: this primitive touches only allocation-free 32-bit counters; its
    // successful reservation is paired with one MainCommit owner.
    unsafe { vn_main_reserve(bytes) != 0 }
}
#[cfg(any(all(target_os = "vita", feature = "graphics"), test))]
pub(crate) fn refund_main(bytes: u32) {
    // SAFETY: only the unique owner calls this after physical release (or before
    // any kernel allocation succeeds); the matching reservation has not moved.
    unsafe { vn_main_refund(bytes) };
}
// These types/functions remain private to production. Host tests invoke the
// same C allocator with a gated SDK, without replacing the host's own malloc.
#[cfg(test)]
mod tests;
