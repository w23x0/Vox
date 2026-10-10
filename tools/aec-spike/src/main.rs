//! S4-C · W0 尖峰：`sonora-aec3` 在受控合成信号上到底能不能压回声，
//! 最小可用调用序列长什么样，代价落在谁头上。
//!
//! ```text
//! cd tools/aec-spike && cargo run --release      # 一条命令出全部数字
//! ```
//!
//! 信号全部确定性合成（xorshift 固定种子 + 解析带限），不读文件、不连声卡。
//!
//! # 走的是块级路径（`sonora-aec3` 唯一的公开形状）
//!
//! ```text
//! 全带 10 ms 帧 ──analysis──▶ num_bands × 160 样本
//!   （48 k → 3 个频带，走 ThreeBandFilterBank；16 k → 1 个，拆/合是恒等拷贝）
//!                    │
//!        每 160 样本拆成两个 80 样本子帧
//!                    ▼
//!   FrameBlocker::insert_sub_frame_and_extract_block   80 子帧 ──▶ 64 块(4 ms)
//!                    │
//!   参考路：BlockProcessor::buffer_render        ◀──┘
//!   采集路：BlockProcessor::process_capture           （64 块进、64 块出）
//!                    ▼
//!   BlockFramer::insert_block_and_extract_sub_frame  64 块 ──▶ 80 子帧
//!                    ▼
//! 全带 10 ms 帧 ◀──synthesis── num_bands × 160 样本
//! ```
//!
//! # 三条从上游源码读出来、文档里没写的形状（实证见程序输出）
//!
//! 1. **每 4 次子帧插入会攒出一个余块，必须 `is_block_available()` 抽干**，
//!    否则第 5 次插入 panic（`frame_blocker.rs` 的 `debug_assert` 与
//!    `extract_block` 的 `debug_assert!(is_block_available())`）。
//! 2. **输入幅度是 int16 域，不是 f32 的 [-1,1]。** `config::Delay` 的
//!    `poor_excitation_render_limit = 150` 折算成 matched filter 的
//!    `x2_sum_threshold = 滤波器长(512) × 150² = 1.152e7`；喂 [-1,1] 的信号
//!    永远过不了这道门 → 时延估不出来 → **ERLE ≈ 0 dB**。
//! 3. **只支持 16000/32000/48000 三个率**（`common::valid_full_band_rate`），
//!    且输出比输入晚若干块（算子延迟；按真值算 ERLE 必须先把它量出来）。

use std::time::Instant;

use sonora::three_band_filter_bank::{
    ThreeBandFilterBank, FULL_BAND_SIZE, NUM_BANDS, SPLIT_BAND_SIZE,
};
use sonora_aec3::block::Block;
use sonora_aec3::block_framer::BlockFramer;
use sonora_aec3::block_processor::BlockProcessor;
use sonora_aec3::common::{num_bands_for_rate, BLOCK_SIZE};
use sonora_aec3::config::EchoCanceller3Config;
use sonora_aec3::frame_blocker::FrameBlocker;

/// 10 ms 一帧（AEC3 的原生工作单位）。
const FRAME_MS: usize = 10;
/// 每频带每帧样本数（16 kHz × 10 ms，所有率都一样）。
const BAND_FRAME: usize = SPLIT_BAND_SIZE;
/// 子帧长 = 半帧，`FrameBlocker` 的输入单位。
const SUB: usize = BAND_FRAME / 2;
/// 块长（16 kHz 下的 4 ms）。
const BLK: usize = BLOCK_SIZE;
/// 一块折合多少毫秒（`common.rs` 里是 crate-private，自己算同一个数）。
const BLOCK_MS: usize = BLOCK_SIZE * 1000 / 16000;
/// AEC3 的门限活在 int16 域（±32768）。这个标尺是 W0 的**第一号发现**。
const S16_SCALE: f32 = 32768.0;
/// 回声耦合衰减 −6 dB。
const ECHO_GAIN: f32 = 0.501_187;
/// 双讲时近端相对参考的幅度（−14 dB）。
const NEAR_GAIN: f32 = 0.2;

// ---------------------------------------------------------------- 确定性信号

/// xorshift32，固定种子。尖峰不引 `rand`。
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed | 1)
    }
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// 一阶高通 + 二阶低通，把白噪声整成带限噪声。
struct BandLimited {
    hp_y: f32,
    hp_prev: f32,
    lp1_y: f32,
    lp2_y: f32,
    hp_a: f32,
    lp_a: f32,
}

