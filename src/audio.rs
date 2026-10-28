//! Blocking AudioOut worker with fixed, 64-byte-aligned stereo PCM buffers.
use crate::app::{Error, sdk, time, wake::Wake};
use std::{
    prelude::v1::*,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// AudioOut main-port sample rate in Hz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Frames per native stereo block; two aligned blocks remain owned until drain.
pub const BLOCK_FRAMES: usize = 512;
const STOP: u32 = 0;
const PAUSED: u32 = 1;
const RUNNING: u32 = 2;
const KIND: u32 = 3;
/// Progress of the latest control request; this does not measure presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    /// The worker is opening its native port; no playback is acknowledged yet.
    Opening,
    /// Play was requested; the worker has not observed this request generation.
    Starting,
    /// The worker observed the latest play request and can invoke its callback.
    Running,
    /// Pause was requested; previously submitted PCM is still retained.
    Draining,
    /// The worker confirmed this pause after all previously submitted PCM drained.
    Paused,
    /// Shutdown has not yet released the worker and native port.
    Stopping,
    /// The worker and native port have both been released.
    Closed,
}
#[repr(align(64))]
struct Block([i16; BLOCK_FRAMES * 2]);
struct Resources {
    port: i32,
    buffers: Option<Box<[Block; 2]>>,
}
impl Resources {
    fn close(&mut self) -> Result<(), Error> {
        if self.port < 0 {
            return Ok(());
        }
        // SAFETY: owned port; NULL blocks until preceding PCM has finished.
        let drain = unsafe { sdk::sceAudioOutOutput(self.port, std::ptr::null()) };
        // SAFETY: no further worker calls exist once Resources is returned.
        let release = unsafe { sdk::sceAudioOutReleasePort(self.port) };
        if release >= 0
            || release as u32 == sdk::SCE_AUDIO_OUT_ERROR_INVALID_PORT
            || release as u32 == sdk::SCE_AUDIO_OUT_ERROR_NOT_OPENED
        {
            self.port = -1;
            if drain < 0 && release >= 0 {
                return Err(Error {
                    operation: "sceAudioOutOutput drain",
                    code: drain,
                });
            }
            return Ok(());
        }
        Error::check("sceAudioOutReleasePort", release)?;
        Ok(())
    }
}
impl Drop for Resources {
    fn drop(&mut self) {
        if self.close().is_err()
            && self.port >= 0
            && let Some(buffers) = self.buffers.take()
        {
            std::mem::forget(buffers);
        }
    }
}
struct Control {
    // Low bits select the state; upper bits identify its request generation.
    desired: AtomicU32,
    acknowledged: AtomicU32,
    error: AtomicI32,
    close_error: AtomicI32,
    close_request: AtomicU32,
    closed: AtomicBool,
    ready: AtomicBool,
    wake: Arc<Wake>,
    completion: Arc<Wake>,
}
/// One native output worker. The callback owns no device handles, and only runs
/// after play acceptance. It must fill the fixed slice without allocation or IO.
pub struct Stream {
    control: Arc<Control>,
    worker: Option<JoinHandle<()>>,
}
impl Stream {
    /// Start a paused output worker and preallocate both native PCM blocks.
    /// Native port opening runs on that worker. Native startup/output failures
    /// and scheduling denial are asynchronous and reported through `fault`.
    /// Both callbacks run on the output worker and must avoid allocation, file IO
    /// and blocking on other threads.
    pub fn new(
        mut render: impl FnMut(&mut [i16], u64) + Send + 'static,
        fault: impl Fn(Error) + Send + 'static,
    ) -> Result<Self, Error> {
        let control = Arc::new(Control {
            desired: AtomicU32::new(PAUSED),
            acknowledged: AtomicU32::new(KIND),
            error: AtomicI32::new(0),
            close_error: AtomicI32::new(0),
            close_request: AtomicU32::new(0),
            closed: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            wake: Wake::new()?,
            completion: Wake::new()?,
        });
        let state = control.clone();
        let buffers = Box::new([Block([0; BLOCK_FRAMES * 2]), Block([0; BLOCK_FRAMES * 2])]);
        let worker = thread::Builder::new()
            .name("audio-output".into())
            .stack_size(128 << 10)
            .spawn(move || {
                // SAFETY: scalar SDK parameters; successful port belongs to this worker.
                let port = unsafe {
                    sdk::sceAudioOutOpenPort(
                        sdk::SCE_AUDIO_OUT_PORT_TYPE_MAIN,
                        BLOCK_FRAMES as i32,
                        SAMPLE_RATE as i32,
                        sdk::SCE_AUDIO_OUT_MODE_STEREO,
                    )
                };
                let mut resources = Resources {
                    port,
                    buffers: Some(buffers),
                };
                let initialized = if port < 0 {
                    let error = Error {
                        operation: "sceAudioOutOpenPort",
                        code: port,
                    };
                    state.error.store(port, Ordering::Release);
                    fault(error);
                    false
                } else {
                    let mut volume = [sdk::SCE_AUDIO_VOLUME_0DB as i32; 2];
                    // SAFETY: owned port; SDK reads exactly the two stereo gain entries.
                    let gain = unsafe {
                        sdk::sceAudioOutSetVolume(
                            port,
                            sdk::SCE_AUDIO_VOLUME_FLAG_L_CH | sdk::SCE_AUDIO_VOLUME_FLAG_R_CH,
                            volume.as_mut_ptr(),
                        )
                    };
                    if gain < 0 {
                        state.error.store(gain, Ordering::Release);
                        fault(Error {
                            operation: "sceAudioOutSetVolume",
                            code: gain,
                        });
                        false
                    } else {
                        true
                    }
                };
                // SAFETY: modifies only this output thread's scheduling priority.
                let priority = unsafe { sdk::sceKernelChangeThreadPriority(0, 0x70) };
                if priority < 0 {
                    fault(Error {
                        operation: "audio realtime scheduling denied",
                        code: priority,
                    });
                }
                state.ready.store(true, Ordering::Release);
                let mut active = false;
                let mut index = 0;
                if initialized {
                    loop {
                        let desired = state.desired.load(Ordering::Acquire);
                        if desired & KIND == STOP {
                            break;
                        }
                        if desired & KIND != RUNNING {
                            if active {
                                // SAFETY: drain while both retained buffers are still alive.
                                let result =
                                    unsafe { sdk::sceAudioOutOutput(port, std::ptr::null()) };
                                if result < 0 {
                                    let error = Error {
                                        operation: "sceAudioOutOutput pause",
                                        code: result,
                                    };
                                    state.error.store(result, Ordering::Release);
                                    fault(error);
                                    break;
                                }
                                active = false;
                            }
                            state.acknowledged.store(desired, Ordering::Release);
                            if let Err(error) = state.wake.wait(None) {
                                state.error.store(error.code, Ordering::Release);
                                fault(error);
                                break;
                            }
                            continue;
                        }
                        active = true;
                        state.acknowledged.store(desired, Ordering::Release);
                        let buffers = resources
                            .buffers
                            .as_mut()
                            .expect("owned native PCM buffers");
                        render(&mut buffers[index].0, time::monotonic_micros());
                        // SAFETY: aligned, complete stereo block; alternating buffers remain
                        // owned until the final explicit drain/release on the control side.
                        let result = unsafe {
                            sdk::sceAudioOutOutput(port, buffers[index].0.as_ptr().cast())
                        };
                        if result < 0 {
                            state.error.store(result, Ordering::Release);
                            fault(Error {
                                operation: "sceAudioOutOutput",
                                code: result,
                            });
                            break;
                        }
                        index ^= 1;
                    }
                }
                // Callback and its application captures retire on this worker
                // before native shutdown. The two SDK-borrowed blocks remain in
                // Resources until the port has really released them.
                drop(render);
                loop {
                    let attempt = state.close_request.load(Ordering::Acquire);
                    let result = resources.close();
                    if let Err(error) = result {
                        state.error.store(error.code, Ordering::Release);
                        fault(error);
                    }
                    if resources.port < 0 {
                        drop(resources);
                        state.closed.store(true, Ordering::Release);
                        let _ = state.completion.signal();
                        break;
                    }
                    state
                        .close_error
                        .store(result.unwrap_err().code, Ordering::Release);
                    let _ = state.completion.signal();
                    // Release failure retains the actual port and both blocks.
                    // Only a new explicit close request retries native cleanup.
                    while state.close_request.load(Ordering::Acquire) == attempt {
                        if let Err(error) = state.wake.wait(None) {
                            state.error.store(error.code, Ordering::Release);
                            fault(error);
                            // Preserve the owner if its event is irrecoverable.
                            std::mem::forget(resources);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| Error {
                operation: "spawn AudioOut worker",
                code: e.raw_os_error().unwrap_or(-1),
            })?;
        Ok(Self {
            control,
            worker: Some(worker),
        })
    }
    fn request(&self, state: u32) -> Result<(), Error> {
        let error = self.control.error.load(Ordering::Acquire);
        if error != 0 {
            return Err(Error {
                operation: "AudioOut worker",
                code: error,
            });
        }
        if self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            let code = self.control.error.load(Ordering::Acquire);
            return Err(Error {
                operation: "AudioOut worker ended",
                code: if code == 0 { -1 } else { code },
            });
        }
        let previous = self.control.desired.load(Ordering::Acquire);
        if previous & KIND == STOP {
            return Err(Error {
                operation: "AudioOut stream closing",
                code: -1,
            });
        }
        if previous & KIND != state {
            self.control.desired.store(
                (previous.wrapping_add(4) & !KIND) | state,
                Ordering::Release,
            );
            self.control.wake.signal()?;
        }
        Ok(())
    }
    /// Request callback processing without waiting for the output worker. A
    /// pending drain completes before the worker observes the new play request.
    /// Serialize control operations on the owning thread and observe `state` for
    /// this generation's acknowledgement. Asynchronous faults use `fault` too.
    pub fn play(&self) -> Result<(), Error> {
        self.request(RUNNING)
    }
    /// Close playback on the worker without waiting for native drain. Both PCM
    /// buffers remain owned until `state` confirms Paused or shutdown completes.
    /// The application must close its own callback-entry gate before this call
    /// when it requires immediate prevention of further fill invocations.
    pub fn pause(&self) -> Result<(), Error> {
        self.request(PAUSED)
    }
    /// Observe the latest request without waiting. A stale Running/Paused
    /// acknowledgement cannot complete a later request of the same kind.
    pub fn state(&self) -> Result<StreamState, Error> {
        if self.control.closed.load(Ordering::Acquire)
            && self.worker.as_ref().is_none_or(JoinHandle::is_finished)
        {
            return Ok(StreamState::Closed);
        }
        let error = self.control.error.load(Ordering::Acquire);
        if error != 0 {
            return Err(Error {
                operation: "AudioOut worker",
                code: error,
            });
        }
        let requested = self.control.desired.load(Ordering::Acquire);
        if requested & KIND == STOP {
            return Ok(StreamState::Stopping);
        }
        if !self.control.ready.load(Ordering::Acquire) {
            return Ok(StreamState::Opening);
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err(Error {
                operation: "AudioOut worker ended",
                code: -1,
            });
        }
        let complete = requested == self.control.acknowledged.load(Ordering::Acquire);
        Ok(match (requested & KIND, complete) {
            (RUNNING, true) => StreamState::Running,
            (RUNNING, false) => StreamState::Starting,
            (_, true) => StreamState::Paused,
            (_, false) => StreamState::Draining,
        })
    }
    /// Request shutdown or explicitly retry a failed native release. Returns
    /// without waiting for output, drain, port release or worker completion.
    /// Retain this Stream and use `poll_stop` before admitting a replacement port.
    pub fn begin_stop(&self) -> Result<(), Error> {
        if self.worker.is_none() {
            return Ok(());
        }
        self.control.close_error.store(0, Ordering::Release);
        self.control.desired.store(STOP, Ordering::Release);
        self.control.close_request.fetch_add(1, Ordering::AcqRel);
        self.control.wake.signal()
    }
    /// Observe shutdown without waiting. True means the actual native port and
    /// buffers are released and the completed worker was joined. On an error,
    /// retain this owner; `begin_stop` explicitly retries failed native release.
    /// Native faults are also delivered through the original `fault` callback.
    pub fn poll_stop(&mut self) -> Result<bool, Error> {
        if self.control.closed.load(Ordering::Acquire)
            && self.worker.as_ref().is_none_or(JoinHandle::is_finished)
        {
            if let Some(worker) = self.worker.take() {
                worker.join().map_err(|_| Error {
                    operation: "AudioOut worker panicked",
                    code: -1,
                })?;
            }
            return Ok(true);
        }
        let error = self.control.close_error.load(Ordering::Acquire);
        if error != 0 {
            return Err(Error {
                operation: "AudioOut native release",
                code: error,
            });
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err(Error {
                operation: "AudioOut worker ended before port release",
                code: -1,
            });
        }
        Ok(false)
    }
    /// Final-owner shutdown. Normal page/session transitions use begin/poll;
    /// this operation may wait for real native drain and port release. A release
    /// failure returns with the worker still owning its port and both blocks.
    pub fn stop(&mut self) -> Result<(), Error> {
        self.begin_stop()?;
        while !self.poll_stop()? {
            self.control
                .completion
                .wait(Some(Duration::from_millis(100)))?;
        }
        Ok(())
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        // Final-owner cleanup may wait. Adapters retain pending Streams across
        // page/session switches. On unrecoverable failure the worker continues
        // owning the port/buffers; dropping its handle never claims release.
        let _ = self.stop();
    }
}
