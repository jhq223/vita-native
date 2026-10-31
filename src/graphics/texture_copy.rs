//! RGBA8 PTLA copies and their shared bounded layout/submission plan.
use super::{Error, TargetStorage, types::Result};

#[derive(Clone, Copy)]
struct Surface {
    size: [u32; 2],
    stride: usize,
    storage: TargetStorage,
}
#[derive(Clone, Copy)]
struct Plan {
    source: [u32; 2],
    destination: [u32; 2],
    size: [u32; 2],
    source_stride: i32,
    destination_stride: i32,
    storage: TargetStorage,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    source: [u32; 2],
    destination: [u32; 2],
    size: [u32; 2],
    last: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Address {
    offset: usize,
    origin: [u32; 2],
    stride: i32,
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
        if source.storage != destination.storage {
            return Err(Error::Unsupported("mixed texture copy layouts"));
        }
        let size = if source.storage == TargetStorage::Tiled {
            // Typed tiled PTLA commands require zero origins, whole tiles and
            // stride == command width. Rebase each tile-row strip below. A
            // partial final tile may only overwrite inaccessible destination
            // padding, never pixels outside the requested logical rectangle.
            let rounded = size.map(|n| n.next_multiple_of(32));
            if [source_origin, destination_origin]
                .into_iter()
                .flatten()
                .any(|n| n % 32 != 0)
                || (0..2).any(|axis| {
                    (size[axis] != rounded[axis]
                        && destination_origin[axis] + size[axis] != destination.size[axis])
                        || source_origin[axis] + rounded[axis]
                            > source.size[axis].next_multiple_of(32)
                        || destination_origin[axis] + rounded[axis]
                            > destination.size[axis].next_multiple_of(32)
                })
                || [source, destination].into_iter().any(|surface| {
                    surface.stride != surface.size[0].next_multiple_of(32) as usize * 4
                })
            {
                return Err(Error::Unsupported("texture copy tile alignment"));
            }
            rounded
        } else {
            size
        };
        Ok(Self {
            source: source_origin,
            destination: destination_origin,
            size,
            source_stride: source.stride as i32,
            destination_stride: destination.stride as i32,
            storage: source.storage,
        })
    }
    fn addresses(self, tile: Tile) -> [Address; 2] {
        [
            (tile.source, self.source_stride),
            (tile.destination, self.destination_stride),
        ]
        .map(|(origin, stride)| {
            if self.storage == TargetStorage::Tiled {
                Address {
                    offset: self.storage.byte_offset(
                        origin[0] as usize,
                        origin[1] as usize,
                        stride as usize,
                        4,
                    ),
                    origin: [0; 2],
                    stride: tile.size[0] as i32 * 4,
                }
            } else {
                Address {
                    offset: 0,
                    origin,
                    stride,
                }
            }
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
        let max_height = if self.storage == TargetStorage::Tiled
            && (self.size[0] > 1024
                || self.size[0] as i32 * 4 != self.source_stride
                || self.source_stride != self.destination_stride)
        {
            32
        } else {
            1024
        };
        let mut y = 0;
        while y < self.size[1] {
            let height = (self.size[1] - y).min(max_height);
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
impl Plan {
    fn textures(
        source: &super::Texture,
        source_origin: [u32; 2],
        destination: &super::Texture,
        destination_origin: [u32; 2],
        size: [u32; 2],
    ) -> Result<Self> {
        if [source, destination].into_iter().any(|texture| {
            texture.format() != super::TextureFormat::Rgba8 || texture.target_storage().is_none()
        }) {
            return Err(Error::Unsupported(
                "texture copy requires RGBA8 render targets",
            ));
        }
        if source.0.allocation.id() == destination.0.allocation.id() {
            return Err(Error::Invalid(
                "texture copy shares source and destination storage",
            ));
        }
        Self::new(
            Surface {
                size: source.size(),
                stride: source.0.layout.row_stride_bytes,
                storage: source.0.storage,
            },
            source_origin,
            Surface {
                size: destination.size(),
                stride: destination.0.layout.row_stride_bytes,
                storage: destination.0.storage,
            },
            destination_origin,
            size,
        )
    }
}

#[cfg(target_os = "vita")]
impl super::Context {
    /// Check immutable rectangle/layout compatibility without issuing GPU work.
    /// Compatible rectangles can be submitted through `copy_texture`.
    pub fn supports_texture_copy(
        &self,
        source: &super::Texture,
        source_origin: [u32; 2],
        destination: &super::Texture,
        destination_origin: [u32; 2],
        size: [u32; 2],
    ) -> bool {
        self.same(&source.0.owner).is_ok()
            && self.same(&destination.0.owner).is_ok()
            && Plan::textures(source, source_origin, destination, destination_origin, size).is_ok()
    }
    /// Copy an unscaled RGBA8 rectangle between independent render targets.
    /// Origins are top-left texel coordinates and `size` must be positive and fit
    /// both textures. Both textures must belong to this context. Other formats,
    /// sampled-texture layouts and shared source/destination allocations are rejected
    /// before issuing GPU work. Only the destination rectangle is written; callers
    /// initialize any other texels they later read. Tiled origins align to 32;
    /// partial tile extents must end at the destination's logical edge. Only
    /// inaccessible destination padding may also be written. Mixed layouts use
    /// the application's draw path, selected through `supports_texture_copy`.
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
        use super::device::Lease;
        use vitasdk_sys::*;
        self.same(&source.0.owner)?;
        self.same(&destination.0.owner)?;
        let plan = Plan::textures(source, source_origin, destination, destination_origin, size)?;
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
                let [src, dst] = plan.addresses(tile);
                let storage = if plan.storage == TargetStorage::Tiled {
                    SCE_GXM_TRANSFER_TILED
                } else {
                    SCE_GXM_TRANSFER_LINEAR
                };
                // SAFETY: both mapped RGBA8 allocations have validated bounds
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
                        storage,
                        source
                            .0
                            .allocation
                            .ptr()
                            .cast::<u8>()
                            .add(src.offset)
                            .cast_const()
                            .cast(),
                        src.origin[0],
                        src.origin[1],
                        src.stride,
                        SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR,
                        storage,
                        destination
                            .0
                            .allocation
                            .ptr()
                            .cast::<u8>()
                            .add(dst.offset)
                            .cast(),
                        dst.origin[0],
                        dst.origin[1],
                        dst.stride,
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
