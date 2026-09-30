//! snd-aloop 上的「播 → 采」实机用例。**默认全部跳过**：没有内核模块的机器上跑不了。
//!
//! 跑法：
//!
//! ```text
//! modprobe snd-aloop            # 造出 Loopback 声卡（0 = 播放侧，1 = 采集侧）
//! cargo test -p vox-audio-alsa --test aloop_roundtrip -- --ignored --nocapture
//! ```
//!
//! 两端必须**同率**：snd-aloop 的 `,0` 与 `,1` 是一对，两边率不一致时 `snd_pcm_start`
//! 报 EIO。所以这里先把播放端开在 48 kHz（也是采集侧阶梯第 1 档要的率），
//! 再起采集；采集拿回别的率就是真的谈崩了，直接断言失败而不是"将就着比"。
//!
//! 判据是**已知的信号对上了**：往 `hw:Loopback,0` 灌一段 1 kHz 正弦（幅度 0.5），
//! 从 `hw:Loopback,1` 采回来，断言能量、主频、块格式、停机边界四件事。
//! 静音直接 FAIL——静音正是"写进去了但没读出来"这一类 bug 的样子。
//!
//! **回环不是零延迟的，测试必须等信号到达**：snd-aloop 的缓冲默认给得很大
//! （本机实测 `buffer_size = 262144` 帧 = 5.46 s，`snd_pcm_delay` 也报 5460 ms），
//! 写端灌进去的音频要过完整个缓冲才在对端读得到。所以这里不按"跑了 N 秒"收数据，
//! 而是**等到采集侧真的收到满幅度的信号，再多收 `ANALYSIS_SECONDS` 秒**才算完
//! （见 `MAX_SECONDS` / `SIGNAL_CHUNK_RMS`）。缓冲小的机器上这段等待是 0，下面的
//! 判据一字不用改。
//!
//! **"收到满幅度"必须连着好几块才算数**：采集侧起流约 0.3 s 处会冒出一两下满幅的咔哒
//! （细节与实测记录见 `SIGNAL_RUN`）。只看单块会被它骗过去，分析窗口的起点就会落到
//! 回环信号到达之前——本用例第一版正是这样，整段 RMS 被静音稀释到 0.22 而查不出原因。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vox_audio_alsa::{AlsaCapture, AlsaPlayback};
use vox_core::pipeline::ResampleFactory;
use vox_core::ports::{
    AudioChunk, CaptureFormat, CaptureSource, CaptureTarget, PlaybackSink, Resample,
};

/// 两端共同的率（理由见文件头）。
const RATE: u32 = 48_000;
/// 灌进播放端的信号。
const TONE_HZ: f64 = 1_000.0;
const TONE_AMPLITUDE: f32 = 0.5;
/// 一次 `push` 的块长（20 ms，与 `INPUT_BLOCK_MS` 同口径）。
const PUSH_BLOCK: usize = RATE as usize / 50;
/// 采集块长。
const BLOCK_MS: u32 = 20;

/// 至少播这么久才考虑收尾（回环延迟为 0 的机器上，这就是普通的一轮时长）。
const MIN_SECONDS: f64 = 6.0;
/// 等回环延迟的上限。本机 snd-aloop 的缓冲是 5.46 s（2026-09-30 实测），
/// 留一倍以上余量；超了这个上限还没收到信号，就按"信号没过去"判 FAIL。
const MAX_SECONDS: f64 = 25.0;
/// 收到信号之后还要再收多久。主频判定在整段信号上做（见 `autocorrelation`），
/// 3 s ≈ 144000 个样本、约 3000 个周期，自相关与 RMS 都稳得很。
const ANALYSIS_SECONDS: f64 = 3.0;
/// 一块（20 ms）的 RMS 到这个数就算"这一块有信号"。0.3536 是满幅正弦的 RMS，
/// 取 0.2 留出每块边界的相位抖动（20 ms 装不下整数个 1 kHz 周期时实测 0.341）。
const SIGNAL_CHUNK_RMS: f64 = 0.2;
/// **要连续这么多块都过线才算"信号到了"**（5 × 20 ms = 100 ms）。
///
/// 这一条不是保险起见，是**实测逼出来的**：snd-aloop 采集侧起流约 0.3 s 处会冒出一两下
/// "咔哒"——2026-09-30 连跑三遍，稳定落在第 14 / 16 / 14 块，`peak` 恰好是满幅的 0.500，
/// 但 `rms` 只有 0.100 ~ 0.339（起流瞬间漏过来的一两个样本，不是一段音）。
/// 按"单块 rms ≥ 0.2"判信号到了，三遍全被这声咔哒骗过去，于是分析窗口的起点落在
/// 回环信号到达之前 5 秒多的静音里，后面整段 RMS 被静音稀释成 0.22 —— 正是这个用例
/// 一开始能量对不上的原因。
/// 改成"连续 5 块都过线"之后，三遍的起点都精确落在第 274 块（= 5.48 s 的回环延迟）。
const SIGNAL_RUN: usize = 5;

