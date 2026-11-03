//! GXM macrotile geometry and ordered feedback admission.
//! SDK render_target.h requires 1..=4 cells per axis, at least two cells,
//! 128-pixel cell alignment and at most 1024 pixels on either cell axis.

use super::{Error, types::Result};
#[cfg(target_os = "vita")]
mod target;
#[cfg(target_os = "vita")]
pub use target::MacrotileTarget;

/// Equal-sized cells processed down each column before advancing to the next.
/// Applications assign their passes to cells; native commands enforce ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacrotileLayout {
    cell: [u32; 2],
    grid: [u32; 2],
    stages: u32,
}
impl MacrotileLayout {
    /// Validate 1..=4 cells per axis, at least two cells, and 128-pixel aligned
    /// cell dimensions in 128..=1024 for single-sample linear RGBA8 storage.
    pub fn new(cell_size: [u32; 2], grid: [u32; 2]) -> Result<Self> {
        if cell_size
            .iter()
            .any(|n| *n == 0 || *n > 1024 || n % 128 != 0)
            || grid.iter().any(|n| !(1..=4).contains(n))
            || grid == [1, 1]
        {
            return Err(Error::Invalid("macrotile geometry"));
        }
        Ok(Self {
            cell: cell_size,
            grid,
            stages: grid[0] * grid[1],
        })
    }
    /// Physical dimensions of each cell, including padding.
    pub fn cell_size(self) -> [u32; 2] {
        self.cell
    }
    /// Cell counts along the X and Y axes.
    pub fn grid(self) -> [u32; 2] {
        self.grid
    }
    /// Number of physical cells in the atlas.
    pub fn cell_count(self) -> u32 {
        self.stages
    }
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn validate_sizes(self, sizes: &[[u32; 2]]) -> Result<()> {
        if sizes.len() < 2
            || sizes.len() > self.stages as usize
            || sizes
                .iter()
                .any(|s| (0..2).any(|a| s[a] == 0 || s[a] > self.cell[a]))
        {
            return Err(Error::Invalid("macrotile logical sizes"));
        }
        Ok(())
    }
    #[cfg(test)]
    fn for_sizes(sizes: &[[u32; 2]], limit: u32) -> Option<Self> {
        if !(2..=16).contains(&sizes.len()) || sizes.iter().any(|size| size.contains(&0)) {
            return None;
        }
        let cell = [0, 1]
            .map(|axis| {
                sizes
                    .iter()
                    .map(|size| size[axis])
                    .max()
                    .unwrap()
                    .max(128)
                    .checked_next_power_of_two()
            })
            .map(|size| size.filter(|size| *size <= 1024));
        let cell = [cell[0]?, cell[1]?];
        // Power-of-two cells/grids fit the existing scheduling size classes.
        // Minimize cell count first, then the longest surface axis.
        let grid = [
            [1, 2],
            [2, 1],
            [2, 2],
            [1, 4],
            [4, 1],
            [2, 4],
            [4, 2],
            [4, 4],
        ]
        .into_iter()
        .filter(|grid| grid[0] * grid[1] >= sizes.len() as u32)
        .filter(|grid| (0..2).all(|axis| cell[axis] * grid[axis] <= limit))
        .min_by_key(|grid| {
            (
                grid[0] * grid[1],
                (cell[0] * grid[0]).max(cell[1] * grid[1]),
            )
        })?;
        let mut layout = Self::new(cell, grid).ok()?;
        layout.stages = sizes.len() as u32;
        Some(layout)
    }
    /// Full atlas dimensions, including every cell's padding.
    pub fn size(self) -> [u32; 2] {
        [self.cell[0] * self.grid[0], self.cell[1] * self.grid[1]]
    }
    /// Top-left x/y/width/height of a cell in GPU execution order.
    pub fn cell_rect(self, index: u32) -> Option<[u32; 4]> {
        if index >= self.stages {
            return None;
        }
        // GXM processes a column before advancing to the next column.
        Some([
            index / self.grid[1] * self.cell[0],
            index % self.grid[1] * self.cell[1],
            self.cell[0],
            self.cell[1],
        ])
    }
    #[cfg(any(target_os = "vita", test))]
    pub(super) fn flags(self) -> u32 {
        // SCE_GXM_RENDER_TARGET_MACROTILE_SYNC and COUNT_X/Y_SHIFT.
        0x2 | self.grid[0] << 8 | self.grid[1] << 12
    }
    #[cfg(test)]
    fn cell_containing(self, rect: [u32; 4]) -> Option<u32> {
        let [x, y, width, height] = rect;
        if width == 0 || height == 0 {
            return None;
        }
        let column = x / self.cell[0];
        let row = y / self.cell[1];
        if column >= self.grid[0]
            || row >= self.grid[1]
            || x.checked_add(width)? > (column + 1) * self.cell[0]
            || y.checked_add(height)? > (row + 1) * self.cell[1]
        {
            return None;
        }
        let index = column * self.grid[1] + row;
        (index < self.stages).then_some(index)
    }
    #[cfg(test)]
    fn feedback(self, source: [u32; 4], destination: [u32; 4], previous: Option<u32>) -> bool {
        let (Some(source), Some(destination)) = (
            self.cell_containing(source),
            self.cell_containing(destination),
        ) else {
            return false;
        };
        source < destination && previous.is_none_or(|previous| previous <= destination)
    }
}

