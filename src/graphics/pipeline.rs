//! Per-draw raster, depth/stencil and prepared color-output contracts.

/// Comparison shared by depth and stencil tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compare {
    /// Reject every sample.
    Never,
    /// Source is less than destination.
    Less,
    /// Source equals destination.
    Equal,
    /// Source is less than or equal to destination.
    LessEqual,
    /// Source is greater than destination.
    Greater,
    /// Source differs from destination.
    NotEqual,
    /// Source is greater than or equal to destination.
    GreaterEqual,
    /// Accept every sample.
    #[default]
    Always,
}

/// Update to an eight-bit stencil sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StencilOp {
    /// Preserve the sample.
    #[default]
    Keep,
    /// Write zero.
    Zero,
    /// Write the reference value.
    Replace,
    /// Increment, clamping at 255.
    IncrementClamp,
    /// Decrement, clamping at zero.
    DecrementClamp,
    /// Invert all bits.
    Invert,
    /// Increment modulo 256.
    IncrementWrap,
    /// Decrement modulo 256.
    DecrementWrap,
}

/// Stencil comparison, reference and independent read/write masks for one face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StencilFace {
    /// Comparison of masked reference against masked stored stencil.
    pub compare: Compare,
    /// Value used by the comparison and replacement operation.
    pub reference: u8,
    /// Bits participating in the comparison.
    pub read_mask: u8,
    /// Bits that stencil operations can replace.
    pub write_mask: u8,
    /// Operation when stencil comparison fails.
    pub fail: StencilOp,
    /// Operation when stencil passes and depth fails.
    pub depth_fail: StencilOp,
    /// Operation when both comparisons pass.
    pub pass: StencilOp,
}
impl Default for StencilFace {
    fn default() -> Self {
        Self {
            compare: Compare::Always,
            reference: 0,
            read_mask: 255,
            write_mask: 0,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Keep,
        }
    }
}

/// Winding removed by the native rasterizer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cull {
    /// Keep both windings.
    #[default]
    None,
    /// Remove clockwise triangles in native viewport coordinates.
    Clockwise,
    /// Remove counterclockwise triangles in native viewport coordinates.
    CounterClockwise,
}

/// Complete native raster state applied by `Context::draw_with_state`.
/// Defaults preserve the ordinary two-dimensional drawing contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterState {
    /// Face winding removed before shading.
    pub cull: Cull,
    /// Depth comparison for both windings.
    pub depth_compare: Compare,
    /// Store depth for samples passing both tests.
    pub depth_write: bool,
    /// Front and back stencil configuration; absence disables stencil updates.
    pub stencil: Option<[StencilFace; 2]>,
}
impl RasterState {
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn requires_depth_stencil(self) -> bool {
        self.depth_write || self.depth_compare != Compare::Always || self.stencil.is_some()
    }
}

/// Native fixed-function color equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendEquation {
    /// Replace the destination with the source.
    Replace,
    /// Add weighted source and destination.
    Add,
    /// Weighted source minus weighted destination.
    Subtract,
    /// Weighted destination minus weighted source.
    ReverseSubtract,
    /// Component-wise minimum.
    Min,
    /// Component-wise maximum.
    Max,
}

/// Native per-component multiplier. Constant colors require shader generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendFactor {
    /// Zero.
    Zero,
    /// One.
    One,
    /// Source color.
    SourceColor,
    /// One minus source color.
    OneMinusSourceColor,
    /// Source alpha.
    SourceAlpha,
    /// One minus source alpha.
    OneMinusSourceAlpha,
    /// Destination color.
    DestinationColor,
    /// One minus destination color.
    OneMinusDestinationColor,
    /// Destination alpha.
    DestinationAlpha,
    /// One minus destination alpha.
    OneMinusDestinationAlpha,
    /// Native saturated source alpha factor.
    SourceAlphaSaturate,
    /// Native saturated destination alpha factor.
    DestinationAlphaSaturate,
}

/// Explicit output combination patched into an offline fragment program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlendState {
    /// Independent RGB equation.
    pub color: BlendEquation,
    /// Independent alpha equation.
    pub alpha: BlendEquation,
    /// Source RGB multiplier.
    pub color_source: BlendFactor,
    /// Destination RGB multiplier.
    pub color_destination: BlendFactor,
    /// Source alpha multiplier.
    pub alpha_source: BlendFactor,
    /// Destination alpha multiplier.
    pub alpha_destination: BlendFactor,
    /// Enabled output channels in RGBA order.
    pub write: [bool; 4],
}
impl Default for BlendState {
    fn default() -> Self {
        Self {
            color: BlendEquation::Replace,
            alpha: BlendEquation::Replace,
            color_source: BlendFactor::One,
            color_destination: BlendFactor::Zero,
            alpha_source: BlendFactor::One,
            alpha_destination: BlendFactor::Zero,
            write: [true; 4],
        }
    }
}

/// Single-sample DF32/S8 storage, padded to complete native 32-by-32 tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthStencilLayout {
    /// Padded row stride in samples.
    pub stride: u32,
    /// Padded number of stored rows.
    pub rows: u32,
    /// Bytes in the floating-point depth plane.
    pub depth_bytes: usize,
    /// Bytes in the eight-bit stencil plane.
    pub stencil_bytes: usize,
}

/// Calculate the same extent used to allocate native depth and stencil planes.
pub fn depth_stencil_layout(size: [u32; 2]) -> Result<DepthStencilLayout, super::Error> {
    super::layout::validate_size(size)?;
    let stride = size[0].next_multiple_of(32);
    let rows = size[1].next_multiple_of(32);
    let samples = stride as usize * rows as usize;
    Ok(DepthStencilLayout {
        stride,
        rows,
        depth_bytes: samples * 4,
        stencil_bytes: samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_planes_cover_partial_edge_tiles_and_validate_limits() {
        let layout = depth_stencil_layout([481, 273]).unwrap();
        assert_eq!((layout.stride, layout.rows), (512, 288));
        assert_eq!(layout.depth_bytes, 512 * 288 * 4);
        assert_eq!(layout.stencil_bytes, 512 * 288);
        assert!(depth_stencil_layout([0, 272]).is_err());
        assert!(depth_stencil_layout([4097, 272]).is_err());
    }

    #[test]
    fn every_observable_depth_or_stencil_use_requires_storage() {
        assert!(!RasterState::default().requires_depth_stencil());
        assert!(
            RasterState {
                depth_write: true,
                ..Default::default()
            }
            .requires_depth_stencil()
        );
        assert!(
            RasterState {
                depth_compare: Compare::Never,
                ..Default::default()
            }
            .requires_depth_stencil()
        );
        assert!(
            RasterState {
                stencil: Some([StencilFace::default(); 2]),
                ..Default::default()
            }
            .requires_depth_stencil()
        );
    }
}
