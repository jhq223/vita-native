//! Fixed native call attribution; production builds erase clocks and counters.
#[cfg(feature = "profiling")]
use super::NativeCallTiming;
use super::SubmissionTiming;
#[cfg(feature = "profiling")]
use super::{
    NotificationTiming, PendingNotifications, SceneQueueTiming, SlowSceneEnd, ThreadRunTiming,
};
#[cfg(feature = "profiling")]
use std::{cell::Cell, rc::Rc, time::Instant};

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
    #[cfg(feature = "profiling")]
    thread: Cell<ThreadRunTiming>,
    #[cfg(feature = "profiling")]
    queue: Cell<SceneQueueTiming>,
    #[cfg(feature = "profiling")]
    notifications: Rc<Cell<NotificationTiming>>,
    #[cfg(feature = "profiling")]
    next_thread_sample_us: Cell<u64>,
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
                scene_end_thread: self.thread.get(),
                scene_queue: self.queue.get(),
                notifications: self.notifications.get(),
                display: super::DisplayTiming::default(),
            }
        }
    }
}
/// The same process-monotonic clock used by Vita hosts; no wall-clock adjustment.
#[cfg(feature = "profiling")]
pub(super) fn clock_us() -> u64 {
    #[cfg(target_os = "vita")]
    {
        // SAFETY: scalar process clock query with no retained pointers.
        unsafe { vitasdk_sys::sceKernelGetProcessTimeWide() }
    }
    #[cfg(not(target_os = "vita"))]
    {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
    }
}

/// Independent counters do not retain the context, its leases, or its SDK owners.
#[cfg(feature = "profiling")]
#[derive(Default)]
pub(super) struct NotificationProfile {
    pub target_id: u64,
    pub submitted_us: u64,
    generation: u32,
    counters: Option<Rc<Cell<NotificationTiming>>>,
}
#[cfg(feature = "profiling")]
impl NotificationProfile {
    pub fn observed(&self, now: u64) {
        if let Some(counters) = &self.counters {
            let mut value = counters.get();
            let age = now.saturating_sub(self.submitted_us);
            value.observations += 1;
            value.observed_age_us += age;
            value.max_observed_age_us = value.max_observed_age_us.max(age);
            value.last_observed_us = now;
            value.last_observed_generation = self.generation;
            counters.set(value);
        }
    }
}

