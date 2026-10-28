use super::{
    Audio, Info, Picture, Poll, Source,
    input::read_at,
    metadata,
    pcm::{self, Packet as PendingAudio},
};
use crate::app::{
    memory::{MemoryBlock, MemoryBudget, MemoryKind, MemoryLayout, MemoryReservation},
    sdk,
};
use std::{
    ffi::{c_char, c_void},
    format,
    io::{BufReader, Seek, SeekFrom},
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    prelude::v1::*,
    ptr,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const INPUT_BYTES: usize = 64 << 10;
const ALLOCATIONS: usize = 256;
const MAX_PCM_BYTES: u32 = 256 << 10;
const STATE_PLAY: i32 = 3;

struct Allocator {
    budget: Arc<dyn MemoryBudget>,
    blocks: Mutex<Vec<MemoryBlock>>,
    failure: Mutex<Option<String>>,
    _slots: Box<dyn MemoryReservation>,
}
impl Allocator {
    fn fail(&self, error: String) {
        if let Ok(mut failure) = self.failure.lock() {
            failure.get_or_insert(error);
        }
    }
    fn allocate(&self, alignment: u32, size: u32, kind: MemoryKind) -> *mut c_void {
        let result = (|| {
            let layout = MemoryLayout::new(kind, alignment, size)?;
            let block = MemoryBlock::allocate(layout, self.budget.as_ref())?;
            let pointer = block.pointer();
            let mut blocks = self
                .blocks
                .lock()
                .map_err(|_| "AvPlayer allocation registry is poisoned")?;
            if blocks.len() == blocks.capacity() {
                return Err("AvPlayer allocation registry exhausted its 256 slots".into());
            }
            blocks.push(block);
            Ok::<_, String>(pointer)
        })();
        match result {
            Ok(pointer) => pointer,
            Err(error) => {
                self.fail(error);
                ptr::null_mut()
            }
        }
    }
    fn release(&self, pointer: *mut c_void) {
        if pointer.is_null() {
            return;
        }
        let result = (|| {
            let mut blocks = self
                .blocks
                .lock()
                .map_err(|_| "AvPlayer allocation registry is poisoned")?;
            let index = blocks
                .iter()
                .position(|block| block.pointer() == pointer)
                .ok_or("AvPlayer returned an unowned allocation to free")?;
            blocks[index].release()?;
            blocks.swap_remove(index);
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            self.fail(error);
        }
    }
    fn contains(&self, pointer: usize, bytes: usize) -> bool {
        self.blocks
            .lock()
            .is_ok_and(|blocks| blocks.iter().any(|block| block.contains(pointer, bytes)))
    }
}

unsafe extern "C" fn allocate(arg: *mut c_void, alignment: u32, size: u32) -> *mut c_void {
    let state = unsafe { &*arg.cast::<Allocator>() };
    catch_unwind(AssertUnwindSafe(|| {
        state.allocate(alignment, size, MemoryKind::Heap)
    }))
    .unwrap_or_else(|_| {
        state.fail("AvPlayer allocator panicked".into());
        ptr::null_mut()
    })
}
unsafe extern "C" fn allocate_frame(arg: *mut c_void, alignment: u32, size: u32) -> *mut c_void {
    let state = unsafe { &*arg.cast::<Allocator>() };
    catch_unwind(AssertUnwindSafe(|| {
        state.allocate(alignment, size, MemoryKind::PhysicalUncached)
    }))
    .unwrap_or_else(|_| {
        state.fail("AvPlayer surface allocator panicked".into());
        ptr::null_mut()
    })
}
unsafe extern "C" fn release(arg: *mut c_void, pointer: *mut c_void) {
    let state = unsafe { &*arg.cast::<Allocator>() };
    if catch_unwind(AssertUnwindSafe(|| state.release(pointer))).is_err() {
        state.fail("AvPlayer deallocator panicked".into());
    }
}

struct Input {
    started: AtomicBool,
    source: Mutex<BufReader<Box<dyn Source>>>,
    length: u64,
    failure: Mutex<Option<String>>,
    _storage: Box<dyn MemoryReservation>,
}
unsafe extern "C" fn event(arg: *mut c_void, id: i32, _: i32, _: *mut c_void) {
    let input = unsafe { &*arg.cast::<Input>() };
    // SCE_AVPLAYER_STATE_PLAY confirms asynchronous stream setup is complete.
    if id == STATE_PLAY {
        input.started.store(true, Ordering::Release);
    }
}
unsafe extern "C" fn open_file(_: *mut c_void, _: *const c_char) -> i32 {
    0
}
unsafe extern "C" fn close_file(_: *mut c_void) -> i32 {
    0
}
unsafe extern "C" fn file_size(arg: *mut c_void) -> u64 {
    unsafe { &*arg.cast::<Input>() }.length
}
unsafe extern "C" fn read_file(
    arg: *mut c_void,
    buffer: *mut u8,
    position: u64,
    length: u32,
) -> i32 {
    let input = unsafe { &*arg.cast::<Input>() };
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<usize, String> {
        if buffer.is_null() || length > i32::MAX as u32 {
            return Err("AvPlayer requested an invalid file buffer".into());
        }
        let mut source = input
            .source
            .lock()
            .map_err(|_| "AvPlayer source reader is poisoned")?;
        let output = unsafe { std::slice::from_raw_parts_mut(buffer, length as usize) };
        // An accepted SDK request finishes even when the transport stop flag is
        // set. Close fences these callbacks before Input and its stream drop.
        read_at(&mut *source, position, output).map_err(|error| {
            format!(
                "AvPlayer source read at {position}, {} bytes: {error}",
                output.len()
            )
        })
    }))
    .unwrap_or_else(|_| Err("AvPlayer source reader panicked".into()));
    match result {
        Ok(count) => count as i32,
        Err(error) => {
            if let Ok(mut failure) = input.failure.lock() {
                failure.get_or_insert(error);
            }
            -1
        }
    }
}

