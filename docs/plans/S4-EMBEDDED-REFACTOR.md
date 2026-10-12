# S4 · 嵌入式优先的重构：共享宿主层 + 算子链 + 轻量化

> 状态：**[部分落地]**（方向 2026-09-26 与用户拍板，见 `docs/architecture/DIRECTIONS.md` §10.9；2026-10-12 按代码回填状态）。
> **已落地**：S4-A 共享宿主层 `vox-host`、S4-B 算子链（`pipeline::chain::Chain` + 逐节自计时 `Chain::timings` / `Runtime::op_timings`）、
> S4-C 的 `vox-audio-alsa`（crate **+ 无屏档入口**）、回声消除算子 `Op::Aec`（W0/W1/W2/W3/W5，**W4 未做**）、半双工门、廉价重采样档，
> 以及 **CI 交叉编译**（`.github/workflows/ci-arm64.yml`；证据见 §2.2 / §3 的表与 §4 的验收表）。
> **未落地**：局域网控制面、S4-D 的 `deb`（`tools/package-headless.sh` 出的是 `tar.gz`）；
> AEC 只剩 **W4**（真板子上的播放汇排队量稳定化，见 `docs/plans/S4-C-AEC.md` §9）。
> 本稿是**第 1 版概要稿**：现状证据 → 目标形状 → 改动清单 → 验收标准。各阶段开工前由 architect
> 按本稿细化成可直接施工的版本（补全调用方清单与测试名），**不要拿本稿直接动手改公共接口**。
>
> 口径：代码与本稿打架以代码为准。本稿里的"现状"是 2026-09-26 读代码核到的；
> 那一轮环境里**没有 Rust 工具链**（`cargo` 不在 PATH），所以**没有跑过任何编译或测试**，
> 凡涉及"跑出来的数"的地方一律写"未跑"（`[已落地]` 那几行的证据来自身后各阶段稿的收口实测，逐条注明出处）。

---

## 0. 一句话

**直接做嵌入式，不先做手机。** 嵌入式档不是"把 Linux 桌面版搬进没屏幕的盒子"，而是新架构的**第一个正式宿主**：
硬件是**组合式**的（有什么就用什么，能力位说话），重构抓两个核心——
**① 装配层只写一份（共享宿主层），② 流水线从"两条写死"变成"按清单执行算子链"**。
其余（ALSA、回声消除、局域网控制面、自计时）都是往这副骨架上插模块。

---

## 1. 为什么要改（现状证据）

| # | 问题 | 证据（2026-09-26 核） |
| --- | --- | --- |
| 1 | **装配层复制了一份，不是重构** | `crates/vox-headless/src/{persist,mcp,dsp,secrets}.rs` 与 `app/src-tauri/src/{persist,mcp,dsp}.rs`、`sys/secrets.rs` 各有一份；按"去缩进后逐行集合差"粗算，`persist` 差 270 行、`mcp` 差 292 行、`dsp` 差 28 行。`EMBEDDED.md` §3.10 第 1–3 条（抽 `platform/`、控制面下沉、`EventSink`）**一条都没做**。再加手机档就是第三份 |
| 2 | **清单只是换了个壳，流水线仍是写死的** | `vox_core::composition::Op` 已有 `Mono / Denoise / Gate / Resample` 且"数组顺序即执行顺序"，但 `pipeline/mod.rs::Plan::from` 把它压回几个布尔（`Plan.denoise: bool`、`passthrough`、`hot_update`…），真正的执行顺序仍写在 `Worker` 里（`pipeline/mod.rs` 约 3000 行） |
| 3 | **没有回声消除** | `grep -rniE "aec\|echo.?cancel\|回声" --include=*.rs crates app/src-tauri/src` 零命中（本轮实跑）。桌面靠用户戴耳机回避；盒子外放译音 + 开麦 = 自己的译音被录回去再翻一遍 |
| 4 | **音频只押 PipeWire，对嵌入式偏重** | 无屏档唯一音频后端是 `vox-audio-linux`（`pipewire = "0.10"`）；随之要 PipeWire daemon + WirePlumber + `systemd --user` + `enable-linger`，且实时优先级、linger 自启、Lite 系统是否预装都标着 `[未核实]`（`docs/platform/EMBEDDED.md` §3.2、§6） |
| 5 | **控制面只认本机，无屏盒子等于遥控不了** | `crates/vox-mcp/src/transport/http.rs`：`bind` 默认 `127.0.0.1:0`，注释"只允许回环，别的地址 `serve` 直接拒绝"。没屏幕的盒子只能 ssh 进去改 `settings.json` |
| 6 | **性能结论全是推出来的，且目前测不了** | 唯一实测是 x86 上"整条 DSP 链 0.3% 单核"；手上**没有目标板**。重采样只有 `SincFixedIn` 一档（`crates/vox-dsp/src/resample.rs`） |
| 7 | **一处多余的胶水** | `dsp.rs` 两份都说"孤儿规则不让直接 impl"，但 `vox-dsp` 本来就依赖 `vox-core`（`crates/vox-dsp/Cargo.toml`），`impl vox_core::ports::Denoise for Denoiser` 写在 `vox-dsp` 里是合法的，两份 newtype 可以一起删 |

