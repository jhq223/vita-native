//! Immutable linear target views sharing one unchanged color allocation.
use super::{Error, TargetFormat, TargetStorage, TextureFormat, TextureLayout, types::Result};

fn reshape_layout(
    size: [u32; 2],
    format: TextureFormat,
    storage: Option<TargetStorage>,
    allocated_bytes: usize,
) -> Result<TextureLayout> {
    if storage != Some(TargetStorage::Linear) {
        return Err(Error::Unsupported(
            "reshape requires a linear render target",
        ));
    }
    let format = match format {
        TextureFormat::Rgba8 => TargetFormat::Rgba8,
        TextureFormat::Coverage8 => TargetFormat::Coverage8,
        _ => return Err(Error::Unsupported("reshape target color format")),
    };
    let layout = TargetStorage::Linear.layout(size, format)?;
    if layout.texture_bytes != allocated_bytes as u64 {
        return Err(Error::Invalid("reshape target allocation byte count"));
    }
    Ok(TextureLayout {
        bytes: allocated_bytes,
        row_stride_bytes: layout.row_stride_bytes as usize,
    })
}

#[cfg(target_os = "vita")]
impl super::Context {
    /// Create an immutable exact-size linear target view over the source's
    /// complete color allocation. RGBA8 and Coverage8 retain their format; the
    /// requested row layout must consume exactly the existing allocation bytes.
    /// Macrotile cells, sampled images and tiled surfaces retain their own APIs.
    ///
    /// The views share allocation identity, capacity and completion ownership.
    /// This allocates texture metadata and issues no GPU work or color-memory
    /// allocation. The caller orders writes after previous readers on the same
    /// graphics queue. CPU access waits for submitted readers/writers while
    /// immutable descriptors can remain owned after those submissions retire.
    pub fn reshape_target_texture(
        &self,
        source: &super::Texture,
        size: [u32; 2],
    ) -> Result<super::Texture> {
        use super::{memory::sdk, resources::TextureInner, resources::texture_format};
        use std::{cell::Cell, rc::Rc};
        use vitasdk_sys::*;

        self.check("target texture reshape")?;
        self.same(&source.0.owner)?;
        if source.0.macrocell.is_some() {
            return Err(Error::Unsupported("reshape macrotile cell"));
        }
        let layout = reshape_layout(
            size,
            source.format(),
            source.target_storage(),
            source.0.allocation.len(),
        )?;
        let mut raw = unsafe { core::mem::zeroed() };
        // SAFETY: the validated row layout consumes the complete retained span.
        sdk("sceGxmTextureInitLinearStrided(reshape)", unsafe {
            sceGxmTextureInitLinearStrided(
                &mut raw,
                source.0.allocation.ptr().cast(),
                texture_format(source.format()),
                size[0],
                size[1],
                layout.row_stride_bytes as u32,
            )
        })?;
        sdk("reshape texture U clamp", unsafe {
            sceGxmTextureSetUAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
        })?;
        sdk("reshape texture V clamp", unsafe {
            sceGxmTextureSetVAddrMode(&mut raw, SCE_GXM_TEXTURE_ADDR_CLAMP)
        })?;
        Ok(super::Texture(Rc::new(TextureInner {
            allocation: source.0.allocation.clone(),
            raw: Cell::new(raw),
            size,
            format: source.format(),
            layout,
            strided: true,
            storage: TargetStorage::Linear,
            mip_count: 1,
            macrocell: None,
            owner: source.0.owner.clone(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_payload_views_keep_exact_dimensions_and_recompute_native_row_stride() {
        for format in [TextureFormat::Rgba8, TextureFormat::Coverage8] {
            let target = if format == TextureFormat::Rgba8 {
                TargetFormat::Rgba8
            } else {
                TargetFormat::Coverage8
            };
            let first = TargetStorage::Linear.layout([2176, 2304], target).unwrap();
            let next = reshape_layout(
                [2304, 2176],
                format,
                Some(TargetStorage::Linear),
                first.texture_bytes as usize,
            )
            .unwrap();
            assert_eq!(next.bytes as u64, first.texture_bytes);
            assert_eq!(
                next.row_stride_bytes as u64,
                TargetStorage::Linear
                    .layout([2304, 2176], target)
                    .unwrap()
                    .row_stride_bytes
            );
            assert_ne!(next.row_stride_bytes as u64, first.row_stride_bytes);
        }
        let original = TargetStorage::Linear
            .layout([3, 8], TargetFormat::Rgba8)
            .unwrap();
        let next = reshape_layout(
            [8, 4],
            TextureFormat::Rgba8,
            Some(TargetStorage::Linear),
            original.texture_bytes as usize,
        )
        .unwrap();
        assert_eq!(next.bytes, 128);
        assert_eq!(next.row_stride_bytes, 32);
    }

    #[test]
    fn reshape_requires_supported_linear_storage_and_identical_complete_payload() {
        for (format, storage, bytes) in [
            (TextureFormat::Rgba8, None, 128),
            (TextureFormat::Rgba8, Some(TargetStorage::Tiled), 128),
            (TextureFormat::Bgra8, Some(TargetStorage::Linear), 128),
            (TextureFormat::Rgba8, Some(TargetStorage::Linear), 127),
            (TextureFormat::Rgba8, Some(TargetStorage::Linear), 129),
        ] {
            assert!(reshape_layout([8, 4], format, storage, bytes).is_err());
        }
        assert!(
            reshape_layout(
                [0, 4],
                TextureFormat::Rgba8,
                Some(TargetStorage::Linear),
                128
            )
            .is_err()
        );
    }
}
