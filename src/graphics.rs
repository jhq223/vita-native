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
mod gxp;
mod layout;
#[cfg(any(target_os = "vita", test))]
mod render_targets;
#[cfg(any(target_os = "vita", test))]
mod suballoc;
mod types;
pub use layout::{TargetFootprint, TargetLayout, TextureLayout, target_layout, texture_layout};
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
#[cfg(target_os = "vita")]
pub use commands::Draw;
#[cfg(target_os = "vita")]
pub use device::{Context, Fence};
#[cfg(target_os = "vita")]
pub use program::{Program, Uniform, UniformValue};
#[cfg(target_os = "vita")]
pub use resources::{Framebuffer, IndexBuffer, Target, Texture, VertexBuffer};