/// RMS 下限。**0.5 幅度正弦的 RMS 是 0.3536**，实测窗口 RMS 为 0.3535 ~ 0.3540，
/// 这条取 0.32（低 10%）。
///
/// 两边余量都要说得清：
/// - 上留 10%：窗口里绝大多数块的 rms 就是 0.354，少数块因周期没对齐掉到 0.341，
///   平均下来离 0.3536 不到 0.1%。10% 足够容纳块边界的相位抖动；
/// - 下留 1100 倍余量：静音的 RMS 精确是 0，16 位量化噪声约 9e-5，
///   即"链路通但信号没过去"这一类故障的量级在 0.001 以下。0.32 与它隔着两个数量级。
///
/// 注意这条**不再是**"回环缓冲开头那几秒静音"的挡箭牌——分析窗口已经从信号到达之后起算
/// （见 `SIGNAL_RUN`），窗口里本来就不该有静音，所以这里可以收得很紧。
const RMS_MIN: f64 = 0.32;
/// RMS 上限。**期望值 0.3536 的 1.19 倍**：超过它说明增益或声道处理出了问题——
/// 最典型的是把交织多声道当成单声道用（值会变成 1/2 倍或 2 倍），那正是
/// `AudioChunk::to_mono` 该挡住而没挡住的情形；另一个是削顶。
const RMS_MAX: f64 = 0.42;

/// 分析用的最短信号。**主频判定在整段信号上做**，不切短窗——理由见 `autocorrelation`。
const MIN_ANALYSIS_SAMPLES: usize = 48_000; // 1 s
/// 自相关扫描的滞后范围（样本数）。1 kHz @ 48 kHz 的周期是 48 个样本；
/// 32..=64 对应 1500 Hz ~ 750 Hz，正好把"率翻倍 / 减半"（周期 24 / 96）挡在外面——
/// 那两种错会让峰值跑到扫描区间的端点上，`periodicity` 随之塌到 0。
const AUTOCORRELATION_LAGS: std::ops::RangeInclusive<usize> = 32..=64;
/// 主频允许偏离灌进去的值的上限（Hz）。
///
/// 滞后是整数，量化得很粗：lag 48 → 1000.0 Hz，lag 47 → 1021.3，lag 49 → 979.6，
/// 所以**一格的误差就是 ±21 Hz**。这里用抛物线插值把峰估到亚样本精度（见
/// `autocorrelation`），实测误差在 1 Hz 以内，取 10 Hz：既容得下一格量化的一半，
/// 又比 ±50% 的频段窄一个数量级，真错了一个数量级的换算绝对逃不出去。
const FREQUENCY_TOLERANCE_HZ: f64 = 10.0;
/// 自相关峰值（归一化到 1.0）的下限，也就是"这根音有多干净"。
///
/// 理想单音在整周期滞后上自相关正好是 1.0；实测 0.9930（回环里夹着几十处 1 ms 的
/// 静音空洞与慢速相位游走）。取 0.9：留 0.09 给这些，回环本身的抖动吃不掉它；
/// 而帧/样本换算写错、率谈崩、交织当单声道用这些真 bug 会把它打到 0.5 以下
/// （能量摊到各处，周期性就没了）。
const PERIODICITY_MIN: f64 = 0.9;

/// 碰本机 Loopback 卡的 `#[ignore]` 用例共用这一把锁。
///
/// crate 里那一把（`lib.rs::LOOPBACK_CARD`）是 `#[cfg(test)]` 的，集成测试拿不到——
/// 它只对同一个测试二进制里的单元测试有效。这里是本文件自己的一把：
/// 几个用例都要在 `,0` / `,1` 上开同率的流，并行跑必然打架。
/// cargo 本身是一个测试二进制跑完再跑下一个，所以这把锁不会与单元测试那把互相等。
static LOOPBACK: Mutex<()> = Mutex::new(());

