//! 算子链执行器：**按 `Composition.ops` 的顺序**把一块采集音频跑过每一节。
//!
//! 装不装某一节、装在哪、装几节，全部由清单说了算——代码里不再有第二个答案。
//! 接线之前那份顺序写死在 `Worker::gated_blocks` + `Worker::feed` 里，现在它只在这里。
//!
//! 统一的是**调用约定**（`ChainStage::process`），不是一个 trait：
//!
//! * **C1** 单声道 `f32` 进、单声道 `f32` 出（混音在 `ChainScratch::begin` 里做一次）。
//! * **C2** 长度不守恒：输入任意长 `N`，出货 `0..M` 块，块长任意。
//! * **C3** 块边界有意义，**不许合并**——链尾的消费者逐块处理，合并会改上行帧的切分。
//! * **C4** 只有 `Resample` 改采样率，其余三节进出口同率。
//! * **C5** 只有 `Gate` 被外部驱动、只有 `Gate` 报状态。
//! * **C6** 只有 `Denoise` / `Resample` 有流状态，断线重来要清；**门不清**
//!   （`ActivationGate::reset` 会把门关掉，见 `Chain::on_disconnect`）。
//!
//! 零分配：热路径上只有两块乒乓缓冲的 `len` 归零与一次异或，各节的块从
//! `ports` / `gate` 的现成返回值**按值搬**进 [`BlockOut`]，不拷贝、不 `clone()`、
//! 不格式化。缓冲的容量建链时一次性给，跨块复用。
use std::mem;
use std::time::Instant;

use crate::cloud::protocol::OUTPUT_SAMPLE_RATE;
use crate::composition::{Composition, Op, RateRef};
use crate::gate::{ActivationGate, GateConfig, GateStatus};
use crate::ports::{AudioChunk, CaptureFormat, Denoise, PortError, PortResult, Resample};

use super::{Deps, DENOISE_RATE};

/// 链上最多几节。`Composition::OP_ORDER` 今天 4 项，S4-C 加 `Op::Aec` 是 5 项，
/// 留一格。**这是定长数组的长度**，不是上限策略：清单超过它直接建不出链
/// （`Chain::build` 里有那道检查），不做"动态扩容"——计量要在热路径上零分配。
pub const MAX_OPS: usize = 6;

/// 一节算子的累计计量。**全是定长字段**（一个 `&'static str` + 四个整数），
/// 没有 `Vec`、没有滚动窗口、没有浮点。
///
/// 计量在 `Chain::run` 的节边界上累加（每节两次时钟读 + 四个整数），重建快照
/// 只在 [`Chain::timings`] 那个**读取口**上做，且只在调用方要的时候。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpTiming {
    /// 清单里这一节叫什么。**建链时从 `Op::kind()` 抄下来**，不另写字符串表
    /// （多一处真源就会漂）。
    pub op: &'static str,
    /// 跑过多少拍。
    pub blocks: u64,
    /// 累计墙钟（纳秒）。
    pub total_ns: u64,
    /// 单拍最慢（纳秒）。嵌入式排障最看这个。
    pub max_ns: u64,
    /// 累计**出口**音频时长（纳秒）。实时倍率的分母。
    ///
    /// 照实记：降噪攒帧时这一拍出 0 块（加 0），门冲 preroll 时出 2 块（都算）。
    pub audio_ns: u64,
    /// 本节的出口率（契约 C4）。实时倍率的分母按它换算。
    pub out_rate: u32,
}

impl OpTiming {
    /// 还没建链/没跑过时的那一份。名字空着——没装过的节不占行。
    pub const fn empty() -> Self {
        Self {
            op: "",
            blocks: 0,
            total_ns: 0,
            max_ns: 0,
            audio_ns: 0,
            out_rate: 0,
        }
    }

    /// 实时倍率 = 处理墙钟 / 音频时长，**百分比**（0.3% 记作 `0.3`）。
    /// 零音频时长（还没出货、静音拍）时给 `0.0`，不炸也不编。
    pub fn realtime_percent(&self) -> f64 {
        if self.audio_ns == 0 {
            return 0.0;
        }
        (self.total_ns as f64) * 100.0 / (self.audio_ns as f64)
    }
}

/// 一节的输出：**若干块**。边界由这一节自己定（门冲 preroll 时一次出两块）。
///
/// 槽位跨块复用：`begin()` 只把 `len` 归零，**不清容量**。这一版的槽位是从各节
/// 现成的返回值**搬**进来的（`push_owned`），所以进这一层零分配、零拷贝。
#[derive(Default)]
pub(crate) struct BlockOut {
    /// 复用槽。`slots.len()` 是见过的最大块数，`len <= slots.len()`。
    slots: Vec<Vec<f32>>,
    /// 本拍真正装了几块。
    len: usize,
}

impl BlockOut {
    /// 建一批空槽（`ChainScratch` 的一次性分配，热路径只写 `len`）。
    fn with_slots(slots: usize) -> Self {
        Self {
            slots: vec![Vec::new(); slots],
            len: 0,
        }
    }

    /// 归零长度，准备接这一拍的新块。**不清容量。**
    fn begin(&mut self) {
        self.len = 0;
    }

    /// 直接接管一节现成返回的块（**不拷贝**）。链上的每一条路径都走这条或 [`Self::take_all_from`]。
    fn push_owned(&mut self, block: Vec<f32>) {
        if self.len < self.slots.len() {
            self.slots[self.len] = block;
        } else {
            self.slots.push(block);
        }
        self.len += 1;
    }

    /// 把 `src` 本拍的块**按值**搬过来：直通型节（`Mono`、降级掉的 `Denoise`）用这条，
    /// 于是"这一节什么都没做"就真的是**什么都没做**——不多一次拷贝、不多一次分配。
    ///
    /// 搬完 `src` 变空（契约 C3：块只能往下走，不能在链上被复制出第二份）。
    fn take_all_from(&mut self, src: &mut Self) {
        for at in 0..src.len {
            self.push_owned(mem::take(&mut src.slots[at]));
        }
        src.len = 0;
    }

    /// 本拍的块，按时间先后。
    pub(crate) fn blocks(&self) -> impl Iterator<Item = &[f32]> {
        self.slots[..self.len].iter().map(|block| block.as_slice())
    }

    /// 本拍一共出了多少样本（各块相加）。**只被计量用**，别的读法是数块数。
    /// 循环，不分配。
    pub(crate) fn samples(&self) -> usize {
        self.slots[..self.len]
            .iter()
            .fold(0usize, |sum, block| sum + block.len())
    }

