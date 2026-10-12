# S4-C · 回声消除算子（`Op::Aec`）：选型 + 施工稿

> 状态：**[部分落地]**（2026-10-12 按代码回填；原文写的是"选型 + 施工稿"）。
> **已落地**：**W0**（`tools/aec-spike/` + §6 全部实测）、**W1**（`Op::Aec` / `ChainStage::Aec` /
> `Chain::push_playback_reference` / `vox-dsp/src/aec.rs::SonoraAec` / `AEC_RATES`）、
> **W2 + W3（含 ALSA-T7）**（`Capability::{HwAec,SoftAec}` / 三档 `host_ceiling` /
> `vox-headless/src/platform/linux.rs::host_facts` 的 B1–B4 / 无屏档切 `vox-audio-alsa`）、
> **W5**（`settings.speak.aec` / `SessionConfig.aec` / `speak::composition` 两道闸 / 差分台 4 条新场景 / 界面按位降级）。
> **未落地**：**W4**（真板子上播放汇排队量的稳定化——本稿唯一没有代码的那张工单）。
> 订正过的地方集中在 §4.1 / §4.2 / §4.3 / §5.1（口子形状在 §8.1）/ §7.1 / §7.3 / §8.1 / §9 / §10，
> 每处都带「落地回填 / 订正」标记；**符号名与命令输出以代码与实跑为准**。
>
> **范围**：回答 `docs/plans/S4-EMBEDDED-REFACTOR.md` §6 第 3 问（软件 AEC 选 SpeexDSP 还是
> WebRTC AEC3），并把该表 S4-C 那一行「回声消除算子」细化成可直接施工的版本。
> **不覆盖**：ALSA 后端本体（`S4-C-ALSA.md`）、半双工门（S4-C 另一张工单）、局域网控制面。
>
> **口径**：代码与本稿打架以代码为准。本稿的现状是 **2026-09-30 在分支 `s4c/aec-design`
> 读代码核到的**；调研事实是一手来源（crates.io API / 官方仓库 / 官方手册），每条附链接。
> 引用的行号会漂，**优先按符号名找**（`.omp/RULES.md` #11）。
>
> **W0 尖峰（2026-10-09）之后**：§6 的全部数字都是 `cargo run --release` 实跑出来的，
> 环境 Linux x86-64 / `rustc 1.98.1`；`cargo check --target aarch64-unknown-linux-gnu` 通过。
> **仍然没有的**只有：目标板（Cortex-A53）上的任何数字、真实声卡（`snd-aloop` 回环）上的数字。
> 凡涉及 A53 的数字，本稿一律写"外推"并由 R1 看守。

---

## 0. 一句话

**选 `sonora-aec3`（WebRTC AEC3 的纯 Rust 移植），不选 SpeexDSP。** 决定性理由有三条：
① 纯 Rust，`aarch64` 交叉编译**不需要 cmake / C 编译器 / 系统库 / git submodule**（SpeexDSP 那条
每一样都要）；② 许可证 BSD-3-Clause，与本仓 MIT 相容；③ 上游 SpeexDSP 基本停更（最后提交
2025-07-05），而 `sonora` 在活跃开发且**有公开的每 10 ms 实测基准**。
**但**：`sonora-aec3` 目前只有两个版本（0.1.0 发于 2026-02-11、**0.2.0 发于 2026-07-29，
投产至今约 2 个月**），MSRV 1.91 而本仓没锁工具链——所以
**芯与 DSP 之间的口子必须是 `ports::Aec` trait，实现藏在 `vox-dsp` 后面**，将来换实现不动链。

> **W0 尖峰已跑完（2026-10-09，§6）**，选型结论不变，但有三条把施工口径改写了：
> ① 它是**频带/块级**接口（3 频带 × 80 子帧 → 64 块），原稿"480 样本帧"那套不存在；
> ② **输入要吃 int16 域**的幅度，喂 [-1,1] 的 f32 会连时延都估不出来（ERLE ≈ 0 dB）；
> ③ **48 kHz 三频带只值 ~25 dB ERLE**，16 kHz 单频带值 62–72 dB。§7 的阈值据此重定。

---

## 1. 现状证据

### 1.1 仓库今天没有 AEC（实跑）

```
$ grep -rniE "aec|echo.?cancel|回声" --include=*.rs crates app/src-tauri/src
crates/vox-audio-alsa/src/capture.rs:479:  let channels = hwp.get_channels()...
crates/vox-audio-alsa/src/registry.rs:14:  //!  退化成"标第一条"。指错的后果是用户没设设备时译文播到麦上，形成回声串扰。
```

两条命中都是**字面偶然**（`get_channels` 里的 "ech"，`registry.rs` 模块注释里"回声串扰"那个词），
不是实现。**结论与概要稿 §1-第 3 条一致：`Op::Aec` 一次都没落地过。**
（2026-09-30 核，worktree `s4c/aec-design`。）

### 1.2 链上今天有四节，没有第五节

| 事实 | 出处（符号名优先） |
| --- | --- |
| `Op` 是封闭枚举，四个变体 `Mono / Denoise / Gate / Resample` | `crates/vox-core/src/composition.rs` 的 `pub enum Op`（约 `:170`） |
| 规定顺序是**长度 4 的常量数组** | `composition.rs::Composition::OP_ORDER = ["mono", "denoise", "gate", "resample"]`（`:415`） |
| `Op::kind()` 穷举四个变体返回字符串 | `composition.rs::Op::kind`（`:188`） |
| 执行器是**枚举而非 trait**，四节形状一致 | `crates/vox-core/src/pipeline/chain.rs::ChainStage`（`:110`） |
| 链上恒为单声道、长度不守恒、块边界不许合并、只有 `Resample` 改率、只有 `Gate` 外部驱动 | `chain.rs` 模块头 C1–C6（`:7-14`） |
| 参考信号的口子**今天不存在** | `chain.rs` 里没有 `push_playback_reference`（S4-B §2.7 说"S4-B 不实现"，见下） |
| `Deps` 五个工厂，没有 aec | `crates/vox-core/src/pipeline/mod.rs::Deps`（`:88-94`）；工厂别名在 `:80-85` |
| `Plan` 收成 6 字段，`chain_ops: Vec<Op>` 一比一照搬清单 | `pipeline/mod.rs::Plan`（`:100-118`） |

### 1.3 播放汇的参考信号在进程内现成（这是全部设计的地基）

`pipeline/mod.rs` 的 `Worker::handle_server_event` 里 `ServerEvent::AudioDelta` 分支
（`:1291-1317`）现在长这样：

```rust
ServerEvent::AudioDelta { pcm } => {
    let samples = pcm16_to_float(&pcm);
    if let Some(sink) = self.sink.as_mut() {
        sink.push(&samples);          // ← 参考信号就在这一行的上一行
    }
    if let Some(sink) = self.monitor_sink.as_mut() {
        sink.push(&samples);
    }
    ...
}
```

`pcm16_to_float(&pcm)` 出来的 `samples` **就是要外放的译音**，也就是 §2.2 说的"参考信号在进程内
现成可得"。**不需要抓系统声音、不需要第二条采集路**——这是本设计最省事也最关键的一处。

### 1.4 时钟与率（决定 AEC 能不能工作）

| 量 | 值 | 出处 |
| --- | --- | --- |
| 播放推的率 | **24 000 Hz** | `crates/vox-core/src/cloud/protocol.rs::OUTPUT_SAMPLE_RATE`（`:28`） |
| 采集块长 | **20 ms** | `crates/vox-core/src/pipeline/mod.rs::INPUT_BLOCK_MS`（`:56`） |
| 降噪原生率 | **48 000 Hz** | `pipeline/mod.rs::DENOISE_RATE`（`:65`） |
| 采集率由驱动协商，要 48 k | 阶梯 3 档 | `docs/plans/S4-C-ALSA.md` §5.2（`CAPTURE_WANT_RATE = 48_000`，该稿 §3.2） |
| 会话上行率 | Aliyun/Gemini 16 k、GPT 24 k | `chain.rs::Chain::resolve` 的 `RateRef::Session`（`:349-355`） |

**这三个数放在一起就是本设计的第一号约束：**
播放 24 k、采集 48 k、AEC 内部按 **10 ms 帧**（AEC3 的帧长）跑，链上的块是 20 ms。
所以 AEC 那一节**自己内部要拆帧 / 拼帧**（和 `Denoise` 攒 480 样本一帧是同一类活），
**并且要把 24 k 的参考升到采集率**。这两件事 S4-B 都留白了——原话在
**`docs/plans/S4-B-OP-CHAIN.md` §2.7（该文件第 311 行）**：
"运行时 `ChainStage::process` 内部**不做任何率算术**——每节自己知道自己的率。
`Op::Aec`（S4-C）若要换率，是它自己的事。"（**这是设计稿里的一句话，不是代码注释**：
`chain.rs` 里搜不到它，别照着 `chain.rs:311` 去找。）

> **W0 更正（§6.1）**：真实形状比"拆帧"更细——AEC3 是**频带 × 子帧 → 块**三级流
> （48 k 拆 3 个 16 kHz 频带；每频带 160 样本拆两个 80 子帧；每 4 次子帧插入攒一个 64 样本块）。
> 因此 `vox-dsp::aec` 里除了"24 k→48 k 升采样"和"20 ms→10 ms"之外，还多两件事：
> **48 k 的带通抽取 + 全带重组**，以及**余块抽干的时序**。工作量按 §9-W1 重估过。

### 1.5 清单里怎么装条目（现成的机制，照抄）

- `speak::composition`（`crates/vox-core/src/pipeline/speak.rs:26`）现在按位装：
  `if bits.contains(Capability::Mic) { inputs.push(...) }`（`:35`）、
  `if config.denoise { ops.push(Op::Denoise) }`（`:45`）。
  → **AEC 走同一套**：`if bits.contains(Capability::SoftAec) && config.aec { ops.push(Op::Aec) }`。
- `Composition::requirement`（`composition.rs:558`）**只映射 `in[] / out[] / session`，不映射 `ops`**。
  → `Op::Aec` **不需要**在 `requirement` 里加条目。这是核实过的现状，不是遗漏。
- `Composition::validate`（`composition.rs:455`）按 `OP_ORDER` 的下标单调性查 `BadOpOrder`。
  → 改 `OP_ORDER` 长度是**唯一**的顺序真源，`Chain::build` 那里还会再查一道
  （`chain.rs:282-288`）。

### 1.6 能力位模型的两处硬约束（实现者必须照做）

`crates/vox-core/src/capability.rs`：

1. **`scope()` 是按判别式数值切分的**：
   ```rust
   pub fn scope(self) -> CapabilityScope {
       if (self as u32) <= Self::FileConfig as u32 { Host } else { Provider }   // :137-141
   }
   ```
   现有 19 位里 `FileConfig = 10` 之后全是 provider 位。**新加的 `HwAec` / `SoftAec` 排在
   19 / 20，会被这条判成 `Provider`** —— 必须同时改 `scope()`（加一个 `PROVIDER_LAST` 之类的
   上界比较，或按 `HOST`/`PROVIDER` 两张表反查）。**不���就位表会错桶**。
2. **分桶测试把数量钉死了**：`capability.rs:552` 的
   `assert_eq!(Capability::ALL.len(), 19);`，同文件 `every_bit_has_one_bucket_and_a_stable_id`
   断言 `HOST.len() + PROVIDER.len() == ALL.len()` 且 `ALL[bit as usize] == bit`。
   → 加两位要同步改 `Capability::ALL`、`Capability::HOST`，并把那个 `19` 改成 `21`。

另外，纪律 #2（`capability.rs` 模块头）：**位为 `true` 必须有定义者**。
`NetIn / NetOut / FileConfig` 今天恒假、原因报 `NotWired`（`HostFacts::uninjected`，`:338`），
是"位先在、事实后补"的既有先例。`hw_aec` 适用同一条（见 §4.2）。

---

## 2. 选型调研

### 2.1 本项目的许可证约束

根 `Cargo.toml`：`license = "MIT"`（`[workspace.package]`，`:27`）。
→ 依赖可以是 MIT / BSD-3-Clause / Apache-2.0；
**GPL / LGPL 的库直接出局**（静态链接进 deb 也有义务问题）。SpeexDSP 与 WebRTC 都不在这一档。

### 2.2 候选总表

| 候选 | 许可证 | 是不是纯 Rust | aarch64 交叉编译 | 帧长 / 率 | 参考时延容忍 | 体积 | CPU（公开实测） | 维护 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **`sonora-aec3` 0.2.0**（WebRTC AEC3 移植） | **BSD-3-Clause** ✅ | **是**，运行时依赖只有 3 个自家纯 Rust crate | **零额外动作**（纯 Rust，无 bindgen / cmake） | **块级**：3 频带 × 80 样本子帧 → 64 样本块（`BLOCK_SIZE = 64`，每块 4 ms）；帧长 10 ms | 由 `config::Delay` 显式配：默认 `default_delay = 5` blocks、`delay_headroom_samples = 32`、`num_filters = 5` | `[未核实]` | **有**：**基准平台是 Apple M4 Max**，整条 APM（AEC3+NS+AGC2）48 kHz 单声道 10 ms = **13.3 µs**（C++ 版 10.8 µs，1.24×）；16 kHz 10 ms = 4.2 µs。README 的 aarch64/Android/iOS 只是 **CI 目标列表**，不是基准平台 | 活跃：225 commits、1 open issue、3 PR；两版，0.2.0 发于 2026-07-29 |
| **`aec3` 0.4.0**（另一份 AEC3 移植） | MIT OR BSD-3-Clause ✅ | 是（必需运行依赖 `num-complex`/`rustfft`/`crossbeam-channel`/`log`；另有可选 `postcard`/`serde`/`hound`） | 零额外动作 | 块级（同样是 AEC3 移植） | 有 `initial_delay_ms`（README 例 116 ms），**但没写搜索范围上限** | `[未核实]` | 无 | 活跃：0.4.0 发于 2026-09-16，39 478 下载 |
| **SpeexDSP AEC**（上游 C） | BSD-3-Clause ✅ | 否 | **要 cmake + C 编译器 + `libspeexdsp-dev` 或 vendored 源码** | `speex_echo_state_init(frame_size, filter_length)`：`frame_size` 官方建议 **10–20 ms**；`filter_length` 官方建议 **100–500 ms** 的回声尾 | 有 `speex_echo_cancellation_estimate_delay`；但 `speex_echo_capture` 的内建播放缓冲**只延迟两帧**来近似声卡延迟，官方注释就写着"假设的延迟可能不准" | `[未核实]` | `[未核实]` | **停滞**：最后提交 **2025-07-05**，17 open issues |
| `aec-rs` 1.0.0 / `aec-rs-sys`（Speex 的 Rust 封装） | MIT ✅ | 否（`aec-rs-sys` 是 FFI 层） | 要 cmake + NDK/toolchain；仓库要 `--recursive` clone（submodule） | 未在 README 写死 | 未在 README 写死 | `[未核实]` | 无 | 停在 2024-12-08，**近两年无提交** |
| `fdaf-aec` 0.1.0 | MIT ✅ | 是 | 零 | 未核 | 未核 | `[未核实]` | 无 | **只有 0.1.0 一个版本**，2025-06 后无更新 |
| WebRTC 原生 AEC3（C++） | BSD-3-Clause（+ 专利条款）✅ | 否 | 要编 gn/ninja + 整个 libwebrtc | 10 ms | 同 AEC3 | 整个 libwebrtc，很大 | 无公开 A53 数字 | 活跃，但集成代价与本项目不匹配 |

