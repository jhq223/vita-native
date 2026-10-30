//! Rendering-thread mappings of immutable producer-owned NV12 blocks.
use super::{
    Context, Error, Texture, TextureFormat, TextureLayout,
    memory::{Allocation, Fault, sdk},
    resources::{TextureInner, texture_format},
    types::Result,
};
use crate::video::SharedNv12;
use std::{cell::Cell, mem::ManuallyDrop, prelude::v1::*, rc::Rc};
use vitasdk_sys::*;

pub(super) struct Import {
    storage: ManuallyDrop<SharedNv12>,
    fault: Fault,
    bytes: Rc<Cell<u64>>,
}
impl Import {
    pub fn pointer(&self) -> usize {
        self.storage.pointer()
    }
    fn id(&self) -> usize {
        self.storage.id()
    }
}
impl Drop for Import {
    fn drop(&mut self) {
        // A failed native ownership boundary deliberately retains the physical
        // source and its ticket. It must never return to a writable pool then.
        if self.fault.get().is_some() {
            return;
        }
        if self
            .fault
            .command("unmap imported NV12", unsafe {
                sceGxmUnmapMemory(self.pointer() as *mut _)
            })
            .is_err()
        {
            return;
        }
        self.bytes
            .set(self.bytes.get() - self.storage.capacity() as u64);
        unsafe {
            ManuallyDrop::drop(&mut self.storage);
        }
    }
}
impl Context {
    /// Import immutable NV12 planes without a second pixel copy. Mapping, texture
    /// creation and unmapping run on this device's thread. Draw submission retains
    /// the mapping and source until fragment completion, even if textures retire.
    /// The producer owns and accounts physical capacity through its final reader.
    pub fn import_nv12(&self, source: &SharedNv12) -> Result<[Texture; 2]> {
        self.check("import NV12")?;
        let mut imports = self.0.video_imports.borrow_mut();
        imports.retain(|entry| entry.strong_count() != 0);
        let found = imports
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .find(|entry| entry.id() == source.id());
        let import = if let Some(import) = found {
            import
        } else {
            // Every live import owns at least one physical block. Metadata is
            // bounded independently of arbitrary caller handle duplication.
            if imports.len() == 64 {
                return Err(Error::Invalid("64 live NV12 imports"));
            }
            imports
                .try_reserve(1)
                .map_err(|_| Error::Invalid("NV12 import metadata allocation"))?;
            sdk("map imported NV12", unsafe {
                sceGxmMapMemory(
                    source.pointer() as *mut _,
                    source.capacity() as u32,
                    SCE_GXM_MEMORY_ATTRIB_READ,
                )
            })?;
            self.0
                .core
                .imported_video_bytes
                .set(self.0.core.imported_video_bytes.get() + source.capacity() as u64);
            let import = Rc::new(Import {
                storage: ManuallyDrop::new(source.clone()),
                fault: self.0.core.fault.clone(),
                bytes: self.0.core.imported_video_bytes.clone(),
            });
            imports.push(Rc::downgrade(&import));
            import
        };
        drop(imports);
        let sizes = [source.size(), source.size().map(|n| n / 2)];
        let formats = [TextureFormat::Luma8, TextureFormat::LumaAlpha8];
        let y_bytes = source.plane(0).len();
        let ranges = [0..y_bytes, y_bytes..y_bytes + source.plane(1).len()];
        let mut textures = Vec::with_capacity(2);
        for index in 0..2 {
            let allocation = Allocation::video(import.clone(), ranges[index].clone());
            let mut raw = unsafe { core::mem::zeroed() };
            sdk("initialize imported NV12 texture", unsafe {
                sceGxmTextureInitLinear(
                    &mut raw,
                    allocation.ptr().cast(),
                    texture_format(formats[index]),
                    sizes[index][0],
                    sizes[index][1],
                    1,
                )
            })?;
            sdk("NV12 U clamp", unsafe {
                sceGxmTextureSetUAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
            })?;
            sdk("NV12 V clamp", unsafe {
                sceGxmTextureSetVAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
            })?;
            textures.push(Texture(Rc::new(TextureInner {
                layout: TextureLayout {
                    bytes: allocation.len(),
                    row_stride_bytes: source.strides()[index] as usize,
                },
                allocation,
                raw: Cell::new(raw),
                size: sizes[index],
                format: formats[index],
                strided: false,
                storage: super::TargetStorage::Linear,
                mip_count: 1,
                owner: self.0.core.clone(),
            })));
        }
        Ok(textures.try_into().ok().expect("two NV12 planes"))
    }
}
