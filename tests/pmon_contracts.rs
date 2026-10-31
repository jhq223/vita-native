//! The production PMON owner runs against a recording SDK. Device tests still
//! establish live counter progress, sleep exclusion, migration and the divider.
#![allow(non_snake_case, dead_code)]
#![cfg(feature = "profiling")]

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicI32, Ordering},
        mpsc,
    },
};

mod app {
    pub(crate) use crate::sdk;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Error {
        pub operation: &'static str,
        pub code: i32,
    }
    impl Error {
        pub fn check(operation: &'static str, code: i32) -> Result<i32, Self> {
            if code < 0 {
                Err(Self { operation, code })
            } else {
                Ok(code)
            }
        }
    }
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}: {:#010x}", self.operation, self.code)
        }
    }
}

#[path = "../src/app/time/pmon.rs"]
mod pmon;
use pmon::Pmon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ThreadId,
    Query,
    Load,
    Unload,
    Reset,
    Start,
    Read,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Call(Op, i32, u32);

#[derive(Default)]
struct Thread {
    running: bool,
    ticks: u32,
}

#[derive(Default)]
struct State {
    loaded: bool,
    calls: Vec<Call>,
    threads: HashMap<i32, Thread>,
    failure: Option<(Op, i32)>,
}

static SERIAL: Mutex<()> = Mutex::new(());
static STATE: Mutex<Option<State>> = Mutex::new(None);
static NEXT_THREAD: AtomicI32 = AtomicI32::new(1);
std::thread_local! {
    static THREAD_ID: Cell<i32> = Cell::new(NEXT_THREAD.fetch_add(1, Ordering::Relaxed));
}

fn record(op: Op, thread: i32, counter: u32, run: impl FnOnce(&mut State) -> i32) -> i32 {
    let mut slot = STATE.lock().unwrap();
    let state = slot.as_mut().unwrap();
    state.calls.push(Call(op, thread, counter));
    if let Some((expected, code)) = state.failure
        && expected == op
    {
        state.failure = None;
        return code;
    }
    run(state)
}

mod sdk {
    use super::*;
    pub const SCE_PERF_ARM_PMON_THREAD_ID_SELF: u32 = 0;
    pub const SCE_SYSMODULE_PERF: u32 = 5;
    pub const SCE_SYSMODULE_ERROR_UNLOADED: u32 = 0x805a_1001;

    pub unsafe fn sceKernelGetThreadId() -> i32 {
        record(Op::ThreadId, 0, 0, |_| THREAD_ID.get())
    }
    pub unsafe fn sceSysmoduleIsLoaded(module: u32) -> i32 {
        assert_eq!(module, SCE_SYSMODULE_PERF);
        record(Op::Query, 0, 0, |state| {
            if state.loaded {
                0
            } else {
                SCE_SYSMODULE_ERROR_UNLOADED as i32
            }
        })
    }
    pub unsafe fn sceSysmoduleLoadModule(module: u32) -> i32 {
        assert_eq!(module, SCE_SYSMODULE_PERF);
        record(Op::Load, 0, 0, |state| {
            assert!(!state.loaded);
            state.loaded = true;
            0
        })
    }
    pub unsafe fn sceSysmoduleUnloadModule(module: u32) -> i32 {
        assert_eq!(module, SCE_SYSMODULE_PERF);
        record(Op::Unload, 0, 0, |state| {
            assert!(state.loaded);
            assert!(state.threads.values().all(|thread| !thread.running));
            state.loaded = false;
            0
        })
    }
    pub unsafe fn scePerfArmPmonReset(thread: i32) -> i32 {
        assert_eq!(thread, 0);
        record(Op::Reset, THREAD_ID.get(), 0, |state| {
            assert!(state.loaded);
            let thread = state.threads.entry(THREAD_ID.get()).or_default();
            assert!(!thread.running);
            thread.ticks = 0;
            0
        })
    }
    pub unsafe fn scePerfArmPmonStart(thread: i32) -> i32 {
        assert_eq!(thread, 0);
        record(Op::Start, THREAD_ID.get(), 0, |state| {
            let thread = state.threads.entry(THREAD_ID.get()).or_default();
            assert!(!thread.running);
            thread.running = true;
            0
        })
    }
    pub unsafe fn scePerfArmPmonStop(thread: i32) -> i32 {
        assert_eq!(thread, 0);
        record(Op::Stop, THREAD_ID.get(), 0, |state| {
            state.threads.entry(THREAD_ID.get()).or_default().running = false;
            0
        })
    }
    pub unsafe fn scePerfArmPmonGetCounterValue(thread: i32, counter: u32, value: *mut u32) -> i32 {
        assert_eq!(thread, 0);
        assert_eq!(counter, 31);
        record(Op::Read, THREAD_ID.get(), counter, |state| {
            let thread = state.threads.get(&THREAD_ID.get()).unwrap();
            assert!(thread.running);
            // SAFETY: the production owner supplies its exclusive stack output.
            unsafe { *value = thread.ticks };
            0
        })
    }
}

fn setup() -> std::sync::MutexGuard<'static, ()> {
    let serial = SERIAL.lock().unwrap();
    *STATE.lock().unwrap() = Some(State::default());
    serial
}

fn calls() -> Vec<Call> {
    STATE.lock().unwrap().as_ref().unwrap().calls.clone()
}

fn fail(op: Op, code: i32) {
    STATE.lock().unwrap().as_mut().unwrap().failure = Some((op, code));
}