#[cfg(feature = "profiling")]
#[derive(Clone, Copy)]
pub(super) struct ThreadQuery {
    started_us: u64,
    ended_us: u64,
    run_us: Result<u64, i32>,
}
#[cfg(all(feature = "profiling", target_os = "vita"))]
impl ThreadQuery {
    fn read() -> Self {
        let started_us = clock_us();
        // SAFETY: the SDK fills an exclusive, zero-initialized fixed-size struct;
        // SELF queries only the issuing thread and retains no pointer. Profiling
        // never uses query results for control flow or replaces a graphics error.
        let mut info: vitasdk_sys::SceKernelThreadInfo = unsafe { core::mem::zeroed() };
        info.size = core::mem::size_of_val(&info) as u32;
        // SAFETY: SELF is the current caller; `info` has the SDK size and remains
        // exclusive until the synchronous query returns. No pointer is retained.
        let code = unsafe {
            vitasdk_sys::sceKernelGetThreadInfo(
                vitasdk_sys::SCE_KERNEL_THREAD_ID_SELF as i32,
                &mut info,
            )
        };
        Self {
            started_us,
            ended_us: clock_us(),
            run_us: if code < 0 {
                Err(code)
            } else {
                Ok(info.runClocks)
            },
        }
    }
}
#[cfg(feature = "profiling")]
pub(super) struct SceneProbe {
    pub notification: u32,
    pub target_id: u64,
    pub extent: [u32; 2],
    pub before: PendingNotifications,
    pub work: super::SceneWork,
    pub entered_us: u64,
    query: Option<ThreadQuery>,
}
#[cfg(feature = "profiling")]
impl Timings {
    pub fn published(&self, target_id: u64, generation: u32, now: u64) -> NotificationProfile {
        let mut value = self.notifications.get();
        value.submissions += 1;
        value.last_submit_us = now;
        value.last_submit_generation = generation;
        self.notifications.set(value);
        NotificationProfile {
            target_id,
            submitted_us: now,
            generation,
            counters: Some(self.notifications.clone()),
        }
    }
    pub fn observe_pending(&self, pending: PendingNotifications) {
        let mut value = self.notifications.get();
        value.peak_pending = value.peak_pending.max(pending.count);
        value.peak_same_target = value.peak_same_target.max(pending.same_target);
        self.notifications.set(value);
    }
    fn thread_sample_due(&self, now: u64) -> bool {
        if now < self.next_thread_sample_us.get() {
            return false;
        }
        self.next_thread_sample_us.set(now.saturating_add(100_000));
        true
    }
    #[cfg(target_os = "vita")]
    pub fn start_scene_end(
        &self,
        notification: u32,
        target_id: u64,
        extent: [u32; 2],
        work: super::SceneWork,
        before: PendingNotifications,
    ) -> SceneProbe {
        let now = clock_us();
        let query = self.thread_sample_due(now).then(ThreadQuery::read);
        SceneProbe {
            notification,
            target_id,
            extent,
            before,
            work,
            query,
            entered_us: clock_us(),
        }
    }
    #[cfg(target_os = "vita")]
    pub fn finish_scene_end(
        &self,
        probe: SceneProbe,
        result: i32,
        pending: impl FnOnce(u64) -> PendingNotifications,
    ) {
        let exited_us = clock_us();
        let query = probe.query.map(|_| ThreadQuery::read());
        self.record_scene_end(probe, exited_us, query, result, pending);
    }
    fn record_scene_end(
        &self,
        probe: SceneProbe,
        exited_us: u64,
        query: Option<ThreadQuery>,
        result: i32,
        pending: impl FnOnce(u64) -> PendingNotifications,
    ) {
        let sdk_wall = exited_us.checked_sub(probe.entered_us);
        let mut thread = self.thread.get();
        let mut clock_regressed = sdk_wall.is_none();
        for q in [probe.query, query].into_iter().flatten() {
            thread.query_calls += 1;
            if let Some(wall) = q.ended_us.checked_sub(q.started_us) {
                thread.query_wall_us += wall;
            } else {
                clock_regressed = true;
            }
            if let Err(error) = q.run_us {
                thread.query_failures += 1;
                thread.last_query_error = Some(error);
            }
        }
        let mut paired_wall = None;
        let run = match (probe.query, query) {
            (Some(before), Some(after)) => {
                let wall = after.ended_us.checked_sub(before.started_us);
                clock_regressed |= wall.is_none();
                match (before.run_us, after.run_us) {
                    (Ok(before), Ok(after)) => {
                        let run = after.checked_sub(before);
                        clock_regressed |= run.is_none();
                        if !clock_regressed {
                            thread.samples += 1;
                            thread.wall_us += wall.unwrap();
                            thread.run_us += run.unwrap();
                            paired_wall = wall;
                            run
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if probe.query.is_some() && clock_regressed {
            thread.clock_regressions += 1;
        }
        self.thread.set(thread);
        self.observe_pending(probe.before);
        let mut queue = self.queue.get();
        if let Some(wall) = sdk_wall {
            queue.max_wall_us = queue.max_wall_us.max(wall);
            if wall >= 1000 {
                let after = pending(query.map_or(exited_us, |q| q.ended_us));
                self.observe_pending(after);
                queue.slow_calls += 1;
                queue.last_slow = Some(SlowSceneEnd {
                    notification: probe.notification,
                    target_id: probe.target_id,
                    target_extent: probe.extent,
                    work: probe.work,
                    entered_us: probe.entered_us,
                    exited_us,
                    thread_run_us: run,
                    thread_wall_us: paired_wall,
                    thread_sampled: probe.query.is_some(),
                    thread_query_error: probe
                        .query
                        .and_then(|q| q.run_us.err())
                        .or(query.and_then(|q| q.run_us.err())),
                    before: probe.before,
                    after,
                    result,
                });
            }
        } else {
            queue.clock_regressions += 1;
        }
        self.queue.set(queue);
    }
}

#[derive(Clone, Copy)]
pub(super) enum DisplayCall {
    Set,
    Wait,
}
#[cfg(feature = "profiling")]
#[derive(Default)]
struct AtomicCall {
    calls: std::sync::atomic::AtomicU64,
    wall_us: std::sync::atomic::AtomicU64,
}
#[cfg(feature = "profiling")]
impl AtomicCall {
    fn add(&self, wall: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        self.wall_us.fetch_add(wall, Relaxed);
        self.calls.fetch_add(1, Relaxed);
    }
    fn snapshot(&self) -> NativeCallTiming {
        use std::sync::atomic::Ordering::Relaxed;
        NativeCallTiming {
            calls: self.calls.load(Relaxed),
            cpu_us: self.wall_us.load(Relaxed),
        }
    }
}
#[derive(Default)]
pub(super) struct DisplayTimings {
    #[cfg(feature = "profiling")]
    calls: [AtomicCall; 3],
    #[cfg(feature = "profiling")]
    max_us: std::sync::atomic::AtomicU64,
    #[cfg(feature = "profiling")]
    entered_us: std::sync::atomic::AtomicU64,
    #[cfg(feature = "profiling")]
    completed_us: std::sync::atomic::AtomicU64,
}
impl DisplayTimings {
    pub fn enter(&self) -> u64 {
        #[cfg(feature = "profiling")]
        {
            let now = clock_us();
            self.entered_us
                .store(now, std::sync::atomic::Ordering::Relaxed);
            now
        }
        #[cfg(not(feature = "profiling"))]
        {
            0
        }
    }
    pub fn measure<T>(&self, call: DisplayCall, operation: impl FnOnce() -> T) -> T {
        #[cfg(feature = "profiling")]
        {
            let start = clock_us();
            let result = operation();
            self.calls[1 + call as usize].add(clock_us().saturating_sub(start));
            result
        }
        #[cfg(not(feature = "profiling"))]
        {
            let _ = call;
            operation()
        }
    }
    pub fn complete(&self, start: u64) {
        #[cfg(feature = "profiling")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            let now = clock_us();
            let wall = now.saturating_sub(start);
            self.calls[0].add(wall);
            self.max_us.fetch_max(wall, Relaxed);
            self.completed_us.store(now, Relaxed);
        }
        #[cfg(not(feature = "profiling"))]
        {
            let _ = start;
        }
    }
    pub fn snapshot(&self) -> super::DisplayTiming {
        #[cfg(feature = "profiling")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            super::DisplayTiming {
                callback: self.calls[0].snapshot(),
                set_framebuffer: self.calls[1].snapshot(),
                wait_framebuffer: self.calls[2].snapshot(),
                max_callback_us: self.max_us.load(Relaxed),
                last_entered_us: self.entered_us.load(Relaxed),
                last_completed_us: self.completed_us.load(Relaxed),
            }
        }
        #[cfg(not(feature = "profiling"))]
        {
            super::DisplayTiming::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "profiling")]
    #[test]
    fn sampled_thread_time_preserves_unavailable_and_regressing_counters() {
        let timings = Timings::default();
        assert!(timings.thread_sample_due(0));
        for now in [1, 999, 99_999] {
            assert!(!timings.thread_sample_due(now));
        }
        assert!(timings.thread_sample_due(100_000));
        assert!(!timings.thread_sample_due(199_999));
        let query = |at, run| ThreadQuery {
            started_us: at,
            ended_us: at + 5,
            run_us: run,
        };
        let probe = |sample| SceneProbe {
            notification: 42,
            target_id: 7,
            extent: [1024, 1024],
            before: PendingNotifications {
                count: 3,
                same_target: 2,
                oldest_age_us: 6000,
            },
            work: super::super::SceneWork::default(),
            entered_us: 110,
            query: sample,
        };
        timings.record_scene_end(
            probe(Some(query(100, Ok(40)))),
            2110,
            Some(query(2115, Ok(90))),
            -77,
            |_| PendingNotifications {
                count: 1,
                same_target: 0,
                oldest_age_us: 8000,
            },
        );
        let measured = timings.snapshot();
        assert_eq!(measured.scene_end_thread.samples, 1);
        assert_eq!(measured.scene_end_thread.wall_us, 2020);
        assert_eq!(measured.scene_end_thread.run_us, 50);
        assert_eq!(measured.scene_end_thread.query_wall_us, 10);
        assert_eq!(
            measured.scene_queue.last_slow.unwrap().thread_wall_us,
            Some(2020)
        );
        let slow = measured.scene_queue.last_slow.unwrap();
        assert_eq!(
            (slow.result, slow.thread_run_us, slow.target_id),
            (-77, Some(50), 7)
        );
        assert_eq!((slow.before.count, slow.after.count), (3, 1));
        timings.record_scene_end(probe(None), 2110, None, -78, |_| {
            PendingNotifications::default()
        });
        assert_eq!(timings.snapshot().scene_end_thread.query_calls, 2);
        assert_eq!(
            timings
                .snapshot()
                .scene_queue
                .last_slow
                .unwrap()
                .thread_run_us,
            None
        );
        timings.record_scene_end(
            probe(Some(query(100, Err(-19)))),
            2110,
            Some(query(2115, Ok(90))),
            -79,
            |_| PendingNotifications::default(),
        );
        let failed = timings.snapshot();
        assert_eq!(failed.scene_end_thread.query_failures, 1);
        assert_eq!(failed.scene_end_thread.last_query_error, Some(-19));
        assert_eq!(failed.scene_queue.last_slow.unwrap().result, -79);
        timings.record_scene_end(
            probe(Some(query(100, Ok(90)))),
            2110,
            Some(query(2115, Ok(40))),
            0,
            |_| PendingNotifications::default(),
        );
        let regressed = timings.snapshot();
        assert_eq!(regressed.scene_end_thread.clock_regressions, 1);
        assert_eq!(regressed.scene_end_thread.samples, 1);
        assert_eq!(regressed.notifications.peak_same_target, 2);
        assert_eq!(regressed.scene_queue.slow_calls, 4);
        timings.record_scene_end(probe(None), 111, None, 0, |_| {
            panic!("fast calls must not rescan the queue")
        });
        assert_eq!(timings.snapshot().scene_queue.slow_calls, 4);
        timings.record_scene_end(
            probe(Some(query(100, Ok(40)))),
            109,
            Some(query(2115, Ok(90))),
            0,
            |_| panic!("invalid wall must not sample a queue"),
        );
        assert_eq!(timings.snapshot().scene_queue.clock_regressions, 1);
        assert_eq!(timings.snapshot().scene_end_thread.clock_regressions, 2);
        assert_eq!(timings.snapshot().scene_end_thread.samples, 1);
        let mut work = super::super::SceneWork::default();
        work.draw(3); // Clear triangle.
        work.draw(6 * 65_535); // Instanced base mesh, expanded by the existing SDK count.
        assert_eq!(
            (work.draw_calls, work.indices, work.triangles),
            (2, 393_213, 131_071)
        );
    }

    #[test]
    fn display_measurement_preserves_errors_without_changing_callback_count() {
        let timing = DisplayTimings::default();
        let entered = timing.enter();
        assert_eq!(timing.measure(DisplayCall::Set, || -41), -41);
        assert_eq!(timing.measure(DisplayCall::Wait, || -42), -42);
        timing.complete(entered);
        let observed = timing.snapshot();
        #[cfg(feature = "profiling")]
        assert_eq!(
            (
                observed.callback.calls,
                observed.set_framebuffer.calls,
                observed.wait_framebuffer.calls
            ),
            (1, 1, 1)
        );
        #[cfg(not(feature = "profiling"))]
        {
            assert_eq!(observed, super::super::DisplayTiming::default());
            assert_eq!(core::mem::size_of_val(&timing), 0);
        }
    }

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
