//! 手动验一遍 ALSA 的「采集 → 播放」整条链路。跑法：
//!
//! ```text
//! cargo run -p vox-audio-alsa --example loopback -- hw:Loopback,1 hw:Loopback,0 3
//! cargo run -p vox-audio-alsa --example loopback -- hw:PCH,DEV=0 hw:PCH,DEV=0 5
//! ```
//!
//! 参数：**`<采集设备> <播放设备> [秒数]`**，缺省就是 `hw:Loopback,1` / `hw:Loopback,0` / 3 秒。
//!
//! 两端必须同率：snd-aloop 的 `,0` 与 `,1` 是一对，率不一致时 `snd_pcm_start` 报 EIO。
//! 本例先把**播放端开在 48 kHz**（采集侧阶梯第 1 档要的也是 48 kHz），再起采集，
//! 两端因此必然落在同一个率上；真对不上时下面会喊出来（能量那部分就不可信了）。
//!
//! 每秒打印一次**块数 / 峰值 / RMS / 播放侧的排队与已渲染样本数**。其中块数才是
//! 「流在流动」的证据：峰值与 RMS 只说明有没有能量，而两端互为对端（自灌回环）时
//! 能量恒为 0 是预期的，那不是坏了。
//!
//! 两件这个例子里**故意不做**的事：
//!
//! - **不报采样格式与 period 大小**：端口契约（`vox_core::ports`）只回报率与声道数，
//!   格式与 period 是 `probe.rs` / `capture.rs` / `playback.rs` 的内部事实，
//!   为了打印它们而给库里加 pub 项不值得。`tests/aloop_roundtrip.rs` 断言的也正是
//!   端口真正承诺的那部分。
//! - **不在非 Loopback 设备上阻止运行**：真麦克风接真扬声器是本例最常见的用法，
//!   只是要先把音量调小（采集到的声音会被放回同一个房间）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vox_audio_alsa::{AlsaCapture, AlsaPlayback};
use vox_core::pipeline::ResampleFactory;
use vox_core::ports::{CaptureSource, CaptureTarget, PlaybackSink, Resample};

/// 采集块长，与 `vox_core::pipeline::INPUT_BLOCK_MS` 同口径。
const BLOCK_MS: u32 = 20;
/// 两端共同的率。**必须同率**，理由见文件头；取 48 kHz 是因为采集侧的
/// `CAPTURE_WANT_RATE`（降噪的原生率）也是它，阶梯第 1 档就能谈成。
const SHARED_RATE: u32 = 48_000;
/// 缺省秒数。
const DEFAULT_SECONDS: u64 = 3;
/// 主线程把攒下的音频推给播放端的间隔。50 ms ≫ 20 ms 的块长，所以队列不会积压，
/// 也不会把 5 秒的环灌满（灌满是丢最旧，丢掉的正是刚才采到的那几段）。
const PUSH_INTERVAL: Duration = Duration::from_millis(50);

