# S4-C · `vox-audio-alsa`：ALSA 音频后端（无屏档缺省）设计稿

> 状态：**[设计稿·未开工]**。对应 `docs/plans/S4-EMBEDDED-REFACTOR.md` §3 S4-C 表格「`vox-audio-alsa`」一行、
> §2.3 轻量化、§4「S4-C · ALSA」验收，以及 §6 第 5 问的答复。
>
> **本稿只设计 `crates/vox-audio-alsa` 这个新 crate 本身**（实现 `vox-core` 的三个端口 trait + 自测）。
> 另一路 architect 正在并行细化 S4-A（共享宿主层 `crates/vox-host`）。**本稿不去设计 `vox-host`**，
> 只在 §7 写"接线说明"，明确标注"依赖 S4-A 落地后的注入点，施工排在 S4-A 之后"。
>
> 口径：代码与本稿打架以代码为准。本稿的"现状证据"是 **2026-09-30** 读代码核到的；
> 引用优先给符号名，行号只作参考并注明核对日期（`.omp/AGENTS.md`「引用要耐久」、`.omp/RULES.md` #11）。
> **本轮环境里 `cargo` 不可执行**（沙箱未放行），因此**没有跑过任何编译、测试或 `aplay`**，
> 凡涉及"跑出来的结果"的地方一律写"未跑"（`.omp/RULES.md` #10）。

---

## 0. 一句话

**新建 `crates/vox-audio-alsa`，用 `alsa` crate 直接开 PCM，实现 `CaptureSource` / `PlaybackSink` /
`DeviceRegistry` 三个端口 trait，做成无屏档的缺省后端；用一段可脱开硬件测的纯函数把"参数协商阶梯"
和"io 结果 → 下一步动作"钉死，硬件相关的东西全部收敛到 `#![ignore]` 用例与 example。**

三个端口 trait 的实现形状几乎逐行照搬 `crates/vox-audio-linux`（PipeWire）与 `crates/vox-audio-win`
（WASAPI）——那两个 crate 已经把"每条流一个线程 / 有界 start 超时 / 2 秒有界 stop / 回调侧不分配不打日志"
这些纪律定下来了，ALSA 是第三次执行同一条纪律，不是新发明。

---

## 1. 现状证据（2026-09-30 核）

### 1.1 端口契约，逐条

契约来源全部是 `crates/vox-core/src/ports.rs`（行号标注核对日期）。

| # | 契约 | 出处（符号名 / 行号） | 对实现方的硬要求 |
| --- | --- | --- | --- |
| C1 | `trait CaptureSource: Send` | `ports.rs::CaptureSource`（`:78`，2026-09-30 核） | 类型必须能跨线程移动（持有 `JoinHandle`、`Arc` 即可） |
| C2 | `start(&mut self, target, block_ms, on_chunk) -> PortResult<CaptureFormat>` | 同上（`:79-84`） | **必须同步返回协商到的格式**；不能返回一个"之后才知道"的占位值而不告诉上层 |
| C3 | `start` 的超时口径 | `crates/vox-audio-linux/src/capture.rs::START_TIMEOUT = 8s` + `report_rx.recv_timeout(START_TIMEOUT)`（`:50` / `:123`，2026-09-30 核） | `start` 内部自己开线程 + 用有界 `recv_timeout` 等协商结果；**不许无限阻塞在 open/hw_params 里** |
| C4 | `stop(&mut self)`；"stop 之后回调不再触发" | `ports.rs::CaptureSource::stop` 的文档（`:86`，2026-09-30 核）；`vox-audio-linux/src/capture.rs::LinuxCapture::stop` 的实现（`:145-167`，2026-09-30 核） | **有界等待**：既有实现给 2 s 上限，超时只 `tracing::warn!` 不 join。ALSA 必须同一量级 |
| C5 | `block_ms` 的含义 = 块长按**帧**算 | `crates/vox-dsp/src/chunk.rs::Blocker::new`（`:21-31`，2026-09-30 核）：`frames_per_block = rate * block_ms / 1000`，`block_ms < 10` 夹到 10 | 用 `Blocker` 就自动拿到这条；**但要保证喂进去的样本率与 `Blocker` 里的一致**（见 §5.7） |
| C6 | `AudioChunk { samples: Vec<f32>, sample_rate, channels }`，**交织**多声道 | `ports.rs::AudioChunk`（`:38-42`，2026-09-30 核）；下混是芯的事（`AudioChunk::to_mono`） | 采集侧交付**交织 f32**，声道数填协商到的值 |
| C7 | `CaptureTarget::Net { .. }` **必须报错**，不许当默认设备 | `ports.rs::CaptureTarget::Net` 的文档（`:70-74`，2026-09-30 核） | `match` 要写全四个变体 |
| C8 | `trait PlaybackSink: Send`；`open(device: Option<&str>, source_rate: u32) -> PortResult<u32>` 返回**实际采样率** | `ports.rs::PlaybackSink`（`:99-111`，2026-09-30 核） | 返回值必须真的等于我们往设备里送的率，否则上层算错延迟 |
| C9 | `push(&mut self, samples: &[f32])`：队列满时**丢最旧的，绝不阻塞** | 同上（`:102`，2026-09-30 核） | 用 `vox_dsp::ring::DropRing`（满了丢最旧，`crates/vox-dsp/src/ring.rs::DropRing::write` 返回丢掉的样本数，`:64`，2026-09-30 核） |
| C10 | `stats() -> PlaybackStats` 默认实现够用 | `ports.rs::PlaybackSink::stats`（`:105-107`，2026-09-30 核）；字段见 `PlaybackStats`（`:113-124`） | `queued_samples` / `sample_rate` / `channels` / `rendered_samples` / `dropped_samples` 要填；`device_latency_ms` 拿不到就 0 |
| C11 | `flush()` 要把"还没放出去的"立刻丢掉 | `ports.rs::PlaybackSink::flush`（`:109`，2026-09-30 核） | `DropRing::clear()` + `Resample::reset()`（与 `vox-audio-linux/src/playback.rs::LinuxPlayback::flush` 同口径，`:173-182`，2026-09-30 核） |
| C12 | `trait DeviceRegistry: Send + Sync` | `ports.rs::DeviceRegistry`（`:170`，2026-09-30 核） | 三个方法都只读、无长时状态；**无屏档每 30 s 轮询一次**（`docs/platform/EMBEDDED.md` §3.2） |
| C13 | `audio_apps()` = "正在放声音的程序"，给「听人说话」的选择器 | `ports.rs::DeviceRegistry::audio_apps`（`:174`，2026-09-30 核） | ALSA **没有这个概念**，见 §5.6 的决定 |
| C14 | `virtual_cable_installed()` 只描述**安装器状态**，恒不等于"能不能用" | `ports.rs::DeviceRegistry::virtual_cable_installed` 的文档（`:176-184`，2026-09-30 核）："Linux 上压根没有'装'这一步，这条恒为 `false`，而虚拟麦照样能在" | ALSA 侧就返回 `false`——**这不是缺陷，是文档已经写死的语义** |
| C15 | 错误类型只有一个：`PortError { message: String }` | `ports.rs::PortError`（`:12-30`，2026-09-30 核） | 库代码里**不许 panic、不许 `unwrap()`**；错误一律中文句子（`.omp/AGENTS.md` 硬约束、两个既有 crate 的模块头都这么写） |

### 1.2 端口调用方怎么用（决定哪些行为不能变）

- 播放侧喂进来的是 **24 kHz 单声道 f32**：`crates/vox-core/src/cloud/protocol.rs::OUTPUT_SAMPLE_RATE = 24_000`（`:28`，2026-09-30 核），
  注释见 `ports.rs::PlaybackSink`（`:98`，2026-09-30 核）"内核推 24 kHz 单声道 f32，外壳负责重采样到设备率"。
- 采集侧块长 = `crates/vox-core/src/pipeline::INPUT_BLOCK_MS = 20`（`crates/vox-core/src/pipeline/mod.rs:47`，2026-09-30 核），
  传参处 `crates/vox-core/src/composition.rs`（`:537`、`:1128`，2026-09-30 核）。
- 上行协议是 **16 kHz**：`protocol.rs::INPUT_SAMPLE_RATE = 16_000`（`:26`，2026-09-30 核）。
- 降噪只在 48 kHz 有效：`docs/platform/EMBEDDED.md` §4-8 记的三处写死（48 kHz 降噪 / 16 kHz 上行 / 24 kHz 回放）。
  → **采集侧优选 48 kHz**，播放侧优选 24 kHz。这两条直接决定 §5.2 的阶梯。
- 设备名从设置里来：`crates/vox-core/src/settings.rs` 的 `speak.input_device` / `speak.output_device` / `listen.output_device`（`:152`/`:154`/`:208`，2026-09-30 核），
  都是 `Option<String>`，`normalize()` 会把纯空白夹成 `None`（`:401-402`、`:413`，2026-09-30 核）。
  → **`DeviceInfo.name` 就是我们 `PCM::new` 要吃的那个名字**，不能只是给人看的标签。

### 1.3 工厂与注入点（决定本稿的边界）

- `crates/vox-core/src/pipeline/mod.rs::CaptureFactory = Box<dyn Fn() -> Box<dyn CaptureSource> + Send + Sync>`（`:68`，2026-09-30 核）、
  `PlaybackFactory`（`:70`）、`ResampleFactory = Box<dyn Fn(u32, u32) -> Box<dyn Resample> + Send + Sync>`（`:75`）。
  三个都是工厂，**每次 Start 都要全新实例**。
- 现有装配点：`crates/vox-headless/src/platform/mod.rs::Platform { capture, playback, registry }`（`:20-24`，2026-09-30 核），
  Linux 实现在 `crates/vox-headless/src/platform/linux.rs::platform()`（`:27-41`，2026-09-30 核），现在注入的是 `vox-audio-linux`。
  重采样器工厂在 `crates/vox-headless/src/dsp.rs::resample_factory()`（`:49-51`，2026-09-30 核）。
- **S4-A 会把这一层搬进 `crates/vox-host`。本稿不动它**（§7 只写接线说明）。

### 1.4 PipeWire 实现：ALSA 要照搬的 / 不适用的

照搬（这是纪律，不是实现细节）：

| 要照搬的 | 出处（2026-09-30 核） | 为什么 |
| --- | --- | --- |
| 每条流一个普通优先级线程，`start` 里 spawn + `recv_timeout` 等协商结果 | `vox-audio-linux/src/capture.rs::capture_thread` / `::playback.rs::stream_thread` | 协商失败要能变成 `PortError` 回传给 `start`，而不是在线程里默默失败 |
| 有界 `start` 超时（8 s）+ 有界 `stop`（2 s，超时只 warn 不 join） | `capture.rs::START_TIMEOUT`、`capture.rs::LinuxCapture::stop` 的 2 s deadline | 宁可漏一个线程，也不能让用户点"停止"没反应 |
| 重复 `start` 视为换目标：先 `self.stop()` 收干净 | `capture.rs::LinuxCapture::start` 第一行 `self.stop()`（`:101`，2026-09-30 核） | 否则两个流同时往回调里灌数据 |
| 失败回报走"一次性槽位"（`Mutex<Option<Sender>>`），谁先拿到谁负责回报 | `capture.rs::report_started` / `::report_failed`（`:449-464`，2026-09-30 核） | 区分"还没开工 → 报给 `start`"与"已经开工 → 只记日志" |
| 播放侧：重采样 + 铺声道在**流水线线程**做，ALSA 线程只做定长搬运 | `vox-audio-linux/src/playback.rs::LinuxPlayback::push`（`:130-157`，2026-09-30 核） | `push` 允许分配、允许打日志；设备线程不许 |
| 播放侧复用 `interleave: Vec<f32>`，`clear()` 后重填，不每次 `push` 新建 | 同上 `:50` / `:138`，2026-09-30 核 | `.omp/RULES.md` #6 |
| 环欠载补静音、不报错 | `playback.rs` 的 process 回调注释（`:298-300`，2026-09-30 核）"欠载时剩下的由 `read_into` 补静音" | 语音流断一下比崩掉强 |
| 库代码无 `unwrap()` / `expect()`、无 `panic!` | 两个 crate 的 `lib.rs` 模块头 | 无屏档没有界面，panic 只能靠 journal 事后看 |
| `#![cfg(target_os = "linux")]` + 依赖放 `[target.'cfg(target_os = "linux")'.dependencies]` | `crates/vox-audio-linux/src/lib.rs:17` / `crates/vox-audio-linux/Cargo.toml` | 让 `cargo test --workspace` 在非 Linux 上不卡住；Windows 上 `alsa-sys` 的 build.rs 根本不会跑 |

**不适用（PipeWire 那套在 ALSA 上没有对应物）**：

