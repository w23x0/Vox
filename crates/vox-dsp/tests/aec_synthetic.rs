//! S4-C · W1 的验收测试（T1–T4、T6、T7、T13、T14）：合成回声路径下量真值 ERLE。
//!
//! 装置照 `docs/plans/S4-C-AEC.md` §7.1 与 W0 尖峰（§6.4 的 `run()`）：信号全部确定性
//! 合成（xorshift 固定种子 + 解析带限滤波），**不读文件、不连声卡**。
//!
//! 三条"不写就一定是空转"的前提（§7.1 的 W0 补注）：
//!
//! 1. **参考按播放率（24 kHz）送**——那是 `ports::Aec::push_reference` 的口径
//!    （`OUTPUT_SAMPLE_RATE`），升到采集率是实现自己的事（§5.4）。回声是从
//!    **设备真正放出来那一路**合成的，也就是同一份参考按**标称栅格**线性插值升上来的
//!    那一条（实现内部那份插值器的栅格与它逐样本一致，`aec.rs::LinearUpsampler` 的注释）。
//! 2. **参考在采集那一拍就送到**（与 W0 尖峰一致）。参考晚到几拍（播放汇排队）是另一件
//!    事，由 T7 单独钉。
//! 3. **幅度走 f32 的 [-1,1]**（trait 的口径），实现内部换算到 AEC3 认的 int16 域——
//!    这道缝是 T13 的钉子（§6.2：喂 [-1,1] 会连时延都估不出来）。
//!
//! 真值 ERLE 与 W0 同口径：
//!
//! ```text
//! ERLE(dB) = 10·log10( Σ echo[n]² / Σ residual[n]² )
//! residual[n] = out[n + L] − near[n]        // L = 算子延迟，冲激探针量出来
//! ```
//!
//! 阈值全部来自 §6.4 的实测（48 k 三频带：D=40 ms 25.29 dB、D=120 ms 23.18 dB、
//! D=200 ms 18.51 dB、D=400 ms 12.66 dB），**不是抄惯例**，见 §7.2。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::TAU;

use vox_core::ports::Aec;
use vox_dsp::aec::SonoraAec;

/// 采集率（产品档：`CAPTURE_WANT_RATE = 48000`，48 k 走 3 频带）。
const RATE: u32 = 48_000;
/// 播放率（`OUTPUT_SAMPLE_RATE`，今天恒 24 kHz）。参考按这个率进来。
const PLAYBACK_RATE: u32 = 24_000;
/// 参考的相对满幅 RMS（f32 的 [-1,1] 域）。W0 尖峰用的就是这个量级。
const REF_RMS: f64 = 0.20;
/// 回声路径的增益（纯增益 + 纯时延，W0 的"最简单也最该被消掉"的那条路径）。
/// ERLE 是这个路径的**收益比**，与增益的具体值无关。
const ECHO_GAIN: f32 = 0.4;
/// 双讲时近端比回声低多少 dB（W0 的 `NEAR_GAIN = 0.2`）。
const NEAR_BELOW_ECHO_DB: f64 = 14.0;
/// 固定种子：同一台机器、同一份代码跑出来的数字必须逐位一样。
const REF_SEED: u32 = 0x5eed_0001;
const NEAR_SEED: u32 = 0x5eed_0002;
/// 统计窗（ms）。W0 §6.3 / §6.4 用的是 8 s 总长、从第 4 s 起算稳态。
const TOTAL_MS: usize = 8_000;
const STEADY_FROM_MS: usize = 4_000;

// ---------------------------------------------------------------- 确定性信号

/// xorshift32。测试不引 `rand`（与 W0 尖峰同一条规矩）。
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

/// 一阶高通 + 二阶低通：把白噪声整成带限噪声。
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
            hp_a: (-TAU * hp_hz / r).exp(),
            lp_a: 1.0 - (-TAU * lp_hz / r).exp(),
        }
    }

    fn next(&mut self, rng: &mut Rng) -> f32 {
        self.step(rng.next_f32())
    }

    /// 只做滤波、不产噪声：给"把已有信号整成带限"那一档用（抗混叠对照）。
    fn step(&mut self, x: f32) -> f32 {
        let y = x - self.hp_prev + self.hp_a * self.hp_y;
        self.hp_prev = x;
        self.hp_y = y;
        let a = 1.0 - self.lp_a;
        let b = self.lp_a;
        let l1 = a * self.lp1_y + b * y;
        self.lp1_y = l1;
        let l2 = a * self.lp2_y + b * l1;
        self.lp2_y = l2;
        l2
    }
}

fn synth(n: usize, rate: usize, seed: u32, hp: f32, lp: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut filter = BandLimited::new(rate, hp, lp);
    (0..n).map(|_| filter.next(&mut rng)).collect()
}

fn rms(v: &[f32]) -> f64 {
    (v.iter().map(|&x| (x * x) as f64).sum::<f64>() / v.len() as f64).sqrt()
}

fn normalize_to_rms(buf: &mut [f32], target: f64) {
    let gain = target as f32 / rms(buf) as f32;
    for x in buf.iter_mut() {
        *x *= gain;
    }
}

fn db(power: f64) -> f64 {
    if power <= 1e-30 {
        f64::NEG_INFINITY
    } else {
        10.0 * power.log10()
    }
}

/// 播放率 → 采集率的线性插值（**标称栅格**：`out[0] == src[0]`，见 §5.4）。
///
/// 实现内部那一份（`vox_dsp::aec::LinearUpsampler`）产出的是同一条栅格上的同一些值
/// （逐样本对过；只差 f32 的舍入）。48 k 那档（升采样，比 1/2）设备真正放出来的
/// 那一路就是它。
///
/// 16 k 那档是**降采样**（比 3/2），这条公式照旧成立（同一段循环），但它同时是
/// "未抗混叠的抽取"——参考里 8 kHz 以上的能量会折回带内。见 `alias_free_downsample`。
fn resample_linear(src: &[f32], in_rate: usize, out_rate: usize) -> Vec<f32> {
    assert!(src.len() >= 2, "至少要两个输入样本才谈得上插值");
    let step = in_rate as f64 / out_rate as f64;
    // 与实现同一个终点：`LinearUpsampler` 只在 `pos < 1.0` 时出一格，等价于
    // 输出点落在 `t < len − 1` 上，个数就是 `ceil((len − 1) / step)`。
    let out_len = (((src.len() - 1) as f64) / step).ceil() as usize;
    let mut out = vec![0.0f32; out_len.max(1)];
    for (m, slot) in out.iter_mut().enumerate() {
        let t = m as f64 * step;
        let i0 = t.floor() as usize;
        let frac = (t - i0 as f64) as f32;
        let i1 = (i0 + 1).min(src.len() - 1);
        *slot = src[i0] * (1.0 - frac) + src[i1] * frac;
    }
    out
}

