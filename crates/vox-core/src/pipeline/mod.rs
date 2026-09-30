//! 两条流水线的执行体。
//!
//! 每条流水线 = **一个工作线程 + 一条 WS 会话**。线程里干的事是死的：
//! 采集块 → 单声道 → 降噪 → 阀门 → 重采样到 16 kHz → 上传；
//! 收到的语音 → 播放汇，收到的文字 → 字幕轨。
//!
//! 平台相关的东西一律不在这里：socket、麦克风、扬声器、降噪、重采样全是
//! [`crate::ports`] / [`crate::cloud`] 里的 trait，由外壳通过 [`Deps`] 里的
//! **工厂**注入——每次 Start 都要全新的实例，所以传的是工厂而不是实例。
//!
//! 从旧版 `session.py` 照搬过来、不许"优化"掉的行为：
//! - 输入队列深度 8（约 160 ms），满了**丢最旧的**；只在第 1 次和每第 25 次时打警告。
//! - 阀门命令带单调序号，`seq <= last_gate_seq` 的直接扔（过期命令）。
//! - 阀门状态节流：状态变了、或者距上次上报满 200 ms，才报一次。
//! - `initial_gate_active` 在**会话线程里同步生效**，不靠后续命令补。
//! - Stop 先竖停止旗，启动流程每一步都看一眼；连接期间被停要**安静收尾**，
//!   不许冒出假"错误"。
//! - 没连上时来的音频**立刻丢**，绝不排队。

mod chain;
pub(crate) mod listen;
pub(crate) mod speak;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::capability::HostFacts;
use crate::cloud::protocol::{pcm16_to_float, OUTPUT_SAMPLE_RATE};
use crate::cloud::{self, HotChange, Incoming, ServerEvent, Session, SessionParams, Transport};
use crate::composition::{
    Composition, Input, Op, Output, PlaybackRole, COMPOSITION_SCHEMA_VERSION,
};
use crate::event::{Notice, Pipeline, PipelineState};
use crate::gate::{ActivationGate, GateConfig, GateState};
use crate::latency::LatencyTracker;
use crate::ports::{
    AudioChunk, CaptureSource, CaptureTarget, Denoise, PlaybackSink, PortError, PortResult,
    Resample,
};
use crate::runtime::{PipelineCommand, PipelineControl, Runtime, SessionConfig};

/// 采集块长（ms）。20 ms 在 RNNoise / 重采样的 10 ms 粒度上对齐，
/// 同时把原来 40 ms 分块造成的平均等待减半。
pub const INPUT_BLOCK_MS: u32 = 20;
/// 输入最多积压 160 ms；满了丢最旧的，实时语音优先保新鲜而不是保完整。
pub const INPUT_QUEUE_SIZE: usize = 8;
/// 主循环一拍最多阻塞多久（ms）。也是 Stop 握手的最坏响应时间。
const POLL_MS: u32 = 5;
/// 阀门状态最多多久报一次（ms）。照抄旧版的 0.2 秒。
const GATE_THROTTLE_MS: u64 = 200;
/// 降噪只在这个采样率下有效（RNNoise 的原生率）。对不上就不降噪。
const DENOISE_RATE: u32 = 48_000;
/// 一拍最多吃几条服务端消息，别让消息把音频处理饿死。
const MAX_MESSAGES_PER_TICK: usize = 32;
/// 一拍最多处理 4 个输入块，然后必须给下行消息机会，防止上行过载饿死字幕/译音。
const MAX_AUDIO_PER_TICK: usize = 4;
/// 队列健康度最多每 500 ms 推一次；首字/首声/一轮完成等里程碑会立即推。
const LATENCY_THROTTLE_MS: u64 = 500;

// --- 外壳注入的工厂 --------------------------------------------------------

/// WebSocket 工厂。每条会话一根新 socket。
pub type TransportFactory = Box<dyn Fn() -> Box<dyn Transport> + Send + Sync>;
/// 采集源工厂。麦克风和进程环回都走这个，具体抓谁看 [`CaptureTarget`]。
pub type CaptureFactory = Box<dyn Fn() -> Box<dyn CaptureSource> + Send + Sync>;
/// 播放汇工厂。只有要出声的会话才会调。
pub type PlaybackFactory = Box<dyn Fn() -> Box<dyn PlaybackSink> + Send + Sync>;
/// 降噪器工厂。造不出来（模型加载失败之类）就返回 `Err`，此时**降级为不降噪**，
/// 不让整条流水线挂掉。
pub type DenoiseFactory = Box<dyn Fn() -> PortResult<Box<dyn Denoise>> + Send + Sync>;
/// 重采样器工厂，参数是 (输入率, 输出率)。两个率相等时实现方应该给个直通的。
pub type ResampleFactory = Box<dyn Fn(u32, u32) -> Box<dyn Resample> + Send + Sync>;

/// 流水线要用的一整套平台能力。外壳启动时装好，之后只读。
pub struct Deps {
    pub transport: TransportFactory,
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    pub denoise: DenoiseFactory,
    pub resample: ResampleFactory,
}

// --- 一条会话的作业单 ------------------------------------------------------

/// 两条流水线的差别全压缩成这张作业单，由各自的 `plan()` 生成。
/// 骨架（[`Worker`]）只认作业单，不认 `Pipeline` 枚举，这样加一条流水线不用动骨架。
pub(crate) struct Plan {
    /// 抓谁的声音。
    pub target: CaptureTarget,
    /// 上传前要不要降噪。数字源（环回）本来就干净，白降一遍还费 CPU。
    pub denoise: bool,
    /// 直通模式：不起云端会话、不开 WS，把闸门放行的原声直接推给播放汇。
    /// 对外说话关掉「翻译」时用；带云端的会话恒为 `false`。
    pub passthrough: bool,
    /// 收到的语音往哪放。`None` = 这条会话不出声（纯文字）。
    pub playback_device: Option<Option<String>>,
    /// 是否把同一份译音额外回放到系统默认播放设备。
    pub monitor_translation: bool,
    /// 认不认 `HotUpdate`。只有"对外说话"认。
    pub hot_update: bool,
    /// 协议参数。
    pub params: SessionParams,
}

impl Plan {
    /// 按 `SessionConfig` + **这台机器的事实**派活：先造清单（[`Composition::of`]，位为假的格子
    /// 在这一步被关掉），再由清单推出作业单（[`Plan::from`]）。
    ///
    /// 语言 / 音色这些已经在 `Runtime::session_config` 里按流水线定好了，这里只管把平台侧的
    /// 活儿排出来。`facts` 只影响两处：哪一格进不进清单、虚拟麦的缺省设备。
    pub(crate) fn build(config: &SessionConfig, facts: &HostFacts) -> PortResult<Self> {
        Self::from(&Composition::of(config, facts)?)
    }

    /// 清单 → 作业单。**`Plan` 只能这么造**：清单是唯一的装配描述，没有第二条派生路径。
    pub(crate) fn from(composition: &Composition) -> PortResult<Self> {
        if composition.schema_version != COMPOSITION_SCHEMA_VERSION {
            return Err(PortError::new(format!(
                "清单版本不认识：这份是 {}，本芯只认 {COMPOSITION_SCHEMA_VERSION}。",
                composition.schema_version
            )));
        }
        if let Err(errors) = composition.validate() {
            return Err(PortError::new(format!("清单不合法：{errors:?}")));
        }
        let target = match composition.r#in.first() {
            Some(Input::Mic { device, .. }) => CaptureTarget::Microphone(device.clone()),
            Some(Input::ProcessLoopback {
                executable,
                include_tree,
                ..
            }) => CaptureTarget::ProcessLoopback {
                executable: executable.clone(),
                include_tree: *include_tree,
            },
            // 网络进 / 宿主喂进来的声音：清单先占名，实现归 S3。
            Some(Input::NetIn { .. } | Input::HostFeed { .. }) => {
                return Err(PortError::new(
                    "清单里的输入还没实现（net_in / host_feed 归 S3）。",
                ));
            }
            None => return Err(PortError::new("清单里没有输入：这台设备的采集能力位关着。")),
        };
        let session = composition.session.as_ref();
        Ok(Self {
            target,
            denoise: composition.ops.iter().any(|op| matches!(op, Op::Denoise)),
            // 直通 = 没有云端会话。
            passthrough: session.is_none(),
            // 主播放出口 = 非回听的那条 `playback`；清单里没有它 = 这条会话不出声（纯文字）。
            playback_device: composition.out.iter().find_map(|output| match output {
                Output::Playback { role, device, .. } if *role != PlaybackRole::Monitor => {
                    Some(device.clone())
                }
                _ => None,
            }),
            monitor_translation: composition.out.iter().any(|output| {
                matches!(
                    output,
                    Output::Playback {
                        role: PlaybackRole::Monitor,
                        ..
                    }
                )
            }),
            hot_update: session.is_some_and(|session| session.hot_update),
            // 直通清单里没有 `session`，而 `Plan.params` 在直通时**是死格**：不连云端就不发报文、
            // 不记用量，采样率也只由 provider 决定。这里给一份空参数，而不是回头偷偷读 `config`
            // ——`Plan` 只由清单构造。
            params: session
                .map(|session| session.params.clone())
                .unwrap_or_else(|| SessionParams::text_only(String::new(), String::new())),
        })
    }
}

// --- 引擎 ------------------------------------------------------------------

/// 收命令、管线程。[`Runtime`] 只跟它说"起/停/改"，剩下的它自己安排。
///
/// 和 `Runtime` 互相持有（`Runtime` 拿着 `Arc<dyn PipelineControl>` 指回来），
/// 这个环在进程活着期间是有意的；退出前调 [`PipelineEngine::shutdown`] 把线程收干净。
pub struct PipelineEngine {
    runtime: Runtime,
    deps: Arc<Deps>,
    workers: Mutex<HashMap<Pipeline, Handle>>,
}

/// 一个工作线程的把手。
struct Handle {
    session_id: u64,
    thread: JoinHandle<()>,
    inbox: Arc<Inbox>,
}

impl PipelineEngine {
    pub fn new(runtime: Runtime, deps: Deps) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            deps: Arc::new(deps),
            workers: Mutex::new(HashMap::new()),
        })
    }

    /// 停掉所有还活着的会话并等它们收尾。退出前调一次。
    pub fn shutdown(&self) {
        let handles: Vec<Handle> = self.workers.lock().drain().map(|(_, h)| h).collect();
        for handle in handles {
            retire(handle);
        }
    }

    /// 起一条会话。同一条流水线已经有会话在跑就先把旧的收掉。
    fn start(&self, config: Box<SessionConfig>) {
        let pipeline = config.pipeline;
        let session_id = config.session_id;

        // 先把旧的摘出来，**在锁外面**收尾——收尾会回调 Runtime，Runtime 可能反手
        // 再派命令进来，握着锁 join 就是自锁。
        let stale = self.workers.lock().remove(&pipeline);
        if let Some(stale) = stale {
            retire(stale);
        }

        let plan = match Plan::build(&config, &self.runtime.host_facts()) {
            Ok(plan) => plan,
            Err(err) => {
                self.runtime
                    .on_pipeline_failed(pipeline, session_id, err.message);
                return;
            }
        };

        let inbox = Arc::new(Inbox::default());
        let worker = Worker::new(
            self.runtime.clone(),
            Arc::clone(&self.deps),
            Arc::clone(&inbox),
            *config,
            plan,
        );
        let thread = thread::Builder::new()
            .name(format!("vox-{}", pipeline.label()))
            .spawn(move || worker.run())
            .expect("流水线线程起不来");
        self.workers.lock().insert(
            pipeline,
            Handle {
                session_id,
                thread,
                inbox,
            },
        );
    }

    /// 按会话号停。号对不上说明是过期的 Stop，忽略。
    fn stop(&self, session_id: u64) {
        let handle = {
            let mut workers = self.workers.lock();
            let pipeline = workers
                .iter()
                .find(|(_, h)| h.session_id == session_id)
                .map(|(p, _)| *p);
            match pipeline {
                Some(pipeline) => workers.remove(&pipeline),
                None => None,
            }
        };
        if let Some(handle) = handle {
            retire(handle);
        }
    }

    /// 把命令塞给持有这个会话号的线程。
    fn post(&self, session_id: u64, note: Note) {
        let workers = self.workers.lock();
        let Some(handle) = workers.values().find(|h| h.session_id == session_id) else {
            return;
        };
        let inbox = Arc::clone(&handle.inbox);
        drop(workers);
        inbox.post(note);
    }
}

/// 竖停止旗 → 叫醒线程 → 等它真收完。
///
/// 如果调用方**就是这个线程自己**，join 自己会死锁，所以只竖旗然后放手。
fn retire(handle: Handle) {
    handle.inbox.stop();
    if handle.thread.thread().id() == thread::current().id() {
        return;
    }
    let _ = handle.thread.join();
}

