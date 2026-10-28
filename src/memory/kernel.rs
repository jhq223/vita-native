//! Kernel allocation classes; their free counters are not additive.
use super::{FreeMemory, MemoryError};
/// Query kernel free capacities after already allocated blocks have been deducted.
/// The reported classes need not be disjoint and must not be added together.
#[cfg(target_os = "vita")]
pub fn free_memory() -> Result<FreeMemory, MemoryError> {
    let mut info = vitasdk_sys::SceKernelFreeMemorySizeInfo {
        size: std::mem::size_of::<vitasdk_sys::SceKernelFreeMemorySizeInfo>() as i32,
        size_user: 0,
        size_cdram: 0,
        size_phycont: 0,
    };
    // SAFETY: the complete initialized output record lives through this call.
    MemoryError::check("sceKernelGetFreeMemorySize", unsafe {
        vitasdk_sys::sceKernelGetFreeMemorySize(&mut info)
    })?;
    let nonnegative = |value: i32| {
        u64::try_from(value).map_err(|_| MemoryError {
            operation: "invalid kernel free memory size",
            code: -1,
        })
    };
    Ok(FreeMemory {
        main_bytes: nonnegative(info.size_user)?,
        cdram_bytes: nonnegative(info.size_cdram)?,
        contiguous_bytes: nonnegative(info.size_phycont)?,
    })
}