/// 累计器。采集回调写它、主线程读它，靠一把锁串起来——**这是 example，不是库的设备线程**：
/// 库里 RULES #6 的「回调里不加锁」说的是采集/播放自己的设备回调，这里是 example 级的一把锁。
#[derive(Default)]
struct Meter {
    /// 采到但还没推给播放端的单声道音频（主线程每 50 ms 取走一次）。
    pending: Vec<f32>,
    /// 本秒收到的块数。
    chunks: u64,
    /// 本秒的峰值（绝对值最大）。
    peak: f32,
    /// 本秒的平方和（f64 累加：f32 累加几十万个样本会掉精度）。
    sum_squares: f64,
    /// 本秒的样本数（单声道）。
    samples: u64,
    /// 全程收到的块数与平方和、样本数。
    total_chunks: u64,
    total_sum_squares: f64,
    total_samples: u64,
    /// 全程见到的非有限样本数。NaN 会一路污染下游滤波，必须当场报出来。
    non_finite: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() > 3 {
        eprintln!("参数多了。用法：loopback <采集设备> <播放设备> [秒数]");
        std::process::exit(1);
    }
    let capture_name = args
        .first()
        .cloned()
        .unwrap_or_else(|| "hw:Loopback,1".to_string());
    let playback_name = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "hw:Loopback,0".to_string());
    let seconds: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SECONDS);

    // 两端在不在同一张卡上？决定「能量为 0」是故障还是预期。
    let same_card = match (card_key(&capture_name), card_key(&playback_name)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    println!("采集：{capture_name}");
    println!("播放：{playback_name}");
    if same_card {
        println!(
            "注意：两端在**同一张卡**上（snd-aloop 的回灌）。采集到的声音被推回这张卡的播放端，\
             而起始时环里没有信号，所以能量为 0 是预期的 —— 这组参数下本例证明的是\
             「两端都打开成功、流一直在流动、不崩、按时退出」。"
        );
    } else {
        println!(
            "注意：这不是同一张卡。真麦克风接真扬声器时，采集到的声音会被放回同一个房间，\
             音量先调小再跑（否则容易啸叫）。"
        );
    }

    // 播放侧的工厂：两端率不相等时装的重采样器就是它。
    let factory: ResampleFactory =
        Box::new(|from, to| Box::new(vox_dsp::Resampler::new(from, to)) as Box<dyn Resample>);
    let mut playback = AlsaPlayback::new(factory);
    let rate = playback.open(Some(&playback_name), SHARED_RATE)?;
    println!("播放侧协商到 {rate} Hz（请求 {SHARED_RATE} Hz）");

    let meter = Arc::new(Mutex::new(Meter::default()));
    let mut source = AlsaCapture::new();
    let negotiated = source.start(
        &CaptureTarget::Microphone(Some(capture_name.clone())),
        BLOCK_MS,
        Box::new({
            let meter = Arc::clone(&meter);
            move |chunk| {
                let mono = chunk.to_mono();
                let mut slot = meter.lock().unwrap_or_else(|p| p.into_inner());
                slot.chunks += 1;
                slot.total_chunks += 1;
                let mut sum = 0.0f64;
                for sample in &mono {
                    if !sample.is_finite() {
                        slot.non_finite += 1;
                        continue;
                    }
                    slot.peak = slot.peak.max(sample.abs());
                    sum += (*sample as f64) * (*sample as f64);
                }
                slot.sum_squares += sum;
                slot.samples += mono.len() as u64;
                slot.total_sum_squares += sum;
                slot.total_samples += mono.len() as u64;
                slot.pending.extend_from_slice(&mono);
            }
        }),
    )?;
    println!(
        "采集侧协商到 {} Hz / {} 声道",
        negotiated.sample_rate, negotiated.channels
    );
    if negotiated.sample_rate != rate {
        // 两端不同率：snd-aloop 上采集侧会直接起不来（EIO），别的设备上则是各走各的率、
        // 播放侧多了一次重采样。都要说出来，因为下面的能量数字这时没有意义。
        println!(
            "⚠ 两端不同率（采集 {} Hz / 播放 {rate} Hz）：下面的能量数字不可信，\
             本例在这组参数下只能证明「流在流动」",
            negotiated.sample_rate
        );
    }

    let started = Instant::now();
    let mut next_report = Duration::from_secs(1);
    while started.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(PUSH_INTERVAL);
        // 采到什么就放什么：这条链路上没有滤波也没有（率相同时的）重采样，
        // 端到端是什么样就是什么样。
        let batch = {
            let mut slot = meter.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut slot.pending)
        };
        playback.push(&batch);

        if started.elapsed() < next_report {
            continue;
        }
        next_report += Duration::from_secs(1);
        // 报的是**刚过去**的那一秒：先把计数器加一，再拿旧值去打印，
        // 否则第 1 次打印会写成"第 2 秒"。
        let reported_second = next_report.as_secs() - 1;
        let (chunks, peak, rms, non_finite) = {
            let mut slot = meter.lock().unwrap_or_else(|p| p.into_inner());
            let report = (
                slot.chunks,
                slot.peak,
                rms_of(slot.sum_squares, slot.samples),
                slot.non_finite,
            );
            slot.chunks = 0;
            slot.peak = 0.0;
            slot.sum_squares = 0.0;
            slot.samples = 0;
            report
        };
        let stats = playback.stats();
        println!(
            "  第 {} 秒：收到 {chunks} 块，峰值 {peak:.4}，RMS {rms:.4}；\
             播放侧排队 {} / 已渲染 {} / 已丢弃 {} 样本{}",
            reported_second,
            stats.queued_samples,
            stats.rendered_samples,
            stats.dropped_samples,
            if non_finite > 0 {
                format!("，⚠ 其中 {non_finite} 个样本不是有限值")
            } else {
                String::new()
            }
        );
    }

    // 停机顺序：先停采集（它不再往 `pending` 里灌），再把尾巴推完，最后关播放。
    let stop_at = Instant::now();
    source.stop();
    let stop_took = stop_at.elapsed();
    let tail = {
        let mut slot = meter.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut slot.pending)
    };
    playback.push(&tail);
    // **统计必须在 close() 之前取**：`close()` 会把 `shared` 拿走（`playback.rs::close`
    // 的第一行就是 `self.shared.take()`），事后再问 `stats()` 拿到的是全 0 的默认值。
    // 早先在这里才取，于是收尾永远报"一个样本都没渲染"，明明设备线程一直在搬数据。
    let stats = playback.stats();
    let close_at = Instant::now();
    playback.close();
    let close_took = close_at.elapsed();
    println!("停止采集耗时 {stop_took:?}（上界 2 s：poll 超时 200 ms）");
    println!("关闭播放耗时 {close_took:?}（上界 2 s：播放线程的 poll 超时 200 ms）");

    println!(
        "播放侧收尾：排队 {} 样本 / 已渲染 {} 样本 / 丢弃 {} 样本 / 设备延迟 {} ms",
        stats.queued_samples,
        stats.rendered_samples,
        stats.dropped_samples,
        stats.device_latency_ms
    );

    let (total_chunks, total_samples, total_rms, non_finite) = {
        let slot = meter.lock().unwrap_or_else(|p| p.into_inner());
        (
            slot.total_chunks,
            slot.total_samples,
            rms_of(slot.total_sum_squares, slot.total_samples),
            slot.non_finite,
        )
    };
    println!("全程：收到 {total_chunks} 块 / {total_samples} 个单声道样本，RMS {total_rms:.4}");

    if non_finite > 0 {
        eprintln!("结果：FAIL —— 音频里出现了 {non_finite} 个 NaN/Inf 样本");
        std::process::exit(1);
    }
    if total_chunks == 0 {
        eprintln!(
            "结果：FAIL —— {seconds} 秒里一块音频都没收到：采集线程没有在流动\
             （avail_update / poll / readi 这一路，或播放端根本没在写）"
        );
        std::process::exit(1);
    }
    if stop_took > Duration::from_secs(2) {
        eprintln!("结果：FAIL —— stop() 花了 {stop_took:?}，超过 2 s 的有界上限");
        std::process::exit(1);
    }
    if close_took > Duration::from_secs(2) {
        eprintln!("结果：FAIL —— close() 花了 {close_took:?}，超过 2 s 的有界上限");
        std::process::exit(1);
    }
    if stats.rendered_samples == 0 {
        eprintln!("结果：FAIL —— 播放线程一个样本都没渲染出去");
        std::process::exit(1);
    }
    if same_card {
        println!("结果：PASS（流在流动、不崩、按时退出；能量为 0 是自灌回环的预期）");
    } else if total_rms < 0.001 {
        eprintln!(
            "结果：FAIL —— 两端不是同一张卡却全程静音：先查麦克风静音/增益、\
             以及采集与播放是不是真的指到了同一套硬件"
        );
        std::process::exit(1);
    } else {
        println!("结果：PASS（采到的声音真的推到播放端去了）");
    }
    Ok(())
}

/// 平方和与样本数 → RMS。样本数为 0 时报 0（而不是 NaN 或除零）。
fn rms_of(sum_squares: f64, samples: u64) -> f64 {
    if samples == 0 {
        return 0.0;
    }
    (sum_squares / samples as f64).sqrt()
}

/// `hw:CARD=Loopback,DEV=0` / `hw:Loopback,1` 里的**卡名**。
///
/// 两种写法都要认：枚举报出来的是前一种（§5.1），人手敲的常是后一种。
/// 认不出（`default` / `pcm.voxmike` 这类）就返回 `None`——**宁可不判，也不猜**。
fn card_key(name: &str) -> Option<String> {
    let rest = name.strip_prefix("hw:")?;
    let card = rest.split_once(',')?.0;
    Some(
        card.strip_prefix("CARD=")
            .unwrap_or(card)
            .trim_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .to_ascii_lowercase(),
    )
}
