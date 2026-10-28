//! Exclusive NV12 output blocks sealed into immutable cross-thread storage.
use crate::app::memory::{MemoryBlock, MemoryBudget, MemoryKind, MemoryLayout, MemoryReservation};
use std::{
    prelude::v1::*,
    sync::{Arc, Mutex, Weak},
};

struct PoolState {
    free: Vec<MemoryBlock>,
    // Includes physical blocks currently being allocated outside this mutex.
    allocated: usize,
}
struct Metadata {
    _ticket: Mutex<Box<dyn MemoryReservation>>,
}
struct PoolInner {
    state: Mutex<PoolState>,
    budget: Arc<dyn MemoryBudget>,
    layout: Layout,
    blocks: usize,
    metadata: Arc<Metadata>,
}
// A reserved slot counts against admission until allocation commits or unwinds.
struct AllocationSlot<'a> {
    pool: &'a PoolInner,
    committed: bool,
}
impl Drop for AllocationSlot<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.pool.state.lock().unwrap().allocated -= 1;
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub size: [u32; 2],
    pub strides: [u32; 2],
    pub y_bytes: usize,
    pub bytes: usize,
}
impl Layout {
    fn new(size: [u32; 2]) -> Result<Self, String> {
        if size.iter().any(|n| *n == 0 || !n.is_multiple_of(2)) || size[0] > 960 || size[1] > 544 {
            return Err("owned NV12 requires even dimensions up to 960x544".into());
        }
        let strides = [
            size[0].next_multiple_of(8),
            (size[0] / 2).next_multiple_of(8) * 2,
        ];
        let y_bytes = strides[0] as usize * size[1] as usize;
        let bytes = y_bytes + strides[1] as usize * size[1] as usize / 2;
        Ok(Self {
            size,
            strides,
            y_bytes,
            bytes,
        })
    }
    fn memory(self) -> Result<MemoryLayout, String> {
        MemoryLayout::new(MemoryKind::PhysicalUncached, 4096, self.bytes as u32)
    }
}
/// A bounded worker-side pool. Every physical block carries its original budget
/// reservation while writable, sealed, mapped, submitted or idle in this pool.
pub struct Nv12Pool(Arc<PoolInner>);
impl Nv12Pool {
    /// Bound the pool by rounded physical capacity (currently 1 MiB per block).
    pub fn new(
        size: [u32; 2],
        max_bytes: u64,
        budget: Arc<dyn MemoryBudget>,
    ) -> Result<Self, String> {
        let layout = Layout::new(size)?;
        let blocks =
            usize::try_from(max_bytes / layout.memory()?.bytes()).map_err(|e| e.to_string())?;
        if blocks == 0 || blocks > 64 {
            return Err("owned NV12 pool requires 1..64 physical blocks".into());
        }
        let metadata_bytes = std::mem::size_of::<Metadata>()
            + 2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<PoolInner>()
            + 2 * std::mem::size_of::<usize>()
            + blocks
                * (std::mem::size_of::<MemoryBlock>()
                    + std::mem::size_of::<Sealed>()
                    + 2 * std::mem::size_of::<usize>());
        let metadata = budget.reserve(crate::memory::MemoryClass::Heap, metadata_bytes as u64)?;
        Ok(Self(Arc::new(PoolInner {
            metadata: Arc::new(Metadata {
                _ticket: Mutex::new(metadata),
            }),
            state: Mutex::new(PoolState {
                free: Vec::with_capacity(blocks),
                allocated: 0,
            }),
            budget,
            layout,
            blocks,
        })))
    }
    /// Acquire one exclusively writable block. None means every admitted slot is
    /// retained by consumers or an in-progress allocation. Allocation and initial
    /// clearing run outside the recycle lock; failures return the reserved slot.
    /// The decoder must obtain a block before polling another picture.
    pub fn acquire(&self) -> Result<Option<Nv12Buffer>, String> {
        let available = {
            let mut state = self.0.state.lock().unwrap();
            let available = state.free.pop();
            if available.is_none() {
                if state.allocated == self.0.blocks {
                    return Ok(None);
                }
                state.allocated += 1;
            }
            available
        };
        let block = if let Some(block) = available {
            block
        } else {
            let mut slot = AllocationSlot {
                pool: &self.0,
                committed: false,
            };
            let block = MemoryBlock::allocate(self.0.layout.memory()?, self.0.budget.as_ref())?;
            // Plane slices include row padding, which is initialized once.
            unsafe {
                std::ptr::write_bytes(block.pointer().cast::<u8>(), 0, self.0.layout.bytes);
            }
            slot.committed = true;
            block
        };
        Ok(Some(Nv12Buffer {
            block: Some(block),
            layout: self.0.layout,
            pool: Arc::downgrade(&self.0),
            metadata: self.0.metadata.clone(),
        }))
    }
}
/// Exclusive output storage. Only the decoder can write before sealing.
pub struct Nv12Buffer {
    block: Option<MemoryBlock>,
    layout: Layout,
    pool: Weak<PoolInner>,
    metadata: Arc<Metadata>,
}
impl Nv12Buffer {
    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }
    pub(crate) fn planes_mut(&mut self) -> (&mut [u8], &mut [u8]) {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                self.block.as_ref().unwrap().pointer().cast::<u8>(),
                self.layout.bytes,
            )
        };
        bytes.split_at_mut(self.layout.y_bytes)
    }
    /// Consume all write access. Shared readers and GPU imports keep this exact
    /// allocation alive; its final reader returns it to the originating pool.
    pub fn seal(mut self) -> SharedNv12 {
        SharedNv12(Arc::new(Sealed {
            pointer: self.block.as_ref().unwrap().pointer() as usize,
            block: Mutex::new(self.block.take()),
            layout: self.layout,
            pool: self.pool.clone(),
            _metadata: self.metadata.clone(),
        }))
    }
}
impl Drop for Nv12Buffer {
    fn drop(&mut self) {
        recycle(self.block.take(), &self.pool);
    }
}
fn recycle(block: Option<MemoryBlock>, pool: &Weak<PoolInner>) {
    if let (Some(block), Some(pool)) = (block, pool.upgrade()) {
        pool.state.lock().unwrap().free.push(block);
    }
}
struct Sealed {
    pointer: usize,
    // Mutex makes the Send accounting owner Sync without declaring its internals
    // thread-safe. It is touched only on final drop, never when reading planes.
    block: Mutex<Option<MemoryBlock>>,
    layout: Layout,
    pool: Weak<PoolInner>,
    _metadata: Arc<Metadata>,
}
impl Drop for Sealed {
    fn drop(&mut self) {
        recycle(self.block.get_mut().unwrap().take(), &self.pool);
    }
}
/// Immutable, naturally Send + Sync NV12 storage. It contains no device handle;
/// importing and retiring GXM mappings remains the rendering thread's job.
#[derive(Clone)]
pub struct SharedNv12(Arc<Sealed>);
impl std::fmt::Debug for SharedNv12 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedNv12")
            .field("size", &self.size())
            .finish()
    }
}
impl SharedNv12 {
    /// Visible luma extent.
    pub fn size(&self) -> [u32; 2] {
        self.0.layout.size
    }
    /// Physical Y and interleaved UV byte strides.
    pub fn strides(&self) -> [u32; 2] {
        self.0.layout.strides
    }
    /// Immutable initialized plane, including row padding.
    pub fn plane(&self, index: usize) -> &[u8] {
        let l = self.0.layout;
        let (offset, len) = match index {
            0 => (0, l.y_bytes),
            1 => (l.y_bytes, l.bytes - l.y_bytes),
            _ => return &[],
        };
        unsafe { std::slice::from_raw_parts((self.0.pointer as *const u8).add(offset), len) }
    }
    #[cfg(feature = "graphics")]
    pub(crate) fn id(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    #[cfg(feature = "graphics")]
    pub(crate) fn pointer(&self) -> usize {
        self.0.pointer
    }
    #[cfg(feature = "graphics")]
    pub(crate) fn capacity(&self) -> usize {
        self.0.layout.memory().expect("validated layout").bytes() as usize
    }
}
