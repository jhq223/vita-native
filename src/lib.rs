//! Owned PlayStation Vita devices over VitaSDK import stubs.
//!
//! Native handles remain inside this crate. Graphics resources are thread-bound;
//! submitted work retains its allocations until native completion. Audio and
//! video expose owned buffers and explicit start, pause and shutdown operations.
#![deny(unsafe_op_in_unsafe_fn)]

/// Optional process-wide C/newlib/Rust heap and physical MAIN commitment.
#[cfg(feature = "process-allocator")]
pub mod allocator;
/// Application identity, input, power events and native host services.
#[cfg(all(feature = "app", target_os = "vita"))]
pub mod app;
/// Blocking AudioOut playback with preallocated PCM buffers.
#[cfg(all(feature = "audio", target_os = "vita"))]
pub mod audio;
/// GXM graphics resources, commands, completion and presentation.
#[cfg(feature = "graphics")]
pub mod graphics;
#[cfg(all(feature = "graphics", target_os = "vita"))]
mod graphics_active;
/// Native allocation budget and owned reservation contracts.
pub mod memory;
#[cfg(all(
    feature = "profiling",
    target_os = "vita",
    any(feature = "app", feature = "graphics")
))]
mod thread_runtime;
/// AvPlayer hardware decoding from an owned seekable stream.
#[cfg(all(feature = "video", target_os = "vita"))]
pub mod video;
