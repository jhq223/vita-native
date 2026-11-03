//! Per-use tile-aligned writes without changing a framebuffer's sampled view.
use super::types::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WriteRegion {
    min: [u32; 2],
    max: [u32; 2],
}
impl WriteRegion {
    pub fn new(rect: [u32; 4], size: [u32; 2], macrotile: bool) -> Result<Self> {
        if macrotile {
            return Err(Error::Invalid("macrotile write region"));
        }
        let min = [rect[0], rect[1]];
        let max = [rect[0].checked_add(rect[2]), rect[1].checked_add(rect[3])];
        let [Some(x), Some(y)] = max else {
            return Err(Error::Invalid("framebuffer write region overflow"));
        };
        let max = [x, y];
        if (0..2).any(|axis| {
            min[axis] >= max[axis]
                || max[axis] > size[axis]
                || !min[axis].is_multiple_of(32)
                || (max[axis] != size[axis] && !max[axis].is_multiple_of(32))
        }) {
            return Err(Error::Invalid(
                "framebuffer write region alignment or extent",
            ));
        }
        Ok(Self { min, max })
    }

    pub fn end(self) -> [u32; 2] {
        self.max
    }

    pub fn intersect(self, min: [u32; 2], max: [u32; 2]) -> ([u32; 2], [u32; 2]) {
        (
            std::array::from_fn(|axis| min[axis].max(self.min[axis])),
            std::array::from_fn(|axis| max[axis].min(self.max[axis])),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_window_keeps_physical_edges_and_clips_clear_and_draws() {
        let region = WriteRegion::new([32, 64, 65, 65], [97, 129], false).unwrap();
        assert_eq!(region.end(), [97, 129]);
        assert_eq!(region.intersect([0, 0], [97, 129]), ([32, 64], [97, 129]));
        assert_eq!(region.intersect([40, 70], [60, 80]), ([40, 70], [60, 80]));
        assert_eq!(region.intersect([0, 0], [16, 16]), ([32, 64], [16, 16]));
        for rect in [
            [1, 0, 63, 64],
            [0, 0, 63, 64],
            [0, 0, 128, 64],
            [0, 0, 0, 64],
        ] {
            assert!(WriteRegion::new(rect, [97, 129], false).is_err());
        }
        assert!(WriteRegion::new([0, 0, 32, 32], [97, 129], true).is_err());
        assert!(WriteRegion::new([u32::MAX, 0, 2, 32], [97, 129], false).is_err());
    }
}