| PipeWire 的做法 | 为什么 ALSA 不能照搬 |
| --- | --- |
| `probe::connect_request_format` 向图请求 48 kHz / 2 ch / f32，由**图**转成设备要的格式（`vox-audio-linux/src/probe.rs::REQUEST_RATE/REQUEST_CHANNELS` + `connect_request_format`，`:348-360`，2026-09-30 核） | ALSA 直开设备**没有混音图**：设备只给硬件支持的格式，多了少了都不行。必须自己做协商阶梯（§5.2） |
| `AudioInfoRaw` 解析 `param_changed` 拿到协商结果 | ALSA 的协商结果在 `HwParams` / `hw_params_current()`，不是回调（§5.2） |
| `node.autoconnect` + `link_keeper` 显式建链做按程序抓音 | ALSA **没有"按程序抓音"这个概念**（`docs/platform/LINUX.md` §9.1 的结论在 ALSA 下更成立）。`CaptureTarget::ProcessLoopback` 必须报错（§5.5） |
| `PipeWire 的 process 回调` 跑在 RT 线程上，所以采集侧**故意不设** `RT_PROCESS`（`capture.rs` 模块头，2026-09-30 核） | ALSA 没有 RT 回调这回事：采集发生在我们自己的 `readi` 循环里。这条纪律的**结果**（不在实时上下文调 `on_chunk`）照搬，**理由**换掉 |
| 播放侧请求 48 kHz 立体声、由图重采样（`playback.rs` 模块头 `:8-9`，2026-09-30 核） | 同第一条。ALSA 播放侧要按 §5.2 阶梯，要么让设备直接给 24 kHz，要么自己装 `Resample` |
| `PipeWire 在不在` = `probe::available()` = 能连上图就算在（`probe.rs:168-170`，2026-09-30 核） | ALSA 没有 daemon。"在不在"退化成"能不能开一条 PCM"（§5.8） |
| `time().delay()` 报流延迟（`playback.rs:311-320`，2026-09-30 核） | ALSA 侧用 `PCM::delay()`（`snd_pcm_delay`），拿不到就按 `PlaybackStats` 的注释填 0（`ports.rs:123`，2026-09-30 核） |

### 1.5 能力位现状（§6 第 5 问的判据）

- 档位上限表在芯里：`crates/vox-core/src/capability.rs::host_ceiling`（`:388-420`，2026-09-30 核）。
  `HostKind::LinuxHeadless` 只有 **两位**：`Mic` + `BackgroundService`（`:414-418`，2026-09-30 核）。
- 需要 PipeWire 的两位——`ProgramTap`（按程序抓音）与 `VirtualMic`——**在无屏档上限之外**，芯直接算
  `false(unsupported)`（`capability.rs::status_of` 的 `!ceiling.contains(bit)` 分支，`:484-487`，2026-09-30 核）。
  独立佐证：`docs/platform/EMBEDDED.md` §2 那张表对 `virtual_mic` / `program_tap` 两行写的都是 `false(unsupported)`。
- 无屏档现在的事实：`crates/vox-headless/src/platform/linux.rs::host_facts()`（`:54-60`，2026-09-30 核）
  —— `off` 里只有 `background_service = not_wired`，`mic` **不进 `off`**（按上限报开）。
  注释已经写明理由：装配期没有任何否定证据，"麦克风被独占"要真去开流才发现。
- `UnavailableReason` 里已有现成的两个 reason：`Busy`（"麦克风被别的程序占着"）、`Permission`（`capability.rs::Busy` / `::Permission`，`:260-272`，2026-09-30 核）。

> **这张表就是 §6 第 5 问的全部答案基础**：无屏档里，ALSA 能不能开设备，**一位能力位都不影响**。

---

## 2. 依赖选型

### 2.1 选 `alsa` crate 0.12.1

| 项 | 值 | 依据 |
| --- | --- | --- |
| crate | `alsa = "0.12.1"` | 本机 cargo registry 里已有 `alsa-0.12.1`（`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/alsa-0.12.1/`，2026-09-30 核） |
| 传递依赖 | `alsa-sys = 0.6.1`、`bitflags = 2.13`、`cfg-if = 1.0`、`libc = 0.2.186` | `alsa-0.12.1/Cargo.toml`（`:59-69`，2026-09-30 核） |
| features | **只用默认的 `std`** | `alsa-0.12.1/Cargo.toml::[features] default = ["std"]`（`:51-53`，2026-09-30 核）。不开 `use-bindgen`——`alsa-sys` 的 `use-bindgen` 是非默认 feature（`alsa-sys-0.6.1/build.rs:3-4` 用 `#[cfg(feature = "use-bindgen")]` 门着），默认走**预生成绑定**，这正好避开 `vox-audio-linux` 那个"交叉编译要 clang + bindgen"的坑（`docs/platform/EMBEDDED.md` §3.1） |
| 许可证 | `Apache-2.0/MIT`（双许可） | `alsa-0.12.1/Cargo.toml::license`（`:42`，2026-09-30 核）。与本仓库 `license.workspace = "MIT"`（根 `Cargo.toml`）兼容 |
| 系统依赖（编译期） | `libasound2-dev`（`.pc` + 头文件） | `alsa-sys-0.6.1/build.rs::main` 只做一件事：`pkg_config::Config::new().statik(false).probe("alsa")`，失败时 panic 并打印 Debian/Ubuntu 的 `apt-get install libasound2-dev`（`:6-21`，2026-09-30 核） |
| 系统依赖（运行期） | `libasound2`（`libasound.so.2`） | 同上（`statik(false)` = 动态链接） |
| `edition` | 2021（`alsa` 自身） | 与本仓库 `edition.workspace = "2021"` 一致 |

**为什么不用 cpal / 不自己写 FFI**：cpal 的后端选择与格式协商是自己一套，且它的 ALSA 后端在
`format` 不匹配时行为与本项目要的"阶梯回落"对不上；直接 `alsa` crate 是 alsa-lib 的薄封装，
`HwParams` 的每一次试探都直接对应 alsa-lib 的语义，写出来的阶梯是可读、可单测的纯函数。

**为什么不用 mmap（`Access::MMapInterleaved`）**：`RWInterleaved` 是所有 ALSA 驱动都实现的；
mmap 需要驱动支持 `mmap` 能力位，多一个失败维度、少一个可测点。**这一稿只做 `RWInterleaved`**，
`io_i32()` / `io_i16()` 的 `readi` / `writei`（`alsa-0.12.1/src/pcm.rs::IO::readi:422` / `::IO::writei:416`，2026-09-30 核）。

### 2.2 Cargo.toml（新增 crate，逐字形状）

```toml
[package]
name = "vox-audio-alsa"
description = "ALSA 音频 I/O：麦克风采集、播放、设备目录（无屏档缺省后端）"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
vox-core.workspace = true
# 播放侧的 DropRing 在 vox-dsp 里（与 vox-audio-win / vox-audio-linux 共用同一份）。
vox-dsp.workspace = true
tracing.workspace = true
# 只为了 errno 常量（EPIPE / ESTRPIPE / EAGAIN），判读 io 结果要用。
# alsa 依赖 libc，所以这不新增任何编译产物。
libc = "0.2"

[target.'cfg(target_os = "linux")'.dependencies]
# alsa 是 libasound 的薄封装；编译期要 libasound2-dev（.pc + 头文件），
# 运行期只要 libasound2。默认 feature 只有 std，不开 alsa-sys 的 use-bindgen
# （那会把 bindgen + clang 拖进来，交叉编译时是纯负担）。
alsa = "0.12.1"
```

依赖放在 `[target.'cfg(target_os = "linux")'.dependencies]` 里，照抄 `crates/vox-audio-linux/Cargo.toml` 的形状。
这样 Windows 上 `alsa-sys` 的 `build.rs`（`alsa-sys-0.6.1/build.rs`，2026-09-30 核）根本不执行，
`cargo test --workspace` 在 Windows runner 上不会因为缺 `libasound2-dev` 而断。

---

## 3. 目标形状（模块与符号）

```
crates/vox-audio-alsa/
├── Cargo.toml
├── src/
│   ├── lib.rs        # #![cfg(target_os = "linux")] + 重导出 + 两个探测原语
│   ├── probe.rs      # 纯函数：协商阶梯 / io 结果判读 / 格式换算 / 错误翻译
│   ├── capture.rs    # CaptureSource → AlsaCapture
│   ├── playback.rs   # PlaybackSink  → AlsaPlayback
│   └── registry.rs   # DeviceRegistry → AlsaDeviceRegistry + 默认设备对号（纯函数）
├── examples/
│   ├── devices.rs    # 列设备（照 crates/vox-audio-linux/examples/devices.rs 的形状）
│   └── loopback.rs   # snd-aloop 上"采 → 播"，带 RMS 自证
└── tests/
    └── aloop_roundtrip.rs   # #[ignore] 真回环用例
```

`probe.rs` 是**唯一放纯逻辑的地方**——它是本稿里全部可脱开硬件测试的落点，也是 §6「测试与验收」的实现基础。

### 3.1 `lib.rs` 对外接口（照抄两个兄弟 crate 的形状）

```rust
//! ALSA 音频 I/O：麦克风采集、播放、设备目录（无屏档缺省后端）。
//!
//! 实现 `vox_core::ports` 里的 `CaptureSource` / `PlaybackSink` / `DeviceRegistry`。
//! 贯穿全 crate 的规矩与 `vox-audio-linux` / `vox-audio-win` 同口径：
//! - 设备线程只搬数据，不分配、不加锁、不打日志；
//! - 错误一律翻成中文 `PortError`，永不 panic；
//! - 库里没有 `unwrap()` / `expect()`，测试里可以有。

#![cfg(target_os = "linux")]

mod capture;
mod playback;
mod probe;
mod registry;

pub use capture::AlsaCapture;
pub use playback::AlsaPlayback;
pub use registry::AlsaDeviceRegistry;

/// ALSA 在不在 = **能不能打开一条 PCM**。装配层用它决定"能不能装音频后端"。
///
/// 不缓存：USB 声卡随时可能被拔，缓存下来的 `true` 是假事实
/// （与 `vox_core::capability` 模块头"位 = 事实，不是期望"同一条纪律）。
pub fn alsa_available() -> bool;

/// PipeWire 在 ALSA 这边铺的桥（`pipewire-alsa` 那套 pcm 定义）在不在。
///
/// **只用来发启动提示，不作能力判据**：它既不能证明 PipeWire 在跑
/// （daemon 可能没起、可能只有 Pulse），也不能证明 ALSA 打不开设备
/// （`hw:` 直开根本不走那个桥）。判定规则见 §6。
pub fn pipewire_alsa_bridge_present() -> bool;
```

`#![cfg(target_os = "linux")]` 与那句"非 Linux 上编译成空 lib"的理由，与
`crates/vox-audio-linux/src/lib.rs`（`:17` 起，2026-09-30 核）逐字同口径。

### 3.2 `probe.rs` 的纯函数（本稿最要紧的部分）

```rust
//! ALSA 探测与参数协商。**这里只放纯逻辑**：所有不碰硬件的决策都在这儿，
//! 所有碰硬件的调用（`PCM::new` / `hw_params` / `readi`）都在 capture/playback/registry 里。

use alsa::pcm::{Format, Frames};

/// 我们向采集侧要的率：降噪的原生率（`docs/platform/EMBEDDED.md` §4-8 的三处写死之一）。
pub(crate) const CAPTURE_WANT_RATE: u32 = 48_000;
/// 我们向播放侧要的率：回放协议的率（`protocol.rs::OUTPUT_SAMPLE_RATE`）。
pub(crate) const PLAYBACK_WANT_RATE: u32 = 24_000;
/// 采集侧要的声道数：麦克风基本都给单声道或双声道，多声道麦克风阵列不在这一稿。
pub(crate) const CAPTURE_WANT_CHANNELS: u32 = 1;
/// 播放侧要的声道数：单声道铺成双声道给声卡（与 `vox-audio-linux` 侧同思路）。
pub(crate) const PLAYBACK_WANT_CHANNELS: u32 = 2;

/// io 之后该怎么走。**热路径上唯一的分支**，单独拆出来就是为了能脱开硬件测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// 数据到手 / 出去了，继续。
    Continue { frames: usize },
    /// 缓冲欠载（写）/ 本轮没有数据（读）：补静音或跳过，**不算错**。
    Underrun,
    /// `-EPIPE`（欠载致设备挂起）或 `-ESTRPIPE`（设备内部断了，如 USB 拔出重插）：
    /// 恢复后继续。不恢复的话任何一次爆音都会让整条流水线永久停摆。
    Recover,
    /// 别的错：这条流到此为止。
    Fatal { func: &'static str, errno: i32 },
}

/// `readi` / `writei` 的结果该怎么判读。
///
/// `requested` 是本次请求的**帧数**（不是样本数）。参数用 `&Result<..>` 而不是 `Result<..>`
/// 是为了不把 `alsa::Error` 的所有权搬进这个纯函数。
pub(crate) fn step_after(res: &Result<usize, alsa::Error>, requested: Frames) -> Step {
    match res {
        // 短读/短写 = 缓冲欠载。`Ok(0)` 落到这一支是**故意的**：
        // 它必须被当成欠载而不是"成功"，否则播放侧会在没数据时空转烧 CPU。
        Ok(frames) if *frames < requested => Step::Underrun,
        Ok(frames) => Step::Continue { frames: *frames },
        Err(e) => match e.errno() {
            libc::EPIPE | libc::ESTRPIPE => Step::Recover,
            errno => Step::Fatal {
                func: e.func(),
                errno,
            },
        },
    }
}
```