impl BandLimited {
    fn new(rate: usize, hp_hz: f32, lp_hz: f32) -> Self {
        let r = rate as f32;
        Self {
            hp_y: 0.0,
            hp_prev: 0.0,
            lp1_y: 0.0,
            lp2_y: 0.0,
            hp_a: (-std::f32::consts::TAU * hp_hz / r).exp(),
            lp_a: 1.0 - (-std::f32::consts::TAU * lp_hz / r).exp(),
        }
    }
    fn next(&mut self, rng: &mut Rng) -> f32 {
        let x = rng.next_f32();
        let y = x - self.hp_prev + self.hp_a * self.hp_y;
        self.hp_prev = x;
        self.hp_y = y;
        let l1 = y + self.lp_a * (self.lp1_y - y);
        self.lp1_y = l1;
        let l2 = l1 + self.lp_a * (self.lp2_y - l1);
        self.lp2_y = l2;
        l2
    }
}

fn synth(n: usize, rate: usize, seed: u32, hp: f32, lp: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut f = BandLimited::new(rate, hp, lp);
    (0..n).map(|_| f.next(&mut rng)).collect()
}

fn rms(v: &[f32]) -> f64 {
    (v.iter().map(|&x| (x * x) as f64).sum::<f64>() / v.len() as f64).sqrt()
}

fn normalize_to_rms(buf: &mut [f32], target: f64) {
    if target == 0.0 {
        for x in buf.iter_mut() {
            *x = 0.0;
        }
        return;
    }
    let g = target as f32 / rms(buf) as f32;
    for x in buf.iter_mut() {
        *x *= g;
    }
}

fn db(x: f64) -> f64 {
    if x <= 1e-30 {
        f64::NEG_INFINITY
    } else {
        10.0 * x.log10()
    }
}

/// 24 k → 48 k 的**线性插值**升采样（2 倍整数插值）。这正是 §5.4 里 W1 要用的
/// 那一步：上游译音先按 `OUTPUT_SAMPLE_RATE` 合成，再拉到采集率。
fn upsample_linear(src: &[f32], ratio: usize) -> Vec<f32> {
    assert!(ratio >= 2);
    let mut out = vec![0.0f32; (src.len() - 1) * ratio + 1];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / ratio as f64;
        let i0 = t.floor() as usize;
        let frac = (t - i0 as f64) as f32;
        let i1 = (i0 + 1).min(src.len() - 1);
        *o = src[i0] * (1.0 - frac) + src[i1] * frac;
    }
    out
}

// ---------------------------------------------------------------- 块级 AEC3

/// 一条完整的块级 AEC3 链。**这就是 W1 要照抄的调用序列。**
struct BlockLevelAec {
    num_bands: usize,
    /// 只有 `num_bands > 1` 才需要滤波器组；1 个频带时拆/合是恒等拷贝。
    split: Option<(ThreeBandFilterBank, ThreeBandFilterBank)>,
    render_blocker: FrameBlocker,
    capture_blocker: FrameBlocker,
    out_framer: BlockFramer,
    proc: BlockProcessor,

    render_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    capture_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    out_bands: [[f32; BAND_FRAME]; NUM_BANDS],
    sub: [[f32; SUB]; NUM_BANDS],
    render_block: Block,
    capture_block: Block,
}

impl BlockLevelAec {
    fn new(rate: usize, external_delay_ms: Option<i32>) -> Self {
        assert!(
            matches!(rate, 16_000 | 48_000),
            "公开的块级 API 只驱动得了 1 频带(16k)与 3 频带(48k)：\
             2 频带的 QMF 分频器在父 crate 里是 pub(crate)"
        );
        let nb = num_bands_for_rate(rate);
        let mut cfg = EchoCanceller3Config::default();
        cfg.validate();
        if external_delay_ms.is_some() {
            // 打开外部时延估计器 = 关掉内建那条。
            cfg.delay.use_external_delay_estimator = true;
        }
        let mut me = Self {
            num_bands: nb,
            split: (nb > 1).then(|| (ThreeBandFilterBank::new(), ThreeBandFilterBank::new())),
            render_blocker: FrameBlocker::new(nb, 1),
            capture_blocker: FrameBlocker::new(nb, 1),
            out_framer: BlockFramer::new(nb, 1),
            proc: BlockProcessor::new(&cfg, rate, 1, 1),
            render_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            capture_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            out_bands: [[0.0; BAND_FRAME]; NUM_BANDS],
            sub: [[0.0; SUB]; NUM_BANDS],
            render_block: Block::new(nb, 1),
            capture_block: Block::new(nb, 1),
        };
        if let Some(ms) = external_delay_ms {
            me.proc.set_audio_buffer_delay(ms);
        }
        me
    }