/// 拿锁。**中毒了也要拿到**：一个用例 panic 不该让后面几个用例全部跳过。
fn lock_loopback() -> MutexGuard<'static, ()> {
    LOOPBACK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 播放侧的重采样工厂。两端同率时这个闭包根本不会被调用（`needs_resampler` 判 false）。
fn resample_factory() -> ResampleFactory {
    Box::new(|from, to| Box::new(vox_dsp::Resampler::new(from, to)) as Box<dyn Resample>)
}

/// 一块正弦。**连续相位**：每次调用都从 0 开始会让块边界处相位跳变，
/// 那是宽带噪声，会把下面那个"能量集中度"的比值拉下来。
fn tone_block(start_sample: u64) -> Vec<f32> {
    tone_span(start_sample, PUSH_BLOCK as u64)
}

/// `frames` 帧连续相位的正弦（单声道，`push` 要的就是单声道）。
fn tone_span(start_sample: u64, frames: u64) -> Vec<f32> {
    let step = std::f64::consts::TAU * TONE_HZ / RATE as f64;
    (0..frames)
        .map(|i| {
            let n = start_sample + i;
            (step * n as f64).sin() as f32 * TONE_AMPLITUDE
        })
        .collect()
}

/// 采集侧攒下来的东西。回调只往里写，主线程读（集成测试，不是库的设备线程，
/// 那把锁是测试自己的，不违反 RULES #6）。
#[derive(Default)]
struct Collected {
    /// 收到的单声道样本，顺序拼接。
    samples: Vec<f32>,
    /// 每一块的 RMS（单声道，20 ms 一格）。用来找"信号从哪一块开始到"。
    chunk_rms: Vec<f64>,
    /// 收到的块数（= `chunk_rms.len()`，单列出来是为了报错时读得懂）。
    chunks: u64,
    /// 不是有限值的样本数。
    non_finite: u64,
    /// 与 `start` 回报对不上的块数（率/声道/块长任一不一致就 +1）。
    mismatched_chunks: u64,
    /// `start` 回报的格式，用来当每块的对照。
    expect: Option<CaptureFormat>,
    /// 对照用的块长（交织样本数）。
    expect_samples: usize,
}

/// 收集回调。`expect` 在 `start` 之后才填，所以每块都对照"当时填好的值"。
fn collector(collected: Arc<Mutex<Collected>>) -> Box<dyn FnMut(AudioChunk) + Send> {
    Box::new(move |chunk: AudioChunk| {
        let mut slot = collected.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(expect) = slot.expect {
            let ok = chunk.sample_rate == expect.sample_rate
                && chunk.channels == expect.channels
                && chunk.samples.len() == slot.expect_samples;
            if !ok {
                slot.mismatched_chunks += 1;
                eprintln!(
                    "块对不上 start 的回报：率 {}/{}、声道 {}/{}、块长 {}/{}",
                    chunk.sample_rate,
                    expect.sample_rate,
                    chunk.channels,
                    expect.channels,
                    chunk.samples.len(),
                    slot.expect_samples
                );
            }
        }
        slot.chunks += 1;
        slot.non_finite += chunk.samples.iter().filter(|s| !s.is_finite()).count() as u64;
        let mono = chunk.to_mono();
        slot.chunk_rms.push(rms_of(
            mono.iter().map(|s| (*s as f64) * (*s as f64)).sum(),
            mono.len(),
        ));
        slot.samples.extend_from_slice(&mono);
    })
}

/// 采集块长（交织样本数）。与 `probe::block_frames` 同一口径：块长按**协商到的率**算，
/// 下限 10 ms（`vox_dsp::chunk::Blocker::new` 内部那一套）。
fn expect_block_samples(format: CaptureFormat) -> usize {
    let frames = ((format.sample_rate as u64 * BLOCK_MS as u64) / 1000).max(1) as usize;
    frames * format.channels.max(1) as usize
}

/// 单声道一块的样本数（`block_frames` 的块长除掉声道）。
fn mono_frames_per_block(format: CaptureFormat) -> usize {
    ((format.sample_rate as u64 * BLOCK_MS as u64) / 1000).max(1) as usize
}

fn rms_of(sum_squares: f64, samples: usize) -> f64 {
    if samples == 0 {
        return 0.0;
    }
    (sum_squares / samples as f64).sqrt()
}

