//! Ordered owners retained by an open scene, without a separate identity index.
use std::vec::Vec;

pub(super) fn retain<T, K: Ord>(owners: &mut Vec<T>, value: T, key: impl Fn(&T) -> K) {
    if let Err(index) = owners.binary_search_by_key(&key(&value), key) {
        owners.insert(index, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{rc::Rc, time::Instant};

    #[test]
    fn ordered_owners_release_exactly_after_the_last_submission_reference() {
        let a = Rc::new(1);
        let b = Rc::new(2);
        let key = |owner: &Rc<i32>| **owner;
        let mut owners = Vec::new();
        retain(&mut owners, b.clone(), key);
        retain(&mut owners, a.clone(), key);
        retain(&mut owners, a.clone(), key);
        assert_eq!(owners.len(), 2);
        assert_eq!(Rc::strong_count(&a), 2);
        assert_eq!(Rc::strong_count(&b), 2);
        let submitted = owners;
        assert_eq!(Rc::strong_count(&a), 2);
        drop(submitted);
        assert_eq!(Rc::strong_count(&a), 1);
        assert_eq!(Rc::strong_count(&b), 1);
    }

    #[test]
    #[ignore = "release-mode ordered owner cost measurement"]
    fn repeated_scene_owner_cost() {
        for count in [8, 64, 512] {
            for indexed in [false, true] {
                let mut owners = Vec::new();
                let start = Instant::now();
                for draw in 0..2048 {
                    for item in 0..count {
                        let value = std::hint::black_box(((item * 17 + draw) % count, [0usize; 2]));
                        if indexed {
                            retain(&mut owners, value, |owner| owner.0);
                        } else if !owners.iter().any(|owner| owner.0 == value.0) {
                            owners.push(value);
                        }
                    }
                }
                assert_eq!(owners.len(), count);
                eprintln!(
                    "owners={count} indexed={indexed} references={} cpu_us={} capacity_bytes={}",
                    count * 2048,
                    start.elapsed().as_micros(),
                    owners.capacity() * size_of_val(&owners[0])
                );
                owners.sort_by_key(|owner| owner.0);
                assert!(
                    owners
                        .iter()
                        .enumerate()
                        .all(|(index, owner)| index == owner.0)
                );
            }
        }
    }
}