    fn metrics(&self) -> (f64, f64, i32) {
        let m = self.proc.get_metrics();
        (
            m.echo_return_loss_enhancement,
            m.echo_return_loss,
            m.delay_ms,
        )
    }

    /// 送一帧 10 ms 全带参考（播放侧）。
    fn push_render(&mut self, frame: &[f32]) {
        split_bands(&mut self.split, frame, &mut self.render_bands);
        for sub in 0..2 {
            let off = sub * SUB;
            // 借用拆分：self.sub 由 push_sub_frame 内部填，Block 由外面给。
            for band in 0..self.num_bands {
                self.sub[band].copy_from_slice(&self.render_bands[band][off..off + SUB]);
            }
            let refs: Vec<Vec<&[f32]>> = (0..self.num_bands)
                .map(|b| vec![self.sub[b].as_slice()])
                .collect();
            let mut blk = std::mem::replace(&mut self.render_block, Block::new(self.num_bands, 1));
            self.render_blocker
                .insert_sub_frame_and_extract_block(&refs, &mut blk);
            self.render_block = blk;
            self.proc.buffer_render(&self.render_block);
        }
        // 每第 4 次插入攒出的余块必须抽干，否则第 5 次插入越界 panic。
        if self.render_blocker.is_block_available() {
            self.render_blocker.extract_block(&mut self.render_block);
            self.proc.buffer_render(&self.render_block);
        }
    }

    /// 送一帧 10 ms 全带采集（上行侧），同样 10 ms 全带结果写进 `out`。
    fn process_capture(&mut self, frame: &[f32], out: &mut [f32]) {
        split_bands(&mut self.split, frame, &mut self.capture_bands);
        for sub in 0..2 {
            let off = sub * SUB;
            for band in 0..self.num_bands {
                self.sub[band].copy_from_slice(&self.capture_bands[band][off..off + SUB]);
            }
            let refs: Vec<Vec<&[f32]>> = (0..self.num_bands)
                .map(|b| vec![self.sub[b].as_slice()])
                .collect();
            let mut blk = std::mem::replace(&mut self.capture_block, Block::new(self.num_bands, 1));
            self.capture_blocker
                .insert_sub_frame_and_extract_block(&refs, &mut blk);
            self.capture_block = blk;
            self.proc
                .process_capture(false, false, None, &mut self.capture_block);

            // 块 → 子帧
            let mut sf: Vec<Vec<Vec<f32>>> =
                (0..self.num_bands).map(|_| vec![vec![0.0; SUB]]).collect();
            self.out_framer
                .insert_block_and_extract_sub_frame(&self.capture_block, &mut sf);
            #[allow(clippy::needless_range_loop, reason = "band 同时索引 out_bands 与 sf")]
            for band in 0..self.num_bands {
                self.out_bands[band][off..off + SUB].copy_from_slice(&sf[band][0]);
            }
        }
        // 余块：先处理，再只插不抽（这次插进去的子帧下一帧才吐出来）。
        if self.capture_blocker.is_block_available() {
            self.capture_blocker.extract_block(&mut self.capture_block);
            self.proc
                .process_capture(false, false, None, &mut self.capture_block);
            self.out_framer.insert_block(&self.capture_block);
        }
        merge_bands(&mut self.split, &self.out_bands, out);
    }
}

/// 全带 10 ms 帧 → `num_bands` × 160 样本。`split` 为 `None`（单频带）时是恒等拷贝。
fn split_bands(
    split: &mut Option<(ThreeBandFilterBank, ThreeBandFilterBank)>,
    frame: &[f32],
    bands: &mut [[f32; BAND_FRAME]; NUM_BANDS],
) {
    if let Some((ana, _)) = split {
        let mut tmp = [0.0f32; FULL_BAND_SIZE];
        tmp.copy_from_slice(frame);
        ana.analysis(&tmp, bands);
    } else {
        bands[0].copy_from_slice(frame);
    }
}

/// `num_bands` × 160 样本 → 全带 10 ms 帧。
fn merge_bands(
    split: &mut Option<(ThreeBandFilterBank, ThreeBandFilterBank)>,
    bands: &[[f32; BAND_FRAME]; NUM_BANDS],
    out: &mut [f32],
) {
    if let Some((_, syn)) = split {
        let mut tmp = [0.0f32; FULL_BAND_SIZE];
        syn.synthesis(bands, &mut tmp);
        out.copy_from_slice(&tmp[..out.len()]);
    } else {
        out.copy_from_slice(&bands[0][..out.len()]);
    }
}

