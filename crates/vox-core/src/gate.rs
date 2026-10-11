//! 激活门：决定采集到的音频哪些块要上传。
//!
//! 两种门：
//!
//! * `Manual`：由外部状态驱动（开关 / 按住说话模式下的按键）。激活时放行并先
//!   冲出 preroll 缓冲；从激活转为未激活的瞬间发一段 `tail_ms` 的零样本，触发
//!   服务端断句；未激活时样本进 preroll 缓冲，保证下次激活时起音不被削掉。
//! * `Level`：单个 RMS 阈值门（开关模式的电平判断与听人说话）。`threshold <= 0`
//!   时全部放行；否则 RMS 超过阈值放行，低于阈值时先走 tail 再收尾，静音期进
//!   preroll 缓冲。
//!
//! 门控参数（threshold / tail_ms / preroll_ms）与状态机逻辑是上一版调好的资产，
//! 逐行移植，不要重新推导。
//!
//! ## 第三种门不是 `GateKind`，是正交的一格
//!
//! [`GateKind`] 只有 `Manual` / `Level` 两种，**没有被扩成第三种**。半双工门
//! （[`HalfDuplex`]）回答的是另一个问题——"这段时间里一律不许上行"，而 `Manual` /
//! `Level` 回答的是"什么时候算在说话"。两者正交：任何一种门都可以同时装着半双工，
//! 所以加成 `GateKind` 就得表达 `HalfDuplex × {Manual, Level}` 的叉积（3 个 kind、
//! 6 份参数、两条状态机）。它因此是 [`GateConfig`] 的一个**字段**，`None` = 不装
//! （**桌面缺省**），装了就与 `kind` 无关地生效。
//!
//! 实现上唯一的体现是 [`ActivationGate::process`] 里那个**排在
//! `match self.config.kind` 之前**的压制分支：它不碰 `external_active`、不碰
//! `config`、不看 RMS ⇒ 两个 kind 被同一条路径覆盖，不存在"只在 Manual 上生效"的写法。

/// 门的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum GateKind {
    Manual,
    Level,
}

/// **半双工门**：回声消除三档里的保底档。
///
/// 外放的译音是我们自己播的，麦克风会把它录回去。硬件 AEC / 软件 AEC 拿不到时，
/// 这一档的做法是**播译音期间关掉上行，播完再压一段尾巴才重新开门**。
/// 代价是**会切断正在说的话**——这是"保底"两个字的实价，界面要照实说。
///
/// 这一档与 [`GateKind`] 的两种门**正交**：`Manual` 与 `Level` 照旧决定"什么时候算在
/// 说话"，本结构只决定"这段时间里一律不许上行"。所以它是 [`GateConfig`] 的一个字段，
/// 不是第三种门（模块头那一节有完整论证）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HalfDuplex {
    /// 播放停下之后还要压多久才重新开门（ms）。
    ///
    /// 压的是**房间里的回声尾巴**：最后一声译音出喇叭之后，麦克风还要再听一会儿
    /// 自己那间屋子的反射。与 [`GateConfig::tail_ms`] 是两个不同的东西——后者是"门关掉
    /// 时发多少零样本去触发服务端断句"。300 ms 的来由见 `S4-C-GATE-RESAMPLE.md` §2.3，
    /// **是一个估算值不是实测值**，有板子后要按实测定。
    pub tail_ms: u32,
}

impl HalfDuplex {
    /// 缺省尾巴。**改这个值会重录 `speak_48k_speaker_half_duplex` 那份金样**——
    /// 这就是它的护栏（新金样把"第几块恢复上行"逐字钉住）。
    pub const DEFAULT_TAIL_MS: u32 = 300;

    pub const fn with_tail_ms(tail_ms: u32) -> Self {
        Self { tail_ms }
    }

    /// 清单 / `Composition::of` 用的缺省实例。
    pub const DEFAULT: Self = Self {
        tail_ms: Self::DEFAULT_TAIL_MS,
    };
}

/// 一种激活门的完整参数。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GateConfig {
    pub kind: GateKind,
    /// Level 门使用；<= 0 表示全部放行。
    pub threshold: f32,
    pub tail_ms: u32,
    pub preroll_ms: u32,
    /// 半双工门。`None` = 不装这一档（**桌面缺省**），门的行为与不装时逐字相同。
    ///
    /// 两个 serde 属性缺一不可：`default` 让**老清单**（没有这个键）能读回来；
    /// `skip_serializing_if` 让它为假时**不写**这个键，否则 `composition.rs` 里那两条
    /// 逐字比 JSON 的既有测试当场红。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_duplex: Option<HalfDuplex>,
}