/// 第一段**连续 `run` 块都过线**的起点。没有这样一段就返回 `None`。
///
/// 单块判定会被 snd-aloop 起流那声咔哒骗过去（见 `SIGNAL_RUN` 的实测记录）。
fn sustained_run(profile: &[f64], threshold: f64, run: usize) -> Option<usize> {
    if run == 0 {
        return None;
    }
    profile
        .windows(run)
        .position(|window| window.iter().all(|rms| *rms >= threshold))
}

/// 归一化自相关 `Σx[n]·x[n+lag] / Σx[n]²`。
///
/// **为什么主频用自相关而不是频谱**：snd-aloop 这条回环路上回来的音，频率不是死钉在
/// 1000 Hz 的——播放端 PCM 定时器与回环拷贝的边界各自对齐，抄回来的正弦有**缓慢的相位
/// 游走**（2026-09-30 实测：整段 3.4 s 的频谱峰值只剩理想值的 36%，能量摊到了谱底上）。
/// 频谱那一路对这种游走极其敏感：拿 8192 样本的短窗去量，相位漂过半个周期时"集中度"
/// 就在 0.16 与 1.00 之间乱跳，同一段信号相邻的窗能差 6 倍——那不是判据，是掷骰子。
/// 自相关量的是**局部**周期（一格滞后之内），对慢漂移免疫：整段扫下来峰值干净地落在
/// lag 48 上，r = 0.9930。
///
/// 量程 `AUTOCORRELATION_LAGS` 覆盖 750 ~ 1500 Hz，倍率级的换算错误会落到区间外。
fn autocorrelation(samples: &[f32], lag: usize) -> f64 {
    let energy: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    if energy <= 0.0 || samples.len() <= lag {
        return 0.0;
    }
    let cross: f64 = samples
        .windows(lag + 1)
        .map(|pair| pair[0] as f64 * pair[lag] as f64)
        .sum();
    cross / energy
}

/// 峰值所在的滞后，并按抛物线插值估到**亚样本**精度。
///
/// 滞后是整数，一格就是 ±21 Hz（见 `FREQUENCY_TOLERANCE_HZ`），直接拿整数格算频率会
/// 白白吃掉半个容差。抛物线插值是自相关测周期的标准做法：在峰顶三点上拟合一条抛物线，
/// 顶点落在格点之间。对理想单音它几乎无偏。
fn peak_lag(samples: &[f32]) -> (usize, f64, f64) {
    let peaks: Vec<(usize, f64)> = AUTOCORRELATION_LAGS
        .map(|lag| (lag, autocorrelation(samples, lag)))
        .collect();
    let (index, &(lag, value)) = peaks
        .iter()
        .enumerate()
        .max_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap())
        .expect("滞后范围非空");
    // 峰在端点时插值没有两侧的邻居，直接用整数格。
    if index == 0 || index + 1 >= peaks.len() {
        return (lag, value, lag as f64);
    }
    let r_left = peaks[index - 1].1;
    let r_right = peaks[index + 1].1;
    let denominator = r_left - 2.0 * value + r_right;
    let shift = if denominator.abs() < f64::EPSILON {
        0.0
    } else {
        0.5 * (r_left - r_right) / denominator
    };
    (lag, value, lag as f64 + shift)
}