> **2026-10-12 注**：上表是 **2026-09-26** 的现状，别按它判断今天。第 1、2 条随 S4-A/S4-B 落地；
> 第 3 条（没有回声消除）随 S4-C 的 `Op::Aec` 落地（W4 除外）；第 4 条（音频只押 PipeWire）已改成
> **无屏档缺省 ALSA 并落地**；第 6 条（重采样只有一档）随廉价重采样档落地。第 5 条（控制面只认本机）
> 仍未做。

---

## 2. 目标形状

```
                 ┌──────────────── 宿主入口（薄）────────────────┐
                 │ app/src-tauri   vox-headless   (将来) android │
                 │  只负责：本机有哪些端口 + 本机能力位 + 入口参数     │
                 └──────────────────────┬────────────────────────┘
                                        │
                 ┌──────────────────────▼────────────────────────┐
                 │ vox-host（新，共享宿主层，只写一份）              │
                 │ 装配顺序 · 配置目录/持久化 · 密钥选择 · 控制面胶水 │
                 │ 事件出口 EventSink · 生命周期/退出顺序 · 自计时汇总 │
                 └──────────────────────┬────────────────────────┘
                                        │
   ┌────────────────────────────────────▼─────────────────────────────────┐
   │ vox-core：Composition（in → ops[] → out）→ 算子链执行器（取代写死的两条）│
   └───────┬──────────────┬──────────────┬──────────────┬─────────────────┘
       输入端口        算子端口         输出端口        控制面
   alsa / pipewire   mono·gate·aec    alsa / pipewire   vox-mcp
   / wasapi / net    denoise·resample / wasapi / net    （本机 + 局域网）
```

### 2.1 组合式硬件（"有什么就用什么"）

- 盒子插了什么，就报什么能力位；清单按位装配，**不靠用户手动配档**。
- 典型组合（都是同一份二进制）：
  - 只有网络：`net_in → net_out`（中继/服务器形态）；
  - USB 声卡 / 麦克风阵列 + 喇叭：`mic → … → playback{speaker}`（翻译盒子形态）；
  - 带硬件回声消除的会议全向麦：同上，但 AEC 那一节由硬件承担。

### 2.2 回声消除：三档，按能力位自动选

关键前提：**外放的译音是我们自己播的**，参考信号在进程内现成可得（播放前抄一份给 AEC），不需要抓系统声音。

| 档 | 条件（能力位） | 做法 | 代价 |
| --- | --- | --- | --- |
| **硬件 AEC** | `hw_aec`：设备自带（会议全向麦、XMOS 类阵列） | 什么都不做，只报位 | 零 |
| **软件 AEC** | `soft_aec`：麦克风与喇叭在**同一声卡**（共用时钟，时延稳定） | 播放路径把译音样本送进 AEC 作参考。**已选 `sonora-aec3`**（WebRTC AEC3 的纯 Rust 移植，BSD-3-Clause，无 C 工具链；SpeexDSP 出局，见 [`S4-C-AEC.md`](S4-C-AEC.md) §2.3）。**上限**：48 kHz 三频带 ≈ **25 dB**（16 kHz 单频带 62–72 dB），且**随时延下滑**（200 ms → 18.5 dB、400 ms → 12.7 dB；W0 实测 §6.3/§6.4）。**这一位今天各档报什么**：桌面两档外壳**硬写 `not_wired`**（`app/src-tauri/src/platform/{win.rs,linux/mod.rs}`）、**无屏档按判据真算**（`vox-headless/src/platform/linux.rs`，条件齐时**位为真**）、`Android` 不在上限（`unsupported`） | 中 |
| **半双工** | 兜底（永远可用） | 播译音期间关掉上行，播完再放一段尾巴才重新开门；在现有 `vox_core::gate::ActivationGate` 上加"播放中"输入 | ≈零 |

