//! `PlaybackSink` 的 ALSA 实现：非阻塞 PCM + 轮询写循环 + 无锁环缓冲。
//!
//! 分工与 `vox-audio-linux`（PipeWire）、`vox-audio-win`（WASAPI）两侧同口径：
//! - **流水线线程**（`push`）做重采样和铺声道，写完塞进 `DropRing`——这里允许分配；
//! - **设备线程**（`vox-playback`）只做定长搬运，不分配、不加锁、不打日志；
//! - 环里没数据就补静音（欠载不报错，语音流断一下比崩掉强）。
//!
//! 与 PipeWire 那份的两处差别（施工稿 `docs/plans/S4-C-ALSA.md` §3.4）：
//! 1. PipeWire 固定向图请求 48 kHz 立体声、由图转格式；ALSA 直开设备**没有那张图**，
//!    请求值来自 §5.2 的协商阶梯，`target_rate` 是真协商出来、如实回报给芯的。
//! 2. PipeWire 的 process 回调用 `stream.time().delay()` 报延迟；ALSA 侧在写循环里用
//!    `PCM::delay()`（`snd_pcm_delay`）刷新同一个原子量。
//!
//! **与 Windows 侧 `rates::choose_output_rate` 的口径分歧是故意的**（§5.2 末段）：那份
//! 三档全灭时退回设备默认率，这里**报错**。ALSA 阶梯的第 3 档（`set_rate_near` 的
//! Nearest）必然会拿到一个率，"退回默认率"在 ALSA 上等于替用户做了一个他没要求的重采样。
//!
//! 满量程口径按 §12 M6：`probe::I32_SCALE = 2^31`（f32 存不下 `2^31 - 1`，字面量写
//! 哪个都会舍入成 `2^31`），16 位那一路同理用 `2^15`。设备整数样本的换算在
//! `probe::f32_to_i32` 与本文件的 `f32_to_i16` 里，都是显式夹紧 + 显式 NaN 归零。
//!
//! M5（§12）落地的地方是写循环里那个**周期内游标**：短写只推进实际写出的帧数，
//! 剩下的下一轮接着写，已经写出去的帧不重写，也不为这件事另分配缓冲。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alsa::pcm::{Access, Format, Frames, HwParams, IO, PCM};
use alsa::poll::{pollfd, Descriptors};
use alsa::{Direction, ValueOr};
use vox_core::pipeline::ResampleFactory;
use vox_core::ports::{PlaybackSink, PlaybackStats, PortError, PortResult, Resample};
use vox_dsp::channels::duplicate_mono;
use vox_dsp::ring::{should_warn, DropRing};

use crate::probe::{
    f32_to_i32, format_ladder, map_err, rate_ladder, step_after, Attempt, Step,
    PLAYBACK_WANT_CHANNELS,
};

/// 环缓冲容量：5 秒（按**协商到的**设备率算，见 `playback_thread` 里建环那一行）。
const RING_SECONDS: usize = 5;
/// 等设备协商出结果的上限。超时就报错，不留一条没人管的流。
const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
/// 停流的有界等待上限。超时不 join，只打一条警告（与 `vox-audio-linux` 同一量级）。
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// `poll` 超时。**这是 `close()` 从设标志到线程真正退出的上界**，所以它必须明显
/// 小于 `STOP_TIMEOUT`（200 ms vs 2 s，留 10 倍余量，§5.3）。
const POLL_TIMEOUT_MS: i32 = 200;

/// 设备线程与流水线线程之间共享的那点状态。全部是原子量或无锁环，读写两侧都不加锁。
struct Shared {
    ring: DropRing,
    stop: AtomicBool,
    /// 累计真正从环里取走的交织样本数。**不含自动补的静音**（`ports.rs::PlaybackStats`）。
    rendered_samples: AtomicU64,
    device_latency_ms: AtomicU64,
}

