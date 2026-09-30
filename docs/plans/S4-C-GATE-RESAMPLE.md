# S4-C · 半双工门 + 廉价重采样档 · 施工设计稿

> 状态：**[设计稿·未开工]**。对应 [`S4-EMBEDDED-REFACTOR.md`](S4-EMBEDDED-REFACTOR.md) §3「S4-C 新模块」表里
> 「半双工门」与「廉价重采样档」两行，§2.2（回声消除三档）、§2.3（轻量化与 A53 设计预算），以及 §4 的两行验收。
>
> 上游：[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md)（算子链已落地，本稿按**代码**写，不按那稿的预想写）、
> [`S4-C-ALSA.md`](S4-C-ALSA.md) §5.2（"能不重采样就不重采样"）与 §12（M1–M6 拍板）。
> 方向：[`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §8 第 16/17 行、§10.9 第 4/7 条。
>
> 口径：代码与本稿打架以代码为准。引用**优先给符号名**，行号是 **2026-09-30** 在分支 `s4c/gate-design`
> （HEAD = `6fbb6c8`）核到的、写稿当时的值（`.omp/AGENTS.md`「引用要耐久」、`.omp/RULES.md` #11）。
> 本轮**实跑过**的只有 `tools/bench-dsp`（§3.1）与一份 `/tmp` 临时基准（§3.1 表二）；`cargo test` /
> `clippy` **未跑**，凡涉及"跑出来的结果"的地方一律注明跑没跑（`.omp/RULES.md` #10）。

---

## 0. 一句话

**半双工门 = 给 `ActivationGate` 加一个"按采集率样本倒数的压制窗口"，压制窗口在"刚把译音推进播放汇"的那一刻
由 `Worker` 量着"设备里还剩多少毫秒没放"装满；清单侧落在 `GateConfig` 的一个新字段上，桌面档一个字节都不变。
廉价重采样档 = `vox-dsp` 加一档**只对升采样方向生效**的线性插值实现，装配期由**入口外壳**选，
`ports.rs` / `vox-core` / 清单 / 能力位**一行都不动。**

两件事**互不重叠文件**（§5），可以并行派。

### 0.1 本稿对上位稿的收窄与推翻（`DIRECTIONS.md` §8「新者胜」）

| # | 上位稿写的 | 本稿改成什么 | 为什么 |
| --- | --- | --- | --- |
| **N1** | §2.2「在现有 `vox_core::gate::ActivationGate` 上加"**播放中**输入」 | 不是 `bool` 输入，是**一个按采集率样本倒数的计数器**（`ActivationGate::set_mute_for_ms(u64)`） | ① 门内部**没有时钟**（它连 `tail_ms` / `preroll_ms` 都是按 `self.sample_rate` 换算成样本数的，见 `gate.rs::samples_for_ms`），加一个 `bool` 就得给它塞一个时钟进来；② 差分台要的是**与墙钟无关的确定性**（`golden.rs` 模块头把两个 `Instant::now()` 来源都排除了），时钟会让新金样依赖 `clock.advance` 的时机；③ 计数器让整条逻辑能被 `gate.rs` 的单测单独钉住，不牵动 `Worker` |
| **N2** | §2.3「重采样加一档廉价实现（**线性/多相**）」 | **只做线性、且只对升采样方向**；**多相 FIR 这一档不做** | §3.1 实测：降采样方向上多相 FIR **不比 rubato 便宜**（P=32×T=32：0.63 vs 0.50 ms/s；T=48：1.00 vs 0.50），而线性便宜 12 倍但**混叠 0 dBFS**。升采样方向上镜像落在输出带外，喇叭上听不见，线性便宜 12.7 倍 |
| **N3** | §2.2「回声消除三档**按能力位自动选**」 | 半双工**不新增能力位**，判据落在清单已有的两格上（麦克风采集 + 有出声的播放出口 + 无屏档） | 这一档只由"哪一档构建"决定，而"哪一档构建"已经是 `HostKind` 这条硬事实（`composition.rs::HostKind`），再加一位是同一件事的第二份表达。**加位的时机是 AEC 落地时**（三档互斥需要一个共同的选档入口），见 §2.5.4 |
| **N4** | §2.2 半双工「`Op::Gate` 可配」 | 配置落在 **`GateConfig` 的新字段**上（`Op::Gate` 仍然是同一格，**不加新 `Op`、不加新 `GateKind`**） | 半双工与 `Manual` / `Level` 是**正交**的，不是第三种门；加成 `GateKind` 就得表达 `HalfDuplex×{Manual,Level}` 的叉积。见 §2.5 |

### 0.2 本稿没有推翻的

S4-B 的 **INV-1**（任何 `Note` 都不许重建算子链）、**INV-2**（断线不重建链）、契约 C1–C6、
`DenoiseSkip` 的两支路可观察行为、15 份金样"逐字不变"——**一条都不动**，本稿的全部设计都与它们相容（§2.5.5、§4.1）。

---

## 1. 现状证据（2026-09-30 核，行号写稿当时的）

### 1.1 门：`ActivationGate` 的形状与状态机

全部在 `crates/vox-core/src/gate.rs`（384 行）。

| 事实 | 符号 / 行号 |
| --- | --- |
| 两种门：`Manual` / `Level` | `GateKind`（`:19`） |
| 参数是 `Copy` 的四格：`kind / threshold / tail_ms / preroll_ms` | `GateConfig`（`:27-33`） |
| 两个预设：`MANUAL`（tail 150 / preroll 100）与 `level(t)`（tail 600 / preroll 200） | `GateConfig::MANUAL`（`:37`）、`GateConfig::level`（`:48`） |
| 门的**状态**：一个 `config` + 一个 `sample_rate` + `active` + `silent_samples` + preroll 缓冲 + `external_active` | `ActivationGate`（`:99-107`） |
| **门里没有时钟**：时长一律按 `sample_rate` 换算成样本数 | `ActivationGate::samples_for_ms`（`:235`） |
| 手动门的**收尾边**：从激活转为未激活那一拍发一段 `tail_ms` 的零样本，`ended = true` | `process_manual`（`:175-184`） |
| 电平门的收尾：静音期**照常放行**直到 tail 走完，`ended` 在最后一拍 | `process_level`（`:202-219`） |
| preroll 是 `VecDeque<Vec<f32>>`，上升沿 `accept` 时一次冲出来 | `prebuffer`（`:104`）、`accept`（`:240-248`） |
| `set_config` 复位内部状态但**保留 `external_active`**，且有专门的钉子用例 | `set_config`（`:136-141`）、`tests::set_config_keeps_external_active`（`:377`） |
| `reset()` 会把 `external_active` 清成 `false`（所以断线路径**不许**调它） | `reset`（`:143-149`）；`chain.rs::Chain::on_disconnect` 的注释与实现（`:396-406`） |
| 既有 5 条单测，全在 `gate.rs::tests` | `:291-383` |

**要点**：`process` 的分派是 `match self.config.kind { Manual => …, Level => … }`（`:164-167`），
**新逻辑要插在 `match` 之前**（`gate.rs:161-163` 那两行 RMS 计算之后），这样两个 kind 天然被一起覆盖，
不需要在两个分支里各抄一遍。

### 1.2 播放汇在哪、`push` 在哪调 —— **与门在同一个线程**（本稿最关键的一条现状）

| 事实 | 符号 / 行号 |
| --- | --- |
| 播放汇是 `Worker` 的字段 `sink` / `monitor_sink`，**不是链上的一节** | `pipeline/mod.rs::Worker`（`:654-655`） |
| 译音进播放汇的**唯一**生产点：`ServerEvent::AudioDelta` 分支里连续两次 `sink.push(&samples)` | `Worker::handle_server_event`（`:1291-1298`） |
| 直通模式下也有一次 `sink.push(block)`，但那是**麦克风原声** | `Worker::feed_chain`（`:1143-1151`） |
| `push` 与门 `ActivationGate::process` **跑在同一个 `Worker` 线程上**：`pump`（`:960`）里先 `feed_chain`（走链、过门），后 `drain_messages`（→ `handle_server_event` → `push`）；`handle_message`（`:1246`）与 `feed_chain` 都是 `&mut self` 方法 | `pump`（`:960-1007`） |
| `sink.push` 跑在**同一个 `&mut self`** 上 ⇒ 不需要 `Arc` / 原子 / 任何跨线程同步 | 同上 |
| "播放汇里还压着多少毫秒"**已经有一个函数算它** | `Worker::playback_queue_ms`（`:921-931`），用 `sink.stats().queued_samples / channels / sample_rate` |
| `PlaybackSink::stats()` 的 `queued_samples` 是"环里还没被渲染线程取走的样本数"；`rendered_samples` 是渲染线程累计取走的 | `ports.rs::PlaybackStats`（`:113-124`）、`PlaybackSink::stats`（`:105-107`，**有默认实现**，测试替身可以只填一部分） |
| 现有播放侧遥测**已经在**用 `stats()`：`Worker::poll_playback`（`:935-950`）读 `rendered_samples` | 同上 |

**⇒ 推论（本稿全部设计的地基）**：

1. "正在播"这件事**在芯里是现成的、零分配的、同一线程的事实**，不需要新通道、不需要 `Arc<AtomicU64>`、不需要 `Cell`。
2. 播完没播完**也已经有一个测法**（`playback_queue_ms`），本稿复用同一个式子。
3. S4-B §2.7 给 AEC 预留的参考信号口（`Worker::handle_server_event` 的 `AudioDelta` 分支、`sink.push` 之前）
   与本稿的"标记正在播"落点是**同一处**——两者要串行派（都改 `pipeline/mod.rs`），见 §5.3。

### 1.3 算子链的接线点（S4-B 已落地的代码，以代码为准）

| 事实 | 符号 / 行号 |
| --- | --- |
| 链执行器 | `pipeline/chain.rs::Chain`（`:251`，只有 `stages: Vec<ChainStage>` 一个字段） |
| 四种节，枚举分派 | `ChainStage`（`:110-135`）、`ChainStage::process`（`:141-172`） |
| 门这一节把 `ActivationGate` **整个搬进来**，另带一个 `status: Option<GateStatus>` 槽位 | `ChainStage::Gate { gate, rate, status }`（`:123-129`） |
| 建链时门建在**采集率**上，参数逐字取清单的 `Op::Gate { config }` | `Chain::build`（`:308-315`） |
| 门的三个外部口**都在 `Chain` 上**，`Worker` 不直接碰门 | `Chain::set_gate_active`（`:438`）、`set_gate_config`（`:447`）、`take_gate_status`（`:456`） |
| 链尾率 / 无门时的 `flush_resample` / 断线只清 `Denoise`+`Resample` | `Chain::out_rate`（`:418`）、`flush_resample`（`:379`）、`on_disconnect`（`:396`） |
| **`chain.rs` 里没有 `OpTiming` / 自计时**（S4-B B3 未落地） | `Chain`（`:251-253`）只有 `stages`；`runtime.rs` 里 `grep op_timings` 零命中 |
| 门的四个外部口在 `Worker` 侧的调用点 | `boot`（`:817` `set_gate_active`）、`handle_note`（`:1420` / `:1426`）、`feed_chain`（`:1130` `take_gate_status`） |

### 1.4 `GateConfig` 的下游（决定"加一个字段"要动哪些地方）

| 下游 | 符号 / 行号 | 加字段的影响 |
| --- | --- | --- |
| **本机派生**：`Runtime::gate_for(&Settings)` 从设置折算成 `GateConfig` | `runtime.rs:689-693` | 用构造函数（`MANUAL` / `level(t)`）→ **加字段不用改这里**（前提：`MANUAL` / `level()` 两个预设自己填新字段） |
| 会话配置 | `runtime.rs::SessionConfig.gate`（`:50`） | 不动 |
| 控制面命令 | `runtime.rs::PipelineCommand::SetGateConfig { config }`（`:80`）、`runtime.rs:643` 的自动派发 | 不动（`GateConfig` 整体传） |
| 清单 JSON | `composition.rs::Op::Gate { config: GateConfig }`（`:178`） | ⚠️ **有两条逐字比 JSON 的既有测试**：`composition.rs::tests::the_manifests_match_the_design_documents_instances`（`:783`），里面的期望值是 `"config": {"kind":"level","threshold":0.012,"tail_ms":600,"preroll_ms":200}`（`:798`、`:835`）。⇒ **新字段必须 `skip_serializing_if`**，否则这两条红 |
| 前端类型 | `app/ui/src/types.ts::GateState`（`:35-43`，`snake_case` 联合类型） | 加 `GateState` 变体要同步加一行（`types.ts`） |
| 前端渲染 | `grep` 全 `app/ui/src`：**`live.gates` 只在 `app/ui/src/store.tsx:104` 被写，没有任何组件读 `gate.state`** | 不用加 i18n、不用改组件 |
| 账本事件 | `runtime.rs::on_gate_status`（`:1021`）→ `PipelineStatus.gate` → `app/src-tauri/src/dto.rs:121` 的 `PipelineSnapshotDto` | `GateStatus` 加字段会改 DTO 的线上形状 ⇒ **本稿不给 `GateStatus` 加字段**（见 §2.2.4） |

### 1.5 重采样：一个工厂同时服务上行与播放侧（**这一条决定了 R 那张工单只有 4 个文件**）

| 事实 | 符号 / 行号 |
| --- | --- |
| 芯只认一个工厂签名 | `pipeline/mod.rs::ResampleFactory = Box<dyn Fn(u32, u32) -> Box<dyn Resample>>`（`:85`） |
| 唯一实现是 `SincFixedIn`（128 点 sinc + cubic 插值，`f_cutoff 0.95`，oversampling 128） | `vox-dsp/src/resample.rs::sinc_params`（`:19-27`）、`Resampler::new`（`:51-76`） |
| 同率时零开销透传 | `ResamplerInner::Passthrough`（`:36`）、`process`（`:90`） |
| **一个工厂被四个地方用**，其中两个是**播放端口内部**的率转换 | `vox-dsp/src/ports.rs::resample_factory`（`:45-47`）→ ① `app/src-tauri/src/lib.rs:208`（`Deps.resample`，上行）② `app/src-tauri/src/platform/win.rs:51` / `linux/audio.rs:19`（`WinPlayback` / `LinuxPlayback` 内部）③ `crates/vox-headless/src/headless.rs:248`（`Deps.resample`，上行）④ `crates/vox-headless/src/platform/linux.rs:34`（`LinuxPlayback` 内部） |
| **ALSA 播放端口的率转换也是注入进来的工厂**，不自己造 | `vox-audio-alsa/src/playback.rs:77`（`AlsaPlayback::new(resample_factory)`）、`:140`（`Some((self.resample_factory)(source_rate, rate))`）；三档阶梯的档 3 装的就是它（`S4-C-ALSA.md` §5.2） |
| 端口 trait `Resample` 的三个方法 | `ports.rs::Resample`（`:143-150`）：`process -> Vec<f32>` / `flush -> Vec<f32>` / `reset` |
| 既有 5 条单测全在 `vox-dsp/src/resample.rs::tests` | `:159-236` |
| `Resampler::new` 的**全部**调用方 | `vox-dsp/src/ports.rs:46`、`vox-audio-linux/examples/smoke.rs:175`、`vox-audio-alsa/src/playback.rs:797` 与 `:884`（两处都在 `#[cfg(test)]`/`#[ignore]` 用例里）、`tools/bench-dsp/src/main.rs:214/225/236/314/316` |

**⇒ 换档只需要换"入口装的那个工厂闭包"**，播放端口侧一行都不用动（它拿的是注入进来的 `ResampleFactory`）。
`ports.rs` 与 `ResampleFactory` 的签名**一个字都不用改**。

### 1.6 差分台现状（S4-B B0 落的 15 份金样）

| 事实 | 符号 / 行号 |
| --- | --- |
| 场景表 15 条，id 与顺序照 `S4-B-OP-CHAIN.md` §6.4 | `pipeline/golden.rs::SCENARIOS`（`:1022-1050`） |
| 逐字比对用 `include_str!`（缺文件 = 编译错）；`VOX_GOLDEN_REGEN=1` 覆写 | `verify!` 宏（`:406-...`） |
| 清单是由**账本**在 `Start` 时派生的（`Plan::build(&config, &runtime.host_facts())`），夹具注入的是 `HostFacts::all_wired(HostKind::Windows)` | `pipeline/mod.rs:244`、`pipeline/mod.rs:1964` |
| 15 份金样的 Speak 场景：`output_device = Some("CABLE Input")`、`voice = Some("Tina")`、`monitor_translation = false` ⇒ 角色是 **`virtual_mic`**（`all_wired(Windows)` 里 `Capability::VirtualMic` 开着） | `golden.rs::start_speak`（`:110-131`）、`speak::composition` 的 `role` 那一段 |
| 假播放汇的 `stats()` **真的报了 `queued_samples`**（= `played.len()`，只被 `flush` 清空）与 `rendered_samples`（一个测试可拨的计数器） | `pipeline/mod.rs::SpeakerHandle::stats`（`:1828-1839`）、`Speaker::rendered`（`:1817`） |
| `LatencyChanged` 的**内容**不进金样（有两个墙钟来源） | `golden.rs` 模块头、`render`（`:370-380`） |

---

## 2. 半双工门

### 2.1 一句话机制

**译音被推进播放汇的那一刻**，`Worker` 量出"设备里还剩多少毫秒没放"（两个出声口取大），
加上配置的尾巴，换算成**采集率下的样本数**交给门；门在那之前一直照常工作，
从那一拍起**整块不放行**（连 preroll 都不进），计数走完才回到自己那套逻辑。

压制期间如果门本来是开着的，**照手动门的收尾边**发一段零样本尾巴并 `ended = true`（触发服务端断句）。

### 2.2 目标形状

#### 2.2.1 `gate.rs` 新增的类型与字段

```rust
// crates/vox-core/src/gate.rs

/// **半双工门**：回声消除三档里的保底档（概要稿 §2.2 第三行）。
///
/// 外放的译音是我们自己播的，麦克风会把它录回去。硬件 AEC / 软件 AEC 拿不到时，
/// 这一档的做法是**播译音期间关掉上行，播完再压一段尾巴才重新开门**。
/// 代价是**会切断正在说的话**——这是"保底"两个字的实价，界面要照实说。
///
/// 这一档与 [`GateKind`] 的两种门**正交**：`Manual` 与 `Level` 照旧决定"什么时候算在说话"，
/// 本结构只决定"这段时间里一律不许上行"。所以它是 [`GateConfig`] 的一个字段，不是第三种门。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HalfDuplex {
    /// 播放停下之后还要压多久才重新开门（ms）。
    ///
    /// 压的是**房间里的回声尾巴**：最后一声译音出喇叭之后，麦克风还要再听一会儿
    /// 自己那间屋子的反射。默认 300 ms 的来由见 S4-C 稿 §2.3。
    pub tail_ms: u32,
}

impl HalfDuplex {
    /// 缺省尾巴。**改这个值会重录 `speak_48k_speaker_half_duplex` 那份金样**——这是它该干的活。
    pub const DEFAULT_TAIL_MS: u32 = 300;

    pub const fn with_tail_ms(tail_ms: u32) -> Self {
        Self { tail_ms }
    }

    /// 清单 / `Composition::of` 用的缺省实例。
    pub const DEFAULT: Self = Self { tail_ms: Self::DEFAULT_TAIL_MS };
}
```

```rust
// GateConfig 加一格（gate.rs:27）。**serde 两个属性缺一不可**：
//   `default` —— 老清单里没有这个键，要能读回来（`Composition` 是可反序列化的数据）。
//   `skip_serializing_if` —— 为假时**不写这个键**，否则 composition.rs:798/:835 那两条
//   逐字比 JSON 的既有测试会红（S4-A/S4-B 立下的"既有测试逐字未改"纪律）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GateConfig {
    pub kind: GateKind,
    pub threshold: f32,
    pub tail_ms: u32,
    pub preroll_ms: u32,
    /// 半双工门。`None` = 不装这一档（**桌面缺省**），门的行为与今天逐字相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_duplex: Option<HalfDuplex>,
}
```

两个预设各自填新格（`gate.rs:37` / `gate.rs:48`），`Default for GateConfig`（`gate.rs:58`）走 `MANUAL`：

```rust
pub const MANUAL: Self = Self { kind: GateKind::Manual, threshold: 0.012,
                               tail_ms: 150, preroll_ms: 100, half_duplex: None };
// level() 同理补一行 half_duplex: None
```

**不给 `GateStatus` 加字段**：`GateStatus` 会进 `PipelineSnapshotDto` 的线上形状
（`app/src-tauri/src/dto.rs:121`），加字段等于改前端拿到的 JSON。状态由新 `GateState` 变体表达就够。

```rust
// GateState 加一变体（gate.rs:67）。serde rename_all = "snake_case" ⇒ 线上是 "playback_mute"。
    /// 半双工门：正在外放译音（或还在回声尾巴里），上行整块压住不放行。
    PlaybackMute,
```

#### 2.2.2 门里的状态与新入口

```rust
pub struct ActivationGate {
    config: GateConfig,
    sample_rate: u32,
    active: bool,
    silent_samples: usize,
    prebuffer: std::collections::VecDeque<Vec<f32>>,
    prebuffer_samples: usize,
    external_active: bool,
    /// **半双工压制窗口：还剩多少个采集样本不许上行。`0` = 上行开着。**
    ///
    /// 单位是**采集率下的样本数**，不是毫秒、不是 `bool`：门里没有时钟
    /// （`samples_for_ms` 的既有约定），而差分台要求与墙钟无关的确定性。
    /// `None`（`config.half_duplex`）时这一格恒为 0，`process` 里那个分支根本进不去。
    mute_samples: usize,
}

impl ActivationGate {
    /// 装满压制窗口：`ms` 毫秒 = `ms × sample_rate / 1000` 个采集样本（**取上界**，
    /// 宁可多压一点；窗口只会被后面的 `set_mute_for_ms` 重新装满，不会自己变长）。
    ///
    /// 门**没装** `half_duplex` 时这一句就是空操作（`mute_samples` 恒 0）——
    /// 桌面档为此**一个整数都不多算**。
    pub fn set_mute_for_ms(&mut self, ms: u64) {
        self.mute_samples = if self.config.half_duplex.is_some() {
            ((self.sample_rate as u64).saturating_mul(ms) / 1000).min(usize::MAX as u64) as usize
        } else {
            0
        };
    }
}
```

#### 2.2.3 `process` 里的压制分支（插在 `match self.config.kind` **之前**）

```rust
    pub fn process(&mut self, samples: &[f32]) -> (Vec<Vec<f32>>, GateStatus) {
        if samples.is_empty() { /* 原样照抄 gate.rs:153-159 */ }

        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();

        // —— 半双工：新增，插在这两行之后、原 `match self.config.kind` 之前 ——
        if self.mute_samples > 0 {
            self.mute_samples = self.mute_samples.saturating_sub(samples.len());
            // 压制**刚开始**的这一拍：把 preroll 倒掉，压制期间也不往里存。
            // 理由见 §2.4.3——冲出来的会是我们自己译音的回声。
            self.prebuffer.clear();
            self.prebuffer_samples = 0;
            if self.active {
                // 门本来是开着的：照手动门的收尾边断句（gate.rs:175-184 逐条同构，
                // 尾巴用 `config.tail_ms`——**不新增字段**）。
                self.active = false;
                self.silent_samples = 0;
                let tail = vec![0.0f32; self.samples_for_ms(self.config.tail_ms).max(1)];
                return (vec![tail], self.status(GateState::PlaybackMute, rms, false, true));
            }
            return (Vec::new(), self.status(GateState::PlaybackMute, rms, false, false));
        }

        match self.config.kind { /* 一行不动 */ }
    }
```

**热路径代价**（`half_duplex` 为假时也是）：`if self.mute_samples > 0` —— 一次 `usize` 比较，
50 次/秒/腿，**可忽略**。`prebuffer.clear()` 掉的是 `VecDeque` 的元素（保留已分配的 `Vec`），
只在压制期间发生，不在稳态热路径上。

#### 2.2.4 `reset` / `set_config` 的处置

- `reset()`（`gate.rs:143`）**照旧清干净**，包括 `mute_samples = 0`——"reset 就是清干净"的既有语义不为例外。
- `set_config`（`gate.rs:136`）在 reset **之后把当前压制窗口装回去**，与它今天保留 `external_active` 的写法同构：

```rust
    pub fn set_config(&mut self, config: GateConfig) {
        let external_active = self.external_active;
        self.config = config;
        let mute = self.mute_samples;
        self.reset();
        self.external_active = external_active;
        self.mute_samples = mute;   // ← 新增这一行
    }
```

  为什么：换门参数是一条 `Note`（`handle_note` 的 `Note::GateConfig`，`pipeline/mod.rs:1422`），
  它**必须**在压制中途也不许把上行放开了。有专门的钉子用例（§4.3 的第 6 条）。
  注意换门**不会**改变压制窗口的长度（`config.half_duplex` 变 `None` 时下一拍起压制分支就进不去了，
  `set_mute_for_ms` 也会在下次调用时归零——这一格由"重新装窗口"的那一处负责，不由换门负责）。

### 2.3 尾巴时长：怎么定、谁配

| 问题 | 答案 |
| --- | --- |
| 谁配 | **清单**：`Op::Gate.config.half_duplex.tail_ms`。本机派生时由 `speak::composition` 填 `HalfDuplex::DEFAULT`（§2.5.2）；外部提交的清单（S1 / Agent 面 / 无屏档的配置文件）可以自己填。**不进 `Settings`**（理由见 §2.5.3） |
| 生效范围 | 整条会话（`Chain::build` 时进 `ActivationGate`），**运行期不热改**（改清单 = 新会话 = `Stop`+`Start`，与 INV-1 不冲突，见 §2.5.5） |
| 缺省 300 ms 的来由 | ① 门本来就有两个尾巴量级可参照：`MANUAL.tail_ms = 150`（断句用）、`level().tail_ms = 600`（断句用），半双工尾巴夹在中间；② 面对面轮流说的一轮约 500–800 ms，尾巴每多 100 ms 就少接得上半句；③ 常见办公室的混响 RT60 在 300–500 ms，尾巴短于它就等于没关。**这是一个估算值，不是实测值**——`[未核实]`：真机上到底要多少，取决于箱子与房间，得等有板子时听（见 §6.3 Q1） |
| 与 `config.tail_ms` 的关系 | **两个不同的东西，别混**：`config.tail_ms` 是"门关掉时发多少零样本去触发服务端断句"（`gate.rs:179`），`half_duplex.tail_ms` 是"播完之后压多久才重新开门"。§2.2.3 的收尾边用前者，压制窗口用后者 |

### 2.4 与手动门 / 电平门的组合语义

#### 2.4.1 决策表

| `kind` | 压制中（`mute_samples > 0`） | 压制刚结束（下一拍） |
| --- | --- | --- |
| `Manual`（按键） | 整块不放行；`external_active` **原样保留**（用户可能还按着） | 若还按着 → `accept()` 上升沿，冲 preroll（此时 preroll 是空的，见 §2.4.3）+ 当前块；**用户压制期间说的话一律丢掉** |
| `Level(0.0)`（Listen 的恒开门） | 整块不放行 | 立刻恢复恒放行（Listen 走进程环回，**根本不该装半双工**，见 §2.5.2） |
| `Level(t > 0)` | 整块不放行（**不评估 RMS**） | 从这一拍起重新按 RMS 判；`silent_samples` / `active` 在压制开始的收尾边已清零，等于从零开始 |

#### 2.4.2 三条正交性保证（都要有用例钉住）

1. **压制分支在 `match self.config.kind` 之前** ⇒ 两种 kind 被同一条路径覆盖，不存在"只在 Manual 上生效"的写法。
2. **压制分支不碰 `external_active`** ⇒ 用户松手/按住的状态跨越压制不变。
3. **压制分支不碰 `config`** ⇒ 换门参数只影响压制**结束之后**的判据，不影响压制本身。

#### 2.4.3 压制期间为什么不填 preroll、且开始时要清空（一条要写进代码注释的决策）

三个选项：

| 选项 | 后果 |
| --- | --- |
| **A. 照常填 preroll**，重新开门时冲出来 | preroll 是 `preroll_ms`（Manual 100 / Level 200）长度的环形缓冲，**冲出来的全是压制窗口末尾的音频**。而压制窗口的**前半段设备还在放译音**（窗口 = 队列里剩下的毫秒 + 尾巴），那几毫秒麦克风听到的是**我们自己的译音**。⇒ 重新开门的第一件事就是把 100–200 ms 的译音回声上传给服务端。这一档存在的意义当场被推翻 |
| **B. 压制开始时清空 + 期间不填**（**本稿采纳**） | 重新开门后听到的第一块是压制窗口**之外**的干净音频。代价：用户起音最多被削掉 `preroll_ms`（≤200 ms）。但他是在**听完译音之后**才开口的，preroll 那点提前量本来也不够用（爆破音在起音后 10–20 ms 内） |
| C. 只在压制**结束时**清空 | 等价于 A（冲出来的还是压制期间的音频），且多一次清空 | 

**采纳 B。** 一处 `clear()`、一处"不调 `push_prebuffer`"，两个都是既有方法，**零新增状态**。

### 2.5 清单与能力位怎么表达

#### 2.5.1 为什么是 `GateConfig` 的新字段（不是新 `GateKind`、不是新 `Op`）

| 备选 | 为什么不选 |
| --- | --- |
| 新 `GateKind::HalfDuplex` | 半双工与 `Manual`/`Level` **正交**（§2.4 的决策表）。加成 kind 就得表达 `HalfDuplex × {Manual, Level}` 的叉积：3 个 kind、6 份参数、两条状态机。而且 S4-B 的 `Op::kind()`（`composition.rs:188`）→ `ChainStage::name()`（`chain.rs:186`）→ 自计时的行名，全是按 `Op` 的种类取的，新增 kind 会牵动那条链 |
| 新 `Op::HalfDuplex` 一节 | 它**不改样本**：没有自己的 `process(&[f32]) -> Vec<f32>` 语义，不满足链的契约 C1/C2（`S4-B-OP-CHAIN.md` §2.2）。而且它需要的是"播放参考信号"，那正是 S4-B §2.7 给 AEC 留的口子（`Chain::push_playback_reference`）——同一张嘴开两次不如开一次。⇒ **AEC 与半双工若同时在链上，应共用一个口**（§6.3 Q2） |
| **`GateConfig` 新字段**（**本稿采纳**） | ① `Op::Gate` 已在 `Composition::OP_ORDER`（`composition.rs:415`）里，清单不用动；② `Chain::build`（`chain.rs:308-315`）已经**逐字**把 `Op::Gate.config` 喂给 `ActivationGate::new`（S4-B B2 修掉了 D1/D2/D3 那个"清单说了代码不听"的缺陷），本稿只是让这一格里多一个字段被读；③ `ChainStage` **不加变体** ⇒ 执行器的 `match` 一个字不动；④ `GateConfig` 是 `Copy`，热路径代价是 §2.2.3 的一次比较 |

#### 2.5.2 本机派生：什么时候自动开

判据全部落在**清单里已有的两格**上，**不新增能力位**（理由见 §2.5.4）：

```rust
// crates/vox-core/src/pipeline/speak.rs::composition

    // —— 纯代码位移：把构造 `ops` 的那一段整体挪到构造 `out` 的那段之后 ——
    // 两段互不依赖（`ops` 只看 passthrough / config.denoise / config.gate），
    // 而半双工的判据要看 `out` 里有没有"会出声的播放出口"。挪完之后判据**直接读 `out`**，
    // 不另写一份推导（另写一份就是第二份真源）。
    // ⚠️ 挪动是纯位移：不改任何一行逻辑，`OP_ORDER` 管的只是 `ops` 数组的顺序，
    //    与 `out` 在清单里的相对位置无关。

    let audible = out.iter().any(|output| matches!(output,
        Output::Playback { role: PlaybackRole::Speaker | PlaybackRole::Monitor, .. }));

    let mut gate = config.gate;
    // 半双工门（保底档）：只在"麦克风采集"+"译音真的会从房间里放出来"+"无屏档"三条同时成立时开。
    if audible && facts.host == HostKind::LinuxHeadless {
        gate.half_duplex = Some(HalfDuplex::DEFAULT);
    }
    ops.push(Op::Gate { config: gate });
```

三个条件各自的理由：

| 条件 | 为什么必须有 |
| --- | --- |
| `audible`（角色是 `Speaker` 或 `Monitor`） | 译音进 `VirtualMic`（VB-CABLE / PipeWire sink）时声音在**线缆里**，不在房间里，麦克风听不到 ⇒ 无回声、无需关门。`Monitor`（回听）例外：它进的是系统默认设备，是**真出声**的 |
| 采集是麦克风 | 走 `Input::Mic` 才有"房间"这回事。Listen 抓的是**进程环回**（数字源），根本录不到回声 ⇒ `listen::composition` 一个字都不改 |
| `facts.host == HostKind::LinuxHeadless` | **这一条是纯保守，为了不动已经发货的桌面产品。** 桌面 Linux 用户若没装 VB-CABLE，角色会退成 `Speaker`（`speak::composition` 的 `role` 那一段），此时开了半双工就是**在用户没要求的情况下切断他说话**。概要稿 §3 的原则是"先搭骨架、行为不变，再插新模块"；无屏盒子是本轮目标（§10.9 第 1 条），桌面保持逐字不变 |

**逃生口**：这一格在清单里，外部提交的清单可以自己开或关（`Op::Gate.config.half_duplex`），
S1 / Agent 面 / 无屏档的配置文件都能写，**零代码改动**。所以"无屏档自动开"是**缺省**，不是**唯一入口**。

**直通模式**（`passthrough`，关翻译）自动排除：`out` 里的播放出口 `source` 是 `EdgeSource::Chain`
且此时没有 `session`，`audible` 可能为真，但 §2.2.2 的 `mark_playing` **只从
`handle_server_event` 的 `AudioDelta` 分支调用**——直通时那个分支根本不会被走到，
所以不会出现"把原声推出去时反过来把自己关掉"的自噬。**两道保险**：判据侧（要有 `session`）+
调用侧（只认译音）。

#### 2.5.3 为什么不进 `Settings`、不做运行期切换

- `GateConfig` 的既有路径就是"设置 → `Runtime::gate_for`（`runtime.rs:689`）→ `SessionConfig.gate` → 清单"。
  要让用户能开关，就得在 `Settings` 加一格（`speak.echo_guard: Off | Auto | On`），
  外加 `gate_for` 的一支、三份 i18n、一个界面开关、**以及一条运行期怎么换档的答案**。
- 而换档在运行期是**做不到**的：`Deps.resample` 那种"装配期定死"的东西还好说，
  这里是清单里的一格，清单在 `Start` 时由 `Plan::build` 派生成（`pipeline/mod.rs:244`），
  运行期唯一的门控口是 `Note::GateConfig`（INV-1 允许的），它改的是 `ActivationGate.config`——
  **改 `half_duplex` 字段恰好是 `set_config` 能做的、不重建链的事**（§2.2.4）。
  ⇒ 技术上"用户热开关"是零成本的；**本稿不做的理由是它会让半双工在没有耳机/扬声器信息的机器上乱切话**，
  而这一轮的目标机器（无屏盒子）压根不需要这个开关。留作后续：`[未决]` §6.3 Q3。

#### 2.5.4 为什么不新增能力位（以及什么时候该补）

- 能力位模型的三条纪律（`capability.rs` 模块头）：**位 = 事实、由真的把路打开的那段代码负责、
  芯只给 `(位, reason)` 句子不在芯里**。
- "要不要开半双工"这件事**已经**完全由 `HostKind`（一条硬事实）+ 清单形状决定，没有第三个信息源。
  再加一位 `Capability::HalfDuplex`，`host_ceiling` 的四张表（`capability.rs:388-424`）、
  `Capability::ALL` / `HOST` / `id()`（`:82`/`:105`/`:148`）、JSON schema 的 properties 钉子用例
  （`capability.rs:828`）、界面 i18n 全要动，换来的是"把 `HostKind` 再说一遍"。
- **什么时候该补**：AEC 落地时。三档（`hw_aec` / `soft_aec` / 半双工）是**互斥的三选一**，
  选档入口应当是 `Composition::of`（`composition.rs:425`）里的一处 `match`，
  那时才值得把"这机器有硬件 AEC"做成位。本稿的 `audible && host == LinuxHeadless`
  到那时**整条替换**掉（§6.3 Q2）。

#### 2.5.5 与 INV-1 的相容性（逐条核）

`S4-B-OP-CHAIN.md` §3.4 的 **INV-1 = 任何 `Note` 都不许重建算子链**。

| 核对项 | 结论 |
| --- | --- |
| 半双工要不要新的 `PipelineCommand`？ | **不要。** 它是每拍从播放汇现算出来的事实，不是用户命令 |
| 要不要新的 `ChainStage` 变体？ | **不要**（§2.5.1）⇒ `Chain` 的 `stages: Vec` 长度与形状不变 |
| 要不要新的 `Op` 种类 / 改 `OP_ORDER`？ | **不要** ⇒ `Composition::validate`（`composition.rs:444-462`）的子序列校验不变 |
| 门在热路径上还是装配期建的？ | 装配期（`Chain::build` → `ActivationGate::new`，`chain.rs:311`），与今天一致 |
| 用户改门参数（`Note::GateConfig`）会重建链吗？ | 不会，走 `Chain::set_gate_config` → `ActivationGate::set_config`（`chain.rs:447`），与今天逐字相同 |
| 断线（`Chain::on_disconnect`）会清掉压制窗口吗？ | 不会——`on_disconnect`（`chain.rs:396-406`）只清 `Denoise` / `Resample`，**门一动不动**（INV-2）。压制窗口是一个 `usize`，重连后接着数，**这正是要的**：断线时译音可能还在播 |

### 2.6 播放状态从哪来（零分配，逐行）

```rust
// crates/vox-core/src/pipeline/mod.rs

/// 某个播放汇里还压着多少毫秒没放出去。没开或率/声道是 0 时给 0。
///
/// **本函数是 `Worker::playback_queue_ms` 的逐字搬移**（原 `pipeline/mod.rs:921-931`），
/// 差别只是从"只看主出口"变成"传进来的这一个"，返回值逐字相同 ⇒ 延迟快照一个数都不变。
fn sink_queue_ms(sink: &dyn PlaybackSink) -> u64 {
    let stats = sink.stats();
    if stats.sample_rate == 0 || stats.channels == 0 {
        return 0;
    }
    let frames = stats.queued_samples / stats.channels as usize;
    (frames as u64).saturating_mul(1000) / stats.sample_rate as u64
}

impl Worker {
    fn playback_queue_ms(&self) -> u64 {
        match self.sink.as_ref() {
            Some(sink) => sink_queue_ms(sink.as_ref()),
            None => 0,
        }
    }

    /// **半双工**：译音刚被推进播放汇，把门的压制窗口重新装满。
    ///
    /// 窗口 = 设备里还没放出来的毫秒（`stats().queued_samples` 量出来的真值）
    ///        + `half_duplex.tail_ms`（房间里的回声尾巴）。
    ///
    /// **为什么在 `push` 之后量**：量的是"这一刻起还要播多久"，所以必须是推完之后。
    /// **为什么在同一个 `&mut self` 上**：`handle_server_event` 与 `feed_chain` 同线程（§1.2），
    /// 零原子、零 `Arc`、零 `Cell`。
    /// **两条出声的路取大**：主出口 + 回听（回听也进系统默认设备，是真出声的）。
    ///
    /// 门没装半双工 → 第一句就返回，**桌面档一个整数都不多算**。
    fn mark_playing(&mut self) {
        let Some(tail_ms) = self.chain.echo_guard_tail_ms() else {
            return;
        };
        let mut ms = 0u64;
        if let Some(sink) = self.sink.as_ref() {
            ms = ms.max(sink_queue_ms(sink.as_ref()));
        }
        if let Some(sink) = self.monitor_sink.as_ref() {
            ms = ms.max(sink_queue_ms(sink.as_ref()));
        }
        self.chain
            .set_echo_guard_ms(ms.saturating_add(u64::from(tail_ms)));
    }
}
```

**唯一调用点**（`handle_server_event` 的 `AudioDelta` 分支，`pipeline/mod.rs:1291-1298`）：

```rust
            ServerEvent::AudioDelta { pcm } => {
                let samples = pcm16_to_float(&pcm);
                if let Some(sink) = self.sink.as_mut() {
                    sink.push(&samples);
                }
                if let Some(sink) = self.monitor_sink.as_mut() {
                    sink.push(&samples);
                }
                // ← 新增这一行（紧跟两次 push 之后）。它就是 S4-B §2.7 给 AEC
                //   预留的参考信号口的同一个位置，将来 AEC 落在这两行之间。
                self.mark_playing();
                /* 下面 turn_probe / playback_probe 两段一行不动 */
            }
```

**`Chain` 侧两个新方法**（`chain.rs`，与 `set_gate_active` 同一形状，`pipeline/mod.rs` 侧不新增任何门相关的口）：

```rust
impl Chain {
    /// 链里装了半双工门吗？`None` = 没装（**含**"链里压根没有门"）。
    /// `Worker::mark_playing` 用它在一句 `if` 里短路掉整段计算。
    pub(crate) fn echo_guard_tail_ms(&self) -> Option<u32> {
        self.stages.iter().find_map(|stage| match stage {
            ChainStage::Gate { gate, .. } => gate.config().half_duplex.map(|hd| hd.tail_ms),
            _ => None,
        })
    }

    /// 装满压制窗口。链里没有门 / 门没装半双工 → **静默忽略**
    /// （与 `set_gate_active` 同一个口径，`chain.rs:438`）。
    pub(crate) fn set_echo_guard_ms(&mut self, ms: u64) {
        for stage in &mut self.stages {
            if let ChainStage::Gate { gate, .. } = stage {
                gate.set_mute_for_ms(ms);
            }
        }
    }
}
```

**残差风险（如实记）**：`stats().queued_samples` 是**推完之后那一刻**的快照。若渲染线程此后卡住，
译音会**比预计更晚**放出来，而压制窗口按采集样本倒数，会**提前**结束 ⇒ 上行早开了一点。
反向（队列溢出丢样本）只会让窗口偏长，**保守安全**。缓解：`mark_playing` 在**每一次** `AudioDelta`
都重新装满，所以只要还有译音在推进，窗口就被续上；真正只靠倒数的只有"最后一次 push 之后"那一段。
`[未核实]` 真机上渲染卡顿多久算一次事件——有板子后用自计时（`OpTiming`）看。

### 2.7 热路径零新增分配（`RULES.md` #6）逐条记账

| 位置 | 新增分配 | 说明 |
| --- | --- | --- |
| `Worker::mark_playing` | **0** | 只读 `stats()`（返回 `Copy` 的 `PlaybackStats`）、几次 `u64` 比较；两个 `Option::as_ref` 都不克隆 |
| `Chain::set_echo_guard_ms` | **0** | 一个 `for` + 一次 `if let`；`ActivationGate` 是 `ChainStage` 里的值，`&mut` 就够 |
| `ActivationGate::set_mute_for_ms` | **0** | 一个 `u64` 乘除 |
| `process` 的压制分支 | **0**（稳态） | 分支不进时是"一次 `usize` 比较"；进去时 `vec![0.0; n]` 与今天 `process_manual` 的收尾边**同一处同一种**（`gate.rs:179` 已有，不新增形态） |
| `prebuffer.clear()` | **0** | `VecDeque::clear` 只丢元素，已分配的 `Vec` 还在队列的槽里；下一个 `push_prebuffer` 重新 `to_vec()`，分配次数与今天一样 |

**净结论：每块音频新增分配 0。**

### 2.8 差分台

#### 2.8.1 15 份金样必须逐字不变 —— 机制逐条核

| 可能让它变的机制 | 为什么不会 |
| --- | --- |
| 清单多了一个键 | ① `skip_serializing_if` ⇒ 为假时**不写**这个键；② 15 份的 `meta` 段与 `event` 段都不含清单（§1.6） |
| 15 份金样的 Speak 会不会自动开半双工 | 它们的角色是 `virtual_mic`（`all_wired(Windows)` 有 `VirtualMic` 位，`output_device = CABLE Input`，`monitor_translation = false`）⇒ `audible == false`；**外加** `host == Windows` ⇒ 即使 `audible` 为真也不开。**两条独立的理由** |
| `GateState` 加了变体 | 老变体的 serde 值一个都没变（`rename_all = "snake_case"` 作用于**名字**，新增变体不改老的） |
| `Worker::mark_playing` 插进了 `AudioDelta` 分支 | 门没装半双工时第一句 `let Some(tail_ms) = … else { return }` 就返回 ⇒ `AudioDelta` 分支的可观察行为逐字不变（`play` 段的 push 计数与指纹不变） |
| `playback_queue_ms` 重构成 `sink_queue_ms` | 同一个式子、同一份数据 ⇒ `LatencyChanged` 的内容不变（而且它本来就**不进**金样，`golden.rs` 模块头） |
| `chain.rs` 加两个方法 | 只加不改；`ChainStage` 与 `process` 一个字不动 |
| 既有单测 | `gate.rs::tests` 的 5 条用的都是 `GateConfig::MANUAL` / `level(t)`（`half_duplex = None`）⇒ 走的老路径一字未改，**不需要动任何一条** |

**验收**：`cargo test -p vox-core pipeline::golden` 之后
`git status --porcelain crates/vox-core/src/pipeline/golden/` 必须是**空**
（`VOX_GOLDEN_REGEN` 不许出现在验收命令里，[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md) §6.1）。

#### 2.8.2 新增金样场景（**新增，不改旧的**）

| id | 脚本 | 钉的是什么 |
| --- | --- | --- |
| **`speak_48k_speaker_half_duplex`** | ① `Rig::with_facts(48_000, HostFacts::all_wired(HostKind::LinuxHeadless))`；② 设置：Speak / 开麦（门开）/ 降噪开 / 翻译开 / `voice = "Tina"` / `output_device = Some("hw:0,0")`（⇒ 角色退成 `Speaker`，因为 `LinuxHeadless` 的上限里没有 `VirtualMic`，`capability.rs:414-419`）；③ 灌 3 块 `loud`（应当照常上行，`wire` 段有 3 个 `audio_append`）；④ 推一条 960 个 i16 样本的 `AudioDelta`（**假播放汇把 `stats().sample_rate` 写死成 48 kHz**，`pipeline/mod.rs:1828-1839`，所以 960 个样本 = 20 ms 的队列）；⑤ 再灌 **20 块** `loud`（每块 1920 样本 = 40 ms，`golden.rs::BLOCK_48`）⇒ 压制窗口 = 20 ms 队列 + 300 ms 尾巴 = 320 ms = 15 360 个采集样本 = **正好 8 块** ⇒ 前 8 块一块都不许上行，第 9 块起恢复）；⑥ 收尾 | ① `meta` 段里 `gate=manual+half_duplex(300)`；② 第 ④ 步之后 `event` 段出现 `gate_status` 且 `state` 是 `playback_mute`；③ `wire` 段**逐字**钉住"哪一块之后上传断了、断了几块、第几块恢复"；④ 第 ④ 步那一拍发出**一段 150 ms 的零样本**并把重采样缓冲冲掉（`MANUAL.tail_ms = 150`）⇒ `wire` 段最后多一个 `audio_append`；⑤ `play` 段不受影响（播放照旧推） |

**为什么这个场景是确定性的（不靠墙钟）**：压制窗口按**采集样本**倒数（§2.2.2），
假播放汇的 `queued_samples` = `played.len()`（`pipeline/mod.rs:1828-1839`）在第 ④ 步之后是一个**常数**
（没有渲染线程去取它），所以窗口 = 一个可手算的常数。场景脚本里**不许 `clock.advance`**
（`TestClock` 只用来驱动 200 ms 的门状态节流，`golden.rs` 模块头那条规矩不变）。

**夹具要加一个构造子**（`pipeline/mod.rs::tests`）：

```rust
    /// 按给定的事实建夹具。`build` = `with_facts(rate, HostFacts::all_wired(Windows))`，
    /// **既有 `build` 的函数体一行不改**（只是把 `set_host_facts` 的参数换成入参）。
    pub(crate) fn with_facts(rate: u32, facts: HostFacts) -> Self { /* build 的现有函数体 */ }
```

（不改 `build` 现有行为的理由同 S4-B §6.2：那份夹具是 41 条既有测试的依靠。）

#### 2.8.3 这份新金样的证据强度（**比旧的弱，如实说**）

15 份旧金样是**重构前录的**，所以"逐字不变"= 差分证据。
新场景是**实现之后录的**，它是**回归钉子**不是差分证据。真正的逻辑证明在那 6 条 `gate.rs` 单测里
（§4.3），它们是对着**规格**写的、不依赖金样。两层加起来才够，**别只拿金样当证明**。

---

## 3. 廉价重采样档

### 3.1 实测（本轮**真跑过**，命令与原始数字见下）

#### 表一：`tools/bench-dsp`（仓库内基准工程）

命令与结果（2026-09-30，`cd tools/bench-dsp && cargo run --release`；工具链在 PATH；
`README.md` 建议的 `taskset -c 2` **本轮没用**——沙箱不放行 `taskset`）：

| 算子 | ms / 1s 音频 | % 单核 | 极值 |
| --- | --- | --- | --- |
| RNNoise 降噪（`vox-dsp::Denoiser`, 48k） | **2.724** | 0.272% | 2.690–2.765 |
| rubato sinc 48k→16k（上行） | 0.563 | 0.056% | 0.552–0.582 |
| rubato sinc 24k→48k（播放侧真路径） | **1.624** | 0.162% | 1.613–1.639 |
| rubato sinc 16k→48k | 1.775 | 0.178% | 1.734–1.834 |
| 线性插值 48k→16k（对照） | **0.045** | 0.004% | 0.044–0.049 |
| 线性插值 16k→48k（对照） | 0.120 | 0.012% | 0.120–0.122 |
| 下混 2ch→1ch | 0.037 | 0.004% | — |
| `Blocker` 切块 | 0.008 | 0.001% | — |
| `DropRing` 写+读 | 0.219 | 0.022% | — |
| `duplicate_mono` 铺声道 | 0.033 | 0.003% | — |
| **采集路径合计**（下混+降噪+48k→16k+切块） | **3.332** | **0.333%** | — |
| **播放路径合计**（24k→48k+铺声道+环缓冲） | **1.876** | **0.188%** | — |

这张表与 `AGENTS.md` 里记的"采集链 3.03 ms/s、播放链 1.73 ms/s、RNNoise 2.5 ms/s"同量级（本机略快，
版本/微架构差异），**可互相印证**。

#### 表二：候选算法对照（**本轮临时基准，不在仓库里**）

- 工程：`/tmp/resample-probe`（独立 crate，path 依赖本 worktree 的 `vox-dsp`，**不写仓库任何文件**），
  命令 `cargo run --release --manifest-path /tmp/resample-probe/Cargo.toml`。
- 口径：30 s 音频、20 ms 块（960 样本，三条路径**共用同一块长**）、5 次中位、x86。
  **与表一不是同一套协议**（表一按各自真实块长喂），所以**两张表的绝对值不可互相比较，只有各自表内的比值有意义**。
- `Linear` 与 `Poly` 是本轮写的**参考实现**（未调优），`Poly` 的相位量化到 32 相、窗用 Kaiser、
  截止取输出奈奎斯特 × 0.70、目标阻带 70 dB。

| 实现 | 48k→16k | 44.1k→16k | 24k→48k | （ms / 1s 音频） |
| --- | --- | --- | --- | --- |
| rubato sinc（现网） | 0.496 | 0.608 | 2.971 | |
| 线性插值（无滤波） | **0.040** | 0.044 | **0.234** | |
| 多相 FIR P=32×T=24 | 0.400 | 0.436 | 2.398 | |
| 多相 FIR P=32×T=32 | 0.630 | 0.685 | 3.778 | |
| 多相 FIR P=32×T=48 | 0.998 | 1.086 | 5.997 | |

**音质**（48k→16k 降采样，1 s 正弦，取后半段稳态，单位 dBFS）：

| 实现 | 带内 1 kHz | 混叠：10 kHz → 6 kHz | 混叠：14 kHz → 2 kHz |
| --- | --- | --- | --- |
| rubato sinc（现网） | −0.00 | **−120.0** | **−160.3** |
| 线性插值（无滤波） | −0.00 | **−0.0** | **−0.0** |
| 多相 FIR P=32×T=24 | −0.71 | −22.6 | −53.8 |
| 多相 FIR P=32×T=32 | −0.18 | −32.3 | −94.2 |
| 多相 FIR P=32×T=48 | −0.01 | −60.4 | −165.4 |

#### 3.1.1 三条能直接下结论的读法

1. **降采样方向上，线性插值不可用**：10 kHz 的输入被原样折到 6 kHz，**−0.0 dBFS**（= 全幅），
   而 6 kHz 正落在语音带里。麦克风的高频（齿音、风噪）折进来会直接干扰识别。
   **多相 FIR 也不划算**：要做到 −60 dB 得 48 抽头，那是 rubato 的**两倍**开销（0.998 vs 0.496）。
2. **升采样方向上，线性插值是 12.7 倍的便宜**（0.234 vs 2.971 ms/s），而它的问题只是**镜像**
   （24 kHz 带限的信号被插值到 48 kHz 后在 24–48 kHz 出现镜像），落在一只小喇叭基本发不出来的频段，
   且译音本来已经过了一趟云端编解码往返。**省下的 2.7 ms/s 是播放链总成本的 87%**
   （表一：播放链 1.876 里 1.624 是这一档）——**这是 R 这张工单唯一的、也是全部的收益来源**。
3. **真正的大头是降噪，不是重采样**（表一：2.724 / 3.332 = 82%）。
   要在 A53 上省 CPU，**第一刀应该砍降噪，第二刀是"根本不重采样"**（`S4-C-ALSA.md` §5.2 的
   `rate_ladder` 第 1/2 档：设备直接给 16 kHz 采集 / 24 kHz 播放时，`Resampler` 走
   `ResamplerInner::Passthrough`，**零开销**——`resample.rs:52` 已经是这个行为了）。
   ⇒ 本稿在 §3.3 里把这条写成"先做这个，再做 R"。

`[未核实]`：表二的 `Poly` 是未调优的参考实现（每输出样本一次 `Vec::push` 改成预留容量之后仍如此、
`pos` 用 `f64`、历史缓冲用 `copy_within`）。一个定长相位累加器 + `process_into` 的版本
**有可能**在 T=32 档追平甚至超过 rubato。本稿**不**据此改结论，因为：即使追平，
它也只值 0.5 ms/s（A53 口径约 2 ms/s），而 R 的收益全在升采样那一侧（2.7 ms/s / A53 约 11 ms/s）。

**顺带一条实测发现**：`tools/bench-dsp/src/main.rs` 里的对照用 `Linear`（`:104-140`）在**步长非整数**时
会越界 panic（`pos` 自增后可能正好等于 `buf.len()`，`copy_within(done..)` 的 `done > len`）。
表一的 `48k→16k`（步长 3.0 整除）与 `16k→48k` 侥幸躲过了它，`44.1k→16k` 会炸。
本轮**没有改它**（本稿只写这一个文档），记在这里供 `bench-dsp` 的 owner 处理（§5.4 的 R2 里顺带修一行）。

### 3.2 算法选择

| 档 | 算法 | 用在哪 | 理由 |
| --- | --- | --- | --- |
| `Sinc`（**缺省，今天的行为**） | `rubato::SincFixedIn`，128 点 sinc + cubic 插值 | **降采样（`out < in`）、同率、桌面全部** | 抗混叠是硬的：混叠不可逆，且直接折进语音带（表二 −120 dB vs 线性 −0.0 dB） |
| `Linear`（**便宜档**） | 线性插值，无滤波 | **仅升采样（`out > in`）** | 便宜 12.7 倍（表二），代价是镜像而非混叠；镜像落在输出带外，喇叭上听不见 |
| 多相 FIR | —— | **不做** | 降采样方向上不比 rubato 便宜（§3.1.1-1）；升采样方向上便宜的那部分线性已经拿到了，留着只会多一套要维护的系数表与一份"这个阻带够不够"的争论 |

**用"能不能构造出来"表达这条规则，而不是运行时 `if`**（`Option` 让不成立的那一半在类型上就不存在）：

```rust
// crates/vox-dsp/src/resample.rs

/// 构造一档**便宜**的升采样器。
///
/// 返回 `None` 的两种情形，都是"这一档在这条路上不成立"，**不是失败**：
///   * `output_rate <= input_rate` —— 降采样没有抗混叠，线性插值会把高频原样折进
///     语音带（实测 10 kHz → 6 kHz 残留 −0.0 dBFS，见 S4-C 稿 §3.1 表二）。
///     降采样只有 sinc 一档。
///   * `input_rate == output_rate` —— 同率是透传，根本没有插值这回事。
///
/// 想要一个**降采样**的便宜实现，请把这一行改成 `Option<Box<dyn Resample>>` 之外的东西；
/// 本模块刻意不提供静默降级（`RULES.md` #4：不许留假实现），由调用方决定要不要退回 sinc。
pub fn cheap_upsampler(input_rate: u32, output_rate: u32) -> Option<Self> {
    if output_rate <= input_rate {
        return None;
    }
    Some(Self { inner: ResamplerInner::Linear { .. } })
}
```

### 3.3 选档规则

**本稿采纳：装配期由入口外壳选，不新增能力位 / `HostFacts` 字段 / `Settings` 字段 / 运行期切换。**

| 备选 | 结论与理由 |
| --- | --- |
| **入口外壳选**（**采纳**） | 桌面 `app/src-tauri` 装 `resample_factory()`（全 sinc，**一个字节都不变**）；无屏 `vox-headless` 装 `cheap_resample_factory()`。理由：① "哪一档构建"已经是 `HostKind` 这条硬事实，换档就是换入口里那一个闭包；② `vox-audio-alsa::AlsaPlayback` 的率转换是**注入进来的工厂**（`playback.rs:77`/`:140`），所以播放侧**一行都不用改**；③ 不引入任何新的跨 crate 接口，§1.5 那张调用方表全部不动 |
| 能力位（`Capability::CheapDsp` + `host_ceiling(LinuxHeadless)`） | **不做**：与 `HostKind` 重复表达同一件事，而代价是四张上限表 + `ALL`/`HOST`/`id()` + JSON schema 钉子用例 + 界面 i18n 全动（`capability.rs:82`/`:105`/`:148`/`:388-424`/`:828`）。等真有"高档位无屏机器"时再补 |
| `Settings` 字段（用户可强制） | **不做**：运行期换档要么被 INV-1 挡住（不许塞进 `Note`），要么得 `Stop`+`Start` 整轮（用户可感知的重连延迟）。**这正是 `S4-B-OP-CHAIN.md` §9.3 的 O-5，本稿的答复是"不做运行期切换"** |
| 环境变量（无屏入口读） | **采纳（作为开发/现场 A/B 的口子）**：`VOX_RESAMPLE_QUALITY=sinc|cheap`，缺省 `cheap`（无屏档）。非法值 → 启动时报错退出，**不许静默回落**（`RULES.md` #4）。这不是占位钩子：它有一个真实的用户（拿着盒子的开发本人），且缺省路径不依赖它 |

**先做什么的次序（§3.1.1-3 的直接结论）**：

```
① 让设备直接给协议要的率（ALSA rate_ladder 第 1/2 档 → Resampler 走 Passthrough）  ← 省 0.5 ms/s 且音质最好
② 播放侧的升采样换便宜档（本稿 R）                                              ← 省 2.7 ms/s
③ 降噪分档（大纲 §2.3「降噪可关」，已经是清单里的 Op::Denoise）                    ← 省 2.7 ms/s
④ 上行降采样换便宜档                                                            ← **不做**，收益不抵风险
```

### 3.4 `ResampleFactory` 怎么分派（**不改 `ports.rs` 一行**）

```rust
// crates/vox-dsp/src/resample.rs

/// 重采样档。**缺省 [`Self::Sinc`]**，与本模块引入前一模一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResampleQuality {
    /// 128 点 sinc + 抗混叠（`rubato::SincFixedIn`）。**降采样唯一的合格档。**
    #[default]
    Sinc,
    /// 线性插值。**只对升采样方向成立**（见 [`Self::cheap_upsampler`] 的文档）。
    Linear,
}

impl Resampler {
    /// **缺省 sinc**。既有调用方一个都不改（`RULES.md` #5：不留 shim）。
    pub fn new(input_rate: u32, output_rate: u32) -> Self { /* 与今天逐字相同 */ }

    /// 便宜档。降采样 / 同率 → `None`（见 [`Self::cheap_upsampler`]）。
    pub fn cheap_upsampler(input_rate: u32, output_rate: u32) -> Option<Self> { /* §3.2 */ }
}
```

```rust
// crates/vox-dsp/src/ports.rs

/// 重采样工厂。**同率时 `Resampler` 内部零开销透传**。逐字照抄今天（`:45-47`）。
pub fn resample_factory() -> ResampleFactory {
    Box::new(|from, to| Box::new(Resampler::new(from, to)))
}

/// 便宜档工厂：**只在升采样方向**给线性插值，其余情况给 sinc。
///
/// 落回 sinc **不是降级**，是"这一档在这条路上不存在"（降采样需要抗混叠，见 S4-C 稿 §3.2）。
/// 到底是哪一档，由装这个工厂的入口在启动时记一行日志说清楚（vox-dsp 不依赖 tracing，
/// 所以日志在 `vox-headless` 那一头打）。
pub fn cheap_resample_factory() -> ResampleFactory {
    Box::new(|from, to| {
        match Resampler::cheap_upsampler(from, to) {
            Some(resampler) => Box::new(resampler) as Box<dyn Resample>,
            None => Box::new(Resampler::new(from, to)) as Box<dyn Resample>,
        }
    })
}
```

**`ports::Resample` trait（`crates/vox-core/src/ports.rs:143-150`）一个方法都不加。**
理由：加一个 `fn quality(&self)` 的默认方法确实能拿到"这条链用的是哪一档"，
但**这一格在 R1 里没有消费者**：真要说清楚，只有两个地方能说——装工厂的入口（它自己知道）与日志。
为一个"以后可能要用"的读取口改 `pub` trait，按 `RULES.md` #4 属于半成品。
> `[未决]` §6.3 Q4：等自计时外露（`S4-B` 的 X1 工单）落地后，如果要给 `Snapshot` 加"本条链的重采样档"，
> 那时才加 `ports::Resample::quality()` 的默认方法，并同步 `vox-dsp` 与各测试替身。

### 3.5 差分台

- **15 份金样一个字都不受影响**：R 的改动**一个文件都不在 `vox-core` 里**（§5.2），
  金样用的是 `pipeline/mod.rs::tests::Rig` 里的假 `Decimate`（`pipeline/mod.rs:1870-1884`），
  它按整数比抽点、跟 `vox-dsp` 一点关系都没有。
- `vox-dsp` 的 5 条既有单测（`resample.rs:159-236`）测的是 `Resampler::new` = sinc 路径，
  **一条都不用改**（`RULES.md` #5：新 API 是加法）。
- **新增 5 条** `vox-dsp/src/resample.rs::tests`（§4.3）。
- **新增 2 行** `tools/bench-dsp/src/main.rs`（把便宜档加进基准表，让"这个数是怎么来的"留在仓库里而不是留在 `/tmp`）。

---

## 4. 改动清单与工单

### 4.1 半双工门（G）

| 动作 | 文件 | 改什么 | owner |
| --- | --- | --- | --- |
| 修改 | `crates/vox-core/src/gate.rs` | ① 新增 `HalfDuplex`（§2.2.1）；② `GateConfig` 加 `half_duplex`（含两个 serde 属性）+ 两个预设各补一行；③ `GateState` 加 `PlaybackMute`；④ `ActivationGate` 加 `mute_samples` 字段 + `set_mute_for_ms`；⑤ `process` 加压制分支（§2.2.3）；⑥ `reset` 清它、`set_config` 装回它（§2.2.4）；⑦ 模块头补一段"第三种门不是 kind，是正交的一格"；⑧ **新增 6 条单测，既有 5 条一字不动** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/chain.rs` | ① `Chain::echo_guard_tail_ms` + `Chain::set_echo_guard_ms`（§2.6）；② `ChainStage` / `ChainStage::process` / `on_disconnect` **一个字不动**；③ **新增 2 条单测** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/mod.rs` | ① 拆出自由函数 `sink_queue_ms`（`playback_queue_ms` 改为逐字转调）；② `Worker::mark_playing`；③ `handle_server_event` 的 `AudioDelta` 分支插一行；④ `tests::Rig` 加 `with_facts`（`build` 函数体行为不变）；⑤ **既有 41 条测试的断言一字不动** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/speak.rs` | ① `composition` 里 `ops` 段**纯位移**到 `out` 段之后；② 算 `audible` + 填 `half_duplex`；③ `use` 补 `HostKind`；④ **新增 2 条单测** | core-dev |
| 修改 | `crates/vox-core/src/pipeline/golden.rs` + 新增 `golden/speak_48k_speaker_half_duplex.txt` | 场景表加一条 + 一个 `#[test]` + 一个 `verify!` 调用 | core-dev |
| 修改 | `crates/vox-core/src/lib.rs` | `pub use gate::{…}` 补 `HalfDuplex`（`:36`） | core-dev |
| 修改 | `app/ui/src/types.ts` | `GateState` 联合类型加一行 `"playback_mute"`（**一行**；§1.4 已核：没有任何组件读 `gate.state`，不用改 i18n 与组件） | shell-dev |
| **不动** | `composition.rs` / `capability.rs` / `ports.rs` / `runtime.rs` / `vox-dsp/**` / `app/src-tauri/**` / `vox-host/**` | 见 §4.4 的自证表 | — |

### 4.2 廉价重采样档（R）

| 动作 | 文件 | 改什么 | owner |
| --- | --- | --- | --- |
| 修改 | `crates/vox-dsp/src/resample.rs` | ① `ResampleQuality`；② `ResamplerInner` 加 `Linear` 变体（`step/pos/buffer` + 复用 `chunk`）；③ `Resampler::cheap_upsampler`；④ `process` / `flush` / `reset` 各加一个 `Linear` 臂（**`Passthrough` 与 `Sinc` 两臂的函数体一字不动**）；⑤ 模块头改写"选 SincFixedIn 而非 FastFixedIn"那段，补"为什么便宜档只对升采样"；⑥ **新增 5 条单测，既有 5 条一字不动** | core-dev（vox-dsp 归芯侧） |
| 修改 | `crates/vox-dsp/src/ports.rs` | 加 `cheap_resample_factory()`；`resample_factory()` / `denoise_factory()` / 两个 `impl` **一字不动** | core-dev |
| 修改 | `crates/vox-headless/src/headless.rs` | `Deps.resample`（`:248`）改用新工厂 + 读 `VOX_RESAMPLE_QUALITY` + 启动时 `tracing::info!` 记实际档 | shell-dev |
| 修改 | `crates/vox-headless/src/platform/linux.rs` | `LinuxPlayback` 内部那个工厂（`:34`）同上（与上一条**同一个 owner，同一张工单**） | shell-dev |
| 修改 | `tools/bench-dsp/src/main.rs` | 加便宜档两行测量 + 修 `Linear` 在非整数步长下的越界（§3.1 末尾） | core-dev |
| **不动** | `crates/vox-core/src/ports.rs` / `**/pipeline/**` / `composition.rs` / `capability.rs` / `crates/vox-audio-alsa/**` / `app/src-tauri/**` / `vox-audio-{win,linux}/**` | 见 §4.4 | — |

### 4.3 测试清单（逐条可数）

| 工单 | 新增用例 | 净增 |
| --- | --- | --- |
| **G1** `gate.rs` | ① `half_duplex_silences_the_uplink_until_the_window_runs_out` ② `a_mute_arriving_mid_sentence_ends_it_with_a_zero_tail` ③ `the_prebuffer_is_cleared_when_the_mute_begins_and_not_refilled` ④ `the_window_ends_after_exactly_its_tail_in_capture_samples` ⑤ `a_gate_without_half_duplex_never_mutes` ⑥ `set_config_keeps_the_mute_window`（与 `set_config_keeps_external_active` 同形） | **+6** |
| **G2** `chain.rs` | ⑦ `a_chain_without_a_half_duplex_gate_ignores_the_echo_guard` ⑧ `the_echo_guard_reaches_the_gate_through_the_manifest` | **+2** |
| **G3** `speak.rs` | ⑨ `speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker`（Windows+virtual_mic → `None`；LinuxHeadless+speaker → `Some`；LinuxHeadless 无 `voice` → `None`；Windows+speaker → `None`）⑩ `the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset`（对着 `composition.rs:798` 那份期望值再比一次，钉 `skip_serializing_if`） | **+2** |
| **G4** `golden.rs` | ⑪ `speak_48k_speaker_half_duplex_matches_its_golden` + 1 份 `.txt` | **+1** |
| **G5** `app/ui` | 0（`npm run verify` 里 `tsc` 过即可） | 0 |
| **R1** `vox-dsp` | ⑫ `cheap_upsampler_exists_only_for_upsampling` ⑬ `a_cheap_upsampler_hits_the_target_rate_within_a_few_samples`（流式散碎块 vs 一次性的总量差 ≤ 5，与 `streaming_odd_chunks_matches_single_big_chunk` 同口径）⑭ `flush_drains_the_cheap_path` ⑮ `reset_clears_the_cheap_path` ⑯ `the_cheap_path_stays_linear_where_linear_is_better`（同一段 1 kHz，线性档与 sinc 档的带内电平差 ≤ 0.1 dB，**不给它带任何抗混叠的承诺**） | **+5** |
| | | **净 +16** |

**既有用例改动数：0。**（唯一有风险的是 `composition.rs::the_manifests_match_the_design_documents_instances`，
它**必须**在 G 之后仍然绿——那正是 `skip_serializing_if` + `host == LinuxHeadless` 两条机制的价值，
它就是"15 份金样不变"在清单侧的对偶证据。）

### 4.4 平行性自证（G 与 R、R 与 S4-B B3）

| 文件 | G 碰吗 | R 碰吗 |
| --- | --- | --- |
| `crates/vox-core/src/{gate,lib}.rs` | ✅ | ❌ |
| `crates/vox-core/src/pipeline/{mod,chain,speak,golden}.rs` + `golden/*.txt` | ✅ | ❌ |
| `app/ui/src/types.ts` | ✅ | ❌ |
| `crates/vox-dsp/src/{resample,ports}.rs` | ❌ | ✅ |
| `crates/vox-headless/src/{headless,platform/linux}.rs` | ❌ | ✅ |
| `tools/bench-dsp/src/main.rs` | ❌ | ✅ |
| `crates/vox-core/src/{composition,capability,ports,runtime}.rs` | ❌ | ❌ |
| `crates/vox-audio-alsa/**`、`app/src-tauri/**`、`crates/vox-host/**`、`vox-audio-{win,linux}/**` | ❌ | ❌ |

⇒ **G ∥ R 完全平行**（零交集）。
⇒ **G 与 S4-B B3 冲突**（B3 要改 `pipeline/chain.rs` 与 `pipeline/mod.rs` 加 `OpTiming` + `Runtime::op_timings`），
按 `RULES.md` #8 串行。**建议 G 排在 B3 之后**（B3 已经在跑，先落地省一次冲突）；
若 Main 要抢在 B3 前，G 只需要动 `chain.rs` 里 `impl Chain` 的**末尾**加两个方法、
不动 `ChainStage::process` 与 `Chain::run` 的循环体（自计时的循环体在 `Chain::run` 里），
冲突面可压到最小，但**仍需 Main 排期**。

### 4.5 工单

| # | 名字 | owner | 独占文件 | 前置 | 并行 |
| --- | --- | --- | --- | --- | --- |
| **G1** | `gate.rs`：半双工数据结构 + 状态机 + 6 条单测 | core-dev | `crates/vox-core/src/gate.rs` | 无 | 与 G2/G3/G4/R 全平行 |
| **G2** | `chain.rs`：两个门控口 + 2 条单测 | core-dev | `crates/vox-core/src/pipeline/chain.rs` | 无（**G1 的 `set_mute_for_ms` 要在**）⇒ 实取前置 G1 | — |
| **G3** | `Worker::mark_playing` + `Rig::with_facts` + `speak::composition` 的判据 | core-dev | `pipeline/mod.rs`、`pipeline/speak.rs`、`src/lib.rs`、`app/ui/src/types.ts` | G1（`HalfDuplex` 要在） | — |
| **G4** | 差分台：新场景 + 1 份金样 | core-dev | `pipeline/golden.rs`、`pipeline/golden/speak_48k_speaker_half_duplex.txt` | G3 | — |
| **R1** | `vox-dsp` 便宜升采样档 + 新工厂 + 5 条单测 | core-dev | `crates/vox-dsp/src/{resample,ports}.rs` | 无 | 与 G1–G4 全平行 |
| **R2** | 无屏入口装便宜档 + 环境变量 + 启动日志 + 基准补两行（含 `bench-dsp` 的越界修一行） | shell-dev（R2 前半）+ core-dev（`bench-dsp` 那半）⇒ **拆成 R2 / R3** | R2：`crates/vox-headless/src/{headless.rs,platform/linux.rs}`；R3：`tools/bench-dsp/src/main.rs` | R1 | — |
| **G5 / R4** | 文档回填（`S4-EMBEDDED-REFACTOR.md` §3 表这两行标"已落地"、`DIRECTIONS.md` §8 第 16/17 行旁注） | docs-scribe | `docs/plans/S4-EMBEDDED-REFACTOR.md`、`docs/architecture/DIRECTIONS.md` | G4 且 R3 | — |

依赖图：

```
G1 ──▶ G2 ──┐
 │          ├──▶ G4 ──▶ G5
 └──▶ G3 ───┘
 R1 ──▶ R2 ──▶ R3 ──▶ R4
```

**工单间文件不重叠**（G1/G2/G3/G4/R1/R2/R3 逐对零交集），符合 `RULES.md` #8。

---

## 5. 验收标准

### 5.1 每张工单收口必跑

```bash
cargo fmt --all --check                                            # 期望：静默
cargo test -p vox-core                                             # 期望：全部 ok，0 failed
cargo test -p vox-dsp                                              # 期望：全部 ok，0 failed
cargo test --workspace                                             # 期望：全部 ok，0 failed
cargo clippy --workspace --all-targets -- -D warnings              # 期望：静默
cd app/ui && npm run verify                                        # G3 之后：tsc 过
```

### 5.2 G 专属（差分台）

```bash
# ① 15 份旧金样逐字不变（验收命令里**绝不含** VOX_GOLDEN_REGEN）
cargo test -p vox-core pipeline::golden                            # 期望：16 passed（15 旧 + 1 新）
git status --porcelain crates/vox-core/src/pipeline/golden         # 期望：**空**（新 .txt 已在上一张工单提交）

# ② 新金样自己是确定的
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
git status --porcelain crates/vox-core/src/pipeline/golden         # 期望：空

# ③ 跨调度稳定
RUST_TEST_THREADS=1 cargo test -p vox-core pipeline::golden        # 期望：16 passed
RUST_TEST_THREADS=8 cargo test -p vox-core pipeline::golden        # 期望：16 passed
```

**可观察行为**（概要稿 §4 的原文「播放中上行为静音、播放结束 + 尾巴后恢复；去掉该逻辑即红」）怎么测：

| 断言 | 在哪 |
| --- | --- |
| 播放中上行**不出声**（`wire` 段没有新的 `audio_append`） | 新金样的 `wire` 段 |
| 播放结束 + 尾巴之后恢复 | 同上（第 9 块起重新有帧，**块号逐字钉住**） |
| 去掉该逻辑即红 | ① 删 `process` 里的压制分支 ⇒ 用例 ①②③④ + 新金样全红；② 把 `speak::composition` 里 `facts.host == LinuxHeadless` 去掉 ⇒ 用例 ⑨ 的第三条红；③ 把 `skip_serializing_if` 去掉 ⇒ `composition.rs` 的两条既有 JSON 测试红 |
| 热路径零新增分配 | 代码论证（§2.7）+ `gate.rs` 用例 ④ 的块数逐块可数；`OpTiming` 那条全局分配计数用例（S4-B §8.3 第 2 条）合流后一并验 |

### 5.3 R 专属

```bash
# ① 便宜档只对升采样存在
cargo test -p vox-dsp cheap_upsampler_exists_only_for_upsampling   # 期望：1 passed

# ② 基准工程里能看见两档
cd tools/bench-dsp && cargo run --release                          # 期望：表格里出现"线性插值 24k→48k（便宜档）"一行
```

**可观察行为**：在无屏盒子上设 `VOX_RESAMPLE_QUALITY=cheap` 启动，
`journal`/日志里出现一行说清实际档；`=sinc` 启动则出现 sinc 那行；
`=foo` **启动失败并说清合法取值**（不许静默回落）。**本轮没有板子，盒子上那两条未跑。**

### 5.4 明确不做的事（写下是为了不被"顺手做了"）

- **不**给 `ports::Resample` 加 `quality()`（§3.4）。
- **不**给 `GateStatus` 加字段（§2.2.1）。
- **不**新增 `Capability` 位（§2.5.4）、**不**加 `HostFacts` 字段、**不**加 `Settings` 字段（§2.5.3）。
- **不**做多相 FIR 那一档（§3.1.1-1）。
- **不**做半双工的用户开关（§2.5.3）。
- **不**实现 AEC（`S4-B` §2.7 的参考信号口留给它那张工单；本稿只在 `AudioDelta` 分支留了位置，
  `Worker::mark_playing` 插在两次 `push` 之后，将来 AEC 的 `push_playback_reference` 插在 `push` 之前，
  **两者是相邻两行，串行派**）。

---

## 6. 风险与未决

> `[未核实]` = 本轮没核到 / 核不到 / 核不了。**不许在这些地方编。**

### 6.1 风险

| # | 风险 | 后果 | 处置 |
| --- | --- | --- | --- |
| **GR-1** | **`skip_serializing_if` 漏了** | `composition.rs:783` 那两条逐字比 JSON 的既有测试当场红；违反"既有测试逐字未改" | §2.2.1 把两个 serde 属性写成必带；用例 ⑩ 就是专门钉它的 |
| **GR-2** | **`facts.host == LinuxHeadless` 这一条被评审当成"随便加的"删掉** | 桌面 Linux 无虚拟麦的用户**在没要求的情况下被切断说话** | §2.5.2 把三条判据的理由逐条写开；用例 ⑨ 的第四条（Windows + Speaker → `None`）钉住它；`DIRECTIONS.md` §8 记一条"半双工只在无屏档自动开" |
| **GR-3** | **压制期间清空 preroll 的那条决策被改成"照常填"** | 重新开门第一件事就是把 100–200 ms 的**译音回声**上传，正是这一档要防的事 | §2.4.3 三个选项都列了；用例 ③ 钉住"压制开始的这一拍 preroll 已被倒掉" |
| **GR-4** | **尾巴 300 ms 是估算** | 太短 → 回声尾巴漏进上行；太长 → 面对面轮流说接不上话 | §2.3 写清来由与三条依据；新金样把"哪个块恢复"逐字钉住 ⇒ **改这个值必然要重录金样**，这就是它的护栏。等有板子时按 §6.3 Q1 实测 |
| **GR-5** | **`stats().queued_samples` 是"推完之后那一刻"的快照** | 渲染线程随后卡住 ⇒ 压制提前结束，上行早开一点（反向：队列溢出只会让窗口偏长，保守安全） | §2.6 如实记；`mark_playing` 每一次 `AudioDelta` 都重新装满，只靠倒数的只有"最后一次 push 之后"那一段；`[未核实]` 见 Q5 |
| **GR-6** | **G 与 S4-B B3 抢 `chain.rs` / `pipeline/mod.rs`** | 违反 `RULES.md` #8，两边互相踩 | §4.4 建议 G 排在 B3 之后；若抢排，G2 只在 `impl Chain` 末尾加方法、不碰 `Chain::run` 的循环体，冲突面最小，但**由 Main 排期** |
| **GR-7** | **半双工在"译音不进扬声器"的机器上被打开**（例如用户把 `output_device` 选成蓝牙耳机，但耳机没戴） | 白切话 | 芯**无从知道**耳机戴没戴（§6.3 Q6）。本轮靠 §2.5.2 的 `audible` 判据 + 无屏档限定兜住；用户开关列为后续 |
| **RR-1** | **线性升采样的镜像在小喇叭上其实听得见**（廉价喇叭的 12–20 kHz 响应并不为零） | 译音发闷/发毛 | `[未核实]`（Q7）：真机上听 A/B。退路是把便宜档的适用范围收窄到"设备率 ≥ 2× 源率"（本轮不收窄，因为镜像的听感靠耳朵判断，靠数字判断不了） |
| **RR-2** | **表二的 `Poly` 参考实现未调优** | 若有人据此说"多相 FIR 其实更便宜"，那是拿未调优的实现比的 | §3.1 末尾标了 `[未核实]`；§3.2 明确"不做"，理由不是"调不动"，而是"即使调得动也只值 0.5 ms/s，收益全在升采样那一侧" |
| **RR-3** | **`VOX_RESAMPLE_QUALITY` 变成事实上的长期配置** | 无屏档多一条要长期维护的旋钮 | 它只在无屏入口读，缺省 `cheap`，非法值启动失败。要收进 `Settings` 时**删掉这个环境变量**，不许两条真源并存（`RULES.md` #3） |

### 6.2 有意接受的行为差异

1. **无屏档 + 译音走扬声器时，播译音期间上行是关的，用户会被切断。** 这是"保底档"的实价，
   概要稿 §2.2 已经写明。桌面档与直通模式**零差异**。
2. **无屏档的播放侧率转换（升采样）从 sinc 换成线性**：译音里会出现 24 kHz 以上的镜像。
   已在 §5.3 给出实机验收方法。
3. **清单的线上 JSON 在半双工为真时多一个 `half_duplex` 键**（为假时**没有**这个键）。

### 6.3 未决

| # | 事项 | 状态 |
| --- | --- | --- |
| **Q1** | **`HalfDuplex::DEFAULT_TAIL_MS = 300` 的实机值** | `[未核实]` 估算值，依据见 §2.3。有板子后：录一段"译音 → 立刻接话"的音频，测译音结束后多久麦克风上的自回声低于 −40 dB，取那个值再加 50 ms 余量 |
| **Q2** | **AEC 落地时，半双工的判据与 AEC 的参考信号口怎么合** | `[未决]` 本稿给的形状是：AEC 的 `Chain::push_playback_reference` 插在 `mark_playing` 的**前一行**（都在 `AudioDelta` 分支），三档互斥的选档入口落在 `Composition::of` 的一处 `match` 上，并把 §2.5.2 的 `audible && host == LinuxHeadless` **整条替换**成位驱动。**G4 之后动 `pipeline/mod.rs` 的 AEC 工单必须与本稿串行** |
| **Q3** | **要不要给用户一个半双工开关** | `[未决]` §2.5.3 写了技术成本为零（`set_config` 已经能带），不做是**取舍**不是**做不到**。要做的完整清单：`Settings` 加一格 + `Runtime::gate_for` 一支 + 界面开关 + 三份 i18n + 一条新金样 |
| **Q4** | **`ports::Resample::quality()` 要不要加** | `[未决]` §3.4 给了"没有消费者就不加"的判据。触发条件：自计时外露（`S4-B` 的 X1）落地后，`Snapshot` 要显示"本条链的重采样档" |
| **Q5** | **渲染卡顿多久算一次事件** | `[未核实]` 影响 `GR-5` 的实际暴露面。有板子后用 `OpTiming` 的 `max_ns` 观察 |
| **Q6** | **能不能知道"用户戴没戴耳机"** | `[未核实]` 三个端口里都没有这个信息（`PlaybackStats` 只有队列/已渲染/丢弃/设备延迟）。要做就得给 `PlaybackSink` 加一个"设备是不是耳机"的事实 —— 那是 §3.4 那种"没有消费者就先不加"的反面：**有消费者（GR-7）时可以加**，但要单独立项 |
| **Q7** | **线性升采样的镜像在真机喇叭上听感如何** | `[未核实]` 见 `RR-1`。退路已写 |
| **Q8** | **`ActivationGate::process_into`（S4-B 的 X3）落地后，半双工分支的形状要不要跟着改** | `[未决]` 本稿的压制分支是"要么发一块尾巴、要么发空"，`process_into` 化之后 `Vec` 变借用，**收尾边那块的分配会消失**。G1 若赶在 X3 之前，就要接受 X3 之后改一次。**建议 X3 在 G 之后**（X3 会改 `gate.rs` 的 5 条既有单测，越晚越省事） |
| **Q9** | **A53 上的实际倍率** | `[未核实]` 本稿全部性能数字是 **x86** 口径。S4 的纪律是"没有板子之前不写任何'够用'的结论"，本稿照办：§3.1 的数字只用来**比较候选之间**，不用来推算 A53 的绝对性能。转换系数 `[未核实]` |

---

## 7. 参考与关联

- 上位稿：[`S4-EMBEDDED-REFACTOR.md`](S4-EMBEDDED-REFACTOR.md) §2.2（回声消除三档）、§2.3（轻量化 + A53 预算）、§3 S4-C 表两行、§4 验收两行
- 方向与裁决：[`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §8 第 15/16/17 行、§10.9 第 1/4/7 条
- 算子链（**已落地的代码，不是预想**）：[`S4-B-OP-CHAIN.md`](S4-B-OP-CHAIN.md) §2.2（契约 C1–C6）、§2.7（参考信号口）、§3.4（INV-1/INV-2）、§9.3 O-5（本稿给了答复）、§11 M1–M4
- ALSA 后端：[`S4-C-ALSA.md`](S4-C-ALSA.md) §5.2（"能不重采样就不重采样"、三档率阶梯）、§12 M1–M6
- 清单由来：[`S0-COMPOSITION-MANIFEST.md`](S0-COMPOSITION-MANIFEST.md) §2.1（`Composition` / `Op`）、§2.7（`Plan` 的边界）
- 性能事实：`tools/bench-dsp/`（**本轮实跑**，§3.1 表一）、`AGENTS.md`「性能事实」一节（采集链 3.03 / 播放链 1.73 ms/s，与本轮实测同量级）
- 临时基准：`/tmp/resample-probe`（**不在仓库里**，§3.1 表二；方法写在 §3.1 开头，要复现照着写一遍即可）
- 代码：`crates/vox-core/src/{gate,capability,composition,ports}.rs`、`crates/vox-core/src/pipeline/{mod,chain,speak,listen,golden}.rs`、`crates/vox-dsp/src/{resample,ports}.rs`、`crates/vox-audio-alsa/src/playback.rs`、`crates/vox-headless/src/{headless.rs,platform/linux.rs}`、`app/ui/src/types.ts`

---

## Main 拍板（2026-09-30，审稿后追加；与上文冲突以本节为准）

- **Q1**：`HalfDuplex::DEFAULT_TAIL_MS = 300` 先用，实机校准后再改（改时新增金样，不改旧的）。
- **Q2**：AEC（`docs/plans/S4-C-AEC.md`）与半双工 G 系列串行，G 先落。
- **Q3**：本轮**不**给用户半双工开关。
- **Q8**：`ActivationGate::process_into`（S4-B X3）排在 G 系列之后。
- **Q4/Q5/Q6/Q7/Q9**：记账，不在本轮。
- **排期**：R1 ∥ G1 立即开工；G2–G4 在 S4-B B3 合入后；R2 在 S4-A W5 合入后；R3 在 R1 后。