`Frames` 用 `alsa::pcm::Frames`（= `alsa::snd_pcm_sframes_t` = `isize`，
`alsa-0.12.1/src/pcm.rs:65`，2026-09-30 核）。上面 `readi` / `writei` 返回的是**帧数**不是样本数
（`alsa-0.12.1/src/pcm.rs::IO::readi:420-421` 的文档"Multiply with number of channels"，
2026-09-30 核）——这是最容易写错的一处，单测里专门钉它。

```rust
/// 采样格式阶梯：先要最好的（S32_LE），实在不行退 S16_LE。
///
/// 32 位不是为了音质（我们是 f32 进 f32 出），是为了**采集侧的余量**：
/// 廉价 USB 声卡常把麦克风增益拉得很高，16 位很容易削顶，而削顶发生在
/// 驱动里，我们拿回来就只剩 0 了。
pub(crate) fn format_ladder() -> Vec<Format> {
    let mut out = vec![Format::s32()];
    if Format::s16() != Format::s32() {
        out.push(Format::s16());
    }
    out
}

/// 协商阶梯里的一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// 设备直接给这个率：**不重采样**（S4 §2.3"能不重采样就不重采样"的字面兑现）。
    Exact,
    /// 设备只能给别的率，但 `plughw:` 的 plug 插件能替我们换率：
    /// 我们仍然按 `want` 推给设备，Vox 侧**不装重采样器**。
    PlugConvert,
    /// 设备只能给别的率且不走 plug：在 `hw:` 上按 `probe_rate` 试探，
    /// 装 Vox 侧重采样器（`ResampleFactory` 注入的那份）。
    VoxResample { probe_rate: u32 },
}

/// 播放侧的率协商阶梯，按 `wanted` 生成。**全灭时调用方必须报错，不许悄悄换一个率**
/// （与 `crates/vox-audio-win/src/rates.rs::choose_output_rate` 的"退回设备默认率"相反：
/// 那份退下去等于替用户做了一个没要求的重采样，这里报出来让用户知道）。
pub(crate) fn rate_ladder(wanted: u32) -> Vec<Attempt> {
    vec![
        Attempt::Exact,
        Attempt::PlugConvert,
        Attempt::VoxResample {
            probe_rate: wanted,
        },
    ]
}

/// f32 → 设备整数样本。**显式夹紧**，不靠 Rust 的 float→int 饱和转换。
///
/// `i32::MAX`（2147483647）而不是 `2147483648` 当满量程：`-1.0` 映射到 `-2147483647`
/// 而非 `-2147483648`，换来的是 `0.0` 有一个精确的映射，两边不不对称。
pub(crate) const I32_SCALE: f32 = 2_147_483_647.0;

pub(crate) fn f32_to_i32(x: f32) -> i32 {
    // NaN 落在这里：`clamp` 对 NaN 返回 NaN，转整数是 0。NaN 进设备等于噪声，
    // 显式归零比让下游猜要好。
    let scaled = (x * I32_SCALE).clamp(-I32_SCALE, I32_SCALE);
    if scaled.is_nan() {
        0
    } else {
        scaled as i32
    }
}

/// 设备整数样本 → f32。**这是采集侧唯一的换算**。
pub(crate) fn i32_to_f32(x: i32) -> f32 {
    x as f32 * (1.0 / I32_SCALE)
}

/// 一块该喂给 `Blocker` 的音频有多少个交织样本。
pub(crate) fn block_frames(rate: u32, block_ms: u32, channels: u16) -> usize {
    let frames = ((rate as u64 * block_ms as u64) / 1000).max(1) as usize;
    frames * channels.max(1) as usize
}

/// `alsa::Error` → 中文 `PortError`。**带函数名和 errno**：无屏档没有界面，
/// 运维只看得到 journal，报错必须自解释。
pub(crate) fn map_err(what: &str, e: alsa::Error) -> PortError {
    PortError::new(format!("ALSA {what} 失败：{}（{}，errno {}）", e, e.func(), e.errno()))
}
```

### 3.3 `capture.rs` 目标形状

```rust
pub struct AlsaCapture {
    running: Option<Running>,
}

struct Running {
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

struct Shared {
    stop: AtomicBool,
    /// 启动回报通道。协商结果、启动错误都从这儿出去；谁先拿到谁负责回报。
    report: Mutex<Option<mpsc::Sender<PortResult<CaptureFormat>>>>,
}

impl AlsaCapture {
    pub fn new() -> Self { ... }
}
```

`CaptureSource` 实现（骨架，`impl` 内每个分支都要对上 §1.1 的契约编号）：

```rust
impl CaptureSource for AlsaCapture {
    fn start(&mut self, target: &CaptureTarget, block_ms: u32,
             on_chunk: Box<dyn FnMut(AudioChunk) + Send>) -> PortResult<CaptureFormat> {
        self.stop();                                   // C1.1 重复 start 视为换目标
        let plan = resolve_plan(target)?;              // §5.5
        let (report_tx, report_rx) = mpsc::channel();
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), report: Mutex::new(Some(report_tx)) });
        let block_ms_shared = block_ms;
        let thread = std::thread::Builder::new()
            .name("vox-capture".into())
            .spawn(move || capture_thread(plan, block_ms_shared, Arc::clone(&shared), on_chunk))
            .map_err(|e| PortError::new(format!("创建采集线程失败：{e}")))?;
        self.running = Some(Running { shared: Arc::clone(&shared), thread });
        // C3 有界等待，超时 = 先 stop 再报错
        match report_rx.recv_timeout(START_TIMEOUT) { /* 同 vox-audio-linux/src/capture.rs:123-140 */ }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.shared.stop.store(true, Ordering::Release);
            // C4 有界等待：2 s 上限，超时只 warn 不 join。
            // 为什么 2 s 够：采集线程最坏是卡在 `alsa::poll` 的 200 ms 超时上（§5.3）。
            let deadline = Instant::now() + STOP_TIMEOUT;
            while !running.thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if running.thread.is_finished() { let _ = running.thread.join(); }
            else { tracing::warn!("采集线程没在 2 秒内退出，先放它自生自灭（不再 join）"); }
        }
    }
}
```

### 3.4 `playback.rs` 目标形状

```rust
pub struct AlsaPlayback {
    resample_factory: ResampleFactory,
    source_rate: u32,
    target_rate: u32,
    channels: u16,
    resampler: Option<Box<dyn Resample>>,
    /// 重采样 + 铺声道的复用缓冲（流水线线程用，允许分配但复用）。
    interleave: Vec<f32>,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    ring: DropRing,
    stop: AtomicBool,
    rendered_samples: AtomicU64,
    device_latency_ms: AtomicU64,
}
```

字段与 `crates/vox-audio-linux/src/playback.rs::LinuxPlayback`（`:44-55`，2026-09-30 核）**同形**，
差别只有两处（都要在模块头注释里写明）：

1. `LinuxPlayback` 固定请求 48 kHz / 2 ch（`playback.rs:330`，2026-09-30 核）；ALSA 侧请求值来自
   §5.2 的阶梯结果，`target_rate` 是真协商出来的。
2. `LinuxPlayback` 的 `process` 回调拿 `stream.time().delay()` 报延迟（`:311-320`，2026-09-30 核）；
   ALSA 侧在 `writei` 循环里用 `PCM::delay()`（`snd_pcm_delay`，`alsa-0.12.1/src/pcm.rs::PCM::delay:218`，
   2026-09-30 核）刷新同一个原子量。

`push` / `stats` / `flush` / `close` 的形状照抄 `vox-audio-linux/src/playback.rs:130-197`（2026-09-30 核），
只把 `if self.source_rate == self.target_rate { 直通 }` 这一支保留（§5.2 阶梯第 1 档命中时就是它）。

### 3.5 `registry.rs` 目标形状

```rust
pub struct AlsaDeviceRegistry;

/// 一条枚举结果。`addr` 是 `hw:CARD=<name>,DEV=<n>` 背后的 `(card index, device)`，
/// 用来和 `default` PCM 对号（§5.4）。报给上层的 `id` 用**字符串**而不是 (index, device)，
/// 因为 card index 在插拔后会变、card name 不会。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceEntry {
    /// 交给 `PCM::new` 的名字（`hw:CARD=PCH,DEV=0` / `plughw:...` / `default`）。
    pub(crate) id: String,
    /// 给人看的名字（hint 的 `DESC`，或 card 的 longname）。
    pub(crate) label: String,
    /// `info()` 里读回来的硬件地址；纯逻辑设备（`default` / `pluggw`）是 `None`。
    pub(crate) addr: Option<(i32, u32)>,
}

/// 枚举出来的设备 + `default` PCM 实际指向的那个硬件地址 → 带 `is_default` 标记的列表。
///
/// **纯函数**：`resolved` 是"打开 `default` PCM 后 `info()` 读回来的 (card, device)"，
/// 拿不到就传 `None`。拿不到时的口径：谁都别标默认（列表里没有 `*`），
/// 由 `startup_notes` 记一条提示——宁可没有星号，也不要指错设备。
pub(crate) fn apply_default(mut devices: Vec<DeviceEntry>, resolved: Option<(i32, u32)>) -> Vec<DeviceEntry>

/// 一条 hint 该不该进列表。**纯函数**，过滤表（§5.4）在这里。
///
/// 参数就是 `alsa::device_name::Hint` 的三个可空字段，不引 alsa 类型进来，
/// 这样测试不用构造 `Hint`（它只能从 `HintIter` 拿）。
pub(crate) fn keep_hint(name: &str, direction: Option<Direction>) -> bool

/// 排序：默认设备第一，其余按 `label` 的小写字典序（§5.4 末段）。
pub(crate) fn sort_entries(entries: &mut Vec<DeviceEntry>)
```

`DeviceRegistry` 三个方法的形状见 §5.6。

---

## 4. 线程模型与热路径（RULES #6）

### 4.1 一条流一个线程

| 流 | 线程名 | 跑什么 |
| --- | --- | --- |
| 采集 | `vox-capture` | 建 PCM → `hw_params` → `start()` → `avail_update` + `poll` + `readi` 循环 |
| 播放 | `vox-playback` | 建 PCM → `hw_params` → `writei` 循环 |

两条流**不共享线程**，与 `vox-audio-linux` 一致（`capture_thread` / `stream_thread` 各自 spawn）。

### 4.2 采集循环（伪码，形状照 alsa crate 的 API）

```
pcm = PCM::new(&device, Direction::Capture, /* nonblock = */ true)   // 非阻塞打开
hwp = HwParams::any(&pcm)?
     .set_access(Access::RWInterleaved)?
     .set_channels(channels)?          // 由协商阶梯定
     .set_rate(rate, ValueOr::Nearest)?
     .set_format(format)?
pcm.hw_params(&hwp)?
let io = pcm.io_i32()? 或 io_i16()?
pcm.start()?
report_started(CaptureFormat { sample_rate: hwp.get_rate()?, channels: hwp.get_channels()? as u16 })

scratch_i32 = vec![0i32; period_frames * channels]      // 预分配，之后不再增长
scratch_f32 = vec![0.0f32; period_frames * channels]
blocker     = Blocker::new(rate, channels, block_ms)
pfd         = pcm.poll_descriptors()                     // Vec<pollfd>，循环外分配一次

loop {
    if shared.stop.load(Acquire) { break }               // 热路径唯一的状态读
    match pcm.avail_update() {
        Ok(avail) if avail >= period_frames => {
            let frames = step_after(&io.readi(&mut scratch_i32), period_frames);
            match frames {
                Step::Continue { frames } => {
                    for (dst, src) in scratch_f32.iter_mut().zip(&scratch_i32[..frames * channels]) {
                        *dst = i32_to_f32(*src);
                    }
                    blocker.feed(&scratch_f32[..frames * channels], &mut *on_chunk);
                }
                Step::Underrun => { /* 短读：这一轮什么都不做，接着 poll */ }
                Step::Recover   => { let _ = pcm.recover(EPIPE_or_ESTRPIPE, true); let _ = pcm.start(); }
                Step::Fatal { func, errno } => { report_failed(...); return; }
            }
        }
        Ok(_) => {
            // 数据还没到。poll 超时 200 ms —— 这是 stop 延迟的上界，也是它选 200 的原因。
            alsa::poll::poll(&mut pfd[..], 200)?;
        }
        Err(e) => { /* 同上，按 step_after 判 */ }
    }
}
```