面对面翻译本来就近似轮流说，半双工档单独也能用，所以它是**保底**，其余两档是提升。

### 2.3 轻量化

- **音频**：新增 `crates/vox-audio-alsa`（实现 `CaptureSource` / `PlaybackSink` / `DeviceRegistry`），作为**嵌入式缺省后端**；
  PipeWire 降为可选（只有要"按程序抓音"或要虚拟麦时才用）。ALSA 路径不需要会话管理器、不需要 linger，可以做系统服务。
- **算子分档、都能关**：降噪可关；重采样加一档廉价实现（线性/多相）；**优先让声卡直接给协议要的率**，能不重采样就不重采样。
- **设计预算**（没有板子时的替代品）：以 **Cortex-A53 四核 1 GHz / 512 MB** 为假想下限，所有算子缺省走最便宜的一档，贵的档由能力位或配置打开。

### 2.4 控制面：本机 + 局域网（带鉴权）

- `http.rs` 的"只许回环"改成**开关**：缺省仍只绑本机（桌面行为不变）；无屏档可开局域网监听。
- 局域网监听**必须鉴权**：首次**配对码**换长期 token（配对码在 journal/设备灯/串口等"物理在场"渠道给出），之后请求带 token；
  沿用现有 `Grants` 与 `control.enabled` 总闸。
- 开箱配置（录 API key、选语言、看状态）走同一条路；将来的手机 App 只是它的一个客户端。
- 配网（Wi-Fi）**不在本稿范围**，另议。

### 2.5 没有板子时怎么保证"轻"

1. **自计时**：每个算子记录每块耗时与实时倍率（处理时长 / 音频时长），汇总进 `Snapshot` 并经控制面报出——**谁手上有板子，跑一次就有数**。
2. **CI 交叉编译 `aarch64-unknown-linux-gnu`** + `qemu-user` 跑测试：保证"在 ARM 上能编、跑得对"，**不代表性能**。
3. 热路径守 `.omp/RULES.md` #6（不新增分配/拷贝），自计时本身也不能分配。

---

## 3. 改动清单（分阶段）

> 顺序原则：**先搭骨架、行为不变，再插新模块**。每一阶段结束都要全绿才进下一阶段。

### S4-A 共享宿主层 `vox-host`（行为不变）

1. 新建 `crates/vox-host`：把两份装配层里**同义**的部分收进来——配置目录三级回落、`settings/usage` 持久化（去抖 + 原子写）、
   密钥后端选择、控制面胶水（`Switch` / `LedgerBackend` / 握手文件清扫）、事件出口 `EventSink` trait、装配顺序与退出顺序。
2. `app/src-tauri` 与 `vox-headless` 改成薄入口：只提供本机端口工厂 + `host_kind()` / `host_facts()` + 入口参数；
   Tauri 命令变成 `vox-host` API 的薄包装。
3. `Denoise` / `Resample` 的 impl 挪进 `vox-dsp`，删掉两份 `dsp.rs`。
4. 按 `.omp/RULES.md` #5：迁移全部调用方，不留 shim。

### S4-B 算子链执行器（行为不变）

1. `Worker` 不再读 `Plan` 里的布尔，而是**按 `Composition.ops` 顺序**执行；每个 `Op` 对应一个实现了统一小接口的算子。
2. `Plan` 收缩或删除（清单即作业单）；两条流水线的差别只剩"各自的清单长什么样"。
3. 自计时（§2.5-1）在这一步顺手接上：执行器天然知道每一节的边界。

### S4-C 新模块（逐个插）