    /// 块数。**只被 `mod tests` 读**：生产路径一律走 [`Self::blocks`]，块数就是
    /// 迭代次数，没有单独问一句的地方。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// 见 [`Self::len`]。
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// 降噪没装上的两种原因。**各自对应 `Worker::boot` 的一个分支**，报什么、报不报通知
/// 都不一样，别合并。
#[derive(Debug, Clone)]
pub(crate) enum DenoiseSkip {
    /// 采集率不是 48 kHz（`DENOISE_RATE`）。今天：**只有** `tracing::warn!`，
    /// 不发 `Notice`。
    Rate(u32),
    /// 降噪器工厂起不来。今天：`tracing::warn!` **加**
    /// `Notice::warning("降噪启动失败，本次已关闭").on(pipeline)`。
    ///
    /// 带原始错误消息：那句 `tracing::warn!` 里 `error = %err` 的内容要逐字保留
    /// （S4-B 稿 §9.2），丢了消息就保不住。
    Factory(PortError),
}

/// 算子链的一节。**四种，全列出来**；`Composition::ops` 决定装哪几种、什么顺序。
pub(crate) enum ChainStage {
    /// `Op::Mono`。交织 → 单声道。**混音在 [`ChainScratch::begin`] 里做**（今天的
    /// `chunk.to_mono()` 就在那儿），这一节在链上只负责"清单说有这一节"这件事：
    /// 顺序、名字、出口率、以及计量那一行。
    Mono { rate: u32 },
    /// `Op::Denoise`。`port = None` = 装了但这一拍直通，原因见 `skip`。
    Denoise {
        port: Option<Box<dyn Denoise>>,
        rate: u32,
        /// `None` = 降噪器真的装上了；`Some(why)` = 装这一节但不降噪，原因是它。
        skip: Option<DenoiseSkip>,
    },
    /// `Op::Gate`。带一份"本拍的状态"槽位，`Chain::take_gate_status` 从这里取。
    Gate {
        gate: ActivationGate,
        /// 门建在**采集率**上（`tail_ms` / `preroll_ms` 按率换算样本数）。
        rate: u32,
        /// 本拍的状态，`take` 语义 = 取走即清。
        status: Option<GateStatus>,
    },
    /// `Op::Resample`。
    Resample {
        port: Box<dyn Resample>,
        out_rate: u32,
    },
}

impl ChainStage {
    /// **统一调用约定**（契约 C1–C3）。四个变体形状完全一样，差别只在内部那一句 `match`。
    ///
    /// 直通型的节用 `out.take_all_from(input)`：块按值往下走，**不拷贝、不分配**。
    fn process(&mut self, input: &mut BlockOut, out: &mut BlockOut) {
        // 乒乓两块必须不同，否则"读自己的输出"会写坏别名。
        debug_assert!(
            !std::ptr::eq(input as *const BlockOut, out as *const BlockOut),
            "乒乓的两块必须是两块"
        );
        match self {
            Self::Mono { .. } => out.take_all_from(input),
            Self::Denoise { port, .. } => match port {
                Some(port) => {
                    for block in input.blocks() {
                        out.push_owned(port.process(block));
                    }
                }
                None => out.take_all_from(input),
            },
            Self::Gate { gate, status, .. } => {
                for block in input.blocks() {
                    let (accepted, this) = gate.process(block);
                    for block in accepted {
                        out.push_owned(block);
                    }
                    *status = Some(this);
                }
            }
            Self::Resample { port, .. } => {
                for block in input.blocks() {
                    out.push_owned(port.process(block));
                }
            }
        }
    }

    /// 本节的**出口**采样率（契约 C4）。只有 `Resample` 与入口不同。
    pub(crate) fn out_rate(&self) -> u32 {
        match self {
            Self::Mono { rate } | Self::Denoise { rate, .. } | Self::Gate { rate, .. } => *rate,
            Self::Resample { out_rate, .. } => *out_rate,
        }
    }
}

/// 乒乓缓冲。**两块**，跨块复用，建链/建 Worker 时一次性分配。
///
/// `ping[cur]` 是本拍的**输入**，`ping[cur ^ 1]` 是本节的**输出**。一次异或就够，
/// 不需要 `Vec` 队列。
pub(crate) struct ChainScratch {
    ping: [BlockOut; 2],
    cur: usize,
}

impl Default for ChainScratch {
    fn default() -> Self {
        Self {
            ping: [BlockOut::with_slots(4), BlockOut::with_slots(4)],
            cur: 0,
        }
    }
}

impl ChainScratch {
    /// 装入口块。`downmix = true` 时混音（`AudioChunk::to_mono`），否则原样搬。
    ///
    /// ⚠️ 两条路的**分配次数相同**（各一次 `Vec`），`begin` 不省也不多花——省掉
    /// `to_mono` 那个 `Vec` 是 S4-C 零分配化那张工单（`ports` 的 `process_into` 化）的活。
    pub(crate) fn begin(&mut self, chunk: &AudioChunk, downmix: bool) {
        let block = if downmix {
            chunk.to_mono()
        } else {
            chunk.samples.clone()
        };
        let current = &mut self.ping[self.cur];
        current.begin();
        current.push_owned(block);
    }

    /// 把这一拍用完的 `BlockOut` **按值**放回乒乓槽（**容量复用，不分配**）。
    ///
    /// 有了它，调用方身上就不挂着任何借用，`&mut self` 照常可用。
    pub(crate) fn recycle(&mut self, out: BlockOut) {
        self.ping[self.cur] = out;
    }

    /// 乒乓的两半：`(本拍的输入, 本节的输出)`。恒不相同——一节的输入永远不是
    /// 它的输出，将来"就地改"的节也写不了别名。
    fn halves(&mut self) -> (&mut BlockOut, &mut BlockOut) {
        let Self { ping, cur } = self;
        // 恒从中间切开：前半是 0 号槽、后半是 1 号槽，谁进谁出看 `cur`。
        let (first, second) = ping.split_at_mut(1);
        if *cur == 0 {
            (&mut first[0], &mut second[0])
        } else {
            (&mut second[0], &mut first[0])
        }
    }

    /// 把本拍的 `BlockOut` 按值换出来（`mem::replace` 成空壳），随后由调用方 `recycle` 放回。
    fn take_current(&mut self) -> BlockOut {
        mem::take(&mut self.ping[self.cur])
    }
}

/// 一条按清单建好的算子链。**装配期**在 [`Chain::build`]，运行期只有 [`Chain::run`]。
pub(crate) struct Chain {
    stages: Vec<ChainStage>,
    /// 逐节的累计计量，**行号 = 装配序号**。`build` 时一次性建好（`[OpTiming; MAX_OPS]`，
    /// 建链期分配，运行期只写整数），只被工作线程碰，不加锁——与
    /// `latency.rs::LatencyTracker` 同一条规矩。
    timings: [OpTiming; MAX_OPS],
}

impl Chain {
    /// 空链（`Worker::teardown` 丢链时用）。
    pub(crate) fn empty() -> Self {
        Self {
            stages: Vec::new(),
            timings: [OpTiming::empty(); MAX_OPS],
        }
    }