`io.readi` 返回的是**帧数**；`scratch_i32[..frames * channels]` 的长度换算是本文件最容易写错的地方，
单测 `step_after` 旁边那条 `block_frames` 就是钉它的。

### 4.3 播放循环

```
loop {
    if shared.stop.load(Acquire) { break }
    // 欠载补静音：ring 里没数据就写 0（不报错，语音流断一下比崩掉强）
    let want = period_frames * channels;
    let read = shared.ring.read_into(&mut scratch_f32[..want]);   // 不够长度的部分由 DropRing::read_into 留 0
    for (dst, src) in scratch_i32.iter_mut().zip(&scratch_f32[..want]) { *dst = f32_to_i32(*src); }
    match step_after(&io.writei(&scratch_i32[..want]), period_frames) {
        Step::Continue { frames } => {
            shared.rendered_samples.fetch_add(frames as u64 * channels as u64, Release);
            if let Ok(delay) = pcm.delay() {
                let ms = (delay.max(0) as u64).saturating_mul(1000) / rate.max(1) as u64;
                shared.device_latency_ms.store(ms, Release);
            }
        }
        Step::Underrun => { /* 欠载：下次多等一轮 poll，别空转 */ }
        Step::Recover   => { let _ = pcm.recover(EPIPE, true); let _ = pcm.start(); }
        Step::Fatal { .. } => { tracing::error!(...); return; }
    }
}
```

**播放侧也要 poll**：非阻塞 PCM 下 `writei` 在 `avail` 不够时返回 `EAGAIN`，
所以每轮之前先 `pcm.avail_update()`，不够就 `alsa::poll::poll(..., 200)`。
不用 poll 而直接 `writei` 会撞上 `Err(EAGAIN)` —— 那条在 `step_after` 里是 `Fatal`（`EAGAIN` 不是
`EPIPE`/`ESTRPIPE`），流会当场死掉。这是本稿里最容易踩的坑，`step_after` 的设计已经把
"`Ok(0)` / 短写 = 欠载"和"`EAGAIN` = 真错"这两件事分开，就是为了逼实现者在循环里先 poll。

> **决策**：把 `EAGAIN` 也算进 `Recover` 还是留在 `Fatal`？**留在 `Fatal`**——理由见上，
> 留着它当"你忘了 poll"的信号比当成可恢复错误有用。测试里钉死这一点。

### 4.4 热路径零新增分配：逐条账

`.omp/RULES.md` #6 的口径在 `.omp/AGENTS.md` 里写的是"音频回调、DSP 每帧、渲染每帧里不许新增
`Vec`/`clone`/格式化"。ALSA 这一层**我们能控制的**部分：

| 位置 | 分配 | 怎么保证 |
| --- | --- | --- |
| `open` / `start` 里 | `scratch_i32` / `scratch_f32` / `pfd` / `interleave` / `blocker.staging` | **协商结果一出来就一次性 `vec![]` 定死大小**，之后只 `readi(&mut …)` / `writei(&…)`，容量只读 |
| 采集循环 | 换算 `for (dst, src) in …` | 无分配；`iter_mut().zip()` 拿两个已分配切片的迭代器，不新建容器 |
| 采集循环 → `on_chunk` | `AudioChunk` 内部那个 `Vec<f32>` + `Blocker::feed` 的 `split_off`/`mem::replace` | **这不是我们新增的**：`AudioChunk { samples: Vec<f32> }` 是 `ports.rs` 定的端口形状（C6），`Blocker` 是两个既有平台共用的 `vox-dsp` 代码（`crates/vox-dsp/src/chunk.rs:42-54`）。WASAPI 与 PipeWire 两侧**每一次 `on_chunk` 都在这里分配**。ALSA 侧做到"一次都不比它们多"即达标；真要归零是 S4-B 算子链的事（§8 未决 6） |
| 播放循环 | `ring.read_into` / `writei` | `DropRing::read_into` 是原地写调用方给的切片（`crates/vox-dsp/src/ring.rs:104`），不新建 |
| 播放循环 | 原子量加 | `fetch_add`，无分配 |
| 播放循环 | `tracing::` | **一律不打**。日志只在 `push`（流水线线程）和致命错误路径上打，与两个既有实现同纪律 |
| `push`（流水线线程，非设备线程） | 重采样器内部 + `duplicate_mono` 写进复用的 `interleave` | 照抄 `vox-audio-linux/src/playback.rs::push`（`:130-157`，2026-09-30 核）：`self.interleave.clear()` 后重填，不新建 `Vec` |
| 错误路径 | `format!` | **错误不是热路径**。但频次要有节制：`Step::Fatal` 直接退出循环，不会反复触发 |

**验收口径**：不写基准（本轮没有板子，测出来的数没有可比意义，`docs/plans/S4-EMBEDDED-REFACTOR.md` §5），
改为**代码形状的钉子**——`AlsaCapture` / `AlsaPlayback` 的构造与循环里除了上表列出的那几处，
不许出现 `vec![]` / `Vec::with_capacity` / `format!` / `.to_string()` / `.clone()`；
`clippy` 之外靠人工复核 + 一条"缓冲只增不减"的结构性说明。

---

## 5. 逐项设计

### 5.1 设备名与 `DeviceInfo.name`（C12 的实现约束）

`DeviceInfo.name` 必须是能直接喂给 `PCM::new` 的名字（§1.2 最后一条），所以报的是
hint 给的**原生 PCM 名**，不是我们自己编的短 id。给人看的另开一条：

```rust
DeviceInfo { name: entry.id.clone(), is_default: ... }   // id = "hw:CARD=PCH,DEV=0"
```

代价：界面上会看到 `hw:CARD=PCH,DEV=0` 这种串。两边接受这个代价——`vox-audio-linux` 那边是
`node.description`（`crates/vox-audio-linux/src/registry.rs::label_of:87-93`，2026-09-30 核），
看起来好看但 `DeviceInfo` 没有第二个字段装 label，而 `settings.input_device` / `output_device`
存的就是这一个字符串（§1.2）。**要好看只能另加字段，那是 vox-core 的接口变更，不在本稿范围**
（§8 未决 4）。

### 5.2 格式与采样率（§2.3"能不重采样就不重采样"）

**采集侧**：想要 48 kHz（降噪原生率）。`CaptureFormat` 回报的是**真协商到的率**，芯自己会重采样到 16 kHz
（`protocol.rs::INPUT_SAMPLE_RATE`），所以采集侧不需要"要不要重采样"这个决定：

| 档 | 设备 | 条件 | 回报 |
| --- | --- | --- | --- |
| 1 | `hw:<id>` | `set_rate(48000, Nearest)` 成功且拿回的率就是 48000 | 48000 |
| 2 | `plughw:<id>` | 同上，但由 plug 插件换率 | 48000 |
| 3 | `hw:<id>` | `set_rate(48000, Nearest)` 拿回的率 ≠ 48000 | 拿回的率（48000 附近），交给芯重采样 |

阶梯跑完还没有 → **报错**，不静默换默认率。

**播放侧**：内核推 24 kHz 单声道（§1.2），`open` 要回报**实际**设备率（C8），
所以这里必须显式决定"谁来重采样"。阶梯（`rate_ladder`，§3.2）：

| 档 | `Attempt` | 怎么开 | `open` 回报 | 装 `Resample`？ |
| --- | --- | --- | --- | --- |
| 1 | `Exact` | `hw:<id>` @ 24000 整 | 24000 | **不装**（`push` 走 `source_rate == target_rate` 直通，与 `vox-audio-linux/src/playback.rs:139-140` 同分支） |
| 2 | `PlugConvert` | `plughw:<id>` @ 24000 整 | 24000 | **不装**（plug 插件在设备侧换率） |
| 3 | `VoxResample { probe_rate: 24000 }` | `hw:<id>` @ `set_rate_near(24000, Nearest)`，拿回什么率就是什么率 | 拿回的率 | **装**（`ResampleFactory` 注入的那份） |

三档全灭 → 报错。

**为什么档 2 在档 3 前面**（plug 插件的率转换是线性插值，音质一般，为什么还排在 Vox 自己的 sinc 前面）：

- S4 §2.3 的设计预算是 Cortex-A53 四核 1 GHz / 512 MB，且"算子分档、都能关"。当前唯一一档重采样
  是 `crates/vox-dsp/src/resample.rs` 的 `SincFixedIn`（128 点 sinc，`docs/platform/EMBEDDED.md` §4-8 记着
  "重采样只有贵的 sinc 那一档"）。让它在**每一句话**上都跑，比让 alsa-lib 的线性插值跑一遍更贵。
- plug 的线性插值音质差，但 24 kHz 单声道 → 48 kHz 双声道这条路上，它带来的失真落在译音的回放侧，
  而译音本来就还要经过云端那次编码往返。
- **代价要写进日志**：命中档 2 时，`startup_notes` 记一条"播放走的是 alsa-lib 的线性插值换率"，
  别让它变成一件没人知道的事。

**与 Windows 侧的口径差异**：`crates/vox-audio-win/src/rates.rs::choose_output_rate`（`:22-46`，2026-09-30 核）
的探测顺序是 设备默认 → 24000 → 48000 → 44100，**全灭时退回设备默认率**。
ALSA 侧我们**故意不退**：ALSA 这条阶梯的第 3 档必然会拿到一个率（`set_rate_near` 没有"全灭"这个状态），
所以"退回默认率"在 ALSA 上等于"替用户做了一个他没要求的重采样"，而 Windows 那份退下去时
`Initialize` 大概率会失败并报错（它自己的注释就是这么写的）。**这是有意的分歧，不是抄漏了**，
在 `playback.rs` 模块头注明。

### 5.3 打开方式：非阻塞 + `poll`

`PCM::new(name, dir, nonblock)`（`alsa-0.12.1/src/pcm.rs::PCM::new:153`，2026-09-30 核）的第三个参数
**取 `true`**。理由：`snd_pcm_readi` / `snd_pcm_writei` 在阻塞模式下会一直睡，
Rust 侧的 `stop` flag 唤醒不了它，唯一能停的办法是从另一个线程对同一个 PCM 调
`snd_pcm_drop` / `snd_pcm_close`——那是跨线程操作同一个句柄，FFI 层的竞态，本稿不碰。

非阻塞下的循环靠 `PCM::avail_update()`（`:211`，2026-09-30 核）+ `PCM::poll_descriptors()`
（`alsa-0.12.1/src/pcm.rs:367`，实现 `alsa::poll::Descriptors`，2026-09-30 核）+ `alsa::poll::poll(&mut fds, timeout_ms)`
（`alsa-0.12.1/src/poll.rs:44`，2026-09-30 核）。

**`POLLIN` / `POLLOUT` 方向**：`poll_descriptors()` 返回的 `pollfd` 已经带了正确的 `events`
（alsa-lib 自己填的），我们只改 `timeout`；**不要手工改 `events` 方向**——采集是 `POLLIN`、
播放是 `POLLOUT`，方向搞反的表现是"永远超时"或"永远立刻返回"，两者都很难查。

**`poll` 超时 = 200 ms**：这是 `stop()` 从设 flag 到线程真正退出的上界（§4.2）。
既有实现的 `stop` 上限是 2 s（§1.1 C4），200 ms 留了 10 倍余量。
`[未决]` 更小的值（如 50 ms）能缩短停机延迟但要付出更多 `poll` 系统调用；这一稿取 200 ms，
等有了板子能测停机延迟再调（§8 未决 5）。

**`sw_params` 这一稿不碰**：不设 `avail_min`、不设 `start_threshold`。
默认 `avail_min` = 1（一个 period），够用；`start_threshold` 的精细设置在 `plughw` 下由插件自己管。
**理由**：这是能调但不影响正确性的旋钮，`[未核实]` 目标板上的最优值，先不动。

### 5.4 设备目录：枚举方式与 id 稳定性

**枚举**：`alsa::device_name::HintIter::new(None, c"pcm")`（`alsa-0.12.1/src/device_name.rs:36-41`，2026-09-30 核），
每次 `Hint` 给 `name` / `desc` / `direction`（`device_name.rs::Hint:63-67`，`direction` 由 `IOID`
字符串映射到 `Direction`，`:77-82`，2026-09-30 核）。