impl GateConfig {
    /// 开关 / 按住说话共用的手动门（原按住说话预设的调好参数）。
    pub const MANUAL: Self = Self {
        kind: GateKind::Manual,
        threshold: 0.012,
        tail_ms: 150,
        preroll_ms: 100,
        half_duplex: None,
    };

    /// 听人说话的电平门不设阈值（`level(0.0)` = 无条件放行）——环回的数字源
    /// 有假底噪，电平门会被底噪骗"正在说话"，与其烧 token 不如不设门。
    /// 要一个"带阈值但预设好参数"的电平门，直接调 `level(threshold)`。
    /// 开关模式的电平门：单个可调阈值，tail/preroll 用调好的默认值。
    pub fn level(threshold: f32) -> Self {
        Self {
            kind: GateKind::Level,
            threshold: threshold.max(0.0),
            tail_ms: 600,
            preroll_ms: 200,
            half_duplex: None,
        }
    }
}

impl Default for GateConfig {
    fn default() -> Self {
        Self::MANUAL
    }
}

/// 门当前处在状态机的哪一步，用于驱动 UI 的实时电平指示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    Empty,
    /// 手动门：按键按住，放行中。
    Manual,
    /// 手动门：刚松开，这一拍发的是断句用的静音尾。
    Released,
    /// 手动门：没按住，样本进 preroll 缓冲。
    Waiting,
    /// 电平门：阈值 <= 0，无条件放行。
    Always,
    /// 电平门：RMS 过阈值，放行中。
    Speech,
    /// 电平门：掉到阈值下但还在 tail 窗口内，继续放行。
    Tail,
    /// 电平门：tail 窗口走完，这一段结束。
    TailEnd,
    /// 电平门：静音，样本进 preroll 缓冲。
    Silence,
    /// 半双工门：正在外放译音（或还在回声尾巴里），上行整块压住不放行。
    ///
    /// 线上的 serde 值是 `"playback_mute"`（`rename_all = "snake_case"` 作用于名字，
    /// 新增变体不改老变体的值）。前端 `app/ui/src/types.ts::GateState` 是同名的
    /// `snake_case` 联合类型，加这一行是 `S4-C-GATE-RESAMPLE.md` G3 工单的活。
    PlaybackMute,
}

/// 一次 `process` 的状态快照。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GateStatus {
    pub kind: GateKind,
    pub state: GateState,
    pub rms: f32,
    pub active: bool,
    /// 这一拍是否是一段语音的收尾（可用于提示服务端已断句）。
    pub ended: bool,
}

/// 把 f32 样本块流过滤成要上传的部分。
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
    /// 门没装 `half_duplex` 时这一格恒为 0，`process` 里那个分支根本进不去。
    mute_samples: usize,
}

impl ActivationGate {
    pub fn new(config: GateConfig, sample_rate: u32) -> Self {
        Self {
            config,
            sample_rate,
            active: false,
            silent_samples: 0,
            prebuffer: std::collections::VecDeque::new(),
            prebuffer_samples: 0,
            external_active: false,
            mute_samples: 0,
        }
    }

    pub fn config(&self) -> GateConfig {
        self.config
    }

    /// 手动门的外部驱动（按键按下/松开、手动纠正）。
    pub fn set_external_active(&mut self, active: bool) {
        self.external_active = active;
    }

    pub fn external_active(&self) -> bool {
        self.external_active
    }

    /// 热切换门参数；内部状态复位，但保留外部激活状态。
    pub fn set_config(&mut self, config: GateConfig) {
        let external_active = self.external_active;
        // 压制窗口只在"换完之后**仍然**装着这一档"时保留。
        //
        // 保留的理由：换 `tail_ms` / `threshold` / `preroll_ms` 是一条 `Note`
        // （`Note::GateConfig`），它在压制中途也**不许**把上行放开——与保留
        // `external_active` 同一个道理，注意换门**不改变窗口的长度**。
        //
        // 归零的理由：`half_duplex` 由 `Some` 变 `None` 是"**关掉这一档**"的语义，
        // 带着窗口继续压就是"关了还在生效"，自相矛盾。
        let mute = if config.half_duplex.is_some() {
            self.mute_samples
        } else {
            0
        };
        self.config = config;
        self.reset();
        self.external_active = external_active;
        self.mute_samples = mute;
    }

