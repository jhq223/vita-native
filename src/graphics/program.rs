//! Offline GXP ownership, reflected parameters and prepared output variants.
use super::types::Result;
use super::{
    device::Core,
    memory::{Fault, sdk},
    *,
};
use std::{ffi::CString, mem::ManuallyDrop, prelude::v1::*, rc::Rc};
use vitasdk_sys::*;
struct Binary {
    data: ManuallyDrop<Box<[u32]>>,
    fault: Fault,
}
impl Binary {
    fn new(code: ShaderCode<'_>, fault: &Fault) -> Result<Self> {
        let ShaderCode::GxpBinary(bytes) = code;
        let bytes = super::gxp::payload(bytes)?;
        let mut aligned = std::vec![0u32;bytes.len().div_ceil(4)];
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                aligned.as_mut_ptr().cast(),
                bytes.len(),
            );
        }
        let b = Self {
            data: ManuallyDrop::new(aligned.into_boxed_slice()),
            fault: fault.clone(),
        };
        sdk("sceGxmProgramCheck", unsafe { sceGxmProgramCheck(b.ptr()) })?;
        Ok(b)
    }
    fn ptr(&self) -> *const SceGxmProgram {
        self.data.as_ptr().cast()
    }
}
impl Drop for Binary {
    fn drop(&mut self) {
        if self.fault.get().is_none() {
            unsafe {
                ManuallyDrop::drop(&mut self.data);
            }
        }
    }
}
pub(super) struct ProgramInner {
    vertex_code: Binary,
    fragment_code: Binary,
    vertex_id: SceGxmShaderPatcherId,
    fragment_id: SceGxmShaderPatcherId,
    pub vertex: [*mut SceGxmVertexProgram; 2],
    pub fragment: [*mut SceGxmFragmentProgram; 2],
    pub clip: Option<*const SceGxmProgramParameter>,
    pub uniform_sizes: [u32; 2],
    pub texture_slots: usize,
    pub stride: u32,
    pub attributes: Vec<VertexAttribute>,
    pub owner: Rc<Core>,
}
impl Drop for ProgramInner {
    fn drop(&mut self) {
        let f = &self.owner.fault;
        if f.get().is_some() {
            return;
        }
        let p = self.owner.native.patcher;
        for fragment in self.fragment {
            if !fragment.is_null()
                && f.command("release fragment GXP", unsafe {
                    sceGxmShaderPatcherReleaseFragmentProgram(p, fragment)
                })
                .is_err()
            {
                return;
            }
        }
        for vertex in self.vertex {
            if !vertex.is_null()
                && f.command("release vertex GXP", unsafe {
                    sceGxmShaderPatcherReleaseVertexProgram(p, vertex)
                })
                .is_err()
            {
                return;
            }
        }
        for id in [self.fragment_id, self.vertex_id] {
            if !id.is_null()
                && f.command("unregister GXP", unsafe {
                    sceGxmShaderPatcherUnregisterProgram(p, id)
                })
                .is_err()
            {
                return;
            }
        }
    }
}
/// Prepared vertex/index-width and blend variants; no runtime source compilation.
#[derive(Clone)]
pub struct Program(pub(super) Rc<ProgramInner>);
#[derive(Clone, Copy)]
pub(super) struct Parameter {
    pub raw: *const SceGxmProgramParameter,
    pub fragment: bool,
    pub sampler: bool,
    pub resource: u32,
    pub components: u32,
}
/// Reflected immutable binding; keeps the owning GXP bytes and patched programs alive.
#[derive(Clone)]
pub struct Uniform {
    pub(super) program: Program,
    pub(super) parameters: Vec<Parameter>,
}
impl Program {
    /// Reflect a numeric/default-buffer or fixed TEXUNIT binding once; `[0]` names normalize.
    pub fn uniform(&self, name: &str) -> Option<Uniform> {
        let name = CString::new(name.strip_suffix("[0]").unwrap_or(name)).ok()?;
        let mut parameters = Vec::with_capacity(2);
        for (fragment, code) in [(false, &self.0.vertex_code), (true, &self.0.fragment_code)] {
            let raw = unsafe { sceGxmProgramFindParameterByName(code.ptr(), name.as_ptr()) };
            if raw.is_null() {
                continue;
            }
            let category = unsafe { sceGxmProgramParameterGetCategory(raw) };
            if category != SCE_GXM_PARAMETER_CATEGORY_UNIFORM
                && category != SCE_GXM_PARAMETER_CATEGORY_SAMPLER
            {
                continue;
            }
            parameters.push(Parameter {
                raw,
                fragment,
                sampler: category == SCE_GXM_PARAMETER_CATEGORY_SAMPLER,
                resource: unsafe { sceGxmProgramParameterGetResourceIndex(raw) },
                components: unsafe {
                    sceGxmProgramParameterGetComponentCount(raw)
                        * sceGxmProgramParameterGetArraySize(raw)
                },
            });
        }
        (!parameters.is_empty()).then(|| Uniform {
            program: self.clone(),
            parameters,
        })
    }
}
#[derive(Clone, Copy)]
/// Borrowed values consumed synchronously into native uniform ring storage.
pub enum UniformValue<'a> {
    /// Numeric integer converted to float, or a validated compiled TEXUNIT selector.
    Int(&'a Uniform, i32),
    /// One float component.
    Float(&'a Uniform, f32),
    /// Two float components.
    Vec2(&'a Uniform, [f32; 2]),
    /// Three float components.
    Vec3(&'a Uniform, [f32; 3]),
    /// Four float components.
    Vec4(&'a Uniform, [f32; 4]),
    /// A contiguous array of float4 values.
    Vec4Array(&'a Uniform, &'a [[f32; 4]]),
}
impl<'a> UniformValue<'a> {
    pub(super) fn uniform(self) -> &'a Uniform {
        match self {
            Self::Int(u, _)
            | Self::Float(u, _)
            | Self::Vec2(u, _)
            | Self::Vec3(u, _)
            | Self::Vec4(u, _)
            | Self::Vec4Array(u, _) => u,
        }
    }
    pub(super) fn upload(
        self,
        vertex: *mut core::ffi::c_void,
        fragment: *mut core::ffi::c_void,
    ) -> Result<()> {
        let mut local = [0f32; 4];
        let (source, count, int) = match self {
            Self::Int(_, v) => {
                local[0] = v as f32;
                (local.as_ptr(), 1, Some(v))
            }
            Self::Float(_, v) => {
                local[0] = v;
                (local.as_ptr(), 1, None)
            }
            Self::Vec2(_, v) => {
                local[..2].copy_from_slice(&v);
                (local.as_ptr(), 2, None)
            }
            Self::Vec3(_, v) => {
                local[..3].copy_from_slice(&v);
                (local.as_ptr(), 3, None)
            }
            Self::Vec4(_, v) => {
                local = v;
                (local.as_ptr(), 4, None)
            }
            Self::Vec4Array(_, v) => (v.as_ptr().cast(), v.len() * 4, None),
        };
        for p in &self.uniform().parameters {
            if p.sampler {
                if int != Some(p.resource as i32) {
                    return Err(Error::Invalid("sampler differs from compiled TEXUNIT"));
                }
                continue;
            }
            if count > p.components as usize {
                return Err(Error::Invalid("uniform component extent"));
            }
            sdk("sceGxmSetUniformDataF", unsafe {
                sceGxmSetUniformDataF(
                    if p.fragment { fragment } else { vertex },
                    p.raw,
                    0,
                    count as u32,
                    source,
                )
            })?;
        }
        Ok(())
    }
}
impl Context {
    /// Patch both index widths and both blend modes once. Stride and offsets use f32 elements.
    pub fn create_program(&self, descriptor: ProgramDescriptor<'_>) -> Result<Program> {
        let ProgramDescriptor {
            vertex,
            fragment,
            bindings,
            stride,
            attributes,
            clip_uniform,
        } = descriptor;
        self.check("program creation")?;
        if stride == 0
            || stride > 16383
            || attributes.iter().any(|a| {
                a.location >= 16
                    || a.components == 0
                    || a.components > 4
                    || a.offset
                        .checked_add(a.components)
                        .is_none_or(|v| v > stride)
            })
        {
            return Err(Error::Invalid("program vertex layout"));
        }
        let v = Binary::new(vertex, &self.0.core.fault)?;
        let f = Binary::new(fragment, &self.0.core.fault)?;
        if unsafe { sceGxmProgramGetType(v.ptr()) } != SCE_GXM_VERTEX_PROGRAM
            || unsafe { sceGxmProgramGetType(f.ptr()) } != SCE_GXM_FRAGMENT_PROGRAM
        {
            return Err(Error::Invalid("GXP program stage"));
        }
        let clip = if let Some(name) = clip_uniform {
            let name = CString::new(name).map_err(|_| Error::Invalid("clip uniform name"))?;
            // SAFETY: the validated program and terminated parameter name remain live.
            let clip = unsafe { sceGxmProgramFindParameterByName(f.ptr(), name.as_ptr()) };
            if clip.is_null()
                || unsafe { sceGxmProgramParameterGetCategory(clip) }
                    != SCE_GXM_PARAMETER_CATEGORY_UNIFORM
                || unsafe { sceGxmProgramParameterGetComponentCount(clip) } != 4
                || unsafe { sceGxmProgramParameterGetArraySize(clip) } != 1
            {
                return Err(Error::Invalid("clip uniform must be one float4"));
            }
            Some(clip)
        } else {
            None
        };
        let uniform_sizes = unsafe {
            [
                sceGxmProgramGetDefaultUniformBufferSize(v.ptr()),
                sceGxmProgramGetDefaultUniformBufferSize(f.ptr()),
            ]
        };
        let texture_slots = (0..unsafe { sceGxmProgramGetParameterCount(f.ptr()) })
            .filter_map(|index| unsafe {
                let parameter = sceGxmProgramGetParameter(f.ptr(), index);
                (sceGxmProgramParameterGetCategory(parameter) == SCE_GXM_PARAMETER_CATEGORY_SAMPLER)
                    .then(|| {
                        sceGxmProgramParameterGetResourceIndex(parameter) as usize
                            + sceGxmProgramParameterGetArraySize(parameter) as usize
                    })
            })
            .max()
            .unwrap_or(0);
        let mut inner = ProgramInner {
            uniform_sizes,
            texture_slots,
            vertex_code: v,
            fragment_code: f,
            vertex_id: core::ptr::null_mut(),
            fragment_id: core::ptr::null_mut(),
            vertex: [core::ptr::null_mut(); 2],
            fragment: [core::ptr::null_mut(); 2],
            clip,
            stride,
            attributes: attributes.to_vec(),
            owner: self.0.core.clone(),
        };
        let p = self.0.core.native.patcher;
        sdk("register vertex GXP", unsafe {
            sceGxmShaderPatcherRegisterProgram(p, inner.vertex_code.ptr(), &mut inner.vertex_id)
        })?;
        sdk("register fragment GXP", unsafe {
            sceGxmShaderPatcherRegisterProgram(p, inner.fragment_code.ptr(), &mut inner.fragment_id)
        })?;
        let mut native_attributes = Vec::with_capacity(attributes.len());
        for b in bindings {
            let name = CString::new(b.name).map_err(|_| Error::Invalid("attribute name"))?;
            let param =
                unsafe { sceGxmProgramFindParameterByName(inner.vertex_code.ptr(), name.as_ptr()) };
            if param.is_null() {
                continue;
            }
            if unsafe { sceGxmProgramParameterGetCategory(param) }
                != SCE_GXM_PARAMETER_CATEGORY_ATTRIBUTE
            {
                return Err(Error::Invalid("vertex input category"));
            }
            let a = attributes
                .iter()
                .find(|a| a.location == b.location)
                .ok_or(Error::Invalid("vertex attribute binding"))?;
            native_attributes.push(SceGxmVertexAttribute {
                streamIndex: 0,
                offset: (a.offset * 4) as u16,
                format: SCE_GXM_ATTRIBUTE_FORMAT_F32 as u8,
                componentCount: a.components as u8,
                regIndex: unsafe { sceGxmProgramParameterGetResourceIndex(param) } as u16,
            });
        }
        for (index, source) in [
            SCE_GXM_INDEX_SOURCE_INDEX_16BIT,
            SCE_GXM_INDEX_SOURCE_INDEX_32BIT,
        ]
        .into_iter()
        .enumerate()
        {
            let stream = SceGxmVertexStream {
                stride: (stride * 4) as u16,
                indexSource: source as u16,
            };
            sdk("patch vertex GXP", unsafe {
                sceGxmShaderPatcherCreateVertexProgram(
                    p,
                    inner.vertex_id,
                    native_attributes.as_ptr(),
                    native_attributes.len() as u32,
                    &stream,
                    1,
                    &mut inner.vertex[index],
                )
            })?;
        }
        for blend in 0..2 {
            let mut info: SceGxmBlendInfo = unsafe { core::mem::zeroed() };
            info.colorMask = SCE_GXM_COLOR_MASK_ALL as u8;
            info.set_colorFunc(if blend == 0 {
                SCE_GXM_BLEND_FUNC_NONE
            } else {
                SCE_GXM_BLEND_FUNC_ADD
            } as u8);
            info.set_alphaFunc(if blend == 0 {
                SCE_GXM_BLEND_FUNC_NONE
            } else {
                SCE_GXM_BLEND_FUNC_ADD
            } as u8);
            info.set_colorSrc(SCE_GXM_BLEND_FACTOR_ONE as u8);
            info.set_alphaSrc(SCE_GXM_BLEND_FACTOR_ONE as u8);
            info.set_colorDst(if blend == 0 {
                SCE_GXM_BLEND_FACTOR_ZERO
            } else {
                SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA
            } as u8);
            info.set_alphaDst(if blend == 0 {
                SCE_GXM_BLEND_FACTOR_ZERO
            } else {
                SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA
            } as u8);
            sdk("patch fragment GXP", unsafe {
                sceGxmShaderPatcherCreateFragmentProgram(
                    p,
                    inner.fragment_id,
                    SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
                    SCE_GXM_MULTISAMPLE_NONE,
                    &info,
                    inner.vertex_code.ptr(),
                    &mut inner.fragment[blend],
                )
            })?;
        }
        Ok(Program(Rc::new(inner)))
    }
}