// ---------------------------------------------------------------- 场景与度量

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 单讲：采集 = 回声。
    SingleTalk,
    /// 双讲：采集 = 回声 + 近端。
    DoubleTalk,
    /// 只有近端（无回声）——量"近端被吃掉多少"的对照组。
    NearOnly,
}

#[derive(Clone, Copy)]
struct Scenario {
    /// 采集率，也决定频带数（`rate/16000`）。
    rate: usize,
    delay_ms: usize,
    mode: Mode,
    /// `Some(24_000)` 时参考按 §5.4 用线性插值升到采集率。
    ref_rate: Option<usize>,
    /// `Some(ms)` = 走外部时延估计器并喂这个值。
    external_delay_ms: Option<i32>,
    /// 参考 / 采集**相对 int16 域**的额外衰减（dB）。幅度阶梯用它扫。
    attn_db: f64,
    /// 参考带限噪声的上下边带（Hz）。默认 (100, 0.45·rate) ≈ 全带。
    ref_hp_hz: f32,
    ref_lp_hz: f32,
}

impl Scenario {
    fn new(rate: usize, delay_ms: usize, mode: Mode) -> Self {
        Self {
            rate,
            delay_ms,
            mode,
            ref_rate: None,
            external_delay_ms: None,
            attn_db: 0.0,
            ref_hp_hz: 100.0,
            ref_lp_hz: 0.45 * rate as f32,
        }
    }
}

struct Outcome {
    erle_db: f64,
    /// 每 0.5 s 一个点的真 ERLE。
    curve: Vec<(f64, f64)>,
    converge_ms: Option<f64>,
    /// 近端保留 = 输出里近端那一份的能量 / 真近端能量（dB）。负 = 近端被吃。
    near_keep_db: f64,
    builtin_erl: f64,
    builtin_erle: f64,
    builtin_delay_ms: i32,
    /// 算子延迟：capture[n] 在 out[n+L] 出来。
    latency: usize,
    frame_us_median: f64,
    frame_us_max: f64,
}

/// 量算子延迟：渲染全零、只在采集的一个已知位置送一个冲激，看输出峰值落在哪。
/// 渲染静默时 AEC3 对这个冲激基本是直通（滤波器学到的是零），量到的是**结构延迟**。
/// ERLE 对这个数不敏感（±600 样本内摆动 < 1 dB，见 §3 的说明）。
fn probe_latency(rate: usize) -> usize {
    let frame = rate / 100;
    let n = rate * 3;
    let at = n / 2;
    let mut aec = BlockLevelAec::new(rate, None);
    let mut out = vec![0.0f32; n];
    let mut f_ref = vec![0.0f32; frame];
    let mut f_cap = vec![0.0f32; frame];
    let mut f_out = vec![0.0f32; frame];
    for t0 in (0..3_000usize).step_by(FRAME_MS) {
        let s = t0 * rate / 1000;
        f_ref.fill(0.0);
        for (i, x) in f_cap.iter_mut().enumerate() {
            *x = if s + i == at { S16_SCALE } else { 0.0 };
        }
        aec.push_render(&f_ref);
        aec.process_capture(&f_cap, &mut f_out);
        out[s..s + frame].copy_from_slice(&f_out);
    }
    out.iter()
        .enumerate()
        .max_by(|x, y| x.1.abs().partial_cmp(&y.1.abs()).unwrap())
        .map(|(i, _)| i.saturating_sub(at))
        .unwrap_or(0)
}

/// (标签, 结果) 一列。
type Rows = Vec<(String, Outcome)>;

