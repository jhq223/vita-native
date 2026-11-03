//! One SDK implementation for app and graphics thread execution observations.

/// An observation of the issuing thread, with the kernel's microsecond counter.
#[derive(Clone, Copy, Debug)]
pub struct ThreadRuntime {
    /// Native thread identity; compare only samples of the same thread.
    pub thread_id: i32,
    /// Raw kernel-reported execution microseconds, excluding descheduled time.
    /// Updates may settle at scheduling boundaries rather than at every query.
    pub run_micros: u64,
    /// Current scheduling priority, as reported by the SDK.
    pub priority: i32,
    /// Current CPU affinity mask, as reported by the SDK.
    pub affinity: i32,
    /// CPU executing this thread when the kernel took the observation.
    pub cpu: i32,
}

pub(crate) fn read() -> Result<ThreadRuntime, (&'static str, i32)> {
    use vitasdk_sys::*;
    // SAFETY: scalar query of the caller, with no retained handle ownership.
    let thread_id = unsafe { sceKernelGetThreadId() };
    if thread_id < 0 {
        return Err(("sceKernelGetThreadId", thread_id));
    }
    // SAFETY: complete exclusive SDK-sized output; the synchronous query keeps
    // no pointer. Querying the explicit caller ID also identifies paired reads.
    let mut info: SceKernelThreadInfo = unsafe { core::mem::zeroed() };
    info.size = core::mem::size_of_val(&info) as u32;
    let code = unsafe { sceKernelGetThreadInfo(thread_id, &mut info) };
    if code < 0 {
        return Err(("sceKernelGetThreadInfo", code));
    }
    Ok(ThreadRuntime {
        thread_id,
        run_micros: info.runClocks,
        priority: info.currentPriority,
        affinity: info.currentCpuAffinityMask,
        cpu: info.currentCpuId,
    })
}
