# vita-native

English | [简体中文](README.zh-CN.md)

Owned Rust interfaces for PlayStation Vita graphics, host services, audio and video.
Native calls use `vitasdk-sys = 0.3.3` and the VitaSDK import libraries.

| Feature | Interface |
| --- | --- |
| `graphics` | GXM contexts, offline GXP programs, textures, buffers, commands, completion and presentation |
| `app` | Application identity, controller/touch input, IME, power callbacks, IO and memory |
| `process-allocator` | One growing C/newlib/Rust process heap backed by independent system mspaces |
| `audio` | Fixed-buffer AudioOut playback with explicit pause and drain |
| `video` | AvPlayer decoding from a seekable stream into caller-owned video and PCM buffers |

The default features are `app`, `graphics` and `audio`. Applications enable `video`
when needed. Native devices are available on `target_os = "vita"`; pure graphics
layout calculations and ownership-policy tests also run on desktop.

## Dependency

Pin the Git revision used by your application:

```toml
[dependencies]
vita-native = { git = "https://github.com/jhq223/vita-native", rev = "COMMIT", features = ["video"] }
```

Use `default-features = false` and select `graphics`, `app`, `audio` or `video`
for a narrower build. Audio and video enable the host services they use.

## Process heap

`process-allocator` opts the final executable into a single C/newlib/Rust heap.
Retain `vita_native_process_heap_anchor` with the linker before CRT startup; call
`allocator::configure_main_budget` before starting graphics or session/media workers.
CRT initialization aborts if its kernel mutex or initial arena cannot be created.
The CRT creates one 1 MiB cached MAIN arena. Small allocations reuse this arena;
growth obtains independent page-rounded MAIN blocks with their own system
`sceClibMspace` and kernel UID. The 1 MiB minimum arena size follows the supported
CPU page sizes and amortizes kernel allocation; only the first arena stays warm.
An empty additional arena returns its whole kernel block. A failed kernel release
retains its UID and committed bytes for a later non-real-time retry.

Ordinary and `_reent` allocation functions share the same ownership and kernel
mutex. Zero-sized requests reserve a minimum object; `realloc(p, 0)` preserves
newlib's minimum-object behavior. Failed reallocations retain the old object.
Aligned allocation validates arithmetic and power-of-two alignment. No heap hook
allocates Rust metadata, performs IO, or invokes application callbacks. Audio
callbacks continue to use only their previously allocated buffers.

Physical MAIN admission counts complete CPU arenas and native graphics blocks
once, including arena headers and page rounding. Configuration observes current
kernel free MAIN plus already committed managed blocks; it does not assume the
entire extended SFO budget is free. Loaded code, existing stacks, SceLibc heaps
and SDK allocations already reduce that observation. Later unmanaged allocations
can still make kernel allocation fail. VM-local quotas remain independent:
shared verified code is admitted by each VM but is physically stored once.
`allocator::stats` reports committed capacity and the original release error.
The process heap remains available through newlib teardown; final CRT cleanup
requires application workers and device owners to have finished.

## Memory admission

Native budget admissions distinguish heap storage, ordinary MAIN kernel blocks and physically contiguous MAIN blocks. Video metadata uses heap admission; each NV12 or codec surface keeps its contiguous allocation reservation until the last reader releases it. The host supplies the limits for these classes without adding their kernel free counters.

Native arenas share one physical admission domain for MAIN and one for CDRAM.
The CDRAM capacity is observed after libgxm initialization, so parameter-buffer
storage has already been deducted. Scanout, glyphs and render targets then compete
for that capacity. Arena separation controls lifetime and fragmentation; active allocations share
physical capacity and idle retention values set soft cache targets.
Every rounded block holds its charge until successful physical release. Pressure
reclaims only idle owners in the requested domain, then retries after real release.
Kernel page exhaustion reports the rounded request, managed commitment, kernel
free bytes when available, and the original SDK code. Applications can release
rebuildable caches and retry; submitted owners remain leased through completion.
The optional caller MAIN budget can impose an additional application quota.
`Context::memory_state` reads physical commitment counters in constant time.

With `process-allocator`, the caller budget remains a subsystem limit. Physical
MAIN commitment also competes with CPU arenas in the process ledger.

## Graphics

The application supplies offline GXP code, vertex layouts and reflected binding
names through `ProgramDescriptor`. Programs may declare a float4 clipping uniform
to support exact pixel scissors. `ClearProgram` describes the application's clear
programs and binding names. Programs are prepared once and reused by draws.
`FragmentOutput::Declared` accepts `__nativecolor` programs that own blending and
the output register format; their draws use replacement mode.
`ProgramDescriptor::instance` describes an optional second f32 stream indexed by
instance number. `Draw::instances` supplies its immutable buffer and range; base
indices wrap once per instance in original order. Each draw accepts at most
65,535 instances, and both streams and the index allocation remain retained until
fragment completion. Ordinary draws use one indexed stream.

Textures select CDRAM or uncached MAIN through `TextureMemory`. Render targets
select RGBA8 or single-channel alpha coverage through `TargetFormat`; their strided
layout, color surface and sampling view share the same format-specific pitch.
Render targets and scanout use CDRAM; applications choose sampled-image placement to balance capacity
and bandwidth. `MemoryUsage` reports each physical arena separately. CDRAM admission includes scanout and all mapped pixel blocks; the SDK parameter
buffer is deducted before this shared domain is initialized.

