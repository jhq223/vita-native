//! Immediate context bindings. Only region clipping resets at scene begin.
use super::TextureFilter;

#[derive(Clone, Copy, PartialEq, Eq)]
struct SampledTexture {
    descriptor: [u32; 4],
    filter: TextureFilter,
}

#[derive(Default)]
pub(super) struct Bindings {
    viewport: Option<[u32; 2]>,
    clip: Option<[u32; 4]>,
    vertices: [Option<usize>; 2],
    textures: [Option<SampledTexture>; 16],
}

impl Bindings {
    pub fn begin_scene(&mut self) {
        self.clip = None;
    }

    pub fn viewport(&mut self, size: [u32; 2]) -> bool {
        replace(&mut self.viewport, size)
    }

    pub fn clip(&mut self, rect: [u32; 4]) -> bool {
        replace(&mut self.clip, rect)
    }

    pub fn vertex(&mut self, stream: usize, address: usize) -> bool {
        replace(&mut self.vertices[stream], address)
    }

    pub fn texture(&mut self, unit: usize, descriptor: [u32; 4], filter: TextureFilter) -> bool {
        replace(
            &mut self.textures[unit],
            SampledTexture { descriptor, filter },
        )
    }
}

fn replace<T: Copy + Eq>(current: &mut Option<T>, value: T) -> bool {
    if *current == Some(value) {
        false
    } else {
        *current = Some(value);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_switch_preserves_streams_and_textures_but_resets_clip() {
        let mut state = Bindings::default();
        let mut calls = [0; 4];
        for _ in 0..32 {
            state.begin_scene();
            for _ in 0..64 {
                calls[0] += usize::from(state.viewport([960, 544]));
                calls[1] += usize::from(state.clip([0, 0, 960, 544]));
                calls[2] += usize::from(state.vertex(0, 0x1000));
                calls[3] += usize::from(state.texture(0, [1, 2, 3, 4], TextureFilter::Linear));
            }
        }
        assert_eq!(calls, [1, 32, 1, 1]);
        assert!(state.viewport([480, 272]));
        assert!(state.clip([10, 20, 100, 200]));
        assert!(state.vertex(0, 0x2000));
        assert!(state.vertex(1, 0x2000));
        assert!(!state.vertex(1, 0x2000));
        assert!(state.vertex(1, 0x2030));
    }

    #[test]
    fn texture_slots_filters_and_descriptor_mutations_are_distinct() {
        let mut state = Bindings::default();
        assert!(state.texture(0, [1; 4], TextureFilter::Linear));
        assert!(!state.texture(0, [1; 4], TextureFilter::Linear));
        assert!(state.texture(1, [1; 4], TextureFilter::Linear));
        assert!(state.texture(0, [1; 4], TextureFilter::Nearest));
        for word in 0..4 {
            let mut descriptor = [1; 4];
            descriptor[word] = 2;
            assert!(state.texture(0, descriptor, TextureFilter::Nearest));
        }
    }
}
