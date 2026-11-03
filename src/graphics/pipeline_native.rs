//! SDK conversion kept separate from portable pipeline descriptors.
use super::*;
use vitasdk_sys::*;

fn depth(compare: Compare) -> SceGxmDepthFunc {
    match compare {
        Compare::Never => SCE_GXM_DEPTH_FUNC_NEVER,
        Compare::Less => SCE_GXM_DEPTH_FUNC_LESS,
        Compare::Equal => SCE_GXM_DEPTH_FUNC_EQUAL,
        Compare::LessEqual => SCE_GXM_DEPTH_FUNC_LESS_EQUAL,
        Compare::Greater => SCE_GXM_DEPTH_FUNC_GREATER,
        Compare::NotEqual => SCE_GXM_DEPTH_FUNC_NOT_EQUAL,
        Compare::GreaterEqual => SCE_GXM_DEPTH_FUNC_GREATER_EQUAL,
        Compare::Always => SCE_GXM_DEPTH_FUNC_ALWAYS,
    }
}

fn stencil(compare: Compare) -> SceGxmStencilFunc {
    match compare {
        Compare::Never => SCE_GXM_STENCIL_FUNC_NEVER,
        Compare::Less => SCE_GXM_STENCIL_FUNC_LESS,
        Compare::Equal => SCE_GXM_STENCIL_FUNC_EQUAL,
        Compare::LessEqual => SCE_GXM_STENCIL_FUNC_LESS_EQUAL,
        Compare::Greater => SCE_GXM_STENCIL_FUNC_GREATER,
        Compare::NotEqual => SCE_GXM_STENCIL_FUNC_NOT_EQUAL,
        Compare::GreaterEqual => SCE_GXM_STENCIL_FUNC_GREATER_EQUAL,
        Compare::Always => SCE_GXM_STENCIL_FUNC_ALWAYS,
    }
}

fn operation(op: StencilOp) -> SceGxmStencilOp {
    match op {
        StencilOp::Keep => SCE_GXM_STENCIL_OP_KEEP,
        StencilOp::Zero => SCE_GXM_STENCIL_OP_ZERO,
        StencilOp::Replace => SCE_GXM_STENCIL_OP_REPLACE,
        StencilOp::IncrementClamp => SCE_GXM_STENCIL_OP_INCR,
        StencilOp::DecrementClamp => SCE_GXM_STENCIL_OP_DECR,
        StencilOp::Invert => SCE_GXM_STENCIL_OP_INVERT,
        StencilOp::IncrementWrap => SCE_GXM_STENCIL_OP_INCR_WRAP,
        StencilOp::DecrementWrap => SCE_GXM_STENCIL_OP_DECR_WRAP,
    }
}

pub(super) fn apply(context: &Context, state: RasterState) {
    if !context.0.bindings.borrow_mut().raster(state) {
        return;
    }
    let c = context.0.core.native.context;
    let [front, back] = state.stencil.unwrap_or_default();
    // SAFETY: the calling draw has validated its owner and attachment, and a
    // scene is recording. Every field is applied so ordinary draws reset 3D state.
    unsafe {
        sceGxmSetCullMode(
            c,
            match state.cull {
                Cull::None => SCE_GXM_CULL_NONE,
                Cull::Clockwise => SCE_GXM_CULL_CW,
                Cull::CounterClockwise => SCE_GXM_CULL_CCW,
            },
        );
        sceGxmSetFrontDepthFunc(c, depth(state.depth_compare));
        sceGxmSetBackDepthFunc(c, depth(state.depth_compare));
        let write = if state.depth_write {
            SCE_GXM_DEPTH_WRITE_ENABLED
        } else {
            SCE_GXM_DEPTH_WRITE_DISABLED
        };
        sceGxmSetFrontDepthWriteEnable(c, write);
        sceGxmSetBackDepthWriteEnable(c, write);
        sceGxmSetFrontStencilRef(c, front.reference as u32);
        sceGxmSetBackStencilRef(c, back.reference as u32);
        sceGxmSetFrontStencilFunc(
            c,
            stencil(front.compare),
            operation(front.fail),
            operation(front.depth_fail),
            operation(front.pass),
            front.read_mask,
            front.write_mask,
        );
        sceGxmSetBackStencilFunc(
            c,
            stencil(back.compare),
            operation(back.fail),
            operation(back.depth_fail),
            operation(back.pass),
            back.read_mask,
            back.write_mask,
        );
    }
}

