//! Budgeted native heap and physically contiguous allocations.
//!
//! Layout admission exposes actual rounded bytes before allocation. A reservation
//! remains owned until native storage has been released, including SDK callbacks
//! that free storage on another native thread.
#[cfg(target_os = "vita")]
use std::eprintln;
#[cfg(target_os = "vita")]
use std::fmt;
#[cfg(target_os = "vita")]
use std::format;
use std::prelude::v1::*;

/// An opaque owned accounting lease, released through its ordinary destructor.
pub trait MemoryReservation: Send {}
impl<T: Send> MemoryReservation for T {}

/// Supplies reservations from the caller's existing physical memory ledger.
pub trait MemoryBudget: Send + Sync {
    /// Reserve actual rounded bytes before native allocation begins.
    fn reserve(&self, bytes: u64) -> Result<Box<dyn MemoryReservation>, String>;
}

/// Native storage class required by the allocating subsystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Ordinary aligned process heap, suitable for AvPlayer internal state.
    Heap,
    /// CPU-readable, uncached physical memory used by AvPlayer output surfaces.
    PhysicalUncached,
}

/// Validated physical allocation extent and callback payload alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLayout {
    kind: MemoryKind,
    payload: usize,
    alignment: usize,
    bytes: usize,
}
impl MemoryLayout {
    /// Admit a positive SDK allocation. Physical blocks use native 1 MiB
    /// granularity; stronger alignment is satisfied inside the owned block.
    pub fn new(kind: MemoryKind, alignment: u32, payload: u32) -> Result<Self, String> {
        if alignment != 0 && !alignment.is_power_of_two() {
            return Err("native allocation alignment must be a power of two".into());
        }
        let alignment = (alignment as usize).max(match kind {
            MemoryKind::Heap => 8,
            _ => 1,
        });
        if payload == 0 || !alignment.is_power_of_two() {
            return Err(
                "native allocation requires a positive size and power-of-two alignment".into(),
            );
        }
        let payload = payload as usize;
        let granularity = match kind {
            MemoryKind::Heap => alignment,
            MemoryKind::PhysicalUncached => 1 << 20,
        };
        let slack = if kind == MemoryKind::PhysicalUncached {
            alignment.saturating_sub(granularity)
        } else {
            0
        };
        let bytes = payload
            .checked_add(slack)
            .and_then(|n| n.checked_add(granularity - 1))
            .map(|n| n & !(granularity - 1))
            .filter(|&n| n <= i32::MAX as usize)
            .ok_or("native allocation extent exceeds this device's addressable range")?;
        Ok(Self {
            kind,
            payload,
            alignment,
            bytes,
        })
    }
    /// Actual bytes charged and passed to the native allocator.
    pub fn bytes(self) -> u64 {
        self.bytes as u64
    }
    /// Requested content bytes, excluding storage rounding and alignment slack.
    pub fn payload_bytes(self) -> usize {
        self.payload
    }
    /// Requested native storage class.
    pub fn kind(self) -> MemoryKind {
        self.kind
    }
    /// Alignment of the content pointer returned to the SDK.
    pub fn alignment(self) -> usize {
        self.alignment
    }

    #[cfg(any(target_os = "vita", test))]
    pub(crate) fn offset(self, base: usize) -> Option<usize> {
        let mask = self.alignment - 1;
        let offset = (base.checked_add(mask)? & !mask).checked_sub(base)?;
        let end = offset.checked_add(self.payload)?;
        base.checked_add(self.bytes)?;
        (base != 0 && end <= self.bytes).then_some(offset)
    }
}