    /// 装满压制窗口：`ms` 毫秒 = `ms × sample_rate / 1000` 个采集样本。
    ///
    /// 换算沿用 [`ActivationGate::samples_for_ms`] 的既有约定（整数截断），
    /// 差分台靠"样本"而不是"毫秒"得到确定性；窗口只会被下一次调用重新装满，
    /// 不会自己变长。
    ///
    /// 门**没装** `half_duplex` 时这一句就是空操作（`mute_samples` 恒 0）——
    /// 桌面档为此**一个样本都不多压**。
    pub fn set_mute_for_ms(&mut self, ms: u64) {
        self.mute_samples = if self.config.half_duplex.is_some() {
            ((self.sample_rate as u64).saturating_mul(ms) / 1000).min(usize::MAX as u64) as usize
        } else {
            0
        };
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.silent_samples = 0;
        self.prebuffer.clear();
        self.prebuffer_samples = 0;
        self.external_active = false;
        self.mute_samples = 0;
    }

    /// 过滤一个样本块，返回要上传的块（可能为空）加当前状态。
    pub fn process(&mut self, samples: &[f32]) -> (Vec<Vec<f32>>, GateStatus) {
        if samples.is_empty() {
            let active = self.active;
            return (
                Vec::new(),
                self.status(GateState::Empty, 0.0, active, false),
            );
        }

        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        let rms = (sum_sq / samples.len() as f32).sqrt();

        // —— 半双工压制。排在 `match self.config.kind` **之前** ⇒ 两个 kind 被同一条
        //    路径覆盖，不存在"只在 Manual 上生效"的写法。
        //
        //    `&& self.config.half_duplex.is_some()` **不许省**：它是"没装这一档的会话
        //    绝不会被压住"这条不变式的**最后一道**。少了它，一个因为换门参数而残留的
        //    窗口会把没装半双工的会话也压住。三道机制（`set_mute_for_ms` 见 `None`
        //    写 0、`set_config` 在新配置无半双工时归零、这一句）合起来，`half_duplex =
        //    None` ⇒ 上行**一定**不被压住，任一道被后人改坏另外两道还在。
        //
        //    这一分支**不碰 `external_active`**（用户可能还按着，跨越压制不变）、
        //    **不碰 `config`**（换门参数只影响压制**结束之后**的判据）、
        //    **不看 RMS**。
        if self.mute_samples > 0 && self.config.half_duplex.is_some() {
            self.mute_samples = self.mute_samples.saturating_sub(samples.len());
            // 压制**刚开始**的这一拍：把 preroll 倒掉，压制期间也不往里存。
            // preroll 是 `preroll_ms` 长度的环形缓冲，而压制窗口的前半段设备还在放
            // 译音——冲出来的会是我们自己译音的回声。重新开门的第一件事就是把
            // 100–200 ms 的译音回声上传给服务端，这一档存在的意义当场被推翻。
            // 代价是用户起音最多被削掉 `preroll_ms`（<= 200 ms），但他是在听完译音
            // 之后才开口的，那点提前量本来也不够用。
            self.prebuffer.clear();
            self.prebuffer_samples = 0;
            if self.active {
                // 门本来是开着的：照手动门的收尾边断句（`process_manual` 逐条同构），
                // 尾巴用 `config.tail_ms`——**不新增字段**。
                self.active = false;
                self.silent_samples = 0;
                let tail = vec![0.0f32; self.samples_for_ms(self.config.tail_ms).max(1)];
                return (
                    vec![tail],
                    self.status(GateState::PlaybackMute, rms, false, true),
                );
            }
            return (
                Vec::new(),
                self.status(GateState::PlaybackMute, rms, false, false),
            );
        }

        match self.config.kind {
            GateKind::Manual => self.process_manual(samples, rms),
            GateKind::Level => self.process_level(samples, rms),
        }
    }

    fn process_manual(&mut self, samples: &[f32], rms: f32) -> (Vec<Vec<f32>>, GateStatus) {
        if self.external_active {
            let accepted = self.accept(samples);
            return (accepted, self.status(GateState::Manual, rms, true, false));
        }
        if self.active {
            // 刚从激活转为未激活：补一段零样本触发服务端断句。
            self.active = false;
            // 空块上传没有意义，尾音至少给一个样本（旧版就是这么写的）。
            let tail = vec![0.0f32; self.samples_for_ms(self.config.tail_ms).max(1)];
            return (
                vec![tail],
                self.status(GateState::Released, rms, false, true),
            );
        }
        self.hold(samples, rms, GateState::Waiting)
    }

