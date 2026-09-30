//! `CaptureSource` 的 ALSA 实现：直开 PCM 采集麦克风。
//!
//! 与 PipeWire 那份的分工（`docs/plans/S4-C-ALSA.md` §6）：**无屏档缺省 ALSA**。
//! ALSA 直开设备**没有混音图**，所以"请求什么格式"和"设备给什么格式"之间没有
//! 图来转换，阶梯必须自己走一遍（`capture_ladder` + `probe::format_ladder`）。
//!
//! 决定这里怎么写的四条：
//!
//! 1. **非阻塞打开 + poll**（§5.3）：`PCM::new(.., true)`。阻塞模式下
//!    `snd_pcm_readi` 会一直睡，Rust 侧的 stop flag 叫不醒它，唯一能停的办法是从
//!    另一个线程对同一个句柄 `snd_pcm_drop`——那是跨线程操作同一个句柄，本文件不碰。
//!    poll 超时 200 ms 是 `stop()` 从设 flag 到线程退出的**上界**。
//! 2. **`readi` 之后一律按它真搬到的帧数处理**（§12 M5）：只有 `Ok(0)` 是欠载，
//!    `Ok(n>0)` 就是真数据，哪怕 `n` 小于本轮的请求量——那是这段缓冲里真实存在的
//!    `n` 帧，丢掉就是丢音频。
//! 3. **`Blocker` 用协商到的率，不用请求的率**（§5.7）：阶梯第 3 档两者不等，
//!    传错的后果是 `AudioChunk.sample_rate` 报错的率，芯里的重采样器按错的比率工作，
//!    译音变调。见 `block_plan`。
//! 4. **设备线程只搬数据**（§4.4）：缓冲、poll 描述符、`Blocker` 的 staging
//!    全在协商完成后一次性分配，循环里没有 `vec!` / `format!` / `clone` /
//!    `tracing::`。日志只在 `start`（流水线线程）与致命错误路径上打。
//!
//! `CaptureTarget` 的三条分支（§5.5）：麦克风走这里；"按程序抓音"与"网络音频"
//! 各报一句能看懂的话，**不静默降级到默认麦克风**。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alsa::pcm::{Access, Format, HwParams, IO, PCM};
use alsa::poll::{poll, pollfd, Descriptors};
use alsa::{Direction, ValueOr};
use vox_core::ports::{
    AudioChunk, CaptureFormat, CaptureSource, CaptureTarget, PortError, PortResult,
};
use vox_dsp::chunk::Blocker;

use crate::probe::{self, map_err, Step};

/// 等设备协商好并进入采集的上限（跟两个兄弟实现同一个量级）。
const START_TIMEOUT: Duration = Duration::from_secs(8);
/// `stop()` 等采集线程退出的上限。
///
/// 采集线程最坏卡在一次 `poll` 的 `POLL_TIMEOUT_MS` 上，所以 200 ms 的上界给这里
/// 留了 10 倍余量。**到点还没退出就只记一条警告、不 join**：宁可漏一个线程
/// （它自己看到 stop flag 就退了），也不能让用户点"停止"没反应。
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// `stop()` 轮询线程是否结束的间隔。
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// `poll` 的超时（毫秒）。**这是 stop 延迟的根**（§5.3）。
///
/// 循环里没有数据时整条线程就停在这一行上，所以"设 flag → 线程退出"的上界就是它。
/// 更小的值能缩短停机延迟，代价是每轮多一次系统调用；200 ms 是拍的值，
/// 等有了板子能测停机延迟再调。
const POLL_TIMEOUT_MS: i32 = 200;

/// 打开 PCM 时第三个参数的取值：**非阻塞**。理由见模块头第 1 条。
const NONBLOCK: bool = true;

/// 系统缺省 PCM 的名字。`CaptureTarget::Microphone(None)` 就用它。
const DEFAULT_PCM: &str = "default";

/// `hw:` 前缀。设置里存的设备名是它开头的（`registry.rs` 报 `hw:CARD=…,DEV=…`），
/// 阶梯第 2 档的 `plughw:` 就是拿它换前缀换出来的。
const HW_PREFIX: &str = "hw:";

// --- 计划 -------------------------------------------------------------------

