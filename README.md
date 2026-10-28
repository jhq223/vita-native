# vita-native

English | [简体中文](README.zh-CN.md)

Owned Rust interfaces for PlayStation Vita graphics, host services, audio and video.
Native calls use `vitasdk-sys = 0.3.3` and the VitaSDK import libraries.

| Feature | Interface |
| --- | --- |
| `graphics` | GXM contexts, offline GXP programs, textures, buffers, commands, completion and presentation |
| `app` | Application identity, controller/touch input, IME, power callbacks, IO and memory |
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

## Graphics

The application supplies offline GXP code, vertex layouts and reflected binding
names through `ProgramDescriptor`. Programs may declare a float4 clipping uniform
to support exact pixel scissors. `ClearProgram` describes the application's clear
programs and binding names. Programs are prepared once and reused by draws.

Textures and buffers own mapped allocations. Scene submissions retain their inputs
until fragment completion, and display scanout retains its output independently.
Bound programs remain owned until replaced or unbound. Presentation uses three
scanout buffers and native queue admission; resource updates wait for their own users.
Each scanout buffer owns one persistent display sync object (three per context).
Offscreen surfaces use ordered fragment scenes and fragment/transfer synchronization
flags; their storage remains leased through completion notifications. Native accounting
reports display sync object and scheduling target counts separately from bytes.
The optional `profiling` feature adds fixed cumulative SDK call counts and CPU wall
times to `MemoryUsage::submission_timing`, separating scene begin/end, uniform
reservation, draw submission and three explicit notification wait causes. These
measurements include SDK blocking; they do not measure GPU shader execution.
Builds without this feature erase the timers and return zero timing values.
Compatible color surfaces share bounded firmware scheduling targets. Configured
budgets account for kernel capacity, live payload and infrastructure separately.
Mapped arenas retain recently used blocks within configurable limits: 8 MiB for
pixels and 1 MiB for buffers by default. Scheduling targets retain up to 4 MiB
within their 16 MiB total limit. Allocation pressure releases idle owners; submitted
work keeps its resources until completion.

## Ownership and threads

Create and use a graphics context on one thread. Textures, programs and geometry
retain their device. Drop caller-owned resources and fences before `Context::close`;
failed close returns the context so its owners remain available.

Audio owns a dedicated output worker. Its callback fills a preallocated stereo
slice without allocation, file IO or waiting on the main thread. `pause` drains
submitted PCM before acknowledging; `stop` joins the worker and releases the port.

Video owns a seekable input and an allocation budget supplied by the application.
Output polling copies each native picture or PCM block into caller-owned storage
before AvPlayer can recycle it. Opening waits for native playback readiness; AAC
frame lengths exclude padding from the PCM clock. `Pending`, `Ready` and `End`
identify progress. `Nv12Pool` provides bounded output blocks that seal into immutable
`SharedNv12` storage. Pool admission counts allocations in progress; physical
allocation and initial clearing run outside the recycle lock, so completed GPU
readers can return existing blocks during allocation. GXM imports blocks for
sampling and retains their mapping through fragment completion; the final reader returns each block to its pool.

## Building

Use nightly Rust with the `rust-src` component and VitaSDK:

```sh
VITASDK=/usr/local/vitasdk cargo +nightly check --all-features \
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
