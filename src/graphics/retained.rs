//! Bounded strong retention; removing a cache owner never retires external users.
use std::{prelude::v1::*, rc::Rc};

pub(super) struct Retained<T> {
    entries: Vec<(Rc<T>, u64)>,
    limit: u64,
}
impl<T> Retained<T> {
    pub fn new(limit: u64) -> Self {
        Self {
            entries: Vec::new(),
            limit,
        }
    }
    pub fn touch(&mut self, value: &Rc<T>, bytes: u64) {
        self.entries.retain(|(old, _)| !Rc::ptr_eq(old, value));
        if bytes > self.limit {
            return;
        }
        while self.bytes() > self.limit - bytes {
            self.entries.remove(0);
        }
        self.entries.push((value.clone(), bytes));
    }
    pub fn release_idle(&mut self) -> u64 {
        let before = self.bytes();
        self.entries
            .retain(|(value, _)| Rc::strong_count(value) != 1);
        before - self.bytes()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|(_, bytes)| bytes).sum()
    }
}

impl<T> super::pressure::IdlePool for std::cell::RefCell<Retained<T>> {
    fn release_idle(&self) -> u64 {
        self.borrow_mut().release_idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pressure_releases_only_idle_owners_and_never_invalidates_flights() {
        let mut cache = Retained::new(12);
        let a = Rc::new(1);
        let b = Rc::new(2);
        let c = Rc::new(3);
        cache.touch(&a, 4);
        cache.touch(&b, 8);
        let flight = a.clone();
        let weak_a = Rc::downgrade(&a);
        let weak_b = Rc::downgrade(&b);
        drop((a, b));
        cache.release_idle();
        assert!(weak_b.upgrade().is_none());
        assert_eq!(cache.bytes(), 4);
        cache.touch(&c, 12);
        assert_eq!(cache.bytes(), 12);
        assert_eq!(*weak_a.upgrade().unwrap(), 1);
        drop(flight);
        assert!(weak_a.upgrade().is_none());
        cache.clear();
        assert_eq!(*c, 3);
    }
}