fn run(scn: &Scenario, total_ms: usize, warm_ms: usize) -> Outcome {
    let rate = scn.rate;
    let frame = rate / 100;
    let n = rate * total_ms / 1000;
    let latency = probe_latency(rate);

    // ---- 参考（播放侧译音）。默认在采集率上合成；ref_rate 不同则先合再线性插值升上来。
    let ref_rate = scn.ref_rate.unwrap_or(rate);
    let mut reference = synth(
        ref_rate * total_ms / 1000 + 4 * frame,
        ref_rate,
        0x5eed_0001,
        scn.ref_hp_hz,
        scn.ref_lp_hz * ref_rate as f32 / rate as f32,
    );
    normalize_to_rms(&mut reference, 0.20);
    // 0.7 Hz 慢起伏，别让它像稳态信号（stationarity 门会卡住自适应）。
    for (i, x) in reference.iter_mut().enumerate() {
        let t = i as f32 / ref_rate as f32;
        *x *= 0.55 + 0.45 * (std::f32::consts::TAU * 0.7 * t).sin();
    }
    let mut reference = if ref_rate == rate {
        reference
    } else {
        upsample_linear(&reference, rate / ref_rate)
    };

    // ---- 近端（麦克风里的人声）：300 Hz–4 kHz 带限噪声，fixed seed。
    let mut near = synth(n, rate, 0x5eed_0002, 300.0, 4_000.0);
    normalize_to_rms(&mut near, (ECHO_GAIN * 0.20 * NEAR_GAIN) as f64);
    if scn.mode == Mode::SingleTalk {
        // 单讲：采集里没有近端。不把它当归一化的扣减项，否则"近端残留"会污染 ERLE。
        for x in near.iter_mut() {
            *x = 0.0;
        }
    }

    // ---- AEC3 的门限活在 int16 域：参考与近端都按 attn_db 拉过去。
    let attn = 10f64.powf(scn.attn_db / 20.0) as f32;
    for x in reference.iter_mut() {
        *x *= S16_SCALE * attn;
    }
    for x in near.iter_mut() {
        *x *= S16_SCALE * attn;
    }

    // ---- 回声路径：固定时延 + 固定增益（最简单也最"应该被消掉"的线性路径）。
    let d = rate * scn.delay_ms / 1000;
    let mut echo = vec![0.0f32; n];
    let mut capture = vec![0.0f32; n];
    for i in 0..n {
        let e = if i >= d {
            ECHO_GAIN * reference[i - d]
        } else {
            0.0
        };
        echo[i] = if scn.mode == Mode::NearOnly { 0.0 } else { e };
        capture[i] = match scn.mode {
            Mode::SingleTalk | Mode::DoubleTalk => e + near[i],
            Mode::NearOnly => near[i],
        };
    }

    let mut aec = BlockLevelAec::new(rate, scn.external_delay_ms);
    let mut out = vec![0.0f32; n];
    let mut frame_us: Vec<f64> = Vec::with_capacity(total_ms / FRAME_MS + 1);
    let mut f_ref = vec![0.0f32; frame];
    let mut f_cap = vec![0.0f32; frame];
    let mut f_out = vec![0.0f32; frame];
    for t0 in (0..total_ms).step_by(FRAME_MS) {
        let s = t0 * rate / 1000;
        f_ref.copy_from_slice(&reference[s..s + frame]);
        f_cap.copy_from_slice(&capture[s..s + frame]);
        let t = Instant::now();
        aec.push_render(&f_ref);
        aec.process_capture(&f_cap, &mut f_out);
        frame_us.push(t.elapsed().as_secs_f64() * 1e6);
        out[s..s + frame].copy_from_slice(&f_out);
    }

    // ---- 真值度量。out[i] 对应 capture[i − L]，于是 residual[i] = out[i] − near[i−L]。
    let periods = total_ms / 500;
    let mut p_echo = vec![0.0f64; periods];
    let mut p_res = vec![0.0f64; periods];
    let (mut echo_e, mut res_e, mut near_e, mut cross) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    #[allow(
        clippy::needless_range_loop,
        reason = "i 与 i-latency 两套下标，DSP 对齐偏移"
    )]
    for i in latency..n {
        let j = i - latency;
        let p = (j * 1000 / rate) / 500;
        if p >= periods {
            break;
        }
        let o = out[i] as f64;
        p_echo[p] += (echo[j] as f64).powi(2);
        p_res[p] += (o - near[j] as f64).powi(2);
        if (p + 1) * 500 >= warm_ms {
            echo_e += (echo[j] as f64).powi(2);
            res_e += (o - near[j] as f64).powi(2);
            near_e += (near[j] as f64).powi(2);
            cross += o * near[j] as f64;
        }
    }

    let mut curve = Vec::new();
    for p in 0..periods {
        if p_echo[p] > 0.0 && p_res[p] > 0.0 {
            curve.push(((p + 1) as f64 * 0.5, db(p_echo[p] / p_res[p])));
        }
    }
    let steady = db(echo_e / res_e.max(1e-30));
    let converge_ms = curve
        .iter()
        .copied()
        .find(|&(_, v)| v >= steady - 3.0)
        .map(|(t, _)| t * 1000.0);
    // 近端保留 = 输出里近端那一份的能量 / 真近端能量。
    // 最小二乘增益 g = Σ(out·near)/Σ(near²)，若输出 = g·near 则保留 = g²。
    let near_keep = if near_e > 0.0 {
        let g = cross / near_e;
        db(g * g)
    } else {
        f64::NAN
    };

    let mut sorted = frame_us;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (erl, erle, idelay) = aec.metrics();

    Outcome {
        erle_db: steady,
        curve,
        converge_ms,
        near_keep_db: near_keep,
        builtin_erl: erl,
        builtin_erle: erle,
        builtin_delay_ms: idelay,
        latency,
        frame_us_median: sorted[sorted.len() / 2],
        frame_us_max: sorted[sorted.len() - 1],
    }
}