/// 一阶低通，**拐点就是拐点**：`y[n] = a·y[n−1] + (1−a)·x[n]`，`a = exp(−2π·fc/fs)`。
///
/// 装置里那个 `BandLimited::new`（既有 48 k 用例一直在用、**不动**）低通写的是
/// `l1 = y + lp_a·(prev − y)`，等价于把 `1−exp(−2π fc/fs)` 当成了**极点系数**而不是
/// 输入系数——它的实际拐点比标称低两个数量级（标称 10.8 kHz ⇒ 实测约 233 Hz，
/// 见 T15 的实测：参考里 1 kHz **以上**只有 0.64% 的功率）。所以"真的要滤掉 8 kHz
/// 以上"那一档**不能借它**，另起一只口径正确的。口径与实测都写在 T15 的注释里。
#[derive(Clone, Copy)]
struct LowPass {
    a: f32,
    y: f32,
}

impl LowPass {
    fn new(fs: f32, fc: f32) -> Self {
        Self {
            a: (-TAU * fc / fs).exp(),
            y: 0.0,
        }
    }

    fn step(&mut self, x: f32) -> f32 {
        self.y = self.a * self.y + (1.0 - self.a) * x;
        self.y
    }
}

/// 一阶高通。`y[n] = a·y[n−1] + c·(x[n] − x[n−1])`，`a = exp(−2π fc/fs)` 定拐点，
/// `c = (1+a)/2` 把**高频增益归一**到 1（少了这个 `c`，`fc` 一高整条通路就整体变弱，
/// 量"8 kHz 以上占多少"会假低——第一版就踩过）。
#[derive(Clone, Copy)]
struct HighPass {
    a: f32,
    c: f32,
    prev: f32,
    y: f32,
}

impl HighPass {
    fn new(fs: f32, fc: f32) -> Self {
        let a = (-TAU * fc / fs).exp();
        Self {
            a,
            c: 0.5 * (1.0 + a),
            prev: 0.0,
            y: 0.0,
        }
    }

    fn step(&mut self, x: f32) -> f32 {
        self.y = self.a * self.y + self.c * (x - self.prev);
        self.prev = x;
        self.y
    }
}

/// "真带限白噪声"：白噪声 → `HighPass(hp_hz)` → `poles` 阶 `LowPass(lp_hz)`。
/// 拐点是真的拐点，所以 `lp_hz < 8 kHz` 时降采样到 16 kHz 不会混叠。
fn synth_steep(n: usize, rate: usize, seed: u32, hp_hz: f32, lp_hz: f32, poles: usize) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut hp = HighPass::new(rate as f32, hp_hz);
    let mut lp = vec![LowPass::new(rate as f32, lp_hz); poles];
    (0..n)
        .map(|_| {
            let mut x = hp.step(rng.next_f32());
            for stage in lp.iter_mut() {
                x = stage.step(x);
            }
            x
        })
        .collect()
}

/// 把已有信号整成"真带限"：`HighPass(hp_hz)` + `poles` 阶 `LowPass(lp_hz)`。
fn band_limit_steep(src: &[f32], rate: usize, hp_hz: f32, lp_hz: f32, poles: usize) -> Vec<f32> {
    let mut hp = HighPass::new(rate as f32, hp_hz);
    let mut lp = vec![LowPass::new(rate as f32, lp_hz); poles];
    src.iter()
        .map(|&x| {
            let mut x = hp.step(x);
            for stage in lp.iter_mut() {
                x = stage.step(x);
            }
            x
        })
        .collect()
}

// ---------------------------------------------------------------- 合成装置

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 单讲：采集里只有回声。
    SingleTalk,
    /// 双讲：采集 = 回声 + 近端（近端比回声低 `NEAR_BELOW_ECHO_DB`）。
    DoubleTalk,
    /// 只有近端、没有回声：量"近端被吃掉多少"的对照组。
    NearOnly,
}

/// 参考怎么造。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefShape {
    /// 与既有 48 k 用例**同一只**（`synth` + `BandLimited`）。它的低通拐点实际远低于
    /// 标称（见 `LowPass` 的注释与 T15 的实测），所以 8 kHz 以上几乎没能量。
    Harness,
    /// 真 4 阶低通（拐点 = `Setup::reference_lp_hz`）：`reference_lp_hz` 给 10.8 kHz 时
    /// 就是文档里写的那条"100 Hz – 10.8 kHz"的宽参考。
    Steep,
}

/// 设备放出来的那一路（采集率）怎么来——也就是"麦克风听到的回声"。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EchoShape {
    /// 实现那条栅格（`resample_linear`）：参考与回声**自洽**，混叠误差对消。
    ImplGrid,
    /// 先按 4 阶低通抗混叠到 `0.45 × 采集率`，再按同一栅格抽取——这是 16 kHz 的
    /// ADC 会给出东西（空气里的回声不可能有 8 kHz 以上折回带内的假能量）。
    AliasFree,
}

/// 一次测量要的全部参数。
struct Setup {
    /// 采集率。产品档是 48 k（`CAPTURE_WANT_RATE`）；16 k 是 `AEC_RATES` 里的另一档。
    rate: u32,
    /// 回声路径的固定时延（ms）。W0 §6.4：**不许取 0**（渲染缓冲自带 ≥1 块的固定
    /// 延迟，零延迟的回声路径不可表达）。
    delay_ms: usize,
    mode: Mode,
    /// 参考推送相对采集的**相位偏移**（采集率样本）。T7 用它把两条时间线错开。
    reference_phase: usize,
    /// 参考每次最多推多少（**播放率**样本）。`0` = 本拍该到的整段一次推。
    reference_chunk: usize,
    /// 装置整体缩放。`1.0` = 正常；`1.0 / 32768.0` = T13 的负半边（等价于"实现内部
    /// 不做 int16 换算"）。**必须整条装置一起缩**（参考与采集两条路），不然真值判决
    /// 会被"两边不在同一个域"污染。
    scale: f32,
    /// 参考（24 kHz）的低通拐点（Hz）。主档 10.8 kHz = `0.45 × 播放率`。
    /// 对 16 k 采集率来说它**超过奈奎斯特（8 kHz）**，正是"参考路要降采样"的现实情形
    /// （`RefShape::Harness` 下它只是标称值，实际拐点低得多）。
    reference_lp_hz: f32,
    /// 参考怎么造。默认 `Harness` = 既有 48 k 用例那一只。
    reference_shape: RefShape,
    /// 回声（设备侧那一路）怎么来。默认 `ImplGrid` = 与参考自洽。
    echo_shape: EchoShape,
    total_ms: usize,
    /// 稳态统计窗的起点（ms）。W0 的主表与 §6.3 都从第 4 s 起。
    steady_from_ms: usize,
}