    /// 按 `Composition::ops` 建链。**装配期**，不是热路径。
    ///
    /// 四样输入今天分别在 `Worker::boot` 的四段里取：清单的 `ops` 逐条、`Deps`、
    /// 采集协商到的 `CaptureFormat`、会话上行率。采样率**建链期解析一次**，
    /// 运行期各节只知道自己那一个率。
    pub(crate) fn build(
        ops: &[Op],
        deps: &Deps,
        capture: CaptureFormat,
        session_rate: u32,
    ) -> PortResult<Self> {
        let mut stages: Vec<ChainStage> = Vec::with_capacity(ops.len());
        // 计量行与节一一对应，**行名取自 `Op::kind()`**（建链时抄下来），不另写
        // 一份字符串表——多一处真源就会漂。
        let mut timings = [OpTiming::empty(); MAX_OPS];
        // 链上当前这一块的率，只有 `Resample` 会改它（契约 C4）。
        let mut rate = capture.sample_rate;
        let mut has_mono = false;

        for (at, op) in ops.iter().enumerate() {
            // 防御：`Composition::validate` 已经拦过一道，这里是第二道。
            // `Op` 是封闭枚举 + `kind()` 穷举，所以今天走不到；留着是为了清单
            // 将来加一种节时，这条不会变成一句"永远为真"的空判断。
            let kind = op.kind();
            if !Composition::OP_ORDER.contains(&kind) {
                return Err(PortError::new(format!(
                    "清单第 {} 条算子 `{kind}` 不在规定顺序 {:?} 里。",
                    at + 1,
                    Composition::OP_ORDER
                )));
            }
            // 计量是定长数组（`MAX_OPS` 格）。清单装不下就**建不出链**，不做
            // "运行期扩容"——那条路必然在热路径上分配。
            if at >= MAX_OPS {
                return Err(PortError::new(format!(
                    "清单有 {} 条算子，链上限是 {MAX_OPS} 条。",
                    ops.len()
                )));
            }
            match op {
                // 混音不在这儿做（见 `ChainScratch::begin`），这一节只占一个位置。
                Op::Mono => {
                    has_mono = true;
                    stages.push(ChainStage::Mono { rate });
                }
                Op::Denoise => {
                    // 降噪只认 48 kHz。降级有两种原因，报什么不一样，别合并。
                    let (port, skip) = if rate != DENOISE_RATE {
                        (None, Some(DenoiseSkip::Rate(rate)))
                    } else {
                        match (deps.denoise)() {
                            Ok(port) => (Some(port), None),
                            // 降噪造不出来只是少一层处理，别把整条流水线拖死。
                            Err(err) => (None, Some(DenoiseSkip::Factory(err))),
                        }
                    };
                    stages.push(ChainStage::Denoise { port, rate, skip });
                }
                Op::Gate { config } => {
                    // 门建在**采集率**上，不是会话率——尾巴和 preroll 的时长按率算。
                    stages.push(ChainStage::Gate {
                        gate: ActivationGate::new(*config, capture.sample_rate),
                        rate: capture.sample_rate,
                        status: None,
                    });
                }
                Op::Resample { from, to } => {
                    let from_rate = Self::resolve(*from, capture, session_rate);
                    let to_rate = Self::resolve(*to, capture, session_rate);
                    // 清单说的入口率必须就是链上这一块真实的率，否则"清单说一套、
                    // 链上做另一套"，重采样会换错率。
                    if from_rate != rate {
                        return Err(PortError::new(format!(
                            "清单里的 resample 入口是 {from_rate}，但链上这一步的音频是 {rate}\
                             （它要换到 {to_rate}）。清单与实际采集/会话格式对不上。"
                        )));
                    }
                    stages.push(ChainStage::Resample {
                        port: (deps.resample)(rate, to_rate),
                        out_rate: to_rate,
                    });
                    rate = to_rate;
                }
            }
            // 计量行与节同步长出来：名字取自本条 `Op` 的 `kind()`，出口率取自刚
            // 建好的这一节（`Resample` 换过率，所以必须问节，不能问 `rate`）。
            timings[at] = OpTiming {
                op: kind,
                out_rate: stages[at].out_rate(),
                ..OpTiming::empty()
            };
        }

        // 链上恒为单声道（契约 C1）。多声道采集却不装 `mono`，"不混音"是无定义的
        // ——下游的降噪 / 门会拿到交织样本。建链期就挡下来。
        if capture.channels > 1 && !has_mono {
            return Err(PortError::new(format!(
                "采集是 {} 声道，清单里必须装 mono。",
                capture.channels
            )));
        }

        Ok(Self { stages, timings })
    }

    /// `RateRef` → 具体率。**唯一**的解析表，运行时不再算率。
    fn resolve(rate_ref: RateRef, capture: CaptureFormat, session_rate: u32) -> u32 {
        match rate_ref {
            RateRef::Capture => capture.sample_rate,
            RateRef::Session => session_rate,
            RateRef::Playback => OUTPUT_SAMPLE_RATE,
        }
    }

    /// 按装配顺序把入口块跑过每一节，**按值**交回结果与本拍的门状态。
    ///
    /// **两块乒乓**：一节的输入永远不是它的输出，将来"就地改"的节也写不了别名。
    /// 返回 `BlockOut` 而不是借用，是为了调用方身上不挂着 `&mut self` 的借用。
    ///
    /// 循环体就是**一节的边界**，所以计量也在这里（每节两次时钟读）：
    ///
    /// ```text
    /// t0 = Instant::now();  stage.process(..);  dt = t0.elapsed();
    /// blocks += 1;  total_ns += dt;  max_ns = max(max_ns, dt);
    /// audio_ns += 出口样本数 * 1e9 / 出口率;   // 整数乘除，无浮点
    /// ```
    ///
    /// **这里没有 `Vec` / `String` / `format!` / `clone()`**：`timings` 是建链时
    /// 建好的定长数组，热路径只写它的四个整数字段。`mod tests` 里的
    /// `timings_are_monotonic_counters_with_no_allocation` 用一个计数分配器把这条
    /// 钉住（"纯直通链"每块的分配增量必须是 0）。
    pub(crate) fn run(&mut self, scratch: &mut ChainScratch) -> (BlockOut, Option<GateStatus>) {
        let Self { stages, timings } = self;
        for (at, stage) in stages.iter_mut().enumerate() {
            let (input, out) = scratch.halves();
            out.begin();
            let started = Instant::now();
            stage.process(input, out);
            let elapsed = started.elapsed().as_nanos() as u64;
            // 出口音频时长：整数乘除。降噪攒帧时 `samples()` 是 0（照实记 0），
            // 门冲 preroll 时是两块之和（照实记两块）——分母错了倍率就错了。
            let samples = out.samples() as u64;
            let slot = &mut timings[at];
            slot.blocks += 1;
            slot.total_ns += elapsed;
            slot.max_ns = slot.max_ns.max(elapsed);
            slot.audio_ns += samples * 1_000_000_000 / (slot.out_rate.max(1) as u64);
            scratch.cur ^= 1;
        }
        (scratch.take_current(), self.take_gate_status())
    }

