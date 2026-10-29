//! Profiling observations; none of these values control submission or ownership.

/// CPU execution sampled from the calling thread around EndScene.
/// All fields are zero without `profiling`. Sony defines `runClocks` in microseconds.
/// One pair is sampled at most every 100 ms; unsampled calls remain in the SDK wall counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadRunTiming {
    /// Successful, monotonic pairs of thread queries.
    pub samples: u64,
    /// Wall microseconds covering both queries and the enclosed SDK call.
    pub wall_us: u64,
    /// Calling-thread execution microseconds between the two kernel samples.
    /// Includes query-boundary overhead; this is neither GPU time nor pure SDK CPU time.
    pub run_us: u64,
    /// Thread-information queries, including unsuccessful queries.
    pub query_calls: u64,
    /// Wall microseconds spent querying thread information.
    pub query_wall_us: u64,
    /// Queries returning an SDK error. Submission continues unchanged.
    pub query_failures: u64,
    /// Latest original query error, or `None` when no query has failed.
    pub last_query_error: Option<i32>,
    /// Sampled pairs with backwards execution or wall intervals; excluded above.
    pub clock_regressions: u64,
}

/// Notification state read without polling, retiring leases, or issuing SDK waits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingNotifications {
    /// Actual unmatched scene/transfer notifications, excluding already observed completions.
    pub count: u32,
    /// Unmatched scenes using the sampled scheduling target; zero without a target.
    pub same_target: u32,
    /// Age in microseconds of the oldest unmatched submission, measured since publication.
    pub oldest_age_us: u64,
}

/// Notification publication and first normal-poll observations.
/// Publication tracks ownership: protected transfers may publish before their SDK
/// call, and scenes publish after EndScene returns. It is not a GPU start timestamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationTiming {
    /// Published scene/transfer completion tokens, not GPU completion events.
    pub submissions: u64,
    /// Tokens first observed complete by the ordinary Fence/context poll path.
    pub observations: u64,
    /// Sum of publication-to-first-observation wall microseconds.
    /// This includes execution, dependencies and delayed CPU observation.
    pub observed_age_us: u64,
    /// Largest publication-to-first-observation delay.
    pub max_observed_age_us: u64,
    /// Process-monotonic time of the latest token publication, in microseconds.
    pub last_submit_us: u64,
    /// Notification generation of the latest publication.
    pub last_submit_generation: u32,
    /// Process-monotonic time of the latest first completion observation, in microseconds.
    pub last_observed_us: u64,
    /// Notification generation of the latest first observation.
    pub last_observed_generation: u32,
    /// Current unmatched notifications at the accounting snapshot; `same_target` is zero.
    pub pending: PendingNotifications,
    /// Largest actual unmatched count observed at publication or scene boundaries.
    pub peak_pending: u32,
    /// Largest actual unmatched count for one shared scheduling target.
    pub peak_same_target: u32,
}

/// Submitted command/geometry counts for one scene; not fragment or pixel work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneWork {
    /// Attempted SDK draws, including clear triangles and instanced draws.
    pub draw_calls: u64,
    /// Submitted index count, expanded over instances.
    pub indices: u64,
    /// Submitted triangle-list primitives, including degenerate or culled triangles.
    pub triangles: u64,
}
#[cfg(all(feature = "profiling", any(target_os = "vita", test)))]
impl SceneWork {
    pub(super) fn draw(&mut self, indices: u32) {
        self.draw_calls += 1;
        self.indices += u64::from(indices);
        self.triangles += u64::from(indices / 3);
    }
}

/// One bounded slow-EndScene observation. No additional GPU work is inserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlowSceneEnd {
    /// Notification generation reserved for this scene.
    pub notification: u32,
    /// Opaque SDK scheduling-target address at this call; shared by compatible surfaces.
    /// This is a diagnostic identity, not an owned handle or a permanent identifier.
    pub target_id: u64,
    /// Scheduling-target size class, in pixels.
    pub target_extent: [u32; 2],
    /// Commands and submitted geometry in this scene.
    pub work: SceneWork,
    /// Process-monotonic time immediately before SDK timing, in microseconds.
    pub entered_us: u64,
    /// Process-monotonic time immediately after SDK timing, in microseconds.
    pub exited_us: u64,
    /// Calling-thread execution between successful monotonic kernel samples.
    /// `None` means this call was not sampled, a query failed, or its counter regressed.
    pub thread_run_us: Option<u64>,
    /// Wall interval paired with `thread_run_us`, including the two query boundaries.
    /// Both values are absent together when the pair is unavailable or invalid.
    pub thread_wall_us: Option<u64>,
    /// Whether this scene requested the rate-limited pair of thread queries.
    pub thread_sampled: bool,
    /// First original thread-query error in this pair, when present.
    pub thread_query_error: Option<i32>,
    /// Actual outstanding notifications before this call; excludes the current scene.
    pub before: PendingNotifications,
    /// Actual outstanding notifications after this call; excludes the current scene.
    pub after: PendingNotifications,
    /// Original SDK result. Diagnostic query failures do not replace it.
    pub result: i32,
}

/// Scene-queue observations. `scenes_per_frame` is configuration, not a measured queue limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneQueueTiming {
    /// Configured scenes per shared scheduling target.
    pub scenes_per_frame: u16,
    /// EndScene calls whose measured outer SDK span was at least 1 ms.
    pub slow_calls: u64,
    /// Maximum observed outer EndScene span in microseconds.
    pub max_wall_us: u64,
    /// Backwards outer wall-clock intervals, excluded from duration/slow-call statistics.
    pub clock_regressions: u64,
    /// Latest slow call only; one fixed record bounds storage independently of scene count.
    pub last_slow: Option<SlowSceneEnd>,
}

/// Cumulative display-thread wall measurements, sampled independently of the caller thread.
/// Each counter is atomic; the combined snapshot is not a transactional display event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayTiming {
    /// Callback calls and wall microseconds through flip completion, excluding host wake.
    pub callback: super::NativeCallTiming,
    /// SetFrameBuf calls and wall microseconds.
    pub set_framebuffer: super::NativeCallTiming,
    /// WaitSetFrameBuf calls and wall microseconds; absent when vsync is disabled.
    pub wait_framebuffer: super::NativeCallTiming,
    /// Largest callback duration through flip completion.
    pub max_callback_us: u64,
    /// Process-monotonic time of the latest callback entry, in microseconds.
    pub last_entered_us: u64,
    /// Process-monotonic time of its latest completed flip/error, before wake, in microseconds.
    pub last_completed_us: u64,
}