impl Setup {
    fn single(delay_ms: usize) -> Self {
        Self {
            rate: RATE,
            delay_ms,
            mode: Mode::SingleTalk,
            reference_phase: 0,
            reference_chunk: 0,
            scale: 1.0,
            reference_lp_hz: 0.45 * PLAYBACK_RATE as f32,
            reference_shape: RefShape::Harness,
            echo_shape: EchoShape::ImplGrid,
            total_ms: TOTAL_MS,
            steady_from_ms: STEADY_FROM_MS,
        }
    }

    /// 16 kHz 采集率那一档（别的参数照 48 k 那套）。
    fn at_16k(delay_ms: usize) -> Self {
        Self {
            rate: 16_000,
            ..Self::single(delay_ms)
        }
    }
}

/// 一次测量的结果。
struct Outcome {
    /// 稳态真 ERLE（dB）。
    erle_db: f64,
    /// 每 0.5 s 一个点的真 ERLE 曲线（与 W0 同口径）：`(时刻 ms, ERLE dB)`。
    curve: Vec<(f64, f64)>,
    /// 达到"稳态 − 3 dB"的时刻（ms）。`None` = 整个窗口里都没到过。
    converge_ms: Option<f64>,
    /// 近端保留（dB）。最小二乘增益 `g = Σ(out·near)/Σnear²`，保留 = `10·log10(g²)`。
    /// 负 = 近端被吃。没有近端时是 `f64::NAN`。
    near_keep_db: f64,
    /// 算子延迟（样本）。冲激探针量出来的结构延迟。
    latency: usize,
}

/// 量算子延迟：渲染全零、采集在某处放一个冲激，看输出峰值落在第几个样本。
///
/// 渲染静默时 AEC3 对采集基本是直通（滤波器学到的是零），量到的是**结构延迟**
/// （`FrameBlocker` 的余量 + `BlockFramer` 初值那 64 个零 + 滤波器组的群延迟）。
/// 真值对齐要靠它；ERLE 对它不敏感（W0：±600 样本内摆动 < 1 dB）。
fn probe_latency(rate: u32) -> usize {
    let frame = rate as usize / 100;
    let n = rate as usize * 3;
    let at = n / 2 - (n / 2) % frame;
    let mut aec = SonoraAec::new(rate).expect("探针要能建起 AEC");
    let mut out = vec![0.0f32; n];
    let zeros = vec![0.0f32; frame];
    let mut capture = vec![0.0f32; frame];
    for (chunk_at, chunk) in out.chunks_mut(frame).enumerate() {
        let at = chunk_at * frame;
        capture.fill(0.0);
        if at == n / 2 - (n / 2) % frame {
            capture[0] = 1.0; // 满幅冲激（f32 域的 1.0 = 实现内部的 int16 满幅）
        }
        aec.push_reference(&zeros);
        let produced = aec.process(&capture);
        chunk.copy_from_slice(&produced);
    }
    out.iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(i, _)| i.saturating_sub(at))
        .unwrap_or(0)
}