/// 一条 1 kHz 正弦播进 Loopback，采回来之后能量、主频都对得上。
///
/// 这是 §4 验收标准（`S4-EMBEDDED-REFACTOR.md` §4「本机 snd-aloop 回环上采 → 播跑通」）
/// 的落点。会因什么 bug 红：
/// - 帧 / 样本换算写错（§4.2 那处坑）→ 采回来是错频或走样，主频与能量两条同时红；
/// - `Step::Underrun` 被当成 `Continue`（§12 M5 更正前的写法）→ 采集侧丢帧，块长对不上；
/// - 协商到的率没喂给 `Blocker`（§5.7）→ 块长按请求率算，`mismatched_chunks` 立刻红；
/// - 写进去了没读出来（缓冲、poll、方向搞反）→ 等到 `MAX_SECONDS` 也没有满幅度的块，红。
#[test]
#[ignore = "要真声卡：本机 snd-aloop 的 Loopback 卡（modprobe snd-aloop）"]
fn a_one_kilohertz_tone_played_into_the_loopback_comes_back_intact() {
    let _card = lock_loopback();

    // 先开播放端：两端必须同率，而速率是我们这边定的。
    let mut playback = AlsaPlayback::new(resample_factory());
    let rate = playback
        .open(Some("hw:Loopback,0"), RATE)
        .expect("snd-aloop 的播放侧（hw:Loopback,0）应当打得开");
    assert_eq!(
        rate, RATE,
        "播放侧没给到 {RATE} Hz（给的是 {rate}）：采集侧要同率，对不上就没法比"
    );

    let collected = Arc::new(Mutex::new(Collected::default()));
    let mut capture = AlsaCapture::new();
    let negotiated = capture
        .start(
            &CaptureTarget::Microphone(Some("hw:Loopback,1".to_string())),
            BLOCK_MS,
            collector(Arc::clone(&collected)),
        )
        .expect("snd-aloop 的采集侧（hw:Loopback,1）应当打得开");
    assert_eq!(
        negotiated.sample_rate, RATE,
        "采集侧谈到了 {} Hz，与播放侧的 {RATE} 不一致：snd-aloop 两端必须同率",
        negotiated.sample_rate
    );
    let frames_per_block = mono_frames_per_block(negotiated);
    {
        let mut slot = collected.lock().unwrap_or_else(|p| p.into_inner());
        slot.expect = Some(negotiated);
        slot.expect_samples = expect_block_samples(negotiated);
    }
    eprintln!(
        "Loopback 两端谈成：{negotiated:?}，一块 {} 个交织样本（{frames_per_block} 帧）",
        expect_block_samples(negotiated)
    );

    // 按实时节奏灌正弦，同时盯着采集侧有没有收到满幅度的块。
    //
    // **不预灌环**：本机实测（2026-09-30）预灌 4.5 秒与不预灌采回来的波形一样，
    // 差别只是回环那 5.46 s 的缓冲延迟，而预灌会让环一直满着、测不出"环见底"这件事。
    let mut pushed = 0u64;
    let started = Instant::now();
    let mut signal_at: Option<Duration> = None;
    while started.elapsed() < Duration::from_secs_f64(MAX_SECONDS) {
        playback.push(&tone_block(pushed));
        pushed += PUSH_BLOCK as u64;
        std::thread::sleep(Duration::from_millis(20));

        let elapsed = started.elapsed();
        if elapsed < Duration::from_secs_f64(MIN_SECONDS) {
            continue;
        }
        // 采集侧有没有**持续**收到满幅度的块？（回环延迟过了就会出现，见文件头；
        // 为什么要"持续"见 `SIGNAL_RUN`）
        let arrived = sustained_run(
            &collected
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .chunk_rms,
            SIGNAL_CHUNK_RMS,
            SIGNAL_RUN,
        )
        .is_some();
        if signal_at.is_none() && arrived {
            signal_at = Some(elapsed);
            eprintln!("回环延迟：{elapsed:?} 时采集侧收到满幅度的信号");
        }
        if let Some(at) = signal_at {
            if elapsed >= at + Duration::from_secs_f64(ANALYSIS_SECONDS) {
                break;
            }
        }
    }

    // 停机要快：采集线程最坏卡在一次 200 ms 的 poll 上，`stop` 的上界是 2 s。
    let stop_at = Instant::now();
    capture.stop();
    let stop_took = stop_at.elapsed();
    let stats = playback.stats();
    let close_at = Instant::now();
    playback.close();
    let close_took = close_at.elapsed();
    assert!(
        stop_took < Duration::from_secs(2),
        "stop() 花了 {stop_took:?}，超过 2 s 的有界上限"
    );
    assert!(
        close_took < Duration::from_secs(2),
        "close() 花了 {close_took:?}，超过 2 s 的有界上限"
    );
    assert!(
        signal_at.is_some(),
        "播了 {MAX_SECONDS} 秒，采集侧一块满幅度的信号都没收到\
         （环的延迟超过本用例给的上限，或写进去的东西根本没到对端）"
    );

    let (chunks, mono, chunk_rms, mismatched_chunks, non_finite) = {
        let mut slot = collected.lock().unwrap_or_else(|p| p.into_inner());
        (
            slot.chunks,
            std::mem::take(&mut slot.samples),
            std::mem::take(&mut slot.chunk_rms),
            slot.mismatched_chunks,
            slot.non_finite,
        )
    };
    eprintln!(
        "收到 {chunks} 块 / {} 个单声道样本；播放侧已渲染 {} 样本、丢弃 {} 个",
        mono.len(),
        stats.rendered_samples,
        stats.dropped_samples
    );

    // --- 块格式：每一块都与 start 回报的格式一致，且没有 NaN ---
    assert_eq!(
        mismatched_chunks, 0,
        "有 {mismatched_chunks} 块的率/声道/块长与 start 回报的对不上（块长必须按协商到的率算，§5.7）"
    );
    assert_eq!(non_finite, 0, "采回来的样本里出现了 NaN/Inf");

    // --- 分析窗口：跳过回环缓冲里那几秒静音与起流那声咔哒，从第一段持续满幅度的
    //     块**之后**起算（`SIGNAL_RUN` 块 = 100 ms，够跨过回环起流的边界） ---
    let first_full = sustained_run(&chunk_rms, SIGNAL_CHUNK_RMS, SIGNAL_RUN)
        .expect("上面已经断言过有持续满幅度的块了");
    // 夹到"至少还剩够长的分析段"的位置。**不能夹到一半**：回环延迟 5.48 s 之后信号才到，
    // 而本机一轮只推 9 秒，起点本来就在中点之后；早先写成 `min(mono.len() / 2)` 会把窗口
    // 硬生生拉回静音那一头，把整段 RMS 稀释成 0.312（176/226 个块有信号 → 0.3536·√0.7786）。
    let from = ((first_full + SIGNAL_RUN) * frames_per_block)
        .min(mono.len().saturating_sub(MIN_ANALYSIS_SAMPLES));
    let signal = &mono[from..];
    assert!(
        signal.len() >= MIN_ANALYSIS_SAMPLES,
        "信号到了之后只攒了 {} 个样本（要 ≥ {MIN_ANALYSIS_SAMPLES}）：主频没法判",
        signal.len()
    );

    // --- 能量 ---
    let sum_squares: f64 = signal.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    let rms = rms_of(sum_squares, signal.len());
    let peak = signal.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
    let mean = signal.iter().map(|s| *s as f64).sum::<f64>() / signal.len() as f64;
    eprintln!("回环回来：RMS {rms:.4}（期望 ≈ 0.3536）、峰值 {peak:.4}、均值 {mean:.6}");
    assert!(
        (RMS_MIN..=RMS_MAX).contains(&rms),
        "回环回来的 RMS 是 {rms:.4}，不在 [{RMS_MIN}, {RMS_MAX}] 里：\
         播进去的是 {TONE_HZ} Hz / 幅度 {TONE_AMPLITUDE} 的正弦（期望 RMS ≈ 0.3536）"
    );
    assert!(
        peak <= 1.0,
        "回环回来的峰值是 {peak:.4}，超过了满量程：削顶说明某一侧没夹紧"
    );

    // --- 主频与"这根音有多干净"：自相关（在整段信号上量，理由见 `autocorrelation`） ---
    let (lag, periodicity, refined_lag) = peak_lag(signal);
    let measured_hz = RATE as f64 / refined_lag;
    eprintln!(
        "自相关峰：lag {lag}（插值 {refined_lag:.3}）→ {measured_hz:.2} Hz，\
         周期性 {periodicity:.4}；期望 {TONE_HZ} Hz（周期 {RATE} / {TONE_HZ} = {} 样本）",
        RATE as f64 / TONE_HZ
    );
    assert!(
        (measured_hz - TONE_HZ).abs() <= FREQUENCY_TOLERANCE_HZ,
        "回环回来的主频是 {measured_hz:.2} Hz（lag {lag}），离灌进去的 {TONE_HZ} Hz 超过 \
         {FREQUENCY_TOLERANCE_HZ} Hz：帧/样本换算或率协商错了"
    );
    assert!(
        periodicity >= PERIODICITY_MIN,
        "回来的不是一根干净的单音：整周期自相关只有 {periodicity:.4}，低于 {PERIODICITY_MIN}\
         （能量摊到了别处），看 RMS {rms:.4} 与均值 {mean:.6}"
    );
}