**出处**（逐条）：

- `sonora-aec3` 元数据：<https://crates.io/api/v1/crates/sonora-aec3>
  （`"license":"BSD-3-Clause"`、`"newest_version":"0.2.0"`、`"downloads":409648`、
  crate `created_at 2026-02-11`——那是 **0.1.0** 的发布日，**0.2.0 发布于 2026-07-29**；
  仓库 <https://github.com/dignifiedquire/sonora>；版本列表见 `.../sonora-aec3/versions`，
  一共 0.1.0 与 0.2.0 两版）
- `sonora-aec3` 运行时依赖（**只有 3 个自家 crate；dev-dep 只有 `proptest` 与 `test-strategy`**。
  `cpal` / `hound` 是**父 crate `sonora`** 的可选依赖，不在本 crate 的树里）：
  <https://crates.io/api/v1/crates/sonora-aec3/0.2.0/dependencies>
  （normal: `sonora-common-audio` / `sonora-fft` / `sonora-simd`）
- `sonora` README（**基准表**：48 kHz 单声道 10 ms 13.3 µs / C++ 10.8 µs；16 kHz 4.2 µs / 4.0 µs；
  平台含 Android aarch64 + iOS aarch64；**MSRV 1.91**；纯 Rust，无 C 工具链）：
  <https://github.com/dignifiedquire/sonora>
- `sonora-aec3::common` 常量（**`BLOCK_SIZE = 64`**、`FRAME_SIZE` = 16 kHz 下的 10 ms、`MAX_NUM_BANDS`）：
  <https://docs.rs/sonora-aec3/latest/sonora_aec3/common/index.html>
- `sonora-aec3::config::Delay` 默认值（**`default_delay = 5`**、`delay_headroom_samples = 32`、
  `num_filters = 5`、`down_sampling_factor = 4`、`delay_estimate_smoothing = 0.7`、
  `use_external_delay_estimator` 存在）：
  <https://docs.rs/sonora-aec3/latest/sonora_aec3/config/struct.Delay.html>
- `sonora-aec3::config::Filter`（`config_change_duration_blocks = 250` blocks、
  **`initial_state_seconds = 2.5 s`**、`coarse_reset_hangover_blocks = 25`）：
  <https://docs.rs/sonora-aec3/latest/sonora_aec3/config/struct.Filter.html>
- `aec3` 0.4.0 元数据与依赖（`num-complex` / `rustfft` / `crossbeam-channel` / `log` 为**必需**运行依赖；
  另有可选 normal：`postcard` / `serde` / `hound`；`anyhow` / `cpal` / `ctrlc` / `hound` 是
  **dev-dependencies**；feature 只有 `diagnostics`）：
  <https://crates.io/api/v1/crates/aec3>、<https://raw.githubusercontent.com/RubyBit/aec3-rs/main/Cargo.toml>、
  <https://github.com/RubyBit/aec3-rs>
- SpeexDSP 许可证 = **BSD 3-Clause**，著作权人含 Xiph.org Foundation / Jean-Marc Valin 等：
  <https://raw.githubusercontent.com/xiph/speexdsp/master/COPYING>
- Speex 官方 API 文档（`speex_echo_state_init(frame_size, filter_length)`：frame_size 建议代表
  **10–20 ms**、filter_length 是**要消除的回声样本数**，一般对应 **100–500 ms**；
  `speex_echo_capture` 用**"延迟两帧"**的内建播放缓冲近似典型声卡延迟，并明确警告假设可能不准）：
  <https://www.speex.org/docs/api/speex-api-reference/group__SpeexEchoState.html>
- SpeexDSP 上游停更：仓库默认分支 `master` 的最新提交是 **2025-07-05**（提交信息
  `testresample2: define M_PI if needed`），17 个 open issue：<https://github.com/xiph/speexdsp/commits/master>
- `speexdsp` 0.1.2（2018 年的老绑定，MIT，2022-04 最后更新，4 828 下载）：
  <https://crates.io/api/v1/crates/speexdsp>
- `aec-rs` / `aec-rs-sys`（均 2024-12-08 后未更新）：<https://crates.io/api/v1/crates/aec-rs>、
  <https://crates.io/api/v1/crates/aec-rs-sys>；构建说明（`--recursive` clone、Android 要 NDK）见
  <https://github.com/thewh1teagle/aec>
- `fdaf-aec` 0.1.0（唯一版本，2025-06-22）：<https://crates.io/api/v1/crates/fdaf-aec>
- **一个值得记的坑**：有人给 SpeexDSP 提过 issue「Echo doesn't cancel with PortAudio」
  （用 wav 离线测很好，实时就是不消）——典型的参考信号没对齐：
  <https://gitlab.xiph.org/xiph/speexdsp/-/work_items/14>
  这条恰好说明**参考信号对齐是 AEC 成败的关键变量，不是细节**，本稿 §5 才把它写成一等公民。

### 2.3 结论与理由

**选 `sonora-aec3`。** 按重要性排：

1. **交叉编译是本项目的硬约束，不是偏好。** 概要稿 §2.3 的设计预算是 Cortex-A53 四核 1 GHz，
   §2.5 的做法是"CI 交叉编译 `aarch64-unknown-linux-gnu`"。SpeexDSP 那条路要 **cmake + C 编译器 +
   `libspeexdsp-dev` 或 vendored 源码**（`aec-rs` 还要 `--recursive` clone submodule），
   这会让 S4-C 的 CI 交叉编译那一工单（概要稿 S4-C 末行）多出一整条工具链维护线。
   `sonora-aec3` 是纯 Rust，**交叉编译就是 `cargo build --target aarch64-unknown-linux-gnu`，零额外动作**。
2. **许可证干净。** BSD-3-Clause vs 本仓 MIT，无摩擦、无 copyleft 义务。WebRTC 原生那份还带专利条款
   （`aec3` 移植把它表述成 `MIT OR BSD-3-Clause`，`sonora` 直接是 BSD-3-Clause）。
3. **有公开的每 10 ms 实测基准可引用。** 这是四个候选里**唯一**给了 10 ms 帧级 CPU 数字的
   （M4 Max 上 13.3 µs）。SpeexDSP 侧我**一个公开的 CPU 数字都没找到**（§2.2 标了 `[未核实]`，
   不编）。
4. **依赖树干净。** `sonora-aec3` 的运行时依赖是 3 个自家的纯 Rust crate，没有 `cpal` / `anyhow` /
   `crossbeam`（`aec3` 那份有 `crossbeam-channel`，多一层并发原语）。
5. **AEC3 本身比 Speex 好。** 概要稿 §2.2 自己的措辞就是"AEC3 好但重"；在同等实现质量下，
   "重"这个缺点被纯 Rust 的构建优势抵消掉了。

**不选 `aec3` 0.4.0（另一份 AEC3 移植）的原因**：下载量 39 478 vs `sonora` 409 648；
`sonora` 有 `audio_processing` 之外还把 NS/AGC/重采样一并移植了（我们要的降噪档正好在
`vox-dsp` 里自己接），且 `sonora` 公开了基准而 `aec3` 没有。**算法同源时，选有数据可引的那个。**

**不选 SpeexDSP 的原因**（不是"Speex 差"）：
① 构建线（上面第 1 条）；② 上游停更；③ Rust 侧唯二可用的封装（`speexdsp` 0.1.2 / `aec-rs` 1.0.0）
**都停更多年**；④ 官方 API 文档自己承认 `speex_echo_capture` 的内建两帧延迟"可能不准"——
而**参考信号对齐正是这个项目最需要自己做对的一环**（§1.2-3 的 24 k/48 k/10 ms 三个率都要自己调和）。

### 2.4 选型带来的三条设计约束（必须写进代码，不许只写在文档里）

| # | 约束 | 落到哪 |
| --- | --- | --- |
| K1 | **实现藏在 `ports::Aec` trait 后面**。`sonora-aec3` 只有两版、0.2.0 投产 2 个月、MSRV 1.91；它一定会变，MSRV 也可能把我们顶出去。**W0 又加了一条：真实接口是频带/块级、且吃 int16 幅度域（§6.1/§6.2）——这两个"不知道就写不出来"的东西只能是 `vox-dsp::aec` 的内部知识** | `crates/vox-core/src/ports.rs::Aec`（新 trait）；实现在 `crates/vox-dsp/src/aec.rs`；`ChainStage::Aec` 只认 trait |
| K2 | **speexdsp 留作记录在案的次选**，不是"待定"。将来若 `sonora` 停更，退路是自己在 `vox-dsp` 里接 `speexdsp`（只换 `vox-dsp` 一个文件） | §9 风险 R2 |
| K3 | **`sonora-aec3` 走 `[target.'cfg(...)'.dependencies]` 还是普通依赖，取决于它是否 `no_std` 友好**——`[未核实]`，W1 第一步就要跑出来（`cargo tree -p vox-dsp`） | §9 W1 的第 0 步 |

---

## 3. `Op::Aec` 在清单里的位置

### 3.1 结论：`["mono", "denoise", "aec", "gate", "resample"]`

**AEC 在降噪之后、门之前、重采样之前。**

**⚠️ 本稿推翻 S4-B §2.7 的一条落点。** S4-B 稿（`docs/plans/S4-B-OP-CHAIN.md` §2.7）
那张表里 `Op::Aec` 插在 `OP_ORDER` 的 `"gate"` 与 `"resample"` 之间，即**门之后**。
本稿改到**门之前**。按 `docs/architecture/DIRECTIONS.md` §8"新者胜"，本稿（S4-C，09-30）
比 S4-B（09-30，S4-B 稿 §9 已把这条标成 O-1 待定）新，且 O-1 明确是留给 S4-C 的。
**理由**（这才是真正的决定性理由，不是"哪个更自然"）：

> `ActivationGate` 判开合看的是**能量/RMS**。AEC 若在门之后，**残留的回声能量会直接推开门**，
> 于是"没人说话时门被自己的回声打开"→ 上行发出去一整轮回声 → 云端把我们的译音又转回中文
> → **我们翻译自己**。**这正是 AEC 被装进来要消灭的那个 bug**，把它放在门后面等于没装。
> 反过来，AEC 若在门之前，它在近端主导时可能吃掉开头几十毫秒的语音——但
> `ActivationGate` 有 `preroll_ms`（`chain.rs::ChainStage::Gate` 带 `ActivationGate`，`gate.rs` 里
> 预滚 ≤200 ms），**被吃掉的那段正好落在预滚里，开门时会原样补回来**。
> 两个方向的失效模式：门后是**灾难性**的（自译），门前是**可恢复**的（少几个音）。

### 3.2 三条顺序约束逐条说清

| 约束 | 为什么 | 谁定的 |
| --- | --- | --- |
| **AEC 在 `Denoise` 之后** | RNNoise 削掉麦克风里的噪声底（空调、风扇）之后，回声是一个更干净的信号，自适应滤波器的收敛更快、ERLE 更高 | S4-B §2.7 已写，本稿沿用 |
| **AEC 在 `Gate` 之前** | 见 §3.1 的门开合论证 | **本稿新定（推翻 S4-B §2.7 的落点）** |
| **AEC 在 `Resample` 之前** | 它必须在**采集率**上做。`Op::Resample` 把它换到 16 k/24 k 会话率，跨到低采样率之后 48 k 宽带的声学尾就看不见了 | S4-B §2.7 已写，本稿沿用 |

### 3.3 顺序的校验从哪来

改 `composition.rs::Composition::OP_ORDER` 一处（4 元素 → 5 元素）就够：

- `Composition::validate`（`composition.rs:455`）按 `OP_ORDER` 下标单调性查 `BadOpOrder` → 自动生效；
- `Chain::build`（`chain.rs:282-288`）再查一道「不在规定顺序里」→ 自动生效；
- `speak::composition`（`speak.rs:43-56`）按上面的顺序 push；
- `listen::composition`（`listen.rs:78-83`）**不加 `Op::Aec`**——Listen 抓的是别的程序的声音，
  我们的译音根本没进那条路，**没有回声可消**。

`chain.rs` 里 `ChainStage::name()`（`:186`）的 `match` 和 `out_rate()`（`:175`）各要加一个臂。

---

## 4. 能力位：`hw_aec` 与 `soft_aec`

### 4.1 落位

`capability.rs` 的 `Capability` 判别式**只许往后加**（模块头纪律）：

```rust
// 追加在 SourceTranscript = 18 之后
SourceTranscript = 18,
/// 设备自带回声消除（会议全向麦、XMOS 类阵列）。
HwAec = 19,
/// 本机能做软件回声消除。
SoftAec = 20,
```

同步要改（缺一即红）：

| 改什么 | 位置 | 改成 |
| --- | --- | --- |
| `Capability::ALL` | `capability.rs:82-102` | 加这两位 |
| `Capability::HOST` | `capability.rs:105-117` | 加这两位 |
| **`Capability::scope()`** | `capability.rs:137-141` | **必须修（已落地）**：现在 `> FileConfig(10)` 一律判 Provider，新位会错桶 |
| 测试里的 `19` | `capability.rs:552` | 改成 `21` |
| `Capability::id()` | `capability.rs:148-170` | `"hw_aec"` / `"soft_aec"`（与 serde 的 `snake_case` 必须一致，有测试钉着） |
| `host_ceiling` | `capability.rs::host_ceiling`（写稿时 `:388-420`） | **拆开（落地回填）**：`hw_aec` **四档都不加**（§4.2）；`soft_aec` **进 `Windows` / `LinuxDesktop` / `LinuxHeadless` 三档、不进 `Android`**（§4.3）。**上限条目与它的定义者同轮落地**——`soft_aec` 的三档上限就是跟它的定义者（外壳的 `host_facts()`）一起进树的，本轮即 ALSA-T7 那一轮 |
| 界面 i18n | `app/ui/src/i18n/*` | 两个 key（`capability.rs` 模块头纪律 3：句子不在芯里） |

**`scope()` 怎么修**（给个具体写法，实现者照抄）：

```rust
pub fn scope(self) -> CapabilityScope {
    if Self::HOST.contains(&self) {
        CapabilityScope::Host
    } else {
        CapabilityScope::Provider
    }
}
```

按两张表反查，而不是按数值比大小——数值比大小是这次差点踩的坑。

### 4.2 `hw_aec`：任何一档的档位上限里都没有它（今天**恒假**）

**判据：没有定义者 ⇒ 恒假。** 这不是占位符，是 `capability.rs` 既有纪律
（`NetIn` / `NetOut` / `FileConfig` 三个位今天就这么写着）。理由：我们**没有任何手段探测一块会议全向麦
的 AEC 芯片在不在、开了没**——udev 属性不报，ALSA 不报，厂商 API 各家不同。
等有探测手段（udev 厂商 ID 表 / 厂商 SDK）再由"真的把这条路打开的那段代码"来定义它。

**落地形态**：
- `host_ceiling` 四档**都不含** `HwAec` → 报告恒 `CapabilityStatus::off(Unsupported)`
  （`status_of` 走 `:484-490` 那条"上限里根本没有 ⇒ 结构上做不到"）。