impl PipelineControl for PipelineEngine {
    fn apply(&self, cmd: PipelineCommand) -> PortResult<()> {
        match cmd {
            PipelineCommand::Start(config) => self.start(config),
            PipelineCommand::Stop { session_id } => self.stop(session_id),
            PipelineCommand::SetGateActive {
                session_id,
                seq,
                active,
            } => self.post(session_id, Note::GateActive { seq, active }),
            PipelineCommand::SetGateConfig {
                session_id,
                seq,
                config,
            } => self.post(session_id, Note::GateConfig { seq, config }),
            PipelineCommand::HotUpdate {
                session_id,
                target_language,
                voice,
            } => self.post(
                session_id,
                Note::Hot {
                    target_language,
                    voice,
                },
            ),
            PipelineCommand::SetTranslationAudio {
                session_id,
                voice,
                output_device,
            } => self.post(
                session_id,
                Note::TranslationAudio {
                    voice,
                    output_device,
                },
            ),
            PipelineCommand::SetMonitorTranslation {
                session_id,
                enabled,
            } => self.post(session_id, Note::MonitorTranslation { enabled }),
        }
        Ok(())
    }
}

// --- 信箱 ------------------------------------------------------------------

/// 引擎 → 工作线程的一条通知。Start/Stop 不走这里（那两个是线程的生死）。
enum Note {
    GateActive {
        seq: u64,
        active: bool,
    },
    GateConfig {
        seq: u64,
        config: GateConfig,
    },
    Hot {
        target_language: Option<String>,
        voice: Option<String>,
    },
    TranslationAudio {
        voice: Option<String>,
        output_device: Option<String>,
    },
    MonitorTranslation {
        enabled: bool,
    },
}

/// 工作线程的信箱：停止旗 + 通知队列 + 音频队列。一把锁管全部，持锁时间都是微秒级。
///
/// 采集回调跑在别人的线程（WASAPI 的），所以音频也从这里进。
#[derive(Default)]
struct Inbox {
    state: Mutex<InboxState>,
    wake: Condvar,
}

#[derive(Default)]
struct InboxState {
    stopping: bool,
    notes: VecDeque<Note>,
    audio: VecDeque<QueuedAudio>,
    dropped: u64,
    /// 已经走完全程的块数。丢帧率 = dropped / (processed + dropped)。
    processed: u64,
}

struct QueuedAudio {
    chunk: AudioChunk,
    enqueued_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct InboxStats {
    depth: usize,
    oldest_ms: u64,
    processed: u64,
    dropped: u64,
}

impl Inbox {
    /// 竖停止旗并叫醒线程。**先竖旗**是关键：启动流程每一步都会回头看这面旗，
    /// 不然连接期间被停会留下一个杀不掉的孤儿会话，麦克风一直往云端传。
    fn stop(&self) {
        self.state.lock().stopping = true;
        self.wake.notify_all();
    }

    fn stopping(&self) -> bool {
        self.state.lock().stopping
    }

    fn post(&self, note: Note) {
        let mut state = self.state.lock();
        if state.stopping {
            return;
        }
        state.notes.push_back(note);
        self.wake.notify_all();
    }

    /// 采集回调调这个。队列满了**丢最旧的**——宁可丢掉过时的话，也不能阻塞
    /// 音频驱动线程。警告只在第 1 次和每第 25 次时打，不然日志会被刷爆。
    fn push_audio(&self, chunk: AudioChunk) {
        let mut state = self.state.lock();
        if state.stopping {
            return;
        }
        if state.audio.len() >= INPUT_QUEUE_SIZE {
            state.audio.pop_front();
            state.dropped += 1;
            let dropped = state.dropped;
            if dropped == 1 || dropped.is_multiple_of(25) {
                tracing::warn!(dropped, "输入队列满了，丢最旧的音频块（处理跟不上采集）");
            }
        }
        state.audio.push_back(QueuedAudio {
            chunk,
            enqueued_at: Instant::now(),
        });
        self.wake.notify_all();
    }

    fn take_notes(&self) -> Vec<Note> {
        self.state.lock().notes.drain(..).collect()
    }

    fn take_audio(&self) -> Option<QueuedAudio> {
        self.state.lock().audio.pop_front()
    }

    fn has_audio(&self) -> bool {
        !self.state.lock().audio.is_empty()
    }

    fn stats(&self) -> InboxStats {
        let state = self.state.lock();
        InboxStats {
            depth: state.audio.len(),
            oldest_ms: state
                .audio
                .front()
                .map_or(0, |queued| queued.enqueued_at.elapsed().as_millis() as u64),
            processed: state.processed,
            dropped: state.dropped,
        }
    }

    /// 一块走完全程了。
    fn mark_processed(&self) {
        self.state.lock().processed += 1;
    }

    /// 处理了多少块、丢了多少块。测试拿它当"这一拍走完了"的信号。
    #[cfg(test)]
    fn counters(&self) -> (u64, u64) {
        let state = self.state.lock();
        (state.processed, state.dropped)
    }

    /// 可打断的等待，退避重连时用。返回 `true` 表示"别等了，要停了"。
    fn wait(&self, ms: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_millis(ms);
        let mut state = self.state.lock();
        loop {
            if state.stopping {
                return true;
            }
            if self.wake.wait_until(&mut state, deadline).timed_out() {
                return state.stopping;
            }
        }
    }

    /// 丢了多少块（测试和诊断用）。
    #[cfg(test)]
    fn dropped(&self) -> u64 {
        self.counters().1
    }
}

// --- 工作线程 --------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct UploadedSpan {
    stream_start_sample: u64,
    stream_end_sample: u64,
    capture_start_ms: u64,
    capture_end_ms: u64,
}

/// 把服务端的 `audio_start_ms` 映射回本机采集时刻。只留最近两分钟，
/// 长会话也不会无限长；服务端 VAD 的回报远早于这个窗口。
#[derive(Debug)]
struct UploadTimeline {
    spans: VecDeque<UploadedSpan>,
    total_samples: u64,
    sample_rate: u32,
}

impl UploadTimeline {
    fn new(sample_rate: u32) -> Self {
        Self {
            spans: VecDeque::new(),
            total_samples: 0,
            sample_rate,
        }
    }

    fn reset(&mut self) {
        self.spans.clear();
        self.total_samples = 0;
    }

    fn push(&mut self, samples: usize, capture_start_ms: u64, capture_end_ms: u64) {
        if samples == 0 {
            return;
        }
        let start = self.total_samples;
        self.total_samples = self.total_samples.saturating_add(samples as u64);
        self.spans.push_back(UploadedSpan {
            stream_start_sample: start,
            stream_end_sample: self.total_samples,
            capture_start_ms,
            capture_end_ms: capture_end_ms.max(capture_start_ms),
        });
        let keep_after = self
            .total_samples
            .saturating_sub(self.sample_rate as u64 * 120);
        while self
            .spans
            .front()
            .is_some_and(|span| span.stream_end_sample < keep_after)
        {
            self.spans.pop_front();
        }
    }

    fn capture_time(&self, audio_offset_ms: u64) -> Option<u64> {
        let sample = audio_offset_ms.saturating_mul(self.sample_rate as u64) / 1000;
        let span = self
            .spans
            .iter()
            .find(|span| sample >= span.stream_start_sample && sample <= span.stream_end_sample)?;
        let stream_len = span
            .stream_end_sample
            .saturating_sub(span.stream_start_sample)
            .max(1);
        let capture_len = span.capture_end_ms.saturating_sub(span.capture_start_ms);
        let within = sample.saturating_sub(span.stream_start_sample);
        Some(
            span.capture_start_ms
                .saturating_add(capture_len.saturating_mul(within) / stream_len),
        )
    }
}

#[derive(Debug)]
struct TurnProbe {
    started_at_ms: u64,
    first_text_seen: bool,
    first_audio_seen: bool,
}

impl TurnProbe {
    /// 这一轮第一次见到文字：返回轮次起点（给端到端延迟用），之后一律 `None`。
    fn take_first_text(&mut self) -> Option<u64> {
        let first = !self.first_text_seen;
        self.first_text_seen = true;
        first.then_some(self.started_at_ms)
    }

    /// 同上，给译音用。
    fn take_first_audio(&mut self) -> Option<u64> {
        let first = !self.first_audio_seen;
        self.first_audio_seen = true;
        first.then_some(self.started_at_ms)
    }
}

#[derive(Debug, Clone, Copy)]
struct PlaybackProbe {
    turn_started_at_ms: u64,
    rendered_baseline: u64,
}

/// 关掉一个播放汇：先把积压的样本放掉再关，然后把槽位清空。
fn close_sink(slot: &mut Option<Box<dyn PlaybackSink>>) {
    if let Some(mut sink) = slot.take() {
        sink.flush();
        sink.close();
    }
}

/// 节流判断：`force`（状态变了、或者是里程碑）一律放行；否则距上次上报满
/// `window_ms` 才放行。放行时顺手把"上次上报时刻"推到 `now`。
fn should_emit(last_emit: &mut u64, now: u64, force: bool, window_ms: u64) -> bool {
    if !force && now.saturating_sub(*last_emit) < window_ms {
        return false;
    }
    *last_emit = now;
    true
}

/// 一条会话的全部状态。除了 `Inbox`，这里的东西只被这一个线程碰，所以不用锁。
struct Worker {
    runtime: Runtime,
    deps: Arc<Deps>,
    inbox: Arc<Inbox>,
    config: SessionConfig,
    plan: Plan,

    session: Session,
    transport: Option<Box<dyn Transport>>,
    capture: Option<Box<dyn CaptureSource>>,
    sink: Option<Box<dyn PlaybackSink>>,
    monitor_sink: Option<Box<dyn PlaybackSink>>,
    denoiser: Option<Box<dyn Denoise>>,
    resampler: Option<Box<dyn Resample>>,
    gate: Option<ActivationGate>,

    /// 阀门命令的序号闸。旧版初值 -1，这里用 `None` 表示"还没收过"。
    last_gate_seq: Option<u64>,
    /// 阀门状态节流：上次报的状态 + 上次报的时刻。
    last_gate_state: Option<GateState>,
    last_gate_emit: u64,
    last_gate_active: bool,
    /// 已经推给字幕轨的文字。服务端的 delta 是**累计整句**，得自己算增量。
    sent_text: String,
    /// 汇报给账本的运行状态，去重用。
    reported: Option<PipelineState>,
    backoff: cloud::Backoff,
    latency: LatencyTracker,
    last_latency_emit: u64,
    session_opened_at: u64,
    upload_timeline: UploadTimeline,
    turn_probe: Option<TurnProbe>,
    playback_probe: Option<PlaybackProbe>,
    fallback_speech_start: Option<u64>,
}

impl Worker {
    fn new(
        runtime: Runtime,
        deps: Arc<Deps>,
        inbox: Arc<Inbox>,
        config: SessionConfig,
        plan: Plan,
    ) -> Self {
        let session =
            Session::new_for(config.provider, config.api_key.clone(), plan.params.clone());
        let input_sample_rate = session.input_sample_rate();
        Self {
            runtime,
            deps,
            inbox,
            config,
            plan,
            session,
            transport: None,
            capture: None,
            sink: None,
            monitor_sink: None,
            denoiser: None,
            resampler: None,
            gate: None,
            last_gate_seq: None,
            last_gate_state: None,
            last_gate_emit: 0,
            last_gate_active: false,
            sent_text: String::new(),
            reported: None,
            backoff: cloud::Backoff::new(),
            latency: LatencyTracker::default(),
            last_latency_emit: 0,
            session_opened_at: 0,
            upload_timeline: UploadTimeline::new(input_sample_rate),
            turn_probe: None,
            playback_probe: None,
            fallback_speech_start: None,
        }
    }

    fn pipeline(&self) -> Pipeline {
        self.config.pipeline
    }

    fn session_id(&self) -> u64 {
        self.config.session_id
    }

    fn now(&self) -> u64 {
        self.runtime.now_ms()
    }

    fn stopping(&self) -> bool {
        self.inbox.stopping()
    }

    /// 线程主体。启动 → 主循环 → 收尾，三段都能被停止旗打断。
    fn run(mut self) {
        match self.boot() {
            Ok(true) => self.pump(),
            // 启动过程中被 stop()：安静收尾，不冒假"错误"。
            Ok(false) => {}
            Err(err) => {
                if !self.stopping() {
                    self.runtime.on_pipeline_failed(
                        self.pipeline(),
                        self.session_id(),
                        err.message,
                    );
                }
            }
        }
        self.teardown();
    }