pub struct AlsaPlayback {
    resample_factory: ResampleFactory,
    source_rate: u32,
    target_rate: u32,
    channels: u16,
    /// 只在 `source_rate != target_rate` 时装（§5.2 的阶梯第 3 档）。
    resampler: Option<Box<dyn Resample>>,
    /// 重采样 + 铺声道的复用缓冲（流水线线程用，允许分配但复用，同 PipeWire 那份）。
    interleave: Vec<f32>,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl AlsaPlayback {
    pub fn new(resample_factory: ResampleFactory) -> Self {
        Self {
            resample_factory,
            source_rate: 0,
            target_rate: 0,
            channels: 1,
            resampler: None,
            interleave: Vec::new(),
            shared: None,
            thread: None,
        }
    }
}

impl PlaybackSink for AlsaPlayback {
    /// 打开设备并回报**实际**采样率（C8）。协商在设备线程里做，这里有界等它（§5.3：
    /// 不许无限阻塞在 `open` / `hw_params` 里）。
    fn open(&mut self, device: Option<&str>, source_rate: u32) -> PortResult<u32> {
        // 重复 open 视为换目标：先把上一条流收干净，否则两条流同时往环里灌。
        self.close();
        if source_rate == 0 {
            return Err(PortError::new("播放源采样率不能是 0"));
        }

        let (report_tx, report_rx) = mpsc::channel::<PortResult<Opened>>();
        let device = device.map(str::to_owned);
        let thread = std::thread::Builder::new()
            .name("vox-playback".into())
            .spawn(move || playback_thread(device, source_rate, report_tx))
            .map_err(|e| PortError::new(format!("创建播放线程失败：{e}")))?;

        let opened = match report_rx.recv_timeout(OPEN_TIMEOUT) {
            // 谈成了：线程接着跑写循环，句柄留给 `close()` 去收。
            Ok(Ok(opened)) => opened,
            // 谈崩了但线程已经报完：立刻收掉，不留僵尸。
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                return Err(PortError::new("播放线程意外退出（协商还没谈完就没了）"));
            }
            Err(RecvTimeoutError::Timeout) => {
                // **不 join**：此时线程可能正卡在驱动的一次 open / hw_params 里，那不是
                // 我们有界能控制的，`join` 会把"点停止没反应"这条毛病引进 `open`。
                // 它的报告通道已经没有接收方，协商一有结论它就自己退出并把 PCM 关掉；
                // 这里直接 return，`thread` 随之析构（脱离等待）。
                return Err(PortError::new(format!(
                    "打开播放设备超时（等了 {} 秒还没就绪）",
                    OPEN_TIMEOUT.as_secs()
                )));
            }
        };

        let Opened {
            rate,
            channels,
            shared,
        } = opened;
        // 装不装重采样器**只看率是否真的相等**：阶梯第 1/2 档拿回来的就是 source_rate
        // （第 2 档是 plug 在设备侧换的率），装一个恒等重采样器等于每一句都白跑一遍 sinc。
        let resampler = if needs_resampler(source_rate, rate) {
            Some((self.resample_factory)(source_rate, rate))
        } else {
            None
        };
        self.channels = channels;
        self.source_rate = source_rate;
        self.target_rate = rate;
        self.resampler = resampler;
        self.shared = Some(shared);
        self.thread = Some(thread);
        Ok(rate)
    }

    fn push(&mut self, samples: &[f32]) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        if samples.is_empty() {
            return;
        }
        // 重采样与铺声道在流水线线程上做（允许分配、允许日志），照抄 PipeWire 那份的写法：
        // 复用 `interleave`，`clear()` 之后重填，不每次 `push` 新建一个 Vec。
        self.interleave.clear();
        if self.source_rate == self.target_rate {
            duplicate_mono(samples, self.channels, &mut self.interleave);
        } else {
            let Some(resampler) = self.resampler.as_mut() else {
                return;
            };
            let resampled = resampler.process(samples);
            duplicate_mono(&resampled, self.channels, &mut self.interleave);
        }

        // 满了丢最旧，绝不阻塞调用方（C9）。
        let dropped = shared.ring.write(&self.interleave);
        if dropped > 0 && should_warn(shared.ring.drop_events()) {
            tracing::warn!(
                "播放缓冲满了，已累计丢弃 {} 个样本（第 {} 次）",
                shared.ring.dropped_samples(),
                shared.ring.drop_events()
            );
        }
    }

    fn stats(&self) -> PlaybackStats {
        let Some(shared) = self.shared.as_ref() else {
            return PlaybackStats::default();
        };
        PlaybackStats {
            queued_samples: shared.ring.len(),
            sample_rate: self.target_rate.max(1),
            channels: self.channels,
            rendered_samples: shared.rendered_samples.load(Ordering::Acquire),
            dropped_samples: shared.ring.dropped_samples(),
            device_latency_ms: shared.device_latency_ms.load(Ordering::Acquire),
        }
    }

    /// 立刻丢掉还没放出去的（C11）。重采样器的跨块状态也要清，
    /// 否则残留的尾巴会接到下一句开头。
    fn flush(&mut self) {
        if let Some(shared) = self.shared.as_ref() {
            shared.ring.clear();
        }
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        self.interleave.clear();
    }

    fn close(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.stop.store(true, Ordering::Release);
        }
        if let Some(thread) = self.thread.take() {
            // 有界等待（C4）：设备线程最坏卡在 `poll` 的 200 ms 上，所以 2 s 绰绰有余。
            // 超时就只 warn 不 join——宁可漏一个线程，也不能让"停止"点不动。
            let deadline = Instant::now() + STOP_TIMEOUT;
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                tracing::warn!("播放线程没在 2 秒内退出，先放它自生自灭（不再 join）");
            }
        }
        self.resampler = None;
        self.interleave.clear();
    }
}

impl Drop for AlsaPlayback {
    fn drop(&mut self) {
        self.close();
    }
}