    /// 计量快照：装配顺序上一行一节。**纯读取**，调用方要才建（`Vec` 在调用方
    /// 那个节流点上，不在热路径上）。
    pub(crate) fn timings(&self) -> Vec<OpTiming> {
        self.timings[..self.stages.len()].to_vec()
    }

    /// 链上各节的清单名字，按装配顺序。见 [`OpTiming::op`]：**只被 `mod tests` 读**
    /// （读取口那一行自带名字）。
    #[cfg(test)]
    pub(crate) fn stage_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.timings[..self.stages.len()].iter().map(|row| row.op)
    }

    /// 冲重采样缓冲里的零头（一段语音收尾时用）。链里没有 `Resample` 就给空。
    pub(crate) fn flush_resample(&mut self) -> Vec<f32> {
        for stage in &mut self.stages {
            if let ChainStage::Resample { port, .. } = stage {
                return port.flush();
            }
        }
        Vec::new()
    }

    /// 断线重来。**逐节按契约 C6 决定清不清**：
    ///
    /// | 节 | 动作 | 为什么 |
    /// | --- | --- | --- |
    /// | `Denoise` | `port.reset()` | 半句作废：480 帧的分帧缓冲与 RNNoise 内部状态都丢掉 |
    /// | `Resample` | `port.reset()` | 缓冲里的零头别带到下一条连接 |
    /// | `Mono` | 无状态可清 | — |
    /// | `Gate` | **不动** | `ActivationGate::reset` 会把 `external_active` 清成 `false`——门会被**关掉**。接线前 `Worker::reconnect` 也不碰门，行为是"重连后按键仍按着"，所以这里不许调 |
    pub(crate) fn on_disconnect(&mut self) {
        for stage in &mut self.stages {
            match stage {
                ChainStage::Denoise {
                    port: Some(port), ..
                } => port.reset(),
                ChainStage::Resample { port, .. } => port.reset(),
                _ => {}
            }
        }
    }

    /// 链里装没装 `Op::Mono`。**没装就不混音**。
    pub(crate) fn has_mono(&self) -> bool {
        self.stages
            .iter()
            .any(|stage| matches!(stage, ChainStage::Mono { .. }))
    }

    /// 链尾率 = **最后一节的出口率**（建链时解析，运行期不再算率）。
    /// `Worker::boot` 拿它做两条断言：有会话时等于 `session.input_sample_rate()`，
    /// 直通时（链里没有 `Resample`）等于 `CaptureFormat::sample_rate`。
    pub(crate) fn out_rate(&self) -> u32 {
        self.stages.last().map_or(0, ChainStage::out_rate)
    }

    /// 降噪降级的原因。`None` = 链里没有降噪节**或**降噪节真的装上了。
    pub(crate) fn denoise_skip(&self) -> Option<&DenoiseSkip> {
        self.stages.iter().find_map(|stage| match stage {
            ChainStage::Denoise { skip, .. } => skip.as_ref(),
            _ => None,
        })
    }

    /// 控制面推门。**链里没有门就是静默忽略**（今天 `Worker::handle_note` 也是
    /// `if let Some(gate)`）。
    pub(crate) fn set_gate_active(&mut self, active: bool) {
        for stage in &mut self.stages {
            if let ChainStage::Gate { gate, .. } = stage {
                gate.set_external_active(active);
            }
        }
    }

    /// 换门参数。转 `ActivationGate::set_config`，**保留 `external_active`**。
    pub(crate) fn set_gate_config(&mut self, config: GateConfig) {
        for stage in &mut self.stages {
            if let ChainStage::Gate { gate, .. } = stage {
                gate.set_config(config);
            }
        }
    }