过滤规则：

| hint 名 | 收不收 | 理由 |
| --- | --- | --- |
| `hw:*` | ✅ | 直通驱动的实体设备，**报进列表** |
| `plughw:*` | ❌ | 同一张卡的另一种打开方式，会让列表里出现两张一样的卡 |
| `default` | ✅ | 系统缺省。**标 `is_default`**（见下） |
| `front:*` / `surround51:*` / `dsnoop:*` 等 | ❌ | 同一张卡的引脚别名 / 混音节点，不是"一张设备"。收进来会让用户选到"某个声道对" |
| `pulse` / `pipewire*` / `jack` | ❌ | **显式排除**：`default` 走不走服务层取决于 `alsa.conf`，但 `pulse` / `pipewire-*` 这些名字是明确的"经服务层"。列进来会让无屏档出现两个都叫"麦克风"的条目。排除它们是 §6 那条结论的直接落实 |
| `null` | ❌ | 空设备，列出来是噪声 |

**card index ↔ card name 的映射**：`hw:CARD=PCH,DEV=0` 里的 `PCH` 是 **card name**（udev 属性派生，
声卡不换插槽就不变），而 `hw:0,0` 里的 `0` 是 **card index**（插拔后会变）。
→ **我们报的 id 一律是 `hw:CARD=<name>,DEV=<n>` 形式**，因为它是唯一能直接喂回 `PCM::new` 又跨插拔稳定的写法。
把 `Card::iter()`（`alsa-0.12.1/src/card.rs:17-31`，2026-09-30 核）的 index → `get_name()`（`:37`）
做成一张表，就能把 hint 名里的 `CARD=` 段反查回 index。这一张表**只在枚举时构造一次**（低频动作，§1.1 C12）。

**`is_default` 怎么定**（这是本节最需要说清的一处）：

`default` 这个 PCM 的真实指向写在 `/usr/share/alsa/alsa.conf` 里，**从名字看不出来**。
去解析那个文件太脆（发行版各有各的 include 链）。所以走 alsa-lib：

1. 打开 `default` PCM（对应方向），**不做任何 `hw_params`**；
2. 调 `PCM::info()`（`alsa-0.12.1/src/pcm.rs::PCM::info:320`，2026-09-30 核）拿
   `Info::get_card() -> i32`（`:77`）与 `Info::get_device() -> u32`（`:81`）；
3. 和枚举结果里每条的 `addr`（同法取）比对，命中那条标 `is_default`；
4. `info()` 报错、或没有任何一条命中 → **谁都别标**，调用方记一条 `startup_notes`。

`apply_default`（§3.5）就是第 3 步的纯函数形态，`resolved` 为 `None` 时全表无星号——
**宁可没有星号，也不要指错设备**（指错的后果是用户没设设备时译文播到麦上，形成回声串扰）。

**排序**：`is_default` 的排第一，其余按 `label` 的小写字典序。与
`crates/vox-audio-linux/src/registry.rs::devices_of_class`（`:71-84`，2026-09-30 核）保持一致
（那边不排序，靠 PipeWire 图的顺序；ALSA 的 hint 顺序不保证，所以我们要自己排）。

**`input_devices` 与 `output_devices` 的差异**：用 `Hint::direction` 分流（`Capture` → 输入，
`Playback` → 输出）。`direction` 是 `None` 的 hint（有些驱动不填 `IOID`）**两边都不列**——
宁可少列，不要把输出设备列成麦克风。

### 5.5 `CaptureTarget` 的三条不支持路径

```rust
fn resolve_plan(target: &CaptureTarget) -> PortResult<Plan> {
    match target {
        CaptureTarget::Microphone(name) => {
            // 给了名字就必须认它，不认就报错——不许悄悄抓默认源（那是静默换源，
            // 与 ports.rs::CaptureTarget::Net 的注释同一条纪律）。
            // 校验方式：在 registry 的枚举结果里按 id 精确匹配；
            // 匹配不上报「找不到输入设备「{name}」」。
            Ok(Plan::Microphone(name.clone()))
        }
        CaptureTarget::ProcessLoopback { .. } => Err(PortError::new(
            "ALSA 没有「抓某个程序的声音」这个能力（它不记录谁在出声）。\
             这一格要按程序抓音请用 PipeWire 后端。",
        )),
        CaptureTarget::Net { .. } => Err(PortError::new(
            "网络音频不归 ALSA 采集实现（由 vox-net 的媒体面提供）。",   // C7，逐字照 ports.rs 的口径
        )),
    }
}
```

第二条的错误文案是**能力位降级在端口层的落点**：无屏档 `program_tap` 在上限之外（§1.5），
用户真去选了那一格时，这里给出的是一句能看懂的话，而不是 `unsupported` 或者一个空列表。

### 5.6 `DeviceRegistry` 三条方法

| 方法 | 行为 | 依据 |
| --- | --- | --- |
| `input_devices` | `HintIter` + §5.4 过滤 + `direction == Capture` + `apply_default` | C12 |
| `output_devices` | 同上，`direction == Playback` | C12 |
| `audio_apps` | **返回 `Ok(vec![])`**，不报错 | C13。ALSA 没有"谁在出声"的概念，报错会让界面把这一格当故障显示；返回空列表 = "这一档没有这一格"，与 `host_ceiling(LinuxHeadless)` 不含 `program_tap`（§1.5）口径一致 |
| `virtual_cable_installed` | **返回 `false`** | C14。`ports.rs` 的文档已经写死"Linux 上压根没有'装'这一步，这条恒为 `false`"——ALSA 是这句话的加强版（连虚拟麦都没有） |

`DeviceRegistry: Send + Sync`（C12）：`AlsaDeviceRegistry` 是**零字段**的（与
`vox-audio-linux::LinuxDeviceRegistry` 同形，`registry.rs:25`，2026-09-30 核），
所有状态都是每次调用现构造的局部值 → 天然满足。

### 5.7 `Blocker` 与协商结果的一致性（C5）

`Blocker::new(sample_rate, channels, block_ms)` 里的 `sample_rate` 必须是**协商到的率**，
不是请求的率（阶梯档 3 下两者不等）。传错的后果很隐蔽：块长算错，
`AudioChunk.sample_rate` 报的是错的率，**芯里的重采样器会按错的比率工作**，译音变调。

钉法：`block_frames(rate, block_ms, channels)`（§3.2）与
`vox_dsp::chunk::Blocker::new(rate, channels, block_ms).frames_per_block() * channels`
在若干组 `(rate, block_ms, channels)` 上必须逐字相等——单测里直接对比，不复制公式。

### 5.8 探测原语（`alsa_available` / `pipewire_alsa_bridge_present`）

```rust
/// ALSA 在不在 = 能不能打开一条 PCM。
pub fn alsa_available() -> bool {
    // 试采集方向 `default`，开完立刻丢。失败返回 false。
    // **不缓存**：USB 声卡随时可能被拔。
    probe::alsa_available()
}
```

选"打开一条 PCM"而不是"读 `/proc/asound`"或"查 `/dev/snd`"：前者是**端到端的真事实**
（权限、驱动、`/dev/snd` 可达性全都在里面），后两者只是必要条件。
代价是每次调用会做一次真实的 open/close——装配期调一次 + 可能 30 s 轮询一次（§1.1 C12），
开销可忽略。

`pipewire_alsa_bridge_present()`：**只读文件系统**——
检查 `/usr/share/alsa/alsa.conf` 及 `/etc/alsa/conf.d/` 下有没有 PipeWire 铺的
`pcm.*.pipewire.*` 定义。**它不作任何判据**，只用来在 `startup_notes` 里说一句
"检测到 PipeWire 的 ALSA 桥，当前用的是 ALSA 直开"。理由见 §6。

---

## 6. §6 第 5 问的答复：ALSA 与 PipeWire 同时存在时

> 原文（`docs/plans/S4-EMBEDDED-REFACTOR.md:165`）："ALSA 与 PipeWire 同时存在时，缺省选谁、能力位怎么报。"

### 6.1 事实前提

1. PipeWire 在 ALSA 这边铺了一层桥：`/usr/share/alsa/alsa.conf` 会被改写，让 `default` / `pulse`
   指向服务层的 PCM。同时 PipeWire 的 `module-alsa-card` 系列**自己也会打开硬件设备**。
   → 在一台跑着 PipeWire 的机器上，`hw:CARD=X,DEV=0` **仍然能开**，但那已经是"和 PipeWire 抢同一张卡"。
2. 抢的结果是什么，**分设备、分驱动**：`dmesg`/驱动是否支持共享、是否走 `dmix`、PipeWire 是否已持有。
   **`[未核实]`：本轮没有验证过"PipeWire 跑着时 ALSA 直开某张具体声卡会不会 `EBUSY`。**
   这条要在第一台板子上实测（`alsa::Error::errno()` 会告诉我们是 `EBUSY` 还是别的）。
3. 另一条确定的坏处：`default` 在 PipeWire 系统上**会经服务层**（先被 PipeWire 重采样一次，
   我们再重采样一次），所以**在 PipeWire 机器上用 `default` PCM 是双重路由**，音质与延迟都白扔。
   → §5.4 的过滤表把 `pulse` / `pipewire*` 排除、但保留 `default` 作为一条，**只有在 §5.4 第 4 步
   对号成功时才给它星号**；对不上就不标（宁可没星号）。

### 6.2 结论一：缺省选谁

> **无屏档（`HostKind::LinuxHeadless`）硬缺省 ALSA，不做运行时探测切换。
> 桌面档（`HostKind::LinuxDesktop`）保持 PipeWire，不动。
> "同时存在"时不是"探测谁在、选谁"，而是**按档位定、按用户/配置显式覆盖**。**

三条理由：

1. **无屏档里 PipeWire 没有任何增量**（这是最硬的一条，来自代码不是判断）：
   `host_ceiling(HostKind::LinuxHeadless)` 只有 `Mic` + `BackgroundService`（§1.5）。
   需要 PipeWire 的 `ProgramTap` 与 `VirtualMic` **在上限之外**，芯直接算 `false(unsupported)`。
   也就是说：**在这档里，"PipeWire 在不在"一位能力位都不影响。**
2. **已拍板**：`docs/architecture/DIRECTIONS.md` §10.9 第 5 条"音频后端：**ALSA 为嵌入式缺省**，
   PipeWire 可选"（2026-09-26），§8 第 17 行同款裁决。S4 §2.3 同款。
3. **运行时探测切换 = 两个真源**，与 `crates/vox-core/src/capability.rs` 模块头的
   "**位 = 事实，不是期望**"和"位为 `true` 必须有定义者"（§2.5 纪律 2）打架：
   "后端是谁"如果变成一个运行期才定的事实，它既不进位表、也没有定义者，等于凭空多一个
   没有判据的状态。**不引入。**

**缺省值怎么落到代码**（属于 `vox-host`，本稿只规定契约）：
缺省后端由 `HostKind` 派生——`LinuxHeadless → alsa`、`LinuxDesktop → pipewire`。
**这张映射表不放在本 crate 里**（本 crate 只提供一个后端，不做选择），也不放在 `vox-core`
（芯不许知道后端名，见 `.omp/AGENTS.md` 硬约束"芯里不许出现平台 API"）。
它属于 S4-A 的 `vox-host`。§7 只写本 crate 需要被满足的注入契约。

**需要覆盖的两种情况**（由 `vox-host` 承担，本 crate 只提供原语）：

| 情况 | 处理 |
| --- | --- |
| 无屏档但用户显式选了 PipeWire（比如后期给无屏盒加了虚拟麦的需求） | 装配层改用 `vox-audio-linux`。`vox-host` 需要同时链上两个音频 crate —— **这是 S4-A/S4-C 交界处的一个待排期项**（§8 未决 1） |
| 桌面档但 PipeWire 没在跑 | 既有行为不变（`vox-audio-linux` 起不来就报错），**这一稿不改**。要降级到 ALSA 是另一个提案，本稿不扩 |

### 6.3 结论二：能力位怎么报

> **一位都不动。ALSA crate 落地不改变任何 `Capability` 位，也不需要新增任何位。**

逐条：

