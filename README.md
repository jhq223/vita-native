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
Compatible color surfaces share bounded firmware scheduling targets. Configured
budgets account for kernel capacity, live payload and infrastructure separately.

## Ownership and threads

Create and use a graphics context on one thread. Textures, programs and geometry
retain their device. Drop caller-owned resources and fences before `Context::close`;
failed close returns the context so its owners remain available.

Audio owns a dedicated output worker. Its callback fills a preallocated stereo
slice without allocation, file IO or waiting on the main thread. `pause` drains
submitted PCM before acknowledging; `stop` joins the worker and releases the port.

Video owns a seekable input and an allocation budget supplied by the application.
Output polling copies each native picture or PCM block into caller-owned storage
before AvPlayer can recycle it. `Pending`, `Ready` and `End` identify progress.

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