#[test]
fn duplicate_start_preserves_running_raw_counter_and_drop_releases_owner() {
    let _serial = setup();
    let owner = Pmon::start().unwrap();
    let thread_id = owner.read().unwrap().thread_id;
    STATE
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .threads
        .get_mut(&thread_id)
        .unwrap()
        .ticks = u32::MAX;
    let before = calls();
    assert_eq!(
        Pmon::start().err().unwrap().operation,
        "PMON thread already owned"
    );
    assert_eq!(calls(), before);
    assert_eq!(owner.read().unwrap().raw_cycle_ticks, u32::MAX);
    STATE
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .threads
        .get_mut(&thread_id)
        .unwrap()
        .ticks = 3;
    let wrapped = owner.read().unwrap();
    assert_eq!(wrapped.thread_id, thread_id);
    assert_eq!(wrapped.raw_cycle_ticks, 3);
    assert_eq!(wrapped.raw_cycle_ticks.wrapping_sub(u32::MAX), 4);
    drop(owner);
    assert_eq!(calls().iter().filter(|call| call.0 == Op::Reset).count(), 1);
    assert_eq!(calls().iter().filter(|call| call.0 == Op::Stop).count(), 1);
    assert!(!STATE.lock().unwrap().as_ref().unwrap().loaded);
    Pmon::start().unwrap().stop().unwrap();
}

#[test]
fn two_threads_share_module_until_last_counter_has_stopped() {
    let _serial = setup();
    let mut first = Pmon::start().unwrap();
    let first_id = first.read().unwrap().thread_id;
    let (opened, wait_opened) = mpsc::channel();
    let (release, wait_release) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut second = Pmon::start().unwrap();
        opened.send(second.read().unwrap().thread_id).unwrap();
        wait_release.recv().unwrap();
        second.stop().unwrap();
    });
    let second_id = wait_opened.recv().unwrap();
    assert_ne!(first_id, second_id);
    first.stop().unwrap();
    assert!(!calls().iter().any(|call| call.0 == Op::Unload));
    release.send(()).unwrap();
    worker.join().unwrap();
    let calls = calls();
    assert_eq!(calls.iter().filter(|call| call.0 == Op::Query).count(), 1);
    assert_eq!(calls.iter().filter(|call| call.0 == Op::Load).count(), 1);
    assert_eq!(calls.iter().filter(|call| call.0 == Op::Unload).count(), 1);
    assert_eq!(calls.last(), Some(&Call(Op::Unload, 0, 0)));
}

#[test]
fn externally_loaded_module_is_retained() {
    let _serial = setup();
    STATE.lock().unwrap().as_mut().unwrap().loaded = true;
    let mut owner = Pmon::start().unwrap();
    owner.stop().unwrap();
    assert!(
        !calls()
            .iter()
            .any(|call| matches!(call.0, Op::Load | Op::Unload))
    );
    assert!(STATE.lock().unwrap().as_ref().unwrap().loaded);
}

#[test]
fn failed_stop_retains_running_owner_and_original_error_until_retry() {
    let _serial = setup();
    let mut owner = Pmon::start().unwrap();
    fail(Op::Stop, -101);
    let error = owner.stop().unwrap_err();
    assert_eq!((error.operation, error.code), ("scePerfArmPmonStop", -101));
    assert!(owner.read().is_ok());
    assert!(Pmon::start().is_err());
    assert!(!calls().iter().any(|call| call.0 == Op::Unload));
    owner.stop().unwrap();
    let before = calls();
    assert_eq!(owner.read().unwrap_err().operation, "PMON owner is stopped");
    owner.stop().unwrap();
    assert_eq!(calls(), before);
    Pmon::start().unwrap().stop().unwrap();
}

#[test]
fn unload_failure_retries_only_module_release_and_allows_new_thread_owner() {
    let _serial = setup();
    let mut stopped = Pmon::start().unwrap();
    fail(Op::Unload, -102);
    let error = stopped.stop().unwrap_err();
    assert_eq!(
        (error.operation, error.code),
        ("sceSysmoduleUnloadModule PERF", -102)
    );
    assert!(stopped.read().is_err());
    let mut replacement = Pmon::start().unwrap();
    stopped.stop().unwrap();
    assert!(replacement.read().is_ok());
    replacement.stop().unwrap();
    assert_eq!(calls().iter().filter(|call| call.0 == Op::Load).count(), 1);
    assert_eq!(
        calls().iter().filter(|call| call.0 == Op::Unload).count(),
        2
    );
}

#[test]
fn initialization_and_read_failures_preserve_error_codes_and_release_reservations() {
    let _serial = setup();
    for (op, expected) in [
        (Op::ThreadId, "sceKernelGetThreadId"),
        (Op::Query, "sceSysmoduleIsLoaded PERF"),
        (Op::Load, "sceSysmoduleLoadModule PERF"),
        (Op::Reset, "scePerfArmPmonReset"),
        (Op::Start, "scePerfArmPmonStart"),
    ] {
        fail(op, -103);
        let error = Pmon::start().err().unwrap();
        assert_eq!((error.operation, error.code), (expected, -103));
        let mut retry = Pmon::start().unwrap();
        retry.stop().unwrap();
        assert!(!STATE.lock().unwrap().as_ref().unwrap().loaded);
    }
    let mut owner = Pmon::start().unwrap();
    fail(Op::Read, -104);
    let error = owner.read().unwrap_err();
    assert_eq!(
        (error.operation, error.code),
        ("scePerfArmPmonGetCounterValue", -104)
    );
    assert!(owner.read().is_ok());
    owner.stop().unwrap();
}

#[test]
fn owner_is_neither_send_nor_sync() {
    trait AmbiguousIfSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    let _ = <Pmon as AmbiguousIfSend<_>>::check;
    trait AmbiguousIfSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    let _ = <Pmon as AmbiguousIfSync<_>>::check;
}
