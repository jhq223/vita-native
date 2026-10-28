//! Device contracts independent of the target SDK.
use std::{prelude::v1::*, sync::Arc};
/// First native failure that leaves completion or ownership unresolved.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceFault {
    /// Native operation whose first failure remains terminal.
    pub operation: &'static str,
    /// Original signed SDK return code.
    pub code: i32,
}
/// Native errors retain the actual operation and SDK status.
#[derive(Debug)]
pub enum Error {
    /// Caller input violates the documented interface.
    Invalid(&'static str),
    /// The selected native format or behavior is unavailable.
    Unsupported(&'static str),
    /// A native domain would exceed its explicit capacity.
    Budget {
        /// Native allocation domain that rejected admission.
        domain: &'static str,
        /// Required simultaneous native capacity.
        requested: u64,
        /// Configured maximum capacity for this domain.
        limit: u64,
    },
    /// Native construction failed before submission.
    Sdk(&'static str, i32),
    /// A submitted/native ownership operation failed; owners remain retained.
    Terminal(Arc<DeviceFault>),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
pub(crate) type Result<T> = std::result::Result<T, Error>;
/// Fragment notifications are the GXM submission-completion source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionMode {
    /// Fragment or ordered transfer notification in the libgxm notification region.
    FragmentNotification,
}
/// GXM hardware limits; shader uniform storage is validated from each GXP.
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// Maximum 2D physical texture dimension: 4096 pixels.
    pub max_texture_size: u32,
    /// Fragment sampler slots: 16.
    pub max_texture_units: u32,
    /// Vertex attributes: 16.
    pub max_vertex_attributes: u32,
    /// Direct UBC1/UBC3 sampling and portable BC block upload.
    pub bc: bool,
    /// BGRA8 sampled storage is available.
    pub bgra: bool,
    /// RGBA16F sampled storage is available.
    pub half_float: bool,
    /// Both u16 and u32 element streams are available.
    pub indices_u32: bool,
    /// Programs use offline GXP input.
    pub shader_binary: bool,
    /// Submission completion source.
    pub completion: CompletionMode,
}
/// Direct device and shader ABI identity.
#[derive(Clone, Debug)]
pub struct DriverInfo {
    /// Platform graphics library owner.
    pub vendor: String,
    /// Direct native device identity.
    pub renderer: String,
    /// This owned boundary interface version.
    pub version: String,
    /// Offline shader profiles consumed by this device.
    pub shading_language: String,
}
/// Fixed infrastructure budgets. Pixel/buffer arenas grow by suballocated blocks.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Maximum kernel capacity retained by each suballocation domain.
    pub pixel_arena_bytes: u64,
    /// Reserved uncached LPDDR cap for geometry and upload staging; default 16 MiB.
    pub buffer_arena_bytes: u64,
    /// Strongly retained pixel block capacity, included in pixel_arena_bytes.
    pub pixel_retained_bytes: u64,
    /// Strongly retained buffer block capacity, included in buffer_arena_bytes.
    pub buffer_retained_bytes: u64,
    /// Total firmware scheduling block capacity, including active and retained targets.
    pub render_target_bytes: u64,
    /// Strongly retained scheduling capacity within render_target_bytes.
    pub render_target_retained_bytes: u64,
    /// Maximum simultaneously owned transfer staging; admission waits real completion at this cap.
    pub upload_staging_bytes: u64,
    /// libgxm-owned CDRAM parameter-buffer size; default 16 MiB.
    pub parameter_bytes: u32,
    /// Mapped VDM command ring size; default 128 KiB.
    pub vdm_ring_bytes: u32,
    /// Mapped vertex uniform/command ring size; default 2 MiB.
    pub vertex_ring_bytes: u32,
    /// Mapped fragment uniform/command ring size; default 512 KiB.
    pub fragment_ring_bytes: u32,
    /// Mapped fragment USSE ring size; default 16 KiB.
    pub fragment_usse_ring_bytes: u32,
    /// Fixed mapped shader-patcher data pool; default 1 MiB.
    pub patcher_buffer_bytes: u32,
    /// Fixed vertex executable pool; default 256 KiB.
    pub patcher_vertex_usse_bytes: u32,
    /// Fixed fragment executable pool; default 512 KiB.
    pub patcher_fragment_usse_bytes: u32,
    /// Queued scenes per shared scheduling target: 1..=8, default 4.
    pub scenes_per_frame: u16,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            pixel_arena_bytes: 96 * 1024 * 1024,
            buffer_arena_bytes: 16 * 1024 * 1024,
            pixel_retained_bytes: 8 * 1024 * 1024,
            buffer_retained_bytes: 1024 * 1024,
            render_target_bytes: 16 * 1024 * 1024,
            render_target_retained_bytes: 4 * 1024 * 1024,
            upload_staging_bytes: 4 * 1024 * 1024,
            parameter_bytes: 16 * 1024 * 1024,
            vdm_ring_bytes: 128 * 1024,
            vertex_ring_bytes: 2 * 1024 * 1024,
            fragment_ring_bytes: 512 * 1024,
            fragment_usse_ring_bytes: 16 * 1024,
            patcher_buffer_bytes: 1024 * 1024,
            patcher_vertex_usse_bytes: 256 * 1024,
            patcher_fragment_usse_bytes: 512 * 1024,
            scenes_per_frame: 4,
        }
    }
}
/// One native arena's kernel capacity and live suballocated payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArenaUsage {
    /// Actual kernel block capacity, including free suballocation intervals.
    pub reserved_bytes: u64,
    /// Live suballocated payload bytes; excludes free alignment gaps.
    pub used_bytes: u64,
    /// Live kernel blocks in this arena.
    pub blocks: u32,
    /// Completely idle capacity retained for reuse, included in reserved_bytes.
    pub idle_bytes: u64,
    /// Successful kernel block allocations since device creation.
    pub block_allocations: u64,
}
/// CPU wall time inside a native SDK call, including any internal blocking.
/// Values are cumulative since context creation and zero without `profiling`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeCallTiming {
    /// Number of measured calls, including calls returning a native error.
    pub calls: u64,
    /// Total CPU wall time in microseconds; this is not GPU execution time.
    pub cpu_us: u64,
}
/// Fixed native submission attribution. Enabled by the `profiling` feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmissionTiming {
    /// BeginScene, including scheduling-target admission inside the SDK.
    pub scene_begin: NativeCallTiming,
    /// EndScene submission inside the SDK.
    pub scene_end: NativeCallTiming,
    /// Vertex and fragment default-uniform reservations inside the SDK.
    pub uniform_reserve: NativeCallTiming,
    /// Draw submission inside the SDK, including any ring-buffer pressure.
    pub draw: NativeCallTiming,
    /// Explicit notification wait when all completion slots are occupied.
    pub notification_capacity_wait: NativeCallTiming,
    /// Explicit notification wait to admit bounded upload staging.
    pub upload_capacity_wait: NativeCallTiming,
    /// Explicit notification wait for resource readers before CPU replacement/capture.
    pub resource_replacement_wait: NativeCallTiming,
}
/// Independent native memory domains. Renderer tickets describe their own payloads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryUsage {
    /// Native submission CPU attribution; zero when `profiling` is disabled.
    pub submission_timing: SubmissionTiming,
    /// Caller texture/target CDRAM arena; excludes scanout and parameter memory.
    pub pixels: ArenaUsage,
    /// Geometry and transfer staging arena; includes upload_staging_bytes.
    pub buffers: ArenaUsage,
    /// libgxm parameter-buffer reservation requested at initialization.
    pub parameter_cdram_bytes: u64,
    /// Actual mapped capacity of the four context rings.
    pub context_ring_bytes: u64,
    /// Actual mapped capacity of the three fixed patcher pools.
    pub patcher_pool_bytes: u64,
    /// Current patcher CPU storage returned by its native query.
    pub patcher_host_bytes: u64,
    /// Actual uncached kernel block capacity backing all live render targets.
    pub render_target_driver_bytes: u64,
    /// Read-only media capacity mapped for sampling. Its physical budget belongs
    /// to the producer's frame pool; this is an alias count, not extra storage.
    pub imported_video_bytes: u64,
    /// Live shared firmware targets; scheduling size classes bound this count to 36.
    pub render_targets: u32,
    /// Live display synchronization objects, one per context-owned scanout buffer.
    /// Offscreen targets use ordered scenes and fragment completion notifications.
    pub display_sync_objects: u32,
    /// Scanout payload bytes; a subset of display_reserved_bytes.
    pub display_pixel_bytes: u64,
    /// Actual scanout CDRAM kernel block capacity, separate from pixels.
    pub display_reserved_bytes: u64,
    /// Live transfer-staging cap within the buffer arena; default 4 MiB.
    pub upload_staging_bytes: u64,
    /// Successfully ended direct scenes.
    pub scene_submissions: u64,
    /// Admission waits caused by exhausting the 256 completion slots.
    pub notification_capacity_waits: u64,
    /// Submissions observed at display capacity before native queue admission.
    pub display_queue_waits: u64,
    /// Successfully completed display callbacks.
    pub display_flips: u64,
}
/// Sample/storage formats. Luma formats retain the GLES `.r` / `.ra` contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    /// Byte-order RGBA8, sampled as linear premultiplied color.
    Rgba8,
    /// Byte-order BGRA8.
    Bgra8,
    /// Four IEEE half-float components.
    Rgba16Float,
    /// One byte replicated to RGB with alpha one.
    Luma8,
    /// Two bytes sampled as (R,R,R,G), preserving .ra for NV12 chroma.
    LumaAlpha8,
    /// Portable row-major BC1 blocks, sampled with opaque alpha.
    Bc1Rgb,
    /// Portable row-major BC3 blocks with native alpha.
    Bc3Rgba,
}
impl TextureFormat {
    pub(crate) fn pixel_bytes(self) -> usize {
        match self {
            Self::Luma8 => 1,
            Self::LumaAlpha8 => 2,
            Self::Rgba16Float => 8,
            _ => 4,
        }
    }
    pub(crate) fn compressed(self) -> bool {
        matches!(self, Self::Bc1Rgb | Self::Bc3Rgba)
    }
}
/// Hardware minification/magnification selection for a draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFilter {
    /// Nearest texel selection.
    Nearest,
    /// Linear interpolation of adjacent texels.
    Linear,
}
/// Pipeline output variants prepared when a program is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// Replace all RGBA channels.
    Replace,
    /// Premultiplied source-over: source plus destination times one minus source alpha.
    Premultiplied,
}
/// Offline GXM program, with retained reflection symbols.
#[derive(Clone, Copy)]
pub enum ShaderCode<'a> {
    /// Compiler-padded GXP payload with retained reflection symbols.
    GxpBinary(&'a [u8]),
}
/// Map a caller vertex attribute identity to its retained GXP parameter name.
pub struct AttributeBinding<'a> {
    /// Caller vertex attribute identity.
    pub location: u32,
    /// Retained GXP attribute parameter name.
    pub name: &'a str,
}
/// Interleaved float input. Offsets and stride use f32 elements, matching graphics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexAttribute {
    /// Caller vertex attribute identity.
    pub location: u32,
    /// Number of f32 input components, from one through four.
    pub components: u32,
    /// Attribute offset in f32 elements.
    pub offset: u32,
}