/// 协商阶梯里的一档：**一份按顺序试的候选**（纯函数，见 `capture_ladder`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    /// 交给 `PCM::new` 的名字。
    pcm: String,
    /// 这一档是不是"必须拿到 48000 整"：拿不到就往下一档退（阶梯第 1/2 档）。
    /// 第 3 档为 `false`——它存在的意义就是接受设备给的任何率。
    exact: bool,
}

/// `start` 时定下来、之后线程照着做的采集计划。
#[derive(Debug)]
struct Plan {
    /// 用户点的设备名（`None` = 系统缺省），只进错误消息。
    requested: Option<String>,
    /// 按顺序试的候选设备。
    candidates: Vec<Candidate>,
}

/// 采集侧的协商阶梯（纯函数）。**为什么采集侧也有阶梯**：ALSA 直开没有图帮我们
/// 换率，设备只给硬件支持的格式（§5.2）。三档与施工稿的表逐条对应：
///
/// | 档 | 候选 | 条件 |
/// | --- | --- | --- |
/// | 1 | `hw:<id>` | 拿回的率就是 48000（降噪的原生率） |
/// | 2 | `plughw:<id>` | 同上，但由 plug 插件换率；我们仍然按 48000 推，芯那边照旧收 |
/// | 3 | `hw:<id>` | 拿回什么率就用什么率，交给芯重采样到 16 kHz |
///
/// 三档全灭 → 报错，**不静默换默认率**。
///
/// `Some(name)` 但名字不是 `hw:` 开头（用户自己在 `asoundrc` 里定义的 PCM、
/// 或者本来就是 `default`）时**只试它一个**：不把用户点名的设备改写成另一个
/// （`pcm.foo` 与 `hw:` 家族之间没有可靠的对应关系，改写等于替用户换设备）。
fn capture_ladder(device: Option<&str>) -> Vec<Candidate> {
    let plain = |pcm: &str, exact: bool| Candidate {
        pcm: pcm.to_string(),
        exact,
    };
    match device {
        // 系统缺省：用户没点名任何设备，就按缺省原样开一个，拿回什么率报什么率。
        // 不去猜 `hw:default` / `plughw:default`——那是在替用户挑设备。
        None => vec![plain(DEFAULT_PCM, false)],
        Some(name) if name == DEFAULT_PCM => vec![plain(name, false)],
        Some(name) if name.starts_with(HW_PREFIX) => vec![
            plain(name, true),
            plain(&plug_name(name), true),
            plain(name, false),
        ],
        Some(name) => vec![plain(name, false)],
    }
}

/// `hw:CARD=…,DEV=…` → `plughw:CARD=…,DEV=…`（同 id 的 plug 打开方式）。
fn plug_name(hw: &str) -> String {
    format!("plug{hw}")
}

/// 把内核给的 `CaptureTarget` 翻成采集计划。**三条分支一个都不能少**（§5.5 / C7）。
fn resolve_plan(target: &CaptureTarget) -> PortResult<Plan> {
    match target {
        CaptureTarget::Microphone(name) => Ok(Plan {
            candidates: capture_ladder(name.as_deref()),
            requested: name.clone(),
        }),
        // ALSA 不记录"谁在出声"，没有按程序抓音这个概念。这句错误是能力位降级在
        // 端口层的落点：无屏档 `program_tap` 在上限之外，用户真去选了那一格时，
        // 拿到的是一句能看懂的话，而不是 `unsupported` 或者一个空列表。
        CaptureTarget::ProcessLoopback { .. } => Err(PortError::new(
            "ALSA 没有「抓某个程序的声音」这个能力（它不记录谁在出声）。\
             这一格要按程序抓音请用 PipeWire 后端。",
        )),
        // 网络采集目标（媒体面）不归设备采集实现：它是 `vox-net` 的监听侧。
        // 遇到它必须报错，**不许当成默认设备**——那是静默换源。
        CaptureTarget::Net { .. } => Err(PortError::new(
            "网络音频不归 ALSA 采集实现（由 vox-net 的媒体面提供）。",
        )),
    }
}

// --- 端口 -------------------------------------------------------------------

struct Shared {
    stop: AtomicBool,
    /// 启动回报通道。协商结果、启动错误都从这儿出去；谁先拿到谁负责回报。
    report: Mutex<Option<mpsc::Sender<PortResult<CaptureFormat>>>>,
}

