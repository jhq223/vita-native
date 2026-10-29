//! One physical-domain account shared by independently suballocated arenas.
use crate::memory::PhysicalUsage;
use std::{cell::Cell, rc::Rc};

pub(super) struct Pool {
    capacity: Cell<u64>,
    committed: Cell<u64>,
}
impl Default for Pool {
    fn default() -> Self {
        Self {
            capacity: Cell::new(u64::MAX),
            committed: Cell::new(0),
        }
    }
}
impl Pool {
    pub fn initialize(&self, bytes: u64) {
        assert_eq!(
            self.committed.get(),
            0,
            "initialize before physical allocations"
        );
        self.capacity.set(bytes);
    }
    pub fn usage(&self) -> PhysicalUsage {
        PhysicalUsage {
            capacity_bytes: self.capacity.get(),
            committed_bytes: self.committed.get(),
        }
    }
    pub fn reserve(self: &Rc<Self>, bytes: u64) -> Option<Commit> {
        if bytes > self.usage().available_bytes() {
            return None;
        }
        self.committed.set(self.committed.get() + bytes);
        Some(Commit {
            pool: self.clone(),
            bytes,
        })
    }
}
pub(crate) struct Commit {
    pool: Rc<Pool>,
    bytes: u64,
}
impl Drop for Commit {
    fn drop(&mut self) {
        self.pool
            .committed
            .set(self.pool.committed.get() - self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arena_owners_share_capacity_without_fixed_partitions() {
        let pool = Rc::new(Pool::default());
        pool.initialize(12);
        let scanout = pool.reserve(2).unwrap();
        let pixels = pool.reserve(9).unwrap();
        assert!(pool.reserve(2).is_none());
        assert_eq!(pool.usage().committed_bytes, 11);
        drop(pixels);
        let glyphs = pool.reserve(10).unwrap();
        assert_eq!(pool.usage().available_bytes(), 0);
        drop((scanout, glyphs));
        assert_eq!(pool.usage().available_bytes(), 12);
    }
}
