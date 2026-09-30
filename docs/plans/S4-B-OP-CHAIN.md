# S4-B · 算子链执行器（行为不变）· 施工设计稿

> 上位稿：[`S4-EMBEDDED-REFACTOR.md`](S4-EMBEDDED-REFACTOR.md) §3「S4-B 算子链执行器（行为不变）」、§4 验收。
> 方向：[`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §8 第 16 行（**已拍板**：清单即作业单，`Worker` 按 `Composition.ops` 执行；第 5 行"拍板前不动手"作废）、§10.9 第 3 条。
> 清单的由来：[`S0-COMPOSITION-MANIFEST.md`](S0-COMPOSITION-MANIFEST.md) §2.1（`Composition` / `Op` 的定义）、§2.7（`Plan` 为什么"字段一个不加一个不减"）。
> 并行约束：[`S4-A-HOST-LAYER.md`](S4-A-HOST-LAYER.md) 正在施工，**本稿一个文件都不碰它的**（见 §7.2 的自证）。
>
> 口径：代码与本稿打架以代码为准。引用**符号名优先**（`Plan::from`、`Worker::gated_blocks`），行号是 2026-09-30 核的、写稿当时的值。
> 本轮环境有 Rust 工具链：`cargo test -p vox-core` 实跑过（**258 passed / 0 failed / 0 ignored**，2026-09-30）。
> `cargo test --workspace` 本轮**未跑**（沙箱未批准），§8.1 的 598/0/5 是用户提供的基线。

---

## 0. 一句话

**`Plan` 里的六个布尔压回四个，`Worker` 里写死的"单声道 → 降噪 → 阀门 → 重采样"换成一条按 `Composition.ops` 顺序走的有序节列表；装不装某一节由清单说了算，代码里不再有第二个答案。**
先把差分台和金样落进仓库（跑在**重构前**的代码上），再动 `Worker`。

### 0.1 本稿推翻的既有决策（`DIRECTIONS.md` §8「新者胜」）

| # | 被推翻的 | 出处 | 本稿的做法 | 为什么 |
| --- | --- | --- | --- | --- |
| N1 | 「`Plan` **保持原样**：它是 Worker 认的作业单，**字段一个不加、一个不减**」 | [`S0-COMPOSITION-MANIFEST.md`](S0-COMPOSITION-MANIFEST.md) §2.7（09-22 拍板） | **删 `denoise`、加 `chain_ops`**（§3.1） | S0 的目标是"清单能表达现状而**行为不变**"，手段是"清单 → `Plan` → **Worker 一行不动**"。S4-B 的目标变了：要的是"**清单直接就是作业单**"。`denoise: bool` 存在的唯一理由就是"Worker 一行不动"——`Op::Denoise` 在清单里已经是一个条目，再压成布尔就是**清单之外第二份"装不装"的答案**，正是 S4 要消灭的东西（概要稿 §1-第 2 条）。S0 比 S4-B 旧，按 §8 第 16 行以 09-26 的 S4 为准 |
| N2 | 「`Plan ──► Worker`（**一行不动**）」（同 §2.7 的图与文字） | 同上 | `Worker::feed` / `boot` / `reconnect` / `teardown` **都要动**；`mod tests` 里 5 处断言改写目标 | 同 N1。S0 明确把这当作"清单中转不改行为"的**硬证据**；S4-B 不再要求"Worker 不动"，改要求"**轨迹逐字相等**"（差分台）。证据从"diff 为空"换成"15 份金样一个字不用改"——**后者更强**（它覆盖音频样本与帧边界，`diff` 不覆盖） |
| N3 | 「`Composition::OP_ORDER` 的子序列校验保证顺序」被当成"清单已经驱动了执行" | `composition.rs:449-460` + §1.1 的 `speak_manifest_names_the_virtual_mic` | 承认**顺序**维度上清单已驱动（§1.3 结论），**不装**与**字段值**三个维度仍没驱动（§1.3 的 D1/D2/D3） | S0 的验收（`composition.rs::plan_is_derived_from_the_manifest`，`:860`）只断言 `plan.denoise == 清单里有没有 Denoise`——**这恰恰证明了布尔是死字段**。S4-B 要让 `Op` 本身被读，不再经过布尔 |

**N1/N2 只动 `vox-core` 内部的一个 `pub(crate)` 类型与一个私有方法，不影响任何外部调用方**（§7.3：`Plan` 是 `pub(crate)`，`app/**` 与 `vox-headless/**` 零命中）。S0 的**其余**决策（`Composition` 的形状、`Op` 的四种、能力位、开门/关门）**一条都不推翻**。

---

## 1. 现状证据

> 全部 2026-09-30 在 `crates/vox-core/src/pipeline/mod.rs`（**3055 行**）、`speak.rs`、`listen.rs`、`composition.rs`、`ports.rs`、`gate.rs`、`crates/vox-dsp/src/{denoise,resample}.rs` 上核的。

### 1.1 `Plan` 每个字段是什么、谁读它

`Plan` 定义在 `pipeline/mod.rs::Plan`（`:90`，`pub(crate)`）。

| 字段 | 类型 | 语义 | 读它的地方（符号） |
| --- | --- | --- | --- |
| `target` | `CaptureTarget` | 抓谁的声音 | `Worker::boot` → `capture.start(&self.plan.target, …)`（`:786`） |
| `denoise` | `bool` | 上传前要不要降噪 | **只** `Worker::boot`（`:797`）。`Plan::from` 写它（`:150`），除此之外全仓库无人读 |
| `passthrough` | `bool` | 直通（不起云端会话） | `Worker::boot`（`:752` / `:814` / `:840`）、`Worker::pump`（`:921`）、`Worker::set_monitor_translation`（`:1432`） |
| `playback_device` | `Option<Option<String>>` | 主播放出口；外层 `None` = 不出声 | `Worker::boot`（`:768`、`:815`）、`Worker::set_monitor_translation`（`:1442-1443`）、`Worker::set_translation_audio`（`:1489`、`:1497`、`:1499`） |
| `monitor_translation` | `bool` | 额外回听 | `Worker::boot`（`:773`）、`Worker::set_monitor_translation`（`:1436`） |
| `hot_update` | `bool` | 认不认 `HotUpdate` | `Worker::handle_note` 的 `Note::Hot`（`:1411`）、`Worker::send_hot_change` 之后写回 params（`:1532`）、**以及 `Worker::track_local_gate`（`:1148`）——后者把它当"这条腿是 Speak"的替身用** |
| `params` | `SessionParams` | 协议参数 | `Worker::handle_server_event` 的 `TurnDone` 记用量（`:1317`）；`Worker::new` 建 `Session`（`:673`）；`Worker::send_hot_change` 写回（`:1532`） |

**派生路径只有一条**：`Plan::build`（`:114`）= `Plan::from(&Composition::of(config, facts)?)`。`Plan::from`（`:119`）先查 `schema_version`、跑 `composition.validate()`，再从 `r#in[0]` 取 `target`、从 `ops` 取 `denoise`、从 `session` 取 `passthrough`/`hot_update`/`params`、从 `out` 取两个播放布尔。

**`Plan` 是 `pub(crate)`**（`grep` 实跑，2026-09-30）：`crates/vox-core/src/composition.rs` 的 `mod tests`（`:596`、`:625`、`:1133`、`:1224`、`:1385`）与 `pipeline/{mod,speak,listen}.rs` 内部引用。**`app/src-tauri` 与 `crates/vox-headless` 一个引用都没有。** 这一点是 §7.3 能说"零外部调用方"的全部依据。

### 1.2 两条流水线的执行顺序写死在哪

**唯一一份顺序写死在 `Worker::gated_blocks` + `Worker::feed` 这两个函数里**（`pipeline/mod.rs`）：

```
feed()                       :1083
 └─ gated_blocks()           :1119
     ├─ chunk.to_mono()      :1123     ← 第 1 节 Mono（无条件执行）
     ├─ self.denoiser.process:1125     ← 第 2 节 Denoise（denoiser 是 Option，没有就跳过）
     └─ self.gate.process()  :1133     ← 第 3 节 Gate
 └─ for block in &accepted {            :1087
      resampler.process(block)          :1089  ← 第 4 节 Resample
      self.upload(&resampled, …)        :1092
    }
 └─ if status.ended { resampler.flush() ; upload(tail) }   :1095-1101
```

`feed_passthrough`（`:1106`）是同一段的前半截：`gated_blocks` 之后直接 `sink.push(block)`，不重采样、不上传。

`Worker::boot`（`:745`）里四节的**装配**也是写死的：

| 节 | 建在哪 | 装配顺序上的关键事实 |
| --- | --- | --- |
| 门 | `:792` `ActivationGate::new(self.config.gate, format.sample_rate)` | 建在**采集率**上（不是会话率）；`set_external_active(self.config.gate_active)` 在会话线程里同步生效（`:794`） |
| 降噪 | `:797-812` | 三分支：`plan.denoise` 真 + `format.sample_rate == DENOISE_RATE`(48k) + 工厂成功 → 装；率对不上只 `tracing::warn!`；工厂失败 `tracing::warn!` + `runtime.notify(Notice::warning("降噪启动失败，本次已关闭"))` |
| 重采样 | `:829` `deps.resample(format.sample_rate, session.input_sample_rate())` | **直通模式下不建**（在 `else` 分支，`:828`） |
| 单声道 | 无需建 | `to_mono` 是 `AudioChunk` 上的固有方法（`ports.rs::AudioChunk::to_mono`） |

`Worker::reconnect`（`:1022`）断线后 `resampler.reset()`（`:1033`）与 `denoiser.reset()`（`:1036`），**不动门**。

`Worker::teardown`（`:1543`）顺序：停采集 → `session.close_frame` → 关 transport → `close_sink(&mut self.sink)` → `close_sink(&mut self.monitor_sink)` → `gate = None` → `denoiser = None` → `resampler = None`。

### 1.3 `Op` 数组顺序与实际执行顺序：**已经一致**（但有三个字段是死的）

- `Composition::OP_ORDER = ["mono", "denoise", "resample"→"gate"→…]`（`composition.rs:412`）= `["mono", "denoise", "gate", "resample"]`。
- `speak::composition` 推 `ops` 的顺序就是它（`speak.rs:43-56`：`Mono` → 可选 `Denoise` → `Gate` → 非直通时 `Resample`）。
- `listen::composition` 硬写 `[Mono, Gate, Resample]`（`listen.rs:77-86`），无 `Denoise`。
- 断言：`composition.rs::speak_manifest_names_the_virtual_mic`（`:969-974`）断言 Speak 的 `ops` 逐字等于 `OP_ORDER`。

**⇒ 顺序维度上，清单今天说的和代码做的一致。这条不需要"改"，只需要"别弄坏"。**

**但有三处"清单说了、代码没听"，这才是 S4-B 真正要收的账**：

| # | 现象 | 证据 | 后果 |
| --- | --- | --- | --- |
| **D1** | `Op::Resample { from, to }` 的两个字段**一个字节都没被读过**。`Plan::from`（`:119-177`）里没有 `from`/`to`；`Worker::boot` 直接 `deps.resample(format.sample_rate, session.input_sample_rate())`（`:829-832`） | `grep -n "RateRef\|from:\|to:"` 在 `pipeline/mod.rs` 只命中 `plan.rs` 侧，无读 | 清单可以把 `resample {from: playback, to: capture}` 写成"换错率了"，照样装上 |
| **D2** | `Op::Gate { config }` 的 `config` **也没被读过**。门建在 `self.config.gate` 上（`SessionConfig` 的字段，`:792`），不是清单里那个 | 同上 | 清单里那份 `GateConfig` 是**第二份真源**。今天两者相等（`speak.rs:48-50` / `listen.rs:79-81` 都原样抄 `config.gate`），但没人保证它们不漂 |
| **D3** | `Op::Mono` 装不装**完全不影响执行**。`gated_blocks:1123` 无条件 `chunk.to_mono()` | — | 清单里删掉 `mono` 也不会有任何变化 → 清单不能表达"不装" |

外加一条**语义借用**：`Worker::track_local_gate`（`:1145-1152`）用 `plan.hot_update` 当"这条腿是 Speak"的替身，靠它决定要不要在门的**本地上升沿**开一个轮次。代码注释自己写明了（`:1141-1144`）。今天成立是因为 Listen 的门是 `GateConfig::level(0.0)`（`listen_config()` 的 `gate` 字段，`mod.rs:1896`），恒 `Always`/`active=true`，只在第一块有一个伪上升沿；把 `hot_update` 的判断拿掉，Listen 会在第一块就 `begin_turn` —— **行为会变**。所以执行器必须有一个显式的"本地门驱动轮次"标志，不能顺手删。

### 1.4 `vox-dsp` 的块大小约束（为什么链上不能有"固定块长"）

| 算子 | 内部粒度 | 证据 |
| --- | --- | --- |
| 降噪 | `FRAME_SIZE = 480`（`denoise.rs:23`），**满一帧才出货**，不够返回空；**首帧被丢弃**（`:74-78`） | `Denoiser::process`（`denoise.rs:47`） |
| 重采样 | `CHUNK_SIZE = 480` 输入帧（`resample.rs:16`），内部按 `input_frames_next()` 攒；同率时 `Passthrough` 分支零开销（`:52-55`、`:90`） | `Resampler::process`（`resample.rs:88`） |
| 阀门 | 无内部帧长，但 tail / preroll 按**采集率**换算样本数（`gate.rs::samples_for_ms`） | `ActivationGate::new(config, sample_rate)` |
| 采集 | `INPUT_BLOCK_MS = 20`（`pipeline/mod.rs:47`），队列深 `INPUT_QUEUE_SIZE = 8` | 常量 |

**⇒ 四节的输入/输出长度全都不守恒，且各不相同。任何"块大小 = N"的统一接口在这里就是错的。**

---

## 2. 算子接口（回答概要稿 §6 第 2 问）

### 2.1 结论先行：分派用**枚举**，不用 trait object

任务要求权衡，我给枚举，理由三条，每条都对着本仓库的具体形状：

1. **门需要一个"外部驱动 + 状态上报"的口子，而它只有一个实现。** 门的 `set_external_active` / `set_config` / 读 `GateStatus`（`Worker::handle_note` `:1391`/`:1400`、`Worker::emit_gate_status` `:1199`）今天都在 `Worker` 里直接调 `self.gate`。做成 trait object 就得选：加一个只有门实现的 `drive()` 默认方法（RULES #4 说的"空函数钩子"）、加 `Any` 下转（`downcast` 每次一拍）、或再开一个平行的 trait object（要 `Rc`/`Arc` 共享同一块内存）。**枚举一个 `match` 就完了。**
2. **trait object 并没有换来可插拔性。** 可插拔的部分早就被 `Deps` 的工厂吃掉了（`DenoiseFactory` / `ResampleFactory`，`:73`/`:75`）：外部 crate 换的是**实现**（`vox-dsp` 的 `Denoiser`），不是**节**。`Op` 的种类是芯自己定的封闭集（`composition.rs::Op` 是 serde 数据类型，外部提交只能选这四种）。
3. **泛型会组合爆炸。** 4 节 = 2⁴ = 16 个单态化实例；S4-C 加 `Aec`（第 5 种）就是 32，再加廉价重采样档就是 64。**枚举的 `Vec<ChainStage>` 只有一个实例**（`Vec` 是运行期长度）。

**代价要说清**：加一种新节要动执行器的 `match` 臂。但这是**必须的**——新节本来就要说清"怎么被外部驱动"。真正被消灭的是 `Plan` 里那一排"装不装"的布尔（`:150-169`），那才是写死的清单。

### 2.2 接口形状

**统一的是调用约定（一份四节都必须满足的契约），不是一个 trait。** 契约写成表，逐条都有上面的证据：

| 契约 | 规定 | 依据 |
| --- | --- | --- |
| **C1 单声道 f32 进、单声道 f32 出** | 每节的 `input: &[f32]` 恒为单声道交织 | `Worker::gated_blocks:1123` 已经先 `to_mono()`；`Op::Mono` 是第 1 节，之后都是单声道 |
| **C2 长度不守恒** | 输入任意长 `N`，输出 `0..M` 块，块长任意且 ≥ 0 | 降噪满帧出货（`denoise.rs:47`）、重采样攒帧（`resample.rs:99`）、门冲 preroll 时一次出 2 块（`gate.rs::accept:240-248`） |
| **C3 块边界有意义，不许合并** | 链尾的消费者**逐块**处理。合并会让上行的 WS 帧数、每帧长度、`UploadTimeline::push` 的调用次数全变 | `Worker::feed:1087-1093` 逐块 `resampler.process` + `upload` |
| **C4 只有 `Resample` 改采样率** | 其余三节的出口率恒等于入口率 | `Worker::boot:829-832` 只在这一处换率 |
| **C5 只有 `Gate` 被外部驱动、只有 `Gate` 报状态** | 其余三节对控制面无感 | `handle_note` 只碰 `gate`（`:1391`/`:1400`） |
| **C6 只有 `Denoise` / `Resample` 有流状态** | 断线重来要清；门**不清**（它的状态归控制面，见 1.3 的 `reconnect:1033-1036` 只清这两个） | 同上 |

```rust
// crates/vox-core/src/pipeline/chain.rs（新增）

/// 一节的输出：**若干块**。边界由这一节自己定（门冲 preroll 时一次出两块）。
///
/// 缓冲由**执行器**持有并跨块复用（`begin()` 只把 `len` 归零，不 `clear` 容量），
/// 未来把某节改成 `process_into` 就能落到零分配。S4-B 本身**不**改 `ports.rs`
/// 也不改 `gate.rs`，所以这一版的槽位是从各节现成的返回值**搬**进来的。
pub struct BlockOut {
    slots: Vec<Vec<f32>>,
    len: usize,
}

impl BlockOut {
    /// 归零长度，准备接这一拍的新块。**不清容量。**
    pub fn begin(&mut self);
    /// 从一段借用切片接一块。
    pub fn push_block(&mut self, samples: &[f32]);
    /// 直接接管一节现成返回的块（**不拷贝**，S4-B 的每一条路径都走这条）。
    pub fn push_owned(&mut self, block: Vec<f32>);
    /// 本拍的块，按时间先后。
    pub fn blocks(&self) -> impl Iterator<Item = &[f32]>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool { self.len == 0 }
}

/// 算子链的一节。**四种，全列出来**；`Composition::ops` 决定装哪几种、什么顺序。
pub enum ChainStage {
    /// `Op::Mono`。交织 → 单声道（`AudioChunk::to_mono`）。
    Mono { rate: u32 },
    /// `Op::Denoise`。`None` = 装了但这一拍直通（见 `DenoiseSkip`）。
    Denoise {
        port: Option<Box<dyn Denoise>>,
        rate: u32,
        skip: DenoiseSkip,
    },
    /// `Op::Gate`。带一份"本拍的状态"槽位，`Chain::take_gate_status` 从这里取。
    Gate { gate: ActivationGate, status: GateStatus },
    /// `Op::Resample`。
    Resample { port: Box<dyn Resample>, in_rate: u32, out_rate: u32 },
}

impl ChainStage {
    /// **统一调用约定**（§2.2 的契约 C1–C3）。四个变体**形状完全一样**，
    /// 差别只在内部那一句 `match`。
    pub fn process(&mut self, input: &[Vec<f32>], out: &mut BlockOut);

    /// 本节的**出口**采样率（契约 C4）。只有 `Resample` 与入口不同。
    pub fn out_rate(&self) -> u32;

    /// 清单里叫什么（自计时的行名、差分台的指纹用它）。**取自 `Op::kind()`**，
    /// 不另写一份字符串表（否则又多一处会漂的真源）。
    pub fn name(&self) -> &'static str;
}

impl Chain {
    /// 按 `Composition.ops` 建链。**装配期**（不是热路径）。
    ///
    /// 需要的输入：清单的 `ops` 逐条 + `Deps` + 采集协商到的 `CaptureFormat`
    /// + 会话上行率。四样今天分别在 `Worker::boot` 的四段里取。
    pub fn build(
        ops: &[Op],
        deps: &Deps,
        capture: CaptureFormat,
        session_rate: u32,
    ) -> PortResult<Self>;

    /// 按装配顺序把入口块跑过每一节，**按值**交回结果。
    ///
    /// 按值（`BlockOut` 而不是 `&[Vec<f32>]`）是刻意的：返回值不借 `self`，
    /// `Worker` 之后才能随便用 `&mut self`。缓冲的容量通过
    /// [`ChainScratch::recycle`] 原样还回乒乓池，热路径零分配。
    pub fn run(
        &mut self,
        scratch: &mut ChainScratch,
    ) -> (BlockOut, Option<GateStatus>);

    /// 冲重采样缓冲里的零头（一段语音收尾时用）。链里没有 `Resample` 就给空。
    pub fn flush_resample(&mut self) -> Vec<f32>;

    /// 断线重来。`Denoise` / `Resample` 清内部缓冲，**门不动**（§2.6）。
    pub fn on_disconnect(&mut self);

    /// 链里装没装 `Op::Mono`。**没装就不混音**（今天恒混，见 §1.3 的 D3）。
    pub fn has_mono(&self) -> bool;

    /// 链尾率（供 `Worker::boot` 断言，见 §2.4）。
    pub fn out_rate(&self) -> u32;
}
```

**乒乓缓冲**（`Worker` 的一个字段，热路径只写 `len` 与几个 `u64`）：

```rust
/// 两块乒乓。跨块复用，**建链时一次性分配**。
pub struct ChainScratch {
    /// `ping[cur]` 是本拍的**输入**，`ping[cur ^ 1]` 是本节的**输出**。
    ping: [BlockOut; 2],
    cur: usize,
}

impl ChainScratch {
    /// 装入口块。`downmix = true` 时混音（`AudioChunk::to_mono`），否则原样搬。
    ///
    /// ⚠️ 两条路的**分配次数相同**（各一次 `Vec`），`begin` 不省也不多花——
    /// 省掉 `to_mono` 那个 Vec 的活儿是 S4-C 零分配化那张工单（X2/X3）的，不在本稿。
    pub fn begin(&mut self, chunk: &AudioChunk, downmix: bool);

    /// 当前这一拍的输入（`&[Vec<f32>]`）——第一节读它。
    pub fn current(&self) -> &[Vec<f32>];
    /// 乒乓的另一半，可写——本节把结果写进它。
    pub fn alt_mut(&mut self) -> &mut BlockOut;
    /// 乒乓的另一半，只读——`ChainStage::process` 的输出形状（算 `out_len` 用）。
    pub fn alt(&self) -> &BlockOut;
    /// 交换乒乓（`cur ^= 1`）。**一次异或就够，不需要 `Vec` 队列。**
    pub fn swap(&mut self);

    /// 把这一拍的 `BlockOut` **按值**换出来（`mem::replace` 成空壳）。
    /// 有了它，调用方身上就不挂着任何借用，`&mut self` 照常可用（见 §3.2 的方框）。
    pub fn take_current(&mut self) -> BlockOut;

    /// 把用完的 `BlockOut` 放回那个被换空的槽（**容量复用，不分配**）。
    pub fn recycle(&mut self, out: BlockOut);
}
```

**为什么 `Denoise` 是 `Option<Box<dyn Denoise>>` + 一个 `DenoiseSkip` 原因，而不是"建不起来就不装这一节"**：今天两种降级路径的**可观察行为不一样**，差分台会逐字比：

```rust
/// 降噪没装上的两种原因。**各自对应今天 `Worker::boot:797-812` 的一个分支**，
/// 报什么、报不报通知，都不一样，别合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenoiseSkip {
    /// 采集率不是 48 kHz（`DENOISE_RATE`）。今天：**只有** `tracing::warn!`，
    /// 不发 `Notice`（`boot.rs:810`）。
    Rate(u32),
    /// 降噪器工厂起不来。今天：`tracing::warn!` **加**
    /// `Notice::warning("降噪启动失败，本次已关闭").on(pipeline)`（`boot.rs:802-806`）。
    Factory,
}
```

### 2.3 块大小：链上**没有**块大小

- 每节接受任意长度单声道 `f32`，出货 `0..n` 块（契约 C2）。链不重新切块、不插补零。
- 链**告诉**每节自己的采样率（门要它算 `tail_ms` / `preroll_ms` 的样本数），但**不告诉**它块长。
- 唯一两个真实的"块大小"留在 `Worker::pump` 外面，不进链：采集块长 `INPUT_BLOCK_MS = 20`、队列深 `INPUT_QUEUE_SIZE = 8`。
- **不引入"重采样到统一块长"**——那会改上传帧的切分（C3），是行为变化。

### 2.4 采样率变化如何沿链传播

**建链期解析一次，运行时不再算率。** 三步：

1. `RateRef` → 具体 `u32` 的解析表**建在 `Chain::build` 里**，唯一来源三个：

   | `RateRef` | 解析成 | 证据 |
   | --- | --- | --- |
   | `Capture` | `CaptureFormat.sample_rate` | `boot:785` 传进 `capture.start` 的返回值 |
   | `Session` | `session.input_sample_rate()`（Aliyun/Gemini 16k、GPT 24k） | `boot:831`；`cloud/mod.rs:168` |
   | `Playback` | `OUTPUT_SAMPLE_RATE`（24k） | `cloud/protocol.rs:28` |

2. `Chain::build` **逐条校验**，三类错各给一句人话（`PortError`）：
   - 一个 `Op::Resample { from, to }` 解析后若 `from != 链当前率`，直接 `Err`，消息里带 `to` 与当前率 → **堵死 D1**：清单从此不能说一套、代码做另一套；
   - `CaptureFormat.channels > 1` 而 `ops` 里**没有** `Op::Mono` → `Err("采集是多声道，清单里必须装 mono")`。理由：契约 C1 说链上恒为单声道，"不装 mono" 在多声道下**无定义**，不是"原样传下去"（下游的降噪/门会拿到交织样本）。**这一条同时把 §1.3 的 D3 收成一个可校验的规则**；
   - 出现 `Composition::OP_ORDER` 里没有的 `Op` 种类（防御；`Composition::validate` 已经拦过一道，这里是第二道）。
3. 建链时记 `Chain::out_rate`（链尾率）。`Worker` 用它做两处断言：
   - 有 `session` 时 `out_rate == session.input_sample_rate()`（今天恒等：`boot:829-832` 就是拿它建的）；
   - 直通时链里**没有** `Resample`，`out_rate == CaptureFormat.sample_rate`（今天直通不重采样，`feed_passthrough:1110-1113` 直接送播放汇）。

运行时 `ChainStage::process` 内部**不做任何率算术**——每节自己知道自己的率。`Op::Aec`（S4-C）若要换率，是它自己的事。

### 2.5 有状态算子怎么挂

| 节 | 状态 | 建 | 清 |
| --- | --- | --- | --- |
| `Mono` | 无 | — | — |
| `Denoise` | 480 样本攒帧缓冲 + RNNoise 内部状态 + "首帧已丢弃"标志 | `deps.denoise()`，在 `Chain::build` | `Chain::on_disconnect()` → `port.reset()`（对应今天 `reconnect:1036`） |
| `Resample` | 输入缓冲 + 复用的 `chunk` + rubato 内部状态 | `deps.resample(in_rate, out_rate)`，在 `Chain::build` | `Chain::on_disconnect()` → `port.reset()`（对应今天 `reconnect:1033`） |
| `Gate` | `prebuffer`（`VecDeque<Vec<f32>>`）+ `silent_samples` + `active` + **`external_active`** | `ActivationGate::new(op_config, capture_rate)`，在 `Chain::build` | **断线不重置**（C6，见下） |

**门的三个外部入口，都归执行器，不归通用节接口：**

```rust
impl Chain {
    /// 控制面推门（`PipelineCommand::SetGateActive` → `Note::GateActive`）。
    /// 链里没有门就是**静默忽略**——今天也是（`handle_note:1391` 的 `if let Some(gate)`）。
    pub fn set_gate_active(&mut self, active: bool);
    /// 换门参数（`Note::GateConfig`）。转 `ActivationGate::set_config`，
    /// **保留 `external_active`**（`gate.rs::set_config:136-141`，有专门的钉子用例
    /// `gate.rs::tests::set_config_keeps_external_active`）。
    pub fn set_gate_config(&mut self, config: GateConfig);
    /// 取本拍的门状态。执行器**每节跑完调一次**，`take` 语义 = 取走即清。
    pub fn take_gate_status(&mut self) -> Option<GateStatus>;
    /// 门的本地上升沿要不要开一个轮次。
    /// **今天是 `session.hot_update`**（`track_local_gate:1148`），因为 Listen 的门
    /// 恒开、没有真上升沿。S4-B 不改这个判据，只把注释里"用 hot_update 区分两条腿"
    /// 这句换成"清单里没有这一格，用 session.hot_update 顶"——见 §1.3 D 段。
    pub fn gate_drives_turns(&self) -> bool;
}
```

`Chain` 上的这三个 `match ChainStage::Gate { gate, .. }` 是**唯一**碰到门的地方；`Worker` 里 `self.gate: Option<ActivationGate>` 这个字段（`:642`）删掉。

### 2.6 `reset` 语义（断线重来）

`Chain::on_disconnect`（声明见 §2.2）**逐节按契约 C6 决定清不清**：

| 节 | 动作 | 对应今天 |
| --- | --- | --- |
| `Denoise` | `port.reset()` | `Worker::reconnect:1036` |
| `Resample` | `port.reset()` | `Worker::reconnect:1033` |
| `Mono` | 无状态可清 | — |
| `Gate` | **不动** | 今天也没有（见下） |

**门为什么不动（这是行为不变的关键一条，必须写死）：**
`ActivationGate::reset()`（`gate.rs:143-149`）会 `self.external_active = false`——**门会被关掉**。今天 `Worker::reconnect` 不碰门，所以行为是"重连后按键仍按着"。所以 S4-B **不许**在重连路径上调门的 `reset()`。将来 S4-C 的半双工门 / AEC 若要改这一条，是那一张工单的事，不在本稿。

（`prebuffer` 里的 ≤200 ms preroll 因此会跨重连留着——和今天一样。差分台场景 12 就是钉这一条；场景 12 同时钉"重连后降噪又丢一次首帧"。）

### 2.7 需要参考信号的算子（`Op::Aec`）：**只设计位置，不实现**

任务明确要求"S4-B 不实现 AEC"。所以本稿**不加** `Op::Aec`、**不加** `ChainStage::Aec`、**不加** `hw_aec`/`soft_aec` 能力位、**不写**任何半成品分支（RULES #4）。

**S4-C 的 AEC 工单按下表落，一处都不许跑偏：**

| 落点 | 形状 | 依据 / 约束 |
| --- | --- | --- |
| 清单 | `Op::Aec` 插在 `Composition::OP_ORDER` 的 `"gate"` 与 `"resample"` 之间（`composition.rs::OP_ORDER`，`:412`）——**AEC 必须在降噪之后**（降噪会削掉回声里的噪声底，让估计更准）、**必须在重采样之前**（它要在采集率上做，跨时钟域的重采样会把估计搞坏） | 这是 S4-C 选型时才能最终定的；本稿给的是**约束**，不是选型 |
| 链上 | `ChainStage::Aec { port: Box<dyn Aec>, rate: u32 }`，插在 `Gate` 之后 | 契约 C1–C6 逐条照抄，不许开特例 |
| **参考信号的口子** | `Chain::push_playback_reference(&mut self, samples: &[f32])`，与上面三个门控口**同层**（`match ChainStage::Aec`）。调用点唯一：`Worker::handle_server_event` 的 `ServerEvent::AudioDelta` 分支，**在 `sink.push(&samples)` 之前**（`:1264-1270`） | 外放的译音是我们自己播的，参考信号在进程内现成（概要稿 §2.2）。**不另开一条路抓系统声音。** |
| 链里没有 AEC 时 | `push_playback_reference` 走 `match` 找不到 `Aec` 臂 → 什么都不做。**零分配、零拷贝**（`samples` 是借用） | 50 Hz 一次 `match`，成本 ~1 ns |
| **时钟域（未决，见 §9）** | 播放是 24 kHz（`OUTPUT_SAMPLE_RATE`），采集是 48 kHz。AEC 要把 24 k 的参考升到采集率、并估计麦克风↔喇叭时延 | 概要稿 §2.2 说"同声卡时延稳定"，但**同声卡是否保证同采样率**没有核实过 |
| 降级 | 能力位假 → `Op::Aec` **不进清单**（`Composition::of` 的开门/关门，与 `Op::Denoise` 同一套机制） | `composition.rs::of` 的既有做法 |

**为什么口子不先埋**：今天就加一个没人调的 `push_playback_reference`，就是 RULES #4 说的"空函数钩子"。**口子和它的第一个实现在同一张工单、同一轮提交里落地**，不留中间态。

### 2.8 状态上报

| 上报物 | 现在的路 | S4-B 之后 |
| --- | --- | --- |
| 门状态 + RMS | `Worker::gated_blocks:1134` 调 `emit_gate_status` | 一样。执行器每节跑完 `chain.take_gate_status()`，拿到就走**原封不动的** `emit_gate_status` / `track_local_gate` |
| 本地轮次起点 | `Worker::track_local_gate:1145` | 一样，只是判据从 `plan.hot_update` 换成 `chain.gate_drives_turns()`（值相同） |
| 降噪降级 | `Worker::boot:797-812` 的两条 `tracing::warn!` + 一条 `Notice` | 一样。`Chain::build` 返回 `DenoiseSkip`，`Worker::boot` 按变体发**逐字相同**的两条日志（其中一条的 `Notice` 进事件流，差分台能比） |
| 其它节 | 无 | **无**。降噪/重采样/单声道今天就不报任何状态，链上也不许新报（会改事件流，差分台会红） |

---

## 3. 执行器

### 3.1 `Plan` 收缩成什么

**不删**。理由：它还是"这一轮跑什么"的描述子，而且 `passthrough` / `playback_device` / `hot_update` / `params` 四个字段**都不是算子**，删了 `Plan` 就得让 `Worker` 回到"第二份真源"。

```
Plan（6 个字段，2026-09-30 核）             Plan（6 个字段：删 1 增 1）
  target        ── 保留 ──▶  target
  denoise: bool ── 删除 ──▶  （由 Chain 决定）
  passthrough   ── 保留 ──▶  passthrough
  playback_device ─ 保留 ──▶  playback_device
  monitor_translation ─ 保留 ──▶  monitor_translation
  hot_update    ── 保留 ──▶  hot_update
  params        ── 保留 ──▶  params
                                chain_ops: Vec<Op>   ← 新增：清单的 ops 逐条照搬
```

| 动作 | 字段 | 理由 |
| --- | --- | --- |
| **删** | `denoise: bool` | 概要稿 §4 的验收原文："`Plan` 里不再有'装不装某一节'的布尔"。它已被 `chain_ops` 完整表达 |
| **加** | `chain_ops: Vec<Op>` | `Plan::from:150` 今天只把 `ops` 压成一个 `denoise` 布尔，其余全丢。改成一比一照搬，**建链的输入从这一刻起就是清单**（堵死 D1/D2/D3） |
| 不动 | `passthrough` | 它不是"装不装某一节"，是"有没有云端会话"（`session.is_none()`）。`pump` / `pump_passthrough` 的分叉靠它，删了要么让 `Chain` 知道云端（越界），要么在 `Worker` 里重新判断 `plan.session`（新的第二份真源） |
| 不动 | `hot_update` | 它是**控制面**语义（认不认 `HotUpdate`）。门驱动轮次这个借用改成 `Chain::gate_drives_turns()`，但它的**值**仍来自 `session.hot_update`（`Plan::from:169`） |
| 不动 | `target` / `playback_device` / `monitor_translation` / `params` | 都不是算子 |

`Plan` 的可见性、可见调用方、构造路径**全部不变**（`pub(crate)`，唯一入口 `Plan::from`）。

### 3.2 `Worker` 怎么按 `Composition.ops` 顺序执行

```
Worker::boot（:745）
  ├─ 连 socket / 开播放汇 / 开采集        ← 一行不动
  ├─ let format = capture.start(…)        ← 一行不动
  ├─ self.chain = Chain::build(&self.plan.chain_ops, &self.deps, format,
  │                            self.session.input_sample_rate())?   ← 新增（取代 :792-833）
  ├─ 链建好后，原样搬走三件事：
  │    · 门：chain.set_gate_active(self.config.gate_active)          ← 取代 :794
  │    · 降噪降级：按 chain 的 DenoiseSkip 发原来那两条日志/通知      ← 取代 :797-812
  │    · 直通分支的播放汇：仍读 plan.playback_device + format.sample_rate  ← 一行不动
  └─ 后面全不动

Worker::pump（:920）          ← 一行不动
Worker::feed（:1083）         ← 换成 self.feed_chain(…)
Worker::gated_blocks（:1119） ← 删掉
```

新的单块路径（`pump` 里那一拍对每块调一次）。**它与重构前 `Worker::feed`（`:1083`）+
`Worker::gated_blocks`（`:1119`）逐条同构**：第一节做混音、后面按清单跑、链尾**逐块**
走下游、`status.ended` 时冲重采样尾巴。差别只有一处——"装哪几节"由 `Chain` 说了算。

> **借用形状是这份稿子里最容易被写歪的一处，先说死**：`Chain::run` **按值返回一个
> `BlockOut`**（`take_current` 用 `std::mem::replace` 把它从乒乓槽里换出来，那个槽
> 留下一个空壳），`Worker` 用完再 `scratch.recycle(out)` 放回同一个槽——容量全程复用
> （**零分配**），同时**没有任何借用挂在 `self` 上**，于是后面 `self.emit_gate_status(...)`
> / `self.upload(...)` 这些 `&mut self` 调用照常成立。**这正是今天 `gated_blocks` 返回
> `Vec<Vec<f32>>`（`&mut self` 的返回值不借 `self`）的同一个道理。**
> 不许改成"返回一个借用的 `&[Vec<f32>]`"——那样 `&mut self` 就用不了了。

```rust
/// 一块采集音频走完全程。链按 `Composition.ops` 的顺序跑，尾部逐块处理。
///
/// 与重构前 `Worker::feed` + `Worker::gated_blocks` 的差别**只有**"哪几节"由
/// `Chain` 决定：块边界、单块上传、`status.ended` 冲尾巴都逐条照抄。
/// 差分台场景 1–15 钉的就是它。
fn feed_chain(&mut self, chunk: &AudioChunk, capture_start_ms: u64, capture_end_ms: u64) {
    // 入口块。`Op::Mono` 装了就混音（`AudioChunk::to_mono`，今天的分配同一处）；
    // 没装就原样搬（此时采集必是单声道，`Chain::build` 校验过——见 §2.4）。
    self.scratch.begin(chunk, self.chain.has_mono());

    // 按值拿回结果（借用不挂在 self 上，见上面的方框）。
    let (mut out, gate) = self.chain.run(&mut self.scratch);

    // `Denoise` 攒帧中时链出 0 块 → 今天 `gated_blocks:1129-1131` 的早退。
    let Some(status) = gate.filter(|_| !out.is_empty()) else {
        self.scratch.recycle(out);
        return;
    };
    // 原封不动搬过来的两件事。判据从 `plan.hot_update` 换成 `chain.gate_drives_turns()`
    // （值相同，见 §1.3 D 段 / §2.5）。
    self.emit_gate_status(status);
    self.track_local_gate(status);

    // 链尾**逐块**处理（契约 C3：块边界不许合并，否则 WS 帧数与 `UploadTimeline`
    // 的调用次数都会变）。直通时下游是播放汇，否则是上传。
    let passthrough = self.plan.passthrough;
    for block in out.blocks() {
        if passthrough {
            if let Some(sink) = self.sink.as_mut() {
                sink.push(block);
            }
        } else {
            self.upload(block, capture_start_ms, capture_end_ms);
        }
    }

    // 一段收尾：把重采样缓冲里的零头挤出去（`feed:1095-1101`）。
    // **直通路径今天不冲尾巴**（`feed_passthrough:1106` 里没有这一段），照抄。
    if status.ended && !passthrough {
        let tail = self.chain.flush_resample();
        self.upload(&tail, capture_start_ms, capture_end_ms);
    }

    // 缓冲还回乒乓池（容量复用，本拍零分配）。
    self.scratch.recycle(out);
}
```

对应的**前重构**代码是 `gated_blocks`（`:1119`，产 `Vec<Vec<f32>>`）+ `feed`（`:1083`，逐块
`resampler.process` + `upload`）+ `feed_passthrough`（`:1106`，逐块 `sink.push`）——三者合并成上面一个。
`Worker::pump`（`:920`）与 `pump_passthrough`（`:972`）**一行不动**。

`Chain::run` 内部：

```rust
/// 按装配顺序把 `in` 跑过每一节，结果写进 `out`。
///
/// **两块乒乓**（`out_a` / `out_b`）：一节的输入永远不是它的输出，
/// 这样将来"就地改"的节也写不了别名。
pub fn run(&mut self, in_blocks: &[Vec<f32>], scratch: &mut ChainScratch) -> &[Vec<f32>]
```

`Worker` 的 `sink` / `monitor_sink` / `transport` / `capture` / `session` / 全部账本交互 **一行不动**。`Worker` 结构体只少三个字段：`denoiser`（`:640`）、`resampler`（`:641`）、`gate`（`:642`）；多两个：`chain: Chain`、`scratch: ChainScratch`。

### 3.3 两条流水线的差别只剩"清单长什么样"

| | Speak | Listen | 差别落在清单的哪一格 |
| --- | --- | --- | --- |
| `ops` | `[mono, denoise, gate, resample]` | `[mono, gate, resample]` | `speak.rs:45` 的 `if config.denoise` / `listen.rs:77-86` |
| 门的 `config` | `config.gate`（`MANUAL` 或 `level(t)`） | `config.gate`（`level(0.0)` 恒开） | `speak.rs:48-50` / `listen.rs:79-81` |
| `session.hot_update` | `true` | `false` | `speak.rs:115` / `listen.rs:95` |
| 播放角色 | `virtual_mic`（位假则退 `speaker`） | 恒 `speaker` | `speak.rs:61-67` / `listen.rs:57-62` |
| 输入 | `Mic` | `ProcessLoopback` | `speak.rs:36` / `listen.rs:45` |

**`Worker` 与 `pipeline/mod.rs` 全文里，`speak.rs` / `listen.rs` 只剩"派活"这一步**——`Worker::new(config, plan)` 里那个 `plan: Plan` 已经是两条腿唯一的分叉。`PipelineEngine::start`（`:217`）也只认 `config.pipeline` 来选哪份 `composition`，那是**造清单**，不是**执行**。

### 3.4 热更新在算子链下的语义

`PipelineCommand`（`runtime.rs:68`）六种，S4-B 逐条定语义：

| 命令 | 链上发生什么 | 是否重建链 |
| --- | --- | --- |
| `SetGateActive` | `chain.set_gate_active(bool)`（`handle_note:1387-1394` 的序号闸 `accept_seq` 照旧） | 否 |
| `SetGateConfig` | `chain.set_gate_config(config)` + `self.config.gate = config` + 节流复位（`handle_note:1395-1406`） | 否 |
| `HotUpdate` | **链上一动不动**。`plan.hot_update` 为假就扔（`handle_note:1411`）；为真则 `Worker::hot_update` → `session.update` 帧 → 写回 `plan.params`（`hot_update:1476` / `send_hot_change:1526`） | 否 |
| `SetTranslationAudio` | **链上一动不动**。只换 `sink` / `monitor_sink` 与 `plan.playback_device`（`set_translation_audio:1487`） | 否 |
| `SetMonitorTranslation` | **链上一动不动**（`set_monitor_translation:1430`） | 否 |
| `Start` / `Stop` | `Start` 里 `Worker::boot` 建链；`Stop` 里 `teardown` 丢链（`teardown:1561-1563` 换成一行 `self.chain = Chain::empty()`） | 是（整条会话） |

**硬不变量（本稿新增，必须有钉子用例）**：

> **INV-1**：**任何 `Note` 都不许重建算子链。** 理由：重建 = 丢 `Denoiser` 的 480 帧缓冲与 RNNoise 状态、丢 `Resampler` 的输入缓冲 = 上行时间线上凭空少一段/多一段，且**首帧丢弃**（`denoise.rs:74-78`）会再来一次。改语言 / 换音色 / 换门 / 换播放设备，全是下行与控制面的事，碰不到上行链。
>
> ⇒ 将来 S4-C 的"廉价重采样档"若要**运行期**切换档位，必须走 `Stop` + `Start` 整轮，或者引入一条新的 `PipelineCommand`——**不许**塞进 `Note`。这一条写在这里，是给 S4-C 的施工单用的。

**INV-2**：`Worker::reconnect`（`:1022`）**不重建链**，只调 `chain.on_disconnect()`（= 今天的 `resampler.reset()` + `denoiser.reset()`，门不动，见 §2.6）。

---

## 4. 热路径零新增分配（`RULES.md` #6）：逐条记账

### 4.1 今天每块音频的分配

以"带翻译的 Speak、48 kHz 采集、20 ms 块、降噪开"为例，一块 = 960 个交织样本（单声道）/ 20 ms：

| # | 位置 | 分配物 | 证据 |
| --- | --- | --- | --- |
| A1 | 采集回调 → 信箱 | `AudioChunk { samples: Vec<f32> }` | `Inbox::push_audio:426`（`Mic::emit` 在测试里是 `vec![…]`，真机是驱动那边分配的） |
| A2 | `Inbox::take_audio` | `VecDeque` 内部可能增长（稳态不涨） | `:450` |
| A3 | `chunk.to_mono()` | 1 个 `Vec<f32>`（960 样本） | `ports.rs:46`；单声道时是 `self.samples.clone()`，**照样分配** |
| A4 | `Denoiser::process` 每帧 | `scaled_in: Vec` + `chunk = buffer.drain(..480).collect()` + `output: Vec` + `normalized: Vec` + 外层 `frame: Vec` ≈ **5 个 / 帧**（20 ms = 2 帧 → 约 10 个） | `denoise.rs:47-83` |
| A5 | `ActivationGate::process` | `Vec<Vec<f32>>` 容器 + 每块 1–2 个 `Vec<f32>`（`samples.to_vec()` / preroll / 尾巴 `vec![0.0; n]`） | `gate.rs:152`、`:179`、`:191`、`:216`、`:246`、`:261` |
| A6 | `Resampler::process` | `output: Vec::with_capacity(...)`（同率时 `input.to_vec()`） | `resample.rs:90`、`:98` |
| A7 | `Session::audio_frame` | `float_to_pcm16` 的 `Vec<u8>` + `next_event_id` 的 `String` + `ClientEvent::to_json` 的 `String` + `base64` 的 `String` + `transport.send` 那份 | `cloud/mod.rs:227-240` |
| A8 | `emit_gate_status` | 0（`GateStatus` 是 `Copy`） | `:1199` |
| A9 | `latency.input_queue` / `upload_timeline.push` | 0 | `:938`、`:1183` |

**A1/A2/A7 不在 S4-B 的射程**（采集侧与协议侧）。S4-B 关心的是 **A3–A6**。

### 4.2 新执行器的账

| 位置 | 变化 | 说明 |
| --- | --- | --- |
| A3 `to_mono` | **±0** | 装了 `Op::Mono` 就照调（同一个 `Vec`）。没装时 `scratch.begin` 直接搬 `chunk.samples`——**同样一次 `Vec` 拷贝、同样一次分配**（见 §2.2 的 `begin` 注释）。现有清单恒带 `mono` |
| A4 降噪 | **±0** | `ChainStage::Denoise::process` 原样调 `port.process(&input)`。**S4-B 不动 `ports.rs`、不动 `vox-dsp`**（见 §4.4） |
| A5 门 | **±0** | `ChainStage::Gate::process` 原样调 `gate.process(&input)`，返回的 `Vec<Vec<f32>>` 用 `push_owned` **搬**进 `BlockOut`（**不拷贝**）。`BlockOut` 的 `slots` 是**建链时**一次性 `Vec` |
| A6 重采样 | **±0** | 原样调 `port.process(block)`，`push_owned` 搬进 `BlockOut` |
| 执行器自身 | **+0** | `Chain::run` 里的 `for (i, stage) in self.stages.iter_mut().enumerate()`、`BlockOut::begin`（只写 `self.len = 0`）、`blocks()`（`slice::Iter`）、计时（见 §5）。**没有一个 `Vec` / `String` / `format!` / `clone()`** |
| 尾部 `resampler.flush()` | **±0** | 结果照样 `upload(&tail, …)`（`feed:1096-1100` 的形状） |

**净结论：新执行器每块音频新增分配 = 0，移除 = 0。**（`RULES.md` #6 的原文是"不许**新增**分配或拷贝"，达标。）

`BlockOut` 的复用能力是为 S4-C 铺路的（把 `gate.process` / `Denoise::process` 改成 `process_into` 之后，A5/A4 就能归零），**但 S4-B 自己不享受**，因为那要动 `gate.rs` 与 `ports.rs` 的公共接口并牵动 `vox-dsp`（S4-C 的活，见 §4.4）。

### 4.3 trait object vs 泛型/枚举：见 §2.1（枚举），补一条量化

一节一次 `match` 的成本 ≈ 1–2 ns；50 块/秒 × 2 条流水线 × 4 节 ≈ **400 次/秒 → < 1 µs/秒**。对照已知的实测基线（采集链 3.03 ms/s、RNNoise 2.5 ms/s，`AGENTS.md`），**执行器自身的分派开销在噪声里测不出来**，不值得为了它引入泛型或 trait object。选枚举是为了**代码形状**（门的驱动口），不是性能。

### 4.4 为什么不改 `ports.rs` / `gate.rs` / `vox-dsp`（三处刻意留白）

| 文件 | 改它会怎样 | 本稿的决定 |
| --- | --- | --- |
| `crates/vox-core/src/ports.rs`（`Denoise::process -> Vec<f32>`、`Resample::process -> Vec<f32>`） | 加 `process_into(&mut self, in: &[f32], out: &mut Vec<f32>)`。**调用方**：`crates/vox-dsp/src/ports.rs`（S4-A W2 已建，`:15` `impl Denoise for Denoiser`、`:25` `impl Resample for Resampler`）+ `pipeline/mod.rs` 的假件 `DenoiseHandle` / `Decimate` | **不改。** W2 刚合并，交叉动它会让 S4-B 与 S4-A 的"已落地部分"重新耦合。记为 **S4-C 的一张工单**（名字：`ports` 的 `process_into` 化），它同时能干掉 A4/A5/A6 |
| `crates/vox-core/src/gate.rs`（`ActivationGate::process -> Vec<Vec<f32>>`） | 加 `process_into(&mut self, in: &[f32], out: &mut BlockOut) -> GateStatus`。**调用方**：门自己的 5 条单测（`gate.rs:291-383`）+ 链的适配器。改它就得改那 5 条既有测试 | **不改。** 理由同上，且动它必然要改既有测试，违反"既有测试逐字未改"的纪律 |
| `crates/vox-dsp/src/{denoise,resample}.rs` | 去掉 `denoise.rs:51` 的 `drain(..FRAME_SIZE).collect()`、`:70` 的 `scaled_in`、`:81` 的 `normalized` | **不改。** 这是 S4-C"廉价重采样档"那张工单的射程（概要稿 §3 S4-C 表） |

**这三处是本稿最重要的取舍**：S4-B 的承诺是"行为不变 + 零新增分配"，不是"零分配"。提前做性能改造会让 B0 录的金样失效（金样必须在**重构前**录），也把一个"行为不变"的工单变成"顺手优化 DSP"的工单。

---

## 5. 自计时（S4 概要稿 §2.5-1）

### 5.1 判断：**计量进 S4-B，外露不进 S4-B**

| | 决定 | 理由 |
| --- | --- | --- |
| **计量**（执行器逐节计时 + 零分配累加 + 断口） | ✅ 进 S4-B | 就在 `chain.rs` 与 `pipeline/mod.rs` 里，**不碰任何 S4-A 的文件**；而且它是"执行器天然知道每节边界"这件事的直接兑现，晚做等于让执行器的形状先长歪再补 |
| **外露**（进 `Snapshot` / `Event` / Tauri DTO / MCP `Status` / 前端 TS） | ❌ 拆成后续工单，**排在 S4-A W5 之后** | ① 它要改 `app/src-tauri/src/dto.rs`（`PipelineSnapshotDto`）、`crates/vox-host/src/{core,report}.rs`（S4-A W4/W5 的独占文件）、`crates/vox-mcp/src/resources.rs`、`app/ui/src/types.snapshot.ts` —— **四个文件此刻都属 S4-A**；② 它改的是**产品可见的线路形状**（前端拿到的 `snapshot` JSON 多了字段、MCP 资源多了字段），按概要稿 §4 那行的口径（"快照里每个算子都有耗时与实时倍率"）应该由 S4-C 收口，而不是在 S4-B 顺手塞进去 |

### 5.2 计量：每节耗时与实时倍率

**执行器天然知道每节边界**：`Chain::run` 的 `for` 循环体首尾就是边界。

```rust
// crates/vox-core/src/pipeline/chain.rs

/// 一节的累计耗时。**全是定长整数，没有 `Vec` / `String` / 滚动窗口。**
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpTiming {
    /// 跑过多少拍。
    pub blocks: u64,
    /// 累计墙钟（纳秒）。
    pub total_ns: u64,
    /// 单拍最慢（纳秒）。嵌入式排障最看这个。
    pub max_ns: u64,
    /// 累计**出口**音频时长（纳秒）。实时倍率的分母。
    pub audio_ns: u64,
    /// 本节当前的采样率（出口率）。
    pub out_rate: u32,
}

impl OpTiming {
    /// 实时倍率 = 处理墙钟 / 音频时长。**百分比**（0.3% = 0.3）。
    /// 零音频时长（静音拍）时给 `0.0`，不炸。
    pub fn realtime_percent(&self) -> f64 {
        if self.audio_ns == 0 {
            return 0.0;
        }
        (self.total_ns as f64) * 100.0 / (self.audio_ns as f64)
    }
}
```

**累加点**（在 `Chain::run`（§2.2）的节循环里，热路径每拍每节两次时钟读）：

```rust
for (i, stage) in self.stages.iter_mut().enumerate() {
    let t0 = Instant::now();
    stage.process(scratch.current(), scratch.alt_mut());   // §2.2 的调用约定
    let dt = t0.elapsed();                                  // 第二次时钟读

    // 计量本身：4 个整数加法 + 1 次取最大。**零分配、零拷贝、零格式化。**
    let out_len: usize = scratch.alt().iter().map(Vec::len).sum();
    let slot = &mut self.timings[i];
    slot.blocks += 1;
    slot.total_ns += dt.as_nanos() as u64;
    slot.max_ns = slot.max_ns.max(dt.as_nanos() as u64);
    // 出口音频时长：整数除法，无浮点、无分配。
    slot.audio_ns += (out_len as u64) * 1_000_000_000 / (stage.out_rate() as u64);
    scratch.swap();                                        // 乒乓
}
```

（`timings` 是 `Chain` 的字段，**建链时一次性分配** `[OpTiming; MAX_OPS]`，只被工作线程碰，
与 `LatencyTracker` 同一条"不加锁"的规矩，`latency.rs:78`。）

**开销**：一节两次 `Instant::now()`。x86-64 上走 vDSO 约 20–25 ns，一拍 4 节 ≈ **200 ns / 20 ms = 0.001%**。相对 RNNoise 的 2.5 ms/s（0.025%）可以忽略。**结论：计量本身不违反 RULES #6，因为它不新增分配**；若将来要更省，可用 `rdtsc`，但那是 S4-C 的事，本稿不做（不做为了避免引入不可移植的代码）。

**注意**：`Gate` 拍出去的块数可能 > 1（preroll），`Denoise` 常常是 0（攒帧中）。两者都要照实记，否则实时倍率的分母会错。

### 5.3 存在哪、怎么进快照

**存**：`Chain.timings: [OpTiming; MAX_OPS]`（`MAX_OPS = 6`，S4-C 加 `Aec` 后是 5 种，留一格）。**建链时一次性分配，执行器热路径只写整数。** 只被工作线程碰，不加锁（与 `LatencyTracker` 同一条规矩，`latency.rs:78`）。

**读**：在 `Worker::emit_latency`（`:862`）那个**已经在的 500 ms 节流点**上重建一次快照。

> ⚠️ **`RollingMetric::summary`（`latency.rs:59-76`）每次调用会 `collect()` + `sort` 一个 `Vec`**。这是今天就有的、且发生在 500 ms 节流点上——所以"RULES #6 不许新增分配"的口径是**每拍/每帧**，不是"每 500 ms"。自计时的快照重建放在**同一个节流点**，分配次数与今天完全相同（+1 个 `Vec<OpTiming>` 的 `to_vec()`，每 500 ms 两次，共 4 次/秒，每次 ≤ 6 × 56 字节）。**这一点必须写进 B3 的验收**：在 `emit_latency` 上跑分配计数断言（见 §8.3）。

**出口（S4-B 范围内）**：一个**纯读取**的公共 API，**不发 `Event`、不改 `Snapshot`、不改线路形状**：

```rust
// crates/vox-core/src/runtime.rs

/// 某条流水线各算子节的耗时与实时倍率（拉取式，**不发事件**）。
///
/// S4-B 只把计量做在芯里并给出这个口；**进 `Snapshot` / 界面 / 控制面是 S4-C
/// 的一张独立工单**（它要改 `dto.rs` / `vox-host` / `vox-mcp` / `app/ui`，
/// 那些文件在 S4-A 施工期间归 S4-A，见 `S4-A-HOST-LAYER.md` §4 的文件清单）。
/// 在那张工单落地前，控制面读不到这一格——**这是有意的**，不是漏了。
pub fn op_timings(&self, pipeline: Pipeline) -> Option<Vec<OpTiming>>;
```

它由 `Worker` 在同一个 500 ms 节流点上通过 `Runtime::on_op_timings(pipeline, session_id, Vec<OpTiming>)` 写进 `PipelineStatus`（与 `on_latency`（`runtime.rs:1030`）**完全同构**：写进锁内 slot、**不发 `Event`**）。

**为什么"只拉不发"不算半成品（RULES #4）**：
1. 它是一个**完整可用**的公共 API，有独立的单测（§8.3 的第 3 条）；
2. 它零分配地产生真数据（不是空壳）；
3. 它有明确的、写在代码注释里的下游消费者与排期（S4-C），不是"以后也许用"；
4. 替代方案（现在就发 `Event`）会改线路形状，而 S4-B 的验收是**逐字相等**。

**计时从哪一刻开始算**：`Chain::run` 的节边界。**上行链的节**（`mono`/`denoise`/`gate`/`resample`）在 S4-B 有计时；**下行**（`sink.push`）今天不计时、S4-B 也不加（`push` 属播放端口，不是算子）。这与概要稿 §2.5-1 的"每个算子记录每块耗时"一致。

---

## 6. 差分台（**先在重构前的代码上录金样**）

### 6.1 落点与形态

| 项 | 决定 | 理由 |
| --- | --- | --- |
| 文件 | `crates/vox-core/src/pipeline/golden.rs`（新增）+ `crates/vox-core/src/pipeline/golden/*.txt`（新增） | **`Plan` / `Worker` / `mod tests` 里的假件全是私有的**（`struct Rig`（`:1915`）是 `pub(crate) mod tests` 里的**私有**项），`tests/` 下的集成测试**够不着**。`golden.rs` 是 `pipeline` 的**兄弟模块**，加 `#[cfg(test)] pub(crate) mod golden;` 就能用同一批假件，**一份假件不复制** |
| 金样格式 | 纯文本，一行一个可观察事件 | 逐字可比、`git diff` 可读、不引新依赖 |
| 读取 | `include_str!("golden/<id>.txt")` | **文件缺失 = 编译错**，而不是运行时一个看不懂的 IO 错 |
| 重录 | `VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden` 覆写文件并打一行醒目提示 | 显式开关，验收命令里绝不含这个变量 |
| 与既有测试的关系 | **`mod tests`（`pipeline/mod.rs:1567-3055`）的 41 条用例（`#[test]` 实数，2026-09-30）一行不改**，除了把 B0 清单里那些符号的可见性从私有放宽到 `pub(crate)` | 见 §6.2 |

### 6.2 B0 要放宽的可见性（机械，逐个列全）

`golden.rs` 需要用到的、现在是私有的符号（全部在 `pipeline/mod.rs`）：

| 符号 | 现在 | 改成 | 用在哪 |
| --- | --- | --- | --- |
| `struct MemoryStore`（`:1614`） | 私有 | `pub(crate)` | 造 `Rig` |
| `struct Wire`（`:1639`）+ `impl Wire` 的 `sent`/`audio_frames`/`open_inbox`/`push_message`/`push_closed` | 私有 / 方法私有 | `pub(crate)` | 读发出去的帧 |
| `struct WireHandle`（`:1690`） | 私有 | `pub(crate)` | 认得出 `Box<dyn Transport>` |
| `struct Mic`（`:1727`）+ `emit` / `target` | 私有 | `pub(crate)` | 灌音频 |
| `struct MicHandle`（`:1758`） | 私有 | `pub(crate)` | 同上 |
| `struct Speaker`（`:1785`）+ `SpeakerHandle`（`:1795`） | 私有 | `pub(crate)` | 读播放汇收到的样本 |
| `struct Dsp`（`:1827`）+ `DenoiseHandle`（`:1834`） | 私有 | `pub(crate)` | 计降噪调用 / 触发工厂失败 |
| `struct Decimate`（`:1847`） | 私有 | `pub(crate)` | 让重采样比例可断言 |
| `struct Rig`（`:1915`）+ 它的字段与 `impl` 的方法 | 私有 | `pub(crate)` | 场景驱动 |
| `impl Inbox { counters }`（`:478`，已 `#[cfg(test)]`） | 私有 | `pub(crate)` | 同步点 |

**验收**：`git diff` 只能看到这些行——`struct X` → `pub(crate) struct X`、`fn f` → `pub(crate) fn f`。**任何一条 `assert` / 任何一行函数体都不许动。**

### 6.3 轨迹里放什么 / 不放什么

**放**（全部逐字稳定）：

| 行 | 形状 | 稳定性论证 |
| --- | --- | --- |
| `wire` | `<序号> <帧摘要>`：非音频帧打**原样 JSON**（`session.update` / `session.close` 等）；音频帧打 `audio_append <帧号> <样本数> <FNV-1a64 十六进制 16 位>` | `Wire::sent` 是 `Vec<String>`，顺序 = 发送顺序。`Worker::upload` 逐块调 `transport.send`（`:1188`），所以**帧边界与块边界都在指纹里** |
| `play` | `play <第几次 push> <样本数> <FNV-1a64>`；`flush <次数>`；`close <次数>`；`open <次数> <设备名或 ->` | `Speaker`（`:1785`）已经在 `push` / `flush` / `close` / `open` 上记账 |
| `dsp` | `denoise_calls <n>`；`denoise_resets <n>` | `Dsp`（`:1827`）已经在记 |
| `capture` | `open <target 摘要>`；`stop <次数>` | `Mic::target`（`:1753`）、`Mic::stops` |
| `event` | `event <Event 的紧凑 JSON>`（`Event: Serialize`，`event.rs:125`） | 覆盖 `PipelineState` / `GateStatus` / `SubtitleDelta` / `Notice` / `UsageChanged` / `SourceDetected` / `MicActive` / `DevicesChanged` |
| `wire_life` | `connect <n>`；`close_transport <n>` | `Wire::connects` / `Wire::closes` |
| `meta` | 场景 id、配置摘要（`translate` / `denoise` / `gate` / 采集率 / 流水线） | 让人肉定位 |

**不放**（并写明为什么）：

| 不放 | 原因 |
| --- | --- |
| `Event::LatencyChanged` 的**内容** | 它有两个墙钟来源：`Inbox::push_audio:441` 的 `enqueued_at: Instant::now()`（→ `pump:937` 的 `queue_ms` → `latency.input_queue` **和** `capture_end_ms` → `upload_timeline` → `capture_time` → `server_vad`）与 `boot:754` 的 `connect_started`（→ `connect_ms`）。**都不可复现。** 轨迹里只记一行 `event latency_changed`（**出现过几次**），不记内容 |
| `tracing::warn!` 的文本 | 差分台接的是 `Event` 流，抓不到 `tracing`。**唯一漏网的一条**是采集率≠48k 时那句 `tracing::warn!(rate, "采集率不是 48 kHz，跳过降噪")`（`boot:810`）——它**不发 `Notice`**，所以事件流里看不见。工厂失败那条（`boot:802-806`）**有** `Notice`，能被抓到。**处置**：B2 的执行器把那两句 `tracing` 逐字保留（评审时 `git diff` 人工核），并**新增一条单测**断言 `DenoiseSkip::Rate` 与 `DenoiseSkip::Factory` 的两分支都被走到（用 `Dsp::denoise_calls == 0` + `Notice` 有无来判）。**是否要加 `tracing` 抓取做金样：见 §9 未决 O-4** |
| 线程交错 | 每个场景**只跑一条流水线**、每步用 `wait_until` 同步（复用 `Rig::feed:2063` 已经做对的"等 `processed` 计数"与 `wait_until`），场景尾 `engine.shutdown()` 后再读 `events` |

**关于 `TestClock`（`:1583`）**：`Runtime::now_ms()` 走 `TestClock`，节流（`should_emit`，`:619`）全靠它 → **确定性**。场景脚本必须**手动 `clock.advance(ms)`** 来驱动 `GateStatus` 的 200 ms 节流（`GATE_THROTTLE_MS`，`:53`）与 `emit_latency` 的 500 ms 节流（`LATENCY_THROTTLE_MS`，`:61`）。**脚本里不许让时间自己流。**

### 6.4 配置矩阵（15 组；覆盖任务要求的降噪开关 / 直通 / 热更新 / 门 / 不同采样率）

| # | 场景 id | 流水线 | 关键配置 | 钉的是什么 |
| --- | --- | --- | --- | --- |
| 1 | `speak_48k_denoise_manual` | Speak | 48k、`denoise=on`、`GateConfig::MANUAL`、`gate_active=false` | **基线**：门关着时 preroll 攒、上升沿冲出 2 块、松手出静音尾、重采样进 `Wire` 的**帧数与指纹** |
| 2 | `speak_48k_denoise_off` | Speak | 48k、`denoise=off` | 降噪那一节**不装**：`Dsp::denoise_calls == 0`，其余轨迹与 1 **逐字相同** |
| 3 | `speak_48k_gate_level` | Speak | 48k、`gate=level(0.02)` | `GateState` 序列 `Silence → Speech → Tail → TailEnd`；`ended` 那一拍的重采样 `flush` 尾巴 |
| 4 | `speak_48k_gate_always_open` | Speak | 48k、`gate=level(0.0)`、`gate_active=true` | `GateState::Always` 恒真；**门驱动轮次的判据**（见 §1.3 D 段）——Listen 式的恒开门在 Speak 上是什么样 |
| 5 | `speak_48k_passthrough` | Speak | 48k、`translate=false` | **直通**：不连 WS（`connect == 0`）、不重采样、`play` 有样本、`playback_device` 跟着 `output_device` 而不是音色 |
| 6 | `speak_44k_denoise_skipped` | Speak | **44.1k** | 采集率 ≠ `DENOISE_RATE`：**不装降噪**，重采样 44.1k→16k（`step = 44100/16000 = 2`，假 `Decimate` 取整） |
| 7 | `speak_48k_denoise_factory_fails` | Speak | 48k、`dsp.fail=true` | 降级：**有** `Notice::warning("降噪启动失败，本次已关闭")`，`denoise_calls == 0`，其余与 1 相同 |
| 8 | `speak_16k_capture` | Speak | **16k** | 采集率 == 会话率：`Resampler` 走 `Passthrough`（`resample.rs:52`），**块边界必须与 48k 那组按比例一致** |
| 9 | `speak_48k_hot_update` | Speak | 48k；起会话后 `HotUpdate{ja→ko, voice}` ×2 + 一次空改动 | `session.update` 帧**逐字**、空改动**不发帧**（`send_hot_change:1527`）、**链一动不动**（INV-1） |
| 10 | `speak_48k_gate_commands` | Speak | 48k；`SetGateActive(true)` → 灌声 → `SetGateConfig(level(0.02))` → 灌声 → 一次**过期 seq** | 序号闸（`accept_seq:1464`）扔掉过期命令；`set_config` 保留 `external_active`（`gate.rs:136`）；节流复位（`handle_note:1404-1405`） |
| 11 | `speak_48k_translation_audio_toggle` | Speak | 48k；`SetTranslationAudio(None,None)` → 灌 → 再开回语音 → `SetMonitorTranslation(true)` | `sink` 的 `flush`/`close`/`open` 序列与顺序（`close_sink:610`）、`play` 归零再起 |
| 12 | `speak_48k_reconnect` | Speak | 48k；`push_closed()` → 等 `connects == 2` → 再灌声 | `Wire` 的 connect/close 计数、**重连后 denoise 又丢一次首帧**（`reconnect:1036` 的 `reset()` → `denoise.rs:68-78`）、**门跨重连不动**（`external_active` 仍为真） |
| 13 | `listen_48k_always_open` | Listen | 48k、`gate=level(0.0)`、`translate=true` | **无降噪**（`Dsp::denoise_calls == 0`）、恒开门的 `GateState::Always`、服务端 `TextDelta`/`TextDone`/`AudioDelta`/`SpeechStarted`/`SpeechStopped`/`TurnDone` 的字幕与用量事件、`play` 收译音 |
| 14 | `listen_48k_hot_update_ignored` | Listen | 48k；发 `HotUpdate` | `handle_note:1411` 的 `plan.hot_update == false` → **一条 `session.update` 都不发** |
| 15 | `speak_48k_text_only` | Speak | 48k、`voice=None` | 清单里**没有播放出口** → `Speaker::opens == 0`，译音无处可去 |

**第 2 条是这个矩阵的心脏**：1 与 2 的轨迹**必须逐字相同**（除了 `denoise_calls` 那一行），这就正面证明"装不装这一节"真的由清单说了算，而不是由 `Plan` 的布尔说了算。

### 6.5 顺序：**B0 在所有改 `Worker` 的工单之前**

```
B0（差分台 + 金样，跑在重构前的代码上）  ──▶  B1 ──▶ B2 ──▶ B3 ──▶ B4
```

**B0 的验收**（必须全过才准开 B1）：
1. `crates/vox-core/src/pipeline/golden/` 下 15 份 `.txt` 已提交；
2. `git diff` 对 `pipeline/mod.rs` **只**含 §6.2 那张表的可见性放宽；
3. `VOX_GOLDEN_REGEN=1` 连跑 3 次 → `git status` 干净（**金样本身是确定的**）；
4. 换 `RUST_TEST_THREADS=1` 与默认并行各跑一遍 → 轨迹逐字相同（**跨机器调度的稳定性**）；
5. `cargo test -p vox-core` 仍 **258 passed**（一条不少、一条不改）。

---

## 7. 改动清单与工单

### 7.1 逐文件

| 动作 | 文件 | 改什么 | owner |
| --- | --- | --- | --- |
| 修改 | `crates/vox-core/src/pipeline/mod.rs` | ①（仅 B0）假件可见性 `→ pub(crate)` + `#[cfg(test)] pub(crate) mod golden;` ②（B2）删 `Plan.denoise`、加 `Plan.chain_ops`；`Worker` 删 `denoiser`/`resampler`/`gate` 三字段、加 `chain`/`scratch`；`boot` 换 `Chain::build`；`feed` 换 `feed_chain`；删 `gated_blocks`；`reconnect` 换 `chain.on_disconnect()`；`teardown` 换一行；`mod tests` 里 4 处 `plan.denoise` 断言 ③（B3）`Worker` 在 `emit_latency` 节流点重建计时快照 | core-dev |
| 新增 | `crates/vox-core/src/pipeline/chain.rs` | `Chain` / `ChainStage` / `ChainScratch` / `BlockOut` / `DenoiseSkip` / `OpTiming` + 自己的 `#[cfg(test)]`（B1）；`OpTiming` 与 `Chain::timings`（B3） | core-dev |
| 新增 | `crates/vox-core/src/pipeline/golden.rs` | 15 个场景的驱动 + 轨迹渲染 + `include_str!` 比对 | core-dev |
| 新增 | `crates/vox-core/src/pipeline/golden/*.txt` ×15 | 金样 | core-dev |
| 修改 | `crates/vox-core/src/pipeline/speak.rs` | `mod tests` 的 `speak.rs:156` `assert!(plan.denoise, …)` → 改成断言**清单**里有 `Op::Denoise`（`plan(&config)` 改成同时拿 composition，或直接断言 `speak::composition(&config, &facts).ops`） | core-dev |
| 修改 | `crates/vox-core/src/pipeline/listen.rs` | `mod tests` 的 `listen.rs:142` 同上（断言清单里**没有** `Op::Denoise`） | core-dev |
| 修改 | `crates/vox-core/src/composition.rs` | ① `Op::kind()`（`:185`）从私有改 **`pub(crate)`**——`ChainStage::name()` 直接取它，不另写一份字符串表（多一处真源就会漂）；② `mod tests` 3 处（`:868`、`:888`、`:919-922` 的 `plan.denoise` 断言）改断言清单；③ `Op::Mono` / `Op::Resample` / `Op::Gate` 的文档注释（说明这三个字段从"死字段"变成被执行的） | core-dev |
| 修改 | `crates/vox-core/src/runtime.rs` | `PipelineStatus` 加一格 `op_timings: Option<Vec<OpTiming>>`；新增 `on_op_timings`（**不发 `Event`**）与 `op_timings`（读取口） | core-dev |
| 修改 | `docs/plans/S4-EMBEDDED-REFACTOR.md`、`docs/architecture/DIRECTIONS.md` §10.7 | 标注 §6 第 2 问"已答"、S4-B 段"已落地" | docs-scribe |

### 7.2 S4-B **一个都不碰** S4-A 的文件（并行性自证）

对照 [`S4-A-HOST-LAYER.md`](S4-A-HOST-LAYER.md) §4 的五张文件表（S4-B 与之**零交集**）：

| S4-A 的文件 | S4-B 碰它吗 |
| --- | --- |
| `crates/vox-host/**` | ❌ |
| `app/src-tauri/**`（`lib.rs` / `state.rs` / `dto.rs` / `events.rs` / `devices.rs` / `commands.rs` / `composition.rs` / `sys/**` / `platform/**` / `mcp.rs` / `persist.rs` / `dsp.rs`） | ❌ |
| `crates/vox-headless/**` | ❌ |
| `Cargo.toml`、`app/src-tauri/Cargo.toml`、`crates/vox-host/Cargo.toml`、`crates/vox-headless/Cargo.toml` | ❌（S4-B **不新增 crate、不加依赖**） |
| `crates/vox-dsp/**` | ❌（§4.4 的第三行） |
| `docs/platform/EMBEDDED.md` | ❌ |

⇒ **S4-B 的六个工单可以与 S4-A 的 W3/W4/W5 完全并行。** 两边唯一的交汇点是**收口时**（`docs/plans/S4-EMBEDDED-REFACTOR.md` 由 B4 改，与 S4-A 的 W6 撞同一文件 → **B4 排在 S4-A W6 之后**，或两者合并成一次文档提交）。

### 7.3 被改公共接口的全部调用方（`RULES.md` #5）

| 被改的符号 | 可见性 | 全部调用方（`grep` 实跑，2026-09-30） |
| --- | --- | --- |
| `pipeline::Plan`（删 `denoise` 字段、加 `chain_ops`） | `pub(crate)` | `composition.rs:596`（`use`）、`:625`、`:1133`、`:1224`、`:1385`；`pipeline/mod.rs:114`/`:119`/`:673`/`:786`/…（`Worker` 全部读点）；`pipeline/speak.rs:137`、`pipeline/listen.rs:116`（`#[cfg(test)] fn plan`）；`pipeline/speak.rs:156`、`pipeline/listen.rs:142`、`composition.rs:868/888/920`（**断言**）。**`app/**` 与 `crates/vox-headless/**`：零命中。** |
| `pipeline::Deps` | `pub` | **不改**，不迁移 |
| `pipeline::{PipelineEngine, INPUT_BLOCK_MS}` | `pub` | **不改** |
| `ports::{Denoise, Resample, CaptureSource, PlaybackSink, AudioChunk}` | `pub` | **不改**，不迁移（§4.4） |
| `gate::{ActivationGate, GateConfig, GateStatus, GateState}` | `pub` | **不改**，不迁移 |
| `composition::{Composition, Op, RateRef, …}` | `pub` | **签名一个都不改**（`Op::kind()` 由私有放宽为 `pub(crate)`，不是签名变化，外部零调用方） |
| `event::Event`、`latency::LatencySnapshot` | `pub` | **不改** |
| `runtime::{Runtime, PipelineCommand, PipelineControl, Snapshot, PipelineSnapshot}` | `pub` | `Runtime` **只加两个方法**（`on_op_timings` / `op_timings`），**不改既有签名** → `app/src-tauri/src/lib.rs`（`set_control` / `apply`）、`crates/vox-headless/src/headless.rs:233` 一行都不用改 |

**`RULES #5` 的结论：S4-B 没有一个"改了签名要迁调用方"的公共接口。** 唯一变了的类型 `Plan` 是 `pub(crate)`，调用方全在同一 crate 内，已全部列在上面。**`app/src-tauri` 与 `crates/vox-headless` 零改动**——所以"排在 S4-A W5 之后"的那部分**不在 S4-B 内**（只有 §5.1 的自计时外露，列为 §7.4 的后续工单）。

### 7.4 工单

| # | 名字 | owner | 独占文件 | 前置 | 可否与 S4-A 并行 |
| --- | --- | --- | --- | --- | --- |
| **B0** | 差分台落地 + **在重构前的代码上录金样** | core-dev | `pipeline/mod.rs`（仅可见性 + `mod golden;` 声明）、`pipeline/golden.rs`（新）、`pipeline/golden/*.txt`（新） | 无 | ✅ |
| **B1** | `Chain` / `ChainStage` / `BlockOut`（纯新增，零接线） | core-dev | `pipeline/chain.rs`（新）、`pipeline/mod.rs`（只加 `mod chain;` 与 `use`） | B0 | ✅ || **B2** | `Plan` 收缩 + `Worker` 按链执行 | core-dev | `pipeline/mod.rs`、`pipeline/speak.rs`、`pipeline/listen.rs`、`composition.rs` | B1 | ✅ |
| **B3** | 自计时计量 + `Runtime::op_timings` 读取口 | core-dev | `pipeline/chain.rs`、`pipeline/mod.rs`、`runtime.rs` | B2 | ✅ |
| **B4** | 文档回填 | docs-scribe | `docs/plans/S4-EMBEDDED-REFACTOR.md`、`docs/architecture/DIRECTIONS.md` | B3 **且** S4-A 的 W6 | — |

**依赖图**（B2 与 B3 都碰 `pipeline/mod.rs`，`RULES.md` #8 不许并行 → 串行）：

```
B0（差分台·金样） ──▶ B1（链·纯新增） ──▶ B2（Plan 收缩·Worker 接线） ──▶ B3（自计时）
                                                                              │
                                                                            B4（文档）
```

**S4-B 之外、排在 S4-A W5 之后的工单**（**不在本稿的施工范围内**，列出来是为了不让它被忘掉）：

| # | 名字 | 要改的文件（**全部属 S4-A 或其之后**） |
| --- | --- | --- |
| X1 | 自计时进 `Snapshot` / Tauri DTO / MCP `Status` / 前端 TS | `crates/vox-core/src/{runtime,event}.rs`、`app/src-tauri/src/dto.rs`、`crates/vox-host/src/{core,report}.rs`、`crates/vox-mcp/src/resources.rs`、`app/ui/src/{types.snapshot.ts,components/Latency.tsx,i18n/*}` |
| X2 | `ports::{Denoise, Resample}` 加 `process_into`（零分配化，A4/A6） | `crates/vox-core/src/ports.rs`、`crates/vox-dsp/src/{ports,denoise,resample}.rs` |
| X3 | `ActivationGate::process_into`（A5） | `crates/vox-core/src/gate.rs`（会改它自己 5 条单测） |
| X4 | `Op::Aec` + 能力位 + 参考信号口（概要稿 S4-C） | `composition.rs`、`capability.rs`、`pipeline/chain.rs`、`pipeline/mod.rs` |

---

## 8. 验收标准

### 8.1 基线

| 口径 | 值 | 出处 |
| --- | --- | --- |
| `cargo test --workspace`（`main`） | **598 passed / 0 failed / 5 ignored** | 用户提供的基线。**本轮未跑**（沙箱未批准该命令） |
| `cargo test -p vox-core` | **258 passed / 0 failed / 0 ignored** | **2026-09-30 本轮实跑**，`cargo test -p vox-core` |
| 测试总数 | S4-B **净 +5**（见下） | — |

**净变化（逐条可核）**：

| 工单 | 既有测试 | 新增 | 净 |
| --- | --- | --- | --- |
| B0 | `pipeline/mod.rs::tests` 的 41 条**断言逐字不动**（只放宽可见性） | `pipeline::golden` 的 15 条（每个场景一条）+ 1 条 `goldens_are_deterministic_when_regenerated` | **+16** |
| B1 | 0 动 | `chain.rs` 里 **10 条**：`every_op_in_the_manifest_becomes_a_stage_in_order` / `an_absent_op_is_simply_not_installed` / `mono_is_only_downmixed_when_the_manifest_says_so` / `a_multichannel_capture_without_mono_is_rejected` / `the_gate_stays_open_across_a_disconnect` / `the_chain_only_resets_the_stages_with_stream_state` / `a_manifest_whose_resample_rate_refs_disagree_is_rejected` / `blocks_keep_their_boundaries_through_the_chain` / `a_stage_sees_the_rate_it_was_built_with` / `denoise_skip_carries_which_degrade_it_was` | **+10** |
| B2 | **3 条断言改写**（`composition.rs:868`、`:888`、`:919-922`；`speak.rs:156`；`listen.rs:142` —— 共 3 个文件 5 处），**测试数不变** | **2 条**：`the_manifest_drives_the_chain_not_a_plan_flag`（证明 1/2 两场景除 `denoise_calls` 外逐字相同）、`no_note_rebuilds_the_chain`（INV-1） | **+2** |
| B3 | 0 动 | **4 条**：`timings_are_monotonic_counters_with_no_allocation` / `realtime_percent_is_zero_when_there_was_no_audio` / `a_disconnected_chain_keeps_counting_from_its_previous_totals` / `op_timings_reports_one_row_per_installed_stage` | **+4** |
| B4 | — | — | 0 |
| | | | **净 +32 → 630 passed / 0 failed / 5 ignored** |

### 8.2 每个工单收口必跑

```bash
cargo fmt --all --check                                            # 期望：静默
cargo test -p vox-core                                             # 期望：全部 ok，0 failed
cargo test --workspace                                             # 期望：全部 ok，0 failed，5 ignored
cargo clippy --workspace --all-targets -- -D warnings              # 期望：静默
```

**B0 额外（这是 B0 存在的全部理由）**：

```bash
# ① 金样文件齐全（15 份）
ls crates/vox-core/src/pipeline/golden/*.txt | wc -l               # 期望：15

# ② 金样是确定的：连录三遍，仓库必须干净
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden
git status --porcelain crates/vox-core/src/pipeline/golden         # 期望：空

# ③ 跨调度稳定
RUST_TEST_THREADS=1  cargo test -p vox-core pipeline::golden        # 期望：15 passed
RUST_TEST_THREADS=8  cargo test -p vox-core pipeline::golden        # 期望：15 passed
```

**B2 额外**：

```bash
# 差分台（此时已有金样，B2 之后必须仍然全绿 = "逐字相等"）
cargo test -p vox-core pipeline::golden                            # 期望：15 passed
```

**B2 的可观察行为**（概要稿 §4 的两行验收）：

| 验收行 | 怎么测 |
| --- | --- |
| 「差分台（多组配置 × 两条流水线）事件轨迹与重构前**逐字相等**」 | 上面那条 `cargo test -p vox-core pipeline::golden`。B0 录的 15 份 `.txt` 在 B2 之后**一个字都不许改**；改了就是行为变了 |
| 「`Plan` 里不再有'装不装某一节'的布尔」 | `grep -n "denoise\|passthrough\|resample\|gate" crates/vox-core/src/pipeline/mod.rs` 在 `Plan` 的定义体（`:90-106`）里 → 只剩 `passthrough`（它不是算子，见 §3.1）；`denoise` 零命中。**B2 的收口检查项** |

### 8.3 B3 的"计量零分配"怎么证明（`RULES.md` #6 + #10）

1. **代码论证**（写进 B3 的 PR 描述）：`Chain::run` 的循环体只有 `Instant::now()` × 2、`as_nanos()` × 2、四个 `u64` 加法/取最大、一次 `u64` 乘除、一次 `swap`。**无 `Vec` / `String` / `format!` / `clone()`**（贴 `git diff` 的循环体）。
2. **全局分配计数用例**（B3 新增，`#[cfg(test)]`）：装一个 `#[global_allocator]` 的计数分配器（`std::alloc::System` 的包装，只在测试里生效），跑 **N 块**音频，断言 `alloc_count` 与 N **无关**（只随 `emit_latency` 的 500 ms 节流次数线性增长）。> ⚠️ 这条用例本身是**新的**（`pipeline/mod.rs` 里没有全局分配器），**放在 B3**，且**必须先跑一遍在"接上 `Chain` 之前"的 `Worker` 上取基线**——本稿的 `emit_latency` 已有 `RollingMetric::summary` 的 `collect()+sort`（`latency.rs:64`），所以基线本来就不是 0。要断言的是 **"接链之后相对基线，每块的分配增量 = 0"**，不是"总分配 = 0"。
3. **读取口不是半成品**（`RULES #4`）：`Runtime::op_timings` 的用例 `op_timings_reports_one_row_per_installed_stage` 要**真的跑一段音频**、拿到非空的 `Vec<OpTiming>`、并断言 `mono` 那一行 `blocks > 0`。

---

## 9. 风险与未决

> `[未核实]` = 本轮没核到 / 核不到。**不许在这些地方编。**

### 9.1 风险

| # | 风险 | 后果 | 处置 |
| --- | --- | --- | --- |
| **RK-1** | **差分台漏掉一处墙钟来源**，金样在某台机器上不重 | 收口时"逐字相等"变成"逐字偶尔相等" | §6.3 把两个 `Instant::now()` 来源点名排除了；B0 的验收要求**连录三遍 + 两种线程度各一遍**都干净。不稳的场景（例如 §6.4 之外的"队列溢出"）**不进金样**，改写成普通单测（现有的 `a_full_input_queue_drops_the_oldest_block` `:2349` 已是这种形态） |
| **RK-2** | **`Chain` 建链期 `?` 传播改变 `boot()` 的错误面** | 某个清单今天能装上、装链时报错 → `runtime.on_pipeline_failed` 多一条 | `Chain::build` 的四类错误（未知 `Op` 种类、`RateRef` 解不出、`Resample{from,to}` 与链当前率不符、多声道缺 `mono`）在 `Composition::validate` 之后已被现有清单穷举过；B1 的用例要**穷举 `Composition::of` 产出的清单形状**（例如 Speak 的 `translate` × `voice` × `denoise` × `gate.kind` × `monitor_translation` 共 32 组、Listen 的 `voice` × `monitor` 组合），断言全部能建链 |
| **RK-3** | **`BlockOut` 的乒乓在某节"读自己的输出"时写坏别名** | 静默的音频损坏 | 两块乒乓（`scratch.a` / `scratch.b`）+ 契约 C3 写死"先读后写"；B1 加 `debug_assert!(!std::ptr::eq(input.as_ptr(), out.current_ptr()))` |
| **RK-4** | **`Op::Mono` 从"恒执行"变成"看清单"**——这是本稿唯一的**语义**变化 | 对现有清单**零影响**（`speak.rs:43` / `listen.rs:78` 恒有 `Mono`），但它是"行为不变"里唯一一处**原则性**的松绑 | 在 §2.4 把"不装 mono"收敛成**建链期的一条校验**（多声道缺 `mono` 直接 `Err`），避免"链上跑着交织样本"这种无定义状态；B1 的用例 `mono_is_only_downmixed_when_the_manifest_says_so`（单声道 + 无 `mono` → 输出 == 输入）与 `a_multichannel_capture_without_mono_is_rejected` 把它钉成**有意的**行为；差分台 15 组全带 `mono`，所以金样照旧全绿 |
| **RK-5** | **`gate_drives_turns()` 的判据仍是 `session.hot_update`** | 与 §1.3 D 段说的"这不是它的本义"长期共存 | 已在 §2.5 的注释里点名"清单里没有这一格，用 `session.hot_update` 顶"，并把 `track_local_gate`（`:1141-1144`）那句"用 `plan.hot_update` 区分两者"的注释同步改掉。**S4-C 若加了"Listen 也用真门"，必须回来重审这一格** |
| **RK-6** | **B3 加的 `Vec<OpTiming>` 快照在 500 ms 节流点分配** | 严格读 RULES #6 的话"不许新增分配"可能被理解成"整个生命周期零分配" | §5.3 给了口径：RULES #6 的对象是**每拍/每帧**（原文"音频回调、DSP 每帧、渲染每帧"）。快照重建挂在**已有的** 500 ms 节流点上，与今天 `RollingMetric::summary` 的 `collect()+sort` 同频同点。§8.3 第 2 条给了**可执行的**零增量断言 |
| **RK-7** | **B0 的可见性放宽万一被当成"动了既有测试"** | 违反 S4-A 立下的"既有测试逐字未改"纪律 | §6.2 给了逐符号的表；验收要求 `git diff` 只出现 `struct X` → `pub(crate) struct X` 与 `fn f` → `pub(crate) fn f`，**任何 `assert` 都不许出现** |

### 9.2 有意接受的行为差异

**零。** 这是 S4-B 的定义。B2 之后 15 份金样一个字都不用改，就是"行为不变"的机器证据。

唯一需要口头说明的**非事件流**差异：采集率 ≠ 48 kHz 时那句 `tracing::warn!`（`boot:810`）与工厂失败那句 `tracing::warn!`（`boot:802`）的**文本**由差分台覆盖不到（见 §6.3 与 §9.3 的 O-4）。**执行器逐字保留这两句**（评审时 `git diff` 人工核）。

### 9.3 未决

| # | 事项 | 状态 |
| --- | --- | --- |
| **O-1** | **`Op::Aec` 的时序位置**（本稿给的是"降噪之后、重采样之前"这条约束） | `[未核实]` 真正的位置要等 S4-C 的软件 AEC 选型（SpeexDSP vs AEC3）。若选型的滤波器需要 16 kHz 宽带参考，位置可能要再往前挪。**本稿的接口设计不依赖这个答案**（`ChainStage` 是枚举，加一节改一处 `match`） |
| **O-2** | **同声卡是否保证同采样率** | `[未核实]` 概要稿 §2.2 的软件 AEC 前提是"麦克风与喇叭在同一声卡，共用时钟、时延稳定"。播放侧固定推 24 kHz（`OUTPUT_SAMPLE_RATE`，`cloud/protocol.rs:28`），采集侧由驱动协商。**AEC 的参考信号要升到采集率，这一步的时延估计能不能稳，本轮没核。** S4-C 的事 |
| **O-3** | **`NetIn` / `HostFeed` 输入进链之后，`Op::Resample{from: Session}` 的语义** | `[未定]` 端点清单（`Composition::endpoint`，`:529`）今天 `ops` 是空的、`Plan::from` 直接报错（`:141-144`）。网络进来的声音没有"采集率"，`RateRef::Capture` 无从解析。S3 落地时要决定：加第四个 `RateRef`，还是让端点清单显式写数字。**S4-B 不碰**（端点今天根本装不上） |
| **O-4** | **要不要给差分台加 `tracing` 抓取** | `[未决]` 加了就能把 §6.3 那条漏网的 `tracing::warn!` 也钉住；成本是引入一个测试用的 `tracing` subscriber（约 40 行），且 `tracing` 的输出格式本身可能带时间戳（要 `tracing_subscriber` 的 `with_test_writer` 或自建 `MakeWriter` 绕开）。**本稿的判断：不加**——那两行文本由 B2 的 `git diff` 人工核 + 一条覆盖两分支的单测兜住，收益不抵新增的脆弱面。**若 Main 认为要加，这是 B0 的一处追加，不影响其它工单** |
| **O-5** | **S4-C 的"廉价重采样档"要不要热切换** | `[未决]` INV-1 已经把"不许塞进 `Note`"写死了，但"整轮 `Stop`+`Start`"对用户是不是可接受（会有一次重连的延迟）没拍 |
| **O-6** | **`Chain` 的 `MAX_OPS` 取 6** | `[未定]` 今天 4 种（`OP_ORDER` 有 4 项），S4-C 加 `Aec` 是 5 种。取 6 是留一格。**若将来要支持"同一节装两次"**（如降噪前后各一次），`OP_ORDER` 的"子序列"校验（`composition.rs:450-460`，`index <= previous` 即报错）也**先得改**——那是 S4-C 的事 |

---

## 10. §6 第 2 问的正式答复（概要稿 `S4-EMBEDDED-REFACTOR.md` §6-2）

> **算子统一接口的形状：块大小、采样率变化、需要参考信号的算子如何接播放路径。**

1. **块大小：链上没有块大小。** 统一的是**调用约定**（单声道 `&[f32]` 进 → `0..n` 块出，块边界有意义、**不许合并**），不是块长。四节各有各的内部粒度（降噪 480 帧、重采样 480 输入帧、门按毫秒换算、采集 20 ms），链不重新切块、不插补零——**切了就改上传帧的切分，那是行为变化**。分派用**枚举**（`ChainStage`）不用 trait object：门的外部驱动口只有一个实现，trait object 得为此付出"假接口 / `Any` 下转 / 平行 trait object"三选一的代价；可插拔性早就被 `Deps` 的工厂吃掉了。
2. **采样率变化：建链期解析一次，运行时不算率。** `RateRef`（`Capture` / `Session` / `Playback`）在 `Chain::build` 里解析成具体 `u32`，链尾率记在 `Chain::out_rate`，`Worker` 拿它做两条断言。只有 `Op::Resample` 改率，其余三节进出口同率。**顺带堵死一个现存缺陷**：`Op::Resample{from,to}` 今天**一个字节都没被读过**（`Plan::from` 不看、`Worker::boot:829` 直接拿 `format.sample_rate` 和 `session.input_sample_rate()`），`Op::Gate{config}` 同理——这两处"清单说了、代码不听"是 S4-B 真正要收的账。
3. **需要参考信号的算子（`Op::Aec`）：接口留在播放路径的同一个 `match` 上，S4-B 只定位置不实现。** 口子是 `Chain::push_playback_reference(&[f32])`，与 `set_gate_active` / `set_gate_config` / `take_gate_status` 同层，**唯一调用点是 `Worker::handle_server_event` 的 `AudioDelta` 分支、`sink.push()` 之前**——外放的译音是我们自己播的，参考信号在进程内现成，不另开一路抓系统声音。链里没有 AEC 时该调用走空 `match`，零分配零拷贝。**口子和它的第一个实现在同一张工单里落地**（S4-C），本稿不留空钩子。

---

## 参考与关联

- 上位稿：`docs/plans/S4-EMBEDDED-REFACTOR.md` §3 S4-B、§4 验收、§6 第 2 问
- 方向与裁决：`docs/architecture/DIRECTIONS.md` §8 第 16 行、§10.9 第 3 条
- 清单由来：`docs/plans/S0-COMPOSITION-MANIFEST.md` §2.1（`Composition`/`Op`）、§2.7（`Plan` 的边界）
- 并行的那一张：`docs/plans/S4-A-HOST-LAYER.md` §4（五张文件表，S4-B 零交集）、§4.1（串行原因）
- 性能事实：`tools/bench-dsp/`（采集链 3.03 ms/s、RNNoise 2.5 ms/s，`AGENTS.md`）；本稿 §4.3 的分派开销估算**未跑基准**，标 `[未核实]`

---

## 11. Main 拍板（2026-09-30，审稿后追加；与上文冲突以本节为准）

- **M1 · O-4：差分台不抓 `tracing`。** 按 §6.3 的处置：B2 逐字保留那两句 `tracing`，并补一条覆盖 `DenoiseSkip::Rate` / `DenoiseSkip::Factory` 两分支的单测。
- **M2 · 排期：B0 ∥ B1。** B1 是纯新增（`chain.rs`），对 `pipeline/mod.rs` 只加 `mod chain;` 一行；B0 对 `pipeline/mod.rs` 只做可见性放宽 + `mod golden;` 一行。两者由 Main 合并。B2 必须等 B0 **和** B1 都合入 main 才开工。
- **M3 · 基线更新。** 本稿写作时 main 为 598/0/5；W1 与 ALSA-T1 合入后 main 为 641 passed / 0 failed / 6 ignored（S4-A 新旧实现并存期），`cargo test -p vox-core` 仍 258。S4-B 各工单以"`-p vox-core` 的测试名一个不少 + 金样逐字相等"为准，workspace 总数只作旁证。
- **M4 · O-1/O-2/O-3/O-5/O-6** 都在 S4-C 或更后，本轮不拍。