struct Running {
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

/// 采集源。`start` 之后音频块通过回调推给内核，`stop` 保证回调不再触发。
pub struct AlsaCapture {
    running: Option<Running>,
}

impl AlsaCapture {
    pub fn new() -> Self {
        Self { running: None }
    }
}

impl Default for AlsaCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSource for AlsaCapture {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat> {
        // 重复 start 视为换目标：先把旧的收干净，否则两个流会同时往回调里灌数据。
        self.stop();

        let plan = resolve_plan(target)?;
        let (report_tx, report_rx) = mpsc::channel::<PortResult<CaptureFormat>>();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            report: Mutex::new(Some(report_tx)),
        });

        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("vox-capture".into())
            .spawn(move || capture_thread(plan, block_ms, thread_shared, on_chunk))
            .map_err(|e| PortError::new(format!("创建采集线程失败：{e}")))?;

        self.running = Some(Running { shared, thread });

        // 有界等待：协商要开 PCM、设硬件参数，失败必须能变成 `PortError` 回传给
        // `start`，而不是在线程里默默失败让上层干等。
        match report_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(negotiated)) => Ok(negotiated),
            Ok(Err(e)) => {
                self.stop();
                Err(e)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                Err(PortError::new(format!(
                    "采集启动超时（等了 {} 秒还没就绪）",
                    START_TIMEOUT.as_secs()
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.stop();
                Err(PortError::new("采集线程意外退出"))
            }
        }
    }

    fn stop(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        running.shared.stop.store(true, Ordering::Release);
        // 线程退出即代表回调不会再触发——这是 trait 契约里唯一能给的保证。
        //
        // 有界等待的理由见 `STOP_TIMEOUT`：最坏是它卡在一次 200 ms 的 `poll` 上，
        // 两秒足够。真的超时了就只记警告、不 join。
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !running.thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(STOP_POLL_INTERVAL);
        }
        if running.thread.is_finished() {
            let _ = running.thread.join();
        } else {
            tracing::warn!("采集线程没在 2 秒内退出，先放它自生自灭（不再 join）");
        }
    }
}

impl Drop for AlsaCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// --- 启动回报 ---------------------------------------------------------------

/// 把协商结果回报给 `start()`。只有第一次有效（通道被取走后就没了）。
fn report_started(shared: &Shared, negotiated: CaptureFormat) {
    if let Some(report) = shared.report.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = report.send(Ok(negotiated));
    }
}

/// 把失败回报给 `start()`。返回 `true` 表示确实发出去了（说明还没开工）。
fn report_failed(shared: &Shared, message: String) -> bool {
    match shared.report.lock().ok().and_then(|mut slot| slot.take()) {
        Some(report) => {
            let _ = report.send(Err(PortError::new(message)));
            true
        }
        None => false,
    }
}

// --- 采集线程 ---------------------------------------------------------------

fn capture_thread(
    plan: Plan,
    block_ms: u32,
    shared: Arc<Shared>,
    on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) {
    let result = run_capture(plan, block_ms, &shared, on_chunk);
    if let Err(e) = result {
        // 还没回报过就交给 `start()`（它在等这个通道）；已经开工了就只记日志。
        if !report_failed(&shared, e.message.clone()) {
            tracing::error!("采集线程退出：{e}");
        }
    }
}