Textures and buffers own mapped allocations. Scene submissions retain their inputs
until fragment completion, and display scanout retains its output independently.
Bound programs remain owned until replaced or unbound. Presentation uses three
scanout buffers and native queue admission; resource updates wait for their own users.
`Context::with_config` accepts a display-completion wake callback. It runs on the
display thread after completion is published and only signals the application.
The device retains it until the display queue has drained during close.
Each scanout buffer owns one persistent display sync object (three per context).
Offscreen surfaces use ordered fragment scenes and fragment/transfer synchronization
flags; their storage remains leased through completion notifications. Native accounting
reports display sync object and scheduling target counts separately from bytes.
The optional `profiling` feature adds fixed cumulative SDK call counts and CPU wall
times to `MemoryUsage::submission_timing`, separating scene begin/end, uniform
reservation, draw submission and three explicit notification wait causes. These
measurements include SDK blocking; they do not measure GPU shader execution.
EndScene samples the caller's kernel execution counter at most once per 100 ms,
reporting paired wall/execution time, query overhead and original query errors.
Notification publication and first normal-poll observation remain distinct; unmatched
counts read notifications without retiring owners. Display-thread flip/vsync waits
are separate. One latest EndScene sample of at least 1 ms records the shared target,
notification generation, draw/index/triangle counts and outstanding work before/after
the call. Counters use fixed storage and existing completion tokens carry their inline
diagnostics; no additional per-call allocation or GPU work is introduced.
Builds without this feature erase the timers and return zero timing values.
Compatible color surfaces share bounded firmware scheduling targets. Configured
budgets account for kernel capacity, live payload and infrastructure separately.
Mapped arenas segregate small, medium and whole-block resources. Upload staging
uses separate blocks; completed uploads cannot leave small staging allocations
among retained graphics buffers. All classes compete for their physical domain and use separate soft idle
retention targets. Allocation and completion leases preserve physical ownership.

Mapped arenas retain recently used blocks within configurable limits: 8 MiB for
pixels and 1 MiB for buffers by default. Scheduling targets retain up to 4 MiB
for reuse. Allocation pressure releases idle owners; submitted
work keeps its resources until completion. When kernel allocation reports exhausted
physical pages, the device releases idle caches in that physical domain and retries
once if storage was released. Retained blocks remain charged to their physical domain.

`Context::copy_texture` copies unscaled RGBA8 rectangles between independent
strided render targets using PTLA. Fragment/transfer dependencies and the final
notification retain both allocations. Applications keep drawing paths for other formats.

## Ownership and threads

Create and use a graphics context on one thread. Textures, programs and geometry
retain their device. Drop caller-owned resources and fences before `Context::close`;
failed close returns the context so its owners remain available. Call `Context::poll`
in the normal completion phase before admitting new resource work, including on an
unchanged image. It observes each scene/transfer notification and releases only
completed submissions. `Fence::poll` observes its own submission;
`memory_usage` returns the accounting snapshot after the latest completion poll.

Audio owns a dedicated output worker, including asynchronous native port opening.
Its callback fills a preallocated stereo
slice without allocation, file IO or waiting on the main thread. `play` and
`pause` publish retained worker requests without waiting for native output or
drain. `state` distinguishes a pending request from the current generation's
worker acknowledgement; `Paused` confirms that previously submitted PCM drained.
The worker retains both native blocks throughout that transition. `begin_stop`
requests shutdown; `poll_stop` observes actual port closure without waiting. The
same worker drains and releases the port, retaining both blocks on release
failure until an explicit retry. Keep the Stream while closing. `stop` is the
blocking final-owner operation; normal page/session transitions use begin/poll.

Video owns a seekable input and an allocation budget supplied by the application.
Output polling copies each native picture or PCM block into caller-owned storage
before AvPlayer can recycle it. Opening waits for native playback readiness; AAC
frame lengths exclude padding from the PCM clock. `Pending`, `Ready` and `End`
identify progress. `Nv12Pool` provides bounded output blocks that seal into immutable
`SharedNv12` storage. Pool admission counts allocations in progress; physical
allocation and initial clearing run outside the recycle lock, so completed GPU
readers can return existing blocks during allocation. GXM imports blocks for
sampling and retains their mapping through fragment completion; the final reader returns each block to its pool.
Profiling builds report worker-local AvPlayer poll counts, returned timestamps,
copy time, source stride and pause/resume residency when each decoder closes.
These are CPU call observations; they do not measure hardware codec execution time.

## Building

Use nightly Rust with the `rust-src` component and VitaSDK:

```sh
VITASDK=/usr/local/vitasdk \
PATH="/usr/local/vitasdk/bin:$PATH" TARGET_CC=arm-vita-eabi-gcc \
cargo +nightly check --all-features \
  --target armv7-sony-vita-newlibeabihf -Z build-std=std,panic_unwind
```

Applications package their executable with `cargo vita build vpk`. GXP programs
are built offline with the Sony shader compiler and supplied as application assets.

CPU checks:

```sh
cargo test --all-features
cargo fmt --all -- --check
```

## License

[MIT](LICENSE).