| 位 | 报什么 | 为什么 |
| --- | --- | --- |
| `mic` | **照旧按 `LinuxHeadless` 档位上限报开**（不进 `off`） | 与 `crates/vox-headless/src/platform/linux.rs::host_facts()` 现在的一致（§1.5）。装配期没有任何否定证据——"麦克风被别的程序占着"要真去开流才发现。这条是 `DIRECTIONS.md` §10.7"已知的、有意不做的"表第一行，**不新开口径** |
| `program_tap` | `false(unsupported)`（**没变**，由上限决定） | `host_ceiling(LinuxHeadless)` 不含它（§1.5）。ALSA 也给不了它（§5.5）——**ALSA 的到来没有改变任何事** |
| `virtual_mic` | `false(unsupported)`（**没变**） | 同上。ALSA 无虚拟麦，与上限一致 |
| `captions` / `tray` / `global_hotkey` / `vr_captions` | `false(unsupported)`（没变） | 无屏盒子上没有屏幕/键盘/托盘宿主，与音频后端无关 |
| `background_service` | `false(not_wired)`（没变） | 与后端无关 |
| `net_in` / `net_out` / `file_config` | `false(unsupported)`（没变） | 没有定义者，恒假 |

**为什么不因为"ALSA 打不开设备"就翻 `mic` 位**——三条理由，一条都不能省：

1. **时点不对**。`mic` 是装配期事实，设备被占要 `start()` 时才发现（§6.3 表第一行）。
   `HostFacts` 是**装配时注入一次**的（`capability.rs::HostFacts` 的文档，`:304-319`，2026-09-30 核），
   拿不到起流结果。现有 Linux 侧也正是这么处理的。
2. **失败的兜底已经在了**。起流失败 → `PortError` → 既有链路的 `Notice`（无屏档进 journal/控制面）。
   把同一个失败在两个地方报一遍，只会让"位"和"错误"不同步。
3. **这会是新口径**。若将来真要报，`UnavailableReason::Busy` / `::Permission`（§1.5）是现成的词，
   但**谁在什么时点问、声卡被拔了怎么刷新、30 s 轮询里怎么重算**都没定。
   **`[未决]`：列为 §8 未决 2，不在这一稿开。**

**唯一新增的对外信息是"文字"，不是"位"**：`alsa_available()` 与
`pipewire_alsa_bridge_present()` 两个探测原语供 `vox-host` 生成 `startup_notes`，
形如"检测到 PipeWire 的 ALSA 桥；当前按无屏档缺省使用 ALSA 直开设备"。
按 `capability.rs` 纪律 3（`:19`，2026-09-30 核）"**芯只给 `(位, reason)`，句子不在芯里**"——
句子在 `vox-host` 的 `startup_notes` 里，与 `crates/vox-headless/src/platform/linux.rs::startup_notes`
（`:64-73`，2026-09-30 核）现在是同一个位置、同一类东西。

### 6.4 与既有决策的冲突裁决（`DIRECTIONS.md` §8「新者胜」）

| # | 被推翻的 | 依据（新的更新） | 处理 |
| --- | --- | --- | --- |
| 1 | `docs/platform/EMBEDDED.md` §3.2「音频：还是 PipeWire（外加两个"必须装"的东西）」，以及 §3.4 里"实时优先级靠 PipeWire `module-rt` + `RLIMIT_RTPRIO` / 回退 RTKit（要 D-Bus）"、"走 `systemctl --user` + `enable-linger`"两条 | `DIRECTIONS.md` §10.9 第 5 条（2026-09-26）+ §8 第 17 行 | **正文推翻**。ALSA 直开不经过 PipeWire，`enable-linger` 不是必需（虽然 `--user` 仍然是更优的部署形态，见下） |
| 2 | `docs/platform/EMBEDDED.md` §4-1「无屏 + 系统服务拿不到实时优先级 → 用 `--user` + `enable-linger`」、§4-2「PipeWire 没有会话管理器 = 声卡不会被打开」 | 同上 | ALSA 侧这两条的**成因**没有了（我们自己开设备，不需要会话管理器）。但**结论 `--user` 仍然保留**：ALSA 的设备访问同样受 logind 的 uaccess ACL 约束（`[未核实]`，§8 未决 3），而 `--system` 的 RT 预算为 0 是 systemd 的事实，与后端无关 |
| 3 | `docs/platform/EMBEDDED.md` §4-3「交叉编译音频 crate 比交叉编译芯贵」（`libspa-sys` 的 bindgen 要 clang） | 本稿 §2.1（`alsa-sys` 默认走**预生成绑定**，不引 bindgen） | **对 ALSA 不成立**。这一条现在只对 `vox-audio-linux`（PipeWire）成立，所以它不是被"推翻"，是**适用范围缩小**到桌面档 |
| 4 | `docs/platform/LINUX.md` §9.1「锚定范围 = **PipeWire 必需**」对**无屏档**的适用性 | 同 #1 | **对无屏档下调**（ALSA 够用）。**对桌面档仍然成立**——那一档的 `program_tap` / `virtual_mic` 真的需要 PipeWire |
| 5 | —— | —— | **不推翻**：`DIRECTIONS.md` §8 第 12 行"虚拟麦按平台"——ALSA 给不了虚拟麦，与"无屏档 `virtual_mic` 在上限之外"完全一致，无需改 |

**这些文档回填不属本稿范围**（本轮只允许写 `docs/plans/S4-C-ALSA.md` 一个文件）。
列为工单 **ALSA-T7**，owner `docs-scribe`，**排在 ALSA 落地并实测之后**（在实测之前回填会把
"没验证过的说法"写进文档，违反 `EMBEDDED.md` §7 的标注口径）。

---

## 7. 接线说明（**依赖 S4-A，施工排在 S4-A 之后**）

> ⚠️ 本节**不设计 `vox-host`**，只写"本 crate 需要被满足的注入契约"。S4-A 的另一路设计稿是权威。

### 7.1 本 crate 对注入方提出的三个要求

| # | 要求 | 依据 |
| --- | --- | --- |
| R1 | 能提供一个**同时链上 `vox-audio-alsa` 与 `vox-audio-linux`** 的构建（Linux 才需要），以便按档位/用户选择挑一个 | §6.2 的"显式覆盖"场景。两个 crate 都是 `#![cfg(target_os = "linux")]`，同时链接没问题 |
| R2 | 能注入 `ResampleFactory`（`crates/vox-core/src/pipeline/mod.rs::ResampleFactory`，`:75`，2026-09-30 核）给 `AlsaPlayback::new` | 与 `vox-audio-linux::LinuxPlayback::new(resample_factory)` 同一形状（`crates/vox-audio-linux/src/playback.rs:58`，2026-09-30 核） |
| R3 | 能生成 `startup_notes`（消费 `alsa_available()` / `pipewire_alsa_bridge_present()` 两个原语） | §6.3 末段 |

### 7.2 装配层现在长什么样、S4-A 之后会变成什么样

**现在**（`crates/vox-headless/src/platform/mod.rs::Platform`，`:20-24`，2026-09-30 核）：
`vox-headless` 自己的 `Platform { capture: CaptureFactory, playback: PlaybackFactory, registry: Arc<dyn DeviceRegistry> }`，
Linux 实现在 `crates/vox-headless/src/platform/linux.rs::platform()`（`:27-41`，2026-09-30 核）。

**S4-A 之后**：`vox-host` 收走装配层，这三个工厂与 `host_facts()` 由 `vox-host` 的平台注入点提供，
`vox-headless` / `app/src-tauri` 退成薄入口。

**S4-A + 本稿之后的 Linux 无屏档注入点（示意，本稿不实现）**：

```rust
// vox-host 的平台注入点（形状待 S4-A 定稿，这里只写本 crate 要求的部分）
Platform {
    // 缺省 ALSA（S4-C §6.2）；用户显式选了 PipeWire 时换这一行。
    capture:  Box::new(|| Box::new(vox_audio_alsa::AlsaCapture::new())),
    playback: Box::new({
        let resample = /* vox-host 的 resample_factory（R2） */;
        Box::new(vox_audio_alsa::AlsaPlayback::new(resample))
    }),
    registry: Arc::new(vox_audio_alsa::AlsaDeviceRegistry::new()),
}
// startup_notes（R3）：追加
//   if !vox_audio_alsa::alsa_available() { "打不开 ALSA 设备：..." }
//   if vox_audio_alsa::pipewire_alsa_bridge_present() { "检测到 PipeWire 的 ALSA 桥；当前按无屏档缺省使用 ALSA 直开设备" }
```

### 7.3 排在 S4-A 之后的施工顺序

```
S4-A（vox-host 落地，行为不变）
  └─→ ALSA-T1..T6（本稿，不碰装配层，可与 S4-A 并行）
        └─→ ALSA-T7（接线：vox-host 的 Linux 注入点改用 ALSA 缺省 + 桌面档保持 PipeWire）
              └─→ 板子实测 → ALSA-T8（文档回填）
```

**本稿（ALSA-T1..T6）不依赖 S4-A**：新 crate 独立编译、独立测试，不改任何现有 crate 的行为。
**只有 ALSA-T7 依赖 S4-A**。

---

## 8. 改动清单

### 8.1 新增文件（全部由本稿独占，`.omp/RULES.md` #8）

| # | 文件 | 内容 | 行数估 |
| --- | --- | --- | --- |
| 1 | `crates/vox-audio-alsa/Cargo.toml` | §2.2 逐字形状 | 18 |
| 2 | `crates/vox-audio-alsa/src/lib.rs` | `#![cfg(target_os = "linux")]` + 重导出 + 两个探测原语 | 40 |
| 3 | `crates/vox-audio-alsa/src/probe.rs` | §3.2 的全部纯函数 + 常量 + 单测 | 260 |
| 4 | `crates/vox-audio-alsa/src/capture.rs` | `AlsaCapture` + `capture_thread` + `resolve_plan` + 单测 | 330 |
| 5 | `crates/vox-audio-alsa/src/playback.rs` | `AlsaPlayback` + `playback_thread` + 单测 | 330 |
| 6 | `crates/vox-audio-alsa/src/registry.rs` | `AlsaDeviceRegistry` + `DeviceEntry` + `apply_default` + 单测 | 240 |
| 7 | `crates/vox-audio-alsa/examples/devices.rs` | 列设备（照 `crates/vox-audio-linux/examples/devices.rs` 的形状，2026-09-30 核） | 60 |
| 8 | `crates/vox-audio-alsa/examples/loopback.rs` | 采→播，带峰值/RMS 自证 | 120 |
| 9 | `crates/vox-audio-alsa/tests/aloop_roundtrip.rs` | `#[ignore]` 真回环用例 | 110 |

### 8.2 对现有文件的最小改动（**都不在本稿里做**，各自的 owner 与前置见 §8.3）

| 文件 | 改动 | 为什么必须改 | owner |
| --- | --- | --- | --- |
| 根 `Cargo.toml` | `members` 数组加 `"crates/vox-audio-alsa"` | `.omp/AGENTS.md`：「放进 workspace 是为了让 `cargo test/clippy --workspace` 真的覆盖到它」（现有注释就是为 `app/src-tauri` 写的） | shell-dev（**ALSA-T6** 收口时改） |
| `.github/workflows/release.yml` | `apt-get install` 那一行（`:74`）加 `libasound2-dev` | 该 job 在 `:100` 跑 `cargo test --workspace`，新成员进来后没有 `libasound2-dev` 会直接 build 失败。放在与 `libpipewire-0.3-dev` 同一行、并在上面的注释里补一句为什么 | shell-dev（**ALSA-T6** 收口时改） |
| `.github/workflows/ci-arm64.yml` | **`[未核实]`**：arm64 的 libasound 怎么来（见 §8.4） | 该 job 只跑 `-p vox-core -p vox-dsp`（`:71`），**当前不受影响**；但 `vox-audio-alsa` 要上 ARM64 就必须解决 | `[后续项]` |
| `crates/vox-headless/src/platform/linux.rs` | **本稿不改**（ALSA-T7 才改，且依赖 S4-A） | 现在换会让无屏档行为变更 | 依赖 S4-A |
| `crates/vox-core/**` | **本稿一行不改** | 端口 trait 不用动（见 §8.5） | — |
| `docs/platform/{EMBEDDED,LINUX}.md` | 文档回填（§6.4 的 #1/#2/#4） | 等板子实测后再写（ALSA-T8） | docs-scribe |

### 8.3 工单

> owner 用 `.omp/agents/` 里的角色名。**前置**指必须先完成的东西。
> 三个 dev 角色里，本稿的九个文件全部由 **shell-dev** 独占（平台外壳），不需要 core-dev 或 agent-face-dev。