    fn process_level(&mut self, samples: &[f32], rms: f32) -> (Vec<Vec<f32>>, GateStatus) {
        if self.config.threshold <= 0.0 {
            return (
                vec![samples.to_vec()],
                self.status(GateState::Always, rms, true, false),
            );
        }

        if rms >= self.config.threshold {
            let accepted = self.accept(samples);
            self.silent_samples = 0;
            return (accepted, self.status(GateState::Speech, rms, true, false));
        }

        if self.active {
            self.silent_samples += samples.len();
            let tail_samples = self.samples_for_ms(self.config.tail_ms);
            let ended = self.silent_samples >= tail_samples;
            if ended {
                self.active = false;
                self.silent_samples = 0;
            }
            let state = if ended {
                GateState::TailEnd
            } else {
                GateState::Tail
            };
            return (
                vec![samples.to_vec()],
                self.status(state, rms, !ended, ended),
            );
        }

        self.hold(samples, rms, GateState::Silence)
    }

    fn status(&self, state: GateState, rms: f32, active: bool, ended: bool) -> GateStatus {
        GateStatus {
            kind: self.config.kind,
            state,
            rms,
            active,
            ended,
        }
    }

    /// 一段时长（ms）换算成当前采样率下的样本数。
    fn samples_for_ms(&self, ms: u32) -> usize {
        (self.sample_rate as u64 * ms as u64 / 1000) as usize
    }

    /// 放行一块：上升沿先把 preroll 冲出来，再跟上当前块。
    fn accept(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut accepted = Vec::new();
        if !self.active {
            accepted.extend(self.flush_prebuffer());
            self.active = true;
        }
        accepted.push(samples.to_vec());
        accepted
    }

    /// 不放行：块进 preroll 缓冲，这一拍什么都不上传。
    fn hold(&mut self, samples: &[f32], rms: f32, state: GateState) -> (Vec<Vec<f32>>, GateStatus) {
        self.push_prebuffer(samples);
        (Vec::new(), self.status(state, rms, false, false))
    }

    fn push_prebuffer(&mut self, samples: &[f32]) {
        let max_samples = self.samples_for_ms(self.config.preroll_ms);
        if max_samples == 0 {
            return;
        }
        self.prebuffer.push_back(samples.to_vec());
        self.prebuffer_samples += samples.len();
        while self.prebuffer_samples > max_samples {
            match self.prebuffer.pop_front() {
                Some(first) => self.prebuffer_samples -= first.len(),
                None => break,
            }
        }
    }

