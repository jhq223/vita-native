//! Owned kernel blocks and coalescing GPU suballocations.
pub(super) use super::fault::{Fault, sdk};
use super::pressure::{Domain, Pressure};
use super::suballoc::{BlockReservation, Class, FreeList, Lifetime};
use super::{ArenaUsage, Error, types::Result};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ops::Range,
    prelude::v1::*,
    rc::{Rc, Weak},
};
use vitasdk_sys::*;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Cdram,
    Uncached,
    VertexUsse,
    FragmentUsse,
    Driver,
}
impl Kind {
    fn domain(self) -> Domain {
        if self == Self::Cdram {
            Domain::Cdram
        } else {
            Domain::Main
        }
    }
}
pub(super) struct Block {
    pub base: *mut c_void,
    pub bytes: usize,
    pub uid: i32,
    pub offset: u32,
    kind: Kind,
    fault: Fault,
    free: RefCell<FreeList>,
    ticket: Option<Box<dyn crate::memory::MemoryReservation>>,
    arena_bytes: Option<BlockReservation>,
    physical_main: Option<crate::memory::MainCommit>,
    physical: Option<super::pressure::Commit>,
}
impl Block {
    pub fn new(bytes: usize, kind: Kind, fault: &Fault, pressure: &Pressure) -> Result<Rc<Self>> {
        fault.check()?;
        let alignment = if kind == Kind::Cdram {
            256 * 1024
        } else {
            4096
        };
        let bytes = bytes.max(1).next_multiple_of(alignment);
        if bytes > u32::MAX as usize {
            return Err(Error::Invalid("native block size"));
        }
        let ty = if kind == Kind::Cdram {
            SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW
        } else {
            SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE
        };
        let ticket = pressure.reserve(kind.domain(), bytes as u64, fault)?;
        let physical = pressure.commit(kind.domain(), bytes as u64, fault)?;
        let physical_main = pressure.commit_main(kind.domain(), bytes, fault)?;
        let uid = pressure.allocate(kind.domain(), bytes as u64, fault, || unsafe {
            sceKernelAllocMemBlock(
                c"VitaGraphics".as_ptr(),
                ty,
                bytes as u32,
                core::ptr::null_mut(),
            )
        })?;
        let mut block = Self {
            base: core::ptr::null_mut(),
            bytes,
            uid,
            offset: 0,
            kind: Kind::Driver,
            fault: fault.clone(),
            free: RefCell::new(FreeList::new(bytes)),
            ticket,
            arena_bytes: None,
            physical_main: Some(physical_main),
            physical: Some(physical),
        };
        sdk("sceKernelGetMemBlockBase", unsafe {
            sceKernelGetMemBlockBase(uid, &mut block.base)
        })?;
        match kind {
            Kind::VertexUsse => sdk("sceGxmMapVertexUsseMemory", unsafe {
                sceGxmMapVertexUsseMemory(block.base, bytes as u32, &mut block.offset)
            })?,
            Kind::FragmentUsse => sdk("sceGxmMapFragmentUsseMemory", unsafe {
                sceGxmMapFragmentUsseMemory(block.base, bytes as u32, &mut block.offset)
            })?,
            Kind::Driver => (),
            _ => sdk("sceGxmMapMemory", unsafe {
                sceGxmMapMemory(block.base, bytes as u32, SCE_GXM_MEMORY_ATTRIB_RW)
            })?,
        }
        block.kind = kind;
        Ok(Rc::new(block))
    }
    fn allocate(self: &Rc<Self>, bytes: usize, alignment: usize) -> Option<Allocation> {
        let range = self.free.borrow_mut().take(bytes, alignment)?;
        Some(Allocation(Rc::new(AllocationInner {
            storage: Storage::Arena(self.clone()),
            range,
        })))
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        if self.fault.get().is_some() {
            std::mem::forget((
                self.ticket.take(),
                self.arena_bytes.take(),
                self.physical_main.take(),
                self.physical.take(),
            ));
            return;
        }
        let code = unsafe {
            match self.kind {
                Kind::VertexUsse => sceGxmUnmapVertexUsseMemory(self.base),
                Kind::FragmentUsse => sceGxmUnmapFragmentUsseMemory(self.base),
                Kind::Driver => 0,
                _ => sceGxmUnmapMemory(self.base),
            }
        };
        if code < 0 {
            self.fault.record("unmap native GXM block", code);
            std::mem::forget((
                self.ticket.take(),
                self.arena_bytes.take(),
                self.physical_main.take(),
                self.physical.take(),
            ));
            return;
        }
        if self
            .fault
            .command("sceKernelFreeMemBlock", unsafe {
                sceKernelFreeMemBlock(self.uid)
            })
            .is_err()
        {
            std::mem::forget((
                self.ticket.take(),
                self.arena_bytes.take(),
                self.physical_main.take(),
                self.physical.take(),
            ));
        }
    }
}
enum Storage {
    Arena(Rc<Block>),
    #[cfg(feature = "video")]
    Video(Rc<super::video::Import>),
}
struct AllocationInner {
    storage: Storage,
    range: Range<usize>,
}
impl Drop for AllocationInner {
    fn drop(&mut self) {
        match &self.storage {
            Storage::Arena(block) => block.free.borrow_mut().release(self.range.clone()),
            #[cfg(feature = "video")]
            Storage::Video(_) => {}
        }
    }
}
#[derive(Clone)]
pub(super) struct Allocation(Rc<AllocationInner>);
impl Allocation {
    pub fn ptr(&self) -> *mut c_void {
        let base = match &self.0.storage {
            Storage::Arena(block) => block.base as usize,
            #[cfg(feature = "video")]
            Storage::Video(import) => import.pointer(),
        };
        (base + self.0.range.start) as *mut c_void
    }
    pub fn writable(&self) -> bool {
        matches!(self.0.storage, Storage::Arena(_))
    }
    #[cfg(feature = "video")]
    pub fn video(import: Rc<super::video::Import>, range: Range<usize>) -> Self {
        Self(Rc::new(AllocationInner {
            storage: Storage::Video(import),
            range,
        }))
    }
    pub fn memory(&self) -> super::TextureMemory {
        match &self.0.storage {
            Storage::Arena(block) if block.kind == Kind::Cdram => super::TextureMemory::Cdram,
            _ => super::TextureMemory::Main,
        }
    }
    pub fn len(&self) -> usize {
        self.0.range.len()
    }
    pub fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }
    pub fn used(&self) -> bool {
        Rc::strong_count(&self.0) > 1
    }
    pub fn write(&self, offset: usize, bytes: &[u8]) {
        assert!(self.writable() && offset + bytes.len() <= self.len());
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr().cast::<u8>().add(offset),
                bytes.len(),
            );
        }
    }
}
pub(super) struct Arena {
    kind: Kind,
    quantum: usize,
    blocks: RefCell<[Vec<Weak<Block>>; Class::COUNT]>,
    reserved: Rc<Cell<u64>>,
    warm: Rc<RefCell<super::retained::Retained<Block>>>,
    pressure: Pressure,
    block_allocations: Cell<u64>,
    fault: Fault,
}
impl Arena {
    pub fn new(kind: Kind, fault: &Fault, pressure: &Pressure, retained: u64) -> Self {
        let warm = Rc::new(RefCell::new(super::retained::Retained::new(retained)));
        pressure.register(kind.domain(), &warm);
        Self {
            kind,
            quantum: if kind == Kind::Cdram {
                256 * 1024
            } else {
                64 * 1024
            },
            blocks: RefCell::new(std::array::from_fn(|_| Vec::new())),
            reserved: Rc::new(Cell::new(0)),
            warm,
            pressure: pressure.clone(),
            block_allocations: Cell::new(0),
            fault: fault.clone(),
        }
    }
    pub fn alloc(&self, bytes: usize, alignment: usize) -> Result<Allocation> {
        self.allocate(bytes, alignment, Lifetime::Retained)
    }
    pub fn upload(&self, bytes: usize, alignment: usize) -> Result<Allocation> {
        self.allocate(bytes, alignment, Lifetime::Upload)
    }
    fn allocate(&self, bytes: usize, alignment: usize, lifetime: Lifetime) -> Result<Allocation> {
        self.fault.check()?;
        if bytes == 0 || !alignment.is_power_of_two() {
            return Err(Error::Invalid("GPU allocation layout"));
        }
        let class = Class::new(bytes, self.quantum, lifetime);
        let mut groups = self.blocks.borrow_mut();
        let blocks = &mut groups[class.0];
        blocks.retain(|b| b.strong_count() > 0);
        for block in blocks.iter().filter_map(Weak::upgrade) {
            if class.dedicated() && block.free.borrow().bytes() != block.bytes {
                continue;
            }
            if let Some(a) = block.allocate(bytes, alignment) {
                self.warm.borrow_mut().touch(&block, block.bytes as u64);
                return Ok(a);
            }
        }
        let page = if self.kind == Kind::Cdram {
            256 * 1024
        } else {
            4096
        };
        let block_bytes =
            self.pressure
                .block_size(self.kind.domain(), bytes, self.quantum, page, &self.fault)?;
        let mut block = Block::new(block_bytes, self.kind, &self.fault, &self.pressure)?;
        Rc::get_mut(&mut block)
            .expect("new arena block")
            .arena_bytes = Some(BlockReservation::new(
            self.reserved.clone(),
            block_bytes as u64,
        ));
        let allocation = block.allocate(bytes, alignment).expect("new aligned block");
        blocks.push(Rc::downgrade(&block));
        self.block_allocations.set(self.block_allocations.get() + 1);
        self.warm.borrow_mut().touch(&block, block.bytes as u64);
        Ok(allocation)
    }
    pub fn release_idle(&self) {
        self.warm.borrow_mut().release_idle();
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.reserved.get()
    }
    pub fn usage(&self) -> ArenaUsage {
        let mut u = ArenaUsage {
            block_allocations: self.block_allocations.get(),
            ..ArenaUsage::default()
        };
        for b in self
            .blocks
            .borrow()
            .iter()
            .flatten()
            .filter_map(Weak::upgrade)
        {
            u.blocks += 1;
            u.reserved_bytes += b.bytes as u64;
            let used = b.bytes - b.free.borrow().bytes();
            u.used_bytes += used as u64;
            if used == 0 {
                u.idle_bytes += b.bytes as u64;
            }
        }
        u
    }
}