/// 协商完成后回报给 `open` 的东西。
struct Opened {
    /// 设备**真的**给到的率（C8：上层按它算延迟，报请求值就是撒谎）。
    rate: u32,
    channels: u16,
    shared: Arc<Shared>,
}

/// 阶梯里某一档协商出来的结果。
struct Negotiated {
    pcm: PCM,
    rate: u32,
    channels: u16,
    /// 一个 period 多少帧，决定一次 `writei` 的上限与两个 scratch 的长度。
    period_frames: usize,
    format: Format,
}

/// 设备线程：协商 → 回报 → 跑写循环。
fn playback_thread(device: Option<String>, wanted: u32, report: mpsc::Sender<PortResult<Opened>>) {
    let negotiated = match negotiate(device.as_deref(), wanted) {
        Ok(negotiated) => negotiated,
        Err(e) => {
            // 还没开工 → 报给 `open`（不是只记日志）。
            let _ = report.send(Err(e));
            return;
        }
    };
    let Negotiated {
        pcm,
        rate,
        channels,
        period_frames,
        format,
    } = negotiated;

    // 环按**协商到的**率算容量：5 秒的排队上限对任何档位都是 5 秒。
    let shared = Arc::new(Shared {
        ring: DropRing::new(rate as usize * channels as usize * RING_SECONDS),
        stop: AtomicBool::new(false),
        rendered_samples: AtomicU64::new(0),
        device_latency_ms: AtomicU64::new(0),
    });
    if report
        .send(Ok(Opened {
            rate,
            channels,
            shared: Arc::clone(&shared),
        }))
        .is_err()
    {
        // `open` 已经超时走了：没人要的流直接在这里关掉 PCM，别留一条野流。
        return;
    }

    // poll 描述符循环外取一次，之后只改 timeout（§5.3：**不要手工改 `events` 方向**，
    // 采集是 POLLIN、播放是 POLLOUT，方向反了的表现是"永远超时"或"永远立刻返回"）。
    let mut pfd = match Descriptors::get(&pcm) {
        Ok(pfd) => pfd,
        Err(e) => {
            tracing::error!("取播放设备的 poll 描述符失败，播放流到此为止：{e}");
            return;
        }
    };

    // `IO` 的元素类型随采样格式变（s32 → i32、s16 → i16），所以在这里分派一次，
    // 写循环本体按类型泛型化。
    if format == Format::s32() {
        match pcm.io_i32() {
            Ok(io) => write_loop(
                &pcm,
                &io,
                &shared,
                rate,
                channels,
                period_frames,
                &mut pfd,
                f32_to_i32,
            ),
            Err(e) => tracing::error!("取 32 位播放写通道失败，播放流到此为止：{e}"),
        }
    } else if format == Format::s16() {
        match pcm.io_i16() {
            Ok(io) => write_loop(
                &pcm,
                &io,
                &shared,
                rate,
                channels,
                period_frames,
                &mut pfd,
                f32_to_i16,
            ),
            Err(e) => tracing::error!("取 16 位播放写通道失败，播放流到此为止：{e}"),
        }
    } else {
        // 走不到：`negotiate` 只从 `format_ladder` 里挑格式，而阶梯里只有这两种。
        tracing::error!("设备给了一个阶梯之外的采样格式（{format}），播放流到此为止");
    }
}

/// 播放侧的率协商阶梯（§5.2）。**三档全灭就报错，不悄悄换一个率**。
///
/// `wanted_rate` 取 `open` 拿到的 `source_rate`，也就是**内核实际推上来的那个率**
/// （今天是 `protocol.rs::OUTPUT_SAMPLE_RATE = 24_000`）。用请求率而不是
/// 写死的 24 kHz：调用方要是哪天推 48 kHz 上来，设备给得起就直通，给不起就装重采样器，
/// 白插一次 24 kHz 中转是纯粹的浪费。
fn negotiate(device: Option<&str>, wanted_rate: u32) -> PortResult<Negotiated> {
    let mut last = String::from("没有试过任何一档");
    for attempt in rate_ladder(wanted_rate) {
        let name = device_for(device, &attempt);
        match open_attempt(&name, &attempt, wanted_rate) {
            Ok(negotiated) => return Ok(negotiated),
            Err(e) => {
                // 阶梯是低频动作（每次 open 一次），这里打日志不打紧。
                tracing::debug!("播放协商档 {attempt:?}（{name}）没成：{e}");
                last = e.message;
            }
        }
    }
    Err(PortError::new(format!(
        "打不开播放设备「{}」：{} Hz / {} 声道的协商三档全灭（最后一条：{last}）",
        device.unwrap_or("default"),
        wanted_rate,
        PLAYBACK_WANT_CHANNELS
    )))
}