/// 跑一条采集流：走阶梯 → 回报协商结果 → 建缓冲 → 跑 `readi` 循环。
fn run_capture(
    plan: Plan,
    block_ms: u32,
    shared: &Shared,
    mut on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) -> PortResult<()> {
    let Opened {
        pcm,
        format,
        negotiated,
        period_frames,
    } = negotiate(&plan)?;

    // 协商完成 → 缓冲一次性定死大小，之后循环里只读不涨。
    let block = block_plan(negotiated, block_ms);
    let channels = negotiated.channels as usize;
    let period_samples = period_frames * channels;
    let mut scratch_f = vec![0.0f32; period_samples];
    let mut pfd = pcm.get().map_err(|e| map_err("取 poll 描述符", e))?;

    // 回报放在下面两档各自的 `pcm.start()` **之后**：回报的是"真协商到的"格式
    // （不是请求值，芯按这个率重采样到 16 kHz，报请求值会让译音变调），而回报
    // 一旦发出，通道就被取走了——所以能把"开读写口/启动"这两步的失败原样带回给
    // `start()` 的回报，只能在回报之前。回报晚了只会让上层白等 8 秒超时。
    match format {
        // 32 位：阶梯第 1 档。廉价 USB 麦在 16 位下很容易削顶，削顶发生在驱动里，
        // 我们拿回来就只剩 0 了（`probe::format_ladder` 的理由）。
        f if f == Format::s32() => {
            let io = pcm.io_i32().map_err(|e| map_err("开 32 位读写口", e))?;
            // 硬件参数谈成、读写口开好之后必须显式 `start`：停在 PREPARED 状态时
            // `avail_update` 一直报 0，循环里只能一路 poll 到停机。
            pcm.start()
                .map_err(|e| map_err("启动采集流（snd_pcm_start）", e))?;
            report_started(shared, negotiated);
            let mut scratch_i = vec![0i32; period_samples];
            pump(
                &pcm,
                &io,
                shared,
                period_frames,
                channels,
                &block,
                &mut scratch_i,
                &mut scratch_f,
                &mut pfd,
                &mut *on_chunk,
            )
        }
        // 16 位：阶梯第 2 档，只有 32 位打不开时才用。
        f if f == Format::s16() => {
            let io = pcm.io_i16().map_err(|e| map_err("开 16 位读写口", e))?;
            // 同 32 位那条：先 start 再进循环。
            pcm.start()
                .map_err(|e| map_err("启动采集流（snd_pcm_start）", e))?;
            report_started(shared, negotiated);
            let mut scratch_i = vec![0i16; period_samples];
            pump(
                &pcm,
                &io,
                shared,
                period_frames,
                channels,
                &block,
                &mut scratch_i,
                &mut scratch_f,
                &mut pfd,
                &mut *on_chunk,
            )
        }
        // 走不到：格式只从 `probe::format_ladder()` 那两档里出。
        // 仍然给一条读得懂的错误，而不是 `unreachable!()`——库里不许 panic。
        other => Err(PortError::new(format!(
            "ALSA 协商出了这一稿不处理的采样格式：{other:?}"
        ))),
    }
}

/// 一块该喂给 `Blocker` 的东西。**率取的是协商到的那个**（§5.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockPlan {
    sample_rate: u32,
    channels: u16,
    block_ms: u32,
    /// 一块的交织样本数。
    block_samples: usize,
}

/// 协商结果 → 喂 `Blocker` 的参数。**纯函数**，`Blocker::new` 的实参全部由它给出。
///
/// 把这一步单独拆出来是为了让"块长按协商率算"这件事能被脱开硬件测：
/// 采集侧一旦把请求的 48000 传给阶梯第 3 档（设备实际给了别的率），
/// 块长就和 `Blocker` 内部那套对不上，`AudioChunk` 报的率也跟着错。
fn block_plan(negotiated: CaptureFormat, block_ms: u32) -> BlockPlan {
    BlockPlan {
        sample_rate: negotiated.sample_rate,
        channels: negotiated.channels,
        block_ms,
        block_samples: probe::block_frames(negotiated.sample_rate, block_ms, negotiated.channels),
    }
}

// --- 协商阶梯 ---------------------------------------------------------------

/// 候选设备开成功之后拿到的东西。
struct Opened {
    pcm: PCM,
    /// 真正谈成的采样格式，决定用哪条 `io` 通路。
    format: Format,
    /// 真协商到的率与声道数，**原样回报给上层**。
    negotiated: CaptureFormat,
    /// 一个采集周期的帧数（不是样本数）。
    period_frames: usize,
}

/// 走完阶梯。**全灭时报错并把每一档的原因都带上**：无屏档没有界面，
/// 运维只看得到 journal，错误必须自解释。
fn negotiate(plan: &Plan) -> PortResult<Opened> {
    let mut reasons: Vec<String> = Vec::with_capacity(plan.candidates.len());
    for candidate in &plan.candidates {
        match open_candidate(candidate) {
            Ok(opened) => return Ok(opened),
            Err(e) => reasons.push(format!("{} → {}", candidate.pcm, e.message)),
        }
    }
    let asked = plan.requested.as_deref().unwrap_or(DEFAULT_PCM);
    Err(PortError::new(format!(
        "打不开采集设备「{asked}」（试了 {} 档）：{}",
        plan.candidates.len(),
        reasons.join("；")
    )))
}