- **不给它加界面文案**（它在 `HOST` 表里就有一条 `off(unsupported)` 条目，会出现在报告里）。
  界面要不要显示由前端自己定；芯这边只出 `(位, reason)`。
- `HostFacts::off` 里**不许**由外壳往上报 `hw_aec`——报了也是
  `HostFacts::excess_off_bits`（`capability.rs:368`）里的外壳 bug。

> **落地回填（2026-10-12 按代码核）**：以上三条与实情一致——
> `crates/vox-core/src/capability.rs::host_ceiling` 四档都不含 `HwAec`，钉子用例
> `capability::tests::the_aec_bits_are_host_bits_with_soft_aec_only_in_the_three_wired_tiers`
> 逐档断言"上限里没有 `hw_aec`"、并断言"外壳往 `off` 里塞 `hw_aec` 会被
> `excess_off_bits()` 报成外壳 bug"。
> **但"四档都不加这两位"这句话只对 `hw_aec` 成立**：`soft_aec` 进三档（见 §4.3 开头）。
> 稿子原文把两位写在同一行（§4.1 表的 `host_ceiling` 行），实现拆开了——**上限条目与它的
> 定义者同轮落地**，本轮 = ALSA-T7 那一轮。

### 4.3 `soft_aec`：五条判据，全 AND

> **落位（落地回填，2026-10-12 按代码核）**：`soft_aec` **进 `Windows` / `LinuxDesktop` /
> `LinuxHeadless` 三档的 `host_ceiling`，不进 `Android`**（`crates/vox-core/src/capability.rs::host_ceiling`；
> `Android` 那一档还没有任何 AEC 判据）。**上限里有它，外壳才能按下面几条把它打开**；
> 这两件事同轮落地：本轮 = ALSA-T7 那一轮，判据落在
> `crates/vox-headless/src/platform/linux.rs::host_facts`。

`soft_aec` 为真的充要条件（`op_kind: p3` 写死顺序）：

| # | 判据 | 不满足时 `HostFacts::off` 报 | 谁判 | 数据从哪来 |
| --- | --- | --- | --- | --- |
| **B1** | 这个构建**编进去了** | `NotBuilt` | —（**恒成立**） | **AEC 是无条件依赖：没有 `soft-aec` cargo feature**（`crates/vox-dsp/Cargo.toml` 里 `sonora-aec3` 是普通依赖）⇒ **B1 恒成立**。稿子原写的 `cfg!(feature = "soft-aec")` 不存在 |
| **B2** | 采集源与播放汇在**同一声卡** | `Unsupported` | 装配层 | 两边 ALSA id 的 `CARD=` 段相同（见下） |
| **B3** | 采集率在 AEC 支持集内 | `Unsupported` | 装配层 | 判 `vox_core::pipeline::AEC_RATES`（**唯一真源**）= **`[16_000, 48_000]`**。稿子写的 `{16000, 32000, 48000}` 里的 **32 kHz 驱不动上游**（§10 R12） |
| **B4** | AEC **工厂起得来**（分配失败、配置非法） | `NotWired` | 装配层 | `aec_factory()(rate).is_ok()`（`crates/vox-dsp/src/ports.rs::aec_factory` → `SonoraAec::new(rate)`，与链上共用同一份 `AEC_RATES`）。稿子写的 `Deps::AecFactory()` 返回 `Err` 是同一件事的另一种写法 |
| **B5** | 用户没在设置里把它关掉 | —（**不是位的事**） | 芯 | `settings.speak.aec` |

> **落地回填（2026-10-12 按代码核）**：B1–B4 的判据实现在
> `crates/vox-headless/src/platform/linux.rs`：`host_facts()` 采事实（B2 走
> `default_devices_share_a_card()` → `vox_audio_alsa::same_card`；B3 读
> `vox_core::pipeline::AEC_RATES`；B4 走 `aec_factory_available_at(rate)`），纯函数
> `soft_aec_reason_from(factory_ok, same_card, rate_ok)` 做映射——B1 那一格的注释就是
> **"B1 恒成立（见上），所以这里没有分支"**。
> **不是 §8.3 写的 `crates/vox-audio-alsa/src/platform.rs`**：那个文件不存在；`vox-audio-alsa`
> 只出厂 `same_card` 与 `CAPTURE_WANT_RATE`，判据本身在**无屏档的装配层**。
>
> **这一位今天各档到底报什么（口径订正，2026-10-12 按代码核）**——**不是"四档全假"**：
> | 档 | 报什么 | 靠什么 |
> | --- | --- | --- |
> | `Windows` | `false(not_wired)` | **硬写**：`app/src-tauri/src/platform/win.rs` 的 `off.insert(Capability::SoftAec, UnavailableReason::NotWired)`（桌面档没有 AEC 判据） |
> | `LinuxDesktop` | `false(not_wired)` | **硬写**：`app/src-tauri/src/platform/linux/mod.rs` 同上 |
> | `LinuxHeadless` | **按 B1–B4 真判**：工厂建得起来 + 缺省采集/播放同卡 + 采集率在 `AEC_RATES` ⇒ **位为真** | `crates/vox-headless/src/platform/linux.rs::soft_aec_unavailable_reason` |
> | `Android` | `false(unsupported)` | 不在 `host_ceiling(Android)` 里（那一档还没有任何 AEC 判据） |
>
> 所以"位为假时界面整个入口不渲染"只在**桌面两档**成立；无屏档这一位是**真的能开**
> （它也正是 `Op::Aec` 的产品形态：盒子上外放译音 + 开麦）。

**B2 怎么判"同声卡"**（`S4-C-ALSA.md` §5.4 已把 id 口径定成 `hw:CARD=<name>,DEV=<n>`）：

```rust
// crates/vox-audio-alsa/src/registry.rs，与 §5.4 的 id 口径同一处
/// 两个 id 的 `CARD=` 段相同吗？（`hw:CARD=PCH,DEV=0` → `Some("PCH")`）
pub fn same_card(capture: &str, playback: &str) -> bool {
    card_of(capture).zip(card_of(playback)).is_some_and(|(a, b)| a == b)
}

fn card_of(id: &str) -> Option<&str> {
    id.strip_prefix("hw:CARD=")?.split(',').next()
}
```

**B2 判据的边界**（写清楚，免得日后被当成比它更强的保证）：
同声卡 = **同一个 `snd_pcm` 时钟域**，这是 ALSA 硬件拓扑给的强保证（同一个 codec 的
ADC/DAC 共用一个采样时钟），但**不是**"时延恒定"的保证——`DropRing` 的排队量、USB 帧抖动、
驱动缓冲深度都会动。**B2 只保证"频率不会漂"，不保证"相位不动"。** 相位那部分靠 §5.3 的对齐机制。
> `[未核实]` 目标板（哪一款 USB 声卡 / codec）上这一条到底成不成立，要等有板子时跑 W4 的对拍。
> 概要稿 §2.2 说"共用时钟、时延稳定"，本稿只保住了前半句。

**B5 为什么不是能力位**：它是**用户的选择**，不是"这台机器的事实"。
`capability.rs` 模块头第一条纪律"位 = 事实，不是期望"直接排除了它。
设置项缺省为 `true`（有板子时开），用户关掉 → `Op::Aec` 不进清单，**与位无关**。
这和 `Op::Denoise` 现在由 `config.denoise` 控制（`speak.rs:45`）是同一套。

---

## 5. 参考信号：零分配传递与时延对齐

### 5.1 口子的形状

严格照 S4-B §2.7 定的那个口子（`match ChainStage::Aec` 与 `set_gate_active` / `set_gate_config` /
`take_gate_status` **同层**）：

```rust
// crates/vox-core/src/pipeline/chain.rs
impl Chain {
    /// 播放汇刚拿到一段要外放的译音。**必须在 `sink.push()` 之前调**。
    ///
    /// 链里没有 AEC 节时走不到 `Aec` 臂 → 什么都不做（零分配、零拷贝，
    /// `samples` 是借用）。50 Hz 一次 `match`，成本可忽略。
    pub fn push_playback_reference(&mut self, samples: &[f32]) {
        for stage in &mut self.stages {
            if let ChainStage::Aec { port, .. } = stage {
                port.push_reference(samples);
            }
        }
    }
}
```

唯一调用点（`pipeline/mod.rs::Worker::handle_server_event` 的 `AudioDelta` 分支，`:1291`），
插在 `sink.push()` **之前**（改动只有这两行之间插一行）：

```rust
ServerEvent::AudioDelta { pcm } => {
    let samples = pcm16_to_float(&pcm);
    // 外放的译音就是 AEC 的参考信号：在进播放汇之前抄一份给它（§1.3）。
    self.chain.push_playback_reference(&samples);
    if let Some(sink) = self.sink.as_mut() {
        sink.push(&samples);
    }
    ...
}
```

**注意 `monitor_sink` 那一路不抄**：`monitor_translation`（`Plan` 的字段）是把同一份译音
**额外**回放到系统缺省播放设备。那一路的输出不进本机的麦克风，**不构成回声路径**，抄它只会
往参考信号里掺一段假的回声。`push_playback_reference` 只在 `sink`（主播放汇）那条路上调。

### 5.2 `ports::Aec` trait（新增，形状照抄 `Denoise`）

```rust
// crates/vox-core/src/ports.rs，紧挨着 trait Denoise（:132）
/// 回声消除器。**有流状态**，断线重来要清（同 `Denoise` 的 `reset`）。
///
/// 与 `Denoise` 同样的约定：**不保证输出长度等于输入长度**（内部攒帧），
/// 返回**已经消掉回声的部分**（可能为空）。
///
/// **幅度口径（W0 §6.2）**：本 trait 的口径与 `Denoise` 一样，是 **f32 的 [-1,1]**；
/// 但底层 `sonora-aec3` 的门限活在 **int16 域**，实现内部必须自己换算（×32768）。
/// 这是"芯里一律 f32"与"AEC3 只认 int16"之间必须由实现兜住的那道缝，别外泄到 trait 上。
pub trait Aec: Send {
    /// 送一块**采集率**的单声道 f32（上行音频），返回已消回声的部分。
    fn process(&mut self, capture: &[f32]) -> Vec<f32>;

    /// 播放汇刚拿到一段要外放的译音。**必须在 `sink.push()` 之前调**。
    ///
    /// `samples` 是**播放率**（`RateRef::Playback` 解析出来的那一档，今天恒 24 kHz），
    /// 与 `process` 的采集率可以不同——换率是本实现自己的事（出处是设计稿
    /// `S4-B-OP-CHAIN.md` §2.7，不是 `chain.rs` 的注释）。
    ///
    /// 参考与上行**不需要同一拍**送到：本实现内部有对齐缓冲（§5.3）。
    fn push_reference(&mut self, samples: &[f32]);

    /// 丢掉内部缓冲和状态（换会话、断流后重来时用）。
    fn reset(&mut self);
}
```

工厂别名照抄 `DenoiseFactory`（`pipeline/mod.rs:83`）：

```rust
pub type AecFactory = Box<dyn Fn(u32) -> PortResult<Box<dyn Aec>> + Send + Sync>;
//                                                  ^^^^^^ 采集率
```

`Deps`（`pipeline/mod.rs::Deps`，`:88`）加第 6 个字段 `pub aec: AecFactory`。
**`Deps` 现在有 4 处字面构造**（verifier 2026-10-10 实跑 `grep -rn "Deps {" crates app/src-tauri/src` 核到的）：

| 位置 | 是什么 |
| --- | --- |
| `crates/vox-core/src/pipeline/chain.rs:531` | `mod tests` 的假件 |
| `crates/vox-core/src/pipeline/mod.rs:1979` | `mod tests` 的假件 |
| `crates/vox-headless/src/headless.rs:242` | 无屏入口 |
| `app/src-tauri/src/lib.rs:203` | 桌面入口 |

四处全都要加 `aec` 字段，**不留默认值、不留 shim**（RULES #5）。

### 5.3 时延对齐（本设计最难的一处）

**问题**：`ref[n]` 是刚 `sink.push()` 出去的样本；`cap[n']` 是声卡采到的同一段空气。
中间的延时 = **播放汇排队 + 声卡播放缓冲 + DAC + 空气 + MIC + ADC + 采集汇排队**。
其中空气那一段**原理上不可测**，而且会变（人挪一下盒子）。

**分两层解决**：

**第一层：基线偏移（D0）——从播放汇自己的计数器拿。**
`ports::PlaybackStats`（`S4-C-ALSA.md` C10 定的形状）**已经有** `queued_samples`、
`rendered_samples`、`device_latency_ms`。前两个是"已收下但还没渲染出去"的样本数——
**这就是数字段时延的主体，而且是我们自己就能读到的。** 所以：

- `Aec` 实现维护一个**参考样本总数** `ref_pushed`；
- 每拍从播放汇读一次 `stats().rendered_samples`，算出"参考已经走了多少"；
- 读指针 = `ref_pushed - rendered_samples + D0`，`D0` 是一个**配置常量**（默认待实测，见 W1）；
- 读指针夹在环形缓冲的合法区间内，越界就记一条 `tracing::warn!` 并**保持**（不跳变）——
  跳变会让自适应滤波器的状态作废。

**读 `stats()` 会跨线程吗**：会。`PlaybackStats` 里的字段是 `AtomicU64`
（`S4-C-ALSA.md` §3 的 `playback.rs` 结构体形状），读它是几个 relaxed load，
**50 Hz 量级**。热路径零分配这条（RULES #6）不受影响。

> **为什么不能只靠自适应滤波器自己的时延估计**：`sonora-aec3::config::Delay` 的
> `default_delay = 5`（blocks）与 `delay_headroom_samples = 32` 定的是它**内部搜索的范围**。
> `[未核实]` 这两个数在 48 kHz 下折合多少毫秒——**W1 的第 0 步必须实测出来**（用一个已知时延的
> 合成信号扫一遍，见 §6.4）。如果它折合的窗口比我们的基线 D0 还窄，D0 就得由我们自己提供，
> AEC3 只在窄窗口里精修。**这个数是 W1 能不能落地的前提。**

**第二层：自适应精修（D1）——交给 AEC3 自己。**
在 D0 附近那个窄窗口里，AEC3 的 `EchoPathDelayEstimator` 做亚采样精修。
**我们不做跨相关、不做自己的时延估计器**——那是 AEC 的活，我们只负责把参考送到正确的率、
并把它放在正确的**粗**位置上。

> **W0 实测改写了这一节的一半（§6.4）**：AEC3 的**内建**估计器自己就能跟上 0–400 ms 的
> 固定时延（逐档估计值与真实 D 吻合，误差 ≤ 1 格 4 ms）。**用 `set_audio_buffer_delay()`
> 从外面喂 D0 几乎不给 ERLE 收益**（48 k、400 ms 处 12.47 dB 对 12.66 dB），只省 CPU
> （16 k ~一半、48 k ~四分之一，见 §6.4）。
> 因此 D0 的定位从"让 AEC 能工作"下调为"让 AEC 少用一半 CPU"：`PlaybackStats` 仍要读
> （W4），但它喂的是**性能档**而不是**正确性**。原 R5 关闭。

**环形缓冲的容量**：

```rust
// crates/vox-dsp/src/aec.rs，建链期一次分配，之后只写不分配
/// 参考环形缓冲的容量。必须 ≥ `max_delay_ms` + 一点余量，
/// 否则播放汇一抖动就把参考写环了，AEC 直接失锁。
const REF_RING_MS: u32 = 200;   // 48 kHz → 9600 样本 ≈ 38.4 KB
```

