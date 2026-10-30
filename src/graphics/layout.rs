//! One layout calculation drives mapping, texture views and resource accounting.
use super::{Error, TargetFormat, TextureFormat, types::Result};
/// Requested offscreen color layout. Small surfaces and Coverage8 remain linear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetStorage {
    /// Rows in scanline order, with the format's native row padding.
    #[default]
    Linear,
    /// Scanline-ordered 32x32 tiles, with scanline texels within each tile.
    Tiled,
}
impl TargetStorage {
    /// Exact offscreen footprint for this requested storage. Coverage8 and
    /// surfaces smaller than one tile use linear addressing.
    pub fn layout(self, size: [u32; 2], format: TargetFormat) -> Result<TargetLayout> {
        target_layout_for(size, format, self)
    }
}
#[cfg(any(target_os = "vita", test))]
impl TargetStorage {
    pub(crate) fn byte_offset(self, x: usize, y: usize, stride: usize, bpp: usize) -> usize {
        match self {
            Self::Linear => y * stride + x * bpp,
            Self::Tiled => {
                (y / 32) * stride * 32 + (x / 32) * 1024 * bpp + ((y % 32) * 32 + x % 32) * bpp
            }
        }
    }
    pub(crate) fn row_run(self, x: usize, remaining: usize) -> usize {
        match self {
            Self::Linear => remaining,
            Self::Tiled => remaining.min(32 - x % 32),
        }
    }
}
/// Exact single-sample target storage including row or complete-tile padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetLayout {
    /// Color payload bytes, including complete row or tile padding.
    pub texture_bytes: u64,
    /// Bytes per logical scanline including padding. For tiled storage this is
    /// the tile-row width divided by 32, not a consecutive CPU row address.
    pub row_stride_bytes: u64,
    /// Actual addressing used by the color surface and sampled view.
    pub storage: TargetStorage,
}
/// Target color storage and independently owned firmware scheduling storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetFootprint {
    /// Format-specific color layout consumed by target planning.
    pub texture: TargetLayout,
    /// Uncached kernel capacity for one shared scheduling class at this extent.
    pub render_target_driver_bytes: Option<u64>,
}
/// Exact mapped texture payload. BC dimensions are physical power-of-two extents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureLayout {
    /// Exact mapped payload bytes; excludes suballocator alignment gaps.
    pub bytes: usize,
    /// Scanline stride including padding. Tiled render-target views split rows
    /// across tiles; `Texture::target_storage` identifies their addressing.
    pub row_stride_bytes: usize,
}
pub(crate) fn validate_size(size: [u32; 2]) -> Result<()> {
    if size.contains(&0) || size.iter().any(|&v| v > 4096) {
        return Err(Error::Invalid("texture size (1..4096)"));
    }
    Ok(())
}
/// Default linear render-target layout. Contexts report their configured actual
/// layout through `Context::target_layout`; surface and sampled-view row alignment
/// are included in both calculations.
pub fn target_layout(size: [u32; 2], format: TargetFormat) -> Result<TargetLayout> {
    target_layout_for(size, format, TargetStorage::Linear)
}
pub(crate) fn target_layout_for(
    size: [u32; 2],
    format: TargetFormat,
    storage: TargetStorage,
) -> Result<TargetLayout> {
    validate_size(size)?;
    if storage == TargetStorage::Tiled
        && format == TargetFormat::Rgba8
        && size.iter().all(|n| *n >= 32)
    {
        let row = u64::from(size[0]).next_multiple_of(32) * 4;
        return Ok(TargetLayout {
            texture_bytes: row * u64::from(size[1]).next_multiple_of(32),
            row_stride_bytes: row,
            storage,
        });
    }
    let row = match format {
        TargetFormat::Rgba8 => (size[0] as u64).next_multiple_of(2) * 4,
        TargetFormat::Coverage8 => (size[0] as u64).next_multiple_of(4),
    };
    Ok(TargetLayout {
        texture_bytes: row * u64::from(size[1]),
        row_stride_bytes: row,
        storage: TargetStorage::Linear,
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
    fn tiled_admission_bounds_each_per_allocation_storage_choice() {
        for w in (1..=4096).step_by(37) {
            for h in (1..=4096).step_by(43) {
                for format in [TargetFormat::Rgba8, TargetFormat::Coverage8] {
                    let linear = TargetStorage::Linear.layout([w, h], format).unwrap();
                    let tiled = TargetStorage::Tiled.layout([w, h], format).unwrap();
                    assert!(tiled.texture_bytes >= linear.texture_bytes);
                    assert_eq!(linear, target_layout([w, h], format).unwrap());
                }
            }
        }
    }
    #[test]
    fn tiled_target_footprints_and_offsets_match_gpu_tile_coordinates() {
        let l = target_layout_for([48, 40], TargetFormat::Rgba8, TargetStorage::Tiled).unwrap();
        assert_eq!((l.texture_bytes, l.row_stride_bytes), (64 * 64 * 4, 64 * 4));
        assert_eq!(l.storage, TargetStorage::Tiled);
        for (point, pixel) in [
            ([0, 0], 0),
            ([31, 31], 1023),
            ([32, 0], 1024),
            ([0, 32], 2048),
            ([32, 32], 3072),
        ] {
            assert_eq!(
                l.storage
                    .byte_offset(point[0], point[1], l.row_stride_bytes as usize, 4),
                pixel * 4
            );
        }
        for size in [[31, 64], [64, 31], [1, 1]] {
            assert_eq!(
                target_layout_for(size, TargetFormat::Rgba8, TargetStorage::Tiled).unwrap(),
                target_layout(size, TargetFormat::Rgba8).unwrap()
            );
        }
        assert_eq!(
            target_layout_for([64, 64], TargetFormat::Coverage8, TargetStorage::Tiled).unwrap(),
            target_layout([64, 64], TargetFormat::Coverage8).unwrap()
        );
        assert!(target_layout_for([0, 64], TargetFormat::Rgba8, TargetStorage::Tiled).is_err());
        assert!(target_layout_for([4097, 64], TargetFormat::Rgba8, TargetStorage::Tiled).is_err());
    }

    #[test]
    fn tiled_rows_cross_tiles_without_aliasing_or_exposing_padding() {
        for size in [[32, 32], [33, 37], [137, 93], [513, 33]] {
            let l = target_layout_for(size, TargetFormat::Rgba8, TargetStorage::Tiled).unwrap();
            let mut pixels = vec![0xa5; l.texture_bytes as usize];
            let mut touched = vec![false; pixels.len() / 4];
            for y in 0..size[1] as usize {
                let mut x = 0;
                while x < size[0] as usize {
                    let run = l.storage.row_run(x, size[0] as usize - x);
                    assert!((1..=32).contains(&run));
                    let at = l.storage.byte_offset(x, y, l.row_stride_bytes as usize, 4);
                    for i in 0..run {
                        assert!(!std::mem::replace(&mut touched[at / 4 + i], true));
                        pixels[at + i * 4..at + (i + 1) * 4].copy_from_slice(
                            &((y * size[0] as usize + x + i) as u32).to_le_bytes(),
                        );
                    }
                    x += run;
                }
            }
            for y in 0..size[1] as usize {
                for x in 0..size[0] as usize {
                    let at = l.storage.byte_offset(x, y, l.row_stride_bytes as usize, 4);
                    assert_eq!(
                        u32::from_le_bytes(pixels[at..at + 4].try_into().unwrap()),
                        (y * size[0] as usize + x) as u32
                    );
                }
            }
            assert_eq!(
                touched.iter().filter(|&&v| v).count(),
                size[0] as usize * size[1] as usize
            );
            assert!(
                pixels
                    .chunks_exact(4)
                    .zip(touched)
                    .all(|(p, touched)| touched || p == [0xa5; 4])
            );
        }
    }
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
            let l = target_layout(size, TargetFormat::Rgba8).unwrap();
            assert_eq!(l.row_stride_bytes, (size[0] as u64).next_multiple_of(2) * 4);
            assert_eq!(l.texture_bytes, l.row_stride_bytes * size[1] as u64);
            let coverage = target_layout(size, TargetFormat::Coverage8).unwrap();
            assert_eq!(
                coverage.row_stride_bytes,
                (size[0] as u64).next_multiple_of(4)
            );
            assert_eq!(
                coverage.texture_bytes,
                coverage.row_stride_bytes * size[1] as u64
            );
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
