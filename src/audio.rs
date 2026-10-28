//! Blocking AudioOut worker with fixed, 64-byte-aligned stereo PCM buffers.
use crate::app::{Error, sdk, time, wake::Wake};
use std::{
    prelude::v1::*,
    sync::{
        Arc,
        atomic::{AtomicI32, AtomicU32, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
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
    wake: Arc<Wake>,
}
/// One native output worker. The callback owns no device handles, and only runs
/// after play acceptance. It must fill the fixed slice without allocation or IO.
pub struct Stream {
    control: Arc<Control>,
    worker: Option<JoinHandle<Resources>>,
    retired: Option<Resources>,
}
impl Stream {
    /// Open a paused main port and start its worker. Construction preallocates
    /// both native PCM blocks. Scheduling denial is reported through `fault`.
    /// Both callbacks run on the output worker and must avoid allocation, file IO
    /// and blocking on other threads.
    pub fn new(
        mut render: impl FnMut(&mut [i16], u64) + Send + 'static,
        fault: impl Fn(Error) + Send + 'static,
    ) -> Result<Self, Error> {
        let control = Arc::new(Control {
            desired: AtomicU32::new(PAUSED),
            acknowledged: AtomicU32::new(PAUSED),
            error: AtomicI32::new(0),
            wake: Wake::new()?,
        });
        let state = control.clone();
        let buffers = Box::new([Block([0; BLOCK_FRAMES * 2]), Block([0; BLOCK_FRAMES * 2])]);
        let (started, ready) = mpsc::sync_channel(1);
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
                if port < 0 {
                    let _ = started.send(Err(Error {
                        operation: "sceAudioOutOpenPort",
                        code: port,
                    }));
                    return resources;
                }
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
                    let _ = started.send(Err(Error {
                        operation: "sceAudioOutSetVolume",
                        code: gain,
                    }));
                    return resources;
                }
                // SAFETY: modifies only this output thread's scheduling priority.
                let priority = unsafe { sdk::sceKernelChangeThreadPriority(0, 0x70) };
                if priority < 0 {
                    fault(Error {
                        operation: "audio realtime scheduling denied",
                        code: priority,
                    });
                }
                if started.send(Ok(())).is_err() {
                    return resources;
                }
                let mut active = false;
                let mut index = 0;
                loop {
                    let desired = state.desired.load(Ordering::Acquire);
                    if desired & KIND != RUNNING {
                        if active {
                            // SAFETY: drain while both retained buffers are still alive.
                            let result = unsafe { sdk::sceAudioOutOutput(port, std::ptr::null()) };
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
                        if desired & KIND == STOP {
                            break;
                        }
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
                    let result =
                        unsafe { sdk::sceAudioOutOutput(port, buffers[index].0.as_ptr().cast()) };
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
                // The callback drops here on the worker; native buffers are returned
                // to the controller so failed release preserves their ownership.
                resources
            })
            .map_err(|e| Error {
                operation: "spawn AudioOut worker",
                code: e.raw_os_error().unwrap_or(-1),
            })?;
        let mut stream = Self {
            control,
            worker: Some(worker),
            retired: None,
        };
        match ready.recv() {
            Ok(Ok(())) => Ok(stream),
            Ok(Err(error)) => {
                let _ = stream.stop();
                Err(error)
            }
            Err(_) => {
                let _ = stream.stop();
                Err(Error {
                    operation: "AudioOut startup",
                    code: -1,
                })
            }
        }
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
            return Err(Error {
                operation: "AudioOut worker ended",
                code: -1,
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
        if self.worker.is_none() && self.retired.is_none() {
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
    /// Stop the worker, then release the native port. A failed release retains the
    /// buffers in this stream; the owner may retry stop without restarting fill.
    pub fn stop(&mut self) -> Result<(), Error> {
        if let Some(worker) = self.worker.take() {
            self.control.desired.store(STOP, Ordering::Release);
            let _ = self.control.wake.signal();
            self.retired = Some(worker.join().map_err(|_| Error {
                operation: "AudioOut worker panicked",
                code: -1,
            })?);
        }
        if let Some(resources) = &mut self.retired {
            resources.close()?;
        }
        self.retired = None;
        Ok(())
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Only the two native PCM blocks/port survive an unrecoverable SDK
            // release failure. Application fill has already been dropped. Freeing
            // memory still borrowed by AudioOut would violate its lifetime.
            if let Some(resources) = self.retired.take() {
                std::mem::forget(resources);
            }
        }
    }
}