/// 这一档拿到 `rate` 之后算不算谈成了。**纯函数**（§5.2 阶梯的判定口径）。
///
/// 前两档要求**真的**落在 `wanted_rate` 上：拿回别的率就换下一档，
/// 不许"差不多就行"——差不多就意味着我们没按自己的阶梯走。
/// 第 3 档相反：它存在的意义就是拿回设备给的任何一个率，代价是 Vox 侧重采样。
fn tier_accepts(attempt: &Attempt, rate: u32, wanted_rate: u32) -> bool {
    match attempt {
        Attempt::VoxResample { .. } => rate > 0,
        Attempt::Exact | Attempt::PlugConvert => rate == wanted_rate,
    }
}

/// 某一档怎么开这个设备。
///
/// - `Exact` / `PlugConvert`：**必须**真的落在 `wanted_rate` 上，拿回来的率不是它就换下一档；
/// - `VoxResample { probe_rate }`：拿回什么率就用什么率，装 Vox 侧重采样器。
///
/// 声道：优先 `PLAYBACK_WANT_CHANNELS`，设备只给别的就取最接近的那个并如实回报
/// （与率的阶梯同一条思路：设备给什么用什么，但上层看到的必须是真值）。
fn open_attempt(name: &str, attempt: &Attempt, wanted_rate: u32) -> PortResult<Negotiated> {
    // 非阻塞打开（§5.3）：阻塞模式下 `writei` 会一直睡，Rust 侧的 stop 标志叫不醒它。
    let pcm = PCM::new(name, Direction::Playback, true)
        .map_err(|e| map_err(&format!("打开 PCM「{name}」"), e))?;
    let hwp = HwParams::any(&pcm).map_err(|e| map_err("取硬件参数", e))?;
    hwp.set_access(Access::RWInterleaved)
        .map_err(|e| map_err("设定访问模式（只支持 RWInterleaved）", e))?;
    if hwp.set_channels(PLAYBACK_WANT_CHANNELS).is_err() {
        hwp.set_channels_near(PLAYBACK_WANT_CHANNELS)
            .map_err(|e| map_err("设定声道数", e))?;
    }
    let probe_rate = match attempt {
        Attempt::VoxResample { probe_rate } => *probe_rate,
        Attempt::Exact | Attempt::PlugConvert => wanted_rate,
    };
    // Nearest：拿回什么率就报什么率，第 3 档靠它落到设备支持的率上。
    hwp.set_rate_near(probe_rate, ValueOr::Nearest)
        .map_err(|e| map_err("设定采样率", e))?;

    // 格式阶梯（S32_LE 优先，退 S16_LE；S16 那一路的换算见 `f32_to_i16`）。
    let mut format = None;
    for candidate in format_ladder() {
        if hwp.set_format(candidate).is_ok() {
            format = Some(candidate);
            break;
        }
    }
    let Some(format) = format else {
        return Err(PortError::new(
            "设备既不收 S32_LE 也不收 S16_LE（播放侧只做这两种整数格式）",
        ));
    };

    pcm.hw_params(&hwp)
        .map_err(|e| map_err("套用硬件参数", e))?;

    // 回报**套用之后**真拿到的值，不是我们请求的值（C8 / C10）。
    let current = pcm
        .hw_params_current()
        .map_err(|e| map_err("回读硬件参数", e))?;
    let rate = current.get_rate().map_err(|e| map_err("回读采样率", e))?;
    if !tier_accepts(attempt, rate, wanted_rate) {
        return Err(PortError::new(format!(
            "这一档只给得到 {rate} Hz，不是想要的 {wanted_rate} Hz"
        )));
    }
    let channels = current
        .get_channels()
        .map_err(|e| map_err("回读声道数", e))?;
    let period_frames = current
        .get_period_size()
        .map_err(|e| map_err("回读 period", e))?;
    // `HwParams` 的 `Drop` 会碰 PCM，句柄要先放掉才能把 PCM 挪进返回值。
    drop(current);
    drop(hwp);
    Ok(Negotiated {
        pcm,
        rate,
        channels: channels.max(1) as u16,
        // period 为 0 的设备（理论上不存在）不许让循环一个帧都不写。
        period_frames: period_frames.max(1) as usize,
        format,
    })
}

