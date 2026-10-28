//! Fixed native call attribution; production builds erase clocks and counters.
#[cfg(feature = "profiling")]
use super::NativeCallTiming;
use super::SubmissionTiming;
#[cfg(feature = "profiling")]
use std::{cell::Cell, time::Instant};

#[derive(Clone, Copy)]
pub(super) enum Call {
    SceneBegin,
    SceneEnd,
    UniformReserve,
    Draw,
    NotificationCapacityWait,
    UploadCapacityWait,
    ResourceReplacementWait,
}
#[derive(Default)]
pub(super) struct Timings {
    #[cfg(feature = "profiling")]
    calls: [Cell<NativeCallTiming>; 7],
}
impl Timings {
    #[inline]
    pub fn measure<T>(&self, call: Call, operation: impl FnOnce() -> T) -> T {
        #[cfg(not(feature = "profiling"))]
        {
            let _ = call;
            operation()
        }
        #[cfg(feature = "profiling")]
        {
            let start = Instant::now();
            let result = operation();
            let elapsed = start.elapsed().as_micros().min(u64::MAX as u128) as u64;
            let slot = &self.calls[call as usize];
            let mut timing = slot.get();
            timing.calls = timing.calls.saturating_add(1);
            timing.cpu_us = timing.cpu_us.saturating_add(elapsed);
            slot.set(timing);
            result
        }
    }
    pub fn snapshot(&self) -> SubmissionTiming {
        #[cfg(not(feature = "profiling"))]
        {
            SubmissionTiming::default()
        }
        #[cfg(feature = "profiling")]
        {
            let get = |call: Call| self.calls[call as usize].get();
            SubmissionTiming {
                scene_begin: get(Call::SceneBegin),
                scene_end: get(Call::SceneEnd),
                uniform_reserve: get(Call::UniformReserve),
                draw: get(Call::Draw),
                notification_capacity_wait: get(Call::NotificationCapacityWait),
                upload_capacity_wait: get(Call::UploadCapacityWait),
                resource_replacement_wait: get(Call::ResourceReplacementWait),
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measurement_preserves_success_and_failure_with_feature_selected_accounting() {
        let timings = Timings::default();
        assert_eq!(timings.measure(Call::Draw, || Ok::<_, i32>(23)), Ok(23));
        assert_eq!(timings.measure(Call::Draw, || Err::<(), _>(-17)), Err(-17));
        for call in [
            Call::SceneBegin,
            Call::SceneEnd,
            Call::UniformReserve,
            Call::NotificationCapacityWait,
            Call::UploadCapacityWait,
            Call::ResourceReplacementWait,
        ] {
            assert_eq!(timings.measure(call, || 7), 7);
        }
        #[cfg(feature = "profiling")]
        {
            let snapshot = timings.snapshot();
            assert_eq!(snapshot.draw.calls, 2);
            for timing in [
                snapshot.scene_begin,
                snapshot.scene_end,
                snapshot.uniform_reserve,
                snapshot.notification_capacity_wait,
                snapshot.upload_capacity_wait,
                snapshot.resource_replacement_wait,
            ] {
                assert_eq!(timing.calls, 1);
            }
        }
        #[cfg(not(feature = "profiling"))]
        {
            assert_eq!(std::mem::size_of::<Timings>(), 0);
            assert_eq!(timings.snapshot(), SubmissionTiming::default());
        }
    }
}