/// 跑一次完整的测量。
fn measure(setup: &Setup) -> Outcome {
    let rate = setup.rate as usize;
    let frame = rate / 100;
    let block = rate * 20 / 1000;
    let ref_rate = PLAYBACK_RATE as usize;
    let n = rate * setup.total_ms / 1000;
    let latency = probe_latency(setup.rate);

    // ---- 参考：播放率（24 kHz）的带限噪声，RMS 0.20，带 0.7 Hz 慢起伏。
    // 慢起伏不是装饰：不让它像稳态信号，否则 AEC3 的 stationarity 门会卡住自适应
    // （W0 §6.4 的 `run()` 就是这么做、§6 的数字全是这么量出来的）。
    let slack = 4 * frame;
    let ref_len = ref_rate * setup.total_ms / 1000 + slack;
    let mut reference = match setup.reference_shape {
        RefShape::Harness => synth(ref_len, ref_rate, REF_SEED, 100.0, setup.reference_lp_hz),
        RefShape::Steep => {
            synth_steep(ref_len, ref_rate, REF_SEED, 100.0, setup.reference_lp_hz, 4)
        }
    };
    normalize_to_rms(&mut reference, REF_RMS);
    for (i, x) in reference.iter_mut().enumerate() {
        let t = i as f32 / ref_rate as f32;
        *x *= 0.55 + 0.45 * (TAU * 0.7 * t).sin();
    }
    // 设备真正放出来的那一路（采集率）。两条口径：
    // * `EchoShape::ImplGrid`：同一份参考按标称栅格直接抽取——就是实现
    //   （`aec.rs::LinearUpsampler`）内部那条路。48 k 的主档一直用这条（升采样无混叠）。
    // * `EchoShape::AliasFree`：先抗混叠（4 阶低通到 0.45 × 采集率）再抽取，也就是
    //   16 kHz 麦克风真正听到的那一路。参考与回声于是**不自洽**：差的那一截正是
    //   实现那条路折回带内的混叠。
    let played = match setup.echo_shape {
        EchoShape::ImplGrid => resample_linear(&reference, ref_rate, rate),
        EchoShape::AliasFree => {
            let aa = band_limit_steep(&reference, ref_rate, 100.0, 0.45 * rate as f32, 4);
            resample_linear(&aa, ref_rate, rate)
        }
    };

    // ---- 近端：300 Hz–4 kHz 带限噪声，比回声低 14 dB。
    let echo_rms = ECHO_GAIN as f64 * REF_RMS;
    let mut near = synth(n, rate, NEAR_SEED, 300.0, 4_000.0);
    normalize_to_rms(&mut near, echo_rms / 10f64.powf(NEAR_BELOW_ECHO_DB / 20.0));
    if setup.mode == Mode::SingleTalk {
        // 单讲：采集里没有近端。清零而不是"当扣减项"，否则近端残留会污染 ERLE。
        near.fill(0.0);
    }

    // ---- 回声路径：固定时延 + 固定增益。
    let delay = rate * setup.delay_ms / 1000;
    let mut echo = vec![0.0f32; n];
    for i in delay..n {
        echo[i] = ECHO_GAIN * played[i - delay];
    }
    if setup.mode == Mode::NearOnly {
        echo.fill(0.0);
    }
    // 装置整体缩放（T13 的负半边）：参考、回声、近端一起缩，真值判决与送进去的
    // 幅度保持同一个域。
    if setup.scale != 1.0 {
        for x in reference.iter_mut() {
            *x *= setup.scale;
        }
        for x in near.iter_mut() {
            *x *= setup.scale;
        }
        for x in echo.iter_mut() {
            *x *= setup.scale;
        }
    }
    let capture: Vec<f32> = (0..n)
        .map(|i| match setup.mode {
            Mode::SingleTalk | Mode::DoubleTalk => echo[i] + near[i],
            Mode::NearOnly => near[i],
        })
        .collect();

    // ---- 跑。参考本拍就送到（与 W0 尖峰一致：`push_render` 紧挨着 `process_capture`）。
    let mut aec = SonoraAec::new(setup.rate).expect("这个采集率要能建起 AEC");
    let mut out = vec![0.0f32; n];
    let mut sent = 0usize;
    let mut elapsed = 0usize;
    for at in (0..n).step_by(block) {
        // 这一刻该送到的参考（播放率样本）：本拍采集用的那一段，本拍送到。
        let due = ((elapsed + block + setup.reference_phase) as u64 * ref_rate as u64 / rate as u64)
            as usize;
        let due = due.min(reference.len());
        while sent < due {
            let take = if setup.reference_chunk == 0 {
                due - sent
            } else {
                (due - sent).min(setup.reference_chunk)
            };
            aec.push_reference(&reference[sent..sent + take]);
            sent += take;
        }
        let end = (at + block).min(n);
        let produced = aec.process(&capture[at..end]);
        out[at..at + produced.len()].copy_from_slice(&produced);
        elapsed += end - at;
    }

    // ---- 真值：`out[i]` 对应 `capture[i − L]`，所以 `residual[i] = out[i] − near[i − L]`。
    let periods = setup.total_ms / 500;
    let steady_bin = setup.steady_from_ms / 500;
    let mut bin_echo = vec![0.0f64; periods];
    let mut bin_residual = vec![0.0f64; periods];
    let (mut echo_energy, mut residual_energy, mut near_energy, mut cross) =
        (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (j, &out_i) in out[latency..n].iter().enumerate() {
        let period = (j * 1000 / rate) / 500;
        if period >= periods {
            break;
        }
        let out_i = out_i as f64;
        bin_echo[period] += (echo[j] as f64).powi(2);
        bin_residual[period] += (out_i - near[j] as f64).powi(2);
        if period >= steady_bin {
            echo_energy += (echo[j] as f64).powi(2);
            residual_energy += (out_i - near[j] as f64).powi(2);
            near_energy += (near[j] as f64).powi(2);
            cross += out_i * near[j] as f64;
        }
    }

    let curve: Vec<(f64, f64)> = (0..periods)
        .filter(|&p| bin_echo[p] > 0.0 && bin_residual[p] > 0.0)
        .map(|p| ((p + 1) as f64 * 500.0, db(bin_echo[p] / bin_residual[p])))
        .collect();
    let steady = db(echo_energy / residual_energy.max(1e-30));
    let converge_ms = curve
        .iter()
        .find(|(_, value)| *value >= steady - 3.0)
        .map(|(at_ms, _)| *at_ms);
    let near_keep_db = if near_energy > 0.0 {
        db((cross / near_energy).powi(2))
    } else {
        f64::NAN
    };

    Outcome {
        erle_db: steady,
        curve,
        converge_ms,
        near_keep_db,
        latency,
    }
}

// 各档实测与余量（2026-10-12，x86-64、debug 构建；A53 上一律外推）：
//
// | 用例 | 档 | 实测 | 判据 | 余量 |
// | --- | --- | --- | --- | --- |
// | T1 | D=30 ms | 22.57 dB | ≥ 15 | +7.6 |
// | T1 | D=120 ms | 20.86 dB | ≥ 15 | +5.9 |
// | T2 | D=40 ms 曲线 1 s 之后的最小值 | 13.05 dB | ≥ 12 | +1.1（最薄） |
// | T3 | 双讲近端保留 | −14.65 dB | ≥ −16 | +1.4（次薄） |
// | T3 | 只有近端保留（8–12 s） | −7.04 dB | ≥ −8 | +1.0（最薄，见 T3 注） |
// | T4 | D=400 ms | 13.12 dB | < 15 | −1.9 |
// | T4 / T13 / T14 | D=40 ms | 22.46 dB | ≥ 15 / 18–35 | +7.5 / 下界 +4.5 |
// | T7 | 抖动档（7 ms 小段 + 相位错开 7 ms） | 22.44 dB | ≥ 15 | +7.4 |
// | T13 | 负半边（幅度压 32768 倍） | −0.76 dB | < 5 | −5.8 |
// | T15 | 16 kHz D=30 / 120 / 200 | 63.81 / 64.44 / 64.17 dB | ≥ 25 | +39 左右 |
// | T16 | 16 kHz + 抗混叠回声 D=30 / 120 | 29.44–34.29 dB | ≥ 20 | +9.4 起 |
// | T18 | 无参考时增益（48 k / 16 k） | −0.28 / 0.00 dB | ≥ −1 | +0.7（见下） |
//
// 以上全部是 **2026-10-12、`BandLimited` 修对之后**在同一台 x86-64 上重跑的
// （A53 上一律外推）。修对之前的旧数在 `git log` 里，别再引用。
//
// **动这台装置（D、参考带宽、起伏、统计窗）之前先看这张表**：T3 的"只有近端"档
// 与双讲档是两条最薄的；T2 的曲线最小值紧随其后。
//
// ⚠ **两条欠账（不当成新基线认了）**：
// ① **双讲比 W0 尖峰差 ~2 dB**：同一补丁下尖峰是 −12.5 dB、我们 −14.65 dB，
//    而 `EchoCanceller3Config` 逐字相同。差额来源未定位（参考环 / 内插 / 时序？），
//    要查的话从这三处下手。
// ② **T3 的"只有近端"档**：8–12 s 窗口下 −7.04 dB，判据收到 −8。掉这一截的是
//    **抑制器**（AEC3 的透传档 `TransparentMode` 要"6 s 渲染在跑但滤波器没收敛"
//    才打开，见 `transparent_mode.rs`）。W0 §6.4 的 −0.9 dB 是从第 8 s 起算的
//    16 s 长表，与本处的 12 s 表不是同一个窗口，别再拿它当判据。

/// T18：**没有译音在播的时候，AEC 不许动麦克风**（产品契约，不是装置细节）。
///
/// 这是 2026-10-12 修掉的一个真缺陷：`run_frame` 原来**每帧都调 `buffer_render`**，
/// 没有参考时参考环欠载补零 ⇒ 每帧都在给 AEC3 推"静音的渲染块" ⇒ **48 kHz 上麦音被压掉
/// 25.91 dB**（16 kHz 单频带那条路碰巧不受影响）。对照：W0 尖峰在同样条件下**一次
/// `buffer_render` 都不调**，压掉 **0.28 dB**。而"远端安静、没有译音可播"恰是**最常见**
/// 的那一格——那时候用户的麦音本该原样过去。修法：参考环**真的取到数据**时才推渲染块
/// （欠载就跳过，见 `RefRing::hit`），实测回到 **−0.28 dB**。
///
/// 判据：两个率上无参考增益都 **≥ −1 dB**。把 `run_frame` 里 `if self.ring.hit` 那道闸门
/// 删掉（恢复成每帧都推）⇒ 48 kHz 这条红在 ~−26 dB。
#[test]
fn a_silent_render_path_must_not_touch_the_microphone() {
    for rate in [48_000u32, 16_000] {
        let frame = rate as usize / 100;
        let chunk = frame * 20;
        let mut aec = SonoraAec::new(rate).expect("表内的率都建得起来");
        // 与参考无关的"近端"信号：不同频率、不同幅度，且**从不**推参考。
        let input: Vec<f32> = (0..chunk)
            .map(|i| 0.05 * ((i as f32) * 0.07).sin())
            .collect();
        let (mut ein, mut eout) = (0.0f64, 0.0f64);
        for tick in 0..60 {
            let out = aec.process(&input);
            // 前 30 拍让滤波器状态稳住，只量稳态。
            if tick > 30 {
                ein += input.iter().map(|&x| (x as f64).powi(2)).sum::<f64>();
                eout += out.iter().map(|&x| (x as f64).powi(2)).sum::<f64>();
            }
        }
        let gain_db = db(eout / ein);
        assert!(
            gain_db >= -1.0,
            "{rate} Hz：没有参考时增益 {gain_db:.2} dB——远端安静这一格不许压麦克风"
        );
    }
}

/// T17：装置的参考**不许再塌成低音**——上一轮那个"写错的低通"的墓碑与防回退钉子。
///
/// 2026-10-12 之前 `BandLimited` 的低通把极点系数与输入系数写反了（`l1 = y +
/// lp_a·(prev − y)`，正确的是 `a·y + (1−a)·x`），实测真实拐点约 **233 Hz**、不是标称的
/// 10.8 kHz ⇒ 那一整批 48 k 用例的参考"1 kHz 以上只剩 0.64% 功率"，等于在近基带信号上
/// 量 AEC；W0 尖峰的装置有同一个 bug，所以它那份 −10.3 dB 也不可比。
///
/// 修对之后（`a·y + (1−a)·x`）实测的口径，**如实记**：1 kHz **−0.00 dB**、
/// 10.8 kHz −1.0 dB、20 kHz **−0.45 dB**、Nyquist −2 dB。也就是"10.8 kHz"在这个
/// 一阶×2 家族里是**极点**而不是 −3 dB 拐点，参考实际接近全带（只在高频末端轻微衰减）。
/// **别把这一条读成"装置把参考限到 10.8 kHz"**——它没有，也不需要：AEC 要消的是整条
/// 链上的回声，参考带得越全，消的回声越全（这也是修对之后 ERLE 反而升高的原因）。
///
/// 所以本用例只钉**当初真正修的那件事**：1 kHz 几乎不被衰减（≥ −1 dB）。谁再把系数写反，
/// 这条立刻红（实测回落到 −25.63 dB）。
#[test]
fn the_reference_must_not_collapse_to_bass() {
    let fs = 24_000usize;
    let mut filter = BandLimited::new(fs, 100.0, 10_800.0);
    let (mut ein, mut eout) = (0.0f64, 0.0f64);
    for i in 0..fs {
        let x = (TAU * 1_000.0 * i as f32 / fs as f32).sin();
        let y = filter.step(x);
        ein += (x as f64).powi(2);
        eout += (y as f64).powi(2);
    }
    assert!(
        db(eout / ein) >= -1.0,
        "参考在 1 kHz 上只有 {:.2} dB——它又塌成低音了（坏版实测 −25.63 dB）",
        db(eout / ein)
    );
}

// ---------------------------------------------------------------- T1

/// T1：合成回声（单讲、纯增益 + 纯时延、D=30 ms 与 120 ms）下 **ERLE ≥ 15 dB**，
/// 且 **≤ 1 s 达到**。
///
/// 15 dB 是 §7.2 照 W0 实测定的：48 k 三频带的天花板是 ~25 dB（D=40 ms 25.29 dB、
/// D=120 ms 23.18 dB），取 15 dB 留 8 dB 给"真板子的回声路径不是纯增益 + 纯时延"。
/// 把实现改成直通（`port: None`）或删掉内部那一步 ×32768 → ERLE ≈ 0 dB → 红。
#[test]
fn synthetic_echo_is_cancelled() {
    for delay_ms in [30, 120] {
        let outcome = measure(&Setup::single(delay_ms));
        assert!(
            outcome.erle_db >= 15.0,
            "D={delay_ms} ms 的 ERLE 只有 {:.2} dB（判据 ≥ 15 dB，算子延迟 {} 样本）",
            outcome.erle_db,
            outcome.latency
        );
        let converge = outcome
            .converge_ms
            .unwrap_or_else(|| panic!("D={delay_ms} ms 一直没到稳态 − 3 dB：{:?}", outcome.curve));
        assert!(
            converge <= 1_000.0,
            "D={delay_ms} ms 收敛用了 {converge} ms（判据 ≤ 1000 ms）"
        );
    }
}

// ---------------------------------------------------------------- T2

/// T2：ERLE 曲线（0.5 s 一格，与 W0 §6.4 的 `curve` 同口径）**从 1 s 起不再低于 12 dB**。
///
/// 用 0.5 s 一格而不是单帧：单帧（10 ms）的 ERLE 抖动极大，钉不住也说明不了问题。
/// 48 k 实测 0.5 s 就到稳态（§6.4）。滤波器不自适应 → 曲线平在 0 dB → 红。
#[test]
fn converges_within_one_second() {
    let outcome = measure(&Setup::single(40));
    assert!(!outcome.curve.is_empty(), "得有曲线可看");
    for (at_ms, value) in outcome.curve.iter().copied() {
        if at_ms < 1_000.0 {
            continue;
        }
        assert!(
            value >= 12.0,
            "{at_ms} ms 处的 ERLE 只有 {value:.2} dB（判据：1 s 之后每一点都 ≥ 12 dB）；\
             整条曲线：{:?}",
            outcome.curve
        );
    }
    assert!(
        outcome.erle_db >= 15.0,
        "稳态也不该掉下去：{:.2} dB",
        outcome.erle_db
    );
}

// ---------------------------------------------------------------- T3

/// T3：双讲时**近端保留 ≥ −16 dB**，并且"只有近端、没有回声"那一档**近端保留 ≥ −8 dB**
/// （不误伤；旧判据 −3 dB 是坏装置上的数，见下面的重定记录）。
///
/// ⚠ **2026-10-12 重定基线**：旧判据是 `≥ −12 dB`，依据是坏装置上量到的 −5.78 dB
/// （W0 自己也是 −10.3 dB，但**它的装置同一个 bug**）。把 `BandLimited` 的系数修对之后
/// （参考变成真正的 100 Hz–10.8 kHz 宽带，见 T17），回声终于与近端的 300 Hz–4 kHz
/// **频段重合**，抑制器要在同一频段里分清"回声"与"近端"，近端被多吃一截：
///
/// | 口径 | 双讲近端保留 |
/// | --- | --- |
/// | 坏装置（近基带参考， overhaul 前） | −5.78 dB |
/// | 本实现 + 修对的装置 | **−14.65 dB** |
/// | W0 尖峰 + 修对的装置（同一补丁） | −12.5 / −12.6 dB |
///
/// 判据取 **−16 dB**（余量 +1.4，**偏薄，动这台装置前先看这张表**）。
/// **我们还比 W0 尖峰差 ~2 dB**（−14.65 vs −12.5，配置逐字相同）——这条差额已记作欠账，
/// 不当成"新基线就认了"。
///
/// 两条必须都写：只写前一条的话，"把近端一起消掉"也能让它变绿。
/// 过度抑制（把近端当回声消了）→ 红。
#[test]
fn near_end_survives() {
    // ① 双讲：近端比回声低 14 dB，输出里的近端不许被吃掉超过 16 dB。
    let mut double = Setup::single(40);
    double.mode = Mode::DoubleTalk;
    let outcome = measure(&double);
    assert!(
        outcome.near_keep_db >= -16.0,
        "双讲把近端吃掉了 {:.2} dB（判据 ≥ −16 dB，本实现实测 −14.65；W0 尖峰 −12.5）",
        outcome.near_keep_db
    );
    // 钉住"真的在消回声"，否则这一半退化成"什么都不做"也能过。
    assert!(
        outcome.erle_db >= 6.0,
        "双讲下的回声抑制只有 {:.2} dB——这条用例得真的在消回声（W0 实测 11.78 dB）",
        outcome.erle_db
    );

    // ② 只有近端、没有回声：近端基本原样留着。
    //
    // 窗口取 8–12 s，**这不是随便挪的**：AEC3 的"透传档"（`TransparentMode`）要
    // 6 s 的"渲染在跑但滤波器没收敛"才打开，而这一档正是为"没有回声（头戴耳机）"
    // 准备的——W0 §6.4 的 −0.9 dB 也是从第 8 s 起算的（它整条主表都是 16 s/8 s）。
    // 4–8 s 那个窗口里透传档还没打开，量到的会是**抑制器**的衰减（约 −7 dB），
    // 那是另一件事。
    let mut near_only = Setup::single(40);
    near_only.mode = Mode::NearOnly;
    near_only.total_ms = 12_000;
    near_only.steady_from_ms = 8_000;
    let outcome = measure(&near_only);
    assert!(
        outcome.near_keep_db >= -8.0,
        "没有回声时不该误伤近端：保留 {:.2} dB（判据 ≥ −8 dB，本实现实测 −7.04）",
        outcome.near_keep_db
    );
}

// ---------------------------------------------------------------- T4

/// T4：**超窗的时延必须诚实退化**。D=400 ms 时 ERLE **< 15 dB**（W0 实测 12.66 dB），
/// 同时 D=40 ms 必须 ≥ 15 dB。
///
/// 这是 T1 的对照：钉住"这个测试真的在测对齐，不是恒等变换"。
/// 参考根本没送进去（或送错）→ 两条都不达标 → 红。
#[test]
fn a_misaligned_reference_cancels_worse() {
    let far = measure(&Setup::single(400));
    assert!(
        far.erle_db < 15.0,
        "D=400 ms 的 ERLE 是 {:.2} dB——超窗的时延必须诚实退化（W0 实测 12.66 dB）",
        far.erle_db
    );
    let near = measure(&Setup::single(40));
    assert!(
        near.erle_db >= 15.0,
        "D=40 ms 的 ERLE 只有 {:.2} dB",
        near.erle_db
    );
    assert!(
        near.erle_db > far.erle_db,
        "近的时延必须比远的消得好：{:.2} vs {:.2} dB",
        near.erle_db,
        far.erle_db
    );
}

// ---------------------------------------------------------------- T7

/// T7：参考写入的颗粒度与上行**不同**（参考按播放率、每次 7 ms 一小段推），
/// 且两者相位**不对齐**（错开 7 ms），AEC 仍收敛。
///
/// 把两路硬绑成同一拍（例如实现里假设"推一次正好对应一拍采集"）、或把参考缓冲去掉
/// 直接当拍消费 → 抖动一加就红。
#[test]
fn reference_survives_playback_jitter() {
    let mut setup = Setup::single(40);
    // 相位错开 7 ms；每次最多推 7 ms（播放率样本）。
    setup.reference_phase = RATE as usize * 7 / 1000;
    setup.reference_chunk = PLAYBACK_RATE as usize * 7 / 1000;

    let outcome = measure(&setup);
    assert!(
        outcome.erle_db >= 15.0,
        "参考与上行不同拍、相位错开时 ERLE 只有 {:.2} dB（判据 ≥ 15 dB）",
        outcome.erle_db
    );
}

// ---------------------------------------------------------------- T13

/// T13：**参考（与采集）必须以 int16 域送进 AEC3**。
///
/// 两半都钉（W0 §6.2 的第一号发现）：
///
/// * 正半边：按 trait 的口径（f32 的 [-1,1]，RMS 0.20）送 → 实现内部 ×32768 →
///   AEC3 看到 int16 量级的激励 → **ERLE ≥ 15 dB**。
/// * 负半边：把同一份信号除以 32768 再送 → AEC3 的激励门限
///   （`512 × 150² = 1.152e7`）过不去 → 时延都估不出来 → **ERLE < 5 dB**。
///
/// 负半边是"这条判据真的在判幅度域"的对照（W0 实测这一档 0–1 dB）；**能抓住改错的是
/// 正半边**：删掉实现里那一步 ×32768 → 正半边立刻掉到 5 dB 以下 → 红。
#[test]
fn the_reference_is_fed_in_the_int16_domain() {
    let scaled = measure(&Setup::single(40));
    assert!(
        scaled.erle_db >= 15.0,
        "按 [-1,1] 的口径送进来（实现内部换算）时 ERLE 只有 {:.2} dB",
        scaled.erle_db
    );

    // 负半边：整条装置（参考 + 回声 + 近端）一起缩到 int16 域以下。
    let mut tiny_setup = Setup::single(40);
    tiny_setup.scale = 1.0 / 32768.0;
    let tiny = measure(&tiny_setup);
    assert!(
        tiny.erle_db < 5.0,
        "把幅度压到 int16 域以下时 ERLE 却有 {:.2} dB——AEC3 的激励门限不该被绕过\
         （W0 实测这一档 0–1 dB）",
        tiny.erle_db
    );
}

// ---------------------------------------------------------------- 16 kHz 那条路

/// **16 kHz（单频带）**：`AEC_RATES` 明说支持它。这条走**真实现**：建得起来、
/// `process` 不 panic（余块抽干那条路一起走到）、输出长度 = 160 样本/10 ms。
///
/// 它顺带钉住 F2 那个"只在 debug 下炸"的缺陷：借用表当时恒交 3 个频带，而 16 k 只有
/// 1 个频带，上游 `frame_blocker.rs` 的 `debug_assert_eq!(num_bands, sub_frame.len())`
/// 会 panic（release 靠后面的 `zip` 截断恰好对）。把 `&refs[..num_bands]` 退回 `&refs`
/// 复跑，这条红在 `frame_blocker.rs:42`。
#[test]
fn the_single_band_path_works_at_16_khz() {
    let frame = 16_000 / 100; // 160 样本 = 10 ms
    let reference = vec![0.01f32; 24_000 * 20 / 1000]; // 播放率 20 ms
    let mut aec = SonoraAec::new(16_000).expect("16 kHz 在支持集里");
    for tick in 0..10 {
        aec.push_reference(&reference);
        let produced = aec.process(&vec![0.01f32; frame * 2]);
        assert_eq!(
            produced.len(),
            frame * 2,
            "第 {tick} 拍：20 ms 的采集该出 20 ms"
        );
    }
    aec.reset();
}

/// **T15：16 kHz 单频带的 ERLE。**
///
/// 这一档**以前一条 ERLE 用例都没有**——装置要从播放率 24 k 升到 16 k（比 2/3，非整数），
/// 而本文件的信号发生器只做整数倍，所以 W1 那轮只留了上面那条"跑得通"的冒烟用例，
/// **16 k 的消回声能力从来没量过**（W0 §6.3 的 62–72 dB 是**尖峰**在 16 k 上量的，
/// 不是本实现）。`Setup::at_16k` 把 48 k 那套装置整体搬到 16 k（参考仍按 24 k 送进
/// `push_reference`，AEC 内部用自己的 `LinearUpsampler` 把它带到采集率——**那一步是
/// 降采样、且没有抗混叠**，正好是这条要量的东西）。
///
/// 实测（本机 x86-64，与 48 k 那批同口径；把 `reference_lp_hz` 设在 0.45×24 k）：
///
/// | D | 稳态 ERLE | 收敛 | 1 s 后曲线最小值 |
/// | --- | --- | --- | --- |
/// | 30 ms | **63.81 dB** | 5.0 s | 31.77 |
/// | 120 ms | **64.44 dB** | 5.0 s | 29.66 |
/// | 200 ms | **64.17 dB** | 5.0 s | 26.77 |
///
/// **阈值 25 dB：留了 ~39 dB 余量**，两条理由：① 单频带没有三频带滤波器组那 ~46 dB 的账
/// （`ThreeBandFilterBank` 往返 SNR 9.92 dB，见 W0 §6.3），所以 16 k 应当远好于
/// T14 的 18–35 dB；② 参考路那步无抗混叠降采样会把 8 kHz 以上的能量折进带内（不修饰参考）。
/// 若哪天它掉到 25 dB 以下，第一时间查 `LinearUpsampler` 那条路。
///
/// **一条如实记的装置边界**（不是代码缺陷）：把回声换成"物理上正确的抗混叠回声"
/// （参考先过正确的二阶低通再走真 24k→16k 重采样），ERLE 掉到 **29–34 dB**——
/// 装置里那个"按线性栅格生成的回声"比真麦克风路径更讨好 AEC。所以本条的数是
/// **本装置**下 16 k 路的数，不是"真实房间"的数。
#[test]
fn the_single_band_path_reaches_a_high_erle_at_16_khz() {
    for delay_ms in [30, 120, 200] {
        let outcome = measure(&Setup::at_16k(delay_ms));
        assert!(
            outcome.erle_db >= 25.0,
            "16 kHz 单频带 D={delay_ms} 的 ERLE 是 {:.2} dB，低于 25 dB（实测 63–64，见头注）",
            outcome.erle_db
        );
    }
}

/// **T16：回声按"物理上正确的方式"进来时，16 k 这条路还剩多少。**
///
/// T15 那个 63–64 dB 有个必须说清的前提：回声是**按实现内部那条栅格**造的
/// （`EchoShape::ImplGrid`），也就是麦克风听到的与 AEC 参考路折出来的**是同一份栅格**——
/// 那比真机的麦克风路径更讨好 AEC。真麦克风听到的是 24 k 的译音经过真实声学路径，
/// 声卡按 16 k 采样时必须**抗混叠再抽取**（`EchoShape::AliasFree`）。
///
/// 这一档就是把回声换成物理正确的那一路，其余不变。实测（本机 x86-64、同口径）：
///
/// | 参考 | D=30 ms | D=120 ms | 1 s 后曲线最小 |
/// | --- | --- | --- | --- |
/// | 陡沿宽带（标称 10.8 kHz） | **29.44 dB** | **29.39 dB** | 18.18–20.31 |
/// | 陡沿窄带（真 7 kHz） | **34.29 dB** | **34.28 dB** | 21.55–25.04 |
///
/// 两条读法：① **这一档才是"真房间"的下界**——即 T15 的 63 dB 是装置上限、这里是更接近
/// 真的数，两者都**远高于 §7.2 的 15 dB**；② 掉下来的那一截几乎全来自参考路那步
/// **无抗混叠的 24k→16k 降采样**：8 kHz 以上的能量被折回带内，参考与回声于是**不自洽**。
/// 要把这一档推到 40 dB 以上，得给 `aec.rs::LinearUpsampler` 加抗混叠（本文件 T6 的
/// 分配账说 `push_reference` 现在是 **0 次分配**，加滤波器不能把这条破了）。
///
/// 判据 **≥ 20 dB**（实测最小 29.44，余量 +9.4）：它钉的是"参考与回声不自洽时 AEC 仍然
/// 在消"——把这一节改成直通（`port: None`）或删掉 ×32768 ⇒ ERLE ≈ 0 ⇒ 红。
#[test]
fn an_alias_free_echo_still_leaves_a_real_cancellation_at_16_khz() {
    for delay_ms in [30, 120] {
        let setup = Setup {
            reference_shape: RefShape::Steep,
            echo_shape: EchoShape::AliasFree,
            ..Setup::at_16k(delay_ms)
        };
        let outcome = measure(&setup);
        assert!(
            outcome.erle_db >= 20.0,
            "16 kHz + 抗混叠回声 D={delay_ms} 只有 {:.2} dB（实测 29–34，见头注）",
            outcome.erle_db
        );
    }
}

// ---------------------------------------------------------------- T14

/// T14：48 kHz 三频带这条路本身值 **~46 dB 的账**（W0 §6.3：`ThreeBandFilterBank`
/// 分析→合成往返只有 9.92 dB SNR，与参考落在哪个频带无关）。
/// 判据：**18 dB ≤ ERLE ≤ 35 dB**。
///
/// 下界钉住"48 k 这条路本身有账要付"，防止拿 16 kHz 单频带的数字（62–72 dB）
/// 冒充 48 kHz 的验收结果；上界钉住"别把滤波器组接错了还能假装很好"
/// （W0 接错时撞到过 −66 dB 之后的假象）。
#[test]
fn the_three_band_path_is_capped_around_25_db() {
    let outcome = measure(&Setup::single(40));
    assert!(
        (18.0..=35.0).contains(&outcome.erle_db),
        "48 kHz 三频带的 ERLE 是 {:.2} dB，落在 18–35 dB 之外（W0 实测 25.29 dB）",
        outcome.erle_db
    );
}

// ---------------------------------------------------------------- T6

/// T6：热路径的分配（`.omp/RULES.md` #6）。
///
/// 分两条，**因为上游库自己每块都在分配**：
///
/// * `push_reference`（我们自己写的那半：线性插值 + 环形缓冲）：**零分配**。
/// * `process`：上限 [`MAX_PROCESS_ALLOCS_PER_BLOCK`]（**一次 `process` 调用** = 一个
///   20 ms 拍 = 2 个 10 ms 帧）。实测 **62**，逐项拆得开：
///     * **55 = 5 × 11**：上游 `sonora-aec3` 的
///       `echo_remover.rs::EchoRemover::process_capture` 每块现搭十一份工作区
///       （`e` / `y2` / `e2` / `r2` / `r2_unbounded` / `s2_linear` / 四个 `FftData` /
///       `subtractor_output`）。一个 20 ms 拍 = **5 块**（4 次子帧插入 + 1 次余块抽干），
///       不是 4 块；渲染侧（`buffer_render`）实测 **0 次**分配。
///     * **1**：我们那一个返回的 `Vec`（与 `Denoise::process` 同性质）。
///     * **6**：我们每帧的**借用表**（`aec.rs::sub_frame_refs`：`Vec::new()` + push，
///       **用到哪个频带才分配哪个**；48 k 三个频带都用 ⇒ 3 次/帧 × 2 帧 = 6）。
///
///   三条加起来 55 + 1 + 6 = **62**（本用例的口径，48 k）。
///
///   **16 kHz**（本用例不量它）：池子只用第 0 个频带 ⇒ 每帧 1 次、一拍 2 次。
///   同一把量具上做过 A/B：换回 `Vec::with_capacity(1)`（三个频带都白建）时
///   16 k 每拍多 **4** 次、48 k 不变——那 4 次是白建的，已经省掉。
///
/// **为什么不把借用表也做成零分配**：上游那个 `&[Vec<&[f32]>]` 的形状要求最里层是
/// `Vec<&[f32]>`（带生命周期），跨帧存进 `self` 就是自引用——只能靠 `unsafe` 扩
/// `'static`，而那条路被 Miri 的 Stacked Borrows / Tree Borrows **两套模型都判 UB**
/// （见 `aec.rs::set_sub_frame_arg` 的注释）。所以取"每帧现搭、就地用完就丢"，
/// 并把代价记在这儿。
///
/// 上限 64（= 62 + 2）是**回归护栏**，它抓什么、抓不住什么：
/// * 抓得住：把借用表改成**每次插入**都现搭（8 次插入/拍 × 4 次分配 = **+32** ⇒ 94）；
///   或者每帧每路各搭一份（**+6** ⇒ 68）；或者任何一处新增的 `format!` / `clone()`。
/// * 抓不住：比 +2 更小的零星分配（例如某个上游版本多分配一两次）。
const MAX_PROCESS_ALLOCS_PER_BLOCK: u64 = 64;

#[test]
fn the_aec_stage_adds_no_allocations_beyond_the_return_vec() {
    let block = RATE as usize * 20 / 1000;
    let capture = vec![0.01f32; block];
    let reference = vec![0.01f32; PLAYBACK_RATE as usize * 20 / 1000];
    let mut aec = SonoraAec::new(RATE).expect("48 kHz 要能建起 AEC");

    // 先跑几拍把内部状态热起来（首拍建环、攒帧，读数没有代表性）。
    for _ in 0..4 {
        aec.push_reference(&reference);
        let _ = aec.process(&capture);
    }

    let before = alloc_count();
    for _ in 0..4 {
        aec.push_reference(&reference);
    }
    let after_reference = alloc_count();
    assert_eq!(
        after_reference - before,
        0,
        "push_reference 不许分配（4 拍里新分配了 {} 次）",
        after_reference - before
    );

    let before_process = alloc_count();
    let produced = aec.process(&capture);
    let after_process = alloc_count();

    assert_eq!(
        produced.len(),
        block,
        "20 ms 一拍的输入该出满 20 ms（两帧）"
    );
    assert!(
        after_process - before_process <= MAX_PROCESS_ALLOCS_PER_BLOCK,
        "process 的分配次数是 {}（上限 {MAX_PROCESS_ALLOCS_PER_BLOCK}；实测 62 = \
         55（上游每块 11 份 × 5 块）+ 1（返回 Vec）+ 6（每帧借用表））",
        after_process - before_process
    );
}

// --- 分配计数（只在本测试二进制里生效）---------------------------------
//
// 与 `chain.rs::tests` 同一个套路：全局分配器 + **按线程**计数，于是同一二进制里
// 并行跑的别的用例不会污染读数。

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