/// 设备线程的主循环。
///
/// **热路径零新增分配**（`.omp/RULES.md` #6）：两个 scratch 与 `pfd` 都在进循环前一次性
/// 定死，循环里只有原子量、已分配切片的迭代器、和对 alsa-lib 的一次 FFI 调用。
/// 不打日志——日志只在 `push`（流水线线程）与致命错误路径上。
#[allow(clippy::too_many_arguments)]
fn write_loop<S: Copy>(
    pcm: &PCM,
    io: &IO<'_, S>,
    shared: &Shared,
    rate: u32,
    channels: u16,
    period_frames: usize,
    pfd: &mut [pollfd],
    to_device: fn(f32) -> S,
) {
    let channels = channels.max(1) as usize;
    let stride = period_frames * channels;
    // 协商完成后一次性分配，之后只增不减；循环里再也不 `vec!`。
    let mut scratch_f32 = vec![0.0f32; stride];
    let mut scratch_dev = vec![to_device(0.0); stride];

    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        // 欠载补静音：环里没数据时 `read_into` 会把剩下的一律填 0（语音流断一下比崩掉强）。
        // 统计只算**真取走的**那部分，不含补的静音（`ports.rs::PlaybackStats` 的口径；
        // PipeWire 侧记的也是 `read_into` 的返回值）。
        let real = shared.ring.read_into(&mut scratch_f32[..stride]);
        shared
            .rendered_samples
            .fetch_add(real as u64, Ordering::Release);

        // M5：周期内游标。短写只推进它，已经写出去的帧绝不重写。
        let mut cursor = 0usize;
        while cursor < period_frames {
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            // **先 poll 再写**：非阻塞 PCM 上位置不够就 writei 会撞 `EAGAIN`，
            // 而 `EAGAIN` 在 `step_after` 里是 Fatal（§4.3 末段）——先等就绕开了它。
            match pcm.avail_update() {
                Ok(avail) if avail >= (period_frames - cursor) as Frames => {}
                Ok(_) => {
                    wait(pfd);
                    continue;
                }
                Err(e) => {
                    if !recover_or_stop(pcm, e, "avail_update") {
                        return;
                    }
                    continue;
                }
            }

            let (from, to) = pending_span(cursor, period_frames, channels);
            for (dst, src) in scratch_dev[from..to].iter_mut().zip(&scratch_f32[from..to]) {
                *dst = to_device(*src);
            }
            match step_after(&io.writei(&scratch_dev[from..to])) {
                Step::Continue { frames } => {
                    cursor = next_cursor(cursor, frames, period_frames);
                    // 流延迟直接问 alsa-lib 要（`snd_pcm_delay`），给内核做延迟统计。
                    if let Ok(delay) = pcm.delay() {
                        shared
                            .device_latency_ms
                            .store(latency_ms(delay, rate), Ordering::Release);
                    }
                }
                Step::Underrun => {
                    // 一帧都没写出去 = 设备那一刻没位置。等一轮，别空转烧 CPU。
                    wait(pfd);
                }
                Step::Recover => {
                    let _ = pcm.recover(libc::EPIPE, true);
                    let _ = pcm.start();
                    // 游标不动：已经写出去的帧不重写（哪怕被 EPIPE 丢掉的那部分也照样不重写）。
                }
                Step::Fatal { func, errno } => {
                    tracing::error!("播放写循环退出：{func} 失败（errno {errno}）");
                    return;
                }
            }
        }
    }
}

/// 设备位置不够时的等待。`poll` 失败（fd 被关掉等）就当成"再等一轮"，
/// 靠 stop 标志出去——这里不打日志，因为打不打都不改变结果。
fn wait(pfd: &mut [pollfd]) {
    if pfd.is_empty() {
        // 没有任何描述符时 `poll` 会立刻返回 0，那会变成一个纯烧 CPU 的空转。
        std::thread::sleep(Duration::from_millis(POLL_TIMEOUT_MS as u64));
        return;
    }
    let _ = alsa::poll::poll(pfd, POLL_TIMEOUT_MS);
}

/// `avail_update` 报错时该走哪条路。返回 `true` = 还能继续。
fn recover_or_stop(pcm: &PCM, e: alsa::Error, what: &'static str) -> bool {
    match step_after(&Err(e)) {
        Step::Recover => {
            let _ = pcm.recover(libc::EPIPE, true);
            let _ = pcm.start();
            true
        }
        Step::Fatal { func, errno } => {
            tracing::error!("播放流到此为止：{what} 的 {func} 失败（errno {errno}）");
            false
        }
        // `Err` 只会落在这两支；把另两支写全是怕以后有人加分支时静默走空。
        Step::Continue { .. } | Step::Underrun => true,
    }
}

/// 某一档该用哪个 PCM 名。**纯函数**。
///
/// 设备名来自设置（§1.2），本来就是能直接喂给 `PCM::new` 的原生名，所以这里只在
/// `hw:` ↔ `plughw:` 之间换前缀；`default` / `sysdefault:` 这类由 alsa.conf 决定的名字
/// 原样留着（用户选的 plug 名也不降级回 `hw:`——降回去就验不了"设备直接给这个率"了）。
fn device_for(device: Option<&str>, attempt: &Attempt) -> String {
    let name = device.unwrap_or("default");
    let plug = matches!(attempt, Attempt::PlugConvert);
    match name.strip_prefix("hw:") {
        Some(rest) if plug => format!("plughw:{rest}"),
        _ => name.to_owned(),
    }
}

/// 要不要装重采样器：**只看率是否真的相等**。**纯函数**。
fn needs_resampler(source_rate: u32, target_rate: u32) -> bool {
    source_rate != target_rate
}

