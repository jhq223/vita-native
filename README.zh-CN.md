# vita-native

[English](README.md) | 简体中文

面向 PlayStation Vita 的 Rust 图形、系统、音频和视频接口。
原生调用使用 `vitasdk-sys = 0.3.3` 和 VitaSDK 导入库。

| 功能开关 | 接口 |
| --- | --- |
| `graphics` | GXM 上下文、离线 GXP 程序、纹理、缓冲、绘制、完成通知和呈现 |
| `app` | 应用标识、手柄与触摸输入、系统键盘、电源回调、文件和内存 |
| `audio` | 固定缓冲的 AudioOut 播放、暂停与排空 |
| `video` | AvPlayer 从可定位流解码，将视频和 PCM 交给调用方持有 |

默认启用 `app`、`graphics` 和 `audio`，需要视频时启用 `video`。
原生设备接口用于 `target_os = "vita"`；图形存储布局计算和所有权策略测试也能在桌面运行。

## 依赖

应用固定使用经过验证的 Git 提交：

```toml
[dependencies]
vita-native = { git = "https://github.com/jhq223/vita-native", rev = "COMMIT", features = ["video"] }
```

按需构建时设置 `default-features = false`，再选择 `graphics`、`app`、`audio` 或 `video`。
音频和视频会启用自身使用的系统服务。

## 图形

应用通过 `ProgramDescriptor` 提供离线 GXP、顶点布局和反射参数名。
片元程序可声明一个 float4 裁剪参数，接收精确到像素的裁剪边界。
`ClearProgram` 描述应用的清空程序及参数名。程序创建后由绘制复用。
`FragmentOutput::Declared` 接收由 `__nativecolor` 声明输出格式和混合的程序，
绘制时使用替换模式。

纹理和缓冲持有映射后的内存。场景提交保留输入资源直到片元处理完成，
显示扫描单独保留输出。已绑定的程序持有至换绑或解绑；三个扫描缓冲通过原生
显示队列调度，资源更新等待自身的使用者完成。`Context::with_config` 可接收显示完成唤醒回调，
在完成状态发布后由显示线程通知应用；设备关闭时，回调随显示队列排空后释放。
每个扫描缓冲固定拥有一个显示同步对象，
每个上下文共三个。离屏表面通过有序片元场景及片元/传输同步标志排序，完成通知保留
存储至使用结束。原生计量分别报告显示同步对象数、
固件调度目标数及存储字节。可选 `profiling` 功能在 `MemoryUsage::submission_timing`
记录固定分项的 SDK 调用次数和 CPU 壁钟时间：场景开始/结束、uniform 预留、draw 提交
及三种显式通知等待。计时包含 SDK 内部阻塞，GPU 着色耗时需要另行测量。
关闭该功能时编译移除计时器，返回零计时值。兼容尺寸的颜色表面共享数量受限的固件调度目标。
内核分配容量、有效载荷和设备基础存储分别计量。映射 arena 按可配置额度保留
近期使用的块，默认像素 8 MiB、缓冲 1 MiB。固件调度目标的总额度为 16 MiB，
其中最多保留 4 MiB 供复用。分配压力释放空闲所有者，已提交工作持有资源至完成。

## 所有权与线程

图形上下文在同一线程创建和使用。纹理、程序和几何缓冲持有设备引用。
调用 `Context::close` 前释放调用方持有的资源与完成通知；关闭失败会返回上下文，
供调用方继续处理尚未释放的所有者。正常完成阶段调用 `Context::poll`，再接纳新的资源工作；
画面保持静止时同样轮询。它逐条检查场景/传输通知并释放已完成提交。
`Fence::poll` 检查自身提交；`memory_usage` 返回最近一次完成轮询后的计量快照。

音频拥有专用输出线程。回调填充预分配的立体声缓冲，期间保持无内存分配、
文件 IO 和主线程等待。`pause` 在已提交 PCM 播放完成后确认暂停；
`stop` 等待工作线程结束，再释放端口。

视频拥有可定位输入流，并使用应用提供的内存额度。
输出轮询在 AvPlayer 回收数据前，将画面或 PCM 复制到调用方存储。
打开操作等待原生播放就绪；PCM 按 AAC 帧长读取有效采样，缓冲填充不计入播放时钟。
`Pending`、`Ready` 和 `End` 分别表示等待、取得数据和结束。
`Nv12Pool` 提供有界输出块，封存后成为只读的 `SharedNv12`。容量准入包含正在分配的名额；
物理分配和初次清零在回收锁外执行，已完成的 GPU 读者可在分配期间归还原有块。
GXM 导入该存储用于采样，
保留映射至片元处理完成；最后一个读者释放后，存储回到原池。

## 构建

使用安装了 `rust-src` 的 nightly Rust 和 VitaSDK：

```sh
VITASDK=/usr/local/vitasdk cargo +nightly check --all-features \
  --target armv7-sony-vita-newlibeabihf -Z build-std=std,panic_unwind
```

应用通过 `cargo vita build vpk` 打包可执行程序。
GXP 使用索尼着色器编译器离线生成，作为应用资源提供。

CPU 检查：

```sh
cargo test --all-features
cargo fmt --all -- --check
```

## 许可证

[MIT](LICENSE)。