    fn flush_prebuffer(&mut self) -> Vec<Vec<f32>> {
        self.prebuffer_samples = 0;
        self.prebuffer.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn loud(n: usize) -> Vec<f32> {
        vec![0.5; n]
    }

    fn quiet(n: usize) -> Vec<f32> {
        vec![0.0001; n]
    }

    #[test]
    fn manual_gate_passes_only_while_active_and_emits_tail() {
        let mut gate = ActivationGate::new(GateConfig::MANUAL, RATE);

        // 没按住：不放行，攒 preroll。
        let (out, st) = gate.process(&loud(160));
        assert!(out.is_empty());
        assert_eq!(st.state, GateState::Waiting);

        // 按住：先冲出 preroll，再放行当前块。
        gate.set_external_active(true);
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
        assert!(st.active);
        assert_eq!(out.len(), 2, "preroll 应当被冲出来跟当前块一起走");

        // 松开：发一段静音尾触发断句。
        gate.set_external_active(false);
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Released);
        assert!(st.ended);
        assert_eq!(out.len(), 1);
        let expected_tail = RATE as usize * GateConfig::MANUAL.tail_ms as usize / 1000;
        assert_eq!(out[0].len(), expected_tail);
        assert!(out[0].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn level_gate_opens_on_speech_and_closes_after_tail() {
        let mut gate = ActivationGate::new(GateConfig::level(0.012), RATE);
        let block = 160; // 10 ms

        let (out, st) = gate.process(&quiet(block));
        assert!(out.is_empty());
        assert_eq!(st.state, GateState::Silence);

        let (out, st) = gate.process(&loud(block));
        assert_eq!(st.state, GateState::Speech);
        assert_eq!(out.len(), 2, "上升沿应冲出 preroll");

        // tail 是 600 ms；静音块要放行到走完 tail 才收尾。
        let tail_blocks = (RATE as usize * 600 / 1000) / block;
        for i in 0..tail_blocks {
            let (out, st) = gate.process(&quiet(block));
            assert_eq!(out.len(), 1, "tail 期间静音也要放行");
            if i + 1 < tail_blocks {
                assert_eq!(st.state, GateState::Tail);
                assert!(st.active);
            } else {
                assert_eq!(st.state, GateState::TailEnd);
                assert!(st.ended);
                assert!(!st.active);
            }
        }

        let (out, st) = gate.process(&quiet(block));
        assert!(out.is_empty());
        assert_eq!(st.state, GateState::Silence);
    }

    #[test]
    fn level_gate_with_zero_threshold_passes_everything() {
        let mut gate = ActivationGate::new(GateConfig::level(0.0), RATE);
        let (out, st) = gate.process(&quiet(160));
        assert_eq!(out.len(), 1);
        assert_eq!(st.state, GateState::Always);
    }

    #[test]
    fn preroll_is_bounded() {
        let mut gate = ActivationGate::new(GateConfig::MANUAL, RATE);
        // preroll 100 ms = 1600 样本；灌远超上限的量。
        for _ in 0..50 {
            gate.process(&loud(160));
        }
        gate.set_external_active(true);
        let (out, _) = gate.process(&loud(160));
        let flushed: usize = out.iter().map(|c| c.len()).sum::<usize>() - 160;
        let cap = RATE as usize * GateConfig::MANUAL.preroll_ms as usize / 1000;
        assert!(
            flushed <= cap + 160,
            "preroll 不能无限涨: {flushed} > {cap}"
        );
    }

    #[test]
    fn set_config_keeps_external_active() {
        let mut gate = ActivationGate::new(GateConfig::MANUAL, RATE);
        gate.set_external_active(true);
        gate.set_config(GateConfig::level(0.02));
        assert!(gate.external_active(), "热切换门参数不该丢掉按键状态");
        assert_eq!(gate.config().kind, GateKind::Level);
    }

    /// 装上半双工的手动门（压 300 ms = 4800 个 16 kHz 样本）。
    fn manual_half_duplex() -> ActivationGate {
        ActivationGate::new(
            GateConfig {
                half_duplex: Some(HalfDuplex::DEFAULT),
                ..GateConfig::MANUAL
            },
            RATE,
        )
    }

    #[test]
    fn half_duplex_silences_the_uplink_until_the_window_runs_out() {
        let mut gate = manual_half_duplex();
        gate.set_external_active(true);
        // 门开着，上行在跑。
        let (out, st) = gate.process(&loud(160));
        assert!(st.active);
        assert_eq!(out.len(), 1);

        // 译音推进播放汇 -> 装满压制窗口。
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        let window = RATE as usize * HalfDuplex::DEFAULT_TAIL_MS as usize / 1000;
        let blocks = window / 160;
        for i in 0..blocks {
            let (out, st) = gate.process(&loud(160));
            assert_eq!(st.state, GateState::PlaybackMute, "第 {i} 块仍在压制窗口内");
            assert!(!st.active);
            // 只有窗口刚开始的那一拍发断句尾，之后一律一块都不上传。
            if i > 0 {
                assert!(out.is_empty(), "第 {i} 块不该有任何上行");
            }
        }

        // 窗口走完：照手动门的老路恢复。
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
        assert!(st.active);
        assert_eq!(out.len(), 1, "压制结束后应当恢复放行");
    }

    #[test]
    fn a_mute_arriving_mid_sentence_ends_it_with_a_zero_tail() {
        let mut gate = manual_half_duplex();
        gate.set_external_active(true);
        gate.process(&loud(160));
        assert!(gate.active);

        // 一句话正说到一半，外放开始。
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::PlaybackMute);
        assert!(st.ended, "压制打断了正在说的那一段，必须告诉服务端断句");
        assert!(!st.active);
        assert_eq!(out.len(), 1);
        // 尾巴长度用 `config.tail_ms`（手动门 = 150 ms），**不是** `half_duplex.tail_ms`。
        let expected = RATE as usize * GateConfig::MANUAL.tail_ms as usize / 1000;
        assert_eq!(out[0].len(), expected);
        assert!(out[0].iter().all(|s| *s == 0.0), "断句尾必须全是零样本");

        // 断句之后压制还在继续：不许再补第二段尾（会重复断句）。
        let (out, st) = gate.process(&loud(160));
        assert!(!st.ended);
        assert!(out.is_empty());
    }

    #[test]
    fn the_prebuffer_is_cleared_when_the_mute_begins_and_not_refilled() {
        let blocks = (RATE as usize * HalfDuplex::DEFAULT_TAIL_MS as usize / 1000) / 160;

        // —— ① 压制**开始**的那一拍把已有 preroll 倒掉。
        let mut gate = manual_half_duplex();
        for _ in 0..5 {
            gate.process(&loud(160)); // 不按着 = Waiting，攒 preroll
        }
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        for _ in 0..blocks {
            gate.process(&loud(160));
        }
        gate.set_external_active(true);
        let (out, _) = gate.process(&loud(160));
        assert_eq!(
            out.len(),
            1,
            "压制开始前攒的 preroll 必须已被倒掉，否则重新开门会把译音回声冲出去"
        );

        // —— ② 压制**期间**不往 preroll 里存。
        let mut gate = manual_half_duplex();
        gate.set_external_active(true);
        gate.process(&loud(160));
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        for _ in 0..blocks {
            gate.process(&loud(160));
        }
        assert!(!gate.active);
        // 按键还按着：accept 的上升沿会冲 preroll。若压制期间回填了，
        // 这里会是好几块而不是当前这一块。
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
        assert_eq!(out.len(), 1, "压制期间攒下的音频（含译音回声）不该被冲出来");
        assert_eq!(out[0].len(), 160);
    }

    #[test]
    fn the_window_ends_after_exactly_its_tail_in_capture_samples() {
        let mut gate = manual_half_duplex();
        gate.set_external_active(true);
        gate.set_mute_for_ms(1000);
        // 160 样本 / 块 @ 16 kHz = 10 ms，1000 ms = 100 块。压满 100 块，第 101 块放行。
        let blocks = 100usize;
        for i in 0..blocks {
            let (_, st) = gate.process(&loud(160));
            let block_ms = (i + 1) * 10;
            assert_eq!(
                st.state,
                GateState::PlaybackMute,
                "第 {i} 块（{block_ms} ms）仍在窗口内"
            );
        }
        let (_, st) = gate.process(&loud(160));
        assert_eq!(
            st.state,
            GateState::Manual,
            "第 101 块（1010 ms）应当已放行"
        );

        // 窗口不会自己变长：走完之后再等多久都是放行态。
        let (_, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
    }

    #[test]
    fn a_gate_without_half_duplex_never_mutes() {
        let mut gate = ActivationGate::new(GateConfig::MANUAL, RATE);
        gate.set_external_active(true);
        // 桌面档：`Worker` 照样会调这一句，门必须当空操作。
        gate.set_mute_for_ms(10_000);
        for i in 0..10 {
            let (out, st) = gate.process(&loud(160));
            assert_eq!(st.state, GateState::Manual, "第 {i} 块不该被压制");
            assert!(st.active);
            assert_eq!(out.len(), 1, "第 {i} 块应当照常上行");
        }
        // 同理，`set_mute_for_ms` 不许把一个本来没有的窗口"攒出来"。
        let mut level = ActivationGate::new(GateConfig::level(0.0), RATE);
        level.set_mute_for_ms(10_000);
        let (out, st) = level.process(&loud(160));
        assert_eq!(st.state, GateState::Always);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn set_config_keeps_the_mute_window() {
        let mut gate = manual_half_duplex();
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        let blocks = (RATE as usize * HalfDuplex::DEFAULT_TAIL_MS as usize / 1000) / 160;
        // 先走掉一半窗口。
        for _ in 0..blocks / 2 {
            gate.process(&loud(160));
        }
        // 换门（`kind` 与阈值都换了，**但仍然装着半双工**）。
        let mut switched = GateConfig::level(0.02);
        switched.half_duplex = Some(HalfDuplex::DEFAULT);
        gate.set_config(switched);
        assert!(!gate.active);
        // 换门参数不该在压制中途把上行放开：剩余的一半照压。
        for i in 0..blocks / 2 {
            let (_, st) = gate.process(&loud(160));
            assert_eq!(st.state, GateState::PlaybackMute, "换门后第 {i} 块仍在压制");
        }
        // 窗口走完之后，新门的判据立刻生效（不会被卡在压制态）。
        let (_, st) = gate.process(&quiet(160));
        assert_eq!(st.state, GateState::Silence);
    }

    #[test]
    fn set_config_dropping_half_duplex_ends_the_mute_immediately() {
        let mut gate = manual_half_duplex();
        gate.set_external_active(true);
        gate.process(&loud(160));
        assert!(gate.active);
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        // 窗口才走了一拍，这时候换门、而且把这一档关掉。
        let (_, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::PlaybackMute);

        gate.set_config(GateConfig::MANUAL); // `half_duplex = None`
                                             // 关掉这一档 = "上行立刻恢复"：窗口不许残留在门里。
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual, "关掉半双工之后不许还在压制");
        assert!(st.active);
        assert_eq!(out.len(), 1, "关掉这一档就该立刻上行");

        // 就算有人再装满一次窗口（`Worker` 照样会调），门也不认——第一道机制。
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
        assert!(st.active);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn reset_clears_the_mute_window() {
        let mut gate = manual_half_duplex();
        gate.set_mute_for_ms(u64::from(HalfDuplex::DEFAULT_TAIL_MS));
        // "reset 就是清干净"这条既有语义对压制窗口不为例外。
        gate.reset();
        gate.set_external_active(true);
        let (out, st) = gate.process(&loud(160));
        assert_eq!(st.state, GateState::Manual);
        assert!(st.active);
        assert_eq!(out.len(), 1, "reset 之后不该还在压制窗口里");
    }

    #[test]
    fn the_playback_mute_state_goes_on_the_wire_as_snake_case() {
        // 线上形状（进 `PipelineStatus.gate` -> 账本事件 -> 前端）钉在这里。
        assert_eq!(
            serde_json::to_string(&GateState::PlaybackMute).unwrap(),
            "\"playback_mute\""
        );
        assert_eq!(
            serde_json::from_str::<GateState>("\"playback_mute\"").unwrap(),
            GateState::PlaybackMute
        );
        // 加变体不许改动老变体的线上值。
        for (json, state) in [
            ("\"empty\"", GateState::Empty),
            ("\"manual\"", GateState::Manual),
            ("\"released\"", GateState::Released),
            ("\"waiting\"", GateState::Waiting),
            ("\"always\"", GateState::Always),
            ("\"speech\"", GateState::Speech),
            ("\"tail\"", GateState::Tail),
            ("\"tail_end\"", GateState::TailEnd),
            ("\"silence\"", GateState::Silence),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
        }
    }

    #[test]
    fn half_duplex_is_absent_from_the_gate_config_wire_when_unset() {
        // 桌面缺省：清单里**不许**多出这个键。`composition.rs` 那两条逐字比 JSON 的
        // 既有测试就靠这一条，改坏了它们当场红。
        let json = serde_json::to_value(GateConfig::MANUAL).unwrap();
        assert!(json.get("half_duplex").is_none(), "不该写这个键: {json}");
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["kind", "preroll_ms", "tail_ms", "threshold"]);
        assert_eq!(obj["kind"], "manual");
        assert_eq!(obj["tail_ms"], 150);
        assert_eq!(obj["preroll_ms"], 100);
        // f32 过 serde 之后与源值逐位相同（`threshold` 在线上就是 0.012000000104308128）。
        assert_eq!(
            obj["threshold"].as_f64().unwrap() as f32,
            GateConfig::MANUAL.threshold
        );
        assert!(serde_json::to_value(GateConfig::level(0.012))
            .unwrap()
            .get("half_duplex")
            .is_none());

        // 装上了就要写出全这一格。
        let hd = manual_half_duplex();
        assert_eq!(
            serde_json::to_value(hd.config())
                .unwrap()
                .get("half_duplex"),
            Some(&serde_json::json!({ "tail_ms": 300 }))
        );

        // 老清单（没有这个键）必须还能读回来。
        let old: GateConfig = serde_json::from_str(
            r#"{"kind":"level","threshold":0.012,"tail_ms":600,"preroll_ms":200}"#,
        )
        .unwrap();
        assert_eq!(old, GateConfig::level(0.012));
        assert!(old.half_duplex.is_none());
    }
}