    /// 取本拍的门状态。`take` 语义 = **取走即清**，链里没有门给 `None`。
    pub(crate) fn take_gate_status(&mut self) -> Option<GateStatus> {
        for stage in &mut self.stages {
            if let ChainStage::Gate { status, .. } = stage {
                return status.take();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::capability::HostFacts;
    use crate::cloud::Transport;
    use crate::composition::HostKind;
    use crate::event::Pipeline;
    use crate::ports::{CaptureSource, PlaybackSink};
    use crate::runtime::SessionConfig;
    use crate::settings::{ListenTarget, ModelProvider};

    const CAPTURE_RATE: u32 = 48_000;
    const SESSION_RATE: u32 = 16_000;

    // --- 分配计数（只在本测试二进制里生效）---------------------------------
    //
    // S4-B 稿 §8.3 第 2 条：计量不许破坏"热路径零新增分配"。数分配次数只有一条
    // 路——全局分配器。`const` 初始化 = 没有析构函数，于是分配器里读它永远安全
    // （`try_with` 再兜住线程收尾那一刻）；**按线程计数**，于是同一二进制里并行跑的
    // 别的用例不会污染本用例的读数。

    thread_local! {
        static ALLOCS: Cell<u64> = const { Cell::new(0) };
    }

    struct CountingAlloc;

    fn count_alloc() {
        let _ = ALLOCS.try_with(|counter| counter.set(counter.get() + 1));
    }

    fn alloc_count() -> u64 {
        ALLOCS.try_with(Cell::get).unwrap_or(0)
    }

    // SAFETY：`CountingAlloc` 是 `System` 的透明包装，多出来的只有一个整数加法。
    // `realloc` 也算一次分配（它可能真去要新内存），`dealloc` 不算。
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_alloc();
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_alloc();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count_alloc();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOC_COUNTER: CountingAlloc = CountingAlloc;

    // --- 假件 --------------------------------------------------------------

    #[derive(Default)]
    struct Dsp {
        denoise_calls: AtomicU32,
        denoise_resets: AtomicU32,
        resample_resets: AtomicU32,
        resample_flushes: AtomicU32,
        /// 重采样工厂被叫到过的 (进率, 出率)——验 `Op::Resample` 的两个字段真被读了。
        resample_rates: Mutex<Vec<(u32, u32)>>,
    }

    struct DenoiseHandle(Arc<Dsp>);

    impl Denoise for DenoiseHandle {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            self.0.denoise_calls.fetch_add(1, Ordering::SeqCst);
            samples.to_vec()
        }
        fn reset(&mut self) {
            self.0.denoise_resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 攒满 480 才出货的假降噪——真的 `Denoiser` 就是这个脾气（满一帧才给货），
    /// 所以它会让某一拍链出 0 块。计量的分母必须照实记 0，不许拿"输入了多少"
    /// 冒充"出口了多少"。
    struct Framer {
        held: Vec<f32>,
    }

    impl Denoise for Framer {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            self.held.extend_from_slice(samples);
            if self.held.len() < FRAME {
                return Vec::new();
            }
            let whole = self.held.len() / FRAME * FRAME;
            self.held.drain(..whole).collect()
        }
        fn reset(&mut self) {
            self.held.clear();
        }
    }

    const FRAME: usize = 480;

    /// 按整数比抽点，长度确实会变，好验调用方没假设长度守恒。
    struct Decimate {
        step: usize,
        dsp: Arc<Dsp>,
    }

    impl Resample for Decimate {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            samples.iter().step_by(self.step).copied().collect()
        }
        fn flush(&mut self) -> Vec<f32> {
            self.dsp.resample_flushes.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        }
        fn reset(&mut self) {
            self.dsp.resample_resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 算子链只用到降噪与重采样两个工厂；采集 / 传输 / 播放都不该被碰到，
    /// 碰到了就是链越界（直接炸，不静默通过）。
    fn deps(dsp: &Arc<Dsp>, fail_denoise: bool) -> Deps {
        let denoise_dsp = Arc::clone(dsp);
        let resample_dsp = Arc::clone(dsp);
        Deps {
            transport: Box::new(|| -> Box<dyn Transport> { unreachable!("链不碰传输层") }),
            capture: Box::new(|| -> Box<dyn CaptureSource> { unreachable!("链不碰采集层") }),
            playback: Box::new(|| -> Box<dyn PlaybackSink> { unreachable!("链不碰播放层") }),
            denoise: Box::new(move || {
                if fail_denoise {
                    return Err(PortError::new("降噪模型加载失败"));
                }
                Ok(Box::new(DenoiseHandle(Arc::clone(&denoise_dsp))) as Box<dyn Denoise>)
            }),
            resample: Box::new(move |in_rate, out_rate| {
                resample_dsp
                    .resample_rates
                    .lock()
                    .unwrap()
                    .push((in_rate, out_rate));
                Box::new(Decimate {
                    step: (in_rate / out_rate).max(1) as usize,
                    dsp: Arc::clone(&resample_dsp),
                }) as Box<dyn Resample>
            }),
        }
    }

    /// 建链失败时把错误取出来（`Chain` 不是 `Debug`，`unwrap_err` 用不了）。
    fn build_err(result: PortResult<Chain>) -> PortError {
        match result {
            Ok(_) => panic!("这份清单本该建不出链"),
            Err(err) => err,
        }
    }

    fn mono_capture() -> CaptureFormat {
        CaptureFormat {
            sample_rate: CAPTURE_RATE,
            channels: 1,
        }
    }

    fn chunk(rate: u32, samples: Vec<f32>) -> AudioChunk {
        AudioChunk {
            samples,
            sample_rate: rate,
            channels: 1,
        }
    }

    fn loud(n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.5 + i as f32 * 1e-6).collect()
    }

    // --- 清单构造（验现网清单形状全都建得出链） ---------------------------

    fn base_config(pipeline: Pipeline) -> SessionConfig {
        SessionConfig {
            session_id: 1,
            pipeline,
            provider: ModelProvider::Aliyun,
            model_name: "paraformer-realtime-v2".to_string(),
            api_key: "sk-test".to_string(),
            target_language: "ja".to_string(),
            voice: Some("Tina".to_string()),
            voice_clone_frequency: None,
            gate: GateConfig::MANUAL,
            gate_active: false,
            input_device: None,
            output_device: Some("CABLE Input".to_string()),
            translate: true,
            monitor_translation: false,
            loopback_target: None,
            denoise: true,
            source_language: None,
        }
    }

    fn speak_manifest(
        translate: bool,
        voice: bool,
        denoise: bool,
        gate: GateConfig,
        monitor: bool,
    ) -> Composition {
        let mut config = base_config(Pipeline::Speak);
        config.translate = translate;
        config.voice = voice.then(|| "Tina".to_string());
        config.denoise = denoise;
        config.gate = gate;
        config.monitor_translation = monitor;
        Composition::of(&config, &HostFacts::all_wired(HostKind::Windows)).unwrap()
    }

    fn listen_manifest(voice: bool, monitor: bool) -> Composition {
        let mut config = base_config(Pipeline::Listen);
        config.target_language = "zh".to_string();
        config.voice = voice.then(|| "Tina".to_string());
        config.gate = GateConfig::level(0.0);
        config.gate_active = true;
        config.loopback_target = Some(ListenTarget {
            executable: "Discord.exe".to_string(),
            display_name: "Discord".to_string(),
            include_process_tree: true,
        });
        config.monitor_translation = monitor;
        Composition::of(&config, &HostFacts::all_wired(HostKind::Windows)).unwrap()
    }

    /// 把一段输入跑完链，返回链尾的块。
    fn drive(
        chain: &mut Chain,
        scratch: &mut ChainScratch,
        chunk: &AudioChunk,
    ) -> (Vec<Vec<f32>>, Option<GateStatus>) {
        scratch.begin(chunk, chain.has_mono());
        let (out, status) = chain.run(scratch);
        let blocks = out.blocks().map(|block| block.to_vec()).collect();
        scratch.recycle(out);
        (blocks, status)
    }

    // --- 用例 --------------------------------------------------------------

    /// 清单里逐条有什么，链上就逐条装什么，顺序照抄。现网能产出的每一份清单
    /// （Speak 的 translate × voice × denoise × gate × monitor、Listen 的 voice ×
    /// monitor）都必须建得出链——`Chain::build` 的校验不许把现网形状挡在外面。
    #[test]
    fn every_op_in_the_manifest_becomes_a_stage_in_order() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);

        let gates = [
            GateConfig::MANUAL,
            GateConfig::level(0.0),
            GateConfig::level(0.02),
        ];
        let mut manifests: Vec<Composition> = Vec::new();
        for translate in [true, false] {
            for voice in [true, false] {
                for denoise in [true, false] {
                    for gate in gates {
                        for monitor in [true, false] {
                            manifests
                                .push(speak_manifest(translate, voice, denoise, gate, monitor));
                        }
                    }
                }
            }
        }
        for voice in [true, false] {
            for monitor in [true, false] {
                manifests.push(listen_manifest(voice, monitor));
            }
        }
        assert_eq!(manifests.len(), 2 * 2 * 2 * 3 * 2 + 4);

        for manifest in &manifests {
            let chain = Chain::build(&manifest.ops, &deps, mono_capture(), SESSION_RATE)
                .unwrap_or_else(|err| panic!("现网清单建不出链：{err}"));
            let want: Vec<&'static str> = manifest.ops.iter().map(Op::kind).collect();
            let got: Vec<&'static str> = chain.stage_names().collect();
            assert_eq!(got, want, "节的顺序必须逐条等于清单的 ops 顺序");
        }
    }

    /// 清单里没有的条目就是**不装**，代码里不再有第二个"装不装"的答案。
    #[test]
    fn an_absent_op_is_simply_not_installed() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);