// ---------------------------------------------------------------- 各节

fn m(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.0} ms"),
        None => "未收敛".to_string(),
    }
}

/// ① 幅度标尺阶梯：AEC3 的门限是 int16 域的，那么至少要喂多响？
fn amplitude_ladder() {
    println!("\n== ① 幅度标尺阶梯（16 kHz 单频带、D=40 ms、内建时延估计）==");
    println!(
        "  poor_excitation_render_limit = 150 → matched filter 门限 = 512 × 150² = {:.3e}",
        512.0 * 150.0 * 150.0
    );
    println!(
        "  {:>8} {:>12} {:>10} {:>10} {:>9}",
        "衰减dB", "render RMS", "真ERLE", "内置ERLE", "内置时延"
    );
    for attn in [0.0f64, -6.0, -12.0, -18.0, -24.0, -30.0, -40.0, -60.0] {
        let scn = Scenario {
            attn_db: attn,
            ..Scenario::new(16_000, 40, Mode::SingleTalk)
        };
        let o = run(&scn, 8000, 4000);
        println!(
            "  {:>8.0} {:>12.2} {:>8.2}dB {:>8.2}dB {:>7}ms",
            attn,
            0.20 * 32768.0 * 10f64.powf(attn / 20.0),
            o.erle_db,
            o.builtin_erle,
            o.builtin_delay_ms
        );
    }
}

/// ② 主表：真产品的形状（48 k 三频带）与决定性对照（16 k 单频带）。
fn main_table() -> (Rows, Rows) {
    let mut out16 = Vec::new();
    let mut out48 = Vec::new();

    for (label, rate) in [("16k", 16_000usize), ("48k", 48_000)] {
        let mut cases = Vec::new();
        for &d in &[40usize, 120] {
            let base = Scenario::new(rate, d, Mode::SingleTalk);
            cases.push((format!("{label} D={d}ms 单讲"), Scenario { ..base }));
            cases.push((
                format!("{label} D={d}ms 双讲"),
                Scenario {
                    mode: Mode::DoubleTalk,
                    ..base
                },
            ));
            cases.push((
                format!("{label} D={d}ms 外部时延"),
                Scenario {
                    external_delay_ms: Some(d as i32),
                    ..base
                },
            ));
        }
        // 近端对照组（无回声）
        cases.push((
            format!("{label} D=40ms 只有近端"),
            Scenario {
                mode: Mode::NearOnly,
                ..Scenario::new(rate, 40, Mode::SingleTalk)
            },
        ));
        // §5.4 的 24 k 参考（线性插值升上来）——只在 48 k 测
        if rate == 48_000 {
            for &d in &[40usize, 120] {
                cases.push((
                    format!("{label} D={d}ms 参考24k线性升"),
                    Scenario {
                        ref_rate: Some(24_000),
                        ..Scenario::new(rate, d, Mode::SingleTalk)
                    },
                ));
            }
        }

        let results: Vec<(String, Outcome)> = cases
            .into_iter()
            .map(|(name, scn)| (name, run(&scn, 16_000, 8_000)))
            .collect();
        if rate == 16_000 {
            out16 = results;
        } else {
            out48 = results;
        }
    }
    (out16, out48)
}

fn print_table(rows: &[(String, Outcome)]) {
    println!(
        "  {:<22} {:>9} {:>9} {:>10} {:>9} {:>9} {:>9}",
        "场景", "真ERLE", "收敛", "算子延迟", "近端保留", "内置ERL", "内置时延"
    );
    for (name, r) in rows {
        println!(
            "  {:<22} {:>6.2}dB {:>9} {:>8}ms {:>8.1}dB {:>6.1}dB {:>7}ms",
            name,
            r.erle_db,
            m(r.converge_ms),
            r.latency,
            r.near_keep_db,
            r.builtin_erl,
            r.builtin_delay_ms
        );
    }
}