| 模块 | 内容 | 依赖 |
| --- | --- | --- |
| `vox-audio-alsa` | ALSA 采集/播放/设备目录；无屏档缺省后端。**已落地**：`crates/vox-audio-alsa/`（`AlsaPlayback`、`registry.rs`、`examples/{devices,loopback}.rs`、`tests/aloop_roundtrip.rs`，workspace 成员）+ **无屏档入口已切过来**——`crates/vox-headless/Cargo.toml` 依赖 `vox-audio-alsa`（旧 `vox-audio-linux` 依赖已删），`Probe::Audio` / `alsa_available()`（2026-10-12 按代码核；见 [`S4-C-ALSA.md`](S4-C-ALSA.md)） | S4-A |
| 半双工门 | `ActivationGate` 加"播放中"输入；`Op::Gate` 可配。**已落地**（2026-10-11）：`gate::HalfDuplex`（`DEFAULT_TAIL_MS = 300`）+ `GateConfig::half_duplex: Option<HalfDuplex>`（`#[serde(default, skip_serializing_if = "Option::is_none")]`）+ `GateState::PlaybackMute`（线上 `"playback_mute"`）+ `ActivationGate::set_mute_for_ms` + `Chain::{echo_guard_tail_ms, set_echo_guard_ms}` + `Worker::mark_playing`；判据在 `pipeline::speak::composition`（`audible && facts.host == HostKind::LinuxHeadless`）。**与概要稿 §2.2 的差别**：不是 `bool` 输入，是按采集率样本倒数的计数器 | S4-B |
| 回声消除算子 | `Op::Aec` + `hw_aec` / `soft_aec` 能力位；软件实现选型（SpeexDSP vs AEC3）。**已落地**（2026-10-12 按代码核）：`Op::Aec` + `Composition::OP_ORDER` 5 项 / `ChainStage::Aec { port: Option<Box<dyn Aec>>, .. }` / `Chain::push_playback_reference`（`crates/vox-core/src/pipeline/chain.rs`）、`vox-dsp/src/aec.rs::SonoraAec` + `AEC_RATES = [16_000, 48_000]`（`sonora-aec3 = "=0.2.0"`）、`Capability::{HwAec,SoftAec}`（三档 `host_ceiling` 含 `soft_aec`、`hw_aec` 一档都不含）、`settings.speak.aec` / `SessionConfig.aec` / `speak::composition` 两道闸（`crates/vox-core/src/pipeline/speak.rs`）。**只剩 W4**（真板子排队量稳定化）——见 [`S4-C-AEC.md`](S4-C-AEC.md) 状态头与 §9 | S4-B |
| 廉价重采样档 | `vox-dsp` 加一档便宜实现，按配置/能力选。**已落地**（2026-10-11）：`vox_dsp::resample::ResampleQuality`（`Sinc` 为 `#[default]` / `Linear`）+ `Resampler::cheap_upsampler`（`output_rate <= input_rate` → `None`）+ `vox_dsp::ports::cheap_resample_factory` + `vox_host::entry::HostPorts::resample`（新字段，`deps(self, transport)` 签名未动）+ `vox_headless::platform::{RESAMPLE_QUALITY_ENV, resample_quality_from_env, resample_factory_for}`（`VOX_RESAMPLE_QUALITY` **只无屏档读**，缺省 `cheap`）。**与概要稿 §2.3 的差别**：只做线性、且**只对升采样方向**，多相 FIR 不做 | S4-B |
| 局域网控制面 | 绑定地址开关 + 配对码 → token；缺省仍本机 | S4-A |
| CI 交叉编译 | `aarch64-unknown-linux-gnu` 构建 + `qemu-user` 测试。**已落地**（2026-10-11 按代码核）：`.github/workflows/ci-arm64.yml`（本机未跑，它要交叉工具链 + qemu-user） | 无 |

### S4-D 交付

- 无屏档出 `deb`（Debian/Raspberry Pi OS）；系统镜像（Buildroot/Yocto 类）**另议**。

---

## 4. 验收标准

