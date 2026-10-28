# vita-native

[English](README.md) | 简体中文

面向 PlayStation Vita 的 Rust 图形、系统、音频和视频接口。
原生调用使用 `vitasdk-sys = 0.3.3` 和 VitaSDK 导入库。

| 功能开关 | 接口 |
| --- | --- |
| `graphics` | GXM 上下文、离线 GXP 程序、纹理、缓冲、绘制、完成通知和呈现 |
| `app` | 应用标识、手柄与触摸输入、系统键盘、电源回调、文件和内存 |
| `process-allocator` | 基于独立系统 mspace 的统一 C/newlib/Rust 可增长进程堆 |
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

## 进程堆

`process-allocator` 为最终可执行程序启用统一 C/newlib/Rust 进程堆。
链接器保留 `vita_native_process_heap_anchor`，使 CRT 启动前接入分配函数；
应用在启动图形及会话/媒体工作线程前调用 `allocator::configure_main_budget`。
CRT 无法创建内核互斥锁或初始 arena 时终止启动。
CRT 创建一个 1 MiB 缓存 MAIN arena，小分配复用已有 arena；增长按页取得独立
MAIN 块，每块保留自身的 `sceClibMspace` 与内核 UID。1 MiB 最小 arena 尺寸对应 CPU
支持的页尺寸并摊薄内核分配成本，仅首块保持温热。其余 arena 完全空闲后归还整块。
内核释放失败时保留 UID 和提交费用，由后续非实时分配重试。

普通和 `_reent` 分配函数共用所有权与内核互斥锁。零尺寸申请取得最小对象，
`realloc(p, 0)` 保留 newlib 的最小对象语义，重分配失败保留旧对象。
对齐分配验证算术和二次幂对齐。堆钩子仅使用固定元数据与系统调用；
实时音频回调继续消费预分配缓冲。进程堆保持到 newlib 收尾完成，
最终 CRT 清理前由应用结束工作线程及设备所有者。

物理 MAIN 准入按完整 CPU arena 与原生图形块计量一次，包含头部和页对齐。
配置从内核当前空闲 MAIN 加已提交的纳管块取得容量；程序、已有线程栈、SceLibc
堆和 SDK 分配已从该观测扣除。之后的未纳管分配仍可能导致内核分配失败。
每个 VM 保留本地准入额度，共享验证代码的物理存储保持一次计量。
`allocator::stats` 返回实际提交容量和原生释放错误。

## 内存准入

原生预算准入区分堆内存、普通 MAIN 内核块和连续物理 MAIN 块。视频元数据使用堆内准入，NV12 和解码表面保持连续块票据直至最后读取者释放；内核各类剩余容量不直接相加。

原生分配池分别共享 MAIN 和 CDRAM 的物理准入。CDRAM 在 libgxm 初始化后读取剩余容量，
参数缓冲已由内核扣除；扫描输出、字形和渲染目标共用剩余空间。分池负责生命周期与碎片管理，
空闲保留量作为缓存目标。页对齐后的块持有额度直至物理释放成功；压力回收同一物理域内的
空闲所有者，实际释放后重试。调用方可额外提供 MAIN 配额。
内核页分配耗尽时返回页对齐请求、已占用容量、可查询的内核空闲量和原始 SDK 返回码。
应用可释放可重建缓存后重试；已提交资源继续持有至完成。
`Context::memory_state` 直接读取物理计数，用于工作集规划。

启用 `process-allocator` 时，调用方预算保留为子系统额度；实际 MAIN 提交量
同时与 CPU arena 在进程账本竞争。

## 图形

应用通过 `ProgramDescriptor` 提供离线 GXP、顶点布局和反射参数名。
片元程序可声明一个 float4 裁剪参数，接收精确到像素的裁剪边界。
`ClearProgram` 描述应用的清空程序及参数名。程序创建后由绘制复用。
`FragmentOutput::Declared` 接收由 `__nativecolor` 声明输出格式和混合的程序，
绘制时使用替换模式。
`ProgramDescriptor::instance` 声明按实例编号读取的第二条 f32 顶点流。
`Draw::instances` 提供不可变缓冲与范围，基础索引按原顺序为各实例循环。
每次绘制最多包含 65,535 个实例；两条顶点流和索引存储均保留至片元处理完成。
普通绘制使用一条索引顶点流。

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
映射分配池按小对象、中等对象和独占块组织资源，上传暂存使用单独的块。
各类竞争对应物理域的容量，并分别维护空闲缓存目标；分配与完成租约持有实际存储。

内核分配容量、有效载荷和设备基础存储分别计量。映射 arena 按可配置额度保留
近期使用的块，默认像素 8 MiB、缓冲 1 MiB，固件调度目标最多保留 4 MiB 供复用。分配压力释放空闲所有者，已提交工作持有资源至完成。
内核报告物理页不足时，设备回收同一物理域中的空闲缓存；实际释放存储后重试一次。
保留块继续计入对应物理域的占用。

纹理通过 `TextureMemory` 选择 CDRAM 或无 CPU 缓存的 MAIN 内存。渲染目标通过
`TargetFormat` 选择 RGBA8 或单通道 alpha 覆盖，布局、色表面和采样视图共用格式对应的行距。
渲染目标与扫描输出使用 CDRAM；应用按容量和带宽需求安排源图片。`MemoryUsage` 分别记录各物理池；
CDRAM 准入包含扫描输出和所有像素块，SDK 参数缓冲在初始化该物理域前扣除。

## 所有权与线程

图形上下文在同一线程创建和使用。纹理、程序和几何缓冲持有设备引用。
调用 `Context::close` 前释放调用方持有的资源与完成通知；关闭失败会返回上下文，
供调用方继续处理尚未释放的所有者。正常完成阶段调用 `Context::poll`，再接纳新的资源工作；
画面保持静止时同样轮询。它逐条检查场景/传输通知并释放已完成提交。
`Fence::poll` 检查自身提交；`memory_usage` 返回最近一次完成轮询后的计量快照。

音频拥有专用输出线程。回调填充预分配的立体声缓冲，期间保持无内存分配、
文件 IO 和主线程等待。`play`、`pause` 发布输出线程请求后返回；`state` 按请求代次区分
待处理与已确认状态，`Paused` 表示此前提交的 PCM 已排空。端口打开也在输出线程执行。
`begin_stop` 发出关闭请求，`poll_stop` 观察真实端口和缓冲释放；页面或会话切换保留原
`Stream` 至关闭完成。释放失败时该线程继续持有端口及双缓冲，由显式关闭请求重试；
`stop` 用于最终所有者的阻塞收尾。

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
VITASDK=/usr/local/vitasdk \
PATH="/usr/local/vitasdk/bin:$PATH" TARGET_CC=arm-vita-eabi-gcc \
cargo +nightly check --all-features \
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