    /// 起会话。返回 `Ok(false)` = 中途被停了，安静收尾。
    ///
    /// 顺序照抄旧版：连 socket → 开播放 → 开采集。每一步之前先看停止旗，
    /// 不然连接期间按停会留下孤儿。
    fn boot(&mut self) -> PortResult<bool> {
        if self.stopping() {
            return Ok(false);
        }
        self.report(PipelineState::Starting);

        // 直通（关翻译）模式不起云端会话；只有带翻译的会话才连 WS、开 24k 播放汇。
        if !self.plan.passthrough {
            let mut transport = (self.deps.transport)();
            let connect_started = Instant::now();
            let now = self.now();
            cloud::open(transport.as_mut(), &mut self.session, now)?;
            self.latency
                .set_connect(connect_started.elapsed().as_millis() as u64);
            self.session_opened_at = self.now();
            self.upload_timeline.reset();
            if self.stopping() {
                transport.close();
                return Ok(false);
            }
            self.transport = Some(transport);

            // 出声的会话才开播放汇。内核推 24 kHz，设备率由外壳自己换。
            if let Some(device) = self.plan.playback_device.clone() {
                let mut sink = (self.deps.playback)();
                sink.open(device.as_deref(), OUTPUT_SAMPLE_RATE)?;
                self.sink = Some(sink);
            }
            if self.plan.monitor_translation {
                self.set_monitor_translation(true);
            }
            if self.stopping() {
                return Ok(false);
            }
        }

        // 采集回调跑在音频驱动线程上，只往信箱里塞，绝不做重活。
        let inbox = Arc::clone(&self.inbox);
        let mut capture = (self.deps.capture)();
        let format = capture.start(
            &self.plan.target,
            INPUT_BLOCK_MS,
            Box::new(move |chunk| inbox.push_audio(chunk)),
        )?;
        self.capture = Some(capture);

        // 阀门建在**采集率**上，不是 16 kHz——尾巴和 preroll 的时长是按率算的。
        let mut gate = ActivationGate::new(self.config.gate, format.sample_rate);
        // 初始开闸状态在线程里同步生效，不靠后续命令补（旧版的坑）。
        gate.set_external_active(self.config.gate_active);
        self.gate = Some(gate);

        if self.plan.denoise {
            if format.sample_rate == DENOISE_RATE {
                match (self.deps.denoise)() {
                    Ok(denoiser) => self.denoiser = Some(denoiser),
                    // 降噪造不出来只是少一层处理，别把整条流水线拖死。
                    Err(err) => {
                        tracing::warn!(error = %err, "降噪器起不来，这次先不降噪");
                        self.runtime.notify(
                            Notice::warning("降噪启动失败，本次已关闭").on(self.pipeline()),
                        );
                    }
                }
            } else {
                tracing::warn!(rate = format.sample_rate, "采集率不是 48 kHz，跳过降噪");
            }
        }
        // 直通模式：把原声推给播放汇，源率就是采集率（外壳自己再换到设备率）。
        if self.plan.passthrough {
            if let Some(device) = self.plan.playback_device.clone() {
                let mut sink = (self.deps.playback)();
                match sink.open(device.as_deref(), format.sample_rate) {
                    Ok(_) => self.sink = Some(sink),
                    Err(err) => {
                        tracing::warn!(error = %err, "原声输出设备打不开");
                        self.runtime.notify(
                            Notice::warning(format!("原声输出失败：{}", err.message))
                                .on(self.pipeline()),
                        );
                    }
                }
            }
        } else {
            self.resampler = Some((self.deps.resample)(
                format.sample_rate,
                self.session.input_sample_rate(),
            ));
        }

        if self.stopping() {
            return Ok(false);
        }
        // 一律先报"待命"，之后由阀门状态驱动 Ready↔Active，免得两处判断打架。
        self.report(PipelineState::Ready);
        if !self.plan.passthrough {
            self.backoff.succeed();
            self.emit_latency(true);
        }
        Ok(true)
    }

    /// 汇报运行状态。同一个状态不重复报；被让位期间账本自己管状态，别乱报。
    fn report(&mut self, state: PipelineState) {
        if self.reported == Some(state) {
            return;
        }
        self.reported = Some(state);
        self.runtime
            .on_pipeline_state(self.pipeline(), self.session_id(), state);
    }

    /// 把延迟统计推给账本。
    ///
    /// `force` = 里程碑（连上了、一轮收尾）立即推；否则走 500 ms 节流，
    /// 别拿 25 Hz 的主循环把 UI 事件通道刷爆。账本那边对内容重复的快照
    /// 本来就不转发，这里的节流只是少做无谓的快照计算。
    fn emit_latency(&mut self, force: bool) {
        let now = self.now();
        if !should_emit(&mut self.last_latency_emit, now, force, LATENCY_THROTTLE_MS) {
            return;
        }
        let stats = self.inbox.stats();
        let playback_queue_ms = self.playback_queue_ms();
        let snapshot = self.latency.snapshot(
            stats.depth,
            stats.oldest_ms,
            playback_queue_ms,
            stats.processed,
            stats.dropped,
        );
        self.runtime
            .on_latency(self.pipeline(), self.session_id(), snapshot);
    }

    /// 播放汇里还压着多少毫秒没放出去。没开播放汇（纯文字会话）给 0。
    fn playback_queue_ms(&self) -> u64 {
        let Some(sink) = self.sink.as_ref() else {
            return 0;
        };
        let stats = sink.stats();
        if stats.sample_rate == 0 || stats.channels == 0 {
            return 0;
        }
        let frames = stats.queued_samples / stats.channels as usize;
        (frames as u64).saturating_mul(1000) / stats.sample_rate as u64
    }

    /// 每拍探一次播放进度：渲染计数越过基线，就说明播放汇真的取走了译音，
    /// 记一次 `first_playback`（原始语音起点 → 首声被渲染线程取走的端到端延迟）。
    fn poll_playback(&mut self) {
        let Some(probe) = self.playback_probe else {
            return;
        };
        let Some(sink) = self.sink.as_ref() else {
            self.playback_probe = None;
            return;
        };
        // `>` 而不是 `>=`：基线是 0（比如 fake sink 不会动）时不误判。
        if sink.stats().rendered_samples > probe.rendered_baseline {
            self.latency
                .first_playback(self.now().saturating_sub(probe.turn_started_at_ms));
            self.playback_probe = None;
            self.emit_latency(true);
        }
    }

    /// 把这一拍攒下的通知全部消化掉。
    fn drain_notes(&mut self) {
        for note in self.inbox.take_notes() {
            self.handle_note(note);
        }
    }

    /// 主循环。一拍最多阻塞 `POLL_MS`，所以 Stop 最坏 5 ms 就能响应。
    fn pump(&mut self) {
        if self.plan.passthrough {
            self.pump_passthrough();
            return;
        }
        while !self.stopping() {
            self.drain_notes();
            if self.stopping() {
                return;
            }
            // 上行一次最多吃四块；即使 CPU 跟不上，也不能让下行字幕/译音饿死。
            let mut did_work = false;
            for _ in 0..MAX_AUDIO_PER_TICK {
                let Some(queued) = self.inbox.take_audio() else {
                    break;
                };
                did_work = true;
                let queue_ms = queued.enqueued_at.elapsed().as_millis() as u64;
                self.latency.input_queue(queue_ms);
                // 这块音频的采集墙钟窗口，用来把服务端的 `audio_start_ms`
                // （上传时间线上的偏移）映射回本机采集时刻。
                let now = self.now();
                let capture_end_ms = now.saturating_sub(queue_ms);
                let frames =
                    queued.chunk.samples.len() as u64 / queued.chunk.channels.max(1) as u64;
                let block_ms = frames.saturating_mul(1000) / queued.chunk.sample_rate.max(1) as u64;
                let capture_start_ms = capture_end_ms.saturating_sub(block_ms);
                self.feed(&queued.chunk, capture_start_ms, capture_end_ms);
                self.inbox.mark_processed();
                if self.stopping() {
                    return;
                }
            }
            // 有上行工作或还有积压时，下行只做非阻塞排空；真正空闲才等 5 ms。
            let recv_timeout = if did_work || self.inbox.has_audio() {
                0
            } else {
                POLL_MS
            };
            if !self.drain_messages(recv_timeout) {
                // 连接掉了，走重连；重连也走不通就退出。
                if !self.reconnect() {
                    return;
                }
            }
            self.poll_playback();
            self.emit_latency(false);
        }
    }

    /// 直通循环：不起 WS、不收消息，只把闸门放行的原声一块块推给播放汇。
    /// 命令（切闸/换门）照常吃，但没有任何云端消息、延迟统计或重连。
    fn pump_passthrough(&mut self) {
        while !self.stopping() {
            self.drain_notes();
            for _ in 0..MAX_AUDIO_PER_TICK {
                let Some(queued) = self.inbox.take_audio() else {
                    break;
                };
                self.feed_passthrough(&queued.chunk);
                self.inbox.mark_processed();
                if self.stopping() {
                    return;
                }
            }
            if self.inbox.wait(POLL_MS as u64) {
                return;
            }
        }
    }

    /// 收服务端消息。返回 `false` 表示连接断了。
    ///
    /// 第一次用 `POLL_MS` 阻塞（这就是主循环的节拍），之后用 0 ms 把积压掏干净，
    /// 但设个上限，别让一屋子消息把音频饿死。
    fn drain_messages(&mut self, first_timeout: u32) -> bool {
        for i in 0..MAX_MESSAGES_PER_TICK {
            let timeout = if i == 0 { first_timeout } else { 0 };
            let Some(transport) = self.transport.as_mut() else {
                return false;
            };
            match transport.recv(timeout) {
                Ok(Some(Incoming::Text(text))) => self.handle_message(&text),
                Ok(Some(Incoming::Closed(reason))) => {
                    tracing::info!(reason = %reason, "对端关了连接");
                    return false;
                }
                // 超时了但连接还在：这一拍就是正常的安静。
                Ok(None) => return true,
                Err(err) => {
                    tracing::warn!(error = %err, "socket 读失败");
                    return false;
                }
            }
            if self.stopping() {
                return true;
            }
        }
        true
    }

    /// 断线重连。返回 `false` = 别再试了（被停了，或者是死错误）。
    fn reconnect(&mut self) -> bool {
        if self.stopping() {
            return false;
        }
        if let Some(mut transport) = self.transport.take() {
            transport.close();
        }
        self.session.on_disconnected();
        self.sent_text.clear();
        // 半句作废，重采样缓冲里的零头也别带到下一条连接去。
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        if let Some(denoiser) = self.denoiser.as_mut() {
            denoiser.reset();
        }
        self.report(PipelineState::Reconnecting);

        loop {
            let wait = self.backoff.fail();
            if self.inbox.wait(wait as u64) {
                return false;
            }
            let mut transport = (self.deps.transport)();
            let now = self.now();
            match cloud::open(transport.as_mut(), &mut self.session, now) {
                Ok(()) => {
                    self.transport = Some(transport);
                    self.backoff.succeed();
                    self.report(PipelineState::Ready);
                    return true;
                }
                Err(err) => {
                    tracing::warn!(error = %err, attempt = self.backoff.attempt(), "重连失败");
                    // 密钥错、欠费、模型没权限：重连一万次也是这个结果。
                    if cloud::is_fatal_error(None, &err.message) {
                        self.fail(err.message);
                        return false;
                    }
                }
            }
            if self.stopping() {
                return false;
            }
        }
    }

    /// 报失败并封口——之后不许再报状态，免得盖掉账本里的错误。
    fn fail(&mut self, message: String) {
        self.reported = Some(PipelineState::Failed);
        self.runtime
            .on_pipeline_failed(self.pipeline(), self.session_id(), message);
    }

    /// 一块采集音频走完全程。
    ///
    /// 单声道 → 降噪 → **阀门** → 重采样 → 上传。降噪在阀门**前面**：
    /// 干净的信号判起来准，不然空调声能把阀门顶开。
    ///
    /// `capture_start_ms` / `capture_end_ms` 是这块音频被采集的墙钟窗口，
    /// 传到底下好让 `UploadTimeline` 能把服务端的 `audio_start_ms` 映回采集时刻。
    fn feed(&mut self, chunk: &AudioChunk, capture_start_ms: u64, capture_end_ms: u64) {
        let Some((accepted, status)) = self.gated_blocks(chunk) else {
            return;
        };
        for block in &accepted {
            let resampled = match self.resampler.as_mut() {
                Some(resampler) => resampler.process(block),
                None => block.clone(),
            };
            self.upload(&resampled, capture_start_ms, capture_end_ms);
        }
        // 一段收尾了，把重采样缓冲里的零头挤出去，别让尾音卡在缓冲里。
        if status.ended {
            let tail = match self.resampler.as_mut() {
                Some(resampler) => resampler.flush(),
                None => Vec::new(),
            };
            self.upload(&tail, capture_start_ms, capture_end_ms);
        }
    }

    /// 直通（关翻译）：闸门放行的原声直接推给播放汇，不重采样、不上传。
    /// 降噪照旧（麦克风收的是空气声）；无译文，所以没有尾巴、没有云端。
    fn feed_passthrough(&mut self, chunk: &AudioChunk) {
        let Some((accepted, _)) = self.gated_blocks(chunk) else {
            return;
        };
        if let Some(sink) = self.sink.as_mut() {
            for block in &accepted {
                sink.push(block);
            }
        }
    }