        let with = Chain::build(
            &speak_manifest(true, true, true, GateConfig::MANUAL, false).ops,
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        let without = Chain::build(
            &speak_manifest(true, true, false, GateConfig::MANUAL, false).ops,
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();

        assert!(with.stage_names().any(|name| name == "denoise"));
        assert!(
            !without.stage_names().any(|name| name == "denoise"),
            "清单里没有降噪就不该有这一节：{:?}",
            without.stage_names().collect::<Vec<_>>()
        );
        // 两份清单只差降噪这一格，其余节一模一样。
        assert_eq!(
            with.stage_names().filter(|name| *name != "denoise").count(),
            without.stage_names().count()
        );
        assert!(with.denoise_skip().is_none());

        // 一条一节都没有的清单 = 一条空链：没有节、没有 mono、没有链尾率、冲不出尾巴。
        let mut bare = Chain::build(&[], &deps, mono_capture(), SESSION_RATE).unwrap();
        assert_eq!(bare.stage_names().count(), 0);
        assert!(!bare.has_mono());
        assert_eq!(bare.out_rate(), 0);
        assert!(bare.flush_resample().is_empty());
        let empty = Chain::empty();
        assert_eq!(empty.out_rate(), 0);
        assert!(!empty.has_mono());
        assert_eq!(empty.stage_names().count(), 0);
    }

    /// 装了 `Mono` 才混音；不装就原样搬（此时采集必是单声道，`Chain::build` 校验过）。
    #[test]
    fn mono_is_only_downmixed_when_the_manifest_says_so() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let open_gate = GateConfig::level(0.0);

        // 装 mono：双声道进，单声道出（长度减半）。
        let mut chain = Chain::build(
            &[Op::Mono, Op::Gate { config: open_gate }],
            &deps,
            CaptureFormat {
                sample_rate: CAPTURE_RATE,
                channels: 2,
            },
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();
        let stereo = AudioChunk {
            samples: vec![1.0, 3.0, 2.0, 6.0],
            sample_rate: CAPTURE_RATE,
            channels: 2,
        };
        let (blocks, _) = drive(&mut chain, &mut scratch, &stereo);
        assert_eq!(blocks, vec![vec![2.0, 4.0]]);

        // 不装 mono：单声道采集原样过去，一个样本不多一个样本不少。
        let mut chain = Chain::build(
            &[Op::Gate { config: open_gate }],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        assert!(!chain.has_mono());
        let mut scratch = ChainScratch::default();
        let input = loud(160);
        let (blocks, _) = drive(
            &mut chain,
            &mut scratch,
            &chunk(CAPTURE_RATE, input.clone()),
        );
        assert_eq!(blocks, vec![input]);
    }

    /// 多声道采集却不装 `mono`，链上"不混音"是无定义的：建链期就挡下来。
    #[test]
    fn a_multichannel_capture_without_mono_is_rejected() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let err = build_err(Chain::build(
            &[Op::Gate {
                config: GateConfig::MANUAL,
            }],
            &deps,
            CaptureFormat {
                sample_rate: CAPTURE_RATE,
                channels: 2,
            },
            SESSION_RATE,
        ));
        assert!(
            err.message.contains("mono"),
            "错误要指名道姓说缺的是 mono：{}",
            err.message
        );
    }

    /// 断线重来**不许**关门（`ActivationGate::reset` 会把 `external_active` 清掉）。
    /// 这条钉的是"重连后按键仍按着"。
    #[test]
    fn the_gate_stays_open_across_a_disconnect() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let mut chain = Chain::build(
            &[
                Op::Mono,
                Op::Denoise,
                Op::Gate {
                    config: GateConfig::MANUAL,
                },
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();
        chain.set_gate_active(true);

        let (blocks, status) = drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(960)));
        assert_eq!(blocks.len(), 1, "门开着就放行当前块");
        assert!(status.unwrap().active);

        chain.on_disconnect();

        // 若门被 reset 了，这里会掉进 preroll、一块都不出。
        let (blocks, status) = drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(960)));
        assert_eq!(blocks.len(), 1, "断线后门必须仍然开着");
        assert!(
            status.unwrap().active,
            "断线后门必须仍然开着（状态也要照报）"
        );

        // 换门参数也不许把按键状态弄丢（`ActivationGate::set_config` 的钉子用例）。
        chain.set_gate_config(GateConfig::level(0.0));
        let (blocks, status) = drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(960)));
        assert_eq!(blocks.len(), 1, "换门参数后门仍应是开着的");
        assert!(status.unwrap().active);
    }

    /// 契约 C6：只有带流状态的节被清；`Mono` 没状态、门不许动。
    #[test]
    fn the_chain_only_resets_the_stages_with_stream_state() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let mut chain = Chain::build(
            &[
                Op::Mono,
                Op::Denoise,
                Op::Gate {
                    config: GateConfig::MANUAL,
                },
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();
        chain.set_gate_active(true);
        drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(960)));

        chain.on_disconnect();

        assert_eq!(dsp.denoise_resets.load(Ordering::SeqCst), 1);
        assert_eq!(dsp.resample_resets.load(Ordering::SeqCst), 1);
    }

    /// `Op::Resample { from, to }` 的两个字段必须被真正读取：入口率对不上就拒建，
    /// 对得上时工厂拿到的就是清单写的那个率。
    #[test]
    fn a_manifest_whose_resample_rate_refs_disagree_is_rejected() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);

        // 清单说"从会话率（16k）换到会话率"，但链上这一步的音频是采集率（48k）。
        let err = build_err(Chain::build(
            &[
                Op::Mono,
                Op::Resample {
                    from: RateRef::Session,
                    to: RateRef::Session,
                },
            ],
            &deps,
            mono_capture(),
            SESSION_RATE,
        ));
        assert!(
            err.message.contains("16000"),
            "错误要报出清单说的率：{}",
            err.message
        );
        assert!(
            err.message.contains("48000"),
            "错误要报出链上真实的率：{}",
            err.message
        );
        assert!(
            dsp.resample_rates.lock().unwrap().is_empty(),
            "校验没过就不许建重采样器"
        );

        // 正确的写法：入口是采集率，出口是会话率。工厂拿到的就是这两个数。
        let chain = Chain::build(
            &[
                Op::Mono,
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        assert_eq!(chain.out_rate(), SESSION_RATE);
        assert_eq!(
            *dsp.resample_rates.lock().unwrap(),
            vec![(CAPTURE_RATE, SESSION_RATE)]
        );

        // 冲尾巴必须冲的就是这一个端口。
        let mut chain = chain;
        assert!(chain.flush_resample().is_empty());
        assert_eq!(dsp.resample_flushes.load(Ordering::SeqCst), 1);
    }

    /// 契约 C3：块边界穿过整条链之后一个都不能合并、不能被改长度。
    #[test]
    fn blocks_keep_their_boundaries_through_the_chain() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let mut chain = Chain::build(
            &[
                Op::Mono,
                Op::Gate {
                    config: GateConfig::MANUAL,
                },
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            // 采集率 == 会话率，重采样是 1:1 直通：长度变了就一定是链干的。
            CaptureFormat {
                sample_rate: SESSION_RATE,
                channels: 1,
            },
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();

        // 门关着：这一拍攒 preroll，不出货。
        scratch.begin(&chunk(SESSION_RATE, loud(160)), chain.has_mono());
        let (out, _) = chain.run(&mut scratch);
        assert!(out.is_empty());
        assert_eq!(out.len(), 0);
        scratch.recycle(out);

        // 门开：上升沿冲出 preroll + 当前块 = 两块，块边界与长度都要原样到链尾。
        chain.set_gate_active(true);
        scratch.begin(&chunk(SESSION_RATE, loud(160)), chain.has_mono());
        let (out, status) = chain.run(&mut scratch);
        assert_eq!(out.len(), 2, "preroll 与当前块必须是两块，不许合并");
        assert!(!out.is_empty());
        let blocks: Vec<Vec<f32>> = out.blocks().map(|block| block.to_vec()).collect();
        assert_eq!(blocks[0].len(), 160);
        assert_eq!(blocks[1].len(), 160);
        assert_eq!(blocks[1], loud(160));
        assert!(status.unwrap().active);
        scratch.recycle(out);
    }

    /// 门建在**采集率**上（尾巴的样本数按率算），链尾率沿链传播，
    /// 直通（没有 `Resample`）时就是采集率。
    #[test]
    fn a_stage_sees_the_rate_it_was_built_with() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        // 采集率必须与 `SESSION_RATE` **不同**：门建在采集率上，用会话率建的话尾巴的
        // 样本数会差一截，这条断言才钉得住。
        let rate = 44_100u32;
        let capture = CaptureFormat {
            sample_rate: rate,
            channels: 1,
        };
        let mut chain = Chain::build(
            &[
                Op::Mono,
                Op::Gate {
                    config: GateConfig::MANUAL,
                },
            ],
            &deps,
            capture,
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();

        chain.set_gate_active(true);
        drive(&mut chain, &mut scratch, &chunk(rate, loud(160)));
        // 松手：门发一段 `tail_ms` 的静音尾触发服务端断句。样本数按**建链时的率**算。
        chain.set_gate_active(false);
        let (blocks, status) = drive(&mut chain, &mut scratch, &chunk(rate, loud(160)));
        let expected_tail = rate as usize * GateConfig::MANUAL.tail_ms as usize / 1000;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].len(), expected_tail, "尾巴必须按采集率算样本数");
        assert!(status.unwrap().ended);

        // 链尾率：换了率就是会话率，没换就是采集率。
        assert_eq!(chain.out_rate(), rate, "没有 Resample 时链尾率就是采集率");
        let with_resample = Chain::build(
            &[
                Op::Mono,
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            capture,
            SESSION_RATE,
        )
        .unwrap();
        assert_eq!(with_resample.out_rate(), SESSION_RATE);
        let passthrough = Chain::build(
            &[Op::Mono, Op::Denoise],
            &deps,
            CaptureFormat {
                sample_rate: 44_100,
                channels: 1,
            },
            SESSION_RATE,
        )
        .unwrap();
        assert_eq!(passthrough.out_rate(), 44_100);
    }

    /// 降噪没装上的两种原因必须能被分辨：报什么、发不发通知都不一样。
    #[test]
    fn denoise_skip_carries_which_degrade_it_was() {
        let dsp = Arc::new(Dsp::default());
        let good = deps(&dsp, false);
        let broken = deps(&dsp, true);

        // 48 kHz + 工厂正常：降噪节装上了，没有降级。
        let ok = Chain::build(&[Op::Denoise], &good, mono_capture(), SESSION_RATE).unwrap();
        assert!(ok.denoise_skip().is_none());
        assert_eq!(dsp.denoise_calls.load(Ordering::SeqCst), 0);

        // 48 kHz + 工厂起不来：降级要带原始错误，那句 `tracing::warn!` 的 `%err` 靠它。
        let factory = Chain::build(&[Op::Denoise], &broken, mono_capture(), SESSION_RATE).unwrap();
        match factory.denoise_skip() {
            Some(DenoiseSkip::Factory(err)) => {
                assert_eq!(err.message, "降噪模型加载失败")
            }
            other => panic!("要认得出是工厂失败：{other:?}"),
        }

        // 采集率不是 48 kHz：另一种降级，带上那个率。今天这一支只 warn、不发通知。
        let wrong_rate = CaptureFormat {
            sample_rate: 44_100,
            channels: 1,
        };
        let rate = Chain::build(&[Op::Denoise], &good, wrong_rate, SESSION_RATE).unwrap();
        assert!(matches!(
            rate.denoise_skip(),
            Some(DenoiseSkip::Rate(44_100))
        ));

        // 清单里压根没有降噪节时，不许凭空造一个降级出来。
        let none = Chain::build(&[Op::Mono], &good, mono_capture(), SESSION_RATE).unwrap();
        assert!(none.denoise_skip().is_none());
    }

    // --- 计量 --------------------------------------------------------------

    /// 计量是**四个整数累加**，不许带进任何分配。
    ///
    /// 怎么数：装一个计数分配器，比两条链跑同样块数的分配次数之差——
    /// 空链（节循环一次都不进）与"只装 `Mono`"的链（进一次、且 `Mono` 是纯直通节，
    /// 按值搬不分配）。**两者之差就是"节循环 + 计量"的全部成本，必须是 0。**
    /// 两边剩下的那一次/块来自 `ChainScratch::begin` 的混音，两条链都一样
    /// （所以差值才干净），这里也顺手把绝对值钉住。
    #[test]
    fn timings_are_monotonic_counters_with_no_allocation() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let mut bare = Chain::build(&[], &deps, mono_capture(), SESSION_RATE).unwrap();
        let mut timed = Chain::build(&[Op::Mono], &deps, mono_capture(), SESSION_RATE).unwrap();
        let mut bare_scratch = ChainScratch::default();
        let mut timed_scratch = ChainScratch::default();
        let input = chunk(CAPTURE_RATE, loud(960));
        let blocks = 32u64;

        // 走 [`drive`] 的话读数会被它自己那两次 `to_vec`/`collect` 盖住，所以
        // 这里手写一拍：只 `begin` → `run` → `recycle`，别的什么都不做。
        fn one_block(chain: &mut Chain, scratch: &mut ChainScratch, input: &AudioChunk) {
            scratch.begin(input, chain.has_mono());
            let (out, _) = chain.run(scratch);
            scratch.recycle(out);
        }

        // 热身：乒乓槽、计量行的第一次写入都不该算进稳态读数。
        for _ in 0..4 {
            one_block(&mut bare, &mut bare_scratch, &input);
            one_block(&mut timed, &mut timed_scratch, &input);
        }

        let before_bare = alloc_count();
        for _ in 0..blocks {
            one_block(&mut bare, &mut bare_scratch, &input);
        }
        let bare_allocs = alloc_count() - before_bare;
        let before_timed = alloc_count();
        for _ in 0..blocks {
            one_block(&mut timed, &mut timed_scratch, &input);
        }
        let timed_allocs = alloc_count() - before_timed;

        assert_eq!(bare_allocs, blocks, "每块只该有 begin 那一次混音");
        assert_eq!(
            timed_allocs - bare_allocs,
            0,
            "节循环 + 计量不许新增一次分配：空链 {bare_allocs} 次、装一节 {timed_allocs} 次"
        );

        // 计量本身照实累加：一行一节，名字取自清单，出口率取自节。
        let rows = timed.timings();
        assert_eq!(rows.len(), 1, "装几节就有几行");
        let mono = rows[0];
        assert_eq!(mono.op, "mono");
        assert_eq!(mono.out_rate, CAPTURE_RATE);
        assert_eq!(mono.blocks, blocks + 4, "跑过的每一拍都要记一次");
        // 出口音频时长 = 每块 960 样本 / 48 kHz = 20 ms。
        assert_eq!(
            mono.audio_ns,
            (blocks + 4) * 960 * 1_000_000_000 / CAPTURE_RATE as u64
        );
        assert!(mono.max_ns <= mono.total_ns, "最慢的一拍不超过累计");
        assert!(
            mono.realtime_percent() > 0.0,
            "跑了这么多拍，纯搬运不至于 0%：{}",
            mono.realtime_percent()
        );

        // 空链没有行可读。
        assert!(bare.timings().is_empty());
    }

    /// 实时倍率的分母是**出口**音频时长。没出货的那一拍分母不许涨。
    #[test]
    fn realtime_percent_is_zero_when_there_was_no_audio() {
        // 纯函数那一半：一秒音频里花了 0.25 ms = 0.025%。
        let mut row = OpTiming::empty();
        assert_eq!(row.realtime_percent(), 0.0, "没跑过的节给 0，不炸");
        row.audio_ns = 1_000_000_000;
        row.total_ns = 250_000;
        assert!(
            (row.realtime_percent() - 0.025).abs() < 1e-9,
            "倍率算错：{}",
            row.realtime_percent()
        );

        // 真链那一半：降噪攒帧时这一拍出 0 块，分母不许被"输入了多少"顶上去。
        let dsp = Arc::new(Dsp::default());
        let deps = Deps {
            denoise: Box::new(|| Ok(Box::new(Framer { held: Vec::new() }) as Box<dyn Denoise>)),
            ..deps(&dsp, false)
        };
        let mut chain = Chain::build(
            &[Op::Mono, Op::Denoise],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();
        // 240 样本 < 一帧 480：链上零块。
        let (blocks, _) = drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(240)));
        assert_eq!(
            blocks.iter().map(|block| block.len()).sum::<usize>(),
            0,
            "没攒满一帧就不该出样本：{:?}",
            blocks.iter().map(|block| block.len()).collect::<Vec<_>>()
        );
        let rows = chain.timings();
        let denoise = rows.iter().find(|row| row.op == "denoise").unwrap();
        assert_eq!(denoise.blocks, 1, "跑过就是跑过");
        assert_eq!(denoise.audio_ns, 0, "这一拍没出货，分母不许涨");
        assert_eq!(denoise.realtime_percent(), 0.0);

        // 再喂一帧：这一拍出了整整 480 样本 = 10 ms。
        drive(&mut chain, &mut scratch, &chunk(CAPTURE_RATE, loud(240)));
        let rows = chain.timings();
        let denoise = rows.iter().find(|row| row.op == "denoise").unwrap();
        assert_eq!(denoise.blocks, 2);
        assert_eq!(
            denoise.audio_ns,
            FRAME as u64 * 1_000_000_000 / CAPTURE_RATE as u64
        );
    }

    /// 断线重来**不许**把计量清零：累计是"这一条链跑了多久"，重连的是同一条链。
    /// （清零会让嵌入式排障看到"重连之后 0%"，而那段时间的负载是真的。）
    #[test]
    fn a_disconnected_chain_keeps_counting_from_its_previous_totals() {
        let dsp = Arc::new(Dsp::default());
        let deps = deps(&dsp, false);
        let mut chain = Chain::build(
            &[
                Op::Mono,
                Op::Denoise,
                Op::Gate {
                    config: GateConfig::level(0.0),
                },
                Op::Resample {
                    from: RateRef::Capture,
                    to: RateRef::Session,
                },
            ],
            &deps,
            mono_capture(),
            SESSION_RATE,
        )
        .unwrap();
        let mut scratch = ChainScratch::default();
        let input = chunk(CAPTURE_RATE, loud(960));

        for _ in 0..3 {
            drive(&mut chain, &mut scratch, &input);
        }
        let before = chain.timings();
        assert!(
            before.iter().all(|row| row.blocks == 3),
            "每节都该记了 3 拍：{before:?}"
        );
        assert_eq!(
            before.iter().map(|row| row.op).collect::<Vec<_>>(),
            vec!["mono", "denoise", "gate", "resample"],
            "行名与顺序照抄清单"
        );
        // 出口率逐节对：换率只发生在 Resample。
        assert_eq!(before[0].out_rate, CAPTURE_RATE);
        assert_eq!(before[1].out_rate, CAPTURE_RATE);
        assert_eq!(before[2].out_rate, CAPTURE_RATE);
        assert_eq!(before[3].out_rate, SESSION_RATE);

        chain.on_disconnect();

        let kept = chain.timings();
        assert_eq!(kept, before, "断线不许动累计");

        for _ in 0..2 {
            drive(&mut chain, &mut scratch, &input);
        }
        let after = chain.timings();
        assert!(
            after
                .iter()
                .all(|row| row.blocks == 5 && row.total_ns >= row.max_ns),
            "重连之后接着往上记：{after:?}"
        );
        assert!(
            after.iter().all(|row| row.audio_ns > before[0].audio_ns),
            "音频时长也得接着涨：{after:?}"
        );
    }
}