| 工单 | 内容 | owner | 独占文件 | 前置 |
| --- | --- | --- | --- | --- |
| **ALSA-T1** | 骨架 + 依赖：建 crate、`Cargo.toml`、`lib.rs`（含两个探测原语）、`probe.rs` 的常量与纯函数 + 全部单测 | shell-dev | 1、2、3 | 无（可与 S4-A 并行） |
| **ALSA-T2** | `AlsaCapture`：线程模型、非阻塞 + poll 循环、协商阶梯、`CaptureTarget` 三分支、`Blocker` 一致性 | shell-dev | 4 | T1 |
| **ALSA-T3** | `AlsaPlayback`：`open` 阶梯、`push`/`stats`/`flush`/`close`、播放循环的欠载补静音与 `delay` 上报 | shell-dev | 5 | T1 |
| **ALSA-T4** | `AlsaDeviceRegistry`：hint 枚举、过滤表、`apply_default` 对号、三条方法 | shell-dev | 6 | T1 |
| **ALSA-T5** | 硬件面验证：`examples/devices.rs`、`examples/loopback.rs`、`tests/aloop_roundtrip.rs`（`#[ignore]`），并在本机/板上各跑一次 | shell-dev | 7、8、9 | T2、T3、T4 |
| **ALSA-T6** | 收口：`cargo test -p vox-audio-alsa` + `cargo clippy -p vox-audio-alsa --all-targets` + `cargo fmt`；改根 `Cargo.toml` 与 `release.yml`（§8.2 前两行）；跑 `cargo test --workspace` 确认既有测试逐字未改 | shell-dev | `Cargo.toml`、`release.yml` | T5 |
| **ALSA-T7** | **接线**（改行为）：S4-A 的 `vox-host` 落地后，把无屏档的 Linux 注入点改成 ALSA 缺省、桌面档保持 PipeWire、接 `startup_notes` | shell-dev | S4-A 产出的文件 | **S4-A 完成** + T6 |
| **ALSA-T8** | 文档回填：`EMBEDDED.md` §3.2/§3.4/§4-1/§4-2/§4-3、`LINUX.md` §9.1、`DIRECTIONS.md` §8 第 17 行加口径注 | docs-scribe | 那三个 md | **板子实测完成** |
| **ALSA-T9** | **`[后续项]`** aarch64 交叉构建该 crate（CI） | shell-dev | `ci-arm64.yml` | §8.4 的未知项查清 |

**并行排期**：T1 由一个 shell-dev 独占起步；T2/T3/T4 文件不同，可在 T1 完成后**并行派三个 shell-dev**
（`.omp/RULES.md` #8 只禁"同一文件同时两个 owner"，不禁同 crate 不同文件）。
T6 的 `Cargo.toml` 与 `release.yml` 是**唯一被多工单碰到**的两个既有文件——排在最后、独占，避免冲突。

### 8.4 交叉编译（`[未核实]`，列为 ALSA-T9）

**本轮未跑，也未查证**。已知与待查：

| 项 | 状态 |
| --- | --- |
| `alsa-sys` 在 aarch64 上需要**目标架构的 `alsa.pc` + 头文件** | 由 `alsa-sys-0.6.1/build.rs::main` 的 `pkg_config::probe("alsa")` 决定（2026-09-30 核）。而 `.github/workflows/ci-arm64.yml` 现在的 sysroot 是 `libc6-dev-arm64-cross` 落在 `/usr/aarch64-linux-gnu`（`:40-41`，2026-09-30 核）——**那里面有没有 libasound 的 `.pc`，本轮未核实** |
| 大概率的做法 | Debian/Ubuntu 的 `libasound2-dev` 是 `Multi-Arch: same`，加 `dpkg --add-architecture arm64` 后装 `libasound2-dev:arm64`，再用 `PKG_CONFIG_LIBDIR`/`PKG_CONFIG_PATH` 指向 arm64 的 `.pc` 目录；`aarch64-linux-gnu-gcc` 链接时给 `-L` 找到 `libasound.so`。**这套只是常见做法，本轮没有验证过任何一步** |
| `alsa-sys` 的 `probe_time64` | 只在**32 位 gnu** 目标上探测（`alsa-sys-0.6.1/build.rs::probe_time64:26-84` 的 early return 条件，2026-09-30 核）。`aarch64-unknown-linux-gnu` 是 64 位 → **直接跳过，不编译探测代码**（这一点可以从 build.rs 读出来，不算未核实） |
| `qemu-user` 跑测试 | `ci-arm64.yml` 的 job 目前只跑 `-p vox-core -p vox-dsp`（`:71`）。`vox-audio-alsa` 的**纯函数单测**在 qemu 下能跑（不碰硬件）；`#[ignore]` 的回环用例在 qemu 下**不可能过**（qemu 里没有 `/dev/snd`） |
| **结论** | ALSA-T9 的第一步是"把 arm64 的 libasound2-dev 装进 CI 并让 `cargo build --target aarch64-unknown-linux-gnu -p vox-audio-alsa` 通过"，第二步才是把纯函数单测加进 qemu job。**这一稿不设计它。** |

### 8.5 明确不改的东西

- **`crates/vox-core` 一行不改**：三个端口 trait 现有签名够用（C1–C15 逐条核过）。
  - 特别是 `CaptureTarget::ProcessLoopback` 的失败路径**不需要**新变体——报 `PortError` 即可（§5.5）。
  - `DeviceInfo` 不加字段（§5.1 的代价已知，§8 未决 4）。
  - `Capability` 枚举不加位（§6.3）。
- **`crates/vox-audio-linux` 一行不改**：它是桌面档后端，本稿不碰。
- **不删任何东西**：本稿是纯新增。

---

## 9. 测试与验收

### 9.1 单元测试（不依赖硬件、CI 能跑、`probe.rs` 里）

按 `.omp/RULES.md` #7「测试只写会因真实 bug 失败的」逐条说清**去掉哪段实现会红**：

| 测试名（`probe.rs::tests`） | 钉的不变量 | 去掉/改错会怎样红 |
| --- | --- | --- |
| `format_ladder_puts_s32_first_and_never_repeats` | 阶梯顺序 S32 → S16，且不出现重复项 | 把 S16 排前面：红（廉价声卡削顶）。写死 `vec![Format::s16()]`：红 |
| `rate_ladder_is_exact_then_plugin_then_vox_resample` | 播放阶梯三档的顺序与档 3 的 `probe_rate == wanted` | 交换档 2/档 3：红（在目标板上白跑 sinc）。漏掉 `Exact`：红（永远装重采样器） |
| `a_short_read_is_underrun_not_progress` | `step_after(&Ok(30), 480)` == `Underrun` | 删掉短读分支：红（播放侧在没数据时空转烧 CPU） |
| `a_zero_frame_result_is_underrun_too` | `step_after(&Ok(0), 480)` == `Underrun` | 写成 `Continue`：红（同上，且 `rendered_samples` 会虚增） |
| `epipe_and_estrpipe_recover_while_eagain_is_fatal` | `EPIPE`/`ESTRPIPE` → `Recover`；`EAGAIN` → `Fatal` | 把 `EAGAIN` 也当可恢复：红（掩盖"忘了 poll"，流会静默死循环）。把 `EPIPE` 当致命：红（一次爆音就永久停摆） |
| `a_full_result_is_progress` | `step_after(&Ok(480), 480)` == `Continue { frames: 480 }` | —— |
| `f32_round_trips_through_the_device_sample` | `i32_to_f32(f32_to_i32(x)) ≈ x`，对 `0.0 / ±1.0 / ±0.5 / 1e-4` | 满量程用 `2^31` 而非 `2^31-1`：红（`±1.0` 不对称，`-1.0` 回来是 `-0.999…`） |
| `out_of_range_and_nan_clamp_to_silence` | `f32_to_i32(2.0) == I32_MAX`；`f32_to_i32(f32::NAN) == 0`；`f32_to_i32(-2.0) == -I32_MAX as i32` | 不夹紧：Rust 的 float→int 转换虽是饱和的，但 `NaN as i32 == 0` 这条是语言细节不是我们的契约——写成显式夹紧 + 显式 NaN 分支，测试钉的是**意图** |
| `block_frames_agrees_with_the_shared_blocker` | 对 6 组 `(rate, block_ms, ch)`，`block_frames(..) == Blocker::new(rate, ch, block_ms).frames_per_block() * ch` | 采集侧传请求率而不是协商率时：红（块长错 → `AudioChunk.sample_rate` 错 → 芯重采样变调）。这条是 §5.7 的钉子 |

`registry.rs::tests`：

| 测试名 | 钉的不变量 | 会因什么 bug 红 |
| --- | --- | --- |
| `apply_default_marks_only_the_matching_hw_address` | 只有 `addr == resolved` 的那条 `is_default` | 把 `addr: None` 的 `default` 条目也标上：红（指错设备 → 回声串扰） |
| `apply_default_marks_nobody_when_unresolved` | `resolved == None` 时全表无星号 | 退化成"标第一条"：红 |
| `hint_filter_keeps_hw_and_default_and_drops_the_rest` | `keep_hint` 对 §5.4 过滤表里每个前缀的判定 | 忘了排除 `front:`：红（列表里多出声道别名）。忘了排除 `pulse:`/`pipewire*`：红（无屏档出现两个"麦克风"） |
| `direction_none_is_listed_on_neither_side` | `direction == None` 的 hint 两边都不列 | 按"不是 Playback 就是 Capture"分：红（输出设备被列成麦克风） |
| `the_default_device_sorts_first_and_the_rest_by_label` | `sort_entries` 的顺序 | 忘了排序：红（hint 顺序不保证，列表会跳来跳去） |

**这些测试全部不需要声卡**——`alsa` crate 在链接期就要 libasound，但**运行期一次 PCM 都不开**
（纯函数不碰 `alsa::Error` 之外的 FFI；`step_after` 只构造 `alsa::Error::new("snd_pcm_writei", EPIPE)`，
不调用任何 alsa-lib 函数）。

### 9.2 `#[ignore]` 集成用例（`tests/aloop_roundtrip.rs`）

```rust
//! snd-aloop 回环上"采 → 播"。需要内核模块，本机/CI 通用环境都没有。
//!
//! 跑法：
//!   sudo modprobe snd-aloop            # 造出 Loopback 声卡（0 = 播放侧，1 = 采集侧）
//!   cargo test -p vox-audio-alsa --test aloop_roundtrip -- --ignored --nocapture
//!
//! 判据：播进去 440 Hz、录回来的 RMS 必须显著大于静音。静音直接 FAIL。
```

| 测试名 | 做什么 | 会因什么 bug 红 |
| --- | --- | --- |
| `capture_then_playback_through_the_loopback_card` | 播放侧 `hw:Loopback,0`，采集侧 `hw:Loopback,1`；往 `push` 灌 3 秒 440 Hz（24 kHz 单声道），采集侧 `on_chunk` 累计 RMS；断言 `rms > 0.05` 且收到 ≥ 2 秒音频 | 帧/样本换算写错（§4.2 的坑）：红。`Step::Underrun` 被当 `Continue`：红。协商率与 `Blocker` 不一致：红 |
| `stop_guarantees_no_further_chunks` | 起流 → 灌 0.5 秒 → `stop()` → 记下游收到的块数 → 等 1 秒 → 断言没再增加 | 停止逻辑漏 flag 或 poll 超时不够：红。**这是 C4 那条契约唯一的钉子** |
| `the_negotiated_format_is_reported_honestly` | 起流后 `start` 的返回值 == 随后每个 `AudioChunk.sample_rate / channels` | 回报请求值而不是协商值：红（芯按错的率重采样） |

另有一个**只在本机跑**的用例（同样 `#[ignore`，需要真声卡）：
`a_real_microphone_starts_and_delivers_non_silence`——默认麦克风录 2 秒断言非静音。
本机有 3 张卡（HDA NVidia + 2× HD-Audio Generic），但**是否有可用麦克风输入本轮未核实**
（沙箱里 `/proc/asound`、`aplay` 都读不到），所以这条的默认设备名从 `hint` 的
`is_default` 来，取不到就 `panic!` 说明"这台机器没有可用的输入设备"而不是静默跳过。

### 9.3 example

| example | 跑法 | 检查点 |
| --- | --- | --- |
| `devices` | `cargo run -p vox-audio-alsa --example devices` | 每个方向有且只有一个 `*`；列表里没有 `pulse`/`pipewire*`/`front:`；输出的名字能直接喂给 `loopback` |
| `loopback` | `cargo run -p vox-audio-alsa --example loopback -- hw:Loopback,0 hw:Loopback,1` | 打印两侧协商到的 `(rate, channels, format, period_size)` 与 3 秒 RMS；RMS 低于阈值报 FAIL。默认参数就是 `hw:Loopback,0/1`，对**非** Loopback 设备会打印一条"这会形成回授/啸叫"的警告但仍可跑 |

### 9.4 本机环境说明与验收命令

**本机（写稿时的实况）**：

