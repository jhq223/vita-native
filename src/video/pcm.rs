//! Owned packet cursors and direct conversion from a currently borrowed SDK packet.
use std::prelude::v1::*;

pub(super) const CHUNK_FRAMES: usize = 1024;

pub(super) struct Packet {
    pub bytes: usize,
    pub offset: usize,
    pub rate: u32,
    pub channels: usize,
    pub first_sample: u64,
}
impl Packet {
    pub fn frames(&self) -> usize {
        (self.bytes - self.offset) / (self.channels * 2)
    }
    pub fn write(
        &mut self,
        source: &[u8],
        origin_ms: u64,
        output: &mut Vec<[f32; 2]>,
    ) -> Result<f64, String> {
        let stride = self.channels * 2;
        let frames = self.frames().min(CHUNK_FRAMES);
        output.clear();
        output
            .try_reserve_exact(frames)
            .map_err(|e| e.to_string())?;
        let end = self.offset + frames * stride;
        for sample in source[self.offset..end].chunks_exact(stride) {
            let left = f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.;
            let right = if self.channels == 1 {
                left
            } else {
                f32::from(i16::from_le_bytes([sample[2], sample[3]])) / 32768.
            };
            output.push([left, right]);
        }
        let absolute = self
            .first_sample
            .saturating_add((self.offset / stride) as u64);
        let origin = origin_ms.saturating_mul(u64::from(self.rate)) / 1000;
        self.offset = end;
        Ok((i128::from(absolute) - i128::from(origin)) as f64 / f64::from(self.rate))
    }
}

pub(super) fn first_sample(next: Option<u64>, reported_ms: u64, rate: u32) -> u64 {
    let rate = u64::from(rate);
    let reported = reported_ms.saturating_mul(rate) / 1000;
    next.filter(|&next| next.abs_diff(reported) <= rate.div_ceil(1000))
        .unwrap_or(reported)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pcm_extremes_and_mono_duplication() {
        let samples = [i16::MIN, -1, 0, i16::MAX];
        let source: Vec<_> = samples.into_iter().flat_map(i16::to_le_bytes).collect();
        let mut output = Vec::new();
        let mut mono = Packet {
            bytes: source.len(),
            offset: 0,
            rate: 48000,
            channels: 1,
            first_sample: 0,
        };
        assert_eq!(mono.write(&source, 10, &mut output).unwrap(), -0.01);
        assert_eq!(
            output,
            [[-1.; 2], [-1. / 32768.; 2], [0.; 2], [32767. / 32768.; 2]]
        );
        let mut stereo = Packet {
            channels: 2,
            ..mono
        };
        stereo.offset = 0;
        stereo.write(&source, 0, &mut output).unwrap();
        assert_eq!(output, [[-1., -1. / 32768.], [0., 32767. / 32768.]]);
    }
    #[test]
    fn chunks_preserve_frames_and_timestamps_across_packet_sizes() {
        for frames in [1023, 1024, 1025, 2049] {
            for channels in [1, 2] {
                let source: Vec<_> = (0..frames * channels)
                    .flat_map(|i| (i as i16).to_le_bytes())
                    .collect();
                let mut packet = Packet {
                    bytes: source.len(),
                    offset: 0,
                    rate: 48000,
                    channels,
                    first_sample: 480,
                };
                let mut output = Vec::with_capacity(CHUNK_FRAMES);
                let capacity = output.capacity();
                let mut received = 0;
                while packet.frames() > 0 {
                    let pts = packet.write(&source, 10, &mut output).unwrap();
                    assert_eq!(pts, received as f64 / 48000.);
                    assert_eq!(output.len(), (frames - received).min(CHUNK_FRAMES));
                    for (i, frame) in output.iter().enumerate() {
                        let sample = (received + i) * channels;
                        assert_eq!(frame[0], sample as f32 / 32768.);
                        assert_eq!(frame[1], (sample + channels - 1) as f32 / 32768.);
                    }
                    received += output.len();
                    assert_eq!(output.capacity(), capacity);
                }
                assert_eq!(received, frames);
            }
        }
    }
    #[test]
    fn packet_clock_absorbs_millisecond_rounding_but_respects_discontinuities() {
        assert_eq!(first_sample(Some(1024), 21, 48000), 1024);
        assert_eq!(first_sample(Some(1056), 21, 48000), 1056);
        assert_eq!(first_sample(Some(1057), 21, 48000), 1008);
        assert_eq!(first_sample(None, 21, 48000), 1008);
        assert_eq!(first_sample(Some(5000), 0, 48000), 0);
    }
}