fn print_cpu(rows: &[(String, Outcome)]) {
    println!("\n== ④ CPU（x86-64，每 10 ms 一拍的墙钟；预算 10000 µs）==");
    println!(
        "  {:<22} {:>12} {:>12} {:>10}",
        "场景", "中位µs", "最大µs", "占用%"
    );
    for (name, r) in rows {
        println!(
            "  {:<22} {:>12.1} {:>12.1} {:>9.2}",
            name,
            r.frame_us_median,
            r.frame_us_max,
            r.frame_us_median / 10_000.0 * 100.0
        );
    }
}

/// ③/⑤ 时延扫描：AEC3 的时延搜索窗口到底盖多少毫秒（设计稿 §5.3 / R5）。
fn delay_sweep(rate: usize, external: bool, step: usize) {
    println!(
        "\n== ⑤ {rate} Hz 时延搜索窗口扫描（{}，步长 {step} ms）==",
        if external {
            "外部时延估计器"
        } else {
            "内建时延估计器"
        }
    );
    println!(
        "  {:>7} {:>10} {:>10} {:>10}",
        "D(ms)", "真ERLE", "内置时延", "算子延迟"
    );
    for d in (0..=400).step_by(step) {
        let scn = Scenario {
            external_delay_ms: external.then_some(d as i32),
            ..Scenario::new(rate, d, Mode::SingleTalk)
        };
        let o = run(&scn, 6000, 3000);
        println!(
            "  {:>7} {:>7.2}dB {:>7}ms {:>7}ms",
            d,
            o.erle_db,
            o.builtin_delay_ms,
            o.latency * 1000 / rate
        );
    }
}

/// 48 k 三频带滤波器组的往返质量——解释"3 频带为什么只有 25 dB"。
fn filterbank_roundtrip() {
    println!("\n== ⑦ 48 k 三频带滤波器组往返质量（白噪声 200 帧）==");
    let mut r = Rng::new(0xa11ce);
    let frames = 200usize;
    let x: Vec<f32> = (0..frames * FULL_BAND_SIZE)
        .map(|_| r.next_f32() * 0.5)
        .collect();
    let mut ana = ThreeBandFilterBank::new();
    let mut syn = ThreeBandFilterBank::new();
    let mut out = vec![0.0f32; frames * FULL_BAND_SIZE];
    for f in 0..frames {
        let input: &[f32; FULL_BAND_SIZE] = x[f * FULL_BAND_SIZE..(f + 1) * FULL_BAND_SIZE]
            .try_into()
            .unwrap();
        let mut bands = [[0.0f32; SPLIT_BAND_SIZE]; NUM_BANDS];
        ana.analysis(input, &mut bands);
        let mut rec = [0.0f32; FULL_BAND_SIZE];
        syn.synthesis(&bands, &mut rec);
        out[f * FULL_BAND_SIZE..(f + 1) * FULL_BAND_SIZE].copy_from_slice(&rec);
    }
    let from = 40 * FULL_BAND_SIZE;
    let search = 4800usize;
    let mut best = (0usize, f64::MIN);
    for lag in 0..search {
        let mut acc = 0.0f64;
        for i in from..x.len() - search {
            acc += out[i + lag] as f64 * x[i] as f64;
        }
        if acc > best.1 {
            best = (lag, acc);
        }
    }
    let (mut se, mut sn) = (0.0f64, 0.0f64);
    for i in from..x.len() - search {
        se += (out[i + best.0] as f64 - x[i] as f64).powi(2);
        sn += (x[i] as f64).powi(2);
    }
    println!(
        "  往返最佳滞后 {} 样本（{:.2} ms），往返 SNR = {:.2} dB",
        best.0,
        best.0 as f64 / 48.0,
        10.0 * (sn / se).log10()
    );
}

