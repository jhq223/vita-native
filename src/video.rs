//! Worker-owned AvPlayer decoding from a seekable resource stream.
//!
//! The device profile is non-fragmented MP4 with H.264 video, optional AAC-LC
//! mono/stereo audio and even visible dimensions up to 960 by 544. Input callbacks
//! read the owned stream directly, including archive-backed sources. Native
//! surfaces are copied before the SDK can recycle them; callers own every output.
use std::{
    io::{Read, Seek},
    prelude::v1::*,
};

#[cfg(feature = "profiling")]
mod diagnostics;
mod info;
mod input;
mod metadata;
mod native;
mod owned;
pub use owned::{Nv12Buffer, Nv12Pool, SharedNv12};
mod pcm;
mod timing;
pub use info::Info;
pub use native::{Player, VideoCopy};

/// An owned, seekable source, independent of its filesystem or archive location.
pub trait Source: Read + Seek + Send {}
impl<T: Read + Seek + Send> Source for T {}

/// A native poll result. Pending retains a live decoder and is never EOF.
#[derive(Debug, PartialEq)]
pub enum Poll<T> {
    /// One owned copy has been written to the caller's destination.
    Ready(T),
    /// Native decoding has not produced this output yet.
    Pending,
    /// The selected track has ended and no delayed output remains.
    End,
}

/// Presentation time of a copied NV12 picture, relative to the video origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
    /// Source timestamp in seconds.
    pub pts: f64,
}

/// Description of PCM copied into the caller's stereo destination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Audio {
    /// Source timestamp of the first copied frame, in seconds.
    pub pts: f64,
    /// Source frames per second.
    pub sample_rate: u32,
    /// Initialized stereo frames in the supplied destination.
    pub frames: usize,
}
