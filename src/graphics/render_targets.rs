//! Firmware scheduling storage shared independently of color-surface pixels.
use super::{layout::validate_size, types::Result};
use std::{
    prelude::v1::*,
    rc::{Rc, Weak},
};

// Six size classes per axis keep all surface combinations below the SDK limit of 48.
// libgxm Overview, Render Targets: valid regions permit smaller color surfaces.
#[cfg(test)]
const CLASSES_PER_AXIS: usize = 6;
#[cfg(test)]
const CLASSES: usize = CLASSES_PER_AXIS * CLASSES_PER_AXIS;
const TARGET_CAPACITY: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    size: [u32; 2],
    flags: u32,
}

pub(super) fn extent(size: [u32; 2]) -> Result<[u32; 2]> {
    validate_size(size)?;
    Ok(size.map(|n| n.max(128).next_power_of_two()))
}

#[cfg(test)]
fn index(size: [u32; 2]) -> usize {
    (size[0].ilog2() as usize - 7) * CLASSES_PER_AXIS + size[1].ilog2() as usize - 7
}

pub(super) struct Pool<T> {
    slots: [Weak<T>; TARGET_CAPACITY],
    keys: [Option<Key>; TARGET_CAPACITY],
    retained: super::retained::Retained<T>,
}
impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self::new(0)
    }
}
impl<T> Pool<T> {
    pub fn new(retained_bytes: u64) -> Self {
        Self {
            slots: std::array::from_fn(|_| Weak::new()),
            keys: [None; TARGET_CAPACITY],
            retained: super::retained::Retained::new(retained_bytes),
        }
    }
    #[cfg(test)]
    pub fn contains(&self, size: [u32; 2]) -> Result<bool> {
        let key = Key {
            size: extent(size)?,
            flags: 0,
        };
        Ok(self
            .keys
            .iter()
            .zip(&self.slots)
            .any(|(k, slot)| *k == Some(key) && slot.strong_count() != 0))
    }
    pub fn release_idle(&mut self) -> u64 {
        self.retained.release_idle()
    }
    pub fn clear(&mut self) {
        self.retained.clear();
    }

    pub fn get_or_create(
        pool: &std::cell::RefCell<Self>,
        size: [u32; 2],
        bytes: u64,
        create: impl FnOnce([u32; 2]) -> Result<Rc<T>>,
    ) -> Result<Rc<T>> {
        let size = extent(size)?;
        Self::get_or_create_key(pool, Key { size, flags: 0 }, bytes, || create(size))
    }
    pub fn get_or_create_macrotile(
        pool: &std::cell::RefCell<Self>,
        layout: super::MacrotileLayout,
        bytes: u64,
        create: impl FnOnce() -> Result<Rc<T>>,
    ) -> Result<Rc<T>> {
        Self::get_or_create_key(
            pool,
            Key {
                size: layout.size(),
                flags: layout.flags(),
            },
            bytes,
            create,
        )
    }
    fn get_or_create_key(
        pool: &std::cell::RefCell<Self>,
        key: Key,
        bytes: u64,
        create: impl FnOnce() -> Result<Rc<T>>,
    ) -> Result<Rc<T>> {
        {
            let mut pool = pool.borrow_mut();
            let cached = pool
                .keys
                .iter()
                .zip(&pool.slots)
                .find_map(|(k, slot)| (*k == Some(key)).then(|| slot.upgrade()).flatten());
            if let Some(target) = cached {
                pool.retained.touch(&target, bytes);
                return Ok(target);
            }
            if pool.slots.iter().all(|slot| slot.strong_count() != 0) {
                pool.retained.release_idle();
            }
            if pool.slots.iter().all(|slot| slot.strong_count() != 0) {
                return Err(super::Error::Budget {
                    domain: "GXM render targets",
                    requested: 49,
                    limit: 48,
                });
            }
        }
        // Creating kernel storage may reclaim this pool's idle owners.
        let target = create()?;
        let mut pool = pool.borrow_mut();
        let slot = pool
            .slots
            .iter()
            .position(|slot| slot.strong_count() == 0)
            .expect("non-reentrant target creation retains a free slot");
        pool.keys[slot] = Some(key);
        pool.slots[slot] = Rc::downgrade(&target);
        pool.retained.touch(&target, bytes);
        Ok(target)
    }