/// 本周期还欠着的那些帧（交织样本下标，半开区间 `[from, to)`）。**纯函数**，M5 的落点。
fn pending_span(cursor: usize, period_frames: usize, channels: usize) -> (usize, usize) {
    let cursor = cursor.min(period_frames);
    (cursor * channels, period_frames * channels)
}

/// 短写之后游标停在哪。**纯函数**：只推进实际写出的帧数，且不许越过周期末尾。
fn next_cursor(cursor: usize, written: usize, period_frames: usize) -> usize {
    (cursor + written).min(period_frames)
}

/// 设备报的帧延迟 → 毫秒。**纯函数**。
///
/// 与施工稿 §4.3 的式子只差 `rate == 0` 那一支：率是 0 说明协商根本没成，
/// 延迟无从换算，按 `ports.rs::PlaybackStats` 的"拿不到就是 0"报 0，
/// 而不是拿 `rate.max(1)` 算出一个 100 万毫秒的假延迟。
fn latency_ms(delay: Frames, rate: u32) -> u64 {
    if rate == 0 {
        return 0;
    }
    (delay.max(0) as u64).saturating_mul(1000) / rate as u64
}

/// f32 → 16 位设备样本。**显式夹紧 + 显式 NaN 归零**，与 `probe::f32_to_i32` 同一套口径。
///
/// 满量程取 `2^15` 而不是 `32767`（M6 的同一条理由：f32 存不下 `2^15 - 1`，
/// 字面量写哪个都会舍入成 `32768`），落点就是 16 位 PCM 的标准约定：
/// `+1.0 → i16::MAX`、`-1.0 → i16::MIN`、两边往返都精确。
const I16_SCALE: f32 = 32_768.0;