`REF_RING_MS = 200` 的理由：S4-C-ALSA 的播放汇用 `vox_dsp::ring::DropRing`（满了丢最旧），
**播放汇的排队量可以到满**。200 ms 覆盖"排队到满 + 声卡缓冲 + 空气"的全部可搜索范围；
再大就是白占内存（A53 那 512 MB 里每一 KB 都要算）。**具体值 W4 实测后定**。

> **W0 给这条加了一条硬边**（§6.4 的时延扫描）：48 kHz 三频带下 ERLE 随 D 单调下滑——
> 28.7 dB @0 ms、18.5 dB @200 ms、12.7 dB @400 ms。**200 ms 不是"内存预算的上界"，
> 而是"AEC 还值 18 dB 的那条线"**；再往上放大环形缓冲没有意义（性能掉得比收益快）。
> W4 的"排队量近恒定"因此不但关系到 D0，也直接关系到 §7 的阈值能不能守住。

### 5.4 24 k → 48 k 的参考重采样

**用线性插值，不上 sinc。** 理由与 `S4-C-ALSA.md` §5.2 对播放侧选 alsa-lib 线性插值**同一条**：
参考信号是**我们自己**播出去的 TTS 译音，而它随后还要过云端那次编码往返（编解码器本身就是
带限的）；而 sinc（`crates/vox-dsp/src/resample.rs` 现有的 `SincFixedIn`，128 点）
在**每一句话**上跑，比线性插值贵得多，在 Cortex-A53 上是明确的净亏。

> **W0 已量出来（§6.4）**：48 k、线性插值升上来的 24 k 参考 vs 原生 48 k 参考，
> D=40 ms 掉 1.1 dB（25.29 → 23.42）、D=120 ms 掉 2.4 dB（23.18 → 20.84）。
> **够不到 3 dB 判据 ⇒ 维持线性插值，不上 sinc。R6 关闭。**
> 剩下没量的只剩 A53 上线性插值 vs sinc 的 CPU 差（那一条仍然是 R1 的外推口径，不单独开票）。

**零分配**：插值的输出写进建链期就分配好的环形缓冲（落地形态是 `LinearUpsampler::push_into`
直接写 `RefRing`，**没有**单独的 `upsample_scratch`），**不新建**。

> **订正（2026-10-12 按代码核）**：上面"除 `process` 返回值那一个 `Vec` 之外不分配"这句
> **不成立**——上游 `sonora-aec3` 每 4 ms 块自建 11 份工作区，一拍实测 **62** 次
> （48 kHz；16 kHz = 58）。分配口径**以 §7.3 的 T6 与 `crates/vox-dsp/src/aec.rs` 模块头为准**；
> 「插值不新分配」这一半仍然成立。

### 5.5 频带 / 子帧 / 块三级流的拆拼（W0 更正，原稿写错了）

链上的块是 20 ms（`INPUT_BLOCK_MS`），AEC3 的原生单位是 **10 ms 全带帧**，但**送进
`BlockProcessor` 的不是帧，是块**。完整的三级流见 §6.1 的图；落到 `vox-dsp/src/aec.rs` 里是：

1. **20 ms 采集 → 2 × 10 ms 全带帧**（与 `Denoise` 攒 480 样本一帧是同一类活的扩展）。
2. **每个 10 ms 全带帧 → `num_bands` × 160 样本**。16 kHz 采集率 = 1 个频带（恒等拷贝）；
   48 kHz = 3 个频带，走 `ThreeBandFilterBank::analysis`（0–8k / 8–16k / 16–24k 三路带通抽取）。
   *32 kHz（2 频带）在公开面上驱不动*，见 §6.1 的 S3。
3. **每个频带的 160 样本 → 2 × 80 样本子帧**，交给 `FrameBlocker`，
   每次插入吐一个 **64 样本块**；**每第 4 次插入会攒出一个余块，必须 `is_block_available()`
   + `extract_block()` 抽干**，否则第 5 次插入 panic。
4. 块分两路：**参考路** `buffer_render`、**采集路** `process_capture`。
5. **块 → 子帧** 用 `BlockFramer::insert_block_and_extract_sub_frame`；余块用 `insert_block`
   只插不抽，留到下一帧吐出来。**注意 `BlockFramer` 的初值是 64 个零样本**，
   所以输出天然比输入晚 64 样本（4 ms）。
6. **`num_bands` × 160 → 全带 10 ms 帧**，走 `ThreeBandFilterBank::synthesis`。
   W0 量到这趟往返只有 **9.92 dB SNR**（§6.3）——这是"48 k 只值 25 dB ERLE"的主要来源，
   W1 不要指望它更好。

**`process` 的首帧返回空**（`Denoise` 也是这个契约，`ports.rs::Denoise::process` 的注释
"可能为空，可能长于输入"）：首批不足 10 ms 就攒着。算子延迟合计 **8 ms**
（16 k = 128 样本，48 k = 430 样本；W0 冲激探针实测），写进 §7 的真值计算。

**不给 `Op::Aec` 加 `flush`**（原稿决定不变）：`tail_ms` 那段是刻意补的静音，不是语音。

### 5.6 断线重来（契约 C6 的延伸）

`chain.rs::Chain::on_disconnect`（`:396`）今天只清 `Denoise` 与 `Resample`。
**AEC 要清**（自适应滤波器里的历史回声路径对下一条会话无效），但**和门一样不许调
会动 `external_active` 的东西**——`Aec::reset` 不会碰门，两者是分开的节，安全。
（**已落地**：`Chain::on_disconnect` 的 `ChainStage::Aec` 臂调 `port.reset()`；
钉子用例 `chain.rs::tests::the_aec_resets_but_the_gate_does_not`。）

```rust
// port 是 Option（降级两态：装了这一节但没有 port）——只清真装上的那个。
ChainStage::Aec { port: Some(port), .. } => port.reset(),
```

**为什么门不动、AEC 要动**（把这条差别钉住，别后来者"顺手"统一了）：
门的状态是**用户的按键**，跨重连要留着（`chain.rs::811` 那条
`the_gate_stays_open_across_a_disconnect` 用例钉着）；AEC 的状态是**声学路径的估计**，
下一条会话的扬声器-麦克风关系可能已经变了，留着有害无益。

---

## 6. W0 尖峰实测（2026-10-09）

> 代码：`tools/aec-spike/`（独立 crate，**不进 workspace**；只依赖 `sonora-aec3 = "=0.2.0"` 与
> `sonora = "0.2"`，两个都在 crates.io 上——**无 `[patch]`、无 `vendor/`**，干净目录
> `cargo run --release` 可复现）。信号全部确定性合成（xorshift 固定种子 + 解析带限），
> 不读文件、不连声卡。**一条命令约 4 s 出全部数字**：
> `cd tools/aec-spike && cargo run --release`。
> 本节数字都是该命令的原始输出，**没有外推、没有修饰**。
> 环境：Linux x86-64，`rustc 1.98.1`；`cargo check --target aarch64-unknown-linux-gnu` 一次通过。

### 6.1 最小可用调用序列（原稿 §5.5 的描述**是错的**，以此为准）

```text
全带 10 ms 帧 ──ThreeBandFilterBank::analysis──▶ num_bands × 160 样本
   （48 k → 3 个 16 kHz 频带，0–8k / 8–16k / 16–24k；16 k → 1 个，拆/合是恒等拷贝）
                    │  每个 160 样本拆成两个 80 样本子帧
                    ▼
FrameBlocker::insert_sub_frame_and_extract_block   80 子帧进 → 64 块出（4 ms/块）
                    │
  参考路 BlockProcessor::buffer_render      ◀──┘
  采集路 BlockProcessor::process_capture(bool 增益复位, bool 饱和, Option<&mut Block>, &mut Block)
                    ▼
BlockFramer::insert_block_and_extract_sub_frame     64 块进 → 80 子帧出
                    ▼
全带 10 ms 帧 ◀──ThreeBandFilterBank::synthesis── num_bands × 160 样本
```

三条**文档里没写、`sonora` README 也没有**的形状（W1 必须照抄）：

| # | 形状 | 不照做会怎样 |
| --- | --- | --- |
| S1 | **每第 4 次子帧插入会攒出一个余块，必须 `is_block_available()` 之后 `extract_block()` 抽干**（源码 `frame_blocker.rs` 的 `debug_assert`） | 第 5 次插入 panic（verifier 冒烟时撞到过 `range end index 80 out of range for slice of length 64`） |
| S2 | **没有"喂 480 样本帧"的入口。** 原稿 §5.5 的"960 → 2 个 480 样本帧 → 逐帧送 AEC3"**不存在**；真实形状是"频带 × 子帧 → 块" | 编不出来（verifier 试过，证伪） |
| S3 | **只支持 16000 / 32000 / 48000**（`common::valid_full_band_rate`），而 **32 kHz 在公开面上驱不动**：2 频带的 QMF 分频器在父 crate 里是 `pub(crate)`，公开模块只有 `three_band_filter_bank`（3 频带那只） | 采集率 32 kHz ⇒ W1 要么自己写 2 频带分频器，要么整条走父 crate 的 APM |

**算子延迟**（结构延迟，用"渲染全零 + 单冲激"探针量）：16 kHz = 128 样本（8 ms），
48 kHz = 430 样本（8 ms）。按真值算 ERLE 必须先对齐这段延迟；实测 ERLE 对它**不敏感**
（±600 样本内摆动 < 1 dB），8 s 以上的统计窗把这个不确定性摊薄了。
（**读尖峰输出时的坑**：主表那列 "算子延迟" 打的是 `128` / `430`——那是**样本数**，
尖峰的 `print_table` 给它加了 `ms` 后缀，单位标错了。以本段为准：都是 8 ms。）

### 6.2 决定性发现 #2：**输入幅度是 int16 域，不是 f32 的 [-1,1]**

`config::Delay::poor_excitation_render_limit = 150`（默认值）折算成 matched filter 的激励门限

```
x2_sum_threshold = 滤波器长(512) × 150² = 1.152e7          （在 ÷4 抽取域）
```

参考信号如果按 [-1,1] 的 f32 送，抽取之后 RMS 只有几百甚至几十，`x2_sum` 永远过不了这道门
⇒ matched filter 永不更新 ⇒ **内部时延估不出来** ⇒ `BlockProcessor::process_capture` 拿不到对齐位置
⇒ **输出 ≈ 输入、ERLE ≈ 0 dB**。这正是 verifier 冒烟时看到的现象，当时无法归因。

幅度阶梯（16 kHz 单频带、D=40 ms、内建时延估计；"render RMS" **是标称值**，见下方读法）：

| 相对满幅衰减 | 标称 render RMS | 真 ERLE | 内置 ERLE | 内置时延估计 |
| --- | --- | --- | --- | --- |
| 0 dB | 6553.6 | **61.31 dB** | 13.30 dB | 36 ms |
| −6 dB | 3284.6 | 58.13 dB | 13.07 dB | 36 ms |
| −12 dB | 1646.2 | 52.96 dB | 12.83 dB | 36 ms |
| −18 dB | 825.1 | 47.20 dB | 12.47 dB | 12 ms |
| −24 dB | 413.5 | 29.92 dB | −20.40 dB | 12 ms（失锁） |
| −30 dB | 207.2 | 24.57 dB | −30.00 dB | 12 ms（失锁） |
| −40 dB | 65.5 | 17.62 dB | −30.00 dB | 12 ms（失锁） |
| −60 dB | 6.6 | 14.86 dB | −30.00 dB | 12 ms（失锁） |

**读法**：门限落在第 −24 dB 一档附近；跌过去以后
"内置时延估计"退化成 `default_delay` 折算出的 12 ms 常量、内置 ERLE 退成哨兵 −30 dB，
ERLE 跟着塌。**W1 的 `Aec::push_reference` / `process` 必须自己换算 `s16 = x × 32768`**。
这是**接口约定、不是实现细节**，原稿没写——而它正是"芯里一律 f32 [-1,1]"这条纪律
与"AEC3 只认 int16 域"之间必须由 `vox-dsp::aec` 兜住的那道缝。

> **verifier 复核修正（2026-10-10）**：上表 "render RMS" 列是**标称值**，由尖峰自己按
> `0.20 × 32768 × 10^(attn/20)` 算出来打印，**不是实测**。真正喂进 AEC3 的参考还乘了
> 那一道 0.7 Hz 慢起伏（`run()` 里的 `0.55 + 0.45·sin`），RMS 只剩标称的 **0.647**
> （独立复算：0 dB 档实际 **4242**，不是 6553.6）。所以真实门限 ≈ **270**（int16 RMS）
> 而不是 400——**两档之间的差只有 ~4 dB，"必须喂 int16 幅度域"这条结论不变**
> （−18 dB 档实测 ERLE 仍有 47.20 dB，−24 dB 档才失锁）。

### 6.3 决定性发现 #3：**48 kHz 三频带 ≈ 25 dB，16 kHz 单频带 ≈ 62–72 dB**

单讲、内建时延估计：

| 形状 | D=40 ms | D=120 ms |
| --- | --- | --- |
| 16 k、1 频带、参考 0–7 kHz | 62.03 dB | 61.84 dB |
| 16 k、1 频带、参考 0–4 kHz | 71.79 dB | 71.94 dB |
| 16 k、1 频带、参考 0–2 kHz | 72.01 dB | 71.57 dB |
| **48 k、3 频带、参考 0–21.6 kHz** | **26.43 dB** | **23.65 dB** |
| 48 k、3 频带、参考只在 band0（0–4 kHz） | 25.18 dB | 23.77 dB |
| 48 k、3 频带、参考只在 band0（0–8 kHz） | 25.14 dB | 24.31 dB |
| 48 k、3 频带、参考只在 band1（8–16 kHz） | 24.84 dB | 23.13 dB |
| 48 k、3 频带、参考只在 band2（16–21 kHz） | 25.52 dB | 24.85 dB |

**统计口径**：本表每格 = 8 s 音频、从第 4 s 起算稳态。§6.4 主表用的是 16 s / 第 8 s 起，
所以同一形状（48 k、D=40 ms）在主表里低 ~1 dB（25.29 对 26.43）；**16 k 差得多（69.42 对 62.03）
是因为它收敛慢**（§6.4：7.5 s 才到稳态−3 dB），短窗里还没收敛完。**结论用 §6.4 的长窗口径。**

**把参考单独关进任何一个频带，ERLE 都不变（≈25 dB）** ⇒ 这不是"频带间分配"问题，
是 3 频带这条路本身的成本，值 **~46 dB**。旁证（同一次 `cargo run` 的输出）：
`ThreeBandFilterBank` analysis→synthesis 往返质量（白噪声 200 帧）只有 **SNR 9.92 dB**
（最佳滞后 46 样本 / 0.96 ms，重建冲激峰值 0.8919 而非 1.0）。

> **这一条直接改写 §7 的验收阈值和 §5.3 的 D0 设计**：产品按 `S4-C-ALSA.md` 的
> `CAPTURE_WANT_RATE = 48000` 采集、走 3 频带，那么 **AEC 的上限就是 ~25 dB 而不是 30+ dB**；
> 而 16 kHz 单频带能给到 62–72 dB。要不要让 `Op::Aec` 内部在 16 kHz 上做
> （采集降采样进、再升回去），是**需要 Main 拍的产品级取舍**，见 §11"待裁决"。