    /// 单声道 → 降噪 → 阀门，顺带把状态报出去、记下本地上升沿。
    /// 返回 `None` = 这一拍没有可用的块（降噪还在攒帧、或者阀门还没建好）。
    fn gated_blocks(
        &mut self,
        chunk: &AudioChunk,
    ) -> Option<(Vec<Vec<f32>>, crate::gate::GateStatus)> {
        let mono = chunk.to_mono();
        let cleaned = match self.denoiser.as_mut() {
            Some(denoiser) => denoiser.process(&mono),
            None => mono,
        };
        // 降噪按 480 样本一帧攒，攒不够就没输出，这一拍正常跳过。
        if cleaned.is_empty() {
            return None;
        }
        let gate = self.gate.as_mut()?;
        let (accepted, status) = gate.process(&cleaned);
        self.emit_gate_status(status);
        self.track_local_gate(status);
        Some((accepted, status))
    }

    /// 记录本地阀门状态，并在上升沿打开流水线时兜底记一个"开始说话"起点。
    ///
    /// 只有 Speak 的本地门控需要这个兜底（它有一个会开合的真阀门）；Listen 的
    /// 常开门（`GateConfig::level(0.0)`）时刻都是 active，靠服务端 VAD 报
    /// SpeechStarted/Stopped 来定轮次。用 `plan.hot_update` 区分两者：那正是
    /// Speak 为真、Listen 为假的既有标志。
    fn track_local_gate(&mut self, status: crate::gate::GateStatus) {
        let rose = status.active && !self.last_gate_active;
        self.last_gate_active = status.active;
        if self.plan.hot_update && rose && self.turn_probe.is_none() {
            self.begin_turn(self.now());
            self.fallback_speech_start = Some(self.now());
        }
    }

    /// 打开一个新的轮次探针——首字/首声/一轮完成都以它的 `started_at_ms` 为基准。
    fn begin_turn(&mut self, started_at_ms: u64) {
        self.turn_probe = Some(TurnProbe {
            started_at_ms,
            first_text_seen: false,
            first_audio_seen: false,
        });
    }

    /// 当前轮次的起点；探针还没开时用调用方给的兜底时刻。
    fn turn_start(&self, fallback: u64) -> u64 {
        self.turn_probe
            .as_ref()
            .map(|p| p.started_at_ms)
            .unwrap_or(fallback)
    }

    /// 上传一段 16 kHz 单声道。
    fn upload(&mut self, samples: &[f32], capture_start_ms: u64, capture_end_ms: u64) {
        if samples.is_empty() {
            return;
        }
        let now = self.now();
        // 没握手时 `audio_frame` 给 `None`——没连上的音频直接丢，绝不排队。
        let Some(frame) = self.session.audio_frame(samples, now) else {
            return;
        };
        // 记下这段采样在上传时间线上的位置，好把服务端的 `audio_start_ms` 映回
        // 本机采集墙钟。上传采样率按 provider catalog 配置。
        self.upload_timeline
            .push(samples.len(), capture_start_ms, capture_end_ms);

        let send_started = self.now();
        if let Some(transport) = self.transport.as_mut() {
            if let Err(err) = transport.send(&frame) {
                tracing::warn!(error = %err, "音频帧发不出去");
                return;
            }
        }
        self.latency
            .upload_send(self.now().saturating_sub(send_started));
    }

    /// 阀门状态上报，带节流：状态变了、或者距上次满 200 ms，才报一次。
    /// 不节流的话 25 Hz 的 RMS 会把 UI 线程刷爆。
    fn emit_gate_status(&mut self, status: crate::gate::GateStatus) {
        let now = self.now();
        let changed = self.last_gate_state != Some(status.state);
        if !should_emit(&mut self.last_gate_emit, now, changed, GATE_THROTTLE_MS) {
            return;
        }
        self.last_gate_state = Some(status.state);
        self.runtime
            .on_gate_status(self.pipeline(), self.session_id(), status);
        // 闸开着就是"正在说话"，让面板上的灯跟着亮。
        let state = if status.active {
            PipelineState::Active
        } else {
            PipelineState::Ready
        };
        self.report(state);
    }

    /// 吃一条服务端消息。
    fn handle_message(&mut self, message: &str) {
        let parsed = self.session.on_messages(message);
        if parsed.is_empty() {
            // 解不开的 JSON：网络上的垃圾不该让流水线炸。
            tracing::debug!("收到解析不了的消息");
            return;
        }
        for parsed in parsed {
            self.handle_server_event(parsed);
        }
    }

    fn handle_server_event(&mut self, parsed: crate::cloud::ParsedEvent) {
        match parsed.event {
            ServerEvent::TextDelta { text, confirmed } => {
                let now = self.now();
                self.push_text(&text, false, confirmed.as_deref());
                if let Some(started) = self
                    .turn_probe
                    .as_mut()
                    .and_then(TurnProbe::take_first_text)
                {
                    self.latency.first_text(now.saturating_sub(started));
                    self.emit_latency(true);
                }
            }
            ServerEvent::TextDone { text } => {
                // done 是终稿，整句都算已确认，交给外层（如 ChatBox）发整行。
                self.push_text(&text, true, Some(text.as_str()));
                self.sent_text.clear();
                let now = self.now();
                if let Some(started) = self
                    .turn_probe
                    .as_mut()
                    .and_then(TurnProbe::take_first_text)
                {
                    self.latency.first_text(now.saturating_sub(started));
                }
            }
            ServerEvent::SourceDetected { language } => {
                self.runtime.on_source_detected(self.pipeline(), language);
            }
            ServerEvent::SourceTranscriptDelta { text } => {
                tracing::debug!(text = %text, "source transcript delta");
            }
            ServerEvent::AudioDelta { pcm } => {
                let samples = pcm16_to_float(&pcm);
                if let Some(sink) = self.sink.as_mut() {
                    sink.push(&samples);
                }
                if let Some(sink) = self.monitor_sink.as_mut() {
                    sink.push(&samples);
                }
                let now = self.now();
                if let Some(started) = self
                    .turn_probe
                    .as_mut()
                    .and_then(TurnProbe::take_first_audio)
                {
                    self.latency.first_audio(now.saturating_sub(started));
                    self.emit_latency(true);
                }
                // 首声到齐就开始盯着播放汇，等渲染线程真的取走这些译音样本。
                if self.playback_probe.is_none() {
                    if let Some(sink) = self.sink.as_ref() {
                        self.playback_probe = Some(PlaybackProbe {
                            turn_started_at_ms: self.turn_start(now),
                            rendered_baseline: sink.stats().rendered_samples,
                        });
                    }
                }
            }
            ServerEvent::SpeechStarted { audio_start_ms, .. } => {
                let now = self.now();
                // 服务端的 `audio_start_ms` 是上传时间线上的偏移，映回本机采集墙钟。
                let start_ms = self
                    .upload_timeline
                    .capture_time(audio_start_ms)
                    .or_else(|| self.fallback_speech_start.take())
                    .unwrap_or(now);
                if self.turn_probe.is_none() {
                    self.begin_turn(start_ms);
                }
                let turn_start = self.turn_start(start_ms);
                self.latency.server_vad(now.saturating_sub(turn_start));
                self.emit_latency(true);
            }
            ServerEvent::SpeechStopped { .. } => {
                // 服务端说这轮说完了；本地没看见 SpeechStarted 的话，现在补一个起点。
                if self.turn_probe.is_none() {
                    let start_ms = self
                        .fallback_speech_start
                        .take()
                        .unwrap_or_else(|| self.now());
                    self.begin_turn(start_ms);
                }
            }
            ServerEvent::TurnDone { usage } => {
                self.runtime
                    .record_usage(&self.plan.params.model_name, &usage);
                self.sent_text.clear();
                let now = self.now();
                if let Some(probe) = self.turn_probe.take() {
                    self.latency
                        .turn_complete(now.saturating_sub(probe.started_at_ms));
                }
                self.playback_probe = None;
                self.fallback_speech_start = None;
                self.emit_latency(true);
            }
            ServerEvent::SessionUpdated => {
                self.latency
                    .set_session_ready(self.now().saturating_sub(self.session_opened_at));
                self.emit_latency(true);
            }
            ServerEvent::Error { code, message } => {
                let explained = cloud::explain_error(code.as_deref(), &message);
                if cloud::is_fatal_error(code.as_deref(), &message) {
                    // 死错误：重连也是白搭，直接失败。
                    self.fail(explained);
                    self.inbox.stop();
                } else {
                    tracing::warn!(error = %explained, "服务端报错，继续跑");
                    self.runtime
                        .notify(Notice::warning(explained).on(self.pipeline()));
                }
            }
            ServerEvent::Other { event_type } => {
                tracing::trace!(event_type = %event_type, "没接过的事件");
            }
        }
    }

    /// 把文字推给字幕轨。
    ///
    /// 服务端的 delta 带的是**到目前为止的整句**，而 `SubtitleTrack::push_text`
    /// 是往后追加的，所以这里得自己算增量，不然屏幕上会一句话叠一句话。
    ///
    /// `confirmed` 是不再会变的那段（已确认前缀；delta 类型累积的整句就是它）。
    /// 它单独透传出去：字幕轨只需要增量 `delta`，但外层订阅（如 VRChat ChatBox）
    /// 要做**整行替换**推进，需要的是完整已确认句，不是增量后缀。
    fn push_text(&mut self, full: &str, done: bool, confirmed: Option<&str>) {
        let (delta, replace) = match full.strip_prefix(self.sent_text.as_str()) {
            // 前缀延续：只发新增的后缀，字幕按它爬动。
            Some(rest) => (rest.to_string(), false),
            // 服务端把句子重写了（改译、纠错）：整句重发一遍。
            // `replace=true` 让字幕层**整行替换**，前面的让它自己淡出——
            // 不替换的话前端 append 会把新句叠在旧句上，屏幕上叠出一坨字。
            None => (full.to_string(), true),
        };
        self.sent_text = full.to_string();
        if delta.is_empty() && !done {
            return;
        }
        // 给外部逐字推进的完整句：确认前缀优先；done 时回落到整句终稿。
        let for_external = confirmed.or_else(|| done.then_some(full));
        self.runtime.on_subtitle_delta(
            self.pipeline(),
            self.session_id(),
            &delta,
            done,
            replace,
            for_external,
        );
    }

    /// 吃一条引擎派来的通知。
    fn handle_note(&mut self, note: Note) {
        match note {
            Note::GateActive { seq, active } => {
                if !self.accept_seq(seq) {
                    return;
                }
                if let Some(gate) = self.gate.as_mut() {
                    gate.set_external_active(active);
                }
            }
            Note::GateConfig { seq, config } => {
                if !self.accept_seq(seq) {
                    return;
                }
                if let Some(gate) = self.gate.as_mut() {
                    gate.set_config(config);
                }
                self.config.gate = config;
                // 换了门就得重新报一次状态，不然节流会把新门的第一拍吞掉。
                self.last_gate_state = None;
                self.last_gate_emit = 0;
            }
            Note::Hot {
                target_language,
                voice,
            } => {
                if !self.plan.hot_update {
                    return;
                }
                self.hot_update(target_language, voice);
            }
            Note::TranslationAudio {
                voice,
                output_device,
            } => {
                self.set_translation_audio(voice, output_device);
            }
            Note::MonitorTranslation { enabled } => {
                self.set_monitor_translation(enabled);
            }
        }
    }

    /// 即时开关本地回听。主输出已经是系统默认时不再开第二路，避免双重声音。
    /// 回听只是辅助功能，打开失败不拖垮正在工作的对外翻译主链路。
    fn set_monitor_translation(&mut self, enabled: bool) {
        // 直通（关翻译）没有译文，回听毫无意义，直接忽略。
        if self.plan.passthrough {
            return;
        }
        self.config.monitor_translation = enabled;
        self.plan.monitor_translation = enabled;
        if !enabled {
            close_sink(&mut self.monitor_sink);
            return;
        }
        if self.monitor_sink.is_some()
            || self.plan.playback_device.is_none()
            || self.plan.playback_device == Some(None)
        {
            return;
        }

        let mut sink = (self.deps.playback)();
        match sink.open(None, OUTPUT_SAMPLE_RATE) {
            Ok(_) => self.monitor_sink = Some(sink),
            Err(err) => {
                tracing::warn!(error = %err, "系统默认播放设备打不开，无法回听译音");
                self.runtime.notify(
                    Notice::warning(format!("回听译音失败：{}", err.message)).on(self.pipeline()),
                );
            }
        }
    }

