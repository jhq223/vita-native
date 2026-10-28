//! Ordered direct GXM draw records with exact fragment clipping.
use super::types::Result;
use super::{
    device::{BoundProgram, Clear, Lease, Recording},
    *,
};
use std::{ops::Range, prelude::v1::*, rc::Rc};
use vitasdk_sys::*;
/// One fully specified direct device draw. Scissor uses top-left x/y/width/height.
pub struct Draw<'a> {
    /// Physical output or the current process scanout buffer.
    pub target: Target<'a>,
    /// Logical viewport extent, bounded by physical storage.
    pub size: [u32; 2],
    /// Prepared GXP variants and reflected interface.
    pub program: &'a Program,
    /// Immutable interleaved float input.
    pub vertices: &'a VertexBuffer,
    /// Optional elements; absent input uses bounded reusable sequential indices.
    pub indices: Option<&'a IndexBuffer>,
    /// Half-open element or sequential-vertex range.
    pub range: Range<u32>,
    /// Fragment texture units in compiled TEXUNIT order, covering every declared sampler.
    pub textures: &'a [&'a Texture],
    /// Complete numeric/fixed-sampler values for this draw.
    pub uniforms: &'a [UniformValue<'a>],
    /// Prepared output-combination variant.
    pub blend: Blend,
    /// Optional half-open top-left x/y/width/height rectangle. Internal edges
    /// need 32-pixel alignment unless the program declares a clip uniform.
    pub scissor: Option<[u32; 4]>,
}
impl Context {
    pub(super) fn init_clear(&self, clear: ClearProgram<'_>) -> Result<()> {
        let program = self.create_program(ProgramDescriptor {
            vertex: clear.vertex,
            fragment: clear.fragment,
            bindings: &[AttributeBinding {
                location: 0,
                name: clear.position_attribute,
            }],
            stride: 2,
            attributes: &[VertexAttribute {
                location: 0,
                components: 2,
                offset: 0,
            }],
            clip_uniform: Some(clear.clip_uniform),
        })?;
        let color = program
            .uniform(clear.color_uniform)
            .ok_or(Error::Invalid("clear color uniform"))?;
        let vertices = self.vertex_buffer(
            &[-1., -1., 3., -1., -1., 3.],
            2,
            &[VertexAttribute {
                location: 0,
                components: 2,
                offset: 0,
            }],
        )?;
        *self.0.clear.borrow_mut() = Some(Clear {
            program,
            vertices,
            color,
        });
        Ok(())
    }
    fn target(&self, target: Target<'_>) -> Result<Framebuffer> {
        match target {
            Target::Framebuffer(f) => {
                self.same(&f.0.owner)?;
                Ok(f.clone())
            }
            Target::Screen => {
                let s = self.0.state.borrow();
                Ok(self.0.screens.borrow()[s.screen].clone())
            }
        }
    }
    fn begin(&self, target: Framebuffer, size: [u32; 2]) -> Result<()> {
        let mut s = self.0.state.borrow_mut();
        if let Some(recording) = &mut s.recording
            && Rc::ptr_eq(&recording.target.0, &target.0)
        {
            if recording.size != size {
                self.viewport(size);
                recording.size = size;
            }
            return Ok(());
        }
        self.end_scene(&mut s)?;
        let (slot, n) = self.reserve_slot(&mut s)?;
        let region = SceGxmValidRegion {
            xMax: target.size()[0] - 1,
            yMax: target.size()[1] - 1,
        };
        let code = unsafe {
            sceGxmBeginScene(
                self.0.core.native.context,
                SCE_GXM_SCENE_FRAGMENT_TRANSFER_SYNC,
                target.0.target.raw,
                &region,
                core::ptr::null_mut(),
                target.0.sync,
                &target.0.surface,
                core::ptr::null(),
            )
        };
        let allocation = target.0.texture.0.allocation.clone();
        s.recording = Some(Recording {
            target: target.clone(),
            leases: std::vec![Lease::Allocation(allocation), Lease::Target(target)],
            slot,
            notification: n.value,
            size,
        });
        self.command("sceGxmBeginScene", code)?;
        self.0.bindings.borrow_mut().begin_scene();
        self.viewport(size);
        Ok(())
    }
    fn viewport(&self, size: [u32; 2]) {
        if !self.0.bindings.borrow_mut().viewport(size) {
            return;
        }
        // SAFETY: recording owns a live immediate context. Viewport changes are
        // scene state; the valid region and color-surface clip retain storage bounds.
        unsafe {
            let c = self.0.core.native.context;
            sceGxmSetViewport(
                c,
                size[0] as f32 * 0.5,
                size[0] as f32 * 0.5,
                size[1] as f32 * 0.5,
                -(size[1] as f32) * 0.5,
                0.5,
                0.5,
            );
        }
    }
    /// Clear the entire physical target through the prepared production GXP program.
    pub fn clear(&self, target: Target<'_>, color: [f32; 4]) -> Result<()> {
        let size = match target {
            Target::Screen => self.output_size(),
            Target::Framebuffer(f) => f.size(),
        };
        let clear = self.0.clear.borrow();
        let clear = clear
            .as_ref()
            .ok_or(Error::Invalid("clear program initialization"))?;
        self.draw(Draw {
            target,
            size,
            program: &clear.program,
            vertices: &clear.vertices,
            indices: None,
            range: 0..3,
            textures: &[],
            uniforms: &[UniformValue::Vec4(&clear.color, color)],
            blend: Blend::Replace,
            scissor: None,
        })
    }
    /// Submit a direct draw with linear minification and magnification.
    pub fn draw(&self, draw: Draw<'_>) -> Result<()> {
        self.draw_filtered(draw, TextureFilter::Linear)
    }
    /// Submit one indexed hardware draw, retaining its exact owners through fragment completion.
    pub fn draw_filtered(&self, d: Draw<'_>, filter: TextureFilter) -> Result<()> {
        self.same(&d.program.0.owner)?;
        self.same(&d.vertices.owner)?;
        if d.size.contains(&0)
            || d.range.start > d.range.end
            || d.textures.len() > 16
            || d.textures.len() < d.program.0.texture_slots
            || d.program.0.stride != d.vertices.stride
            || d.program.0.attributes != d.vertices.attributes
        {
            return Err(Error::Invalid("draw layout or range"));
        }
        let target = self.target(d.target)?;
        if (0..2).any(|i| d.size[i] > target.size()[i]) {
            return Err(Error::Invalid("logical viewport exceeds target"));
        }
        let scissor = d.scissor.unwrap_or([0, 0, d.size[0], d.size[1]]);
        let min = [scissor[0].min(d.size[0]), scissor[1].min(d.size[1])];
        let max = [
            scissor[0].saturating_add(scissor[2]).min(d.size[0]),
            scissor[1].saturating_add(scissor[3]).min(d.size[1]),
        ];
        if d.range.is_empty() || min[0] >= max[0] || min[1] >= max[1] {
            return Ok(());
        }
        if d.program.0.clip.is_none() && super::types::needs_fragment_clip(min, max, target.size())
        {
            return Err(Error::Unsupported("pixel scissor requires a clip uniform"));
        }
        for t in d.textures {
            self.same(&t.0.owner)?;
            if Rc::ptr_eq(&t.0, &target.0.texture.0) {
                return Err(Error::Invalid("same scene texture feedback"));
            }
        }
        for u in d.uniforms {
            if !Rc::ptr_eq(&u.uniform().program.0, &d.program.0) {
                return Err(Error::Invalid("foreign program uniform"));
            }
        }
        let (indices, index_width) = if let Some(i) = d.indices {
            self.same(&i.owner)?;
            if d.range.end as usize > i.count || i.maximum as usize >= d.vertices.count {
                return Err(Error::Invalid("indexed draw extent"));
            }
            (i.allocation.clone(), i.width)
        } else {
            if d.range.end as usize > d.vertices.count || d.range.end > 1_048_576 {
                return Err(Error::Invalid("sequential draw extent"));
            }
            let mut identity = self.0.identity.borrow_mut();
            let count = (d.range.end.max(1024) as usize).next_power_of_two();
            if identity.as_ref().is_none_or(|a| a.len() < count * 4) {
                let a = self.0.core.buffers.alloc(count * 4, 4)?;
                let words =
                    unsafe { core::slice::from_raw_parts_mut(a.ptr().cast::<u32>(), count) };
                for (i, w) in words.iter_mut().enumerate() {
                    *w = i as u32;
                }
                *identity = Some(a);
            }
            (identity.as_ref().expect("identity indices").clone(), 4)
        };
        self.begin(target, d.size)?;
        let mut s = self.0.state.borrow_mut();
        let r = s.recording.as_mut().expect("active scene");
        Self::retain(r, Lease::Program(d.program.clone()));
        Self::retain(r, Lease::Allocation(d.vertices.allocation.clone()));
        Self::retain(r, Lease::Allocation(indices.clone()));
        for t in d.textures {
            Self::retain(r, Lease::Allocation(t.0.allocation.clone()));
        }
        let c = self.0.core.native.context;
        let variants = [
            usize::from(index_width == 4),
            usize::from(d.blend == Blend::Premultiplied),
        ];
        let mut bound = self.0.bound_program.borrow_mut();
        if bound
            .as_ref()
            .is_none_or(|b| !Rc::ptr_eq(&b.program.0, &d.program.0) || b.variants != variants)
        {
            unsafe {
                sceGxmSetVertexProgram(c, d.program.0.vertex[variants[0]]);
                sceGxmSetFragmentProgram(c, d.program.0.fragment[variants[1]]);
            }
            // Context bindings outlive submissions; replace ownership after both native binds.
            *bound = Some(BoundProgram {
                program: d.program.clone(),
                variants,
            });
        }
        drop(bound);
        let mut bindings = self.0.bindings.borrow_mut();
        if bindings.clip([min[0], min[1], max[0], max[1]]) {
            unsafe {
                sceGxmSetRegionClip(
                    c,
                    SCE_GXM_REGION_CLIP_OUTSIDE,
                    min[0],
                    min[1],
                    max[0] - 1,
                    max[1] - 1,
                );
            }
        }
        let mut vertex = core::ptr::null_mut();
        let mut fragment = core::ptr::null_mut();
        let sizes = d.program.0.uniform_sizes;
        if sizes[0] > 0 {
            self.command("reserve vertex uniforms", unsafe {
                sceGxmReserveVertexDefaultUniformBuffer(c, &mut vertex)
            })?;
        }
        if sizes[1] > 0 {
            self.command("reserve fragment uniforms", unsafe {
                sceGxmReserveFragmentDefaultUniformBuffer(c, &mut fragment)
            })?;
        }
        for u in d.uniforms {
            u.upload(vertex, fragment)?;
        }
        if let Some(parameter) = d.program.0.clip {
            let clip = [min[0] as f32, min[1] as f32, max[0] as f32, max[1] as f32];
            parameter.upload(&clip, vertex, fragment)?;
        }
        for (unit, t) in d.textures.iter().enumerate() {
            let mut raw = t.0.raw.get();
            // SAFETY: SceGxmTexture is four initialized control words. Its C
            // layout consists of two 32-bit unions and one 64-bit bitfield,
            // without padding. Constructors zero the complete descriptor.
            let descriptor = unsafe { core::mem::transmute::<SceGxmTexture, [u32; 4]>(raw) };
            if !bindings.texture(unit, descriptor, filter) {
                continue;
            }
            let f = match filter {
                TextureFilter::Nearest => SCE_GXM_TEXTURE_FILTER_POINT,
                TextureFilter::Linear => SCE_GXM_TEXTURE_FILTER_LINEAR,
            };
            self.command("texture mag filter", unsafe {
                sceGxmTextureSetMagFilter(&mut raw, f)
            })?;
            if !t.0.strided {
                self.command("texture min filter", unsafe {
                    sceGxmTextureSetMinFilter(&mut raw, f)
                })?;
            }
            if !t.0.strided {
                self.command("texture mip filter", unsafe {
                    sceGxmTextureSetMipFilter(
                        &mut raw,
                        if t.mip_count() > 1 && matches!(filter, TextureFilter::Linear) {
                            SCE_GXM_TEXTURE_MIP_FILTER_ENABLED
                        } else {
                            SCE_GXM_TEXTURE_MIP_FILTER_DISABLED
                        },
                    )
                })?;
            }
            self.command("fragment texture", unsafe {
                sceGxmSetFragmentTexture(c, unit as u32, &raw)
            })?;
        }
        if bindings.vertex(d.vertices.allocation.ptr() as usize) {
            self.command("vertex stream", unsafe {
                sceGxmSetVertexStream(c, 0, d.vertices.allocation.ptr().cast())
            })?;
        }
        self.command("sceGxmDraw", unsafe {
            sceGxmDraw(
                c,
                SCE_GXM_PRIMITIVE_TRIANGLES,
                if index_width == 2 {
                    SCE_GXM_INDEX_FORMAT_U16
                } else {
                    SCE_GXM_INDEX_FORMAT_U32
                },
                indices
                    .ptr()
                    .cast::<u8>()
                    .add(d.range.start as usize * index_width)
                    .cast(),
                d.range.end - d.range.start,
            )
        })
    }
}