/// An owned native allocation. Its SDK pointer remains private to this crate.
/// Native storage is freed before its caller-supplied accounting reservation.
#[cfg(target_os = "vita")]
pub struct MemoryBlock {
    base: usize,
    pointer: usize,
    block: Option<i32>,
    layout: MemoryLayout,
    reservation: Option<Box<dyn MemoryReservation>>,
}
#[cfg(target_os = "vita")]
impl fmt::Debug for MemoryBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryBlock")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}
#[cfg(target_os = "vita")]
impl MemoryBlock {
    /// Reserve and allocate an admitted extent; failure releases the reservation.
    pub fn allocate(layout: MemoryLayout, budget: &dyn MemoryBudget) -> Result<Self, String> {
        use super::sdk;
        use std::{ffi::c_void, ptr};
        let reservation = budget.reserve(layout.bytes())?;
        let (base, block) = match layout.kind {
            MemoryKind::Heap => {
                unsafe extern "C" {
                    fn memalign(alignment: usize, size: usize) -> *mut c_void;
                }
                let pointer = unsafe { memalign(layout.alignment, layout.bytes) };
                if pointer.is_null() {
                    return Err(format!(
                        "native heap allocation failed: {} bytes",
                        layout.bytes
                    ));
                }
                unsafe {
                    ptr::write_bytes(pointer.cast::<u8>(), 0, layout.bytes);
                }
                (pointer as usize, None)
            }
            MemoryKind::PhysicalUncached => {
                let block = unsafe {
                    sdk::sceKernelAllocMemBlock(
                        c"video-surface".as_ptr(),
                        sdk::SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_PHYCONT_NC_RW,
                        layout.bytes as u32,
                        ptr::null_mut(),
                    )
                };
                if block < 0 {
                    return Err(format!("allocate physical video block: 0x{block:08x}"));
                }
                let mut base = ptr::null_mut();
                let result = unsafe { sdk::sceKernelGetMemBlockBase(block, &mut base) };
                if result < 0 || base.is_null() {
                    let freed = unsafe { sdk::sceKernelFreeMemBlock(block) };
                    if freed < 0 {
                        std::mem::forget(reservation);
                    }
                    return Err(format!("physical video block address: 0x{result:08x}"));
                }
                (base as usize, Some(block))
            }
        };
        let mut owned = Self {
            base,
            pointer: base,
            block,
            layout,
            reservation: Some(reservation),
        };
        let offset = layout
            .offset(base)
            .ok_or("native allocation does not satisfy the requested alignment")?;
        owned.pointer += offset;
        Ok(owned)
    }
    #[cfg(feature = "video")]
    pub(crate) fn pointer(&self) -> *mut std::ffi::c_void {
        self.pointer as *mut _
    }
    #[cfg(feature = "video")]
    pub(crate) fn contains(&self, pointer: usize, bytes: usize) -> bool {
        pointer >= self.pointer
            && pointer
                .checked_add(bytes)
                .is_some_and(|end| end <= self.pointer + self.layout.payload)
    }
    #[cfg(feature = "video")]
    pub(crate) fn capacity(&self) -> usize {
        self.layout.payload
    }
    /// Release storage once. A native free failure retains this allocation and
    /// reservation so the owner can report the failure and retry explicitly.
    pub fn release(&mut self) -> Result<(), String> {
        if self.reservation.is_none() {
            return Ok(());
        }
        if let Some(block) = self.block {
            let result = unsafe { super::sdk::sceKernelFreeMemBlock(block) };
            if result < 0 {
                return Err(format!("free physical video block: 0x{result:08x}"));
            }
        } else {
            unsafe extern "C" {
                fn free(pointer: *mut std::ffi::c_void);
            }
            unsafe {
                free(self.base as *mut _);
            }
        }
        self.reservation = None;
        Ok(())
    }
}
#[cfg(target_os = "vita")]
impl Drop for MemoryBlock {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            // The kernel still owns this block. Keep its reservation alive rather
            // than reporting physical memory that has not actually been freed.
            if let Some(reservation) = self.reservation.take() {
                std::mem::forget(reservation);
            }
            eprintln!("{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_rounding_and_alignment_are_charged_before_allocation() {
        let heap = MemoryLayout::new(MemoryKind::Heap, 16, 17).unwrap();
        assert_eq!(heap.bytes(), 32);
        for alignment in [1, 16, 4096, 1 << 20, 2 << 20, 4 << 20] {
            let layout =
                MemoryLayout::new(MemoryKind::PhysicalUncached, alignment, 1_566_720).unwrap();
            for step in 1..16 {
                let base = 0x8000_0000 + step * (1 << 20);
                let offset = layout.offset(base).unwrap();
                assert_eq!((base + offset) % alignment as usize, 0);
                assert!(offset + layout.payload_bytes() <= layout.bytes() as usize);
            }
        }
        assert!(MemoryLayout::new(MemoryKind::Heap, 3, 1).is_err());
        assert!(MemoryLayout::new(MemoryKind::PhysicalUncached, 0, 0).is_err());
        assert!(MemoryLayout::new(MemoryKind::PhysicalUncached, 1 << 31, 1).is_err());
    }
}
