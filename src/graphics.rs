//! Direct GXM device boundary, interface version 1.
//!
//! Coordinates and sampled targets use a top-left origin. Applications describe
//! vertex layouts, reflected bindings and optional exact fragment clipping through
//! `ProgramDescriptor`; offline GXP code remains owned with its patched programs.
//!
//! Scenes retain referenced allocations until fragment completion. Display scanout
//! holds separate leases. `finish`, `reset` and `close` drain submitted work.
#![deny(missing_docs)]
#[cfg(any(target_os = "vita", test))]
mod bindings;
#[cfg(any(target_os = "vita", test))]
mod completion;
#[cfg(any(target_os = "vita", test))]
mod display_sync;
#[cfg(any(target_os = "vita", test))]
mod fault;
#[cfg(any(target_os = "vita", test))]
mod gxp;
mod layout;
#[cfg(any(target_os = "vita", test))]
mod leases;
#[cfg(any(target_os = "vita", test))]
mod pressure;
#[cfg(any(target_os = "vita", test))]
mod render_targets;
#[cfg(any(target_os = "vita", test))]
mod retained;
#[cfg(any(target_os = "vita", test))]
mod suballoc;
#[cfg(any(target_os = "vita", test))]
mod timing;
mod types;
#[cfg(any(target_os = "vita", test))]
mod uniform_layout;
pub use layout::{
    MipLayout, TargetFootprint, TargetLayout, TextureLayout, mip_layout, mip_texture_layout,
    target_layout, texture_layout,
};
pub use types::*;
#[cfg(target_os = "vita")]
mod commands;
#[cfg(target_os = "vita")]
mod device;
#[cfg(target_os = "vita")]
mod memory;
#[cfg(target_os = "vita")]
mod program;
#[cfg(target_os = "vita")]
mod resources;
#[cfg(all(target_os = "vita", feature = "video"))]
mod video;
#[cfg(target_os = "vita")]
pub use commands::Draw;
#[cfg(target_os = "vita")]
pub use device::{Context, Fence};
#[cfg(target_os = "vita")]
pub use program::{Program, Uniform, UniformValue};
#[cfg(target_os = "vita")]
pub use resources::{Framebuffer, IndexBuffer, Target, Texture, VertexBuffer};
