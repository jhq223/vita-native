//! Owned mapped textures, render targets and indexed geometry.
use super::types::Result;
use super::{
    device::{Core, Lease},
    display_sync::{DisplaySync, SurfaceKind},
    memory::{Allocation, sdk},
    render_targets::{self, RenderTarget},
    *,
};
use std::{cell::Cell, prelude::v1::*, rc::Rc};
use vitasdk_sys::*;
pub(super) struct TextureInner {
    pub allocation: Allocation,
    pub raw: Cell<SceGxmTexture>,
    /// Logical viewport extent, bounded by physical storage.
    pub size: [u32; 2],
    pub format: TextureFormat,
    pub layout: TextureLayout,
    pub strided: bool,
    pub mip_count: u32,
    pub owner: Rc<Core>,
}
/// Sampled texture whose native storage survives every submitted reader.
#[derive(Clone)]
pub struct Texture(pub(super) Rc<TextureInner>);
impl PartialEq for Texture {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}
impl Eq for Texture {}
impl Texture {
    /// Physical texture/target extent in pixels.
    pub fn size(&self) -> [u32; 2] {
        self.0.size
    }
    /// Sampling and payload format of the allocation.
    pub fn format(&self) -> TextureFormat {
        self.0.format
    }
    /// Physical allocation domain, retained across uploads and aliases.
    pub fn memory(&self) -> super::TextureMemory {
        self.0.allocation.memory()
    }
    /// Number of stored levels, including the base.
    pub fn mip_count(&self) -> u32 {
        self.0.mip_count
    }
    /// Exact native row pitch and payload bytes.
    pub fn layout(&self) -> TextureLayout {
        self.0.layout
    }
    /// Live payload bytes; native slab capacity is reported separately.
    pub fn allocated_bytes(&self) -> usize {
        self.0.allocation.len()
    }
}
pub(super) struct FramebufferInner {
    // Destroy sync first: a cleanup failure must quarantine the following storage.
    pub sync: DisplaySync,
    /// RGBA8 color layout consumed by target planning.
    pub texture: Texture,
    /// Shared firmware scheduling backing, independent of this color surface.
    pub target: Rc<RenderTarget>,
    pub surface: SceGxmColorSurface,
    pub owner: Rc<Core>,
}
/// Render-target scheduling backing plus a sampled view of its color allocation.
#[derive(Clone)]
pub struct Framebuffer(pub(super) Rc<FramebufferInner>);
impl Framebuffer {
    /// Physical texture/target extent in pixels.
    pub fn size(&self) -> [u32; 2] {
        self.0.texture.size()
    }
    /// Sampled view sharing this render target's color allocation.
    pub fn texture(&self) -> &Texture {
        &self.0.texture
    }
}
#[derive(Clone, Copy)]
/// Select the ordered scene output.
pub enum Target<'a> {
    /// The admitted process-owned scanout buffer.
    Screen,
    /// A sampled RGBA8/BGRA8 target allocation.
    Framebuffer(&'a Framebuffer),
}
/// Immutable geometry storage; completed buffers can be updated within their capacity.
pub struct VertexBuffer {
    pub(super) allocation: Allocation,
    pub(super) count: usize,
    pub(super) stride: u32,
    pub(super) attributes: Vec<VertexAttribute>,
    pub(super) owner: Rc<Core>,
}
/// Owned immutable element allocation with width and validated maximum index.
pub struct IndexBuffer {
    pub(super) allocation: Allocation,
    pub(super) count: usize,
    pub(super) width: usize,
    pub(super) maximum: u32,
    pub(super) owner: Rc<Core>,
}
pub(super) fn texture_format(f: TextureFormat) -> u32 {
    match f {
        TextureFormat::Rgba8 => SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_ABGR,
        TextureFormat::Bgra8 => SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_ARGB,
        TextureFormat::Rgba16Float => SCE_GXM_TEXTURE_FORMAT_F16F16F16F16_ABGR,
        TextureFormat::Luma8 => SCE_GXM_TEXTURE_FORMAT_U8_1RRR,
        TextureFormat::Coverage8 => SCE_GXM_TEXTURE_FORMAT_U8_RRRR,
        TextureFormat::LumaAlpha8 => SCE_GXM_TEXTURE_FORMAT_U8U8_GRRR,
        TextureFormat::Bc1Rgb => SCE_GXM_TEXTURE_FORMAT_UBC1_1BGR,
        TextureFormat::Bc3Rgba => SCE_GXM_TEXTURE_FORMAT_UBC3_ABGR,
    }
}
pub(super) fn rt_params(size: [u32; 2], scenes: u16) -> SceGxmRenderTargetParams {
    SceGxmRenderTargetParams {
        flags: 0,
        width: size[0] as u16,
        height: size[1] as u16,
        scenesPerFrame: scenes,
        multisampleMode: SCE_GXM_MULTISAMPLE_NONE as u16,
        multisampleLocations: 0,
        driverMemBlock: -1,
    }
}
pub(super) fn bytes<T>(data: &[T]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(data.as_ptr().cast(), core::mem::size_of_val(data)) }
}
impl Context {
    /// Exact ordinary texture storage. Linear textures retain repeat addressing.
    pub fn texture_layout(&self, size: [u32; 2], format: TextureFormat) -> Result<TextureLayout> {
        texture_layout(size, format)
    }
    /// Strided color storage including format-specific surface and texture pitch alignment.
    pub fn target_layout(&self, size: [u32; 2], format: TargetFormat) -> Result<TargetLayout> {
        target_layout(size, format)
    }
    /// Query one shared scheduling class with the configured scene capacity.
    /// Color storage retains its exact extent; compatible surfaces share driver bytes.
    pub fn target_footprint(
        &self,
        size: [u32; 2],
        format: TargetFormat,
    ) -> Result<TargetFootprint> {
        self.check("target footprint")?;
        let texture = self.target_layout(size, format)?;
        let mut bytes = 0;
        sdk("sceGxmGetRenderTargetMemSize", unsafe {
            sceGxmGetRenderTargetMemSize(
                &rt_params(
                    render_targets::extent(size)?,
                    self.0.core.config.scenes_per_frame,
                ),
                &mut bytes,
            )
        })?;
        Ok(TargetFootprint {
            texture,
            render_target_driver_bytes: Some(u64::from(bytes.next_multiple_of(4096))),
        })
    }
    fn allocate_texture(
        &self,
        size: [u32; 2],
        format: TextureFormat,
        strided: bool,
        display: bool,
        mip_count: u32,
        memory: super::TextureMemory,
    ) -> Result<Texture> {
        self.check("texture creation")?;
        let l = if strided {
            let target_format = match format {
                TextureFormat::Rgba8 | TextureFormat::Bgra8 => TargetFormat::Rgba8,
                TextureFormat::Coverage8 => TargetFormat::Coverage8,
                _ => return Err(Error::Unsupported("strided target format")),
            };
            let l = self.target_layout(size, target_format)?;
            TextureLayout {
                bytes: l.texture_bytes as usize,
                row_stride_bytes: l.row_stride_bytes as usize,
            }
        } else {
            layout::mip_texture_layout(size, format, mip_count)?
        };
        let arena = if display {
            &self.0.core.display_pixels
        } else {
            match memory {
                super::TextureMemory::Cdram => &self.0.core.pixels,
                super::TextureMemory::Main => &self.0.core.main_textures,
            }
        };
        let allocation = arena.alloc(l.bytes, 256)?;
        let mut raw = unsafe { core::mem::zeroed() };
        let result = unsafe {
            if format.compressed() {
                sceGxmTextureInitSwizzled(
                    &mut raw,
                    allocation.ptr().cast(),
                    texture_format(format),
                    size[0],
                    size[1],
                    mip_count,
                )
            } else if strided {
                sceGxmTextureInitLinearStrided(
                    &mut raw,
                    allocation.ptr().cast(),
                    texture_format(format),
                    size[0],
                    size[1],
                    l.row_stride_bytes as u32,
                )
            } else {
                sceGxmTextureInitLinear(
                    &mut raw,
                    allocation.ptr().cast(),
                    texture_format(format),
                    size[0],
                    size[1],
                    mip_count,
                )
            }
        };
        sdk("initialize GXM texture", result)?;
        sdk("texture U clamp", unsafe {
            sceGxmTextureSetUAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
        })?;
        sdk("texture V clamp", unsafe {
            sceGxmTextureSetVAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
        })?;
        Ok(Texture(Rc::new(TextureInner {
            allocation,
            raw: Cell::new(raw),
            size,
            format,
            layout: l,
            strided,
            mip_count,
            owner: self.0.core.clone(),
        })))
    }
    /// Allocate an ordinary sampled texture with the pure `texture_layout` contract.
    pub fn create_texture(
        &self,
        size: [u32; 2],
        format: TextureFormat,
        memory: super::TextureMemory,
    ) -> Result<Texture> {
        self.allocate_texture(size, format, false, false, 1, memory)
    }
    /// Allocate a complete sampled chain; count includes the base level (SDK mipCount).
    pub fn create_mip_texture(
        &self,
        size: [u32; 2],
        format: TextureFormat,
        count: u32,
        memory: super::TextureMemory,
    ) -> Result<Texture> {
        self.allocate_texture(size, format, false, false, count, memory)
    }
    /// Write rows of an ordinary uncompressed sampled mip texture after its readers finish.
    /// Source bytes are consumed synchronously; padding follows the native mip layout.
    pub fn upload_mip_rows(
        &self,
        t: &Texture,
        level: u32,
        row: u32,
        rows: u32,
        data: &[u8],
        stride: usize,
    ) -> Result<()> {
        self.same(&t.0.owner)?;
        let mip = layout::mip_layout(t.size(), t.format(), t.mip_count(), level)?;
        let packed = mip.size[0] as usize * t.format().pixel_bytes();
        if t.0.strided
            || !t.0.allocation.writable()
            || t.format().compressed()
            || rows == 0
            || row.checked_add(rows).is_none_or(|n| n > mip.size[1])
            || stride < packed
            || layout::upload_span(rows, stride, packed).is_none_or(|span| span > data.len())
        {
            return Err(Error::Invalid("mip upload rows"));
        }
        self.await_allocation(&t.0.allocation)?;
        for y in 0..rows as usize {
            t.0.allocation.write(
                mip.offset + (row as usize + y) * mip.layout.row_stride_bytes,
                &data[y * stride..y * stride + packed],
            );
        }
        Ok(())
    }
    /// Allocate the same explicit row pitch reported by `target_layout`.
    pub fn create_target_texture(&self, size: [u32; 2], format: TargetFormat) -> Result<Texture> {
        self.allocate_texture(
            size,
            match format {
                TargetFormat::Rgba8 => TextureFormat::Rgba8,
                TargetFormat::Coverage8 => TextureFormat::Coverage8,
            },
            true,
            false,
            1,
            super::TextureMemory::Cdram,
        )
    }
    pub(super) fn create_display_texture(&self, size: [u32; 2]) -> Result<Texture> {
        self.allocate_texture(
            size,
            TextureFormat::Rgba8,
            true,
            true,
            1,
            super::TextureMemory::Cdram,
        )
    }
    /// Reorder row-major BC blocks directly into final mapped Morton storage.
    pub fn create_bc_texture(
        &self,
        size: [u32; 2],
        format: TextureFormat,
        blocks: &[u8],
        memory: super::TextureMemory,
    ) -> Result<Texture> {
        if !format.compressed() {
            return Err(Error::Invalid("BC format"));
        }
        let texture = self.create_texture(size, format, memory)?;
        self.upload_bc_texture(&texture, blocks)?;
        Ok(texture)
    }
    /// Wait for this texture's users, then reorder BC blocks into its mapped storage.
    pub fn upload_bc_texture(&self, t: &Texture, blocks: &[u8]) -> Result<()> {
        self.same(&t.0.owner)?;
        if !t.0.format.compressed() || blocks.len() != t.0.layout.bytes {
            return Err(Error::Invalid("BC payload"));
        }
        self.await_allocation(&t.0.allocation)?;
        let dst = unsafe {
            core::slice::from_raw_parts_mut(t.0.allocation.ptr().cast(), t.0.layout.bytes)
        };
        let n = if t.format() == TextureFormat::Bc1Rgb {
            8
        } else {
            16
        };
        for level in 0..t.mip_count() {
            let mip = layout::mip_layout(t.size(), t.format(), t.mip_count(), level)?;
            let range = mip.offset..mip.offset + mip.layout.bytes;
            layout::morton_bc(
                mip.size.map(|v| v.max(4)),
                n,
                &blocks[range.clone()],
                &mut dst[range],
            )?;
        }
        Ok(())
    }
    /// Copy offline GXM Morton BC blocks directly into mapped texture storage.
    /// Input must have the physical power-of-two extent reported by texture_layout.
    /// Waits only for this allocation's submitted users before overwriting it.
    pub fn upload_bc_morton_texture(&self, t: &Texture, blocks: &[u8]) -> Result<()> {
        self.same(&t.0.owner)?;
        if !t.0.format.compressed() || blocks.len() != t.0.layout.bytes {
            return Err(Error::Invalid("Morton BC payload"));
        }
        self.await_allocation(&t.0.allocation)?;
        t.0.allocation.write(0, blocks);
        Ok(())
    }
    /// Consume CPU rows before returning. In-flight textures use staged GPU copies,
    /// ordered between fragment scenes and retired by a transfer notification.
    pub fn upload(
        &self,
        t: &Texture,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
        stride: usize,
    ) -> Result<()> {
        self.same(&t.0.owner)?;
        self.poll()?;
        let bpp = t.format().pixel_bytes();
        let row = size[0] as usize * bpp;
        if size.contains(&0)
            || !t.0.allocation.writable()
            || t.format().compressed()
            || stride < row
            || layout::upload_span(size[1], stride, row).is_none_or(|span| span > data.len())
            || (0..2).any(|i| {
                origin[i]
                    .checked_add(size[i])
                    .is_none_or(|v| v > t.size()[i])
            })
        {
            return Err(Error::Invalid("texture upload bounds"));
        }
        if !t.0.allocation.used() {
            for y in 0..size[1] as usize {
                t.0.allocation.write(
                    (origin[1] as usize + y) * t.0.layout.row_stride_bytes
                        + origin[0] as usize * bpp,
                    &data[y * stride..y * stride + row],
                );
            }
            return Ok(());
        }
        let stage_stride = row.next_multiple_of(4);
        let max_rows =
            (self.0.core.config.upload_staging_bytes as usize / stage_stride).min(size[1] as usize);
        if max_rows == 0 {
            return Err(Error::Budget {
                domain: "native upload row",
                requested: stage_stride as u64,
                limit: self.0.core.config.upload_staging_bytes,
            });
        }
        if max_rows < size[1] as usize {
            let mut y = 0usize;
            while y < size[1] as usize {
                let h = (size[1] as usize - y).min(max_rows);
                self.upload(
                    t,
                    [origin[0], origin[1] + y as u32],
                    [size[0], h as u32],
                    &data[y * stride..],
                    stride,
                )?;
                y += h;
            }
            return Ok(());
        }
        self.admit_staging(stage_stride * size[1] as usize)?;
        let stage = self
            .0
            .core
            .buffers
            .upload(stage_stride * size[1] as usize, 16)?;
        for y in 0..size[1] as usize {
            stage.write(y * stage_stride, &data[y * stride..y * stride + row]);
        }
        self.transfer_upload(t, stage, origin, size, stage_stride, bpp)
    }
    fn transfer_upload(
        &self,
        t: &Texture,
        stage: Allocation,
        origin: [u32; 2],
        size: [u32; 2],
        stride: usize,
        bpp: usize,
    ) -> Result<()> {
        let mut s = self.0.state.borrow_mut();
        self.end_scene(&mut s)?;
        let (slot, n) = self.reserve_slot(&mut s)?;
        let format = match bpp {
            1 => SCE_GXM_TRANSFER_FORMAT_U8_R,
            2 => SCE_GXM_TRANSFER_FORMAT_RAW16,
            4 => SCE_GXM_TRANSFER_FORMAT_RAW32,
            8 => SCE_GXM_TRANSFER_FORMAT_RAW64,
            _ => return Err(Error::Invalid("transfer pixel width")),
        };
        let dest_stride = t.0.layout.row_stride_bytes;
        let rows = if stride > 32767 || dest_stride > 32767 {
            1
        } else if bpp == 8 {
            512
        } else {
            1024
        };
        let mut y = 0;
        while y < size[1] {
            let h = (size[1] - y).min(rows);
            let mut x = 0;
            while x < size[0] {
                let w = (size[0] - x).min(1024);
                let direct_row = rows == 1;
                let src = unsafe {
                    stage.ptr().cast::<u8>().add(if direct_row {
                        y as usize * stride + x as usize * bpp
                    } else {
                        0
                    })
                };
                let dst = unsafe {
                    t.0.allocation.ptr().cast::<u8>().add(if direct_row {
                        (origin[1] + y) as usize * dest_stride + (origin[0] + x) as usize * bpp
                    } else {
                        0
                    })
                };
                let last = x + w == size[0] && y + h == size[1];
                let code = unsafe {
                    sceGxmTransferCopy(
                        w,
                        h,
                        0,
                        0,
                        SCE_GXM_TRANSFER_COLORKEY_NONE,
                        format,
                        SCE_GXM_TRANSFER_LINEAR,
                        src.cast(),
                        if direct_row { 0 } else { x },
                        if direct_row { 0 } else { y },
                        if direct_row {
                            (w as usize * bpp) as i32
                        } else {
                            stride as i32
                        },
                        format,
                        SCE_GXM_TRANSFER_LINEAR,
                        dst.cast(),
                        if direct_row { 0 } else { origin[0] + x },
                        if direct_row { 0 } else { origin[1] + y },
                        if direct_row {
                            (w as usize * bpp) as i32
                        } else {
                            dest_stride as i32
                        },
                        core::ptr::null_mut(),
                        SCE_GXM_TRANSFER_FRAGMENT_SYNC,
                        if last { &n } else { core::ptr::null() },
                    )
                };
                self.command("sceGxmTransferCopy(upload)", code)?;
                x += w;
            }
            y += h;
        }
        let staging = stage.len();
        self.publish(
            &mut s,
            slot,
            n,
            std::vec![
                Lease::Allocation(stage),
                Lease::Allocation(t.0.allocation.clone())
            ],
            staging,
        );
        Ok(())
    }
    /// Prepare texture-control-word addressing for subsequent draws; strided targets clamp.
    pub fn set_repeat(&self, t: &Texture, repeat: [bool; 2]) -> Result<()> {
        self.same(&t.0.owner)?;
        if t.0.strided && repeat.iter().any(|v| *v) {
            return Err(Error::Invalid("strided render-target repeat"));
        }
        let mut raw = t.0.raw.get();
        let mode = |v| {
            if v {
                SCE_GXM_TEXTURE_ADDR_REPEAT
            } else {
                SCE_GXM_TEXTURE_ADDR_CLAMP
            }
        };
        sdk("texture U addressing", unsafe {
            sceGxmTextureSetUAddrMode(&mut raw, mode(repeat[0]))
        })?;
        sdk("texture V addressing", unsafe {
            sceGxmTextureSetVAddrMode(&mut raw, mode(repeat[1]))
        })?;
        t.0.raw.set(raw);
        Ok(())
    }
    /// Bind offscreen color storage to shared firmware scheduling backing.
    /// Ordered fragment scenes and transfer flags synchronize GPU access; fragment
    /// notifications retain storage until completion. Display sync objects belong
    /// only to the three context-owned scanout buffers.
    pub fn framebuffer(&self, t: &Texture) -> Result<Framebuffer> {
        self.create_framebuffer(t, SurfaceKind::Offscreen)
    }
    pub(super) fn create_framebuffer(&self, t: &Texture, kind: SurfaceKind) -> Result<Framebuffer> {
        self.same(&t.0.owner)?;
        let format = match t.format() {
            TextureFormat::Rgba8 => SCE_GXM_COLOR_FORMAT_A8B8G8R8,
            TextureFormat::Bgra8 => SCE_GXM_COLOR_FORMAT_A8R8G8B8,
            TextureFormat::Coverage8 => SCE_GXM_COLOR_FORMAT_U8_A,
            _ => return Err(Error::Unsupported("framebuffer color format")),
        };
        let target_format = if t.format() == TextureFormat::Coverage8 {
            TargetFormat::Coverage8
        } else {
            TargetFormat::Rgba8
        };
        let bytes = self
            .target_footprint(t.size(), target_format)?
            .render_target_driver_bytes
            .unwrap();
        if !self.0.targets.borrow().contains(t.size())?
            && self.0.core.rt_bytes.get().saturating_add(bytes)
                > self.0.core.config.render_target_bytes
        {
            self.0.targets.borrow_mut().release_idle();
        }
        let target =
            super::render_targets::Pool::get_or_create(&self.0.targets, t.size(), bytes, |size| {
                RenderTarget::new(&self.0.core, size)
            })?;
        let mut surface = unsafe { core::mem::zeroed() };
        sdk("sceGxmColorSurfaceInit", unsafe {
            sceGxmColorSurfaceInit(
                &mut surface,
                format,
                SCE_GXM_COLOR_SURFACE_LINEAR,
                SCE_GXM_COLOR_SURFACE_SCALE_NONE,
                SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT,
                t.size()[0],
                t.size()[1],
                (t.0.layout.row_stride_bytes / t.format().pixel_bytes()) as u32,
                t.0.allocation.ptr().cast(),
            )
        })?;
        let sync = DisplaySync::new(kind, &self.0.core.fault)?;
        Ok(Framebuffer(Rc::new(FramebufferInner {
            texture: t.clone(),
            target,
            surface,
            sync,
            owner: self.0.core.clone(),
        })))
    }
    /// Upload immutable interleaved f32 geometry; stride and attribute offsets use floats.
    pub fn vertex_buffer(
        &self,
        data: &[f32],
        stride: u32,
        attributes: &[VertexAttribute],
    ) -> Result<VertexBuffer> {
        self.check("vertex creation")?;
        if stride == 0
            || stride > 16383
            || !data.len().is_multiple_of(stride as usize)
            || attributes.iter().any(|a| {
                a.components == 0
                    || a.components > 4
                    || a.location >= 16
                    || a.offset
                        .checked_add(a.components)
                        .is_none_or(|v| v > stride)
            })
        {
            return Err(Error::Invalid("vertex layout"));
        }
        let allocation = self
            .0
            .core
            .buffers
            .alloc(core::mem::size_of_val(data).max(4), 16)?;
        allocation.write(0, bytes(data));
        Ok(VertexBuffer {
            allocation,
            count: data.len() / stride as usize,
            stride,
            attributes: attributes.to_vec(),
            owner: self.0.core.clone(),
        })
    }
    fn indices(
        &self,
        data: &[u8],
        count: usize,
        width: usize,
        maximum: u32,
    ) -> Result<IndexBuffer> {
        self.check("index creation")?;
        let allocation = self.0.core.buffers.alloc(data.len().max(4), 4)?;
        allocation.write(0, data);
        Ok(IndexBuffer {
            allocation,
            count,
            width,
            maximum,
            owner: self.0.core.clone(),
        })
    }
    /// Upload immutable u32 triangle elements.
    pub fn index_buffer(&self, i: &[u32]) -> Result<IndexBuffer> {
        self.indices(bytes(i), i.len(), 4, i.iter().copied().max().unwrap_or(0))
    }
    /// Upload immutable u16 triangle elements.
    pub fn index_buffer_u16(&self, i: &[u16]) -> Result<IndexBuffer> {
        self.indices(
            bytes(i),
            i.len(),
            2,
            u32::from(i.iter().copied().max().unwrap_or(0)),
        )
    }
    /// Replace complete vertices within the original capacity after GPU readers retire.
    pub fn update_vertices(&self, b: &mut VertexBuffer, data: &[f32]) -> Result<()> {
        self.same(&b.owner)?;
        self.poll()?;
        if b.allocation.used()
            || !data.len().is_multiple_of(b.stride as usize)
            || core::mem::size_of_val(data) > b.allocation.len()
        {
            return Err(Error::Invalid(
                "vertex replacement extent or active GPU reader",
            ));
        }
        b.allocation.write(0, bytes(data));
        b.count = data.len() / b.stride as usize;
        Ok(())
    }
    /// Update complete active vertices. `first_vertex` uses vertices, not floats.
    /// A buffer remains immutable while a submitted draw owns its allocation.
    pub fn update_vertex_range(
        &self,
        b: &mut VertexBuffer,
        first_vertex: usize,
        data: &[f32],
    ) -> Result<()> {
        self.same(&b.owner)?;
        self.poll()?;
        let stride = b.stride as usize;
        if b.allocation.used()
            || !data.len().is_multiple_of(stride)
            || first_vertex
                .checked_add(data.len() / stride)
                .is_none_or(|end| end > b.count)
        {
            return Err(Error::Invalid("vertex update extent or active GPU reader"));
        }
        b.allocation.write(first_vertex * stride * 4, bytes(data));
        Ok(())
    }
    fn update_index_bytes(
        &self,
        b: &mut IndexBuffer,
        offset: usize,
        data: &[u8],
        width: usize,
        maximum: u32,
    ) -> Result<()> {
        self.same(&b.owner)?;
        self.poll()?;
        if b.width != width
            || b.allocation.used()
            || offset
                .checked_add(data.len() / width)
                .is_none_or(|n| n > b.count)
        {
            return Err(Error::Invalid("index update extent or active GPU reader"));
        }
        b.allocation.write(offset * width, data);
        b.maximum = b.maximum.max(maximum);
        Ok(())
    }
    /// Update active u32 indices at an element offset after all readers retire.
    pub fn update_index_range(&self, b: &mut IndexBuffer, offset: usize, i: &[u32]) -> Result<()> {
        self.update_index_bytes(b, offset, bytes(i), 4, i.iter().copied().max().unwrap_or(0))
    }
    /// Update active u16 indices at an element offset after all readers retire.
    pub fn update_index_range_u16(
        &self,
        b: &mut IndexBuffer,
        offset: usize,
        i: &[u16],
    ) -> Result<()> {
        self.update_index_bytes(
            b,
            offset,
            bytes(i),
            2,
            u32::from(i.iter().copied().max().unwrap_or(0)),
        )
    }
    /// Replace active u32 elements within completed storage of the original width.
    pub fn update_indices(&self, b: &mut IndexBuffer, i: &[u32]) -> Result<()> {
        self.replace_indices(b, bytes(i), 4, i.iter().copied().max().unwrap_or(0))
    }
    /// Replace active u16 elements within completed storage of the original width.
    pub fn update_indices_u16(&self, b: &mut IndexBuffer, i: &[u16]) -> Result<()> {
        self.replace_indices(
            b,
            bytes(i),
            2,
            u32::from(i.iter().copied().max().unwrap_or(0)),
        )
    }
    fn replace_indices(
        &self,
        b: &mut IndexBuffer,
        data: &[u8],
        width: usize,
        maximum: u32,
    ) -> Result<()> {
        self.same(&b.owner)?;
        self.poll()?;
        if b.width != width || b.allocation.used() || data.len() > b.allocation.len() {
            return Err(Error::Invalid(
                "index replacement extent or active GPU reader",
            ));
        }
        b.allocation.write(0, data);
        b.count = data.len() / width;
        b.maximum = maximum;
        Ok(())
    }
    /// Capture top-left visible RGBA rows after this target's users complete.
    pub fn read_rgba(&self, target: &Framebuffer, output: &mut [u8]) -> Result<()> {
        self.same(&target.0.owner)?;
        if target.texture().format() != TextureFormat::Rgba8
            || output.len() != target.size()[0] as usize * target.size()[1] as usize * 4
        {
            return Err(Error::Invalid("capture extent"));
        }
        let t = &target.0.texture;
        self.await_allocation(&t.0.allocation)?;
        let row = target.size()[0] as usize * 4;
        for (y, dest) in output.chunks_exact_mut(row).enumerate() {
            let source = unsafe {
                core::slice::from_raw_parts(
                    t.0.allocation
                        .ptr()
                        .cast::<u8>()
                        .add(y * t.0.layout.row_stride_bytes),
                    row,
                )
            };
            dest.copy_from_slice(source);
        }
        Ok(())
    }
}