/// A synchronous frame-copy operation. Its SDK storage can only be consumed
/// inside `poll_video`'s callback, before another native call can recycle it.
pub struct VideoCopy<'a> {
    source: &'a [u8],
    stride: usize,
    rows: usize,
    info: &'a Info,
    pts: f64,
}
impl VideoCopy<'_> {
    /// Relative source timestamp of this frame, in seconds.
    pub fn pts(&self) -> f64 {
        self.pts
    }
    /// Copy the visible picture to exact-size, caller-owned NV12 planes.
    /// Large contiguous copies use synchronous DMA, including cache visibility.
    pub fn copy_into(&self, y: &mut [u8], uv: &mut [u8]) -> Result<Picture, String> {
        let width = self.info.width as usize;
        let height = self.info.height as usize;
        if y.len() != width * height || uv.len() != width * height / 2 {
            return Err("NV12 destination does not match visible movie dimensions".into());
        }
        copy_nv12(
            self.source,
            [self.stride, self.rows],
            y,
            uv,
            [width, height],
            copy_bytes,
        )?;
        Ok(Picture { pts: self.pts })
    }
    /// Copy into exclusive sampled-compatible storage, preserving its padded row
    /// strides. Sealing the destination afterwards removes every writable alias.
    pub fn copy_to_native(&self, output: &mut super::Nv12Buffer) -> Result<Picture, String> {
        let layout = output.layout();
        if layout.size != [self.info.width, self.info.height] {
            return Err("native NV12 extent mismatch".into());
        }
        let (y, uv) = output.planes_mut();
        let width = self.info.width as usize;
        if layout.strides == [width as u32; 2] {
            return self.copy_into(y, uv);
        }
        for (dest, source) in y
            .chunks_exact_mut(layout.strides[0] as usize)
            .zip(self.source.chunks_exact(self.stride))
            .take(self.info.height as usize)
        {
            copy_bytes(&mut dest[..width], &source[..width])?;
        }
        for (dest, source) in uv
            .chunks_exact_mut(layout.strides[1] as usize)
            .zip(self.source[self.stride * self.rows..].chunks_exact(self.stride))
            .take(self.info.height as usize / 2)
        {
            copy_bytes(&mut dest[..width], &source[..width])?;
        }
        Ok(Picture { pts: self.pts })
    }
    /// Allocate and initialize visible NV12 planes exactly once. The caller's
    /// pre-reserved output owns these vectors; no redundant zero fill occurs.
    pub fn copy_owned(&self) -> Result<(Picture, [Vec<u8>; 2]), String> {
        let width = self.info.width as usize;
        let height = self.info.height as usize;
        let pixels = width * height;
        let mut y = Vec::<u8>::new();
        let mut uv = Vec::<u8>::new();
        y.try_reserve_exact(pixels).map_err(|e| e.to_string())?;
        uv.try_reserve_exact(pixels / 2)
            .map_err(|e| e.to_string())?;
        copy_nv12(
            self.source,
            [self.stride, self.rows],
            &mut y.spare_capacity_mut()[..pixels],
            &mut uv.spare_capacity_mut()[..pixels / 2],
            [width, height],
            |target, source| unsafe { copy_raw(target.as_mut_ptr().cast::<u8>(), source) },
        )?;
        // All visible elements were initialized by synchronous copies above.
        // Failure returns while both vectors still have length zero.
        unsafe {
            y.set_len(pixels);
            uv.set_len(pixels / 2);
        }
        Ok((Picture { pts: self.pts }, [y, uv]))
    }
}