/// 试一个候选设备：非阻塞打开 → 走格式阶梯 → 设硬件参数 → 回报协商结果。
fn open_candidate(candidate: &Candidate) -> PortResult<Opened> {
    let pcm = PCM::new(&candidate.pcm, Direction::Capture, NONBLOCK)
        .map_err(|e| map_err("打开 PCM", e))?;

    // 格式阶梯在内层：同一张卡上先试 32 位，再退 16 位。
    let mut last: Option<PortError> = None;
    for format in probe::format_ladder() {
        match try_hw_params(&pcm, format) {
            Ok((negotiated, period_frames)) => {
                // 阶梯第 1/2 档的要义：拿不到 48000 整就往下一档退，而不是
                // 拿一个"大概是 48000"的率去开工（第 3 档才接受任何率）。
                if candidate.exact && negotiated.sample_rate != probe::CAPTURE_WANT_RATE {
                    last = Some(PortError::new(format!(
                        "这一档没给到 {} 整（给的是 {}）",
                        probe::CAPTURE_WANT_RATE,
                        negotiated.sample_rate
                    )));
                    continue;
                }
                return Ok(Opened {
                    pcm,
                    format,
                    negotiated,
                    period_frames,
                });
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| PortError::new("采样格式阶梯是空的：没谈成任何格式（不该发生）")))
}

/// 在已打开的 PCM 上谈一次硬件参数。**报告的率与声道是 `hw_params` 谈完之后读回来的**，
/// 不是我们请求的值。
fn try_hw_params(pcm: &PCM, format: Format) -> PortResult<(CaptureFormat, usize)> {
    let hwp = HwParams::any(pcm).map_err(|e| map_err("取硬件参数", e))?;
    // `RWInterleaved` 是所有驱动都实现的一种（§2.1 为什么不用 mmap）。
    hwp.set_access(Access::RWInterleaved)
        .map_err(|e| map_err("设访问方式（交织读写）", e))?;
    // 声道用 `_near`：麦克风有单声道也有双声道的（`CAPTURE_WANT_CHANNELS` 想要 1），
    // 拿不到 1 就退到设备给的数并**如实回报**——交织多声道由芯 `AudioChunk::to_mono` 混，
    // 比"整张卡都开不了"强。`set_channels` 在这种卡上会直接失败。
    hwp.set_channels_near(probe::CAPTURE_WANT_CHANNELS)
        .map_err(|e| map_err("设声道数", e))?;
    // `ValueOr::Nearest`：拿不到 48000 就给设备支持的最近值，我们读回来看是哪个。
    hwp.set_rate(probe::CAPTURE_WANT_RATE, ValueOr::Nearest)
        .map_err(|e| map_err("设采样率", e))?;
    hwp.set_format(format)
        .map_err(|e| map_err("设采样格式", e))?;
    pcm.hw_params(&hwp)
        .map_err(|e| map_err("应用硬件参数", e))?;

    let sample_rate = hwp.get_rate().map_err(|e| map_err("读回采样率", e))?;
    let channels = hwp.get_channels().map_err(|e| map_err("读回声道数", e))?;
    let period_frames = hwp
        .get_period_size()
        .map_err(|e| map_err("读回周期长度", e))?;
    if sample_rate == 0 || channels == 0 || period_frames <= 0 {
        return Err(PortError::new(format!(
            "硬件参数谈成了个没用的组合：率 {sample_rate}、声道 {channels}、周期 {period_frames} 帧"
        )));
    }
    Ok((
        CaptureFormat {
            sample_rate,
            channels: channels as u16,
        },
        period_frames as usize,
    ))
}

// --- 采集循环 ---------------------------------------------------------------

/// 设备整数样本 → f32。**采集侧唯一的换算**（32 位走 `probe::i32_to_f32`）。
trait Sample: Copy {
    fn to_f32(self) -> f32;
}

impl Sample for i32 {
    fn to_f32(self) -> f32 {
        probe::i32_to_f32(self)
    }
}

impl Sample for i16 {
    /// 16 位的满量程约定与 32 位那边一致：`-32768 ↔ -1.0`、`+32767 ↔ 接近 +1.0`。
    fn to_f32(self) -> f32 {
        self as f32 * (1.0 / 32_768.0)
    }
}

/// 采集主循环。**热路径**：进来之前缓冲、`poll` 描述符、`Blocker` 都已分配好，
/// 这里一行分配都没有。
#[allow(clippy::too_many_arguments)]
fn pump<S: Sample>(
    pcm: &PCM,
    io: &IO<S>,
    shared: &Shared,
    period_frames: usize,
    channels: usize,
    block: &BlockPlan,
    scratch_i: &mut [S],
    scratch_f: &mut [f32],
    pfd: &mut [pollfd],
    on_chunk: &mut dyn FnMut(AudioChunk),
) -> PortResult<()> {
    let mut blocker = Blocker::new(block.sample_rate, block.channels, block.block_ms);

    loop {
        // 热路径里唯一的状态读。放最前面：停机延迟就是从这里开始算的。
        if shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }

        // 1) 手上有没有数据。
        //
        // 门限是 `> 0` 而不是"够不够一个周期"：`avail_update` 在有些驱动上会返回
        // 不足一个周期的量（尤其是周期被 `sw_params` 改过之后），按"够一个周期"
        // 判会一直落进 poll 分支、把已经可读的数据饿死。读到 `min(avail, 一个周期)`
        // 既不会超缓冲，也保证这一轮之后 `avail` 归零、下轮一定进 poll（不会空转）。
        let avail = match pcm.avail_update() {
            Ok(frames) => frames.max(0) as usize,
            Err(e) => {
                after_io_error(pcm, e, pfd, "查可用的采集量（snd_pcm_avail_update）")?;
                continue;
            }
        };
        if avail == 0 {
            // 数据还没到。`poll` 的方向由 alsa-lib 自己填好，我们只给超时（§5.3）。
            if let Err(e) = poll(pfd, POLL_TIMEOUT_MS) {
                // 信号打断（EINTR）不是设备故障，接着等一轮；别的错这条流没法继续。
                if e.errno() != libc::EINTR {
                    return Err(map_err("等待采集数据（poll）", e));
                }
            }
            continue;
        }

        // 2) 有数据就读一读，读多少帧由设备说了算（§12 M5）。
        let want_frames = avail.min(period_frames);
        let want_samples = want_frames * channels;
        let res = io.readi(&mut scratch_i[..want_samples]);
        match probe::step_after(&res) {
            Step::Continue { frames } => {
                // `readi` 报的是**帧**；换成交织样本数才能切片（§4.2 末段那处坑）。
                // `.min` 是防御：真搬回来的帧数不可能超过请求的。
                let moved = frames.min(want_frames) * channels;
                for (dst, src) in scratch_f[..moved].iter_mut().zip(&scratch_i[..moved]) {
                    *dst = src.to_f32();
                }
                blocker.feed(&scratch_f[..moved], on_chunk);
            }
            // 一帧都没读到：这一轮什么都不做，接着去 poll（不是错）。
            Step::Underrun => {}
            Step::Recover | Step::Fatal { .. } => {
                // 判读只在 `res` 是 `Err` 时给这两支，所以这里拿得到的必然是 `Err`。
                let Err(e) = res else {
                    return Err(PortError::new("ALSA 采集：io 结果与判读对不上"));
                };
                after_io_error(pcm, e, pfd, "读取采集数据（snd_pcm_readi）")?;
            }
        }
    }
}