    pub fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.strong_count() != 0).count()
    }
}

impl<T> super::pressure::IdlePool for std::cell::RefCell<Pool<T>> {
    fn release_idle(&self) -> u64 {
        self.borrow_mut().release_idle()
    }
}

#[cfg(target_os = "vita")]
pub(super) struct RenderTarget {
    pub raw: *mut vitasdk_sys::SceGxmRenderTarget,
    driver: Rc<super::memory::Block>,
    owner: Rc<super::device::Core>,
}
#[cfg(target_os = "vita")]
impl RenderTarget {
    pub fn new(owner: &Rc<super::device::Core>, size: [u32; 2]) -> Result<Rc<Self>> {
        Self::with_params(
            owner,
            super::resources::rt_params(size, owner.config.scenes_per_frame),
        )
    }
    pub fn macrotile(
        owner: &Rc<super::device::Core>,
        layout: super::MacrotileLayout,
    ) -> Result<Rc<Self>> {
        let mut params = super::resources::rt_params(layout.size(), owner.config.scenes_per_frame);
        params.flags = layout.flags();
        Self::with_params(owner, params)
    }
    fn with_params(
        owner: &Rc<super::device::Core>,
        mut params: vitasdk_sys::SceGxmRenderTargetParams,
    ) -> Result<Rc<Self>> {
        use super::memory::{Block, Kind, sdk};
        use vitasdk_sys::*;
        let mut bytes = 0;
        // SAFETY: complete scalar parameters and writable size output.
        sdk("sceGxmGetRenderTargetMemSize", unsafe {
            sceGxmGetRenderTargetMemSize(&params, &mut bytes)
        })?;
        let driver = Block::new(bytes as usize, Kind::Driver, &owner.fault, &owner.pressure)?;
        params.driverMemBlock = driver.uid;
        let mut raw = core::ptr::null_mut();
        // SAFETY: the uncached driver block remains owned until target destruction.
        sdk("sceGxmCreateRenderTarget", unsafe {
            sceGxmCreateRenderTarget(&params, &mut raw)
        })?;
        owner
            .rt_bytes
            .set(owner.rt_bytes.get() + driver.bytes as u64);
        Ok(Rc::new(Self {
            raw,
            driver,
            owner: owner.clone(),
        }))
    }
}
#[cfg(target_os = "vita")]
impl Drop for RenderTarget {
    fn drop(&mut self) {
        if self.owner.fault.get().is_some() {
            return;
        }
        // SAFETY: the final surface/submission owner has retired.
        if self
            .owner
            .fault
            .command("sceGxmDestroyRenderTarget", unsafe {
                vitasdk_sys::sceGxmDestroyRenderTarget(self.raw)
            })
            .is_ok()
        {
            self.owner
                .rt_bytes
                .set(self.owner.rt_bytes.get() - self.driver.bytes as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn every_supported_extent_fits_one_of_36_scheduling_classes() {
        let mut axes = std::collections::BTreeSet::new();
        for n in 1..=4096 {
            let [w, h] = extent([n, 4097 - n]).unwrap();
            assert!(w >= n && h >= 4097 - n);
            axes.insert(w);
        }
        assert_eq!(axes.len(), CLASSES_PER_AXIS);
        let mut slots = std::collections::BTreeSet::new();
        for &w in &axes {
            for &h in &axes {
                slots.insert(index([w, h]));
            }
        }
        assert_eq!(
            slots.into_iter().collect::<Vec<_>>(),
            (0..CLASSES).collect::<Vec<_>>()
        );
        assert!(extent([0, 544]).is_err());
        assert!(extent([4097, 544]).is_err());
    }

    #[test]
    fn surfaces_and_submissions_share_backing_until_the_last_owner_retires() {
        let pool = RefCell::new(Pool::default());
        let creates = Cell::new(0);
        let create = |extent| {
            creates.set(creates.get() + 1);
            Ok(Rc::new(extent))
        };
        let first = Pool::get_or_create(&pool, [960, 544], 4096, create).unwrap();
        let second = Pool::get_or_create(&pool, [1024, 700], 4096, create).unwrap();
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(creates.get(), 1);
        let submission = first.clone();
        drop((first, second));
        assert_eq!(pool.borrow().live_count(), 1);
        let third = Pool::get_or_create(&pool, [900, 600], 4096, create).unwrap();
        assert!(Rc::ptr_eq(&submission, &third));
        drop((submission, third));
        assert_eq!(pool.borrow().live_count(), 0);
        let next = Pool::get_or_create(&pool, [960, 544], 4096, create).unwrap();
        assert_eq!(creates.get(), 2);
        assert_eq!(*next, [1024, 1024]);
    }

    #[test]
    fn alternating_dimensions_reuse_bounded_targets_after_surfaces_retire() {
        let pool = RefCell::new(Pool::new(8192));
        let creates = Cell::new(0);
        for _ in 0..1000 {
            for size in [[128, 128], [256, 256]] {
                drop(
                    Pool::get_or_create(&pool, size, 4096, |extent| {
                        creates.set(creates.get() + 1);
                        Ok(Rc::new(extent))
                    })
                    .unwrap(),
                );
            }
        }
        assert_eq!(creates.get(), 2);
        assert_eq!(pool.borrow().retained.bytes(), 8192);
        assert!(pool.borrow().contains([128, 128]).unwrap());
        let flight = Pool::get_or_create(&pool, [128, 128], 4096, |_| unreachable!()).unwrap();
        pool.borrow_mut().release_idle();
        assert_eq!(pool.borrow().live_count(), 1);
        pool.borrow_mut().clear();
        assert_eq!(*flight, [128, 128]);
        drop(flight);
        assert_eq!(pool.borrow().live_count(), 0);
        eprintln!(
            "target retention: 2000 requests, 2 creations, 8192 retained bytes; baseline 2000 creations"
        );
    }

    #[test]
    fn failed_creation_leaves_the_class_available() {
        let pool = RefCell::new(Pool::<[u32; 2]>::default());
        assert!(
            Pool::get_or_create(&pool, [960, 544], 4096, |_| Err(
                super::super::Error::Invalid("creation")
            ))
            .is_err()
        );
        assert_eq!(pool.borrow().live_count(), 0);
        let target =
            Pool::get_or_create(&pool, [960, 544], 4096, |size| Ok(Rc::new(size))).unwrap();
        assert_eq!(pool.borrow().live_count(), 1);
        assert_eq!(*target, [1024, 1024]);
    }

    #[test]
    fn macrotile_grids_have_distinct_driver_keys_and_share_one_bounded_pool() {
        use super::super::MacrotileLayout;
        let pool = RefCell::new(Pool::default());
        let ordinary =
            Pool::get_or_create(&pool, [256, 256], 4096, |size| Ok(Rc::new(size))).unwrap();
        let a = MacrotileLayout::new([128, 128], [2, 2]).unwrap();
        let b = MacrotileLayout::new([128, 256], [2, 1]).unwrap();
        let first =
            Pool::get_or_create_macrotile(&pool, a, 4096, || Ok(Rc::new(a.size()))).unwrap();
        let second =
            Pool::get_or_create_macrotile(&pool, b, 4096, || Ok(Rc::new(b.size()))).unwrap();
        assert_eq!(*first, *ordinary);
        assert_eq!(*second, *ordinary);
        assert!(!Rc::ptr_eq(&first, &ordinary));
        assert!(!Rc::ptr_eq(&first, &second));
        let hit = Pool::get_or_create_macrotile(&pool, a, 4096, || unreachable!()).unwrap();
        assert!(Rc::ptr_eq(&first, &hit));
        drop((ordinary, first, second, hit));
        let mut held = Vec::new();
        for x in 1..=6 {
            for y in 1..=8 {
                let layout = MacrotileLayout::new([x * 128, y * 128], [2, 2]).unwrap();
                held.push(
                    Pool::get_or_create_macrotile(&pool, layout, 4096, || {
                        Ok(Rc::new(layout.size()))
                    })
                    .unwrap(),
                );
            }
        }
        assert_eq!(pool.borrow().live_count(), 48);
        let extra = MacrotileLayout::new([896, 128], [2, 2]).unwrap();
        assert!(Pool::get_or_create_macrotile(&pool, extra, 4096, || unreachable!()).is_err());
        held.pop();
        let replacement =
            Pool::get_or_create_macrotile(&pool, extra, 4096, || Ok(Rc::new(extra.size())))
                .unwrap();
        assert_eq!(*replacement, extra.size());
        assert_eq!(pool.borrow().live_count(), 48);
    }
}
