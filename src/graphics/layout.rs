//! One layout calculation drives mapping, texture views and resource accounting.
use super::{Error, TextureFormat, types::Result};
/// Exact single-sample RGBA8 storage including the strided-view row padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetLayout {
    /// Color payload bytes, including the actual row pitch.
    pub texture_bytes: u64,
    /// Distance in bytes between consecutive stored rows.
    pub row_stride_bytes: u64,
}
/// Target color storage and independently owned firmware scheduling storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetFootprint {
    /// RGBA8 color layout consumed by target planning.
    pub texture: TargetLayout,
    /// Uncached kernel capacity for one shared scheduling class at this extent.
    pub render_target_driver_bytes: Option<u64>,
}
/// Exact mapped texture payload. BC dimensions are physical power-of-two extents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureLayout {
    /// Exact mapped payload bytes; excludes suballocator alignment gaps.
    pub bytes: usize,
    /// Distance in bytes between consecutive stored rows.
    pub row_stride_bytes: usize,
}
pub(crate) fn validate_size(size: [u32; 2]) -> Result<()> {
    if size.contains(&0) || size.iter().any(|&v| v > 4096) {
        return Err(Error::Invalid("texture size (1..4096)"));
    }
    Ok(())
}
/// Pure RGBA8 render-target layout; color surfaces require a two-pixel stride.
pub fn target_layout(size: [u32; 2]) -> Result<TargetLayout> {
    validate_size(size)?;
    let row = (size[0] as u64).next_multiple_of(2) * 4;
    Ok(TargetLayout {
        texture_bytes: row * u64::from(size[1]),
        row_stride_bytes: row,
    })
}
/// Pure ordinary sampled-texture layout, used before native allocation/admission.
/// Linear row pitch rounds width to eight texels; compressed bytes remain block payload.
pub fn texture_layout(size: [u32; 2], format: TextureFormat) -> Result<TextureLayout> {
    validate_size(size)?;
    if format.compressed() {
        if size.iter().any(|v| !v.is_power_of_two() || *v < 4) {
            return Err(Error::Invalid("BC physical power-of-two dimensions"));
        }
        let row = size[0] as usize / 4
            * if format == TextureFormat::Bc1Rgb {
                8
            } else {
                16
            };
        return Ok(TextureLayout {
            bytes: row * size[1] as usize / 4,
            row_stride_bytes: row,
        });
    }
    let row = (size[0] as usize).next_multiple_of(8) * format.pixel_bytes();
    Ok(TextureLayout {
        bytes: row * size[1] as usize,
        row_stride_bytes: row,
    })
}
/// One mip's native address and row layout (GPU User's Guide pp43,45).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MipLayout {
    /// Offset from the start of mapped texture storage.
    pub offset: usize,
    /// Texel dimensions at this level.
    pub size: [u32; 2],
    /// Allocated span, including the inter-level gap for NPOT linear textures.
    pub layout: TextureLayout,
}
/// Validate a chain and calculate one level. Mip count includes the base level.
pub fn mip_layout(
    size: [u32; 2],
    format: TextureFormat,
    count: u32,
    level: u32,
) -> Result<MipLayout> {
    texture_layout(size, format)?;
    if count == 0 || count > 32 - size.into_iter().max().unwrap().leading_zeros() || level >= count
    {
        return Err(Error::Invalid("texture mip count"));
    }
    let mut offset = 0;
    for i in 0..=level {
        let extent = size.map(|v| (v >> i).max(1));
        let layout = if format.compressed() {
            let row = extent[0].div_ceil(4) as usize
                * if format == TextureFormat::Bc1Rgb {
                    8
                } else {
                    16
                };
            TextureLayout {
                row_stride_bytes: row,
                bytes: row * extent[1].div_ceil(4) as usize,
            }
        } else {
            let row = extent[0].next_multiple_of(8) as usize * format.pixel_bytes();
            let bytes = if count == 1 {
                row * extent[1] as usize
            } else {
                ((size[0].next_power_of_two() >> i).max(8) as usize)
                    * ((size[1].next_power_of_two() >> i).max(1) as usize)
                    * format.pixel_bytes()
            };
            TextureLayout {
                row_stride_bytes: row,
                bytes,
            }
        };
        if i == level {
            return Ok(MipLayout {
                offset,
                size: extent,
                layout,
            });
        }
        offset += layout.bytes;
    }
    unreachable!()
}
/// Exact complete mip storage, including native inter-level padding.
pub fn mip_texture_layout(
    size: [u32; 2],
    format: TextureFormat,
    count: u32,
) -> Result<TextureLayout> {
    let last = mip_layout(size, format, count, count.saturating_sub(1))?;
    Ok(TextureLayout {
        bytes: last.offset + last.layout.bytes,
        row_stride_bytes: texture_layout(size, format)?.row_stride_bytes,
    })
}
#[cfg(any(target_os = "vita", test))]
/// Reorder portable row-major BC blocks directly into final mapped storage.
/// GPU User's Guide pp44-45: v occupies the low interleaved bit.
pub(crate) fn morton_bc(
    size: [u32; 2],
    block_bytes: usize,
    source: &[u8],
    dest: &mut [u8],
) -> Result<()> {
    let width = size[0] / 4;
    let height = size[1] / 4;
    if width == 0
        || height == 0
        || !width.is_power_of_two()
        || !height.is_power_of_two()
        || !matches!(block_bytes, 8 | 16)
        || source.len() != width as usize * height as usize * block_bytes
        || dest.len() < source.len()
    {
        return Err(Error::Invalid("BC block storage"));
    }
    for y in 0..height {
        for x in 0..width {
            let (mut address, mut bit) = (0usize, 0);
            for axis in 0..width.ilog2().max(height.ilog2()) {
                if axis < height.ilog2() {
                    address |= (((y >> axis) & 1) as usize) << bit;
                    bit += 1;
                }
                if axis < width.ilog2() {
                    address |= (((x >> axis) & 1) as usize) << bit;
                    bit += 1;
                }
            }
            let start = (y as usize * width as usize + x as usize) * block_bytes;
            dest[address * block_bytes..(address + 1) * block_bytes]
                .copy_from_slice(&source[start..start + block_bytes]);
        }
    }
    Ok(())
}
#[cfg(any(target_os = "vita", test))]
pub(crate) fn upload_span(rows: u32, stride: usize, packed: usize) -> Option<usize> {
    (rows.checked_sub(1)? as usize)
        .checked_mul(stride)?
        .checked_add(packed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn upload_span_rejects_overflow_before_touching_memory() {
        assert_eq!(super::upload_span(3, 16, 12), Some(44));
        assert_eq!(super::upload_span(1, usize::MAX, 12), Some(12));
        assert_eq!(super::upload_span(2, usize::MAX, 12), None);
        assert_eq!(super::upload_span(0, 16, 12), None);
    }

    use super::*;
    #[test]
    fn mip_offsets_match_official_npot_table_and_bc_tails() {
        let offsets = [32768, 8192, 2048, 512, 128, 32, 16, 8];
        let strides = [144, 72, 40, 24, 16, 8, 8, 8];
        let mut sum = 0;
        for i in 0..8 {
            let m = mip_layout([144, 78], TextureFormat::Rgba8, 8, i).unwrap();
            assert_eq!(
                (m.offset, m.layout.bytes, m.layout.row_stride_bytes),
                (sum, offsets[i as usize] * 4, strides[i as usize] * 4)
            );
            sum += m.layout.bytes;
        }
        for size in [[16u32, 8], [8, 16], [4, 4], [32, 4]] {
            let count = 32 - size.into_iter().max().unwrap().leading_zeros();
            for format in [TextureFormat::Bc1Rgb, TextureFormat::Bc3Rgba] {
                let n = if format == TextureFormat::Bc1Rgb {
                    8
                } else {
                    16
                };
                let mut offset = 0;
                for i in 0..count {
                    let m = mip_layout(size, format, count, i).unwrap();
                    assert_eq!(m.offset, offset);
                    let blocks = (m.size[0].div_ceil(4) * m.size[1].div_ceil(4)) as usize;
                    let source: std::vec::Vec<u8> = (0..blocks)
                        .flat_map(|b| std::iter::repeat_n(b as u8, n))
                        .collect();
                    let mut dest = std::vec![255;source.len()];
                    morton_bc(m.size.map(|v| v.max(4)), n, &source, &mut dest).unwrap();
                    let mut ids: std::vec::Vec<_> = dest.chunks_exact(n).map(|b| b[0]).collect();
                    ids.sort();
                    assert_eq!(ids, (0..blocks as u8).collect::<std::vec::Vec<_>>());
                    offset += m.layout.bytes;
                }
                assert_eq!(
                    offset,
                    mip_texture_layout(size, format, count).unwrap().bytes
                );
            }
        }
    }
    #[test]
    fn strided_target_cost_agrees_with_last_pixel_and_row_start() {
        for size in [[1, 1], [3, 7], [960, 544], [4095, 4096]] {
            let l = target_layout(size).unwrap();
            assert_eq!(l.row_stride_bytes, (size[0] as u64).next_multiple_of(2) * 4);
            assert_eq!(l.texture_bytes, l.row_stride_bytes * size[1] as u64);
        }
        let y = texture_layout([3, 3], TextureFormat::Luma8).unwrap();
        let uv = texture_layout([2, 2], TextureFormat::LumaAlpha8).unwrap();
        assert_eq!(
            (y.row_stride_bytes, uv.row_stride_bytes, y.bytes + uv.bytes),
            (8, 16, 56)
        );
        assert_eq!(
            texture_layout([16, 8], TextureFormat::Bc1Rgb)
                .unwrap()
                .bytes,
            64
        );
        assert_eq!(
            texture_layout([8, 16], TextureFormat::Bc3Rgba)
                .unwrap()
                .bytes,
            128
        );
    }
    #[test]
    fn rectangular_bc_morton_is_bijective_and_v_is_low_bit() {
        for size in [[16, 8], [8, 16], [4, 4], [32, 8]] {
            for n in [8, 16] {
                let blocks = (size[0] * size[1] / 16) as usize;
                let source: std::vec::Vec<u8> = (0..blocks)
                    .flat_map(|i| std::iter::repeat_n(i as u8, n))
                    .collect();
                let mut out = std::vec![255; source.len()];
                morton_bc(size, n, &source, &mut out).unwrap();
                let mut seen = std::vec![false; blocks];
                for block in out.chunks_exact(n) {
                    assert!(block.iter().all(|b| *b == block[0]));
                    assert!(!seen[block[0] as usize]);
                    seen[block[0] as usize] = true;
                }
                assert!(seen.into_iter().all(|v| v));
                if size[1] >= 8 {
                    assert_eq!(out[n], (size[0] / 4) as u8);
                }
            }
        }
    }
}
