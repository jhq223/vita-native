//! The production AudioOut worker runs against a gated in-memory SDK. The gates
//! hold native output/drain while its owner proves that control stays responsive.
#![allow(non_snake_case, dead_code)]
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicI32, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
mod app {
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
    pub mod time {
        pub fn monotonic_micros() -> u64 {
            0
        }
    }
    pub mod wake {
        use super::Error;
        use std::{
            sync::{Arc, Condvar, Mutex},
            time::Duration,
        };
        pub struct Wake(Mutex<bool>, Condvar);
        impl Wake {
            pub fn new() -> Result<Arc<Self>, Error> {
                Ok(Arc::new(Self(Mutex::new(false), Condvar::new())))
            }
            pub fn signal(&self) -> Result<(), Error> {
                *self.0.lock().unwrap() = true;
                self.1.notify_one();
                Ok(())
            }
            pub fn wait(&self, timeout: Option<Duration>) -> Result<(), Error> {
                let mut pending = self.0.lock().unwrap();
                if let Some(timeout) = timeout {
                    pending = self
                        .1
                        .wait_timeout_while(pending, timeout, |v| !*v)
                        .unwrap()
                        .0;
                } else {
                    pending = self.1.wait_while(pending, |v| !*v).unwrap();
                }
                *pending = false;
                Ok(())
            }
        }
    }
    pub mod sdk {
        use super::super::*;
        pub const SCE_AUDIO_OUT_PORT_TYPE_MAIN: i32 = 0;
        pub const SCE_AUDIO_OUT_MODE_STEREO: i32 = 0;
        pub const SCE_AUDIO_VOLUME_0DB: u32 = 0;
        pub const SCE_AUDIO_VOLUME_FLAG_L_CH: i32 = 1;
        pub const SCE_AUDIO_VOLUME_FLAG_R_CH: i32 = 2;
        pub const SCE_AUDIO_OUT_ERROR_INVALID_PORT: u32 = 0x80000001;
        pub const SCE_AUDIO_OUT_ERROR_NOT_OPENED: u32 = 0x80000002;
        pub unsafe fn sceAudioOutOpenPort(_: i32, _: i32, _: i32, _: i32) -> i32 {
            1
        }
        pub unsafe fn sceAudioOutSetVolume(_: i32, _: i32, _: *mut i32) -> i32 {
            0
        }
        pub unsafe fn sceKernelChangeThreadPriority(_: i32, _: i32) -> i32 {
            0
        }
        pub unsafe fn sceAudioOutOutput(_: i32, samples: *const std::ffi::c_void) -> i32 {
            let (state, ready) = native();
            let mut state = state.lock().unwrap();
            if samples.is_null() {
                state.drains += 1;
                ready.notify_all();
                state = ready.wait_while(state, |s| s.drain_permits == 0).unwrap();
                state.drain_permits -= 1;
                0
            } else {
                state.outputs += 1;
                ready.notify_all();
                state = ready.wait_while(state, |s| s.output_permits == 0).unwrap();
                state.output_permits -= 1;
                state.output_error
            }
        }
        pub unsafe fn sceAudioOutReleasePort(_: i32) -> i32 {
            native().0.lock().unwrap().releases += 1;
            0
        }
    }
}
#[path = "../src/audio.rs"]
mod audio;
#[derive(Default)]
struct Native {
    outputs: usize,
    drains: usize,
    releases: usize,
    output_permits: usize,
    drain_permits: usize,
    output_error: i32,
}
fn native() -> &'static (Mutex<Native>, Condvar) {
    static NATIVE: OnceLock<(Mutex<Native>, Condvar)> = OnceLock::new();
    NATIVE.get_or_init(|| (Mutex::new(Native::default()), Condvar::new()))
}
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < until, "native mock did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn permit(outputs: usize, drains: usize) {
    let (state, ready) = native();
    let mut state = state.lock().unwrap();
    state.output_permits += outputs;
    state.drain_permits += drains;
    ready.notify_all();
}
enum Command {
    Play,
    Pause,
    State,
    Stop,
}
#[test]
fn pause_and_resume_return_while_native_output_and_drain_are_blocked_and_preserve_request_generation()
 {
    *native().0.lock().unwrap() = Native::default();
    let renders = Arc::new(AtomicUsize::new(0));
    let rendered = renders.clone();
    let fault = Arc::new(AtomicI32::new(0));
    let faults = fault.clone();
    let (commands, input) = mpsc::channel();
    let (reply, output) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let mut stream = audio::Stream::new(
            move |pcm, _| {
                pcm.fill(7);
                rendered.fetch_add(1, Ordering::Release);
            },
            move |e| {
                faults.store(e.code, Ordering::Release);
            },
        )
        .unwrap();
        for command in input {
            let done = matches!(command, Command::Stop);
            let result = match command {
                Command::Play => stream.play(),
                Command::Pause => stream.pause(),
                Command::State => Ok(()),
                Command::Stop => stream.stop(),
            };
            reply.send((result, stream.state())).unwrap();
            if done {
                break;
            }
        }
    });
    let request = |command| {
        commands.send(command).unwrap();
        output
            .recv_timeout(Duration::from_secs(1))
            .expect("control waited for blocked native IO")
    };
    assert!(request(Command::Play).0.is_ok());
    wait(|| renders.load(Ordering::Acquire) == 1);
    let pause = request(Command::Pause);
    assert!(pause.0.is_ok());
    assert_eq!(pause.1.unwrap(), audio::StreamState::Draining);
    assert_eq!(native().0.lock().unwrap().drains, 0);
    permit(1, 0);
    wait(|| native().0.lock().unwrap().drains == 1);
    let resume = request(Command::Play);
    assert!(resume.0.is_ok());
    assert_eq!(
        resume.1.unwrap(),
        audio::StreamState::Starting,
        "old Running ack cannot complete the newer resume"
    );
    assert_eq!(
        renders.load(Ordering::Acquire),
        1,
        "PCM stays owned through blocked drain"
    );
    permit(0, 1);
    wait(|| renders.load(Ordering::Acquire) == 2);
    assert_eq!(
        request(Command::State).1.unwrap(),
        audio::StreamState::Running
    );
    assert!(request(Command::Pause).0.is_ok());
    permit(1, 1);
    wait(|| request(Command::State).1 == Ok(audio::StreamState::Paused));
    assert_eq!(
        native().0.lock().unwrap().releases,
        0,
        "pause retains the port"
    );
    permit(0, 1);
    assert_eq!(
        request(Command::Stop),
        (Ok(()), Ok(audio::StreamState::Closed))
    );
    owner.join().unwrap();
    assert_eq!(native().0.lock().unwrap().releases, 1);
    assert_eq!(fault.load(Ordering::Acquire), 0);

    // Native failure remains observable with its original code after the worker
    // exits, and closure still owns and drains the same native port.
    let faults = fault.clone();
    let mut stream = audio::Stream::new(
        |_, _| {},
        move |e| {
            faults.store(e.code, Ordering::Release);
        },
    )
    .unwrap();
    native().0.lock().unwrap().output_error = -55;
    stream.play().unwrap();
    permit(1, 0);
    wait(|| fault.load(Ordering::Acquire) == -55);
    assert_eq!(stream.state().unwrap_err().code, -55);
    assert_eq!(stream.pause().unwrap_err().code, -55);
    permit(0, 1);
    stream.stop().unwrap();
    assert_eq!(stream.state().unwrap(), audio::StreamState::Closed);
}