/// Thread-bound hardware decoder. Native callbacks retain their owned source
/// and budgeted allocations until Stop/Close has fenced every SDK callback.
pub struct Player {
    handle: Option<sdk::SceAvPlayerHandle>,
    memory: Option<Box<Allocator>>,
    input: Option<Box<Input>>,
    budget: Arc<dyn MemoryBudget>,
    stopped: Arc<AtomicBool>,
    info: Info,
    sps: Vec<u8>,
    sps_storage: Option<Box<dyn MemoryReservation>>,
    origin: u64,
    video_duration: f64,
    audio_endpoint: Option<u64>,
    audio_frame_samples: usize,
    audio_position: u64,
    audio_next: Option<u64>,
    pending_audio: Option<PendingAudio>,
    audio_buffer: Option<MemoryBlock>,
    audio_enabled: bool,
    paused: bool,
    seen_video: bool,
    final_picture_received: bool,
    seen_active: bool,
    opened: u64,
    #[cfg(feature = "profiling")]
    profile: super::diagnostics::Profile,
    _thread: PhantomData<Rc<()>>,
}

fn check(operation: &str, code: i32) -> Result<(), String> {
    if code < 0 {
        Err(format!("{operation}: 0x{code:08x}"))
    } else {
        Ok(())
    }
}
fn module() -> Result<(), String> {
    static INITIALIZED: OnceLock<Result<(), i32>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| {
            if unsafe { sdk::sceSysmoduleIsLoaded(sdk::SCE_SYSMODULE_AVPLAYER) } == 0 {
                return Ok(());
            }
            let result = unsafe { sdk::sceSysmoduleLoadModule(sdk::SCE_SYSMODULE_AVPLAYER) };
            if result < 0 { Err(result) } else { Ok(()) }
        })
        .map_err(|code| format!("load AvPlayer module: 0x{code:08x}"))
}