    /// 序号闸：过期的阀门命令直接扔。
    ///
    /// 命令是在账本的写锁里编号的，到线程这边可能被后来的命令超车（比如快速
    /// 按放两下热键），只认更新的那条。
    fn accept_seq(&mut self, seq: u64) -> bool {
        if let Some(last) = self.last_gate_seq {
            if seq <= last {
                tracing::debug!(seq, last, "过期的阀门命令，扔了");
                return false;
            }
        }
        self.last_gate_seq = Some(seq);
        true
    }

    /// 换语言/换音色。走 `session.update` 热更新，**不重连**。
    fn hot_update(&mut self, target_language: Option<String>, voice: Option<String>) {
        let change = HotChange {
            target_language,
            // `voice` 是 `Option<Option<String>>`：外层 = 改不改，内层 = 改成啥。
            voice: voice.map(Some),
            clone_frequency: None,
        };
        self.send_hot_change(change);
    }

    /// 即时切换译文语音。协议模态、本地主输出和可选回听必须一起变，不能只改一半。
    fn set_translation_audio(&mut self, voice: Option<String>, output_device: Option<String>) {
        let playback_device = voice.as_ref().map(|_| output_device.clone());
        let playback_changed = self.plan.playback_device != playback_device;
        self.config.voice = voice.clone();
        self.config.output_device = output_device;

        if playback_changed {
            self.playback_probe = None;
            close_sink(&mut self.sink);
            close_sink(&mut self.monitor_sink);
            self.plan.playback_device = playback_device.clone();

            if let Some(device) = playback_device {
                let mut sink = (self.deps.playback)();
                match sink.open(device.as_deref(), OUTPUT_SAMPLE_RATE) {
                    Ok(_) => self.sink = Some(sink),
                    Err(err) => {
                        tracing::warn!(error = %err, "译文播放设备打不开");
                        self.runtime.notify(
                            Notice::warning(format!("译音输出失败：{}", err.message))
                                .on(self.pipeline()),
                        );
                    }
                }
            }
        }

        self.send_hot_change(HotChange {
            target_language: None,
            voice: Some(voice),
            clone_frequency: None,
        });

        // `set_monitor_translation` 会自行判断主输出是否存在以及是否已是默认设备。
        if self.config.monitor_translation {
            self.set_monitor_translation(true);
        }
    }

    fn send_hot_change(&mut self, change: HotChange) {
        if change.is_empty() {
            return;
        }
        let now = self.now();
        let frame = self.session.hot_update(&change, now);
        self.plan.params = self.session.params().clone();
        if let Some(frame) = frame {
            if let Some(transport) = self.transport.as_mut() {
                if let Err(err) = transport.send(&frame) {
                    tracing::warn!(error = %err, "热更新发不出去");
                }
            }
        }
    }