| 项 | 状态 |
| --- | --- |
| sudo | **没有** |
| `libasound2-dev` | **系统里没装**。开发包已解到用户目录：`~/.local/alsa-dev/root`，`libasound.so` 已重指到系统的 `.so.2`，`alsa.pc` 的 prefix 已改写。**编译时必须给 `PKG_CONFIG_PATH`** |
| `alsa` crate 0.12.1 | 已在本机 cargo registry 里，**本稿未重新编译验证**（沙箱未放行 `cargo`） |
| 声卡 | `alsa` crate 的 print_hints 测试此前列出 **3 张卡**（HDA NVidia + 2× HD-Audio Generic）。**本轮未复核** |
| `snd-aloop` | **未加载**（本轮未复核）。→ **§9.2 的 `#[ignore]` 用例在这台机器上跑不了** |

**验收命令**（按顺序，全部在本 crate 落地后跑）：

```bash
# 0) 本机必须先给这个（没有 sudo 装不了 libasound2-dev）
export PKG_CONFIG_PATH=$HOME/.local/alsa-dev/root/usr/lib/x86_64-linux-gnu/pkgconfig

# 1) 编得过（这一步验证 alsa-sys 的 pkg-config 探测成功）
cargo build -p vox-audio-alsa

# 2) 单元测试全绿（不需要声卡、不需要 snd-aloop）—— 这是 CI 上的门
cargo test -p vox-audio-alsa

# 3) lint / 格式
cargo clippy -p vox-audio-alsa --all-targets
cargo fmt --all -- --check

# 4) 设备目录（本机有卡，所以这一步在这台机器上能真跑）
cargo run -p vox-audio-alsa --example devices

# 5) 回环：**本机跑不了**（snd-aloop 未加载 + 无 sudo）。在有权限的机器或 CI 上：
#    sudo modprobe snd-aloop
cargo test -p vox-audio-alsa --test aloop_roundtrip -- --ignored --nocapture
cargo run -p vox-audio-alsa --example loopback

# 6) 收口（ALSA-T6）：既有测试必须逐字未改
cargo test --workspace
cargo clippy --workspace --all-targets

# 7) CI 上：release.yml 的 apt 列表加了 libasound2-dev 之后，
#    该 job 的 `cargo test --workspace` 必须仍然全绿
```

**§4 验收标准**（`docs/plans/S4-EMBEDDED-REFACTOR.md:143`）"本机 `snd-aloop` 回环设备上'采 → 播'跑通
（ignored 用例 + example）"的落点：`§9.2` 的三条用例 + `§9.3` 的 `loopback` example，
在**任一**装了 `snd-aloop` 的机器上跑绿即达标。**本机跑不了这件事要如实记进 PR 描述，不许写成"已验证"。**

### 9.5 部署侧新增的要求（ALSA 路线自带的一条）

ALSA 直开把"打开设备"这件事**搬进了我们自己的进程**——这与 PipeWire 路线有实质区别
（那边是 daemon 持有设备，EMBEDDED §3.4 第一行）。因此无屏档的部署要多一条：

> `/dev/snd` 的可达性。走 `systemd --user` + 活跃会话时靠 logind 的 uaccess ACL；
> 无屏服务常驻（linger）时多半要靠 `audio` 组（`User=`/`Group=`，参考
> `crates/vox-headless/systemd/user/vox-headless.service` 与 `.../system/...` 的现有写法）。
> **`[未核实]`**：本仓库没有实测过 `/dev/snd` 的 ACL 行为（`docs/platform/EMBEDDED.md` §3.4
> 那一行本身就标着 `[未核实]`）。

**这一条属于 ALSA-T8 的文档回填**（写进 `EMBEDDED.md` §3.4），**不属于本稿的实现范围**，
但**必须在动工前就让用户知道**：这是 ALSA 路线相对 PipeWire 路线**新增**的部署负担。

---

## 10. 风险与未决

### 10.1 风险（会影响设计的）

| # | 风险 | 影响 | 缓解 |
| --- | --- | --- | --- |
| R-a | **workspace 成员加了之后，`cargo test --workspace` 在任何 Linux 开发机上都需要 `libasound2-dev`** | 贡献者/CI 的一次性门槛 | 已写进 §9.4。**备选方案**（Main 拍板）：不放在默认 `members`，改成 `exclude` + 两个 CI job 里显式 `-p vox-audio-alsa`。本稿按"加进 members"写（与 `.omp/AGENTS.md` 现有注释的理由一致） |
| R-b | 新 crate 零覆盖地被 `.omp/RULES.md` #4 盯着（不许占位/空钩子） | 每个函数都要真实现 | 三个实现文件都小（各 ~330 行），不构成风险 |
| R-c | 本机跑不了 `#[ignore]` 用例 | 验收证据只有"编得过 + 单测绿" | §9.4 第 5 步如实写"本机跑不了"；在有 `snd-aloop` 的机器或 CI 上补 |

### 10.2 未决（`[未核实]` 与待拍板，逐条列出）

1. `[待 Main 拍板]` 无屏档要同时链 `vox-audio-alsa` 与 `vox-audio-linux` 两个 crate（§6.2 的显式覆盖场景），
   还是**先只链 ALSA**、覆盖场景延后。这决定 S4-A 的 `vox-host` Linux 注入点是一次改完还是留口子。
   **本稿倾向**：先只链 ALSA（无屏档没有一位需要 PipeWire，§1.5），覆盖场景作为 S4-C 的后续条目。
2. `[未决]` 将来真要给 `mic` 报 `Busy` / `Permission` 的话，谁在什么时点问、拔插声卡后怎么刷新
   （§6.3）。**这一稿不报**，别当缺陷。
3. `[未核实]` `/dev/snd` 在无屏服务用户下的 ACL / 组要求（§9.5）。EMBEDDED §3.4 已标同一个 `[未核实]`。
4. `[未核实]` **PipeWire 跑着时 ALSA 直开某张具体声卡会不会 `EBUSY`**（§6.1-2）。第一台板子上实测。
   现象会是 `alsa::Error` 的 `errno() == EBUSY`，我们 §3.2 的 `map_err` 已经把它带进错误消息了。
5. `[未决]` `poll` 超时 200 ms 是拍的（§5.3）。有板子能测停机延迟后再定；目标是"明显小于 2 s 的 `stop` 上限"。
6. `[未决]` 采集侧每次 `on_chunk` 的那次 `Vec` 分配要不要彻底消掉（§4.4 的账）。
   本稿的立场是"不新增"，消掉它属于 S4-B（算子链统一接口）或更晚的"端到端零分配"单独立项。
   **注**：`DIRECTIONS.md` §10.0 第 3 条已经拍"分配清理这一轮不做"（2026-09-21）——本稿与之一致。
7. `[未决]` `DeviceInfo` 只一个字段导致界面显示 `hw:CARD=PCH,DEV=0`（§5.1）。
   要好看就得给 `DeviceInfo` 加 label 字段——那是 `vox-core` 的接口变更 + 全调用方迁移
   （`.omp/RULES.md` #5），**不属本稿**。若 Main 认为界面上不能接受，这一条要单开一个工单。
8. `[未核实]` `plughw` 的率转换（线性插值）在目标板上的 CPU 开销（§5.2 档 2）。
   有板子后用 `tools/bench-dsp` 那套口径量（但要注意：它是 DSP 基准，ALSA 插件的开销不在其中，
   可能要另写一个探针）。
9. `[未核实]` aarch64 交叉构建（§8.4 整表）。ALSA-T9，不阻塞 T1–T6。
10. `[未决]` `sw_params` 精细化（`avail_min` / `start_threshold`，§5.3）。先不动，等有板子。
11. `[未决]` `INPUT_BLOCK_MS = 20 ms` 与 ALSA `period_size` 的关系：采集侧目前是"读满一个 period
    就喂给 `Blocker`，块长由 `Blocker` 自己算（20 ms）"——period 可能是 10 ms 也可能是 40 ms，
    `Blocker` 会自己攒。这是对的，但意味着**回调周期与 period_size 无关**，只与 `Blocker` 有关。
    如果目标板上 period 特别大（比如某些 USB 声卡 128 ms），`Blocker` 的内部 staging 会涨到相应大小
    ——`vox_dsp::chunk::Blocker::new` 的 `with_capacity(frames_per_block * channels * 2)` 是按 **block_ms** 算的，
    不是按 period 算，所以 staging 不会因为 period 大而涨。**结论：没问题**，记一笔免得以后有人重新算一遍。
12. `[未核实]` 本机三张卡（HDA NVidia + 2× HD-Audio Generic）在本稿写下时**没有复核**，
    也没有确认哪张有麦克风输入。§9.2 的真麦克风用例因此是"取不到就 panic 说明"，不是静默跳过。

### 10.3 一句话收口

**能照着编码的部分**（§2、§3、§4、§5、§8、§9 的命令）是完整的；
**不能照着编码的部分**只有一处——`vox-host` 的注入点（§7），它必须等 S4-A 的设计稿定稿；
**明确标了 `[未核实]` 的有 6 条**（未决 2/3/4/8/9/12），它们都需要真机或板子，本轮一律没有验证。

---

## 11. 参考

- 端口契约：`crates/vox-core/src/ports.rs`（本文 §1.1 逐条引用，2026-09-30 核）
- 能力位模型：`crates/vox-core/src/capability.rs::host_ceiling` / `::HostFacts` / `::UnavailableReason`
- 参照实现：`crates/vox-audio-linux/src/{capture,playback,registry,probe}.rs`；`crates/vox-audio-win/src/{playback,rates}.rs`
- 共享 DSP：`crates/vox-dsp/src/{chunk.rs::Blocker, ring.rs::DropRing, channels.rs::duplicate_mono}`
- 装配点（会被 S4-A 搬走）：`crates/vox-headless/src/platform/{mod,linux}.rs`
- 方向与裁决：`docs/architecture/DIRECTIONS.md` §8 第 17 行、§10.9 第 5 条
- 上位稿：`docs/plans/S4-EMBEDDED-REFACTOR.md` §2.3、§3 S4-C 表格、§4、§6 第 5 问
- 平台文档（**正文待回填，见 §6.4**）：`docs/platform/EMBEDDED.md` §2/§3.2/§3.4/§4；`docs/platform/LINUX.md` §9.1
- `alsa` crate 源码（本稿引用的符号出处）：`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/alsa-0.12.1/src/{pcm.rs,device_name.rs,card.rs,poll.rs,error.rs,lib.rs}`、
  `alsa-sys-0.6.1/build.rs`（全部 2026-09-30 核）

---

## 12. Main 拍板（2026-09-30，审稿后追加；与上文冲突以本节为准）

- **M1 · R-a：进 workspace members。** Linux 开发机本来就要装 PipeWire/GTK/WebKit/clang 一串开发包，多一个 `libasound2-dev` 边际门槛很小；
  换来的是 `cargo test/clippy --workspace` 真覆盖到它。依赖已按 §2.2 用 `cfg(target_os = "linux")` 门控，Windows 不受影响。
  **ALSA-T6 追加独占文件 `README.md`**：在 Linux 开发依赖清单里补 `libasound2-dev`。
- **M2 · §8 未决 4（设备名显示为 `hw:CARD=…`）：本轮不改。** 无屏档没有界面，改 `DeviceInfo` 是 vox-core 接口变更 + 全调用方迁移，等有真实需求（手机/控制面客户端要展示）再单独立项。
- **M3 · 排期。** T1 先行（可与 S4-A 并行）；T1 合入后 T2/T3/T4 并行；T5+T6 合成一张收口工单；T7 等 S4-A 的 W5 合入后再派。
- **M4 · 本机环境。** `snd-aloop` 已由用户加载（本机第 3 张卡 `Loopback`），T5 的 `#[ignore]` 回环用例**本机要真跑**；
  编译需 `PKG_CONFIG_PATH=$HOME/.local/alsa-dev/root/usr/lib/x86_64-linux-gnu/pkgconfig`（无 sudo，开发包解在用户目录）。
- **M5 · `step_after` 语义更正（T1 审查时，2026-09-30）。** §4.2/§4.3 伪码里的 `step_after(&io.readi(..), period_frames)` 把"不满一个周期"也判成 `Underrun` 并跳过，
  会让采集侧丢掉已读到的帧、播放侧把已写出的帧重写一遍。改为 `step_after(&res)`：**只有 `Ok(0)` 是 `Underrun`，`Ok(n>0)` 一律 `Continue { frames: n }`**。
  T2 只换算/喂前 `n` 帧；T3 必须维护"本周期已写到哪"的游标，短写只推进 `n` 帧、剩下的下一轮接着写（不许重写已写出的帧，也不许为此新分配）。
- **M6 · `I32_SCALE = 2^31`（T1 实测更正 §3.2）。** f32 表示不了 `2^31 - 1`，字面量写哪个都会舍入成 `2^31`；`±1.0` 往返精确。T2/T3 模块头按此口径引用。