### 6.4 双讲、收敛、时延搜索窗口、CPU

主表（真 ERLE = 10·log10(Σecho² / Σ(out−near)²)，按算子延迟对齐；近端比回声低 14 dB）：

| 场景 | 真 ERLE | 收敛（稳态−3 dB） | 近端保留 | 内置 ERL | 内置时延估计 |
| --- | --- | --- | --- | --- | --- |
| 16 k D=40 ms 单讲 | 69.42 dB | 7.5 s | n/a | 64.2 dB | 36 ms |
| 16 k D=40 ms 双讲 | 12.14 dB | 0.5 s | −10.1 dB | 10.0 dB | 36 ms |
| 16 k D=40 ms 外部时延 | 69.40 dB | 9.0 s | n/a | 72.1 dB | 40 ms |
| 16 k D=120 ms 单讲 | 69.55 dB | 8.0 s | n/a | 64.4 dB | 116 ms |
| 16 k D=120 ms 双讲 | 12.36 dB | 0.5 s | −9.4 dB | 9.6 dB | 116 ms |
| 16 k D=120 ms 外部时延 | 69.38 dB | 9.0 s | n/a | 72.0 dB | 120 ms |
| 16 k D=40 ms 只有近端（无回声） | n/a | n/a | **−0.0 dB** | 0.2 dB | 12 ms |
| 48 k D=40 ms 单讲 | 25.29 dB | 0.5 s | n/a | 10.3 dB | 36 ms |
| 48 k D=40 ms 双讲 | 11.78 dB | 0.5 s | −10.3 dB | 7.2 dB | 36 ms |
| 48 k D=40 ms 外部时延 | 25.23 dB | 1.0 s | n/a | 9.9 dB | 40 ms |
| 48 k D=120 ms 单讲 | 23.18 dB | 0.5 s | n/a | 10.1 dB | 116 ms |
| 48 k D=120 ms 双讲 | 11.63 dB | 0.5 s | −10.3 dB | 7.0 dB | 116 ms |
| 48 k D=120 ms 外部时延 | 23.13 dB | 1.0 s | n/a | 10.0 dB | 120 ms |
| 48 k D=40 ms 只有近端（无回声） | n/a | n/a | **−0.9 dB** | 0.2 dB | 12 ms |
| 48 k D=40 ms 参考 24 k 线性插值升 | 23.42 dB | 0.5 s | n/a | 10.2 dB | 36 ms |
| 48 k D=120 ms 参考 24 k 线性插值升 | 20.84 dB | 0.5 s | n/a | 9.7 dB | 116 ms |

**结论**：

- **§5.4 的 24 k→48 k 线性插值不成问题**：23.42 / 20.84 dB 对 25.29 / 23.18 dB，
  掉 1.1–2.4 dB，**够不到原稿设的 3 dB 判据**。**R6 关闭，维持线性插值，不上 sinc。**
- **双讲会实打实吃掉近端**：近端比回声低 14 dB 时，输出里近端低 ~10 dB（16 k / 48 k 一样）。
  但"没有回声"那一档近端保留 −0.0 dB（16 k）/ −0.9 dB（48 k）⇒ **AEC 不会误伤纯近端**，
  这 10 dB 是双讲时抑制滤波器的代价，不是 bug。§7 的 T3 要按这个数定，别定 3 dB。
- **外部时延估计器几乎不改善 ERLE**（48 k：25.23 对 25.29 dB），但**省 CPU**（见下文：
  16 k 约省一半、48 k 约省四分之一）。收敛 0.5–1.0 s（48 k）/ 0.5–9 s（16 k），快于原稿 §7.2 设的 3 s。

**时延搜索窗口**（内建估计器，D 从 0 扫到 400 ms，步长 20 ms）：

| D | 16 k 真 ERLE | 16 k 内置估计 | 48 k 真 ERLE | 48 k 内置估计 |
| --- | --- | --- | --- | --- |
| 0 ms | 0.29 dB | 12 ms | 28.70 dB | 12 ms |
| 40 ms | 58.39 dB | 36 ms | 26.76 dB | 36 ms |
| 120 ms | 58.12 dB | 116 ms | 23.33 dB | 116 ms |
| 200 ms | 57.40 dB | 196 ms | 18.51 dB | 196 ms |
| 280 ms | 70.29 dB | 276 ms | 15.50 dB | 276 ms |
| 400 ms | 70.33 dB | 396 ms | 12.66 dB | 396 ms |

- **原稿 §5.3 的 `[未核实]` 关闭**：内建估计器**自己就能跟上 400 ms**（估计值与真实 D 逐档吻合，
  误差 ≤ 1 格 4 ms）。**R5 关闭。** 外部时延估计器的逐档结果与内建几乎逐字相同
  （48 k、400 ms 处 12.47 对 12.66 dB）⇒ **外部喂 D0 不给 ERLE 收益**，只省 CPU。
- 但 **48 k 的 ERLE 随 D 单调下滑**：28.7 dB @0 ms → 18.5 dB @200 ms → 12.7 dB @400 ms；
  16 k 不随 D 变（58–70 dB）。**这是"3 频带成本"之外的第二笔账**，写进 §7 的阈值推导。
- 16 k 在 **D=0 完全失效**（0.29 dB）：渲染缓冲自身带 ≥1 块的固定延迟，回声路径零延迟时
  对齐不可表达。真板上 D=0 不可能（至少几十 ms），**不是缺陷**，但别拿 D=0 当合成主判据。

**CPU**（x86-64，每 10 ms 一拍墙钟，含本尖峰自己的胶水）：

| 形状 | 中位 | 最大 | 单核占用 |
| --- | --- | --- | --- |
| 16 kHz 单频带，内建时延估计 | 29 µs | 46 µs | 0.29% |
| 16 kHz 单频带，外部时延估计 | **15 µs** | 26 µs | 0.15% |
| 48 kHz 三频带，内建时延估计 | 53 µs | 76 µs | 0.53% |
| 48 kHz 三频带，外部时延估计 | **39 µs** | 55 µs | 0.39% |

与 `sonora` README 的 M4 Max 数字（16 k 4.2 µs、48 k 13.3 µs，**整条 APM**）不同量级——
本机是 x86-64 且只跑 AEC3 + 胶水，**不能直接比**。**A53 1 GHz 上仍是外推，仍是 R1**，
但在 x86 上先有了一条顺序关系：外部时延估计器比内建便宜（16 k 29→15 µs 约一半，
48 k 53→39 µs 约四分之一），A53 上大概率同向。

> **verifier 复核（2026-10-10，工作树上重新编译后重跑 `cargo run --release`）**：
> ① 本表是**墙钟**，随机器与负载漂：同一份源码在中位一栏量到 16 k 单讲 41.6 µs /
> 48 k 单讲 77.9 µs（比表中高 ~40%），最大值一栏漂得更狠（机时被占时能到毫秒级）
> ⇒ **只有"内建 vs 外部"的排序可用，绝对数字不要当验收阈值**。② 原稿（与尖峰自己打印的
> 结论文本）写的"外部时延估计器便宜 ~45%"**只对 16 kHz 成立**；产品形态 48 kHz 上按本表
> 算只有 ~26%（53→39）。上表已按此改正。

### 6.5 W0 的结论（五条，W1 照抄）

1. 调用序列 = `analysis/synthesis` + `FrameBlocker` + `BlockProcessor` + `BlockFramer`；
   每 4 次子帧插入必须把余块抽干，否则 panic。32 kHz（2 频带）公开面驱不动。
2. **输入是 int16 域**：门限 `512 × 150² = 1.152e7`，[-1,1] 的 f32 永远估不出时延 ⇒ ERLE ≈ 0 dB。
3. **48 kHz 三频带 ≈ 25 dB（随 D 增大跌到 12 dB）；16 kHz 单频带 ≈ 62–72 dB。**
   差距 ~46 dB，与参考落在哪个频带无关。
4. 双讲吃近端 ~10 dB；无回声时近端保留 −0.0 dB（不误伤）；收敛 ≤ 1 s（48 k）。
5. CPU：16 k ≈ 29 µs、48 k ≈ 53 µs（x86-64 墙钟，0.29%–0.53% 单核，**绝对数字随负载漂，只作量级参考**）；
   外部时延估计器更便宜（16 k ~一半、48 k ~四分之一）。

**W0 不构成"停手换库"的条件**（判据是"稳态 ERLE < 10 dB 或不收敛"）：48 kHz 三频带在受控
信号上稳态 23–25 dB 且 ≤ 1 s 收敛，16 kHz 单频带 60+ dB。**`sonora-aec3` 保留不换。**

## 7. 验收测试

### 7.1 测试装置（`crates/vox-dsp/tests/aec_synthetic.rs`）——**以 §6.4 那套为准**

**§7.1 的简笔装置已被 W0 取代（订正，2026-10-12 按代码核）**：稿子原来那套（1 kHz 正弦 +
固定种子均匀噪声的参考、β = 0.4、D = 30 ms）在真实现上只有 **5.43 dB**（D = 30 ms）/
**3.61 dB**（D = 40 ms），**够不到 §7.2 的 15 dB**——而 §7.2 的阈值**全部**来自 §6.4 的装置
（带限噪声 + 0.7 Hz 慢起伏）。实现按 §6.4 那套装，**装置以 §6.4 为准**。
（出处：那两个数是 **W5 那轮的复核实测**，仓内无第二处记载；本稿按它改口径，**不改 §7.2 的阈值**。）

固定种子、确定性，**不依赖真声卡**（`snd-aloop` 那条走 W4 的 `#[ignore]` 用例）。
落地在 `crates/vox-dsp/tests/aec_synthetic.rs`（常量名可核）：

```text
参考    reference[] = 24 kHz 带限噪声（100 Hz – 10.8 kHz，REF_SEED），RMS = REF_RMS(0.20)，
                     再乘 0.7 Hz 慢起伏 0.55 + 0.45·sin
                     ← 少了这层起伏，AEC3 的 stationarity 门会卡住自适应（§6.4 的 run() 就是这么做的）
播放    played[]    = upsample_linear(reference) 升到采集率（线性插值，即 §5.4 那一档）
回声    echo[n]     = ECHO_GAIN(0.4) · played[n − D]        D = setup.delay_ms 折算成样本
近端    near[]      = 300 Hz – 4 kHz 带限噪声，比回声低 NEAR_BELOW_ECHO_DB = 14 dB
采集    capture[n]  = echo[n] + near[n]（单讲把 near 清零；"只有近端"把 echo 清零）
```

**W0 给这个装置补了四条"不写就一定是空转"的前提**（§6）：

1. **幅度必须是 int16 域**：送进 `SonoraAec` 的参考与采集都要 ×32768。
   装置里的 `reference` / `near[]` 因此先归一到 int16 RMS 量级，`ECHO_GAIN·played` 才是回声。
   （这一条 W0 之前没人知道，是所有"ERLE ≈ 0 dB"假象的成因。）
2. **算子延迟 8 ms 必须对齐**（16 k = 128 样本、48 k = 430 样本）：
   `residual[n] = out[n + L] − near[n]`，L 由 `probe_latency()`（渲染全零 + 单冲激）量出来
   （本实现固定 8 ms）。
3. **D 不许取 0**：16 kHz 下渲染缓冲自带 ≥1 块固定延迟，D=0 不可表达（W0 实测 0.29 dB）。
   D=30/40/120/200 ms 各一档。
4. **统计窗**：主档 `TOTAL_MS = 8_000` / `STEADY_FROM_MS = 4_000`（§6.3 的口径）；
   T3 的"只有近端"那一档单独取 **8–12 s**（理由见 §7.3 的 T3 行）。

**真值 ERLE**（与 W0 尖峰同口径，`out` 是经过算子延迟对齐后的）：

```
ERLE(dB) = 10·log10( Σ echo[n]² / Σ residual_echo[n]² )
residual_echo[n] = aec_out[n] − near_end[n]
```

### 7.2 主判据：**48 kHz 三频带 ERLE ≥ 15 dB、≤ 1 s 达到**

**阈值为什么定 15 dB（W0 实测推导，不是抄惯例）**：

| 量 | 值 | 出处 |
| --- | --- | --- |
| 48 kHz 三频带、D=40 ms 单讲实测 | **25.29 dB** | W0 §6.4 |
| 48 kHz 三频带、D=120 ms 单讲实测 | **23.18 dB** | W0 §6.4 |
| 48 kHz 三频带、D=200 ms 单讲实测 | **18.51 dB** | W0 §6.4 |
| 48 kHz 三频带、D=400 ms 单讲实测 | **12.66 dB** | W0 §6.4 |
| **本稿取 T1 判据** | **15 dB @ D ≤ 120 ms** | 比实测低 8 dB 的下余量 |
| `REF_RING_MS` 上限的意义 | 200 ms（18.51 dB，仍>15） | §5.3 |

**理由**：① 产品按 `CAPTURE_WANT_RATE = 48000` 采集、走 3 频带，**ERLE 的天花板就是 ~25 dB**，
原稿想要的 "25–30 dB" 在这条路上**不可能**（W0 §6.3）；② D=120 ms 实测 23.18 dB，取 15 dB
留 8 dB 给"真板子的回声路径不是纯增益 + 纯时延"（有混响、有非线性、有 DropRing 抖动）；
③ **D>200 ms 达不到 15 dB**（400 ms 只有 12.66 dB），所以阈值必须和
`REF_RING_MS = 200` 绑在一起写——超过 200 ms 的时延不是"AEC 再努力一点"，是**换个测量口径**。

**收敛时间从 3 s 收到 1 s**：48 kHz 实测 0.5 s 就达到稳态（16 kHz 是 0.5–9 s，
但 16 k 不是产品形态）。`initial_state_seconds = 2.5` 是"AEC3 退出保守相"的门，
不代表 ERLE 曲线要等它 2.5 s。

**为什么不上 25–30 dB**（AEC3 在 x86 上的设计目标是 30+ dB，本稿不上）：
48 k 三频带本身把上限压到 ~25 dB（W0 §6.3），想再往上只能换成 **16 kHz 单频带档**
（实测 62–72 dB）——那是一个**产品级取舍**（要改 §3.1 的顺序、或让 AEC 内部升降采样），
见 §11"待裁决"，不在 W1 里夹带。

**双讲判据另立（T3）**：W0 实测"近端比回声低 14 dB 时，输出的近端低 ~10 dB"，
而"没有回声"那一档近端保留 **−0.0 dB**。所以 T3 的判据是
**`SNR(aec_out − near_end)` 相对消之前的退化 ≤ 12 dB**（给实测 10 dB 留 2 dB 余量），
并且**必须同时钉住"只有近端的那一档近端保留 ≥ −3 dB"**——否则"把近端全消掉"
也能让前一条变绿。**这一档的统计窗取 8–12 s**（不是主档的 4–8 s）：要等 AEC3 的透传档
（上游 `sonora-aec3` 的 `transparent_mode.rs`，阈值 `>6 s`）打开——那一档正是为"没有回声
（戴耳机）"准备的；4–8 s 量到的还是抑制器的衰减（约 −7 dB），是另一件事。

### 7.3 用例清单（每条都要"去掉实现即红"，RULES #7）