/// `start` 回报的格式 == 随后每一块音频的 `sample_rate` / `channels` / 长度。
///
/// 这一条不需要信号：它钉的是 §5.7（块长按**协商到的**率算）与 C10（回报真值不是请求值）。
/// 回报请求值的话，这里每一块都对不上；`Blocker` 用了请求率的话，长度也对不上。
#[test]
#[ignore = "要真声卡：本机 snd-aloop 的 Loopback 卡的采集侧"]
fn the_negotiated_format_is_reported_honestly() {
    let _card = lock_loopback();

    let collected = Arc::new(Mutex::new(Collected::default()));
    let mut capture = AlsaCapture::new();
    let negotiated = capture
        .start(
            &CaptureTarget::Microphone(Some("hw:Loopback,1".to_string())),
            BLOCK_MS,
            collector(Arc::clone(&collected)),
        )
        .expect("snd-aloop 的采集侧应当打得开");
    let want = expect_block_samples(negotiated);
    {
        let mut slot = collected.lock().unwrap_or_else(|p| p.into_inner());
        slot.expect = Some(negotiated);
        slot.expect_samples = want;
    }

    // 收够 10 块（约 200 ms）就够验格式了。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if collected.lock().unwrap_or_else(|p| p.into_inner()).chunks >= 10 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "5 秒内没攒够 10 块：采集侧没有在流动"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    capture.stop();

    let slot = collected.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(
        slot.mismatched_chunks, 0,
        "有 {} 块的率/声道/块长与 start 回报的 {negotiated:?}（块长 {want}）对不上",
        slot.mismatched_chunks
    );
    assert_eq!(slot.non_finite, 0, "采回来的样本里出现了 NaN/Inf");
    eprintln!("start 回报 {negotiated:?}，10 块的率/声道/块长全部一致");
}