fn f32_to_i16(x: f32) -> i16 {
    let scaled = (x * I16_SCALE).clamp(-I16_SCALE, I16_SCALE);
    if scaled.is_nan() {
        0
    } else {
        scaled as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 阶梯的目标率在实现里取的是 `open` 收到的 `source_rate`（那是内核真推上来的率，
    // 见 `negotiate` 的文档），所以这个常量在库里没有别的用处——只有测试拿它跟芯的协议
    // 对账：内核改了回放率而这个常量没跟上，阶梯就在按一个已经不存在的率设计。
    use vox_core::cloud::protocol::OUTPUT_SAMPLE_RATE as PLAYBACK_WANT_RATE;

    /// 测试用重采样工厂：透传（不改变信号，只让接口能跑通）。
    fn test_resample_factory() -> ResampleFactory {
        Box::new(|_, _| Box::new(TestPassthrough) as Box<dyn Resample>)
    }

    struct TestPassthrough;

    impl Resample for TestPassthrough {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            samples.to_vec()
        }
        fn flush(&mut self) -> Vec<f32> {
            Vec::new()
        }
        fn reset(&mut self) {}
    }

    #[test]
    fn the_plug_tier_swaps_the_hw_prefix_and_the_exact_tier_does_not() {
        // 档 2（plug 换率）必须真的改设备名，否则档 1/2 试的是同一条路，阶梯白写。
        assert_eq!(
            device_for(Some("hw:CARD=PCH,DEV=0"), &Attempt::PlugConvert),
            "plughw:CARD=PCH,DEV=0"
        );
        assert_eq!(
            device_for(Some("hw:CARD=PCH,DEV=0"), &Attempt::Exact),
            "hw:CARD=PCH,DEV=0"
        );
        assert_eq!(
            device_for(
                Some("hw:CARD=PCH,DEV=0"),
                &Attempt::VoxResample { probe_rate: 24_000 }
            ),
            "hw:CARD=PCH,DEV=0"
        );
    }

    #[test]
    fn a_user_picked_plug_name_is_never_downgraded_back_to_hw() {
        // 降回 hw: 就验不了"设备直接给这个率"，第 1/2 档会变成同一档试两遍。
        assert_eq!(
            device_for(Some("plughw:CARD=PCH,DEV=0"), &Attempt::Exact),
            "plughw:CARD=PCH,DEV=0"
        );
    }

    #[test]
    fn a_null_device_stays_default_in_every_tier() {
        // `plughw:default` 不是真名字；这类由 alsa.conf 决定的名字一律原样留着。
        for attempt in rate_ladder(PLAYBACK_WANT_RATE) {
            assert_eq!(device_for(None, &attempt), "default");
            assert_eq!(device_for(Some("default"), &attempt), "default");
            assert_eq!(
                device_for(Some("sysdefault:CARD=PCH"), &attempt),
                "sysdefault:CARD=PCH"
            );
        }
    }

    #[test]
    fn the_resampler_is_installed_only_when_the_rates_really_differ() {
        // 档 2 命中时设备可能是 48 kHz，但 plug 已经在设备侧换好了，Vox 侧报 24 kHz：
        // 这时装一个 sinc 等于每一句话都白跑一遍重采样（S4 §2.3 的算力预算）。
        assert!(!needs_resampler(24_000, 24_000));
        assert!(needs_resampler(24_000, 48_000));
        assert!(needs_resampler(24_000, 44_100));
    }

    #[test]
    fn a_short_write_never_rewrites_the_frames_it_already_sent() {
        // M5：把一个 480 帧的周期故意切成 7 / 300 / 173 三次短写，
        // 每帧只准被写一次，且周期结束时正好写满。
        let period = 480usize;
        let channels = 2usize;
        let mut written = vec![false; period * channels];
        let mut cursor = 0usize;
        for frames in [7usize, 300, 173] {
            let mut left = frames;
            while left > 0 && cursor < period {
                let (from, to) = pending_span(cursor, period, channels);
                let n = left.min((to - from) / channels);
                assert!(n > 0, "游标卡住了：cursor={cursor} span={from}..{to}");
                for slot in &mut written[from..from + n * channels] {
                    assert!(!*slot, "第 {from} 格被写了两次（短写不许重写已写出的帧）");
                    *slot = true;
                }
                cursor = next_cursor(cursor, n, period);
                left -= n;
            }
        }
        assert_eq!(cursor, period, "周期必须正好写满");
        assert!(written.iter().all(|w| *w), "周期结束时还有帧没写出去");
    }

    #[test]
    fn the_cursor_never_runs_past_the_period_even_if_a_device_over_reports() {
        // 越界会让下一轮从周期外面开始读，静默丢一段音频。
        assert_eq!(next_cursor(480, 40, 480), 480);
        assert_eq!(next_cursor(400, 40, 480), 440);
        assert_eq!(pending_span(480, 480, 2), (960, 960));
        assert_eq!(pending_span(0, 480, 2), (0, 960));
        assert_eq!(pending_span(300, 480, 2), (600, 960));
    }

    #[test]
    fn latency_is_reported_in_milliseconds_and_survives_a_broken_delay() {
        assert_eq!(latency_ms(48, 48_000), 1);
        assert_eq!(latency_ms(0, 48_000), 0);
        // 负延迟（驱动偶尔会给）与 0 率（除零）都不许把上报搞成天文数字。
        assert_eq!(latency_ms(-1, 48_000), 0);
        assert_eq!(latency_ms(1_000, 0), 0);
        assert_eq!(latency_ms(96, 48_000), 2);
    }

    #[test]
    fn the_sixteen_bit_conversion_clamps_and_kills_nan() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX, "+1.0 落在正侧满量程");
        assert_eq!(f32_to_i16(-1.0), i16::MIN, "-1.0 落在负侧满量程");
        assert_eq!(f32_to_i16(2.0), i16::MAX, "越界必须夹紧");
        assert_eq!(f32_to_i16(-2.0), i16::MIN);
        assert_eq!(f32_to_i16(f32::NAN), 0, "NaN 进设备等于噪声，显式归零");
        assert_eq!(f32_to_i16(f32::INFINITY), i16::MAX);
        // 往返：0.5 必须精确回来，满量程取错（32767 vs 32768）会在这里露馅。
        assert_eq!(f32_to_i16(0.5), 16_384);
    }

    #[test]
    fn push_before_open_is_ignored() {
        let mut p = AlsaPlayback::new(test_resample_factory());
        p.push(&[0.1, 0.2]);
        p.flush();
        p.close();
        assert_eq!(p.stats(), PlaybackStats::default());
    }

    #[test]
    fn zero_source_rate_is_rejected() {
        let mut p = AlsaPlayback::new(test_resample_factory());
        let err = p.open(None, 0).unwrap_err();
        assert!(err.message.contains("不能是 0"), "{}", err.message);
    }

    #[test]
    fn a_device_that_cannot_be_opened_says_so_in_chinese() {
        // 无屏档没有界面，报错必须自解释（§1.1 C15）。这一条不需要声卡：
        // 这个名字在任何 alsa.conf 下都打不开。
        let mut p = AlsaPlayback::new(test_resample_factory());
        let err = p
            .open(Some("hw:VOX_NO_SUCH_CARD,DEV=9"), 24_000)
            .unwrap_err();
        assert!(err.message.contains("播放设备"), "{}", err.message);
        assert!(err.message.contains("三档全灭"), "{}", err.message);
    }

    #[test]
    #[ignore = "要真声卡：本机用 snd-aloop 的 Loopback 播放卡（hw:Loopback,0）"]
    fn the_loopback_card_opens_and_the_device_thread_drains_the_ring() {
        let _card = crate::LOOPBACK_CARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 本机能跑的那一条：真开一次播放面，断言最基本的事实——
        // 1) `open` 回报的是真拿到的率（C8），2) 设备线程真的在消费环里的音频，
        // 3) `close` 是有界的（停机 200 ms 的 poll 上界 + 2 s 的等待上限）。
        let mut p = AlsaPlayback::new(Box::new(|from, to| {
            Box::new(vox_dsp::Resampler::new(from, to)) as Box<dyn Resample>
        }));
        let rate = p
            .open(Some("hw:Loopback,0"), PLAYBACK_WANT_RATE)
            .expect("snd-aloop 的 Loopback 播放卡应当打得开");
        assert!(rate >= 8_000, "拿到的采样率是 {rate}");

        let stats = p.stats();
        assert_eq!(
            stats.sample_rate, rate,
            "stats 报的率必须和 open 回报的一致"
        );
        assert!(stats.channels >= 1, "声道数是 {}", stats.channels);
        println!(
            "Loopback 播放面：协商到 {rate} Hz / {} 声道",
            stats.channels
        );

        // 灌 2 秒 0.05 的常数（Loopback 的另一头没人在录，放什么都不会吵到谁），
        // 设备线程应当把队列吃掉。
        let block = vec![0.05f32; 480];
        for _ in 0..100 {
            p.push(&block);
            std::thread::sleep(Duration::from_millis(20));
        }
        let stats = p.stats();
        assert!(
            stats.rendered_samples > 0,
            "两秒音频推完了，一个样本都没被设备线程取走：{stats:?}"
        );

        // flush 之后队列里不该还留着东西。
        p.flush();
        assert_eq!(p.stats().queued_samples, 0, "flush 之后队列必须是空的");

        let start = Instant::now();
        p.close();
        let elapsed = start.elapsed();
        assert!(
            elapsed < STOP_TIMEOUT,
            "close() 花了 {elapsed:?}，超过 {STOP_TIMEOUT:?} 的有界上限"
        );
    }

    #[test]
    fn a_tier_only_passes_when_it_really_got_the_rate_it_asked_for() {
        // 前两档拿到别的率就必须换下一档（"差不多就行"等于没过阶梯）；
        // 第 3 档反过来，拿到什么率都算数——它本来就是花钱买这个的。
        assert!(tier_accepts(&Attempt::Exact, 24_000, 24_000));
        assert!(tier_accepts(&Attempt::PlugConvert, 24_000, 24_000));
        assert!(
            !tier_accepts(&Attempt::Exact, 22_050, 24_000),
            "档 1/档 2 拿到别的率还收下，就等于在按一个没谈成的率开流"
        );
        assert!(!tier_accepts(&Attempt::PlugConvert, 48_000, 24_000));
        assert!(tier_accepts(
            &Attempt::VoxResample { probe_rate: 24_000 },
            48_000,
            24_000
        ));
        assert!(tier_accepts(
            &Attempt::VoxResample { probe_rate: 24_000 },
            22_050,
            24_000
        ));
        assert!(
            !tier_accepts(&Attempt::VoxResample { probe_rate: 24_000 }, 0, 24_000),
            "率是 0 说明没协商成功，任何一档都不该收下"
        );
    }

    #[test]
    #[ignore = "要真声卡：本机用 snd-aloop 的 Loopback 播放卡（hw:Loopback,0）"]
    fn an_unusual_rate_still_opens_and_reports_what_the_card_really_gave() {
        let _card = crate::LOOPBACK_CARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 用一个"不太可能"的自定义率去开播放面（`protocol.rs` 改率、别处传了别的率时
        // 就是这条路径），断言的还是那三件最基本的事：打得开、回报的是真拿到的率、
        // 设备线程真的在把环里的东西写出去。
        //
        // **这一条在本机够不到阶梯第 3 档**：snd-aloop 给什么率都收（实测 24 kHz、
        // 17 kHz、3 MHz 全都原样给），所以第 1 档就谈成了，Vox 侧重采样器不会被装上。
        // 第 3 档的判定口径由 `a_tier_only_passes_when_it_really_got_the_rate_it_asked_for`
        // 钉住；要真跑第 3 档得换一张率表受约束的卡。
        let odd_rate = 17_000u32;
        let mut p = AlsaPlayback::new(Box::new(|from, to| {
            Box::new(vox_dsp::Resampler::new(from, to)) as Box<dyn Resample>
        }));
        let rate = p
            .open(Some("hw:Loopback,0"), odd_rate)
            .expect("非标准的率也该谈得下来（三档里总有一档能给）");
        assert!(rate >= 8_000, "拿到的采样率是 {rate}");
        assert_eq!(
            p.stats().sample_rate,
            rate,
            "stats 报的率必须和 open 回报的一致"
        );
        // 推到设备线程真的在干活：推 0.5 秒，再看有没有被取走。
        let block = vec![0.05f32; 400];
        for _ in 0..25 {
            p.push(&block);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            p.stats().rendered_samples > 0,
            "设备线程一个样本都没取走：{:?}",
            p.stats()
        );
        println!("Loopback 播放面：要 {odd_rate} Hz，设备给了 {rate} Hz");
        p.close();
    }
}
