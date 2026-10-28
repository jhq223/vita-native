//! Free intervals shared by native mapped arenas and CPU ownership checks.
use std::{cell::Cell, ops::Range, prelude::v1::*, rc::Rc};
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

/// Short upload staging is separate from retained graphics resources. Large
/// allocations own whole blocks so small survivors cannot pin their tails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lifetime {
    Retained,
    Upload,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Class(pub usize);
impl Class {
    pub const COUNT: usize = 6;
    pub fn new(bytes: usize, quantum: usize, lifetime: Lifetime) -> Self {
        let size = if bytes > quantum {
            2
        } else if bytes > quantum / 4 {
            1
        } else {
            0
        };
        Self(size + usize::from(lifetime == Lifetime::Upload) * 3)
    }
    pub fn dedicated(self) -> bool {
        self.0 % 3 == 2
    }
}

/// A pooled block contributes capacity until its kernel allocation is freed.
/// Failed native cleanup retains this ticket together with physical accounting.
pub(super) struct BlockReservation {
    total: Rc<Cell<u64>>,
    bytes: u64,
}
impl BlockReservation {
    pub fn new(total: Rc<Cell<u64>>, bytes: u64) -> Self {
        total.set(total.get() + bytes);
        Self { total, bytes }
    }
}
impl Drop for BlockReservation {
    fn drop(&mut self) {
        self.total.set(self.total.get() - self.bytes);
    }
}

pub(super) struct FreeList {
    ranges: Vec<Range<usize>>,
    free_bytes: usize,
    cursor: usize,
    alignment: usize,
    #[cfg(test)]
    inspected: usize,
}
impl FreeList {
    pub fn new(bytes: usize) -> Self {
        Self {
            ranges: std::vec![0..bytes],
            free_bytes: bytes,
            cursor: 0,
            alignment: 1,
            #[cfg(test)]
            inspected: 0,
        }
    }
    pub fn take(&mut self, bytes: usize, alignment: usize) -> Option<Range<usize>> {
        if bytes > self.free_bytes {
            return None;
        }
        if alignment != self.alignment {
            self.alignment = alignment;
            self.cursor = 0;
        }
        let cursor = self.cursor.min(self.ranges.len());
        let (i, start) = (cursor..self.ranges.len()).chain(0..cursor).find_map(|i| {
            #[cfg(test)]
            {
                self.inspected += 1;
            }
            let r = &self.ranges[i];
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
        self.free_bytes -= bytes;
        self.cursor = i + usize::from(old.start < start);
        Some(start..start + bytes)
    }
    pub fn release(&mut self, range: Range<usize>) {
        self.free_bytes += range.len();
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
        self.cursor = i;
    }
    pub fn bytes(&self) -> usize {
        self.free_bytes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_capacity_follows_block_reservations() {
        let total = Rc::new(Cell::new(0));
        let a = Rc::new(BlockReservation::new(total.clone(), 65536));
        let b = BlockReservation::new(total.clone(), 131072);
        let flight = a.clone();
        drop(a);
        assert_eq!(total.get(), 196608);
        drop(b);
        assert_eq!(total.get(), 65536);
        drop(flight);
        assert_eq!(total.get(), 0);
    }

    #[test]
    fn resource_classes_keep_uploads_and_large_blocks_separate() {
        let quantum = 64 * 1024;
        assert_eq!(
            Class::new(quantum + 1, quantum, Lifetime::Upload).0 + 1,
            Class::COUNT
        );
        assert_eq!(Class::new(4, quantum, Lifetime::Retained), Class(0));
        assert_eq!(Class::new(32 * 1024, quantum, Lifetime::Retained), Class(1));
        assert!(Class::new(quantum + 1, quantum, Lifetime::Retained).dedicated());
        assert_eq!(Class::new(4, quantum, Lifetime::Upload), Class(3));
        for size in [4, 32 * 1024, 128 * 1024] {
            assert_ne!(
                Class::new(size, quantum, Lifetime::Retained),
                Class::new(size, quantum, Lifetime::Upload)
            );
        }
    }

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
        assert_eq!(
            f.inspected, 1024,
            "aligned holes must not be rescanned for each resource"
        );
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