/// io 报错了怎么走。**判读口径只有一份**（`probe::step_after`），这里额外需要的是
/// "到底是哪个 errno"——`snd_pcm_recover` 对 `EPIPE` 走 `prepare`、对 `ESTRPIPE`
/// 走 `resume`，传错就修不好那一路。
///
/// 可恢复的当场恢复并回到循环；不可恢复的返回 `Err`，由 `capture_thread` 决定
/// 是回报给 `start()` 还是只记一条日志。
fn after_io_error(pcm: &PCM, e: alsa::Error, pfd: &mut [pollfd], what: &str) -> PortResult<()> {
    let errno = e.errno();
    // 这里的一次性分配不是热路径：只有 io 真的报错了才会走到，一次爆音/一次拔线
    // 才一次。换来的是错误消息自解释——无屏档没有界面，运维只看得到 journal。
    let text = e.to_string();
    match probe::step_after(&Err(e)) {
        Step::Recover => {
            pcm.recover(errno, true)
                .map_err(|e| map_err("恢复采集流（snd_pcm_recover）", e))?;
            pcm.start()
                .map_err(|e| map_err("重启采集流（snd_pcm_start）", e))?;
            // 恢复之后 poll 描述符有可能被换掉，原地重填一次（不重新分配）。
            let _ = pcm.fill(pfd);
            Ok(())
        }
        Step::Fatal { func, errno } => Err(PortError::new(format!(
            "ALSA {what} 失败：{text}（{func}，errno {errno}）"
        ))),
        // `step_after` 对 `Err` 只可能给上面两支。落到这里说明判读与调用点脱节了，
        // 报出来而不是当作"成功"继续跑。
        Step::Underrun | Step::Continue { .. } => Err(PortError::new(format!(
            "ALSA {what} 的错误判读结果对不上：{text}（errno {errno}）"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单元测试用的块长，与 `INPUT_BLOCK_MS`（`vox_core::pipeline`）一致。
    const BLOCK_MS: u32 = 20;

    #[test]
    fn the_capture_ladder_is_hw_then_plughw_then_hw_at_any_rate() {
        assert_eq!(
            capture_ladder(Some("hw:CARD=PCH,DEV=0")),
            vec![
                Candidate {
                    pcm: "hw:CARD=PCH,DEV=0".to_string(),
                    exact: true
                },
                Candidate {
                    pcm: "plughw:CARD=PCH,DEV=0".to_string(),
                    exact: true
                },
                Candidate {
                    pcm: "hw:CARD=PCH,DEV=0".to_string(),
                    exact: false
                },
            ],
            "三档的顺序与 exact 标志就是 §5.2 那张表：先 hw 整率、再 plug 整率、最后 hw 随便率"
        );
    }

    #[test]
    fn the_default_device_is_opened_as_is_never_rewritten() {
        // `Microphone(None)` 与 `Microphone(Some("default"))` 是一回事：用户没点名设备，
        // 就按缺省原样开一个，不去猜 `hw:default` / `plughw:default`——那是在替用户挑设备。
        for device in [None, Some("default")] {
            assert_eq!(
                capture_ladder(device),
                vec![Candidate {
                    pcm: "default".to_string(),
                    exact: false
                }],
                "缺省设备只有一档，且不要求 48000 整（拿回什么率就报什么率）"
            );
        }
    }

    #[test]
    fn a_user_named_pcm_is_never_rewritten_into_the_hw_family() {
        // `pcm.voxmike` 这类自定义 PCM 与 `hw:` 家族之间没有可靠对应，改写等于替用户换设备。
        assert_eq!(
            capture_ladder(Some("pcm.voxmike")),
            vec![Candidate {
                pcm: "pcm.voxmike".to_string(),
                exact: false
            }]
        );
    }

    #[test]
    fn program_loopback_and_net_targets_are_refused_with_a_readable_reason() {
        // 这两条是能力位降级在端口层的落点：报错要说得清"为什么不行、该换什么后端"。
        let loopback = resolve_plan(&CaptureTarget::ProcessLoopback {
            executable: "chrome".to_string(),
            include_tree: true,
        })
        .expect_err("ALSA 没有按程序抓音这个能力");
        assert!(
            loopback.message.contains("PipeWire") && loopback.message.contains("程序"),
            "要说清缺的是什么、该换谁：{}",
            loopback.message
        );

        let net = resolve_plan(&CaptureTarget::Net {
            pipe: "default".to_string(),
        })
        .expect_err("网络音频不归设备采集实现");
        assert!(
            net.message.contains("vox-net"),
            "要说清谁来实现它：{}",
            net.message
        );
    }

    #[test]
    fn the_block_length_follows_the_negotiated_rate_not_the_requested_one() {
        // §5.7 的钉子：阶梯第 3 档下请求的 48000 与设备给的 44100 不等，
        // 块长必须按 44100 算，否则 AudioChunk 报错的率会让芯重采样变调。
        let negotiated = CaptureFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let block = block_plan(negotiated, BLOCK_MS);
        assert_eq!(block.sample_rate, 44_100);
        assert_eq!(block.channels, 2);
        let shared = Blocker::new(44_100, 2, BLOCK_MS);
        assert_eq!(
            block.block_samples,
            shared.frames_per_block() * 2,
            "块长必须与 Blocker 自己算的一致"
        );
        assert_eq!(
            block.block_samples,
            probe::block_frames(44_100, BLOCK_MS, 2)
        );
        // 证明这条测试不是恒真的：按请求率 48000 算出来的块长是不同的数。
        assert_ne!(
            block.block_samples,
            probe::block_frames(48_000, BLOCK_MS, 2)
        );
    }

    #[test]
    fn an_unknown_device_name_is_refused_instead_of_falling_back_to_the_default() {
        // 用户点名了设备就必须认它：打不开就报错，绝不悄悄去抓默认源
        // （静默换源，§5.5）。这条不需要声卡：alsa-lib 对一个不存在的 PCM 名直接报错。
        let mut capture = AlsaCapture::new();
        let err = capture
            .start(
                &CaptureTarget::Microphone(Some("definitely-not-a-device".to_string())),
                BLOCK_MS,
                Box::new(|_| {}),
            )
            .expect_err("不存在的设备必须报错");
        assert!(
            err.message.contains("definitely-not-a-device"),
            "错误里要带上用户点的那个名字：{}",
            err.message
        );
    }

    /// 真机用例：打开本机 `snd-aloop` 的采集侧（`hw:Loopback,1`），跑一小段，
    /// 断言回报的格式与每一块音频对得上。
    ///
    /// 需要内核模块（`sudo modprobe snd-aloop`）与一个 Loopback 卡，默认跳过。
    /// 跑法：`cargo test -p vox-audio-alsa -- --ignored capture_opens_the_real_loopback_device`
    #[test]
    #[ignore = "要真的 snd-aloop 卡（本机第 3 张卡 Loopback 的采集侧）"]
    fn capture_opens_the_real_loopback_device() {
        const LOOPBACK_CAPTURE: &str = "hw:Loopback,1";
        let (tx, rx) = mpsc::channel::<AudioChunk>();
        let mut capture = AlsaCapture::new();
        let negotiated = capture
            .start(
                &CaptureTarget::Microphone(Some(LOOPBACK_CAPTURE.to_string())),
                BLOCK_MS,
                Box::new(move |chunk| {
                    let _ = tx.send(chunk);
                }),
            )
            .expect("本机的 snd-aloop 采集侧应该能打开");

        assert!(negotiated.sample_rate > 0 && negotiated.channels >= 1);
        let want = probe::block_frames(negotiated.sample_rate, BLOCK_MS, negotiated.channels);
        // 跑的人要能一眼看出这张卡谈成了什么（板上验收时对着 journal 看）。
        eprintln!("{LOOPBACK_CAPTURE} 谈成：{negotiated:?}，一块 {want} 个交织样本");

        // 收三块：块长、率、声道都必须与 `start` 回报的格式一致（§5.7 的实机一验）。
        for _ in 0..3 {
            let chunk = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("5 秒内没等到音频块：poll/avail/readi 这一路有问题");
            assert_eq!(chunk.sample_rate, negotiated.sample_rate);
            assert_eq!(chunk.channels, negotiated.channels);
            assert_eq!(chunk.samples.len(), want, "块长必须按协商到的率算");
            assert!(
                chunk.samples.iter().all(|s| s.is_finite()),
                "换算出的样本里出现了 NaN/Inf"
            );
        }

        capture.stop();
        while rx.try_recv().is_ok() {}
        // `stop` 返回时线程已 join、回调方已析构；再等一会儿若还有新块就是漏了。
        std::thread::sleep(Duration::from_millis(POLL_TIMEOUT_MS as u64 * 3));
        assert!(
            rx.try_recv().is_err(),
            "stop 之后回调还在触发，违反 C4 的契约"
        );
    }
}