/// ⑥ 诊断：3 频带的 ERLE 天花板是"频带数"还是"参考带宽"造成的？
/// 把参考分别压在 band0 / band1 / band2 里单独量。32 kHz（2 频带）**没法测**：
/// 2 频带的 QMF 分频器在父 crate 里是 `pub(crate)`，公开面只有 3 频带那一只。
fn band_diag() {
    println!("\n== ⑥ 频带数 / 参考带宽 对照（单讲）==");
    println!("  {:<26} {:>9} {:>9}", "形状", "D=40ms", "D=120ms");
    let shapes: [(&str, usize, (f32, f32)); 8] = [
        ("16k 1 频带 0-7kHz", 16_000, (100.0, 7000.0)),
        ("16k 1 频带 0-4kHz", 16_000, (100.0, 4000.0)),
        ("16k 1 频带 0-2kHz", 16_000, (100.0, 2000.0)),
        ("48k 3 频带 0-21.6kHz", 48_000, (100.0, 21_600.0)),
        ("48k 只有 band0 0-4kHz", 48_000, (100.0, 4000.0)),
        ("48k 只有 band0 0-8kHz", 48_000, (100.0, 7900.0)),
        ("48k 只有 band1 8-16kHz", 48_000, (8100.0, 15_900.0)),
        ("48k 只有 band2 16-21kHz", 48_000, (16_500.0, 21_000.0)),
    ];
    for (label, rate, (hp, lp)) in shapes {
        let mut cells = Vec::new();
        for &d in &[40usize, 120] {
            let scn = Scenario {
                ref_hp_hz: hp,
                ref_lp_hz: lp,
                ..Scenario::new(rate, d, Mode::SingleTalk)
            };
            let o = run(&scn, 8000, 4000);
            cells.push(format!("{:>6.2}dB", o.erle_db));
        }
        println!("  {label:<26} {:>9} {:>9}", cells[0], cells[1]);
    }
}

fn main() {
    println!("== S4-C W0 尖峰 · sonora-aec3 0.2.0（块级路径）==");
    println!(
        "常数：频带帧={BAND_FRAME} 子帧={SUB} 块={BLK} 每块={BLOCK_MS}ms 最大频带数={NUM_BANDS}"
    );
    println!(
        "回声耦合 {ECHO_GAIN:.3}（−6 dB）；参考=100Hz–0.45·rate 带限噪声，近端=300Hz–4kHz 带限噪声"
    );

    let lat16 = probe_latency(16_000);
    let lat48 = probe_latency(48_000);
    println!(
        "算子延迟（冲激探针）：16 kHz = {lat16} 样本（{} ms），48 kHz = {lat48} 样本（{} ms）",
        lat16 * 1000 / 16_000,
        lat48 * 1000 / 48_000
    );

    amplitude_ladder();

    println!("\n== ② 主表：16 kHz 单频带（决定性对照） ==");
    let (out16, out48) = main_table();
    print_table(&out16);
    println!("\n== ② 主表：48 kHz 三频带（真产品形状） ==");
    print_table(&out48);

    println!("\n== ③ 真 ERLE 曲线（0.5 s 一点）==");
    for rows in [&out16, &out48] {
        for (name, r) in rows {
            if !name.contains("单讲") || !name.contains("D=40ms") {
                continue;
            }
            let s: Vec<String> = r
                .curve
                .iter()
                .map(|(t, v)| format!("{t:.1}s:{v:.0}"))
                .collect();
            println!("  {name}: {}", s.join(" "));
        }
    }

    print_cpu(&out16);
    print_cpu(&out48);

    delay_sweep(16_000, false, 20);
    delay_sweep(48_000, false, 20);
    delay_sweep(48_000, true, 20);

    band_diag();
    filterbank_roundtrip();

    println!("\n== 结论（W1 必须照抄的五条）==");
    println!("  1. 调用序列 = analysis/synthesis + FrameBlocker + BlockProcessor + BlockFramer，");
    println!("     每 4 次子帧插入必须把余块抽干，否则 panic。32 kHz（2 频带）公开面驱不动。");
    println!(
        "  2. 输入是 int16 域（±32768）：门限 512×150²=1.152e7，[-1,1] 的 f32 永远估不出时延。"
    );
    println!(
        "  3. 48 kHz 三频带 ≈ 25 dB（D 越大越差，400 ms 只剩 12 dB）；16 kHz 单频带 ≈ 62–72 dB。"
    );
    println!("     3 频带本身就值 ~46 dB 的损失，与参考落在哪个频带无关（见 ⑥）。");
    println!("  4. 双讲会实打实吃掉近端：近端比回声低 14 dB 时，输出的近端低 ~10 dB；");
    println!("     但没有回声那一档近端保留 -0.0 dB（不被误伤）。收敛 ≤ 1 s。");
    println!("  5. CPU：16k ≈ 29 µs/10 ms、48k ≈ 53 µs/10 ms（x86-64，0.29%–0.53% 单核）；");
    println!("     外部时延估计器便宜一倍（15 µs / 39 µs）。");
}