/// `stop` 返回之后回调不再触发（`vox_core::ports::CaptureSource` 的 C4 契约）。
#[test]
#[ignore = "要真声卡：本机 snd-aloop 的 Loopback 卡的采集侧"]
fn stop_guarantees_no_further_chunks() {
    let _card = lock_loopback();

    let collected = Arc::new(Mutex::new(Collected::default()));
    let mut capture = AlsaCapture::new();
    capture
        .start(
            &CaptureTarget::Microphone(Some("hw:Loopback,1".to_string())),
            BLOCK_MS,
            collector(Arc::clone(&collected)),
        )
        .expect("snd-aloop 的采集侧应当打得开");
    std::thread::sleep(Duration::from_millis(500));
    let before = collected.lock().unwrap_or_else(|p| p.into_inner()).chunks;
    assert!(before > 0, "起流后 500 ms 一块都没收到，测不了 stop 的契约");

    let stop_at = Instant::now();
    capture.stop();
    let took = stop_at.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "stop() 花了 {took:?}，超过 2 s 的有界上限"
    );

    // `stop` 返回时线程已 join、回调方已析构。再等 3 个 poll 超时（600 ms），
    // 若还有新块就说明回调还在触发。
    std::thread::sleep(Duration::from_millis(600));
    let after = collected.lock().unwrap_or_else(|p| p.into_inner()).chunks;
    assert_eq!(
        after, before,
        "stop 之后回调还在触发（块数 {before} → {after}），违反 C4 的契约"
    );
}

/// 播放侧的 `close()` 是有界的，且设备线程真的把环里的东西取走了。
#[test]
#[ignore = "要真声卡：本机 snd-aloop 的 Loopback 卡的播放侧"]
fn close_keeps_the_playback_thread_bounded_and_draining() {
    let _card = lock_loopback();

    let mut playback = AlsaPlayback::new(resample_factory());
    let rate = playback
        .open(Some("hw:Loopback,0"), RATE)
        .expect("snd-aloop 的播放侧应当打得开");
    assert_eq!(rate, RATE, "播放侧给的是 {rate} Hz");
    assert_eq!(
        playback.stats().sample_rate,
        rate,
        "stats 报的率必须与 open 回报的一致（C8）"
    );

    // 推两秒音频（按实时节奏），看设备线程有没有真的把环里的东西取走。
    let mut pushed = 0u64;
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < 2.0 {
        playback.push(&tone_block(pushed));
        pushed += PUSH_BLOCK as u64;
        std::thread::sleep(Duration::from_millis(20));
    }
    let stats = playback.stats();
    assert!(
        stats.rendered_samples > 0,
        "两秒音频推完了，设备线程一个样本都没取走：{stats:?}"
    );

    playback.flush();
    assert_eq!(
        playback.stats().queued_samples,
        0,
        "flush 之后队列必须是空的"
    );

    let close_at = Instant::now();
    playback.close();
    let took = close_at.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "close() 花了 {took:?}，超过 2 s 的有界上限"
    );
}
