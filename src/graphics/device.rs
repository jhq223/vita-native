//! Single-threaded immediate context and bounded completion/display owners.
use super::types::Result;
use super::{
    memory::{Allocation, Arena, Block, Fault, Kind, sdk},
    resources::Framebuffer,
    *,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::c_void,
    mem::ManuallyDrop,
    prelude::v1::*,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicI32, AtomicU32, Ordering},
    },
};
use vitasdk_sys::*;
pub(super) const SLOTS: usize = 256;
const DISPLAY_PENDING: u32 = 2;
#[derive(Default)]
struct DisplayState {
    completed: AtomicU32,
    flips: AtomicU32,
    error: AtomicI32,
}
#[repr(C)]
struct DisplayData {
    base: *mut c_void,
    pitch: u32,
    serial: u32,
    vsync: u32,
    state: *const DisplayState,
}
unsafe extern "C" fn display_callback(data: *const core::ffi::c_void) {
    let d = unsafe { &*data.cast::<DisplayData>() };
    let state = unsafe { &*d.state };
    let fb = SceDisplayFrameBuf {
        size: core::mem::size_of::<SceDisplayFrameBuf>() as u32,
        base: d.base.cast(),
        pitch: d.pitch,
        pixelformat: 0,
        width: 960,
        height: 544,
    };
    let result = unsafe {
        sceDisplaySetFrameBuf(
            &fb,
            if d.vsync != 0 {
                SCE_DISPLAY_SETBUF_NEXTFRAME
            } else {
                SCE_DISPLAY_SETBUF_IMMEDIATE
            },
        )
    };
    let result = if result >= 0 && d.vsync != 0 {
        unsafe { sceDisplayWaitSetFrameBuf() }
    } else {
        result
    };
    if result < 0 {
        state
            .error
            .compare_exchange(0, result, Ordering::AcqRel, Ordering::Acquire)
            .ok();
    } else {
        state.flips.fetch_add(1, Ordering::Relaxed);
    }
    state.completed.store(d.serial, Ordering::Release);
}
unsafe extern "C" fn host_alloc(_: *mut core::ffi::c_void, size: u32) -> *mut core::ffi::c_void {
    let size = size as usize;
    let layout = std::alloc::Layout::from_size_align(size + 8, 8).expect("GXM host allocation");
    let p = unsafe { std::alloc::alloc(layout) };
    if p.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        p.cast::<usize>().write(size);
        p.add(8).cast()
    }
}
unsafe extern "C" fn host_free(_: *mut core::ffi::c_void, p: *mut core::ffi::c_void) {
    if p.is_null() {
        return;
    }
    let p = unsafe { p.cast::<u8>().sub(8) };
    let size = unsafe { p.cast::<usize>().read() };
    unsafe {
        std::alloc::dealloc(
            p,
            std::alloc::Layout::from_size_align(size + 8, 8).expect("GXM host allocation"),
        );
    }
}
pub(super) struct Native {
    pub context: *mut SceGxmContext,
    pub patcher: *mut SceGxmShaderPatcher,
    host: ManuallyDrop<Vec<u64>>,
    blocks: Vec<Rc<Block>>,
    display: ManuallyDrop<Box<DisplayState>>,
    fault: Fault,
    active: bool,
    initialized: bool,
    closed: bool,
}
impl Native {
    fn new(config: Config, fault: &Fault) -> Result<Self> {
        if crate::graphics_active::ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::Invalid("a graphics device is already alive"));
        }
        let mut n = Self {
            context: core::ptr::null_mut(),
            patcher: core::ptr::null_mut(),
            host: ManuallyDrop::new(std::vec![0;SCE_GXM_MINIMUM_CONTEXT_HOST_MEM_SIZE as usize/8]),
            blocks: Vec::new(),
            display: ManuallyDrop::new(Box::default()),
            fault: fault.clone(),
            active: true,
            initialized: false,
            closed: false,
        };
        let init = SceGxmInitializeParams {
            flags: 0,
            displayQueueMaxPendingCount: DISPLAY_PENDING,
            displayQueueCallback: Some(display_callback),
            displayQueueCallbackDataSize: core::mem::size_of::<DisplayData>() as u32,
            parameterBufferSize: config.parameter_bytes,
        };
        sdk("sceGxmInitialize", unsafe { sceGxmInitialize(&init) })?;
        n.initialized = true;
        for (bytes, kind) in [
            (config.vdm_ring_bytes, Kind::Uncached),
            (config.vertex_ring_bytes, Kind::Uncached),
            (config.fragment_ring_bytes, Kind::Uncached),
            (config.fragment_usse_ring_bytes, Kind::FragmentUsse),
            (config.patcher_buffer_bytes, Kind::Uncached),
            (config.patcher_vertex_usse_bytes, Kind::VertexUsse),
            (config.patcher_fragment_usse_bytes, Kind::FragmentUsse),
        ] {
            n.blocks.push(Block::new(bytes as usize, kind, fault)?);
        }
        let b = &n.blocks;
        let params = SceGxmContextParams {
            hostMem: n.host.as_mut_ptr().cast(),
            hostMemSize: (n.host.len() * 8) as u32,
            vdmRingBufferMem: b[0].base.cast(),
            vdmRingBufferMemSize: config.vdm_ring_bytes,
            vertexRingBufferMem: b[1].base.cast(),
            vertexRingBufferMemSize: config.vertex_ring_bytes,
            fragmentRingBufferMem: b[2].base.cast(),
            fragmentRingBufferMemSize: config.fragment_ring_bytes,
            fragmentUsseRingBufferMem: b[3].base.cast(),
            fragmentUsseRingBufferMemSize: config.fragment_usse_ring_bytes,
            fragmentUsseRingBufferOffset: b[3].offset,
        };
        sdk("sceGxmCreateContext", unsafe {
            sceGxmCreateContext(&params, &mut n.context)
        })?;
        // Fixed pipeline state persists across scenes for this private context.
        unsafe {
            let c = n.context;
            sceGxmSetCullMode(c, SCE_GXM_CULL_NONE);
            sceGxmSetFrontDepthFunc(c, SCE_GXM_DEPTH_FUNC_ALWAYS);
            sceGxmSetBackDepthFunc(c, SCE_GXM_DEPTH_FUNC_ALWAYS);
            sceGxmSetFrontDepthWriteEnable(c, SCE_GXM_DEPTH_WRITE_DISABLED);
            sceGxmSetBackDepthWriteEnable(c, SCE_GXM_DEPTH_WRITE_DISABLED);
            sceGxmSetFrontStencilFunc(
                c,
                SCE_GXM_STENCIL_FUNC_ALWAYS,
                SCE_GXM_STENCIL_OP_KEEP,
                SCE_GXM_STENCIL_OP_KEEP,
                SCE_GXM_STENCIL_OP_KEEP,
                255,
                0,
            );
            sceGxmSetBackStencilFunc(
                c,
                SCE_GXM_STENCIL_FUNC_ALWAYS,
                SCE_GXM_STENCIL_OP_KEEP,
                SCE_GXM_STENCIL_OP_KEEP,
                SCE_GXM_STENCIL_OP_KEEP,
                255,
                0,
            );
        }
        let mut patch: SceGxmShaderPatcherParams = unsafe { core::mem::zeroed() };
        patch.hostAllocCallback = Some(host_alloc);
        patch.hostFreeCallback = Some(host_free);
        patch.bufferMem = b[4].base.cast();
        patch.bufferMemSize = config.patcher_buffer_bytes;
        patch.vertexUsseMem = b[5].base.cast();
        patch.vertexUsseMemSize = config.patcher_vertex_usse_bytes;
        patch.vertexUsseOffset = b[5].offset;
        patch.fragmentUsseMem = b[6].base.cast();
        patch.fragmentUsseMemSize = config.patcher_fragment_usse_bytes;
        patch.fragmentUsseOffset = b[6].offset;
        sdk("sceGxmShaderPatcherCreate", unsafe {
            sceGxmShaderPatcherCreate(&patch, &mut n.patcher)
        })?;
        Ok(n)
    }
    fn close(&mut self) -> Result<()> {
        self.fault.check()?;
        if self.closed {
            return Ok(());
        }
        if !self.context.is_null() {
            self.fault.command("sceGxmDestroyContext", unsafe {
                sceGxmDestroyContext(self.context)
            })?;
            self.context = core::ptr::null_mut();
        }
        if !self.patcher.is_null() {
            self.fault.command("sceGxmShaderPatcherDestroy", unsafe {
                sceGxmShaderPatcherDestroy(self.patcher)
            })?;
            self.patcher = core::ptr::null_mut();
        }
        self.blocks.clear();
        self.fault.check()?;
        if self.initialized {
            self.fault
                .command("sceGxmTerminate", unsafe { sceGxmTerminate() })?;
            self.initialized = false;
        }
        unsafe {
            ManuallyDrop::drop(&mut self.host);
            ManuallyDrop::drop(&mut self.display);
        }
        self.closed = true;
        if self.active {
            crate::graphics_active::ACTIVE.store(false, Ordering::Release);
            self.active = false;
        }
        Ok(())
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.close().ok();
    }
}
pub(super) struct Core {
    pub pixels: Arena,
    pub display_pixels: Arena,
    pub buffers: Arena,
    pub native: Native,
    pub fault: Fault,
    pub config: Config,
    pub caps: Capabilities,
    pub rt_bytes: Cell<u64>,
    pub imported_video_bytes: Rc<Cell<u64>>,
}
pub(super) enum Lease {
    Allocation(Allocation),
    Program(Program),
    Target(Framebuffer),
}
impl Lease {
    fn uses(&self, allocation: &Allocation) -> bool {
        matches!(self, Self::Allocation(a) if a.id() == allocation.id())
    }
    fn key(&self) -> (u8, usize) {
        match self {
            Self::Allocation(a) => (0, a.id()),
            Self::Program(p) => (1, Rc::as_ptr(&p.0) as usize),
            Self::Target(f) => (2, Rc::as_ptr(&f.0) as usize),
        }
    }
}
pub(super) type Completion = super::completion::Completion<Lease>;
type Flight = super::completion::Flight<Lease>;
pub(super) struct Recording {
    pub target: Framebuffer,
    pub leases: Vec<Lease>,
    pub slot: usize,
    pub notification: u32,
    pub size: [u32; 2],
}
pub(super) struct State {
    pub recording: Option<Recording>,
    flights: VecDeque<Flight>,
    free: Vec<usize>,
    generation: u32,
    pub last: Option<Rc<Completion>>,
    pub screen: usize,
    pub last_presented: usize,
    pub presents: u32,
    pub scene_submissions: u64,
    pub slot_waits: u64,
    pub display_waits: u64,
}
pub(super) struct BoundProgram {
    pub program: Program,
    pub variants: [usize; 2],
}
pub(super) struct Clear {
    pub program: Program,
    pub vertices: VertexBuffer,
    pub color: Uniform,
}
pub(super) struct Inner {
    pub timing: super::timing::Timings,
    pub core: Rc<Core>,
    pub state: RefCell<State>,
    pub screens: RefCell<Vec<Framebuffer>>,
    pub targets: RefCell<super::render_targets::Pool<super::render_targets::RenderTarget>>,
    #[cfg(feature = "video")]
    pub video_imports: RefCell<Vec<std::rc::Weak<super::video::Import>>>,
    pub vsync: Cell<bool>,
    pub clear: RefCell<Option<Clear>>,
    pub bound_program: RefCell<Option<BoundProgram>>,
    pub bindings: RefCell<super::bindings::Bindings>,
    pub display_detached: Cell<bool>,
    pub identity: RefCell<Option<Allocation>>,
}
/// Thread-bound device. Resource handles own the device independently of command recording.
#[derive(Clone)]
pub struct Context(pub(super) Rc<Inner>);
/// Fragment/transfer completion. It retains original native owners even when dropped by callers.
pub struct Fence {
    token: Rc<Completion>,
    owner: Rc<Core>,
}
impl Fence {
    /// One nonblocking notification observation; releases the submission's leases only on completion.
    pub fn poll(&self) -> Result<bool> {
        self.owner.fault.check()?;
        let complete = self.token.poll();
        self.owner.fault.check()?;
        Ok(complete)
    }
    /// Wait up to the deadline while observing the original notification generation.
    pub fn wait(&self, timeout: std::time::Duration) -> Result<bool> {
        let start = std::time::Instant::now();
        loop {
            if self.poll()? {
                return Ok(true);
            }
            if start.elapsed() >= timeout {
                return Ok(false);
            }
            unsafe {
                sceKernelDelayThread(100);
            }
        }
    }
}
impl Context {
    /// Create a GXM device and three process-sized scanout buffers.
    pub fn new(vsync: bool, clear: ClearProgram<'_>) -> Result<Self> {
        Self::with_config(Config::default(), vsync, clear)
    }
    /// Select explicit infrastructure and render-target scheduling budgets.
    pub fn with_config(config: Config, vsync: bool, clear: ClearProgram<'_>) -> Result<Self> {
        // Sony gxm/defs.h: SCE_GXM_MAX_SCENES_PER_RENDERTARGET (absent from VitaSDK bindings).
        if config.scenes_per_frame == 0
            || config.scenes_per_frame > 8
            || [
                config.parameter_bytes,
                config.vdm_ring_bytes,
                config.vertex_ring_bytes,
                config.fragment_ring_bytes,
                config.fragment_usse_ring_bytes,
                config.patcher_buffer_bytes,
                config.patcher_vertex_usse_bytes,
                config.patcher_fragment_usse_bytes,
            ]
            .contains(&0)
        {
            return Err(Error::Invalid("GXM configuration"));
        }
        let fault = Fault::default();
        let native = Native::new(config, &fault)?;
        let core = Rc::new(Core {
            native,
            pixels: Arena::new(
                Kind::Cdram,
                &fault,
                config.pixel_arena_bytes,
                config.pixel_retained_bytes,
            ),
            display_pixels: Arena::new(Kind::Cdram, &fault, 8 * 1024 * 1024, 0),
            buffers: Arena::new(
                Kind::Uncached,
                &fault,
                config.buffer_arena_bytes,
                config.buffer_retained_bytes,
            ),
            fault,
            config,
            caps: Capabilities {
                max_texture_size: 4096,
                max_texture_units: 16,
                max_vertex_attributes: 16,
                bc: true,
                bgra: true,
                half_float: true,
                indices_u32: true,
                shader_binary: true,
                completion: CompletionMode::FragmentNotification,
            },
            rt_bytes: Cell::new(0),
            imported_video_bytes: Rc::new(Cell::new(0)),
        });
        let context = Self(Rc::new(Inner {
            timing: super::timing::Timings::default(),
            core,
            state: RefCell::new(State {
                recording: None,
                flights: VecDeque::with_capacity(SLOTS),
                free: (0..SLOTS).rev().collect(),
                generation: 0,
                last: None,
                screen: 1,
                last_presented: 0,
                presents: 0,
                scene_submissions: 0,
                slot_waits: 0,
                display_waits: 0,
            }),
            screens: RefCell::new(Vec::new()),
            #[cfg(feature = "video")]
            video_imports: RefCell::new(Vec::new()),
            targets: RefCell::new(super::render_targets::Pool::new(
                config
                    .render_target_retained_bytes
                    .min(config.render_target_bytes),
            )),
            vsync: Cell::new(vsync),
            clear: RefCell::new(None),
            bound_program: RefCell::new(None),
            bindings: RefCell::new(super::bindings::Bindings::default()),
            display_detached: Cell::new(false),
            identity: RefCell::new(None),
        }));
        for _ in 0..3 {
            let t = context.create_display_texture([960, 544])?;
            unsafe {
                core::ptr::write_bytes(t.0.allocation.ptr().cast::<u8>(), 0, t.0.allocation.len());
            }
            context
                .0
                .screens
                .borrow_mut()
                .push(context.create_framebuffer(&t, super::display_sync::SurfaceKind::Display)?);
        }
        let screens = context.0.screens.borrow();
        let t = &screens[0].0.texture;
        let fb = SceDisplayFrameBuf {
            size: core::mem::size_of::<SceDisplayFrameBuf>() as u32,
            base: t.0.allocation.ptr().cast(),
            pitch: (t.0.layout.row_stride_bytes / 4) as u32,
            pixelformat: 0,
            width: 960,
            height: 544,
        };
        context.command("sceDisplaySetFrameBuf(initial)", unsafe {
            sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_NEXTFRAME)
        })?;
        context.command("sceDisplayWaitSetFrameBuf(initial)", unsafe {
            sceDisplayWaitSetFrameBuf()
        })?;
        drop(screens);
        context.init_clear(clear)?;
        Ok(context)
    }
    pub(super) fn command(&self, name: &'static str, code: i32) -> Result<()> {
        self.0.core.fault.command(name, code)
    }
    pub(super) fn same(&self, owner: &Rc<Core>) -> Result<()> {
        self.check("device owner")?;
        if !Rc::ptr_eq(owner, &self.0.core) {
            return Err(Error::Invalid("foreign GXM resource"));
        }
        Ok(())
    }
    /// Observe the bounded display callback failure channel and cached native fault.
    pub fn check(&self, _operation: &'static str) -> Result<()> {
        self.0.core.fault.check()?;
        let code = self.0.core.native.display.error.load(Ordering::Acquire);
        if code < 0 {
            return Err(self.0.core.fault.record("display callback", code));
        }
        Ok(())
    }
    /// Return the first unresolved native ownership/completion failure.
    pub fn terminal_fault(&self) -> Option<Arc<DeviceFault>> {
        self.check("fault observation").ok();
        self.0.core.fault.get()
    }
    /// Fixed GXM hardware limits; each GXP's reflected uniform extent is checked separately.
    pub fn capabilities(&self) -> &Capabilities {
        &self.0.core.caps
    }
    /// Process scanout size in physical pixels.
    pub fn output_size(&self) -> [u32; 2] {
        [960, 544]
    }
    /// Whether display callbacks schedule at the next VBLANK.
    pub fn vsync(&self) -> bool {
        self.0.vsync.get()
    }
    /// Select timing for future queue entries; already queued entries retain their timing.
    pub fn set_vsync(&self, enabled: bool) -> Result<()> {
        self.check("vsync")?;
        self.0.vsync.set(enabled);
        Ok(())
    }
    /// Identify this direct libgxm boundary and offline shader ABI.
    pub fn driver_info(&self) -> Result<DriverInfo> {
        self.check("driver identity")?;
        Ok(DriverInfo {
            vendor: "Sony Computer Entertainment".into(),
            renderer: "libgxm / SGX543MP4+".into(),
            version: "GXM device boundary v1".into(),
            shading_language: "offline GXP / Cg sce_vp_psp2 + sce_fp_psp2".into(),
        })
    }
    /// Observe each submitted scene/transfer notification without waiting.
    /// Releases only completed submissions and returns their notification slots.
    /// Call during the normal event-loop completion phase, before admitting new
    /// resource work, including while the rendered image remains unchanged.
    /// Polling one `Fence` observes only that fence's submission.
    pub fn poll(&self) -> Result<()> {
        self.check("completion poll")?;
        self.poll_state(&mut self.0.state.borrow_mut())
    }
    pub(super) fn poll_state(&self, s: &mut State) -> Result<()> {
        super::completion::collect(&mut s.flights, &mut s.free, || self.0.core.fault.check())
    }
    pub(super) fn reserve_slot(&self, s: &mut State) -> Result<(usize, SceGxmNotification)> {
        self.check("notification admission")?;
        self.poll_state(s)?;
        if s.free.is_empty() {
            s.slot_waits += 1;
            let f = &s.flights[0];
            let n = SceGxmNotification {
                address: f.token.address,
                value: f.token.value,
            };
            self.command("sceGxmNotificationWait(capacity)", unsafe {
                self.0
                    .timing
                    .measure(super::timing::Call::NotificationCapacityWait, || {
                        sceGxmNotificationWait(&n)
                    })
            })?;
            self.poll_state(s)?;
        }
        let slot = s.free.pop().expect("retired notification");
        s.generation = s
            .generation
            .checked_add(1)
            .ok_or(Error::Invalid("notification generation exhausted"))?;
        let address = unsafe { sceGxmGetNotificationRegion().add(slot) };
        Ok((
            slot,
            SceGxmNotification {
                address,
                value: s.generation,
            },
        ))
    }
    pub(super) fn admit_staging(&self, bytes: usize) -> Result<()> {
        let limit = self.0.core.config.upload_staging_bytes;
        if bytes as u64 > limit {
            return Err(Error::Budget {
                domain: "native upload staging",
                requested: bytes as u64,
                limit,
            });
        }
        let mut s = self.0.state.borrow_mut();
        self.end_scene(&mut s)?;
        loop {
            self.poll_state(&mut s)?;
            let used: usize = s
                .flights
                .iter()
                .filter(|f| !f.token.done.get())
                .map(|f| f.token.staging)
                .sum();
            if used + bytes <= limit as usize {
                return Ok(());
            }
            let token = &s
                .flights
                .iter()
                .find(|f| f.token.staging > 0)
                .expect("pending staging")
                .token;
            self.command(
                "sceGxmNotificationWait(upload capacity)",
                self.0
                    .timing
                    .measure(super::timing::Call::UploadCapacityWait, || unsafe {
                        sceGxmNotificationWait(&SceGxmNotification {
                            address: token.address,
                            value: token.value,
                        })
                    }),
            )?;
        }
    }
    pub(super) fn await_allocation(&self, allocation: &Allocation) -> Result<()> {
        let mut s = self.0.state.borrow_mut();
        self.check("resource wait")?;
        if s.recording
            .as_ref()
            .is_some_and(|r| r.leases.iter().any(|l| l.uses(allocation)))
        {
            self.end_scene(&mut s)?;
        }
        while allocation.used() {
            self.poll_state(&mut s)?;
            if !allocation.used() {
                break;
            }
            let token = &s
                .flights
                .iter()
                .find(|f| f.token.leases.borrow().iter().any(|l| l.uses(allocation)))
                .ok_or(Error::Invalid("allocation has an external native lease"))?
                .token;
            self.command(
                "sceGxmNotificationWait(resource replacement)",
                self.0
                    .timing
                    .measure(super::timing::Call::ResourceReplacementWait, || unsafe {
                        sceGxmNotificationWait(&SceGxmNotification {
                            address: token.address,
                            value: token.value,
                        })
                    }),
            )?;
        }
        Ok(())
    }
    pub(super) fn publish(
        &self,
        s: &mut State,
        slot: usize,
        n: SceGxmNotification,
        leases: Vec<Lease>,
        staging: usize,
    ) -> Rc<Completion> {
        let token = Rc::new(Completion {
            address: n.address,
            value: n.value,
            done: Cell::new(false),
            leases: RefCell::new(leases),
            staging,
        });
        s.flights.push_back(Flight {
            slot,
            token: token.clone(),
        });
        s.last = Some(token.clone());
        token
    }
    pub(super) fn retain(recording: &mut Recording, lease: Lease) {
        super::leases::retain(&mut recording.leases, lease, Lease::key);
    }
    pub(super) fn end_scene(&self, s: &mut State) -> Result<Option<Rc<Completion>>> {
        let Some(r) = s.recording.take() else {
            return Ok(None);
        };
        // Its generation was reserved at scene begin and cannot be reused before completion.
        let n = SceGxmNotification {
            address: unsafe { sceGxmGetNotificationRegion().add(r.slot) },
            value: r.notification,
        };
        let code = self
            .0
            .timing
            .measure(super::timing::Call::SceneEnd, || unsafe {
                sceGxmEndScene(self.0.core.native.context, core::ptr::null(), &n)
            });
        if let Err(e) = self.command("sceGxmEndScene", code) {
            core::mem::forget(r);
            return Err(e);
        }
        s.scene_submissions += 1;
        Ok(Some(self.publish(s, r.slot, n, r.leases, 0)))
    }
    /// End the current scene and return the latest real completion token.
    pub fn fence(&self) -> Result<Fence> {
        self.check("fence")?;
        let mut s = self.0.state.borrow_mut();
        self.end_scene(&mut s)?;
        let token = s.last.clone().unwrap_or_else(|| {
            Rc::new(Completion {
                address: core::ptr::null_mut(),
                value: 0,
                done: Cell::new(true),
                leases: RefCell::new(Vec::new()),
                staging: 0,
            })
        });
        Ok(Fence {
            token,
            owner: self.0.core.clone(),
        })
    }
    /// End queued scene work; GXM submits at EndScene and needs no GL-style flush.
    pub fn flush(&self) -> Result<()> {
        let _ = self.fence()?;
        Ok(())
    }
    /// Explicit complete drain of fragment processing, transfers, and queued display callbacks.
    pub fn finish(&self) -> Result<()> {
        self.check("drain")?;
        let mut s = self.0.state.borrow_mut();
        self.end_scene(&mut s)?;
        unsafe {
            sceGxmFinish(self.0.core.native.context);
        }
        self.command("sceGxmTransferFinish", unsafe { sceGxmTransferFinish() })?;
        self.command("sceGxmDisplayQueueFinish", unsafe {
            sceGxmDisplayQueueFinish()
        })?;
        self.check("drain completion")?;
        self.poll_state(&mut s)?;
        if !s.flights.is_empty() {
            return Err(self.0.core.fault.record("drain notification", -1));
        }
        Ok(())
    }
    /// Reset session work after all submitted GPU work and display callbacks have retired.
    pub fn reset(&self) -> Result<()> {
        self.finish()
    }
    /// Queue the rendered screen with a distinct display sync lease.
    pub fn present(&self) -> Result<()> {
        self.check("present")?;
        let mut s = self.0.state.borrow_mut();
        self.end_scene(&mut s)?;
        let completed = self.0.core.native.display.completed.load(Ordering::Acquire);
        if s.presents.wrapping_sub(completed) >= DISPLAY_PENDING {
            s.display_waits += 1;
        }
        let serial = s
            .presents
            .checked_add(1)
            .ok_or(Error::Invalid("display generation exhausted"))?;
        let screens = self.0.screens.borrow();
        let new = &screens[s.screen];
        let old = &screens[s.last_presented];
        self.command("sceGxmPadHeartbeat", unsafe {
            sceGxmPadHeartbeat(&new.0.surface, new.0.sync.raw())
        })?;
        let data = DisplayData {
            base: new.0.texture.0.allocation.ptr(),
            pitch: (new.0.texture.0.layout.row_stride_bytes / 4) as u32,
            serial,
            vsync: u32::from(self.vsync()),
            state: &**self.0.core.native.display,
        };
        self.command("sceGxmDisplayQueueAddEntry", unsafe {
            sceGxmDisplayQueueAddEntry(
                old.0.sync.raw(),
                new.0.sync.raw(),
                (&data as *const DisplayData).cast(),
            )
        })?;
        s.presents = serial;
        s.last_presented = s.screen;
        s.screen = (s.screen + 1) % screens.len();
        self.check("present completion")
    }
    /// Snapshot native owners and command counters after the latest completion poll.
    /// This observes accounting without polling notifications or retiring owners.
    pub fn memory_usage(&self) -> Result<MemoryUsage> {
        self.check("memory accounting")?;
        let s = self.0.state.borrow();
        let n = &self.0.core.native;
        Ok(MemoryUsage {
            submission_timing: self.0.timing.snapshot(),
            pixels: self.0.core.pixels.usage(),
            buffers: self.0.core.buffers.usage(),
            parameter_cdram_bytes: self.0.core.config.parameter_bytes.into(),
            context_ring_bytes: n.blocks.iter().take(4).map(|b| b.bytes as u64).sum(),
            patcher_pool_bytes: n.blocks.iter().skip(4).map(|b| b.bytes as u64).sum(),
            patcher_host_bytes: unsafe { sceGxmShaderPatcherGetHostMemAllocated(n.patcher) }.into(),
            render_target_driver_bytes: self.0.core.rt_bytes.get(),
            imported_video_bytes: self.0.core.imported_video_bytes.get(),
            render_targets: self.0.targets.borrow().live_count() as u32,
            display_sync_objects: self
                .0
                .screens
                .borrow()
                .iter()
                .filter(|screen| !screen.0.sync.raw().is_null())
                .count() as u32,
            display_reserved_bytes: self.0.core.display_pixels.usage().reserved_bytes,
            display_pixel_bytes: self
                .0
                .screens
                .borrow()
                .iter()
                .map(|f| f.0.texture.0.allocation.len() as u64)
                .sum(),
            upload_staging_bytes: s
                .flights
                .iter()
                .filter(|f| !f.token.done.get())
                .map(|f| f.token.staging as u64)
                .sum(),
            scene_submissions: s.scene_submissions,
            notification_capacity_waits: s.slot_waits,
            display_queue_waits: s.display_waits,
            display_flips: u64::from(n.display.flips.load(Ordering::Relaxed)),
        })
    }
    /// Close after every caller-owned resource/fence is released; returns ownership on rejection.
    pub fn close(mut self) -> std::result::Result<(), (Self, Error)> {
        if let Err(e) = self.finish() {
            return Err((self, e));
        }
        if Rc::strong_count(&self.0) != 1 {
            return Err((self, Error::Invalid("context clones still alive")));
        }
        self.0.unbind_program();
        let internal = 1
            + self.0.screens.borrow().len() * 2
            + self.0.targets.borrow().live_count()
            + usize::from(self.0.clear.borrow().is_some()) * 2;
        if Rc::strong_count(&self.0.core) != internal {
            return Err((self, Error::Invalid("GXM resources or fences still alive")));
        }
        if let Err(e) = self.0.detach_display() {
            return Err((self, e));
        }
        let inner = Rc::get_mut(&mut self.0).expect("unique context");
        inner.clear.get_mut().take();
        inner.screens.get_mut().clear();
        inner.targets.get_mut().clear();
        inner.identity.get_mut().take();
        inner.state.get_mut().last = None;
        let core = Rc::get_mut(&mut inner.core).expect("all external device owners released");
        core.pixels.release_idle();
        core.display_pixels.release_idle();
        core.buffers.release_idle();
        if let Err(e) = core.native.close() {
            return Err((self, e));
        }
        Ok(())
    }
}
impl Inner {
    fn retain_faulted_work(&mut self) {
        if let Some(r) = self.state.get_mut().recording.take() {
            core::mem::forget(r);
        }
        core::mem::forget(core::mem::take(&mut self.state.get_mut().flights));
        core::mem::forget(core::mem::take(self.screens.get_mut()));
        core::mem::forget(self.clear.get_mut().take());
        core::mem::forget(self.bound_program.get_mut().take());
    }
    fn unbind_program(&self) {
        unsafe {
            sceGxmSetVertexProgram(self.core.native.context, core::ptr::null_mut());
            sceGxmSetFragmentProgram(self.core.native.context, core::ptr::null_mut());
        }
        self.bound_program.borrow_mut().take();
    }
    fn detach_display(&self) -> Result<()> {
        if self.display_detached.get() {
            return Ok(());
        }
        self.core
            .fault
            .command("sceDisplaySetFrameBuf(black)", unsafe {
                sceDisplaySetFrameBuf(core::ptr::null(), SCE_DISPLAY_SETBUF_NEXTFRAME)
            })?;
        self.core
            .fault
            .command("sceDisplayWaitSetFrameBuf(black)", unsafe {
                sceDisplayWaitSetFrameBuf()
            })?;
        self.display_detached.set(true);
        Ok(())
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        if self.core.native.closed {
            return;
        }
        let callback_error = self.core.native.display.error.load(Ordering::Acquire);
        if callback_error < 0 {
            self.core.fault.record("display callback", callback_error);
        }
        if self.core.fault.get().is_some() {
            self.retain_faulted_work();
            return;
        }
        // Drop is a final explicit ownership boundary, never a per-pass barrier.
        if self.state.get_mut().recording.is_some() {
            let code = unsafe {
                sceGxmEndScene(
                    self.core.native.context,
                    core::ptr::null(),
                    core::ptr::null(),
                )
            };
            if self
                .core
                .fault
                .command("sceGxmEndScene(drop)", code)
                .is_err()
            {
                return;
            }
        }
        unsafe {
            sceGxmFinish(self.core.native.context);
        }
        if self
            .core
            .fault
            .command("sceGxmTransferFinish(drop)", unsafe {
                sceGxmTransferFinish()
            })
            .is_err()
        {
            return;
        }
        if self
            .core
            .fault
            .command("sceGxmDisplayQueueFinish(drop)", unsafe {
                sceGxmDisplayQueueFinish()
            })
            .is_err()
        {
            return;
        }
        let callback_error = self.core.native.display.error.load(Ordering::Acquire);
        if callback_error < 0 {
            self.core.fault.record("display callback", callback_error);
            self.retain_faulted_work();
            return;
        }
        // Display Reference pp9-10,17: NULL selects black; transitions require NEXTVSYNC.
        if self.detach_display().is_err() {
            return;
        }
        self.unbind_program();
        for f in &self.state.get_mut().flights {
            f.token.poll();
        }
    }
}