impl Player {
    /// Own the resource stream and initialize this profile's hardware decoder.
    /// Source read-ahead, SPS metadata and native allocation descriptors reserve
    /// bounded storage before allocation. Stream setup waits for the native Play
    /// event before returning, so an immediate seek follows decoder readiness.
    pub fn open(
        mut source: Box<dyn Source>,
        budget: Arc<dyn MemoryBudget>,
        stopped: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        if stopped.load(Ordering::Acquire) {
            return Err("AvPlayer open cancelled".into());
        }
        module()?;
        let length = source.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        // Timing cursors provide their own bounded pages. Read metadata before
        // installing SDK read-ahead, so alternating stts/ctts pages do not each
        // trigger another 64 KiB transport read.
        let metadata = metadata::read(&mut *source, length, &stopped, budget.as_ref())?;
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let input_storage = budget.reserve(crate::memory::MemoryClass::Heap, INPUT_BYTES as u64)?;
        let source = BufReader::with_capacity(INPUT_BYTES, source);
        let slots = budget.reserve(
            crate::memory::MemoryClass::Heap,
            (ALLOCATIONS * size_of::<MemoryBlock>()) as u64,
        )?;
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(ALLOCATIONS)
            .map_err(|e| e.to_string())?;
        let mut memory = Box::new(Allocator {
            budget: budget.clone(),
            blocks: Mutex::new(blocks),
            failure: Mutex::new(None),
            _slots: slots,
        });
        let mut input = Box::new(Input {
            started: AtomicBool::new(false),
            source: Mutex::new(source),
            length,
            failure: Mutex::new(None),
            _storage: input_storage,
        });
        let mut data: sdk::SceAvPlayerInitData = unsafe { std::mem::zeroed() };
        data.memoryReplacement.objectPointer = (&mut *memory as *mut Allocator).cast();
        data.memoryReplacement.allocate = Some(allocate);
        data.memoryReplacement.deallocate = Some(release);
        data.memoryReplacement.allocateTexture = Some(allocate_frame);
        data.memoryReplacement.deallocateTexture = Some(release);
        data.fileReplacement.objectPointer = (&mut *input as *mut Input).cast();
        data.fileReplacement.open = Some(open_file);
        data.fileReplacement.close = Some(close_file);
        data.fileReplacement.readOffset = Some(read_file);
        data.fileReplacement.size = Some(file_size);
        data.eventReplacement.objectPointer = (&mut *input as *mut Input).cast();
        data.eventReplacement.eventCallback = Some(event);
        data.basePriority = 0xa0;
        data.numOutputVideoFrameBuffers = 2;
        data.autoStart = 1;
        let handle = unsafe { sdk::sceAvPlayerInit(&mut data) };
        if handle == 0 || (handle as u32 & 0xffff_0000) == 0x806a_0000 {
            return Err(format!("initialize AvPlayer: 0x{handle:08x}"));
        }
        let mut player = Self {
            handle: Some(handle),
            memory: Some(memory),
            input: Some(input),
            budget,
            stopped,
            video_duration: metadata.info.duration,
            info: metadata.info,
            sps: metadata.sps,
            sps_storage: Some(metadata.sps_storage),
            origin: 0,
            audio_endpoint: None,
            audio_frame_samples: metadata.audio_frame_samples,
            audio_position: 0,
            audio_next: None,
            pending_audio: None,
            audio_buffer: None,
            audio_enabled: true,
            paused: false,
            seen_video: false,
            final_picture_received: false,
            seen_active: false,
            opened: crate::app::time::monotonic_micros(),
            #[cfg(feature = "profiling")]
            profile: Default::default(),
            _thread: PhantomData,
        };
        check("open AvPlayer source", unsafe {
            sdk::sceAvPlayerAddSource(handle, c"resource.mp4".as_ptr())
        })?;
        player.prepare_streams()?;
        Ok(player)
    }
    fn prepare_streams(&mut self) -> Result<(), String> {
        let expected = if self.info.audio_sample_rate.is_some() {
            2
        } else {
            1
        };
        loop {
            self.check()?;
            let (mut video, mut audio) = (false, self.info.audio_sample_rate.is_none());
            let mut overall_end = 0_u64;
            for index in 0..expected {
                let mut stream: sdk::SceAvPlayerStreamInfo = unsafe { std::mem::zeroed() };
                if unsafe { sdk::sceAvPlayerGetStreamInfo(self.handle(), index, &mut stream) } < 0 {
                    break;
                }
                if stream.type_ == sdk::SCE_AVPLAYER_VIDEO {
                    self.origin = stream.startTime;
                    if stream.duration > 0 {
                        self.video_duration = stream.duration as f64 / 1000.;
                    }
                    video = true;
                } else if stream.type_ == sdk::SCE_AVPLAYER_AUDIO {
                    let details = unsafe { stream.details.audio };
                    if !matches!(details.channelCount, 1 | 2) || details.sampleRate == 0 {
                        return Err("AvPlayer returned an unsupported PCM format".into());
                    }
                    self.info.audio_channels = details.channelCount;
                    self.info.audio_sample_rate = Some(details.sampleRate);
                    self.audio_endpoint = (stream.duration > 0)
                        .then(|| stream.startTime.saturating_add(stream.duration));
                    audio = true;
                }
                if stream.duration > 0 {
                    overall_end = overall_end.max(stream.startTime.saturating_add(stream.duration));
                }
            }
            if video && audio && self.input.as_ref().unwrap().started.load(Ordering::Acquire) {
                if overall_end > self.origin {
                    self.info.duration = overall_end.saturating_sub(self.origin) as f64 / 1000.;
                }
                return Ok(());
            }
            if crate::app::time::monotonic_micros().saturating_sub(self.opened) >= 10_000_000 {
                return Err("AvPlayer did not start the MP4 tracks within 10 seconds".into());
            }
            crate::app::time::sleep(Duration::from_millis(2)).map_err(|e| e.to_string())?;
        }
    }
    fn handle(&self) -> sdk::SceAvPlayerHandle {
        self.handle.expect("live AvPlayer handle")
    }
    fn check(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("AvPlayer decode cancelled".into());
        }
        for failure in [
            &self.input.as_ref().unwrap().failure,
            &self.memory.as_ref().unwrap().failure,
        ] {
            if let Some(error) = failure
                .lock()
                .map_err(|_| "AvPlayer callback state is poisoned")?
                .as_ref()
            {
                return Err(error.clone());
            }
        }
        Ok(())
    }
    /// Native stream metadata, with visible dimensions retained from MP4.
    pub fn info(&self) -> &Info {
        &self.info
    }
    /// Original video endpoint, independent of a longer optional audio track.
    pub fn video_duration(&self) -> f64 {
        self.video_duration
    }
    /// Encoded SPS used to preserve matrix, transfer and range metadata.
    pub fn sequence_parameter_set(&self) -> &[u8] {
        &self.sps
    }
    /// Disable output admission and PCM staging for poster restoration. Native
    /// AAC output may still be drained internally to keep AvPlayer progressing.
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        if !enabled {
            self.pending_audio = None;
            self.audio_buffer = None;
        }
    }
    /// Pause or resume the native source clock at a worker queue boundary.
    pub fn set_paused(&mut self, paused: bool) -> Result<(), String> {
        self.check()?;
        if paused == self.paused {
            return Ok(());
        }
        // An inactive decoder may still have its final copied packet waiting
        // for publication. That EOF path requires no native pause/resume pair.
        if paused && unsafe { sdk::sceAvPlayerIsActive(self.handle()) } == 0 {
            return Ok(());
        }
        let result = unsafe {
            if paused {
                sdk::sceAvPlayerPause(self.handle())
            } else {
                sdk::sceAvPlayerResume(self.handle())
            }
        };
        check("set AvPlayer transport pause", result)?;
        #[cfg(feature = "profiling")]
        self.profile
            .pause(paused, crate::app::time::monotonic_micros());
        self.paused = paused;
        Ok(())
    }
    /// Seek with native keyframe preroll. Existing copied frame storage is not
    /// referenced by AvPlayer and remains immutable in its consumers.
    pub fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.check()?;
        if !seconds.is_finite() || seconds < 0. {
            return Err("AvPlayer seek position must be finite and nonnegative".into());
        }
        self.pending_audio = None;
        self.audio_next = None;
        self.audio_position = 0;
        let position = ((seconds * 1000.) as u64).saturating_add(self.origin);
        let result = unsafe { sdk::sceAvPlayerJumpToTime(self.handle(), position) };
        if result < 0 {
            return Err(format!(
                "seek AvPlayer source to {position} ms: 0x{result:08x}"
            ));
        }
        self.seen_active = false;
        self.seen_video = false;
        self.final_picture_received = false;
        self.opened = crate::app::time::monotonic_micros();
        if self.paused {
            self.set_paused(false)?;
        }
        Ok(())
    }
    fn ended(&mut self) -> Result<bool, String> {
        let active = unsafe { sdk::sceAvPlayerIsActive(self.handle()) } != 0;
        self.seen_active |= active;
        if active || self.paused {
            return Ok(false);
        }
        if self.seen_video {
            return Ok(true);
        }
        if self.seen_active {
            return Err("AvPlayer ended before producing a picture".into());
        }
        if crate::app::time::monotonic_micros().saturating_sub(self.opened) >= 10_000_000 {
            return Err("AvPlayer produced no picture within 10 seconds".into());
        }
        Ok(false)
    }
    /// Whether no delayed PCM remains after the audio track's native endpoint.
    pub fn audio_finished(&self) -> bool {
        !self.audio_enabled
            || self.info.audio_sample_rate.is_none()
            || (self.pending_audio.is_none()
                && self
                    .audio_endpoint
                    .is_some_and(|end| self.audio_position.saturating_add(1) >= end))
    }
    /// Copy one picture synchronously in the supplied callback. The callback
    /// allocates destination planes only after a frame is available, using the
    /// caller's existing output reservation. SDK memory cannot escape its scope.
    pub fn poll_video<T>(
        &mut self,
        copy: impl for<'frame> FnOnce(VideoCopy<'frame>) -> Result<T, String>,
    ) -> Result<Poll<T>, String> {
        self.check()?;
        if self.final_picture_received {
            return Ok(Poll::End);
        }
        let width = self.info.width as usize;
        let height = self.info.height as usize;
        let mut raw: sdk::SceAvPlayerFrameInfo = unsafe { std::mem::zeroed() };
        #[cfg(feature = "profiling")]
        let started = crate::app::time::monotonic_micros();
        let ready = unsafe { sdk::sceAvPlayerGetVideoData(self.handle(), &mut raw) } != 0;
        #[cfg(feature = "profiling")]
        {
            self.profile.polls += 1;
            self.profile.get_us += crate::app::time::monotonic_micros().saturating_sub(started);
        }
        if !ready {
            return Ok(if self.ended()? {
                Poll::End
            } else {
                Poll::Pending
            });
        }
        let details = unsafe { raw.details.video };
        let stride = details.width as usize;
        let rows = details.height as usize;
        if stride < width
            || rows < height
            || stride - width >= 16
            || rows - height >= 16
            || stride > 960
            || rows > 544
            || !stride.is_multiple_of(2)
            || !rows.is_multiple_of(2)
        {
            return Err(format!(
                "AvPlayer NV12 storage {}x{} does not cover visible {}x{}",
                stride, rows, width, height
            ));
        }
        let bytes = stride * rows * 3 / 2;
        if raw.pData.is_null()
            || !self
                .memory
                .as_ref()
                .unwrap()
                .contains(raw.pData as usize, bytes)
        {
            return Err("AvPlayer returned an unowned NV12 surface".into());
        }
        let source = unsafe { std::slice::from_raw_parts(raw.pData, bytes) };
        #[cfg(feature = "profiling")]
        self.profile.picture(raw.timeStamp, stride, rows);
        #[cfg(feature = "profiling")]
        let copying = crate::app::time::monotonic_micros();
        let copied = copy(VideoCopy {
            source,
            stride,
            rows,
            info: &self.info,
            pts: raw.timeStamp.saturating_sub(self.origin) as f64 / 1000.,
        })?;
        #[cfg(feature = "profiling")]
        {
            self.profile.copy_us += crate::app::time::monotonic_micros().saturating_sub(copying);
        }
        self.seen_video = true;
        // Composition/edit metadata and SDK frame timestamps use the movie
        // clock. Compare before subtracting the stream's native start origin.
        let final_ms = (self.info.final_picture_time * 1000.).floor() as u64;
        self.final_picture_received = raw.timeStamp >= final_ms;
        Ok(Poll::Ready(copied))
    }
    /// Copy valid AAC samples, excluding native buffer padding.
    /// Initialize at most 1024 owned stereo frames in the caller's pre-reserved
    /// output. Pending performs no float allocation or initialization. Larger
    /// SDK packets retain one budgeted integer copy, bounded at 256 KiB. Short
    /// packets convert directly from SDK storage during this call.
    pub fn poll_audio(&mut self, output: &mut Vec<[f32; 2]>) -> Result<Poll<Audio>, String> {
        self.check()?;
        if self.pending_audio.is_none() {
            let mut raw: sdk::SceAvPlayerFrameInfo = unsafe { std::mem::zeroed() };
            if unsafe { sdk::sceAvPlayerGetAudioData(self.handle(), &mut raw) } == 0 {
                return Ok(if self.audio_finished() || self.ended()? {
                    Poll::End
                } else {
                    Poll::Pending
                });
            }
            if !self.audio_enabled {
                return Ok(Poll::Pending);
            }
            let details = unsafe { raw.details.audio };
            let channels = usize::from(details.channelCount);
            if !matches!(channels, 1 | 2)
                || Some(details.sampleRate) != self.info.audio_sample_rate
                || details.size == 0
                || details.size > MAX_PCM_BYTES
                || !(details.size as usize).is_multiple_of(channels * 2)
            {
                return Err(
                    "AvPlayer returned PCM outside the admitted mono/stereo packet profile".into(),
                );
            }
            // AvPlayer can report stereo-sized storage for a mono AAC frame.
            // The codec frame length determines valid PCM, including 960-sample AAC.
            let bytes =
                pcm::payload_bytes(details.size as usize, channels, self.audio_frame_samples);
            if raw.pData.is_null()
                || !self
                    .memory
                    .as_ref()
                    .unwrap()
                    .contains(raw.pData as usize, bytes)
            {
                return Err("AvPlayer returned unowned PCM storage".into());
            }
            let rate = u64::from(details.sampleRate);
            let first_sample =
                pcm::first_sample(self.audio_next, raw.timeStamp, details.sampleRate);
            let mut packet = PendingAudio {
                bytes,
                offset: 0,
                rate: details.sampleRate,
                channels,
                first_sample,
            };
            let frames = packet.frames();
            // Consume SDK PCM synchronously within this poll; its pointer never escapes.
            let source = unsafe { std::slice::from_raw_parts(raw.pData, bytes) };
            let mut reserve_error = None;
            if frames <= pcm::CHUNK_FRAMES {
                match packet.write(source, self.origin, output) {
                    Ok(pts) => {
                        self.audio_next = Some(first_sample.saturating_add(frames as u64));
                        self.audio_position = raw
                            .timeStamp
                            .saturating_add((frames as u64 * 1000).div_ceil(rate));
                        return Ok(Poll::Ready(Audio {
                            pts,
                            sample_rate: details.sampleRate,
                            frames,
                        }));
                    }
                    Err(error) => reserve_error = Some(error),
                }
            }
            if self
                .audio_buffer
                .as_ref()
                .is_none_or(|block| block.capacity() < bytes)
            {
                self.audio_buffer = Some(MemoryBlock::allocate(
                    MemoryLayout::new(MemoryKind::Heap, 8, details.size)?,
                    self.budget.as_ref(),
                )?);
            }
            unsafe {
                ptr::copy_nonoverlapping(
                    source.as_ptr(),
                    self.audio_buffer.as_ref().unwrap().pointer().cast::<u8>(),
                    bytes,
                );
            }
            self.audio_next = Some(first_sample.saturating_add(frames as u64));
            self.audio_position = raw
                .timeStamp
                .saturating_add((frames as u64 * 1000).div_ceil(rate));
            self.pending_audio = Some(packet);
            if let Some(error) = reserve_error {
                return Err(error);
            }
        }
        let pending = self.pending_audio.as_mut().unwrap();
        let source = unsafe {
            std::slice::from_raw_parts(
                self.audio_buffer.as_ref().unwrap().pointer().cast::<u8>(),
                pending.bytes,
            )
        };
        let pts = pending.write(source, self.origin, output)?;
        let sample_rate = pending.rate;
        if pending.offset == pending.bytes {
            self.pending_audio = None;
        }
        Ok(Poll::Ready(Audio {
            pts,
            sample_rate,
            frames: output.len(),
        }))
    }
    /// Stop the SDK and fence callbacks before releasing stream and allocator
    /// state. A failed Close retains callback storage rather than freeing it.
    pub fn close(mut self) -> Result<(), String> {
        self.close_inner()
    }
    fn close_inner(&mut self) -> Result<(), String> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        #[cfg(feature = "profiling")]
        {
            let now = crate::app::time::monotonic_micros();
            self.profile.finish(now);
            std::eprintln!(
                "[vita-video] lifetime_us={} {:?}",
                now.saturating_sub(self.opened),
                self.profile
            );
        }
        let stop = unsafe { sdk::sceAvPlayerStop(handle) };
        let close = unsafe { sdk::sceAvPlayerClose(handle) };
        if close < 0 {
            if let Some(input) = self.input.take() {
                std::mem::forget(input);
            }
            if let Some(memory) = self.memory.take() {
                std::mem::forget(memory);
            }
            return Err(format!(
                "close AvPlayer failed: 0x{close:08x}; callback-owned storage retained"
            ));
        }
        self.pending_audio = None;
        self.audio_buffer = None;
        self.sps = Vec::new();
        self.sps_storage = None;
        self.input = None;
        self.memory = None;
        if stop < 0 {
            return Err(format!("stop AvPlayer: 0x{stop:08x}; Close completed"));
        }
        Ok(())
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        if let Err(error) = self.close_inner() {
            std::eprintln!("{error}");
        }
    }
}

