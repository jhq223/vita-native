//! Free intervals shared by native mapped arenas and CPU ownership checks.
use std::{ops::Range, prelude::v1::*};
/// Keep the preferred pool quantum when it fits, otherwise use the remaining
/// whole kernel pages. Admission is based on the minimum aligned request.
pub(super) fn block_size(
    bytes: usize,
    quantum: usize,
    page: usize,
    available: u64,
) -> Option<usize> {
    let minimum = bytes.checked_next_multiple_of(page)?;
    let preferred = bytes.max(quantum).checked_next_multiple_of(page)?;
    let capacity = available / page as u64 * page as u64;
    (capacity >= minimum as u64).then(|| (preferred as u64).min(capacity) as usize)
}

pub(super) struct FreeList {
    ranges: Vec<Range<usize>>,
}
impl FreeList {
    pub fn new(bytes: usize) -> Self {
        Self {
            ranges: std::vec![0..bytes],
        }
    }
    pub fn take(&mut self, bytes: usize, alignment: usize) -> Option<Range<usize>> {
        let (i, start) = self.ranges.iter().enumerate().find_map(|(i, r)| {
            let start = r.start.checked_add(alignment - 1)? & !(alignment - 1);
            (start.checked_add(bytes)? <= r.end).then_some((i, start))
        })?;
        let old = self.ranges.remove(i);
        if old.start < start {
            self.ranges.insert(i, old.start..start);
        }
        if start + bytes < old.end {
            self.ranges
                .insert(i + usize::from(old.start < start), start + bytes..old.end);
        }
        Some(start..start + bytes)
    }
    pub fn release(&mut self, range: Range<usize>) {
        let mut i = self.ranges.partition_point(|r| r.start < range.start);
        self.ranges.insert(i, range);
        if i > 0 && self.ranges[i - 1].end == self.ranges[i].start {
            let end = self.ranges.remove(i).end;
            i -= 1;
            self.ranges[i].end = end;
        }
        if i + 1 < self.ranges.len() && self.ranges[i].end == self.ranges[i + 1].start {
            self.ranges[i].end = self.ranges.remove(i + 1).end;
        }
    }
    pub fn bytes(&self) -> usize {
        self.ranges.iter().map(Range::len).sum()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pool_quantum_shrinks_to_whole_pages_within_remaining_capacity() {
        assert_eq!(block_size(32, 65536, 4096, 32768), Some(32768));
        assert_eq!(block_size(32, 65536, 4096, 1 << 20), Some(65536));
        assert_eq!(block_size(4097, 65536, 4096, 8191), None);
        assert_eq!(block_size(4097, 65536, 4096, 8192), Some(8192));
        assert_eq!(block_size(32, 262144, 262144, 131072), None);
        assert_eq!(block_size(262145, 262144, 262144, 524288), Some(524288));
    }

    #[test]
    fn many_small_resources_share_one_block_and_free_in_any_order() {
        let mut f = FreeList::new(256 * 1024);
        let allocations: Vec<_> = (0..1024).map(|_| f.take(64, 256).unwrap()).collect();
        assert_eq!(f.bytes(), 256 * 1024 - 64 * 1024);
        assert!(f.take(257, 256).is_none());
        for i in (0..allocations.len()).step_by(2) {
            f.release(allocations[i].clone());
        }
        for i in (1..allocations.len()).step_by(2).rev() {
            f.release(allocations[i].clone());
        }
        assert_eq!(f.take(256 * 1024, 256), Some(0..256 * 1024));
        assert_eq!(f.bytes(), 0);
    }
    #[test]
    fn aligned_split_keeps_prefix_and_suffix_for_other_allocations() {
        let mut f = FreeList::new(128);
        let a = f.take(3, 1).unwrap();
        let b = f.take(32, 32).unwrap();
        let c = f.take(29, 1).unwrap();
        assert_eq!((a.clone(), b.clone(), c.clone()), (0..3, 32..64, 3..32));
        f.release(b);
        f.release(a);
        f.release(c);
        assert_eq!(f.take(128, 1), Some(0..128));
    }
}