/// Offline stages and the caller's interleaved f32 vertex interface.
#[derive(Clone, Copy)]
pub struct ProgramDescriptor<'a> {
    /// Compiled vertex stage.
    pub vertex: ShaderCode<'a>,
    /// Compiled fragment stage.
    pub fragment: ShaderCode<'a>,
    /// Reflected names assigned to vertex attribute locations.
    pub bindings: &'a [AttributeBinding<'a>],
    /// Distance between vertices, in f32 elements.
    pub stride: u32,
    /// Component count and element offset for each location.
    pub attributes: &'a [VertexAttribute],
    /// Optional float4 fragment uniform receiving half-open pixel clip bounds.
    /// The fragment program must discard pixels outside this rectangle.
    /// Without it, internal scissor edges must be aligned to 32-pixel tiles.
    pub clip_uniform: Option<&'a str>,
}

/// Application-supplied fullscreen clear programs and reflected binding names.
#[derive(Clone, Copy)]
pub struct ClearProgram<'a> {
    /// Vertex stage accepting one float2 position attribute.
    pub vertex: ShaderCode<'a>,
    /// Fragment stage writing the supplied float4 color.
    pub fragment: ShaderCode<'a>,
    /// Name of the vertex float2 position attribute at location zero.
    pub position_attribute: &'a str,
    /// Name of the fragment float4 clear-color uniform.
    pub color_uniform: &'a str,
    /// Name of the fragment float4 half-open pixel clip uniform.
    pub clip_uniform: &'a str,
}

/// Whether a rectangle needs fragment clipping in addition to GXM tile clipping.
/// Color-surface bounds provide exact clipping at the physical allocation edges.
#[cfg(any(target_os = "vita", test))]
pub(super) fn needs_fragment_clip(min: [u32; 2], max: [u32; 2], size: [u32; 2]) -> bool {
    (0..2).any(|axis| {
        !min[axis].is_multiple_of(32) || (max[axis] != size[axis] && !max[axis].is_multiple_of(32))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_edges_and_tiles_cover_scissors_without_a_fragment_uniform() {
        assert!(!needs_fragment_clip([0, 0], [513, 273], [513, 273]));
        assert!(!needs_fragment_clip([32, 64], [512, 256], [513, 273]));
        assert!(needs_fragment_clip([1, 0], [513, 273], [513, 273]));
        assert!(needs_fragment_clip([0, 0], [511, 273], [513, 273]));
        assert!(needs_fragment_clip([0, 0], [513, 272], [513, 273]));
    }
}