fn copy_bytes(target: &mut [u8], source: &[u8]) -> Result<(), String> {
    unsafe { copy_raw(target.as_mut_ptr(), source) }
}
unsafe fn copy_raw(target: *mut u8, source: &[u8]) -> Result<(), String> {
    if source.len() >= 32 << 10 {
        check("copy AvPlayer physical surface", unsafe {
            sdk::sceDmacMemcpy(target.cast(), source.as_ptr().cast(), source.len() as u32)
        })
    } else {
        unsafe {
            ptr::copy_nonoverlapping(source.as_ptr(), target, source.len());
        }
        Ok(())
    }
}
fn copy_nv12<T>(
    source: &[u8],
    storage: [usize; 2],
    y: &mut [T],
    uv: &mut [T],
    visible: [usize; 2],
    mut copy: impl FnMut(&mut [T], &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let [stride, rows] = storage;
    let [width, height] = visible;
    let chroma = stride * rows;
    if stride == width {
        copy(y, &source[..y.len()])?;
        copy(uv, &source[chroma..chroma + uv.len()])?;
    } else {
        for row in 0..height {
            copy(
                &mut y[row * width..(row + 1) * width],
                &source[row * stride..row * stride + width],
            )?;
        }
        for row in 0..height / 2 {
            copy(
                &mut uv[row * width..(row + 1) * width],
                &source[chroma + row * stride..chroma + row * stride + width],
            )?;
        }
    }
    Ok(())
}