    /// 收尾。顺序反着来：先停采集（别再有新音频进来），再关 socket，最后关播放。
    fn teardown(&mut self) {
        if let Some(capture) = self.capture.as_mut() {
            capture.stop();
        }
        self.capture = None;
        if let Some(frame) = self.session.close_frame() {
            if let Some(transport) = self.transport.as_mut() {
                if let Err(err) = transport.send(&frame) {
                    tracing::warn!(error = %err, "session.close 发不出去");
                }
            }
        }
        if let Some(transport) = self.transport.as_mut() {
            transport.close();
        }
        self.transport = None;
        close_sink(&mut self.sink);
        close_sink(&mut self.monitor_sink);
        self.gate = None;
        self.denoiser = None;
        self.resampler = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::composition::HostKind;
    use crate::event::{Event, Severity};
    use crate::latency::LatencySnapshot;
    use crate::ports::{CaptureFormat, Clock, PortError, SecretStore};
    use crate::settings::{ListenTarget, Settings};
    use crate::usage::Stamp;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const MODEL: &str = "qwen3.5-livetranslate-flash-realtime";

    // --- 测试用的假件 ------------------------------------------------------

    /// 能手拨的时钟。节流测试要精确控制"过了多久"。
    pub(crate) struct TestClock {
        ms: AtomicU64,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                ms: AtomicU64::new(0),
            }
        }
        fn advance(&self, ms: u64) {
            self.ms.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            self.ms.load(Ordering::SeqCst)
        }
        fn stamp(&self) -> Stamp {
            Stamp {
                unix_secs: 0,
                year: 2026,
                month: 8,
                day: 5,
            }
        }
    }

    /// 内存密钥库，免得测试去碰真的凭据存储。
    #[derive(Default)]
    struct MemoryStore {
        key: Mutex<Option<String>>,
    }

    impl SecretStore for MemoryStore {
        fn load_api_key(&self) -> PortResult<Option<String>> {
            Ok(self.key.lock().clone())
        }
        fn store_api_key(&self, key: &str) -> PortResult<()> {
            *self.key.lock() = Some(key.to_string());
            Ok(())
        }
        fn clear_api_key(&self) -> PortResult<()> {
            *self.key.lock() = None;
            Ok(())
        }
    }

    /// 一条连接的收件箱。
    type WireInbox = Arc<Mutex<VecDeque<Option<Incoming>>>>;
    /// 采集回调。
    type FeedFn = Box<dyn FnMut(AudioChunk) + Send>;

    /// 假 socket。收发都记账，收的消息由测试提前排好。
    #[derive(Default)]
    struct Wire {
        /// 发出去的帧。
        sent: Mutex<Vec<String>>,
        /// 每条连接一个收件箱。**不能共用**：同时跑两条流水线时，共用的队列
        /// 会被先轮到的那条抢走消息，测试就成了掷骰子。
        inboxes: Mutex<Vec<WireInbox>>,
        connects: AtomicU64,
        closes: AtomicU64,
        /// 前几次 connect 要失败（重连测试用）。
        fail_connects: AtomicU64,
        fatal_connect: AtomicBool,
        /// connect 挂在这儿不返回，用来做"连的时候被停了"。
        block_connect: AtomicBool,
        /// 有人正卡在 connect 里。
        connect_waiting: AtomicBool,
    }

    impl Wire {
        fn sent(&self) -> Vec<String> {
            self.sent.lock().clone()
        }
        /// 发出去的帧里有几条是音频。
        fn audio_frames(&self) -> usize {
            self.sent
                .lock()
                .iter()
                .filter(|f| f.contains("input_audio_buffer.append"))
                .count()
        }
        /// 领一个新收件箱，顺手登记进账里。
        fn open_inbox(&self) -> WireInbox {
            let inbox = Arc::new(Mutex::new(VecDeque::new()));
            self.inboxes.lock().push(Arc::clone(&inbox));
            inbox
        }
        /// 投给**最新那条**连接。测试里"当下活着的会话"就是最后连上的那条。
        fn deliver(&self, message: Incoming) {
            let inboxes = self.inboxes.lock();
            let inbox = inboxes.last().expect("还没人连上来").clone();
            drop(inboxes);
            inbox.lock().push_back(Some(message));
        }
        fn push_message(&self, json: impl Into<String>) {
            self.deliver(Incoming::Text(json.into()));
        }
        fn push_closed(&self) {
            self.deliver(Incoming::Closed("对端跑了".into()));
        }
    }

    /// `Wire` 的一个把手。工厂每次给一个新的把手：账是共享的，收件箱是自己的。
    struct WireHandle(Arc<Wire>, WireInbox);

    impl Transport for WireHandle {
        fn connect(&mut self, _request: &crate::cloud::ConnectRequest) -> PortResult<()> {
            self.0.connects.fetch_add(1, Ordering::SeqCst);
            // 卡在这儿一小会儿，模拟"握手要几百毫秒"这段窗口。
            //
            // 必须自己带超时：`Stop` 是握手式的（会 join 工作线程），要是死等
            // 测试线程来放行，两边就锁死了——真实的 socket 也是自带超时的。
            if self.0.block_connect.swap(false, Ordering::SeqCst) {
                self.0.connect_waiting.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(120));
                self.0.connect_waiting.store(false, Ordering::SeqCst);
            }
            if self.0.fatal_connect.load(Ordering::SeqCst) {
                return Err(PortError::new("invalid api-key: 密钥不对"));
            }
            if self.0.fail_connects.load(Ordering::SeqCst) > 0 {
                self.0.fail_connects.fetch_sub(1, Ordering::SeqCst);
                return Err(PortError::new("连不上"));
            }
            Ok(())
        }
        fn send(&mut self, text: &str) -> PortResult<()> {
            self.0.sent.lock().push(text.to_string());
            Ok(())
        }
        fn recv(&mut self, _timeout_ms: u32) -> PortResult<Option<Incoming>> {
            Ok(self.1.lock().pop_front().unwrap_or(None))
        }
        fn close(&mut self) {
            self.0.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 假采集源。测试自己拿着回调往里灌音频。
    #[derive(Default)]
    struct Mic {
        started: Mutex<Option<CaptureTarget>>,
        stops: AtomicU64,
        rate: AtomicU64,
        feed: Mutex<Option<FeedFn>>,
        /// 开采集就失败（设备被占、没权限）。
        fail: AtomicBool,
    }

    impl Mic {
        fn new(rate: u32) -> Self {
            let mic = Self::default();
            mic.rate.store(rate as u64, Ordering::SeqCst);
            mic
        }
        /// 灌一块音频进流水线，模拟音频驱动线程的回调。
        fn emit(&self, samples: Vec<f32>) {
            let rate = self.rate.load(Ordering::SeqCst) as u32;
            if let Some(feed) = self.feed.lock().as_mut() {
                feed(AudioChunk {
                    samples,
                    sample_rate: rate,
                    channels: 1,
                });
            }
        }
        fn target(&self) -> Option<CaptureTarget> {
            self.started.lock().clone()
        }
    }

    struct MicHandle(Arc<Mic>);

    impl CaptureSource for MicHandle {
        fn start(
            &mut self,
            target: &CaptureTarget,
            _block_ms: u32,
            on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
        ) -> PortResult<CaptureFormat> {
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(PortError::new("麦克风打不开：设备被别的程序占着"));
            }
            *self.0.started.lock() = Some(target.clone());
            *self.0.feed.lock() = Some(on_chunk);
            Ok(CaptureFormat {
                sample_rate: self.0.rate.load(Ordering::SeqCst) as u32,
                channels: 1,
            })
        }
        fn stop(&mut self) {
            self.0.stops.fetch_add(1, Ordering::SeqCst);
            *self.0.feed.lock() = None;
        }
    }

    /// 假播放汇。记下放了多少样本、被 flush 过几次。
    #[derive(Default)]
    struct Speaker {
        opened: Mutex<Option<Option<String>>>,
        opens: AtomicU64,
        played: Mutex<Vec<f32>>,
        flushes: AtomicU64,
        closes: AtomicU64,
        /// 渲染线程累计「真正取走」的交错样本数。测试拨它模拟渲染前进。
        rendered: AtomicU64,
    }

    struct SpeakerHandle(Arc<Speaker>);

    impl PlaybackSink for SpeakerHandle {
        fn open(&mut self, device: Option<&str>, _source_rate: u32) -> PortResult<u32> {
            *self.0.opened.lock() = Some(device.map(str::to_string));
            self.0.opens.fetch_add(1, Ordering::SeqCst);
            Ok(48_000)
        }
        fn push(&mut self, samples: &[f32]) {
            self.0.played.lock().extend_from_slice(samples);
        }
        fn stats(&self) -> crate::ports::PlaybackStats {
            crate::ports::PlaybackStats {
                queued_samples: self.0.played.lock().len(),
                sample_rate: 48_000,
                channels: 1,
                rendered_samples: self.0.rendered.load(Ordering::SeqCst),
                dropped_samples: 0,
                device_latency_ms: 0,
            }
        }
        fn flush(&mut self) {
            self.0.flushes.fetch_add(1, Ordering::SeqCst);
            self.0.played.lock().clear();
        }
        fn close(&mut self) {
            self.0.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 假降噪：原样返回，只记调用次数。用来验"降噪在阀门前面"这个顺序。
    #[derive(Default)]
    struct Dsp {
        denoise_calls: AtomicU64,
        resets: AtomicU64,
        /// 造降噪器就失败（模型文件没打包进去之类）。
        fail: AtomicBool,
    }

    struct DenoiseHandle(Arc<Dsp>);

    impl Denoise for DenoiseHandle {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            self.0.denoise_calls.fetch_add(1, Ordering::SeqCst);
            samples.to_vec()
        }
        fn reset(&mut self) {
            self.0.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 假重采样：按整数比抽点，长度确实会变，好验调用方没假设长度守恒。
    struct Decimate {
        step: usize,
    }

    impl Resample for Decimate {
        fn process(&mut self, samples: &[f32]) -> Vec<f32> {
            samples.iter().step_by(self.step).copied().collect()
        }
        fn flush(&mut self) -> Vec<f32> {
            Vec::new()
        }
        fn reset(&mut self) {}
    }

    // --- 会话配置构造器（两个子模块的测试也用） ----------------------------

    pub(crate) fn speak_config() -> SessionConfig {
        SessionConfig {
            session_id: 1,
            pipeline: Pipeline::Speak,
            provider: crate::settings::ModelProvider::Aliyun,
            model_name: MODEL.to_string(),
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

    pub(crate) fn listen_config() -> SessionConfig {
        SessionConfig {
            session_id: 2,
            pipeline: Pipeline::Listen,
            provider: crate::settings::ModelProvider::Aliyun,
            model_name: MODEL.to_string(),
            api_key: "sk-test".to_string(),
            target_language: "zh".to_string(),
            voice: Some("Tina".to_string()),
            voice_clone_frequency: None,
            // 与运行时一致：听人说话不设电平门，无条件放行。
            gate: GateConfig::level(0.0),
            gate_active: true,
            input_device: None,
            output_device: None,
            translate: true,
            monitor_translation: false,
            loopback_target: Some(ListenTarget {
                executable: "Discord.exe".to_string(),
                display_name: "Discord".to_string(),
                include_process_tree: true,
            }),
            denoise: false,
            source_language: None,
        }
    }

    // --- 测试台 ------------------------------------------------------------

    /// 一整套装好的引擎 + 能观察的假件。
    struct Rig {
        engine: Arc<PipelineEngine>,
        runtime: Runtime,
        clock: Arc<TestClock>,
        wire: Arc<Wire>,
        mic: Arc<Mic>,
        speaker: Arc<Speaker>,
        dsp: Arc<Dsp>,
        events: Arc<Mutex<Vec<Event>>>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with_rate(48_000)
        }

        fn with_rate(rate: u32) -> Self {
            Self::build(rate)
        }

        fn build(rate: u32) -> Self {
            let clock = Arc::new(TestClock::new());
            let runtime = Runtime::new(Settings::default(), Arc::clone(&clock) as Arc<dyn Clock>);
            // 外壳装配时会注入事实（`lib.rs` 第 13 步）；缺省那份是 fail-closed 的，
            // 不注入就等于"这台机器什么都没接上"。夹具里的 mic / 播放汇都接好了。
            runtime.set_host_facts(HostFacts::all_wired(HostKind::Windows));
            runtime.set_secret_store(Arc::new(MemoryStore::default()));
            runtime.set_api_key("sk-test");

            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&events);
            runtime.add_listener(Arc::new(move |event: &Event| {
                sink.lock().push(event.clone());
            }));

            let wire = Arc::new(Wire::default());
            let mic = Arc::new(Mic::new(rate));
            let speaker = Arc::new(Speaker::default());
            let dsp = Arc::new(Dsp::default());

            let deps = Deps {
                transport: {
                    let wire = Arc::clone(&wire);
                    Box::new(move || {
                        let inbox = wire.open_inbox();
                        Box::new(WireHandle(Arc::clone(&wire), inbox)) as Box<dyn Transport>
                    })
                },
                capture: {
                    let mic = Arc::clone(&mic);
                    Box::new(move || {
                        Box::new(MicHandle(Arc::clone(&mic))) as Box<dyn CaptureSource>
                    })
                },
                playback: {
                    let speaker = Arc::clone(&speaker);
                    Box::new(move || {
                        Box::new(SpeakerHandle(Arc::clone(&speaker))) as Box<dyn PlaybackSink>
                    })
                },
                denoise: {
                    let dsp = Arc::clone(&dsp);
                    Box::new(move || {
                        if dsp.fail.load(Ordering::SeqCst) {
                            return Err(PortError::new("降噪模型加载失败"));
                        }
                        Ok(Box::new(DenoiseHandle(Arc::clone(&dsp))) as Box<dyn Denoise>)
                    })
                },
                // 48k → 16k 是三抽一，正好对上真实比例。
                resample: Box::new(|input, output| {
                    let step = (input / output).max(1) as usize;
                    Box::new(Decimate { step }) as Box<dyn Resample>
                }),
            };

            let engine = PipelineEngine::new(runtime.clone(), deps);
            runtime.set_control(Arc::clone(&engine) as Arc<dyn PipelineControl>);

            Self {
                engine,
                runtime,
                clock,
                wire,
                mic,
                speaker,
                dsp,
                events,
            }
        }

        /// 直接给引擎派一条 Start 并等它连上（采集开了就说明启动流程走完了）。
        ///
        /// 绕过账本，所以账本里的 `session_id` 还是 0，那边的 `on_*` 会挡住回调；
        /// 要观察账本状态的测试用 [`Rig::boot`]。
        fn start(&self, config: SessionConfig) -> u64 {
            let session_id = config.session_id;
            let connects = self.wire.connects.load(Ordering::SeqCst);
            self.engine
                .apply(PipelineCommand::Start(Box::new(config)))
                .expect("Start 不该失败");
            self.wait_until(|| {
                self.mic.target().is_some() && self.wire.connects.load(Ordering::SeqCst) > connects
            });
            session_id
        }

        /// 直通（关翻译）模式起会话：不连 WS，等采集 + 播放汇都开起来。
        fn start_passthrough(&self, config: SessionConfig) -> u64 {
            let session_id = config.session_id;
            self.engine
                .apply(PipelineCommand::Start(Box::new(config)))
                .expect("Start 不该失败");
            self.wait_until(|| {
                self.mic.target().is_some() && self.speaker.opens.load(Ordering::SeqCst) >= 1
            });
            session_id
        }

        /// 从账本这头开一条流水线（`Runtime::start` → 派命令 → 引擎起线程）。
        /// 账本会记住会话号，所以 `on_*` 回调都能落地。
        fn boot(&self, pipeline: Pipeline) -> u64 {
            self.runtime.start(pipeline);
            self.wait_until(|| self.mic.target().is_some());
            self.engine
                .workers
                .lock()
                .get(&pipeline)
                .map(|h| h.session_id)
                .expect("起完该有把手")
        }

        /// 自旋等条件成立。工作线程是真线程，得等它跑到。
        fn wait_until(&self, mut cond: impl FnMut() -> bool) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if cond() {
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("等超时了");
        }

        /// 灌一块音频并等它**走完全程**。
        ///
        /// 等的是"处理计数涨了"，不是"队列空了"——队列空只说明块被取走了，
        /// 处理可能还没跑完，那样断言会抢在结果前面。
        fn feed(&self, samples: Vec<f32>) {
            let inbox = self.inbox().expect("得先起会话");
            // 先等命令被消化掉再灌音频。
            //
            // 一拍里先 take_notes 再 take_audio，但要是工作线程**已经**走过了
            // take_notes 正在啃音频，刚投的命令就得等下一拍——这块音频会抢在
            // 命令生效前被处理掉。等到命令队列空了，说明它已经被取走并当场生效了。
            self.wait_until(|| inbox.state.lock().notes.is_empty());
            let before = inbox.counters();
            self.mic.emit(samples);
            self.wait_until(|| {
                let (processed, dropped) = inbox.counters();
                processed + dropped > before.0 + before.1
            });
        }

        fn inbox(&self) -> Option<Arc<Inbox>> {
            self.engine
                .workers
                .lock()
                .values()
                .next()
                .map(|h| Arc::clone(&h.inbox))
        }

        /// 排一条服务端消息给**当下最后连上**的那条会话。
        ///
        /// 先等连接真的建起来：把手是 spawn 的那一刻就登记进表的，socket 是线程
        /// 自己跑到 `boot()` 才连的，中间这段窗口收件箱还不存在。
        fn push_message(&self, json: impl Into<String>) {
            self.await_connect();
            self.wire.push_message(json);
        }

        fn push_closed(&self) {
            self.await_connect();
            self.wire.push_closed();
        }

        /// 等到会话数追上工作线程数——每条活着的流水线都连上了。
        fn await_connect(&self) {
            let want = self.engine.workers.lock().len();
            self.wait_until(|| self.wire.inboxes.lock().len() >= want);
        }

        fn events(&self) -> Vec<Event> {
            self.events.lock().clone()
        }

        fn drain_events(&self) -> Vec<Event> {
            std::mem::take(&mut *self.events.lock())
        }

        /// 捞一条流水线最近一次 `LatencyChanged` 的快照（有的话）。
        fn latency_snapshot(&self, pipeline: Pipeline) -> Option<LatencySnapshot> {
            self.events.lock().iter().rev().find_map(|e| match e {
                Event::LatencyChanged {
                    pipeline: p,
                    latency,
                } if *p == pipeline => Some((**latency).clone()),
                _ => None,
            })
        }
    }

    // --- 启动 / 收尾 -------------------------------------------------------

    #[test]
    fn starting_a_session_connects_handshakes_and_opens_capture() {
        let rig = Rig::new();
        rig.start(speak_config());

        assert_eq!(rig.wire.connects.load(Ordering::SeqCst), 1);
        // 连上先发一次 session.update，不发的话服务端用默认配置，翻译语言是错的。
        let sent = rig.wire.sent();
        assert_eq!(sent.len(), 1, "启动只该发握手这一帧");
        assert!(sent[0].contains("session.update"));
        assert!(sent[0].contains("\"ja\""), "握手要带目标语言：{}", sent[0]);
        // 要出声的会话才开播放汇，设备跟着设置走。
        assert_eq!(
            rig.speaker.opened.lock().clone(),
            Some(Some("CABLE Input".to_string()))
        );
        rig.engine.shutdown();
    }

    #[test]
    fn a_booted_pipeline_reports_ready_to_the_ledger() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.engine.shutdown();
    }

    #[test]
    fn text_only_sessions_never_open_playback() {
        let rig = Rig::new();
        let mut config = listen_config();
        config.voice = None;
        rig.start(config);
        assert!(
            rig.speaker.opened.lock().is_none(),
            "纯文字听译不该开播放汇"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn stop_waits_until_the_session_actually_wraps_up() {
        let rig = Rig::new();
        let session = rig.start(speak_config());

        rig.engine
            .apply(PipelineCommand::Stop {
                session_id: session,
            })
            .expect("Stop 不该失败");

        // Stop 返回时线程必须已经收完尾：采集停了、socket 关了、把手也摘了。
        assert_eq!(rig.mic.stops.load(Ordering::SeqCst), 1, "采集该停了");
        assert!(rig.wire.closes.load(Ordering::SeqCst) >= 1, "socket 该关了");
        assert!(rig.engine.workers.lock().is_empty(), "把手该摘了");
    }

    #[test]
    fn a_stale_stop_does_not_kill_the_live_session() {
        let rig = Rig::new();
        rig.start(speak_config());
        // 会话号对不上：这是上一条会话的 Stop，忽略。
        rig.engine
            .apply(PipelineCommand::Stop { session_id: 999 })
            .expect("过期 Stop 也该被吃掉");
        assert_eq!(rig.mic.stops.load(Ordering::SeqCst), 0, "现役会话不该被停");
        rig.engine.shutdown();
    }

    #[test]
    fn restarting_the_same_pipeline_retires_the_old_worker() {
        let rig = Rig::new();
        rig.start(speak_config());
        let mut again = speak_config();
        again.session_id = 7;
        rig.start(again);

        assert_eq!(rig.wire.connects.load(Ordering::SeqCst), 2);
        // 两条会话共用一个假采集源，第一条被收尾时会调它的 stop()。
        assert!(rig.mic.stops.load(Ordering::SeqCst) >= 1, "旧会话该被收掉");
        assert_eq!(rig.engine.workers.lock().len(), 1, "一条流水线只留一个线程");
        rig.engine.shutdown();
    }

    // --- 音频链 ------------------------------------------------------------

    /// 一块 40 ms @ 48 kHz 的响音频。
    fn loud_block() -> Vec<f32> {
        vec![0.5; 1920]
    }

    /// 一块 40 ms @ 48 kHz 的静音。
    fn quiet_block() -> Vec<f32> {
        vec![0.0; 1920]
    }

    #[test]
    fn audio_flows_through_denoise_gate_resample_then_uploads() {
        let rig = Rig::new();
        let mut config = speak_config();
        // 手动门，一开始就开着闸。
        config.gate_active = true;
        rig.start(config);

        rig.feed(loud_block());
        rig.wait_until(|| rig.wire.audio_frames() >= 1);

        // 降噪跑在阀门前面，所以哪怕闸最后放行，降噪也已经调过了。
        assert_eq!(rig.dsp.denoise_calls.load(Ordering::SeqCst), 1);
        assert_eq!(rig.wire.audio_frames(), 1, "开着闸的响音频该上传一帧");
        rig.engine.shutdown();
    }

    #[test]
    fn a_closed_manual_gate_uploads_nothing() {
        let rig = Rig::new();
        // gate_active = false：热键没按住。
        rig.start(speak_config());

        rig.feed(loud_block());
        rig.feed(loud_block());

        // 阀门照样看过这些样本（要出 RMS 给 UI），但一个字节都没上传。
        assert_eq!(rig.dsp.denoise_calls.load(Ordering::SeqCst), 2);
        assert_eq!(rig.wire.audio_frames(), 0, "闸关着不许上传");
        rig.engine.shutdown();
    }

    #[test]
    fn the_initial_gate_state_takes_effect_inside_the_worker_thread() {
        let rig = Rig::new();
        let mut config = speak_config();
        config.gate_active = true;
        rig.start(config);

        // 没发过任何 SetGateActive，第一块音频就该放行——初始状态是在线程里
        // 同步生效的，不靠后续命令补（旧版的坑）。
        rig.feed(loud_block());
        rig.wait_until(|| rig.wire.audio_frames() >= 1);
        rig.engine.shutdown();
    }

    #[test]
    fn listen_upload_is_unconditional_and_skips_denoise() {
        let rig = Rig::new();
        rig.start(listen_config());

        // 听人说话的电平门阈值是 0：静音也原样上传。
        // 环回抓的是对方程序的数字源，假底噪会骗电平门误判"正在说话"，
        // 与其烧 token 传垃圾，不如全放行（省 token 靠服务端 VAD）。
        rig.feed(quiet_block());
        assert_eq!(rig.wire.audio_frames(), 1, "门没有阈值，静音也该上传");
        rig.wait_until(|| rig.wire.audio_frames() >= 1);
        // 听人说话不降噪——数字源本来就干净。
        assert_eq!(rig.dsp.denoise_calls.load(Ordering::SeqCst), 0);
        rig.engine.shutdown();
    }

    #[test]
    fn denoise_is_skipped_when_the_capture_rate_is_not_48k() {
        // 降噪只在 48 kHz 有效，率对不上就跳过，别把信号搞坏。
        let rig = Rig::with_rate(44_100);
        let mut config = speak_config();
        config.gate_active = true;
        rig.start(config);

        rig.feed(vec![0.5; 1764]);
        assert_eq!(rig.dsp.denoise_calls.load(Ordering::SeqCst), 0);
        rig.engine.shutdown();
    }

    // --- 直通（关翻译=输出原声） -------------------------------------------

    #[test]
    fn passthrough_sends_original_voice_to_the_output_device_without_connecting() {
        let rig = Rig::new();
        let mut config = speak_config();
        config.translate = false;
        config.gate_active = true;
        rig.start_passthrough(config);

        // 直通不连云端：没有握手、没有传输。
        assert_eq!(rig.wire.connects.load(Ordering::SeqCst), 0, "直通不该连 WS");

        // 闸门开着，灌一块响音频 → 原声进播放汇，不上传。
        rig.feed(loud_block());
        rig.wait_until(|| !rig.speaker.played.lock().is_empty());
        let played = rig.speaker.played.lock().clone();
        assert!(!played.is_empty(), "原声该进输出设备");
        assert!(
            played.iter().all(|&s| (s - 0.5).abs() < 0.01),
            "播放的原声该是被推的整块值"
        );
        assert_eq!(rig.wire.audio_frames(), 0, "直通没有上传帧");
        rig.engine.shutdown();
    }

    #[test]
    fn passthrough_stays_silent_while_the_gate_is_closed() {
        let rig = Rig::new();
        let mut config = speak_config();
        config.translate = false;
        // 闸门默认关着（热键没按）。
        rig.start_passthrough(config);

        rig.feed(loud_block());
        rig.feed(loud_block());

        assert!(
            rig.speaker.played.lock().is_empty(),
            "闸关着不许把原声送出去"
        );
        assert_eq!(rig.wire.audio_frames(), 0);
        rig.engine.shutdown();
    }

    // --- 队列：满了丢最旧的 ------------------------------------------------

    #[test]
    fn a_full_input_queue_drops_the_oldest_block() {
        let inbox = Inbox::default();
        let chunk = |tag: f32| AudioChunk {
            samples: vec![tag],
            sample_rate: 48_000,
            channels: 1,
        };
        // 灌满 8 格。
        for i in 0..INPUT_QUEUE_SIZE {
            inbox.push_audio(chunk(i as f32));
        }
        assert_eq!(inbox.dropped(), 0, "刚好装满不该丢");

        // 第 9 块进来，最旧的（0 号）被挤掉。
        inbox.push_audio(chunk(999.0));
        assert_eq!(inbox.dropped(), 1);
        assert_eq!(inbox.state.lock().audio.len(), INPUT_QUEUE_SIZE, "深度不变");
        let first = inbox.take_audio().expect("队列里该有货");
        assert_eq!(first.chunk.samples[0], 1.0, "掉的该是最旧的那块");
    }

    #[test]
    fn dropped_blocks_only_warn_on_the_first_and_every_25th() {
        // 警告频率是照抄旧版的：第 1 次、之后每 25 次。日志被刷爆比丢帧更难查问题。
        let should_warn = |dropped: u64| dropped == 1 || dropped.is_multiple_of(25);
        assert!(should_warn(1));
        assert!(!should_warn(2));
        assert!(!should_warn(24));
        assert!(should_warn(25));
        assert!(!should_warn(26));
        assert!(should_warn(50));
    }

    #[test]
    fn audio_that_arrives_after_stop_is_dropped_not_queued() {
        let inbox = Inbox::default();
        inbox.stop();
        inbox.push_audio(AudioChunk {
            samples: vec![0.5],
            sample_rate: 48_000,
            channels: 1,
        });
        assert!(inbox.state.lock().audio.is_empty(), "停了之后的音频直接丢");
    }

    // --- 序号闸 ------------------------------------------------------------

    #[test]
    fn a_stale_gate_command_is_discarded() {
        let rig = Rig::new();
        let session = rig.start(speak_config());

        // seq 5 开闸。
        rig.engine
            .apply(PipelineCommand::SetGateActive {
                session_id: session,
                seq: 5,
                active: true,
            })
            .unwrap();
        rig.feed(loud_block());
        rig.wait_until(|| rig.wire.audio_frames() >= 1);
        let after_open = rig.wire.audio_frames();

        // seq 3 关闸——比 5 旧，是被超车的过期命令，必须扔掉。
        rig.engine
            .apply(PipelineCommand::SetGateActive {
                session_id: session,
                seq: 3,
                active: false,
            })
            .unwrap();
        rig.feed(loud_block());
        rig.wait_until(|| rig.wire.audio_frames() > after_open);
        assert!(
            rig.wire.audio_frames() > after_open,
            "过期的关闸命令不该真把闸关上"
        );

        // seq 6 关闸——比 5 新，认。
        rig.engine
            .apply(PipelineCommand::SetGateActive {
                session_id: session,
                seq: 6,
                active: false,
            })
            .unwrap();
        // 闸关上后第一块会补一段静音尾（服务端靠它切段），之后就彻底没了。
        rig.feed(loud_block());
        let settled = rig.wire.audio_frames();
        rig.feed(loud_block());
        rig.feed(loud_block());
        assert_eq!(rig.wire.audio_frames(), settled, "新命令该真把闸关上");
        rig.engine.shutdown();
    }

    #[test]
    fn gate_commands_for_another_session_go_nowhere() {
        let rig = Rig::new();
        rig.start(speak_config());
        // 会话号对不上：这是别人的命令。
        rig.engine
            .apply(PipelineCommand::SetGateActive {
                session_id: 998,
                seq: 1,
                active: true,
            })
            .unwrap();
        rig.feed(loud_block());
        assert_eq!(rig.wire.audio_frames(), 0, "别人的开闸命令不该开我的闸");
        rig.engine.shutdown();
    }

    // --- 阀门状态节流 ------------------------------------------------------

    /// 数一数账本发出的阀门状态事件。
    fn gate_events(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::GateStatus { .. }))
            .count()
    }

    #[test]
    fn gate_status_is_throttled_to_one_every_200ms() {
        let rig = Rig::new();
        // 默认激活方式是 Toggle，配的是电平门，静音/说话两个状态好摆。
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        // 第一块静音：状态从"没报过"变成 Silence，报一次。
        rig.feed(quiet_block());
        assert_eq!(gate_events(&rig.events()), 1);

        // 时钟不动、状态不变的后续块：全被节流吃掉。25 Hz 的 RMS 会把 UI 刷爆。
        rig.drain_events();
        for _ in 0..5 {
            rig.feed(quiet_block());
        }
        assert_eq!(gate_events(&rig.events()), 0, "同状态又没到 200 ms，不许报");

        // 满 200 ms 了，哪怕状态没变也报一次（UI 要知道 RMS 还在动）。
        rig.clock.advance(GATE_THROTTLE_MS);
        rig.feed(quiet_block());
        assert_eq!(gate_events(&rig.events()), 1);
        rig.engine.shutdown();
    }

    #[test]
    fn a_state_change_reports_immediately_even_inside_the_throttle_window() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.feed(quiet_block());
        rig.drain_events();

        // 时钟一动不动，但状态从 Silence 变成 Speech：这是要紧事，立刻报。
        rig.feed(loud_block());
        assert_eq!(gate_events(&rig.events()), 1, "状态变了就得马上报");
        rig.engine.shutdown();
    }

    #[test]
    fn changing_the_gate_config_resets_the_throttle() {
        let rig = Rig::new();
        let session = rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.feed(quiet_block());
        rig.drain_events();

        // 换门：节流的两个记录都得清掉，不然新门的第一拍会被吞。
        rig.engine
            .apply(PipelineCommand::SetGateConfig {
                session_id: session,
                seq: 1,
                config: GateConfig::level(0.5),
            })
            .unwrap();
        rig.feed(quiet_block());
        assert_eq!(gate_events(&rig.events()), 1, "换门后第一拍必须报");
        rig.engine.shutdown();
    }

    // --- 服务端消息：字幕 / 用量 / 语音 ------------------------------------

    /// 攒一片文字增量。解码器自己往后拼，所以传的是**这一片**，不是整句。
    fn text_delta(piece: &str) -> String {
        format!(
            r#"{{"type":"response.text.delta","delta":{}}}"#,
            serde_json::to_string(piece).unwrap()
        )
    }

    /// 一句说完了，`text` 是服务端给的整句终稿。
    fn text_done(full: &str) -> String {
        format!(
            r#"{{"type":"response.text.done","text":{}}}"#,
            serde_json::to_string(full).unwrap()
        )
    }

    fn audio_delta(pcm: &[i16]) -> String {
        let mut bytes = Vec::new();
        for sample in pcm {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!(r#"{{"type":"response.audio.delta","delta":"{b64}"}}"#)
    }

    fn speech_started(audio_start_ms: u64) -> String {
        format!(
            r#"{{"type":"input_audio_buffer.speech_started","audio_start_ms":{audio_start_ms},"item_id":"item-a"}}"#
        )
    }

    /// 只挑字幕事件里的增量文本。
    fn subtitle_deltas(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SubtitleDelta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cumulative_text_deltas_are_diffed_into_suffixes() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        // 服务端发的是整句，字幕轨是往后追加的，所以只该看见后缀。
        // 协议层每片都吐"到目前为止的整句"，流水线得自己减掉发过的部分。
        rig.push_message(text_delta("こんに"));
        rig.push_message(text_delta("ちは"));
        rig.wait_until(|| subtitle_deltas(&rig.events()).len() >= 2);

        assert_eq!(subtitle_deltas(&rig.events()), vec!["こんに", "ちは"]);
        rig.engine.shutdown();
    }

    #[test]
    fn a_rewritten_sentence_falls_back_to_the_whole_text() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        rig.push_message(text_delta("こんに"));
        // 服务端把整句改译了（`done` 里的终稿不以发过的开头）：整句重发，
        // 别硬算差集算出一串乱码。
        rig.push_message(text_done("さようなら"));
        rig.wait_until(|| subtitle_deltas(&rig.events()).len() >= 2);

        assert_eq!(subtitle_deltas(&rig.events()), vec!["こんに", "さようなら"]);
        rig.engine.shutdown();
    }

    #[test]
    fn synthesized_voice_lands_in_the_playback_sink() {
        let rig = Rig::new();
        rig.start(speak_config());

        rig.push_message(audio_delta(&[16384i16, -16384]));
        rig.wait_until(|| rig.speaker.played.lock().len() >= 2);

        let played = rig.speaker.played.lock().clone();
        assert!(
            (played[0] - 0.5).abs() < 0.01,
            "PCM16 该按 32768 归一：{played:?}"
        );
        assert!((played[1] + 0.5).abs() < 0.01);
        rig.engine.shutdown();
    }

    #[test]
    fn translation_audio_can_be_enabled_and_disabled_without_reconnecting() {
        let rig = Rig::new();
        let mut config = speak_config();
        config.voice = None;
        let output_device = config.output_device.clone();
        let session = rig.start(config);
        assert_eq!(rig.speaker.opens.load(Ordering::SeqCst), 0);

        rig.engine
            .apply(PipelineCommand::SetTranslationAudio {
                session_id: session,
                voice: Some("Tina".to_string()),
                output_device: output_device.clone(),
            })
            .unwrap();
        rig.wait_until(|| rig.speaker.opens.load(Ordering::SeqCst) == 1);
        rig.wait_until(|| rig.wire.sent().len() >= 2);
        let enabled = rig.wire.sent();
        assert!(enabled
            .last()
            .unwrap()
            .contains(r#""modalities":["text","audio"]"#));
        assert_eq!(rig.wire.connects.load(Ordering::SeqCst), 1);

        rig.engine
            .apply(PipelineCommand::SetTranslationAudio {
                session_id: session,
                voice: None,
                output_device,
            })
            .unwrap();
        rig.wait_until(|| rig.speaker.closes.load(Ordering::SeqCst) == 1);
        rig.wait_until(|| rig.wire.sent().len() >= 3);
        let disabled = rig.wire.sent();
        assert!(disabled
            .last()
            .unwrap()
            .contains(r#""modalities":["text"]"#));
        assert_eq!(
            rig.wire.connects.load(Ordering::SeqCst),
            1,
            "切换 TTS 不该重连"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn latency_keeps_server_vad_as_the_frontend_subtraction_baseline() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        // 本地检测到语音的时刻是 0；120ms 后服务端确认 speech_started。
        rig.feed(loud_block());
        rig.clock.advance(120);
        rig.push_message(speech_started(0));
        rig.wait_until(|| {
            rig.latency_snapshot(Pipeline::Speak)
                .and_then(|latency| latency.server_vad.last_ms)
                == Some(120)
        });

        // 首字累计值仍以原始语音起点计；前端减去 server_vad 后得到 380ms。
        rig.clock.advance(380);
        rig.push_message(text_delta("訳"));
        rig.wait_until(|| {
            rig.latency_snapshot(Pipeline::Speak)
                .and_then(|latency| latency.first_text.last_ms)
                == Some(500)
        });
        let latency = rig.latency_snapshot(Pipeline::Speak).unwrap();
        assert_eq!(
            latency.first_text.last_ms.unwrap() - latency.server_vad.last_ms.unwrap(),
            380
        );
        rig.engine.shutdown();
    }

    #[test]
    fn headphone_monitor_can_be_toggled_without_reconnecting() {
        let rig = Rig::new();
        let session = rig.start(speak_config());
        assert_eq!(rig.speaker.opens.load(Ordering::SeqCst), 1);

        rig.engine
            .apply(PipelineCommand::SetMonitorTranslation {
                session_id: session,
                enabled: true,
            })
            .unwrap();
        rig.wait_until(|| rig.speaker.opens.load(Ordering::SeqCst) == 2);
        assert_eq!(
            rig.wire.connects.load(Ordering::SeqCst),
            1,
            "回听不该重连云端"
        );

        rig.push_message(audio_delta(&[16384i16, -16384]));
        rig.wait_until(|| rig.speaker.played.lock().len() >= 4);
        assert_eq!(
            rig.speaker.played.lock().len(),
            4,
            "主输出和回听应各收到一份"
        );

        rig.engine
            .apply(PipelineCommand::SetMonitorTranslation {
                session_id: session,
                enabled: false,
            })
            .unwrap();
        rig.wait_until(|| rig.speaker.closes.load(Ordering::SeqCst) >= 1);
        rig.speaker.played.lock().clear();
        rig.push_message(audio_delta(&[8192i16, -8192]));
        rig.wait_until(|| rig.speaker.played.lock().len() >= 2);
        assert_eq!(rig.speaker.played.lock().len(), 2, "关闭后只保留主输出");
        rig.engine.shutdown();
    }

    #[test]
    fn monitor_does_not_duplicate_an_already_default_primary_output() {
        let rig = Rig::new();
        let mut config = speak_config();
        config.output_device = None;
        config.monitor_translation = true;
        rig.start(config);

        assert_eq!(
            rig.speaker.opens.load(Ordering::SeqCst),
            1,
            "主输出已经是系统默认时不能再开一路造成重音"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn every_finished_turn_is_recorded_into_usage() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);

        rig.push_message(
            r#"{"type":"response.done","response":{"usage":{"input_tokens":100,"output_tokens":40}}}"#,
        );
        rig.wait_until(|| !rig.runtime.usage().models.is_empty());

        // 记在**这条会话用的模型**名下，不是记一笔糊涂账。
        let model = rig
            .runtime
            .usage()
            .models
            .get(MODEL)
            .cloned()
            .expect("该有这个模型");
        assert_eq!(model.total.turns, 1, "每个 response.done 记一轮");
        assert_eq!(model.total.input_tokens, 100);
        assert_eq!(model.total.output_tokens, 40);
        assert_eq!(model.total.total_tokens, 140, "服务端没给总数就自己加");
        rig.engine.shutdown();
    }

    // --- 热更新 ------------------------------------------------------------

    #[test]
    fn hot_update_sends_a_session_update_without_reconnecting() {
        let rig = Rig::new();
        let session = rig.start(speak_config());

        rig.engine
            .apply(PipelineCommand::HotUpdate {
                session_id: session,
                target_language: Some("en".to_string()),
                voice: Some("Cherry".to_string()),
            })
            .unwrap();
        rig.wait_until(|| rig.wire.sent().len() >= 2);

        let sent = rig.wire.sent();
        assert!(sent[1].contains("session.update"));
        assert!(sent[1].contains("\"en\""), "得带新语言：{}", sent[1]);
        assert!(sent[1].contains("Cherry"), "得带新音色：{}", sent[1]);
        // 换语言不重连——重连要几百毫秒，说话就断了。
        assert_eq!(rig.wire.connects.load(Ordering::SeqCst), 1);
        assert_eq!(rig.wire.closes.load(Ordering::SeqCst), 0);
        rig.engine.shutdown();
    }

    #[test]
    fn listen_ignores_hot_update() {
        // 听人说话的目标语言固定中文，热更新对它没有意义。
        let rig = Rig::new();
        let session = rig.start(listen_config());
        rig.engine
            .apply(PipelineCommand::HotUpdate {
                session_id: session,
                target_language: Some("en".to_string()),
                voice: None,
            })
            .unwrap();
        // 听人说话是电平门阈值 0，静音也上传；拿一拍当围栏，确保通知已被消化过。
        rig.feed(quiet_block());
        rig.wait_until(|| rig.wire.sent().len() >= 2);
        // 除了启动握手那一次 session.update，热更新不许再发第二个。
        let updates = rig
            .wire
            .sent()
            .iter()
            .filter(|f| f.contains("session.update"))
            .count();
        assert_eq!(
            updates, 1,
            "听人说话不响应热更新，不该有第二帧 session.update"
        );
        assert!(
            rig.wire.sent().iter().all(|f| !f.contains("\"en\"")),
            "别把新语言热进去：{:?}",
            rig.wire.sent()
        );
        rig.engine.shutdown();
    }

    #[test]
    fn an_empty_hot_update_sends_nothing() {
        let rig = Rig::new();
        let session = rig.start(speak_config());
        rig.engine
            .apply(PipelineCommand::HotUpdate {
                session_id: session,
                target_language: None,
                voice: None,
            })
            .unwrap();
        rig.feed(quiet_block());
        assert_eq!(rig.wire.sent().len(), 1, "什么都没改就别发帧");
        rig.engine.shutdown();
    }

    // --- 连不上 / 断线重连 / 死错误 ----------------------------------------

    #[test]
    fn being_stopped_during_connect_exits_without_crying_wolf() {
        let rig = Rig::new();
        // 连接会慢 120 ms，正好在这段窗口里插一刀 Stop。
        rig.wire.block_connect.store(true, Ordering::SeqCst);
        rig.runtime.start(Pipeline::Speak);
        rig.wait_until(|| rig.wire.connect_waiting.load(Ordering::SeqCst));

        // Stop 是握手式的：它会等工作线程收完尾才返回。
        rig.runtime.stop(Pipeline::Speak);
        assert!(rig.engine.workers.lock().is_empty(), "Stop 返回时人该走了");

        // 用户自己按的停，不该弹"连接失败"，也不该留个 Failed 状态。
        assert_eq!(
            rig.runtime.pipeline_state(Pipeline::Speak),
            PipelineState::Idle
        );
        assert!(
            !rig.events().iter().any(
                |e| matches!(e, Event::Notice { notice } if notice.severity == Severity::Error)
            ),
            "自己停的不该报错：{:?}",
            rig.events()
        );
        rig.engine.shutdown();
    }

    #[test]
    fn a_dropped_connection_reconnects_with_backoff() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        // 对端跑了：报重连中，然后自己接回来。
        rig.push_closed();
        rig.wait_until(|| rig.wire.connects.load(Ordering::SeqCst) >= 2);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);

        assert!(
            rig.events().iter().any(|e| matches!(
                e,
                Event::PipelineState {
                    state: PipelineState::Reconnecting,
                    ..
                }
            )),
            "断了要让用户看见在重连"
        );
        // 重连也要重新握手，不然服务端按默认配置来，翻译语言是错的。
        assert!(
            rig.wire
                .sent()
                .iter()
                .filter(|f| f.contains("session.update"))
                .count()
                >= 2,
            "重连后要重发握手"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn a_transient_connect_failure_is_retried() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);

        // 断线后第一次重连也失败（网还没回来），第二次才成。
        rig.wire.fail_connects.store(1, Ordering::SeqCst);
        rig.push_closed();
        rig.wait_until(|| rig.wire.connects.load(Ordering::SeqCst) >= 3);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        assert_ne!(
            rig.runtime.pipeline_state(Pipeline::Speak),
            PipelineState::Failed,
            "临时性的连不上不该判死"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn a_fatal_connect_error_fails_instead_of_retrying_forever() {
        let rig = Rig::new();
        rig.wire.fatal_connect.store(true, Ordering::SeqCst);
        rig.runtime.start(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Failed);

        // 密钥错了重连一万次也是这个结果：只连一次，然后报人话。
        assert_eq!(
            rig.wire.connects.load(Ordering::SeqCst),
            1,
            "死错误不许重试"
        );
        let error = rig.runtime.snapshot().speak.last_error.expect("该记下错误");
        assert!(error.contains("密钥"), "得是给人看的话：{error}");
        rig.engine.shutdown();
    }

    #[test]
    fn a_fatal_server_error_kills_the_session_without_reconnecting() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);

        rig.push_message(
            r#"{"type":"error","error":{"code":"invalid_api_key","message":"Incorrect API key provided."}}"#,
        );
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Failed);

        // 死错误 = 收摊，不是重连。线程自己退出，但把手留在表里：让工作线程
        // 自己摘登记会跟持锁 join 的 start/stop 撞死锁，下一次 start 或
        // shutdown 顺手回收就行（那时 join 立刻返回）。
        rig.wait_until(|| rig.wire.closes.load(Ordering::SeqCst) == 1);
        assert_eq!(
            rig.wire.connects.load(Ordering::SeqCst),
            1,
            "死错误不许重连"
        );
        rig.engine.shutdown();
    }

    #[test]
    fn a_soft_server_error_only_warns_and_keeps_running() {
        let rig = Rig::new();
        rig.boot(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
        rig.drain_events();

        rig.push_message(
            r#"{"type":"error","error":{"code":"rate_limit_exceeded","message":"too fast"}}"#,
        );
        rig.wait_until(|| {
            rig.events().iter().any(
                |e| matches!(e, Event::Notice { notice } if notice.severity == Severity::Warning),
            )
        });

        // 限流是能缓过来的，别把会话判死。
        assert_ne!(
            rig.runtime.pipeline_state(Pipeline::Speak),
            PipelineState::Failed
        );
        assert!(rig.engine.workers.lock().contains_key(&Pipeline::Speak));
        rig.engine.shutdown();
    }

    #[test]
    fn a_capture_failure_reports_a_readable_error() {
        let rig = Rig::new();
        rig.mic.fail.store(true, Ordering::SeqCst);
        rig.runtime.start(Pipeline::Speak);
        rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Failed);

        let error = rig.runtime.snapshot().speak.last_error.expect("该记下错误");
        assert!(error.contains("麦克风"), "得说清是麦克风的事：{error}");
        rig.engine.shutdown();
    }

    #[test]
    fn a_broken_denoiser_degrades_instead_of_failing() {
        let rig = Rig::new();
        rig.dsp.fail.store(true, Ordering::SeqCst);
        let mut config = speak_config();
        config.gate_active = true;
        rig.start(config);

        // 降噪起不来就不降噪，会话照跑——总比彻底不能说话好。
        rig.feed(loud_block());
        rig.wait_until(|| rig.wire.audio_frames() >= 1);
        assert_eq!(rig.dsp.denoise_calls.load(Ordering::SeqCst), 0);
        rig.engine.shutdown();
    }

    #[test]
    fn shutdown_retires_every_worker() {
        let rig = Rig::new();
        rig.start(speak_config());
        rig.start(listen_config());
        assert_eq!(rig.engine.workers.lock().len(), 2);

        rig.engine.shutdown();
        assert!(rig.engine.workers.lock().is_empty(), "关店得把人都送走");
        assert!(rig.wire.closes.load(Ordering::SeqCst) >= 2, "socket 都得关");
    }
}