| # | 用例 | 钉住什么 | 去掉/改错会怎么红 |
| --- | --- | --- | --- |
| T1 | `synthetic_echo_is_cancelled` | 合成回声（48 kHz、D=30 ms 与 120 ms、β=0.4、**int16 幅度域**）下 ERLE ≥ 15 dB，且 ≤ 1 s 达到 | `Aec` 改成直通 → ERLE ≈ 0 dB（W0 §6.2 复现过） |
| T2 | `converges_within_one_second` | 逐帧 ERLE 曲线，**从 1 s 起不再低于 12 dB**（48 kHz 实测 0.5 s 到稳态 25 dB） | 滤波器不自适应 → 曲线平在 0 dB |
| T3 | `near_end_survives` | 双讲（近端比回声低 14 dB）下 `aec_out − near_end` 的 SNR 相对消前退化 **≤ 12 dB**；**并且**"只有近端、无回声"那一档近端保留 **≥ −3 dB**（那一档的统计窗取 **8–12 s**，等 AEC3 的透传档打开，见 §7.2） | 过度抑制（把近端也当回声消了）→ 红。两条必须都写，否则"把近端全消掉"也能让第一条变绿 |
| T4 | `a_misaligned_reference_cancels_worse` | **D=400 ms 时 ERLE 必须 < 15 dB**（W0 实测 12.66 dB），同时 D=40 ms 必须 ≥ 15 dB | **这条是 T1 的对照**：钉住"这个测试真的在测对齐，不是恒等变换"，也钉住"超窗的时延必须诚实退化" |
| T5 | `without_the_op_the_stage_is_simply_absent` | `Op::Aec` 不进清单时，链上就没有 `ChainStage::Aec`（`chain.rs::stage_names` 不含 `"aec"`），且 `push_playback_reference` 走空 `match` 什么都不做 | 硬塞一个 AEC 进去 → 红 |
| T6 | `the_aec_stage_adds_no_allocations_beyond_the_return_vec` | `Aec::process` **一次 20 ms 拍**的分配次数 **≤ `MAX_PROCESS_ALLOCS_PER_BLOCK = 64`**（回归护栏）；`push_reference` **零分配**。**订正（2026-10-12 按代码核）**：稿子原写的"== 直通实现同一次调用的次数"**不可实现**——上游 `EchoRemover::process_capture` **每 4 ms 块自建 11 份工作区**，实测 **62**（48 k）= **55（上游 5 块 × 11）+ 1（返回 `Vec`）+ 6（借用表 3 频带 × 2 帧）**；**16 kHz = 58**（借用表退成每帧 1 次、一拍 2 次）。详细拆项在 `crates/vox-dsp/tests/aec_synthetic.rs` 的 T6 注释与 `crates/vox-dsp/src/aec.rs` 模块头的"分配"一节 | 任何一次热路径 `Vec::new()` / `collect()` / `clone()` → 红（上限 64 抓得住"借用表每次插入都现搭"= +32 ⇒ 94、"每帧每路各搭一份" = +6 ⇒ 68，抓不住比 +2 更小的零星分配） |
| T7 | `reference_survives_playback_jitter` | 参考写入速率与上行速率**不同**（模拟播放汇 20 ms 一段、采集 20 ms 一拍），且两者相位**不对齐**，AEC 仍收敛 | 把两路硬绑成同一拍 → 抖动一加就红 |
| T8 | `the_chain_aec_stage_sees_the_capture_rate` | 工厂拿到的率 == `CaptureFormat.sample_rate`；`Op::Aec` 的 `out_rate` == 入口率（契约 C4） | 拿会话率建 → 红 |
| T9 | `a_capture_rate_outside_the_supported_set_degrades` | 采集率 44.1 kHz → `AecSkip::Rate(44100)`，与 `DenoiseSkip` 同样两态、同样日志口径 | 硬跑 → 红 |
| T10 | `a_broken_factory_degrades_but_does_not_kill_the_pipeline` | 工厂返 `Err` → `AecSkip::Factory(err)`，带原始消息，链照跑 | 整条流水线挂掉 → 红 |
| T11 | `the_gate_still_sees_a_clean_mic` | **§3.1 的钉子**：只播参考、不给近端（`near_end = 0`）时，`ActivationGate` **不开** | AEC 放在门之后 → 门被回声打开 → 红 |
| T12 | `the_aec_resets_but_the_gate_does_not` | 契约 C6 的 AEC 半边：`on_disconnect` 后滤波器状态清零，门仍开着 | 一起清 → 红 |

T11 是本稿推翻 S4-B §2.7 的那条决定的**回归钉**。它会因真实 bug 失败：AEC 装错位置、
或参考信号根本没送进去，都会让它红。

**新增两条 W0 钉子**（RULES #7：去掉实现即红）：

| # | 用例 | 钉住什么 | 去掉/改错会怎么红 |
| --- | --- | --- | --- |
| T13 | `the_reference_is_fed_in_the_int16_domain` | 同一条合成参考：按 f32 [-1,1] 送（实现内部不换算）时 ERLE 必须 **< 5 dB**（W0 实测 0–1 dB）；按 int16 域送时必须 ≥ 15 dB。**后半条即 T1，两半都钉** | **这是 W0 第一号发现的回归钉**：`sonora-aec3` 的激励门限是 `512 × 150² = 1.152e7`。删掉 `×32768` 这一步 → 前半条绿、后半条红 |
| T14 | `the_three_band_path_is_capped_around_25_db` | 48 kHz 三频带、D=40 ms、单讲、纯增益 + 纯时延回声下 ERLE **≥ 18 dB 且 ≤ 35 dB** | 上界钉住"别把滤波组接错了还能假装很好"（接错时 W0 撞到过 -66 dB 之后的假象）；下界钉住"48 k 这条路本身有 ~46 dB 的账"，防止 W1 拿 16 kHz 的数字冒充 48 kHz 的验收结果 |

T13/T14 不测实现细节，测的是**接口约定与已知量级**：T13 钉幅度域，T14 钉频带开销。

> **T6 的一笔记账（订正，2026-10-12）**：第一版实现用**自引用 `unsafe`** 换"借用表也零分配"，
> **Miri 的 Stacked Borrows 与 Tree Borrows 两套模型都判 UB**，已拆掉——改成"每帧现搭、就地
> 用完就丢"，代价就是上面那 **6 次/拍**（16 k 是 2 次/拍）。**上游公开面没有"既安全又零新增"的路**：
> `FrameBlocker` 只收 `&[Vec<&[f32]>]`，把借用表放进 `self` 会被借用规则挡回（E0521/E0506）。
> 现状：`crates/vox-dsp/src/` 里 `unsafe` **零命中**（`grep -rn unsafe crates/vox-dsp/src` 无输出）。

### 7.4 验收命令

```bash
# 芯 + DSP（含 T1–T12）
cargo test -p vox-core -p vox-dsp

# 只跑 AEC 那组
cargo test -p vox-dsp --test aec_synthetic
cargo test -p vox-core pipeline::chain::tests      # T5 / T8 / T9 / T12

# 差分台不许回归（S4-B B0 起的金样轨迹，今天是 **20 份** = 既有 16 + W5 新开 4；
# 模块名是 `pipeline::golden`，写成 `pipeline::tests::differential` 会一条都不匹配却返回 ok
# ——verifier 2026-10-10 踩过）
cargo test -p vox-core pipeline::golden

# 全门（收口前）
cargo fmt --all --check && cargo test --workspace && cargo clippy --workspace --all-targets
```

---

## 8. 改动清单

> owner 口径：`core-dev` = `vox-core` / `vox-dsp`；`shell-dev` = 平台外壳 / 装配层 / UI。

### 8.1 `crates/vox-core`（core-dev）

| 文件 | 动作 | 内容 |
| --- | --- | --- |
| `src/ports.rs` | **改** | 加 `pub trait Aec`（§5.2 的逐字签名）；模块头补一条"参考信号是**播放率**、上行是**采集率**" |
| `src/composition.rs` | **改** | `Op::Aec` 变体（**无字段**：配置走 `settings`，不进清单——`Gate` 有字段是因为它是用户可调的，而 AEC 的开关已经是能力位 + 设置两项，别再加第三份）；`Op::kind()` 返 `"aec"`；`OP_ORDER` 改 5 元素 `["mono","denoise","aec","gate","resample"]`。**`requirement()` 不动**（§1.5 已核实它不映射 ops） |
| `src/capability.rs` | **改** | `HwAec = 19` / `SoftAec = 20`、`ALL` / `HOST` / `id()`、**修 `scope()`**、测试里 `19 → 21`（§4.1 逐行给了） |
| `src/pipeline/speak.rs` | **改** | `composition()` 里 `if config.aec && bits.contains(Capability::SoftAec) { ops.push(Op::Aec) }`，位置在 `Denoise` 之后（§3.1） |
| `src/pipeline/listen.rs` | 不改 | Listen 不装 `Op::Aec`（§3.3） |
| `src/pipeline/chain.rs` | **改（已落地）** | `ChainStage::Aec { port: Option<Box<dyn Aec>>, rate: u32, skip: Option<AecSkip> }`——**`port` 是 `Option`（订正）**：降级两态（`AecSkip::{Rate,Factory}`）要求"装了这一节但没有 port"，与既有 `Denoise` 同形；`ChainStage::process` / `out_rate` / `name` / `on_disconnect` 各加一个臂；`Chain::build` 里处理 `Op::Aec`（走 `deps.aec` 工厂，降级两态照抄 `DenoiseSkip`）；**新增 `Chain::push_playback_reference`**（§5.1） |
| `src/pipeline/mod.rs` | **改** | `pub type AecFactory`；`Deps` 加 `aec` 字段；`Worker::handle_server_event` 的 `AudioDelta` 分支加那一行；`Worker::boot` 照抄 `DenoiseSkip` 的两条日志口径；`Worker::feed` 跑链（`Chain::run` 内部已经按 `stages` 走，AEC 节自己处理） |
| `src/settings.rs` | **改** | `speak.aec: bool`（缺省 `true`），按 RULES #5 迁移全部读写方（settings 持久化 + 两个入口的默认值） |

### 8.2 `crates/vox-dsp`（core-dev）

| 文件 | 动作 | 内容 |
| --- | --- | --- |
| `src/lib.rs` | **改（已落地）** | 加 `pub mod aec;`；依赖加在 `crates/vox-dsp/Cargo.toml` 的普通 `[dependencies]`（**订正**：原文写 `[target.'cfg(...)'.dependencies]`，实情是普通依赖）：`sonora-aec3 = "=0.2.0"` + `sonora = "0.2"`（只要它的 `three_band_filter_bank`）。W0 已核实：纯 Rust、无 `-sys`/`cc`/`cmake`/`bindgen`，aarch64 交叉编译零额外动作 |
| `src/aec.rs` | **新增（已落地）** | `SonoraAec`：`impl ports::Aec`；**三级流（20ms→10ms→频带→子帧→块→子帧→全带）**、`ThreeBandFilterBank` 48 k 拆/合、**余块抽干时序**、**int16 域换算（×32768）**、`REF_RING_MS = 200` 的 `RefRing` 环形缓冲（**没有 D0**）、24k→48k 线性插值、`config::Delay` 调参、`reset`。**所有缓冲建链期分配**（§5.3-5.4），热路径按 T6 的口径（§7.3）。**订正**：原文这一行还写着"+ D0"，D0 没落（§9 W1-f 的订正） |
| `tests/aec_synthetic.rs` | **新增（已落地）** | T1–T4、T6、T7、T13、T14 + 一条 16 kHz 单频带的旁证（装置见 §7.1）；链上那组 T5/T8–T12 在 `crates/vox-core/src/pipeline/chain.rs::tests` |

### 8.3 平台外壳 / 装配层（shell-dev）

| 文件 | 动作 | 内容 |
| --- | --- | --- |
| `crates/vox-host/src/entry.rs`（**订正**：原稿写 `crates/vox-host/src/dsp.rs`，那个文件不存在） | **改（已落地）** | `aec: vox_dsp::ports::aec_factory()`（`Deps` 第 6 个工厂，与既有 `denoise_factory` / `resample_factory` 同形；工厂本体在 `crates/vox-dsp/src/ports.rs::aec_factory`） |
| `crates/vox-audio-alsa/src/registry.rs` | **改（已落地）** | `same_card()` / `card_of()`（§4.3 B2 的逐字形状）；设备目录条目带上 card name |
| `crates/vox-headless/src/platform/linux.rs`（**订正**：原稿写 `crates/vox-audio-alsa/src/platform.rs`，那个文件不存在） | **改（已落地）** | `host_facts()` 按 B1–B4 填 `HostFacts::off`（§4.3 那张表）：`soft_aec_unavailable_reason()` 采事实 + 纯函数 `soft_aec_reason_from(factory_ok, same_card, rate_ok)` 映射 |
| `crates/vox-audio-alsa/src/playback.rs` | **改（已落地）** | `PlaybackStats::device_latency_ms` **真填上**了：`pcm.delay()`（`snd_pcm_delay`）→ `latency_ms(delay, rate)` 存进 `device_latency_ms`。W0 之后这条的定位下调：D0 主要影响 CPU 档与漂移余量，不影响"AEC 能不能收敛" |
| `vox-host` / 两个入口 | **改（已落地）** | 工厂走**一处**：`vox_host::entry::HostPorts::deps` 里 `aec: vox_dsp::ports::aec_factory()`（原稿写"`vox-audio-win` / `-linux` 各注入一次"——落成共享宿主层里的同一行，两档入口不再各写一份） |
| `app/ui/src/i18n/*` | **改（已落地）** | `hw_aec` / `soft_aec` 两个 key（三语）；Speak 设置里 AEC 开关的文案（**句子在界面，芯里不带**，`capability.rs` 模块头纪律 3） |
| `app/ui` | **改（已落地）** | `HostCapability` 联合补齐 13 位 + AEC 开关 + 按 `soft_aec` 位降级（**位假时整个入口不渲染 + 原位一句带 reason 的说明**，`.omp/AGENTS.md` 硬约束"不许灰掉之后什么都不说"）。**各档今天这一位报什么见 §4.3 的落地回填**（桌面两档硬写 `not_wired` ⇒ 桌面今天看不到这个入口；无屏档按判据真算 ⇒ 可能为真） |

### 8.4 文档（docs）

| 文件 | 动作 | 内容 |
| --- | --- | --- |
| `docs/architecture/DIRECTIONS.md` | **改** | §8 冲突表加一行：S4-B §2.7 的"aec 落在 gate 与 resample 之间" → **本稿改为"落在 denoise 与 gate 之间"**，理由见本稿 §3.1 |
| `docs/plans/S4-EMBEDDED-REFACTOR.md` | **改** | **§2.2 的三档表**（不是 `docs/platform/EMBEDDED.md`——那份的 §2 是"硬件做不到什么"，没有 §2.2 也没有三档表；verifier 2026-10-10 核）：把"软件 AEC"那一行的"候选 SpeexDSP（轻）/ WebRTC AEC3（好但重），选型进 S4-C 细化稿"改成**选 `sonora-aec3`**，并注明 **48 kHz 三频带 ≈ 25 dB 上限、且随时延下滑**（W0 实测 §6.3/§6.4） |
| `docs/platform/EMBEDDED.md` | **改**（可选） | §3.2"音频：还是 PipeWire"已被 S4-C-ALSA 的 ALSA 缺省推翻（DIRECTIONS §8 第 17 行），本稿不重复改；若要顺手改，只补一句"回声消除见 `docs/plans/S4-C-AEC.md`" |
| `crates/vox-core/src/composition.rs` 模块头 | **改** | `OP_ORDER` 的注释补一句"aec 为什么在 gate 之前"（`chain.rs` 模块头 C1–C6 是这种"契约写在最显眼处"的口径，跟上） |
| `tools/aec-spike/`（**新增，不属于任何 workspace**） | **新增** | W0 尖峰本体。独立 crate，`[workspace]` 空表脱离父 workspace；只依赖 `sonora-aec3 = "=0.2.0"` 与 `sonora = "0.2"`，**无 `[patch]`、无 `vendor/`**；`.gitignore` 忽略 `/target/` |

