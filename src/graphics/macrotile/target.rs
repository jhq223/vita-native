//! Owned linear atlas and clamped read-only cell aliases.
use crate::graphics::{
    Context, Error, Framebuffer, MacrotileLayout, TargetFootprint, TargetFormat, TargetStorage,
    Texture, TextureFormat, TextureLayout,
    display_sync::{DisplaySync, SurfaceKind},
    memory::sdk,
    render_targets::{Pool, RenderTarget},
    resources::{FramebufferInner, TextureInner, rt_params, texture_format},
    types::Result,
};
use std::{cell::Cell, prelude::v1::*, rc::Rc};
use vitasdk_sys::*;

/// One atlas whose cells share storage and firmware scheduling ownership.
/// Drawing cells in increasing order keeps them in one native scene. A cell's
/// texture can read earlier cells in that scene, or be sampled by later scenes.
#[derive(Clone)]
pub struct MacrotileTarget {
    layout: MacrotileLayout,
    cells: Rc<[Framebuffer]>,
}
impl MacrotileTarget {
    /// Immutable physical layout used for this allocation.
    pub fn layout(&self) -> MacrotileLayout {
        self.layout
    }
    /// Cell framebuffer with its original logical dimensions and a CLAMP alias.
    /// The cell permits rendering; its sampled texture rejects CPU uploads and
    /// PTLA writes. Framebuffer/texture clones retain the complete allocation.
    pub fn cell(&self, index: usize) -> Option<&Framebuffer> {
        self.cells.get(index)
    }
    /// Complete shared color allocation, counted once for all cells.
    pub fn allocated_bytes(&self) -> usize {
        self.cells[0].texture().allocated_bytes()
    }
}

impl Context {
    /// Exact shared atlas storage and firmware bytes for this macrotile grid.
    /// `sizes` supplies 2..=cell_count logical extents, each fitting one cell.
    pub fn macrotile_target_footprint(
        &self,
        layout: MacrotileLayout,
        sizes: &[[u32; 2]],
    ) -> Result<TargetFootprint> {
        self.check("macrotile footprint")?;
        layout.validate_sizes(sizes)?;
        let texture = TargetStorage::Linear.layout(layout.size(), TargetFormat::Rgba8)?;
        let mut params = rt_params(layout.size(), self.0.core.config.scenes_per_frame);
        params.flags = layout.flags();
        let mut bytes = 0;
        // SAFETY: validated scalar geometry and writable size output.
        sdk("sceGxmGetRenderTargetMemSize(macrotile)", unsafe {
            sceGxmGetRenderTargetMemSize(&params, &mut bytes)
        })?;
        Ok(TargetFootprint {
            texture,
            render_target_driver_bytes: Some(u64::from(bytes.next_multiple_of(4096))),
        })
    }

    /// Allocate linear RGBA8 cells with Sony's ordered macrotile synchronization.
    /// Source aliases retain their logical sizes and atlas stride. Native draws
    /// translate viewport/scissor into the selected cell and reject backward
    /// writes or feedback from the current/later cell. Each intermediate stage
    /// is stored as RGBA8 before the next cell samples it.
    /// Clear each cell before its first draw to initialize the complete physical
    /// extent, including padding outside the logical sampled view.
    pub fn create_macrotile_target(
        &self,
        layout: MacrotileLayout,
        sizes: &[[u32; 2]],
    ) -> Result<MacrotileTarget> {
        let footprint = self.macrotile_target_footprint(layout, sizes)?;
        let atlas = self.create_target_texture_with_storage(
            layout.size(),
            TargetFormat::Rgba8,
            TargetStorage::Linear,
        )?;
        let target = Pool::get_or_create_macrotile(
            &self.0.targets,
            layout,
            footprint.render_target_driver_bytes.unwrap(),
            || RenderTarget::macrotile(&self.0.core, layout),
        )?;
        let stride = footprint.texture.row_stride_bytes as usize;
        let mut cells = Vec::with_capacity(sizes.len());
        for (index, &size) in sizes.iter().enumerate() {
            let rect = layout
                .cell_rect(index as u32)
                .expect("validated cell index");
            let offset = rect[1] as usize * stride + rect[0] as usize * 4;
            let span = (size[1] as usize - 1) * stride + size[0] as usize * 4;
            let allocation = atlas.0.allocation.alias(offset..offset + span)?;
            if !(allocation.ptr() as usize).is_multiple_of(64) {
                return Err(Error::Invalid("macrotile SLC alignment"));
            }
            let mut raw = unsafe { core::mem::zeroed() };
            // SAFETY: the owned view bounds every row; cell alignment and CLAMP
            // prevent filtering from loading another macrotile's SLC lines.
            sdk("sceGxmTextureInitLinearStrided(macrotile)", unsafe {
                sceGxmTextureInitLinearStrided(
                    &mut raw,
                    allocation.ptr().cast(),
                    texture_format(TextureFormat::Rgba8),
                    size[0],
                    size[1],
                    stride as u32,
                )
            })?;
            sdk("macrotile U clamp", unsafe {
                sceGxmTextureSetUAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
            })?;
            sdk("macrotile V clamp", unsafe {
                sceGxmTextureSetVAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
            })?;
            let texture = Texture(Rc::new(TextureInner {
                allocation,
                raw: Cell::new(raw),
                size,
                format: TextureFormat::Rgba8,
                layout: TextureLayout {
                    bytes: span,
                    row_stride_bytes: stride,
                },
                strided: true,
                storage: TargetStorage::Linear,
                mip_count: 1,
                macrocell: Some(super::Cell {
                    layout,
                    index: index as u32,
                    backing: atlas.0.allocation.id(),
                }),
                owner: self.0.core.clone(),
            }));
            let mut surface = unsafe { core::mem::zeroed() };
            // SAFETY: all cells retain atlas storage. The complete color surface
            // is required: macrotile synchronization forbids a custom valid region.
            sdk("sceGxmColorSurfaceInit(macrotile)", unsafe {
                sceGxmColorSurfaceInit(
                    &mut surface,
                    SCE_GXM_COLOR_FORMAT_A8B8G8R8,
                    SCE_GXM_COLOR_SURFACE_LINEAR,
                    SCE_GXM_COLOR_SURFACE_SCALE_NONE,
                    SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT,
                    layout.size()[0],
                    layout.size()[1],
                    (stride / 4) as u32,
                    atlas.0.allocation.ptr().cast(),
                )
            })?;
            cells.push(Framebuffer(Rc::new(FramebufferInner {
                sync: DisplaySync::new(SurfaceKind::Offscreen, &self.0.core.fault)?,
                texture,
                target: target.clone(),
                surface,
                encoding: crate::graphics::SurfaceEncoding::Linear,
                owner: self.0.core.clone(),
            })));
        }
        Ok(MacrotileTarget {
            layout,
            cells: cells.into(),
        })
    }
}