| 阶段 | 验收 |
| --- | --- |
| S4-A | `cargo test --workspace` 全绿且既有测试**逐字未改**；`cargo tree -p vox-headless` 不含 tauri/gtk/webkit；两个入口 crate 里不再有 `persist` / `mcp` 胶水 / `dsp` 适配器的第二份 |
| S4-B | 差分台（沿用 S0 做法：多组配置 × 两条流水线）事件轨迹与重构前**逐字相等**；`Plan` 里不再有"装不装某一节"的布尔 |
| S4-C · ALSA | 本机 `snd-aloop` 回环设备上"采 → 播"跑通（ignored 用例 + example）。**已落地**（2026-10-12 回填）：`crates/vox-audio-alsa/tests/aloop_roundtrip.rs`（`#[ignore]`，跑法 `modprobe snd-aloop` 后 `cargo test -p vox-audio-alsa --test aloop_roundtrip -- --ignored`)+ `examples/{loopback,devices}.rs` 在树上；**本轮实测**（`main` @ `e62cfdc`）`cargo test -p vox-audio-alsa -- --ignored` = lib 4 + `aloop_roundtrip` 4 **全过**（本机真回环）；无屏档入口也已切到它（§3） |
| S4-C · 半双工 | 用例：播放中上行为静音、播放结束 + 尾巴后恢复；去掉该逻辑即红。**已落地**（2026-10-11 实测）：`cargo test -p vox-core` = **364 passed / 0 failed**；`cargo test -p vox-core pipeline::golden` = **17 passed**（15 旧场景 + 新场景 `speak_48k_speaker_half_duplex` + 1 条确定性自检），15 份旧金样 `git diff` 空；**去掉压制分支 ⇒ 8 条红** |
| S4-C · 廉价重采样档 | 便宜档只在升采样方向成立、装配期由入口选、桌面档零差异。**已落地**（2026-10-11 实测）：`cargo test -p vox-dsp` = **34 passed**；桌面 `app/src-tauri` 里 `cheap_resample_factory` **零命中**（只多一行 `resample: vox_dsp::ports::resample_factory()`，`grep -rn cheap_resample_factory crates/ app/` 在 `app/` 下无命中）；`cd tools/bench-dsp && cargo run --release` **exit 0** |
| S4-C · AEC | 合成回声（译音按固定时延衰减叠回麦克风）下，回声能量下降达标（阈值在细化稿里定）；位为假时该节不装。**已落地**（2026-10-12 回填）：阈值 = **48 kHz 三频带 ERLE ≥ 15 dB**（[`S4-C-AEC.md`](S4-C-AEC.md) §7.2，W0 实测推导）；用例在 `crates/vox-dsp/tests/aec_synthetic.rs`（T1–T4/T6/T7/T13/T14）与 `pipeline/chain.rs::tests`（T5/T8–T12），"位为假时该节不装"钉在差分台 `speak_48k_aec_bit_off` 与 `speak.rs::the_aec_stage_needs_both_the_bit_and_the_setting`。**W4（真板子）未做** |
| S4-C · 局域网 | 缺省不监听非回环；无 token / 错 token 一律拒；配对码一次性、过期即失效 |
| S4-C · 自计时 | 快照里每个算子都有耗时与实时倍率；热路径零新增分配（用例或基准佐证） |
| S4-C · CI | ARM64 交叉构建产物可在 `qemu-user` 下跑通 `cargo test -p vox-core -p vox-dsp`。**已落地**（2026-10-11 按代码核）：`.github/workflows/ci-arm64.yml` —— `aarch64-unknown-linux-gnu` 目标 + `qemu-aarch64 -L /usr/aarch64-linux-gnu` 跑 `cargo test --target aarch64-unknown-linux-gnu -p vox-core -p vox-dsp --locked`；**本机未跑它**（要交叉工具链 + qemu-user） |

---

## 5. 不做 / 留待以后

- 手机档（S2）**顺延**：S4-A 完成后，手机只需再写一个薄入口 + 端口实现，代价比现在低。
- MCU：仍砍（`DIRECTIONS.md` §10.0 第 2 条）。
- Wi-Fi 配网、OTA、系统镜像：本稿不含，另开。
- 性能数字：有板子之前**不写任何"够用"的结论**，只交付自计时能力。

## 6. 待细化（开工前 architect 要答）

1. `vox-host` 的边界：`catalog_updater`、设备轮询、`VirtualDeviceStatus` 归不归它。
   （**已答**：见 `docs/plans/S4-A-HOST-LAYER.md`。）
2. 算子统一接口的形状（块大小、采样率变化、需要参考信号的算子如何接播放路径）。
   （**已答**：见 `docs/plans/S4-B-OP-CHAIN.md` §2.6/§2.7 与 `docs/plans/S4-C-AEC.md` §5。）
3. 软件 AEC 选型：SpeexDSP 与 WebRTC AEC3 的体积、许可证、交叉编译难度对比。
   （**已答**：选 `sonora-aec3`（WebRTC AEC3 的纯 Rust 移植），SpeexDSP 出局——理由与逐条出处见
   `docs/plans/S4-C-AEC.md` §2.2/§2.3。）
4. 配对码的"物理在场"渠道在无屏盒子上具体用哪种。
5. ALSA 与 PipeWire 同时存在时，缺省选谁、能力位怎么报。
   （**已答**：**按档位定，不做运行时探测切换**——无屏档缺省 ALSA、桌面档仍是 PipeWire；
   见 `docs/plans/S4-C-ALSA.md` §6.2 与已落地的 `crates/vox-headless/src/platform/linux.rs`。）