---

## 9. 工单拆分

> 每张工单收口时 `cargo fmt --all --check && cargo test --workspace && cargo clippy --workspace --all-targets` 全绿。
> 依赖：`W1` 与 `W2` 可并行（不同文件）；`W3`/`W4` 依赖 `W1` 的 `Aec` trait 定型；`W5` 依赖 `W2` 的位。

> **落地回填：既有用例改动清单（2026-10-12 按提交核）**
> 稿子的收口口径（§4.1 的"同步要改（缺一即红）"表里，测试改动**只列了 `19 → 21` 一处**；
> §4.3 也只讲判据、没提既有用例）与实情不符：**既有用例真改了 9 处**（W1 三处 + W5 六处）。
> **每一处都是契约真变**（清单多了一节 / 规定顺序多了一格），不是为了让测试过而改测试：
> - **W1（`5b0ec73`）改了 3 处**：`composition.rs::the_manifest_schema_is_generated_from_the_type`
>   与 `vox-mcp/tests/protocol.rs::the_composition_cell_is_generated_from_the_manifest_type`
>   里那份 `("Op", …)` 期望值（同一份契约在两处、两条都改）——
>   `["mono","denoise","gate","resample"]` → `[...,"aec",...]`；以及
>   `composition.rs::speak_manifest_names_the_virtual_mic` 的 ops 期望值**暂时**换成字面量
>   （W1 时 `OP_ORDER` 已有 `aec`、但 Speak 清单还没有这一节，见 W1 与 W5 的分界）。
> - **W5（`521cff7`）改了 6 处**：
>   `composition.rs::{the_manifest_round_trips, the_manifests_match_the_design_documents_instances,
>   speak_manifest_names_the_virtual_mic, op_order_must_be_a_subsequence_of_the_canonical_order}`
>   ——清单多一格 `aec`，凡按**下标**找门/找 op 的地方一律改成**按 `kind` 找**；
>   `pipeline/mod.rs::the_playback_reference_reaches_the_aec_stage_before_the_sink`
>   （不再手插 `Op::Aec`，改由"设置 + 事实"推清单，否则 W5 之后会有两个 aec ⇒ `BadOpOrder`）；
>   `vox-mcp/tests/endpoints.rs::editable_is_the_design_table_and_excludes_the_gate_cells`
>   （同上：`ops[2]` 写死下标 → 按 `kind` 找门）。
>   另**新增**两条：`speak.rs::the_aec_stage_needs_both_the_bit_and_the_setting`、
>   `pipeline/mod.rs::the_playback_reference_goes_to_the_primary_sink_only`。
> - **W2（`5bc1271`）只改了 `capability.rs` 里那个 `19 → 21`**（§4.1 已列）。

> **W0 已完成（2026-10-09）**，产出物是 `tools/aec-spike/`（独立 crate，不进 workspace）
> 与本稿 §6 的全部实测数字。它关掉了原 W1 的第 0 步，并把 W1 的范围按真实接口重写。

### W0 · 尖峰：最小可用调用序列 + ERLE 实测（**已完成**）

**owner: core-dev** ｜ 状态：**2026-10-09 完成**

**做了什么**：`tools/aec-spike/`（独立 crate，`[workspace]` 空表脱离父 workspace，根
`Cargo.toml` 未改；只依赖 `sonora-aec3 = "=0.2.0"` 与 `sonora = "0.2"`，均在 crates.io 上，
**无 `[patch]`、无 `vendor/`**）。跑通块级最小调用序列，出 ERLE 曲线 / 稳态值 / 收敛时间 /
双讲近端保留 / 幅度阶梯 / 时延扫描 / CPU，`cd tools/aec-spike && cargo run --release` 一条命令。
`cargo check --target aarch64-unknown-linux-gnu` 一次通过。

**产出与结论**：全部在 §6。五条必须照抄的：
① 调用序列是 3 频带 × 80 子帧 → 64 块，每 4 次插入必须抽干余块；32 kHz 公开面驱不动。
② **输入是 int16 域**，[-1,1] 的 f32 会连时延都估不出来。
③ **48 kHz 三频带 ≈ 25 dB（随 D 跌到 12 dB）；16 kHz 单频带 ≈ 62–72 dB。**
④ 双讲吃近端 ~10 dB；无回声时不误伤。
⑤ CPU 16 k ≈ 29 µs、48 k ≈ 53 µs（x86-64 墙钟，随负载漂）；外部时延估计器更便宜（16 k ~一半、48 k ~四分之一）。

**判定**：不满足"停手换库"的条件（稳态 ERLE < 10 dB 或不收敛）⇒ **`sonora-aec3` 保留不换**。

### W1 · `Op::Aec` 的链上接线 + `SonoraAec` 实现 + 验收测试 T1–T14

**owner: core-dev** ｜ 依赖：无（W2 可并行；**W0 已完成，不再是前置**）

**第 0 步（照抄 W0，不再探索）**：

```bash
cargo tree -p vox-dsp | grep -iE "aec3|sonora"      # K3：依赖树长什么样（预期纯 Rust）
cargo build -p vox-dsp                              # 当前工具链（1.98.1）编不编得过 MSRV 1.91
```

**内容**（范围已按 W0 的真实接口重写，比原稿多出"分频 + 重组 + 块时序"三块）：

| 序号 | 内容 | W0 依据 |
| --- | --- | --- |
| W1-a | `ports::Aec`（§5.2 逐字）；模块头补"**实现内部要把 f32 [-1,1] 换算到 int16 域**" | §6.2 |
| W1-b | `Deps::AecFactory` + `Op::Aec` + `OP_ORDER` 5 元素 | 不变 |
| W1-c | `ChainStage::Aec`（含 `AecSkip` 两态）+ `Chain::push_playback_reference` | 不变 |
| W1-d | `vox-dsp/src/aec.rs::SonoraAec`：**20 ms → 2×10 ms 帧 → `num_bands`×160 → 2×80 子帧 → 64 块 → 子帧 → 全带**，48 k 走 `ThreeBandFilterBank` 拆/合，**余块每 4 次抽干** | §6.1 |
| W1-e | **幅度换算 `×32768`**（`push_reference` 与 `process` 两条路都要） | §6.2 |
| W1-f | `REF_RING_MS` 参考环形缓冲 + 24 k→48 k **线性插值**升采样 | §5.3/§5.4，W0 已判"不上 sinc"。**订正（2026-10-12 按代码核）**：原稿这一行写的"+ **D0**（读 `PlaybackStats`）"在 §5.2 定的 trait 面上**不可实现**——`ports::Aec` 只有 `process` / `push_reference` / `reset` 三个方法，**没有接收 `PlaybackStats` 的通道**。**D0 留给"省 CPU 档"与 W4**（W0 已判：D0 只影响 CPU 档与漂移余量，不影响 AEC 能不能收敛） |
| W1-g | 外部 vs 内建时延估计器的档位：**缺省内建**（ERLE 无差别），把 `set_audio_buffer_delay()` 留给"省 CPU 档" | §6.4 |
| W1-h | `tests/aec_synthetic.rs`：T1–T4、**T6（分配护栏）**、T7、**T13（int16 域）、T14（48 k 频带上限 18–35 dB）** | §7.1/§7.3 |

**注意 32 kHz**：`BlockProcessor` 支持 32000，但 2 频带的 QMF 分频器在 `sonora` 里是
`pub(crate)`。W1 **只支持 16000/48000**，其余率照 `AecSkip::Rate` 降级（T9 已有）。

**验收**：

```bash
cargo test -p vox-dsp --test aec_synthetic        # T1–T4、T6、T7、T13、T14
cargo test -p vox-core pipeline::chain::tests      # T5 / T8 / T9 / T12
cargo test -p vox-core pipeline::golden               # 金样逐字相等（+goldens_are_deterministic）
```

**边界**：W1 **必须一次落完**。只加口子和枚举、把 `ChainStage::Aec` 的实现留成直通
就是 RULES #4 说的空钩子。同理，"先只加 trait 不接链"也不行。
**W0 的尖峰代码不搬进 `vox-dsp`**——它是一次性探针，`tools/` 是它的家。

### W2 · `hw_aec` / `soft_aec` 两个能力位

**owner: core-dev** ｜ 依赖：与 W1 并行

**内容**：`capability.rs` 的逐行改动见 §4.1；`hw_aec` **任何一档的 `host_ceiling` 都不加**（§4.2）；
`soft_aec` **进三档上限**（§4.3）；i18n 两个 key。

> **订正（2026-10-12 按提交核）**：原稿在这儿写的"`HostFacts::uninjected()` 里 `soft_aec`
> 报 `NotWired`（照 `VirtualMic` 的先例）"**当时与其它行互斥**——位不在**任何**档位的上限里
> 的时候往 `off` 里塞它，就是外壳**谎报**（`HostFacts::excess_off_bits()` 会把它当外壳 bug 报出来，
> 既有钉子用例会红）。W2 那一版正是这么处理的：`uninjected()` **一个字未动**
> （见 `5bc1271` 的提交说明）。**实情是"位进上限之后才补这条"**：`(SoftAec, NotWired)` 随
> ALSA-T7/W3（`217ffe5`）进 `uninjected()`，与 `soft_aec` 的三档上限**同一轮**落地。
> 它是对的：位进了上限、外壳不报它就是 fail-open（`effective()` 会算成"开"）。

**验收**：

```bash
cargo test -p vox-core capability::tests
```

必须全绿，且新增/修改的用例覆盖：`ALL.len() == 21`；两位落在 `HOST` 桶里（`scope()` 修好了）；
`hw_aec` **四档 `host_ceiling` 都不含**（报告恒 `off(unsupported)`）；`soft_aec` 的落位**按最终口径**
（进 `Windows` / `LinuxDesktop` / `LinuxHeadless` 三档、不进 `Android`，见 §4.3 + W3）；
`excess_off_bits()` 对"外壳谎报 `hw_aec`"仍能报出外壳 bug。
**订正（2026-10-12 按代码核）**：稿子这句原来把两位写在一起（"四档都不含它们"）——W2 当时确实如此
（`soft_aec` 还没有定义者），但 **W3 给 `soft_aec` 开了三档上限**；今天的钉子用例是
`capability::tests::the_aec_bits_are_host_bits_with_soft_aec_only_in_the_three_wired_tiers`
（逐档断言 `hw_aec` 不在、`soft_aec` 只在三档）。

### W3 · ALSA 侧判据与 `host_facts()`

**owner: shell-dev** ｜ 依赖：W1 的 `Aec` trait 定型 ｜ 状态：**已落地**（2026-10-12 按代码核；
与 W2 同轮，提交 `217ffe5`，那一轮同时把无屏档入口切到 `vox-audio-alsa`——ALSA-T7）

**内容（已落地，两处订正）**：`registry.rs` 的 `same_card()`；
**判据落在 `crates/vox-headless/src/platform/linux.rs::host_facts`**（原稿写的
`crates/vox-audio-alsa/src/platform.rs` 不存在，见 §4.3 的落地回填）；
`playback.rs` 真填 `device_latency_ms`（`pcm.delay()`）；
`aec_factory()` 由 `vox_host::entry::HostPorts::deps` 统一注入（不再由两档入口各写一份）。

**验收**（本轮门禁实测，2026-10-12，`main` @ `e62cfdc`）：

```bash
cargo test -p vox-audio-alsa registry::tests    # same_card 的真值表
cargo test -p vox-audio-alsa -- --ignored        # snd-aloop 回环：lib 4 + aloop_roundtrip 4 全过（本机真回环）
cargo test --workspace                           # 941 passed / 0 failed / 14 ignored
```

### W4 · 播放汇的前置量稳定化（**这才是 W1 在真板子上能不能成的关键**）

**owner: shell-dev** ｜ 依赖：W3 ｜ 状态：**未落地**（2026-10-12 按代码核——本轮唯一没做的一张工单）

**内容**：AEC 开启时，播放汇的 `DropRing` 排队量要**近恒定**（现在"满了丢最旧"意味着
排队量可以在 0 到满之间任意跳，直接打在 §5.3 的 D0 上）。做法是把取样点改成
"追一个固定的排队目标"，并把 `queued_samples` 报上来给 `Aec` 算 D0。

**W0 给 W4 加了一条验收口径**：48 kHz 三频带下 ERLE 随时延 D 单调下滑
（§6.4：28.7 dB @0 ms → 18.5 dB @200 ms → 12.7 dB @400 ms）。所以"排队量漂移"不只是
D0 的问题，直接决定 §7 的 15 dB 守不守得住。W4 的判据因此从"相关峰漂移 ≤ ±10 ms"
**加严为"排队量折合的等效 D 漂移 ≤ ±60 ms"**（对应 ~2 dB 的 ERLE 余量）。

**验收**：`#[ignore]` 的 `#[test]`：播一段已知信号，同时采集，对拍
"参考 vs 采集"的互相关峰值**在整个 10 s 里漂移不超过 ±10 ms**。漂移更大就红。

> 这张工单是本稿**最可能返工**的一张（见 §10 R4）。W1 在 x86 上用合成信号能过，不代表
> W4 在真板子上能过——中间隔着一个我们目前没有的数据：**目标板 `DropRing` 的实际抖动**。
> W0 只证明了"纯增益 + 纯时延的回声路径下 AEC 能收敛"，没有证明"有混响、抖动、
> 非线性的真实路径下也能"。

### W5 · 清单接线 + 界面

**owner: core-dev（清单/settings）+ shell-dev（UI）** ｜ 依赖：W1 + W2 ｜ 状态：**已落地**
（2026-10-12 按代码核，提交 `521cff7`）

**内容**：`speak::composition` 按 `soft_aec` + `config.aec` 装 `Op::Aec`（§3.3）；
`settings.speak.aec` 按 RULES #5 迁移全部读写方；界面开关 + 按位降级。
**落地回填**：两道闸在 `crates/vox-core/src/pipeline/speak.rs::composition`
（`if config.aec && bits.contains(Capability::SoftAec)`）；`SessionConfig.aec` 在
`crates/vox-core/src/runtime.rs`（**Listen 腿恒 `false`**）；差分台新开 4 条场景
（`pipeline/golden/{speak_48k_aec_running,speak_44k_aec_rate_skipped,speak_48k_aec_off,speak_48k_aec_bit_off}.txt`
+ `golden.rs::aec_section` 的 `# aec` 段：`factory_calls` / `process_calls` / `resets` / `stages`）；
界面 13 位 + AEC 开关按位降级（`app/ui/src/capabilities.ts` / `sections/PipelineCard.tsx` /
`scripts/check-capabilities.mjs`）——**降级口径按 §4.3 的落地回填**：桌面两档 `soft_aec` 硬写
`not_wired`（桌面今天看不到那个入口），无屏档按 B1–B4 真算（**可能为真**），`Android` 不在上限。