#[cfg(any(target_os = "vita", test))]
#[derive(Clone, Copy)]
pub(super) struct Cell {
    pub layout: MacrotileLayout,
    pub index: u32,
    pub backing: usize,
}
#[cfg(any(target_os = "vita", test))]
impl Cell {
    pub fn follows(self, previous: Self) -> Result<bool> {
        if self.backing != previous.backing {
            return Ok(false);
        }
        if self.index < previous.index {
            return Err(Error::Invalid("macrotile write order"));
        }
        Ok(true)
    }
    pub fn reads(self, source: Self) -> bool {
        self.backing != source.backing || source.index < self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type Layout = MacrotileLayout;
    #[test]
    fn native_cell_sequence_allows_skips_and_rejects_current_or_future_feedback() {
        let layout = Layout::new([256, 128], [2, 2]).unwrap();
        assert!(layout.validate_sizes(&[[137, 93]; 4]).is_ok());
        assert!(layout.validate_sizes(&[[257, 93]; 2]).is_err());
        assert!(layout.validate_sizes(&[[137, 0]; 2]).is_err());
        assert!(layout.validate_sizes(&[[137, 93]]).is_err());
        assert!(layout.validate_sizes(&[[137, 93]; 5]).is_err());
        let cells = [0, 1, 2, 3].map(|index| Cell {
            layout,
            index,
            backing: 1,
        });
        assert_eq!(cells[0].layout.size(), [512, 256]);
        for previous in cells {
            for next in cells {
                assert_eq!(next.follows(previous).is_ok(), next.index >= previous.index);
                assert_eq!(next.reads(previous), next.index > previous.index);
            }
        }
        let external = Cell {
            backing: 2,
            ..cells[3]
        };
        assert!(!cells[0].follows(external).unwrap());
        assert!(cells[0].reads(external));
        for size in [[0, 128], [127, 128], [1152, 128], [u32::MAX, 128]] {
            assert!(Layout::new(size, [2, 2]).is_err());
        }
        for grid in [[0, 2], [1, 1], [1, 5]] {
            assert!(Layout::new([128, 128], grid).is_err());
        }
    }
    #[test]
    fn column_order_reads_only_finished_cells_and_rejects_cross_cell_sampling() {
        let layout = Layout::for_sizes(&[[96, 80]; 4], 4096).unwrap();
        assert_eq!(layout.size(), [256, 256]);
        assert_eq!(layout.flags(), 0x2202);
        let cells = [0, 1, 2, 3].map(|index| layout.cell_rect(index).unwrap());
        assert_eq!(
            cells,
            [
                [0, 0, 128, 128],
                [0, 128, 128, 128],
                [128, 0, 128, 128],
                [128, 128, 128, 128]
            ]
        );
        assert!(layout.feedback(cells[0], cells[1], Some(0)));
        assert!(layout.feedback(cells[1], cells[2], Some(1)));
        assert!(!layout.feedback(cells[1], cells[1], None));
        assert!(!layout.feedback(cells[2], cells[1], None));
        assert!(!layout.feedback(cells[0], cells[1], Some(2)));
        assert!(!layout.feedback([127, 0, 2, 1], cells[2], None));
        assert!(layout.cell_rect(4).is_none());
    }
    #[test]
    fn allocation_shape_obeys_hardware_limits_without_clipping_the_source() {
        assert_eq!(
            Layout::for_sizes(&[[832, 288]; 2], 4096).unwrap().size(),
            [1024, 1024]
        );
        assert_eq!(
            Layout::for_sizes(&[[129, 129]; 3], 4096).unwrap().size(),
            [512, 512]
        );
        assert!(Layout::for_sizes(&[[1025, 1]; 2], 4096).is_none());
        assert!(Layout::for_sizes(&[[1, 0]; 2], 4096).is_none());
        assert!(Layout::for_sizes(&[[128, 128]], 4096).is_none());
        assert!(Layout::for_sizes(&[[1, 1]; 17], 4096).is_none());
        assert!(Layout::for_sizes(&[[u32::MAX, 1]; 2], 4096).is_none());
        assert!(Layout::for_sizes(&[[1024, 1024]; 16], 2048).is_none());
    }
}
