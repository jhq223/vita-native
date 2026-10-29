//! Linear RGBA8 PTLA copies and their shared bounded submission plan.
use super::{Error, types::Result};

#[derive(Clone, Copy)]
struct Surface {
    size: [u32; 2],
    stride: usize,
}
#[derive(Clone, Copy)]
struct Plan {
    source: [u32; 2],
    destination: [u32; 2],
    size: [u32; 2],
    source_stride: i32,
    destination_stride: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    source: [u32; 2],
    destination: [u32; 2],
    size: [u32; 2],
    last: bool,
}
impl Plan {
    fn new(
        source: Surface,
        source_origin: [u32; 2],
        destination: Surface,
        destination_origin: [u32; 2],
        size: [u32; 2],
    ) -> Result<Self> {
        if size.contains(&0)
            || [(source, source_origin), (destination, destination_origin)]
                .into_iter()
                .any(|(surface, origin)| {
                    (0..2).any(|axis| {
                        origin[axis]
                            .checked_add(size[axis])
                            .is_none_or(|end| end > surface.size[axis] || end >= 8192)
                    })
                })
        {
            return Err(Error::Invalid("texture copy bounds"));
        }
        if [source, destination].into_iter().any(|surface| {
            surface.stride == 0 || surface.stride > 32767 || !surface.stride.is_multiple_of(4)
        }) {
            return Err(Error::Unsupported("texture copy stride"));
        }
        Ok(Self {
            source: source_origin,
            destination: destination_origin,
            size,
            source_stride: source.stride as i32,
            destination_stride: destination.stride as i32,
        })
    }
    fn submit<T>(
        self,
        owners: Vec<T>,
        retain: impl FnOnce(Vec<T>),
        mut copy: impl FnMut(Tile) -> Result<()>,
    ) -> Result<()> {
        // Publish ownership before the first command: a later tile can fail
        // after earlier transfers were accepted by the GPU firmware.
        retain(owners);
        let mut y = 0;
        while y < self.size[1] {
            let height = (self.size[1] - y).min(1024);
            let mut x = 0;
            while x < self.size[0] {
                let width = (self.size[0] - x).min(1024);
                copy(Tile {
                    source: [self.source[0] + x, self.source[1] + y],
                    destination: [self.destination[0] + x, self.destination[1] + y],
                    size: [width, height],
                    last: x + width == self.size[0] && y + height == self.size[1],
                })?;
                x += width;
            }
            y += height;
        }
        Ok(())
    }
}

#[cfg(target_os = "vita")]
impl super::Context {
    /// Copy an unscaled RGBA8 rectangle between independent strided render targets.
    /// Origins are top-left texel coordinates and `size` must be positive and fit
    /// both textures. Both textures must belong to this context. Other formats,
    /// sampled-texture layouts and shared source/destination allocations are rejected
    /// before issuing GPU work. Only the destination rectangle is written; callers
    /// initialize any other texels they later read.
    ///
    /// Ends the open scene and queues fragment-ordered PTLA transfers without a CPU
    /// pixel copy or completion wait, except the existing bounded notification-slot
    /// admission. The last transfer notification retains both allocations through
    /// completion; existing `fence` and `poll` observe it. A partially submitted
    /// failure latches the original SDK fault and retains both allocations.
    pub fn copy_texture(
        &self,
        source: &super::Texture,
        source_origin: [u32; 2],
        destination: &super::Texture,
        destination_origin: [u32; 2],
        size: [u32; 2],
    ) -> Result<()> {
        use super::{TextureFormat, device::Lease};
        use vitasdk_sys::*;
        self.same(&source.0.owner)?;
        self.same(&destination.0.owner)?;
        if [source, destination]
            .into_iter()
            .any(|texture| texture.format() != TextureFormat::Rgba8 || !texture.0.strided)
        {
            return Err(Error::Unsupported(
                "texture copy requires RGBA8 render targets",
            ));
        }
        if source.0.allocation.id() == destination.0.allocation.id() {
            return Err(Error::Invalid(
                "texture copy shares source and destination storage",
            ));
        }
        let plan = Plan::new(
            Surface {
                size: source.size(),
                stride: source.0.layout.row_stride_bytes,
            },
            source_origin,
            Surface {
                size: destination.size(),
                stride: destination.0.layout.row_stride_bytes,
            },
            destination_origin,
            size,
        )?;
        let owners = std::vec![
            Lease::Allocation(source.0.allocation.clone()),
            Lease::Allocation(destination.0.allocation.clone()),
        ];
        let mut state = self.0.state.borrow_mut();
        self.end_scene(&mut state)?;
        let (slot, notification) = self.reserve_slot(&mut state)?;
        plan.submit(
            owners,
            |owners| {
                self.publish(&mut state, slot, notification, owners, 0);
            },
            |tile| {
                // SAFETY: both mapped strided RGBA8 allocations have validated bounds
                // and signed byte strides; the queue owns them before the first call.
                // The final notification covers all FIFO transfers. BeginScene uses
                // FRAGMENT_TRANSFER_SYNC, establishing fragment -> transfer -> fragment.
                // These offscreen textures need no display sync object.
                let code = unsafe {
                    sceGxmTransferCopy(
                        tile.size[0],
                        tile.size[1],
                        0,
                        0,
                        SCE_GXM_TRANSFER_COLORKEY_NONE,
                        SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR,
                        SCE_GXM_TRANSFER_LINEAR,
                        source.0.allocation.ptr().cast_const(),
                        tile.source[0],
                        tile.source[1],
                        plan.source_stride,
                        SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR,
                        SCE_GXM_TRANSFER_LINEAR,
                        destination.0.allocation.ptr(),
                        tile.destination[0],
                        tile.destination[1],
                        plan.destination_stride,
                        core::ptr::null_mut(),
                        SCE_GXM_TRANSFER_FRAGMENT_SYNC,
                        if tile.last {
                            &notification
                        } else {
                            core::ptr::null()
                        },
                    )
                };
                self.command("sceGxmTransferCopy(texture)", code)
            },
        )
    }
}

#[cfg(test)]
mod tests;