**验收**（本轮门禁实测，2026-10-12，`main` @ `e62cfdc`）：

```bash
cargo test -p vox-core composition::tests    # 有位/无位各产出一份清单，ops 顺序逐条相等
cargo test -p vox-core pipeline::golden      # 20 份金样（既有 16 份字节未动 + W5 新开 4 份）
cd app/ui && npm run verify                  # exit 0（12 步）
```

三条变异各自都有金样变红（`speak.rs::the_aec_stage_needs_both_the_bit_and_the_setting` 钉两闸、
`pipeline/mod.rs::the_playback_reference_reaches_the_aec_stage_before_the_sink` 钉接线顺序）。

---

## 10. 风险与未决

| # | 风险 / 未决 | 影响 | 怎么办 |
| --- | --- | --- | --- |
| **R1** | **A53 1 GHz 上的 CPU 仍然没数。** `sonora` 只给了 **M4 Max** 上整条 APM 的 13.3 µs/10 ms；aarch64/Android/iOS 在 README 里只是 **CI 目标**，不是基准平台。按频率倒推的"≈13% 单核"**仍是外推** | 若实际 >40%，A53 上跑不动 | W0 在 x86-64 上只跑了 AEC3 + 胶水：16 k ≈ 29 µs、48 k ≈ 53 µs（0.29%–0.53% 单核），**不能与 README 的数字直接比**。W1 交付的是**自计时**（S4 §2.5-1 已接进算子链，`ChainStage::name()` 给行名），有板子时一张 `Snapshot` 就知道。**在那之前不写任何"够用"的结论**（概要稿 §5 明确禁止） |
| **R2** | `sonora-aec3` 只有两个版本（0.1.0 / 0.2.0），**0.2.0 投产 2 个月**；README **并未承诺 API 稳定性**（这是本稿的推断——README 里没有"演进中 / unstable / breaking"这类表述，也没有承诺稳定） | 升级可能破 API | 这就是 K1 的理由：实现藏在 `vox-dsp::aec` 后面，`Aec` trait 是我们的。退路 = 自己在 `vox-dsp` 里接 `speexdsp`（只改一个文件）。**W0 又加了一条：真实接口是频带/块级且吃 int16 幅度域，这两个"不知道就写不出来"的知识只能是 `vox-dsp::aec` 的内部知识，换实现的成本因此比表面更高** |
| **R3** | **MSRV**：README 说 MSRV **1.91**；本仓**没有 `rust-toolchain.toml`**（Glob 核过，零命中），workspace `edition = "2021"`、无 `rust-version`。**verifier 核实：CI 用的是 `dtolnay/rust-toolcache@stable`（`ci-arm64.yml`）⇒ 构建没有版本下限 ⇒ 不可复现，而不是"可能被顶出去"** | 不是"编不过"，是"不同机器编出来的版本不一样" | W1 第 0 步 `cargo build -p vox-dsp` 一次（当前工具链 1.98.1，远超 1.91，**R3 已降级为"可复现性"问题，不再是阻塞项**）。若要让 `sonora-aec3` 有固定下限，可行解是**给 `vox-dsp` 加 `rust-version`**（core-dev 的地盘），不是给全仓加 `rust-toolchain.toml`（那要另找 Main 排期）。**注意：换 `aec3` 0.4.0 规避 MSRV 这条路不成立**（本仓没下限，无处可规避） |
| **R4** | **播放汇排队抖动是本设计最大的未知数。** `DropRing` 满了丢最旧 ⇒ 排队量 0..满 任意跳 | D0 跟不上 ⇒ **48 k 下 ERLE 随 D 单调下滑**（W0：28.7 dB @0 ms → 18.5 dB @200 ms → 12.7 dB @400 ms）⇒ **T1 那种合成测试全绿、真板子上一条都没消** | W4 那张工单就是对冲的，判据已加严到"等效 D 漂移 ≤ ±60 ms"。**本稿不假装已经解决它**——§5.3 的 D0 机制在"排队量恒定"这个前提上才成立，而那个前提由 W4 保证。W0 只证明了"纯增益 + 纯时延路径下能收敛" |
| **R5** | ~~AEC3 的延迟搜索窗口折合多少毫秒~~ **已关闭（W0 §6.4）** | — | 内建估计器自己跟上 0–400 ms（误差 ≤ 1 格 4 ms）。`default_delay = 5` blocks 折合 **20 ms**（`BLOCK_SIZE_MS = 64×1000/16000 = 4`，可直接从源码读出）。外部喂 D0 不给 ERLE 收益，只省 CPU |
| **R6** | ~~24 k→48 k 线性插值会不会把 ERLE 拉低~~ **已关闭（W0 §6.4）** | — | 实测掉 1.1–2.4 dB，**够不到 3 dB 判据 ⇒ 维持线性插值，不上 sinc** |
| **R7** | **`hw_aec` 恒假，等于软件 AEC 是唯一档** | 会议全向麦用户拿不到硬件档 | 有意为之（§4.2：没有定义者不许报开）。等有探测手段再补 |
| **R8** | **48 kHz 三频带这条路本身值 ~46 dB 的账（W0 新账）。** `ThreeBandFilterBank` 往返只有 **9.92 dB SNR**，与参考落在哪个频带无关 | **产品形态（48 k 采集）下 ERLE 上限 ≈ 25 dB，且随时延下滑**；想拿 60+ dB 必须换成 16 kHz 单频带档 | 已在 §7.2 把阈值定到 15 dB 并与 `REF_RING_MS = 200` 绑定。"要不要让 `Op::Aec` 内部在 16 kHz 上做"是**产品级取舍**，见 §11"待裁决" |
| **R9** | **半双工门与 AEC 同时装**（S4-C 另一张工单）的相互作用 | 半双工时 AEC 还在跑 → 白烧 CPU；或两者都压回声 → 双重处理 | 本稿**不拍**，但给一条约束：清单里**两者都装时，Gate 仍是最后一道**（门后是干净的上行），AEC 收窄到只在门开时自适应。**具体形态等那张工单** |
| **R10** | `AecSkip` 的日志/通知口径要不要逐字照抄 `DenoiseSkip` | 差分台会红 | 本稿定：**照抄**。`DenoiseSkip::Rate` 只 `warn!`、不发 `Notice`；`DenoiseSkip::Factory` `warn!` + `Notice::warning`。AEC 两态沿用同一口径，不发明新的 |
| **R11** | **`Op::Aec` 无字段**（§8.1）——但用户可能想手动调 D0 / 滤波器档 | 配置无处放 | 本稿决定：**不进清单**。真要暴露，等有了板子、知道了哪些参数有用再说（`settings` 里加，不动清单）。W0 已证明"D0 主要影响 CPU 档与漂移余量，不影响能否收敛"⇒ 这条的紧迫性比原稿估的更低 |
| **R12** | **32 kHz 采集率驱不动公开的块级 API**（W0 新账） | 若某个板子的驱动只能给 32 kHz，`Op::Aec` 只能降级 | W1 只支持 16000/48000，其余率走 `AecSkip::Rate`（T9）。真要 32 k：自己写 2 频带 QMF 分频（父 crate 的 `SplittingFilter` 是 `pub(crate)`），或整条走父 crate 的 APM |
| **R13** | **Agent 面（MCP 控制面）今天开不了回声消除**——`crates/vox-mcp/src/endpoints.rs::SPEAK_CELLS` 是 **9 格**，`ops` 那一格只有 `ops[denoise]`；`has_op` **已经**支持 `"aec"`（`endpoints.rs::has_op` 的 `Op::Aec => wanted == "aec"`），但**没有任何 Cell 用它** | `compose_endpoint` 是**唯一**的设置写入口 ⇒ 无屏档 / 自动化路径**改不了** `speak.aec`（只能手工改 `config.json`）。桌面界面那条腿不受影响（走 Tauri 设置写） | **本轮的选择：显式记下"不进 compose"**（不是漏做）——理由是：① `speak.aec` 是**必须重启才生效的启动格**（与 `denoise` 那格的性质不同，后者可以热改）；② 补这一格要同步改 `docs/plans/S1-AGENT-FACE.md` 的设计表与 `vox-mcp/tests/endpoints.rs` 的 editable 断言（9 → 10），**那是 S1 那一轮的地盘**。另一条路（供将来选）：补一格 `ops[aec] 有无 → settings.speak.aec`，与 `denoise` 那格**同形**，一次改三处（Cell 表 + S1 设计表 + editable 断言，9 → 10） |

## 11. 与既有决策的关系

| 决策 | 出处 | 本稿的处置 |
| --- | --- | --- |
| `Op::Aec` 落在 `gate` 与 `resample` 之间 | `S4-B-OP-CHAIN.md` §2.7 | **推翻**。改到 `denoise` 与 `gate` 之间。理由见 §3.1（门用能量判开合，AEC 在门之后等于没装）。S4-B §9 已把这条列为 **O-1 待 S4-C 定**，本稿正是那个 S4-C |
| `Chain::push_playback_reference` 与门控口同层、唯一调用点在 `AudioDelta` 分支 `sink.push()` 之前 | `S4-B-OP-CHAIN.md` §2.7 | **沿用**，一字不改（§5.1） |
| 口子与它的第一个实现同一张工单同一轮落地 | `S4-B-OP-CHAIN.md` §2.7 | **沿用**：W1 一次落完（§9） |
| 门在重连时**不动**、只有 `Denoise`/`Resample` 清 | `S4-B-OP-CHAIN.md` §2.6 / 契约 C6 | **延伸**：AEC 要清、门仍不动。差别理由见 §5.6 |
| 软件 AEC 候选 SpeexDSP / WebRTC AEC3 | `S4-EMBEDDED-REFACTOR.md` §2.2 | **细化**：两者都不直接用，选**纯 Rust 的 AEC3 移植** `sonora-aec3`（§2.3） |
| 三档（硬件 / 软件 / 半双工），半双工兜底 | 概要稿 §2.2 | **沿用**，并把 `hw_aec` 定为恒假（§4.2）——**这一档今天没有定义者** |
| 设计下限 Cortex-A53 四核 1 GHz / 512 MB，算子缺省最便宜档 | 概要稿 §2.3 | **沿用**：Speex 之所以出局，第 1 条理由就是构建线（§2.3-1） |
| 同声卡 ⇒ 共用时钟、时延稳定 | 概要稿 §2.2 | **收窄**（§4.3 B2 的边界说明）：本稿只保住了"**频率不漂**"，**"相位不动"没有保证**，那部分由 W4 兜 |

**按 `docs/architecture/DIRECTIONS.md` §8「新者胜」，本稿（S4-C，09-30）比 S4-B（09-30）新，
且冲突项 S4-B 自己标了 O-1 待定，故推翻成立。** §8 冲突表的那一行已由 Main 在收口时补上
（第 19 行，2026-10-10）。

### 11.1 M1：`Op::Aec` 在哪个率上做 —— **已裁决：维持 48 kHz（选 (a)）**

| # | 事项 | W0 数据 | 两条路 |
| --- | --- | --- | --- |
| **M1** | **`Op::Aec` 要不要在 16 kHz 上做？** | 48 kHz 三频带 23–25 dB（且 D 越大越差，400 ms 只剩 12.7 dB）；16 kHz 单频带 **62–72 dB**，且不随 D 变 | (a) **维持 48 k**（照 §3.1 的顺序，AEC 在 `Resample` 之前、吃 48 k 全带）：改动最小，上限 25 dB；(b) 让 `Op::Aec` 内部降采样到 16 k 跑完再升回 48 k：多一次 3×/1× 重采样（A53 上是真成本），换 60+ dB 的 ERLE，并且**会推翻 §3.1 的"AEC 必须在采集率做"**（那里论证的是"别等重采样之后再消，否则宽带声学尾看不见"——16 k 仍然看得见 0–8 kHz 的绝大部分，那条论证要重写） |

> **裁决（Main，2026-10-10）**：**选 (a)，`Op::Aec` 维持 48 kHz 做。**
> 理由：改动最小、不与任何已拍板的顺序冲突；48 kHz 三频带的 ~25 dB 上限与"随时延下滑"
> 这两条已由 W0 量清，§7 的 15 dB 阈值就是照它定的。**16 kHz 档不砍，但也不是现在做**：
> 等 **W4 在真板子上拿到数据**（排队量抖动、真实声学路径）之后**另开工单**再评估，
> 不由本稿夹带。**W1 因此按 (a) 施工，W1-d / W1-f 的形状不变。**

### 11.2 已按裁决执行、留档的一条

`vendor/` 不入库，尖峰改成纯 crates.io 依赖：`tools/aec-spike/` 只依赖
`sonora-aec3 = "=0.2.0"` 与 `sonora = "0.2"`，**无 `[patch]`、无 `vendor/`**；
干净目录里 `cargo run --release` 数秒（verifier 2026-10-10 量到 5.0 s 编译 + 运行）出全部数字。
将来若某个依赖必须钉 git 版本，再用 `[patch]` 并写明原因；**在那之前不引 vendor。**

---

## 附录 · 引用与核查口径

- **现状代码证据**：2026-09-30 在 worktree `s4c-aec-design`（分支 `s4c/aec-design`）读的。
  §1.1 的 grep **实跑过**，输出贴在正文。其余是 `Read` 过的文件内容，位置按符号名给。
- **调研事实**：全部走一手来源（crates.io 官方 API、GitHub 官方仓库、docs.rs、Speex 官方手册），
  逐条链接在 §2.2 表下。
- **`sonora-aec3` 源码事实**（W0 期间直接读的，registry 里的 `sonora-aec3-0.2.0/src/`）：
  - `frame_blocker.rs` / `block_framer.rs` / `block_processor.rs` / `common.rs` / `render_delay_buffer.rs`
    / `matched_filter.rs` / `echo_path_delay_estimator.rs` / `render_delay_controller.rs`
    / `decimator.rs` / `transparent_mode.rs` **逐篇读过**，§6 的每条数字都能对上源码里的常量。
  - `sonora-0.2.0/src/echo_canceller3.rs` 是"块级路径该怎么串"的上游参照；
    `sonora-0.2.0/src/splitting_filter.rs` 证实用的是同一只 `ThreeBandFilterBank`，
    `audio_processing_impl.rs` 证实用的是 `SplittingFilter`。
  - 父 crate 的 `lib.rs` 只导出 `config` / `high_pass_filter` / `stats` / `three_band_filter_bank`
    四个模块 ⇒ **2 频带的 QMF 分频器拿不到**（R12）。
- **W0 尖峰代码**：`tools/aec-spike/`（独立 crate，`[workspace]` 空表脱离父 workspace，根
  `Cargo.toml` 未改）。不引 `rand`（自带 xorshift32），不读文件，不连声卡。
- **仍然没有跑过的东西**（本稿明确不编）：Cortex-A53 上的任何数字、`snd-aloop` 回环、
  真实扬声器-麦克风对（有混响 / 非线性 / 漂移）。**这些是 W4 的地盘。**
- **没有做过 Rust 工具链以外的实测**这一条只对 2026-09-30 那一轮成立；W0 之后，
  §6 的 x86-64 数字是实测，A53 相关的一律外推。
