/// The visible movie timeline and decoded track format.
#[derive(Clone, Debug)]
pub struct Info {
    /// Visible width, excluding native surface padding.
    pub width: u32,
    /// Visible height, excluding native surface padding.
    pub height: u32,
    /// Source duration in seconds.
    pub duration: f64,
    /// Average picture duration derived from the MP4 video timing table.
    pub frame_duration: f64,
    /// Presentation timestamp of the final picture in the MP4 movie timeline,
    /// before subtracting the native stream's start-time origin.
    pub final_picture_time: f64,
    /// Decoded audio rate, or None when this movie contains no audio track.
    pub audio_sample_rate: Option<u32>,
    /// Mono or stereo source channels; zero when no audio track exists.
    pub audio_channels: u16,
}