fn equation(value: BlendEquation) -> u8 {
    (match value {
        BlendEquation::Replace => SCE_GXM_BLEND_FUNC_NONE,
        BlendEquation::Add => SCE_GXM_BLEND_FUNC_ADD,
        BlendEquation::Subtract => SCE_GXM_BLEND_FUNC_SUBTRACT,
        BlendEquation::ReverseSubtract => SCE_GXM_BLEND_FUNC_REVERSE_SUBTRACT,
        BlendEquation::Min => SCE_GXM_BLEND_FUNC_MIN,
        BlendEquation::Max => SCE_GXM_BLEND_FUNC_MAX,
    }) as u8
}

fn factor(value: BlendFactor) -> u8 {
    (match value {
        BlendFactor::Zero => SCE_GXM_BLEND_FACTOR_ZERO,
        BlendFactor::One => SCE_GXM_BLEND_FACTOR_ONE,
        BlendFactor::SourceColor => SCE_GXM_BLEND_FACTOR_SRC_COLOR,
        BlendFactor::OneMinusSourceColor => SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_COLOR,
        BlendFactor::SourceAlpha => SCE_GXM_BLEND_FACTOR_SRC_ALPHA,
        BlendFactor::OneMinusSourceAlpha => SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
        BlendFactor::DestinationColor => SCE_GXM_BLEND_FACTOR_DST_COLOR,
        BlendFactor::OneMinusDestinationColor => SCE_GXM_BLEND_FACTOR_ONE_MINUS_DST_COLOR,
        BlendFactor::DestinationAlpha => SCE_GXM_BLEND_FACTOR_DST_ALPHA,
        BlendFactor::OneMinusDestinationAlpha => SCE_GXM_BLEND_FACTOR_ONE_MINUS_DST_ALPHA,
        BlendFactor::SourceAlphaSaturate => SCE_GXM_BLEND_FACTOR_SRC_ALPHA_SATURATE,
        BlendFactor::DestinationAlphaSaturate => SCE_GXM_BLEND_FACTOR_DST_ALPHA_SATURATE,
    }) as u8
}

pub(super) fn blend(value: Blend) -> SceGxmBlendInfo {
    let state = match value {
        Blend::Replace => BlendState::default(),
        Blend::Premultiplied => BlendState {
            color: BlendEquation::Add,
            alpha: BlendEquation::Add,
            color_destination: BlendFactor::OneMinusSourceAlpha,
            alpha_destination: BlendFactor::OneMinusSourceAlpha,
            ..Default::default()
        },
        Blend::Custom(state) => state,
    };
    let mut info: SceGxmBlendInfo = unsafe { core::mem::zeroed() };
    for (write, mask) in state.write.into_iter().zip([
        SCE_GXM_COLOR_MASK_R,
        SCE_GXM_COLOR_MASK_G,
        SCE_GXM_COLOR_MASK_B,
        SCE_GXM_COLOR_MASK_A,
    ]) {
        if write {
            info.colorMask |= mask as u8;
        }
    }
    info.set_colorFunc(equation(state.color));
    info.set_alphaFunc(equation(state.alpha));
    info.set_colorSrc(factor(state.color_source));
    info.set_alphaSrc(factor(state.alpha_source));
    info.set_colorDst(factor(state.color_destination));
    info.set_alphaDst(factor(state.alpha_destination));
    info
}
