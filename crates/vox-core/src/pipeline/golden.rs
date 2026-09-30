//! 差分台（S4-B B0）：15 组配置各跑一段脚本，把**可观察的外部行为**渲染成一份
//! 纯文本轨迹，与 `golden/<id>.txt` 逐字比。
//!
//! 存在的全部理由：S4-B 要把 `Worker` 里写死的"单声道 → 降噪 → 阀门 → 重采样"
//! 换成一条按 `Composition.ops` 顺序走的有序节列表，**行为必须一模一样**。
//! "一模一样"不能靠 `git diff` 说话（`diff` 看不见音频样本与帧边界），所以有了它。
//! 这 15 份金样是在**重构前**的代码上录的；B2 之后它们必须一个字都不用改。
//!
//! ## 轨迹里放什么
//!
//! 七节，节内按发生顺序、节间固定顺序（`meta` 打头，其余照可观察面的分类排）：
//!
//! | 节 | 一行的形状 |
//! | --- | --- |
//! | `meta` | 场景 id + 配置摘要，给人肉定位用 |
//! | `wire` | `<序号> <帧摘要>`：非音频帧打**原样 JSON**，音频帧打 `audio_append <帧号> <样本数> <FNV-1a64>` |
//! | `play` | 播放汇开了几次/设备是谁、总共推了多少样本、冲了几次、关了几次 |
//! | `dsp` | 降噪的调用次数与复位次数 |
//! | `capture` | 采集开的抓谁、停了几次 |
//! | `event` | 账本事件流的紧凑 JSON |
//! | `wire_life` | socket 的连接/关闭计数 |
//!
//! `wire` 是最有价值的一节：`Wire::sent` 是 `Vec<String>`，顺序即发送顺序，
//! 而 `Worker::upload` 逐块调 `transport.send`——**帧边界与块边界都在指纹里**。
//!
//! ## 轨迹里不放什么
//!
//! - **`Event::LatencyChanged` 的内容**。它有两个墙钟来源：`Inbox::push_audio` 的
//!   `enqueued_at: Instant::now()`（→ `queue_ms` → `latency.input_queue`、`capture_end_ms`
//!   → `upload_timeline` → `capture_time` → `server_vad`）与 `Worker::boot` 的
//!   `connect_started`（→ `connect_ms`）。**都不可复现。** 轨迹里只记一行
//!   `event latency_changed <n>`，记它出现过几次。
//! - **`tracing::warn!` 的文本**。差分台接的是 `Event` 流，抓不到 `tracing`。采集率
//!   ≠ 48 kHz 那句（`Worker::boot`）**不发 `Notice`**，所以事件流里根本看不见它；
//!   由 B2 逐字保留 + 一条覆盖两个降级分支的单测兜住。
//! - **线程交错**。每个场景只跑一条流水线，每一步都用 `Rig::feed` / `wait_until`
//!   同步到确定的位置，场景收尾 `engine.shutdown()`（握手式，join 工作线程）之后
//!   才读 `events`。
//! - **墙钟**。`Runtime::now_ms()` 走 `TestClock`，节流（阀门 200 ms、延迟 500 ms）
//!   全靠它。脚本里**只**手动 `clock.advance(ms)`，时间绝不自己流。
//!
//! ## 重录
//!
//! `VOX_GOLDEN_REGEN=1 cargo test -p vox-core pipeline::golden` 覆写文件。显式开关，
//! 验收命令里绝不含这个变量。`include_str!` 让"文件缺失"变成**编译错**，而不是
//! 运行期一个看不懂的 IO 错。

use std::sync::atomic::Ordering;

use super::tests::{speak_config, Rig};
use super::GATE_THROTTLE_MS;
use crate::catalog::ActivationMode;
use crate::event::{Event, Pipeline, PipelineState};
use crate::gate::GateConfig;
use crate::ports::CaptureTarget;
use crate::runtime::{PipelineCommand, PipelineControl};
use crate::settings::{ListenSettings, ListenTarget, Settings, SpeakSettings};

// --- 脚本里用的常量 --------------------------------------------------------

const RATE_48: u32 = 48_000;
const RATE_44: u32 = 44_100;
const RATE_16: u32 = 16_000;
/// 40 ms 一块。三种采集率各按自己的率换算块长。
const BLOCK_48: usize = 1920;
const BLOCK_44: usize = 1764;
const BLOCK_16: usize = 640;

/// 一段"正在说话"的块。**同一个 `seed` 永远给同一串数**——轨迹要逐字可复现，
/// 音频内容只能由种子算出来，不能用随机数。`seed` 让相邻两块内容不同，指纹才
/// 看得出块边界。
fn loud(n: usize, seed: u32) -> Vec<f32> {
    (0..n)
        .map(|j| 0.20 + 0.01 * ((j as u32 * 7 + seed * 13) % 5) as f32)
        .collect()
}

/// 一段"没人说话"的块。RMS 远低于 0.02，电平门不会误开。
fn quiet(n: usize, seed: u32) -> Vec<f32> {
    (0..n)
        .map(|j| 0.0005 * ((j as u32 * 3 + seed * 11) % 4) as f32)
        .collect()
}

/// FNV-1a 64。纯 `u64` 运算，无依赖、无分配、跨机器可复现。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 音频块的指纹。逐样本取 `to_le_bytes` 再喂 FNV。
fn sample_hash(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    format!("{:016x}", fnv1a64(&bytes))
}

// --- 起会话 ----------------------------------------------------------------

/// 起一条 Speak 流水线，走**账本**这条路（所以 `Event` 流是全的：`Rig::start`
/// 绕过了账本，账本会按会话号把回调全挡掉）。
///
/// 公共设置先落一遍：手动门、降噪开、开翻译、译音进虚拟麦。`edit` 在这之后改个别格。
///
/// `open_mic` = 起会话之前就把麦克风标志置真：账本会顺手把流水线开起来，于是门的
/// **初始**状态就是开着的（`Worker::boot` 里同步生效，不靠后续命令补）。
/// `false` 则门初始关着。
fn start_speak(rig: &Rig, open_mic: bool, edit: impl FnOnce(&mut SpeakSettings)) {
    rig.runtime.update_settings(|settings: &mut Settings| {
        let speak = &mut settings.speak;
        speak.activation_mode = ActivationMode::Hold;
        speak.denoise = true;
        speak.translate = true;
        speak.monitor_translation = false;
        speak.target_language = "ja".to_string();
        speak.voice = "Tina".to_string();
        speak.output_device = Some("CABLE Input".to_string());
        edit(speak);
    });
    // 配置期那一堆 `SettingsChanged` 噪声不进轨迹：从下一行开始才是场景。
    rig.drain_events();
    if open_mic {
        // 还没开流水线就把开麦标志置真 → 账本顺手开它（`set_mic_active` 里的 autostart）。
        rig.runtime.set_mic_active(true);
    } else {
        rig.runtime.start(Pipeline::Speak);
    }
    rig.wait_until(|| rig.mic.target().is_some());
    rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
}

/// 起一条 Listen 流水线。跟 Speak 同一条路，只是设置落在 `settings.listen` 上。
/// 门是 `GateConfig::level(0.0)`（恒开）、不认 `HotUpdate`、不降噪——那是
/// `listen::composition` 写死的，场景脚本改不了，只能照着它跑。
fn start_listen(rig: &Rig, edit: impl FnOnce(&mut ListenSettings)) {
    rig.runtime.update_settings(|settings: &mut Settings| {
        let listen = &mut settings.listen;
        listen.speak_translation = true;
        listen.voice = "Tina".to_string();
        listen.output_device = None;
        listen.target = Some(ListenTarget {
            executable: "Discord.exe".to_string(),
            display_name: "Discord".to_string(),
            include_process_tree: true,
        });
        edit(listen);
    });
    rig.drain_events();
    rig.runtime.start(Pipeline::Listen);
    rig.wait_until(|| rig.mic.target().is_some());
    rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Listen) == PipelineState::Ready);
}

/// 当下活着的会话号。命令要带会话号，带错了会被账本派发的线程丢掉。
fn session(rig: &Rig, pipeline: Pipeline) -> u64 {
    rig.runtime.session_config(pipeline).session_id
}

fn gate_active(rig: &Rig, seq: u64, active: bool) {
    rig.engine
        .apply(PipelineCommand::SetGateActive {
            session_id: session(rig, Pipeline::Speak),
            seq,
            active,
        })
        .expect("SetGateActive 不该失败");
}

fn gate_config(rig: &Rig, seq: u64, config: GateConfig) {
    rig.engine
        .apply(PipelineCommand::SetGateConfig {
            session_id: session(rig, Pipeline::Speak),
            seq,
            config,
        })
        .expect("SetGateConfig 不该失败");
}

fn hot_update(rig: &Rig, target_language: Option<&str>, voice: Option<&str>) {
    rig.engine
        .apply(PipelineCommand::HotUpdate {
            session_id: session(rig, Pipeline::Speak),
            target_language: target_language.map(str::to_string),
            voice: voice.map(str::to_string),
        })
        .expect("HotUpdate 不该失败");
}

// --- 服务端消息 ------------------------------------------------------------

fn json_str(value: &str) -> String {
    serde_json::to_string(value).expect("字符串编不出 JSON")
}

fn text_delta(piece: &str) -> String {
    format!(
        r#"{{"type":"response.text.delta","delta":{}}}"#,
        json_str(piece)
    )
}

fn text_done(full: &str) -> String {
    format!(
        r#"{{"type":"response.text.done","text":{}}}"#,
        json_str(full)
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

fn speech_stopped() -> String {
    r#"{"type":"input_audio_buffer.speech_stopped","audio_end_ms":3900,"item_id":"item-a"}"#
        .to_string()
}

fn turn_done(input_tokens: u32, output_tokens: u32) -> String {
    format!(
        r#"{{"type":"response.done","response":{{"usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens}}}}}}}"#
    )
}

// --- 轨迹渲染 --------------------------------------------------------------

/// 采集目标的紧凑写法。设备名里不许有空格，缺省设备写 `default`。
fn target_summary(target: Option<&CaptureTarget>) -> String {
    match target {
        None => "capture open none".to_string(),
        Some(CaptureTarget::Microphone(None)) => "capture open microphone default".to_string(),
        Some(CaptureTarget::Microphone(Some(device))) => {
            format!("capture open microphone {device}")
        }
        Some(CaptureTarget::ProcessLoopback {
            executable,
            include_tree,
        }) => format!("capture open loopback {executable} tree={include_tree}"),
        Some(CaptureTarget::Net { pipe }) => format!("capture open net {pipe}"),
    }
}

/// 播放汇在 **`engine.shutdown()` 之前**的一份账。
///
/// 必须在收尾之前取：收尾的 `close_sink` 会先 `flush` 再 `close`，而假播放汇的
/// `flush` 把 `played` 清空——shutdown 之后就只剩计数、推过的样本一个都读不到了。
struct PlaySnapshot {
    opens: u64,
    device: Option<Option<String>>,
    played: Vec<f32>,
    flushes: u64,
    closes: u64,
}

fn snapshot_play(rig: &Rig) -> PlaySnapshot {
    PlaySnapshot {
        opens: rig.speaker.opens.load(Ordering::SeqCst),
        device: rig.speaker.opened.lock().clone(),
        played: rig.speaker.played.lock().clone(),
        flushes: rig.speaker.flushes.load(Ordering::SeqCst),
        closes: rig.speaker.closes.load(Ordering::SeqCst),
    }
}

fn device_name(device: &Option<Option<String>>) -> String {
    match device {
        None => "none".to_string(),
        Some(None) => "default".to_string(),
        Some(Some(name)) => name.clone(),
    }
}

/// 把假件上的可观察账渲染成轨迹。
fn render(rig: &Rig, meta: &[(&str, String)], play: &PlaySnapshot) -> String {
    let mut out = String::new();

    out.push_str("# meta\n");
    for (key, value) in meta {
        out.push_str(&format!("meta {key} {value}\n"));
    }

    // wire：发出去的帧，顺序即发送顺序。音频帧换成"帧号 + 样本数 + 指纹"，
    // 非音频帧原样打 JSON（`session.update` / `session.close` 等）。
    out.push_str("# wire\n");
    let mut audio_index = 0usize;
    for (i, frame) in rig.wire.sent().iter().enumerate() {
        if frame.contains("input_audio_buffer.append") {
            let value: serde_json::Value = serde_json::from_str(frame).expect("音频帧是 JSON");
            let encoded = value["audio"].as_str().expect("音频帧带 audio 字段");
            use base64::Engine as _;
            let pcm = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("音频载荷是 base64");
            out.push_str(&format!(
                "wire {i} audio_append {audio_index} {} {:016x}\n",
                pcm.len() / 2,
                fnv1a64(&pcm)
            ));
            audio_index += 1;
        } else {
            out.push_str(&format!("wire {i} {frame}\n"));
        }
    }

    // play：播放汇。假播放汇只留一个平铺的 `played` 缓冲；要逐次 push 都记下来就得改
    // 它的函数体，而这张工单对 `pipeline/mod.rs` 只许改可见性。所以这里记的是"总共
    // 推了多少样本"（样本内容进指纹）。逐块边界由 `wire` 那节逐字钉住——上传才是
    // 热路径，它逐块 `transport.send`。
    out.push_str("# play\n");
    out.push_str(&format!(
        "play open {} {}\n",
        play.opens,
        device_name(&play.device)
    ));
    out.push_str(&format!(
        "play pushed {} {}\n",
        play.played.len(),
        sample_hash(&play.played)
    ));
    out.push_str(&format!("play flush {}\n", play.flushes));
    out.push_str(&format!("play close {}\n", play.closes));
    // 收尾之后：停采集、`close_sink`（先冲再关）、关 socket。收尾的顺序与次数也是
    // 行为的一部分，所以单列一行。
    out.push_str(&format!(
        "play after_shutdown open {} flush {} close {}\n",
        rig.speaker.opens.load(Ordering::SeqCst),
        rig.speaker.flushes.load(Ordering::SeqCst),
        rig.speaker.closes.load(Ordering::SeqCst)
    ));

    // dsp：降噪这一节的账。
    out.push_str("# dsp\n");
    out.push_str(&format!(
        "dsp denoise_calls {}\n",
        rig.dsp.denoise_calls.load(Ordering::SeqCst)
    ));
    out.push_str(&format!(
        "dsp denoise_resets {}\n",
        rig.dsp.resets.load(Ordering::SeqCst)
    ));

    // capture：采集开的是谁、停了几次。
    out.push_str("# capture\n");
    out.push_str(&target_summary(rig.mic.target().as_ref()));
    out.push('\n');
    out.push_str(&format!(
        "capture stop {}\n",
        rig.mic.stops.load(Ordering::SeqCst)
    ));

    // event：账本事件流。`LatencyChanged` 只记次数（内容有墙钟，见模块头）。
    out.push_str("# event\n");
    let events = rig.events();
    let latency_events = events
        .iter()
        .filter(|event| matches!(event, Event::LatencyChanged { .. }))
        .count();
    for event in events
        .iter()
        .filter(|event| !matches!(event, Event::LatencyChanged { .. }))
    {
        out.push_str(&format!(
            "event {}\n",
            serde_json::to_string(event).expect("事件编得出 JSON")
        ));
    }
    if latency_events > 0 {
        out.push_str(&format!("event latency_changed {latency_events}\n"));
    }

    // wire_life：socket 的连接/关闭计数。
    out.push_str("# wire_life\n");
    out.push_str(&format!(
        "wire_life connect {}\n",
        rig.wire.connects.load(Ordering::SeqCst)
    ));
    out.push_str(&format!(
        "wire_life close_transport {}\n",
        rig.wire.closes.load(Ordering::SeqCst)
    ));

    out
}

/// 比对（或重录）一份金样。
///
/// `include_str!` 必须在**编译期**拿到文件名，所以 id 只能是字面量——用宏而不是函数。
/// 少了文件就是编译错，不是运行期一个看不懂的 IO 错。
macro_rules! verify {
    ($id:literal, $actual:expr) => {{
        let expected = include_str!(concat!("golden/", $id, ".txt"));
        let actual: &str = &$actual;
        if std::env::var_os("VOX_GOLDEN_REGEN").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/pipeline/golden/",
                $id,
                ".txt"
            );
            std::fs::write(path, actual).expect("金样写不下去");
            eprintln!(
                "VOX_GOLDEN_REGEN=1：已重录 {}.txt（{} 字节）——记得人肉核一遍",
                $id,
                actual.len()
            );
        } else {
            assert_eq!(actual, expected, "轨迹与金样不一致：{}.txt", $id);
        }
    }};
}

// --- 15 个场景 -------------------------------------------------------------

/// 场景 1（基线）：手动门、降噪开、48 kHz、门初始关着。
/// 钉的是：门关着时 preroll 攒、上升沿把 preroll 一次冲出（多帧）、松手出静音尾、
/// 以及重采样进 `Wire` 的**帧数与指纹**。
fn speak_48k_denoise_manual() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, false, |_| {});
    rig.feed(quiet(BLOCK_48, 1));
    rig.feed(quiet(BLOCK_48, 2));
    // 时钟不动、状态不变：这一拍被 200 ms 节流吃掉。
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 3));
    // 按下：上升沿把 preroll 冲出来，再跟当前块。
    gate_active(&rig, 5, true);
    rig.feed(loud(BLOCK_48, 4));
    rig.feed(loud(BLOCK_48, 5));
    // 松手：补一段静音尾触发服务端断句。
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_48, 6));
    rig.feed(quiet(BLOCK_48, 7));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 8));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_denoise_manual".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "false".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 2：与 1 逐字同构，只把降噪关掉。
/// 钉的是：降噪那一节**不装**时，除了 `dsp denoise_calls` 那一行，轨迹与场景 1
/// **逐字相同**——这就是"装不装这一节由清单说了算"最正面的一条证据。
fn speak_48k_denoise_off() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, false, |speak| speak.denoise = false);
    rig.feed(quiet(BLOCK_48, 1));
    rig.feed(quiet(BLOCK_48, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 3));
    gate_active(&rig, 5, true);
    rig.feed(loud(BLOCK_48, 4));
    rig.feed(loud(BLOCK_48, 5));
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_48, 6));
    rig.feed(quiet(BLOCK_48, 7));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 8));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_denoise_off".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "off".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "false".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 3：电平门（阈值 0.02）。
/// 钉的是：`Silence → Speech → Tail → TailEnd → Silence` 的状态序列、600 ms 的
/// tail 窗口，以及 `ended` 那一拍重采样 `flush` 出来的尾巴（假重采样器给空，
/// 所以那一拍不多发帧——"不多发"本身就是被钉住的行为）。
fn speak_48k_gate_level() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |speak| {
        speak.activation_mode = ActivationMode::Toggle;
        speak.gate_threshold = 0.02;
    });
    rig.feed(quiet(BLOCK_48, 1));
    rig.feed(loud(BLOCK_48, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    // tail 窗口 600 ms @ 48 kHz = 28800 样本 = 15 块：前 14 块 Tail，第 15 块 TailEnd。
    for i in 0..14u32 {
        if i % 5 == 0 {
            rig.clock.advance(GATE_THROTTLE_MS);
        }
        rig.feed(quiet(BLOCK_48, 10 + i));
    }
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 30));
    rig.feed(quiet(BLOCK_48, 31));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(loud(BLOCK_48, 40));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_gate_level".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "level_0.02".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 4：恒开电平门（`level(0.0)`）在 Speak 上是什么样。
/// 钉的是：门**恒** `GateState::Always`、`PipelineState::Active`，以及"门驱动轮次"
/// 这条借用判据（第一块的上升沿就开一个轮次探针）。
fn speak_48k_gate_always_open() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |speak| {
        speak.activation_mode = ActivationMode::Toggle;
        speak.gate_threshold = 0.0;
    });
    rig.feed(quiet(BLOCK_48, 1));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(loud(BLOCK_48, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 3));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 4));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_gate_always_open".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "level_0.0".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 5：直通（关翻译）。
/// 钉的是：不连 WS（`connect 0`）、不上传、不重采样（推给播放汇的是采集率的原样
/// 块，长度不除以 3）、播放出口跟着 `output_device` 而不是音色。
fn speak_48k_passthrough() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |speak| speak.translate = false);
    rig.feed(quiet(BLOCK_48, 1));
    rig.feed(quiet(BLOCK_48, 2));
    gate_active(&rig, 5, true);
    rig.feed(loud(BLOCK_48, 3));
    rig.feed(loud(BLOCK_48, 4));
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_48, 5));
    rig.feed(loud(BLOCK_48, 6));
    rig.feed(loud(BLOCK_48, 7));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_passthrough".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "false".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 6：采集率 44.1 kHz。
/// 钉的是：采集率 ≠ `DENOISE_RATE` 时**不装降噪**（`denoise_calls 0`，只留一句
/// 抓不到的 `tracing::warn!`），重采样按整数比 44100/16000 = 2 抽点。
fn speak_44k_denoise_skipped() -> String {
    let rig = Rig::build(RATE_44);
    start_speak(&rig, true, |_| {});
    rig.feed(loud(BLOCK_44, 1));
    rig.feed(loud(BLOCK_44, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_44, 3));
    rig.feed(quiet(BLOCK_44, 4));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_44k_denoise_skipped".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_44.to_string()),
            ("block_samples", BLOCK_44.to_string()),
            ("denoise", "on_but_rate_mismatch".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 7：降噪器工厂起不来。
/// 钉的是：降级成不降噪但**会话照跑**，并且发一条 `Notice::warning("降噪启动失败，
/// 本次已关闭")`——降噪降级里唯一能被事件流看见的那一条。
fn speak_48k_denoise_factory_fails() -> String {
    let rig = Rig::build(RATE_48);
    rig.dsp.fail.store(true, Ordering::SeqCst);
    start_speak(&rig, false, |_| {});
    rig.feed(quiet(BLOCK_48, 1));
    rig.feed(quiet(BLOCK_48, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 3));
    gate_active(&rig, 5, true);
    rig.feed(loud(BLOCK_48, 4));
    rig.feed(loud(BLOCK_48, 5));
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_48, 6));
    rig.feed(quiet(BLOCK_48, 7));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 8));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_denoise_factory_fails".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "factory_failed".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "false".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 8：采集率 16 kHz == 会话率。
/// 钉的是：重采样走整数比 1（假件给的是"抽点"，比例 1 = 原样），块边界与帧数
/// 按比例与 48 kHz 那组一致；顺带钉住"采集率不是 48 kHz 就不装降噪"在这一组同样成立。
fn speak_16k_capture() -> String {
    let rig = Rig::build(RATE_16);
    start_speak(&rig, true, |_| {});
    rig.feed(loud(BLOCK_16, 1));
    rig.feed(loud(BLOCK_16, 2));
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_active(&rig, 6, false);
    rig.feed(quiet(BLOCK_16, 3));
    rig.feed(quiet(BLOCK_16, 4));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_16k_capture".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_16.to_string()),
            ("block_samples", BLOCK_16.to_string()),
            ("denoise", "on_but_rate_mismatch".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 9：热更新。
/// 钉的是：`session.update` 帧**逐字**、空改动**不发帧**、换语言换音色**不重连**，
/// 以及（INV-1）整条上行链一动不动——降噪计数继续涨、阀门照旧放行。
fn speak_48k_hot_update() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |_| {});
    rig.feed(loud(BLOCK_48, 1));
    hot_update(&rig, Some("ko"), Some("Cherry"));
    rig.feed(loud(BLOCK_48, 2));
    hot_update(&rig, None, Some("Tina"));
    rig.feed(loud(BLOCK_48, 3));
    // 两格都没给：不该发帧。
    hot_update(&rig, None, None);
    rig.feed(loud(BLOCK_48, 4));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_hot_update".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 10：阀门命令与序号闸。
/// 钉的是：过期 `seq` 被扔掉、新 `seq` 认、`set_config` **保留 `external_active`**
/// （换门之后按同参数的手动门仍然放行，说明外部激活状态没被 `reset` 抹掉）、
/// 换门时节流复位（新门的第一拍必须报）。
fn speak_48k_gate_commands() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, false, |_| {});
    // 闸关着：一块静音进 preroll。
    rig.feed(quiet(BLOCK_48, 1));
    gate_active(&rig, 5, true);
    rig.feed(loud(BLOCK_48, 2));
    // 换门（参数与原来一样，值不值得换另说）：`set_config` 会 reset 门内部状态，
    // 但保留 `external_active`。所以这一拍还是 Manual 放行，而不是退回 Waiting。
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_config(&rig, 7, GateConfig::MANUAL);
    rig.feed(quiet(BLOCK_48, 3));
    // 换成电平门：放行与否改由 RMS 说了算。
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_config(&rig, 8, GateConfig::level(0.02));
    rig.feed(loud(BLOCK_48, 4));
    // 过期命令（seq 3 < 8）必须被扔掉：门照旧放行。
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_active(&rig, 3, false);
    rig.feed(loud(BLOCK_48, 5));
    // 换回手动门：门内部状态被 reset，重新走上升沿。
    rig.clock.advance(GATE_THROTTLE_MS);
    gate_config(&rig, 9, GateConfig::MANUAL);
    rig.feed(loud(BLOCK_48, 6));
    // 新的关闸命令：补静音尾。
    gate_active(&rig, 10, false);
    rig.feed(quiet(BLOCK_48, 7));
    rig.clock.advance(GATE_THROTTLE_MS);
    rig.feed(quiet(BLOCK_48, 8));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_gate_commands".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual_then_level_then_manual".to_string()),
            ("gate_active_at_boot", "false".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 11：译音开关与回听。
/// 钉的是：`SetTranslationAudio` 关掉语音 → `flush`/`close` 的顺序，关掉之后译音
/// 无处可去，再开回来 → `open`，以及 `SetMonitorTranslation(true)` 之后主输出与
/// 回听各收一份。
fn speak_48k_translation_audio_toggle() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |_| {});
    rig.push_message(audio_delta(&[16384, -16384]));
    rig.wait_until(|| rig.speaker.played.lock().len() >= 2);
    // 关掉译音：播放汇先冲再关。
    rig.engine
        .apply(PipelineCommand::SetTranslationAudio {
            session_id: session(&rig, Pipeline::Speak),
            voice: None,
            output_device: Some("CABLE Input".to_string()),
        })
        .expect("SetTranslationAudio 不该失败");
    rig.feed(quiet(BLOCK_48, 1));
    rig.wait_until(|| rig.speaker.closes.load(Ordering::SeqCst) >= 1);
    // 无处可去：这一段译音哪儿也不去。
    rig.push_message(audio_delta(&[8192, -8192]));
    rig.feed(quiet(BLOCK_48, 2));
    // 再开回来。
    rig.engine
        .apply(PipelineCommand::SetTranslationAudio {
            session_id: session(&rig, Pipeline::Speak),
            voice: Some("Tina".to_string()),
            output_device: Some("CABLE Input".to_string()),
        })
        .expect("SetTranslationAudio 不该失败");
    rig.wait_until(|| rig.speaker.opens.load(Ordering::SeqCst) == 2);
    rig.push_message(audio_delta(&[4096, -4096]));
    rig.wait_until(|| rig.speaker.played.lock().len() >= 2);
    // 开回听：主输出与回听各一份。
    rig.engine
        .apply(PipelineCommand::SetMonitorTranslation {
            session_id: session(&rig, Pipeline::Speak),
            enabled: true,
        })
        .expect("SetMonitorTranslation 不该失败");
    rig.wait_until(|| rig.speaker.opens.load(Ordering::SeqCst) == 3);
    rig.push_message(audio_delta(&[2048, -2048]));
    rig.wait_until(|| rig.speaker.played.lock().len() >= 4);
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_translation_audio_toggle".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 12：断线重连。
/// 钉的是：连接/关闭计数、重连后重新握手、重连把**有流状态的两节**（降噪、重采样）
/// 复位而**不动门**——所以重连之后没有再发任何开闸命令，音频照旧放行上传。
fn speak_48k_reconnect() -> String {
    let rig = Rig::build(RATE_48);
    start_speak(&rig, true, |_| {});
    rig.feed(loud(BLOCK_48, 1));
    rig.push_closed();
    rig.wait_until(|| rig.wire.connects.load(Ordering::SeqCst) >= 2);
    rig.wait_until(|| rig.runtime.pipeline_state(Pipeline::Speak) == PipelineState::Ready);
    rig.feed(loud(BLOCK_48, 2));
    rig.feed(loud(BLOCK_48, 3));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_reconnect".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

/// 场景 13：听人说话（恒开门）。
/// 钉的是：**不降噪**、恒开门的 `GateState::Always`、服务端那几类消息转成的字幕 /
/// 译音 / 用量事件，以及译音进系统默认播放设备。
fn listen_48k_always_open() -> String {
    let rig = Rig::build(RATE_48);
    start_listen(&rig, |_| {});
    // 恒开门：静音也原样上传。
    rig.feed(quiet(BLOCK_48, 1));
    rig.push_message(speech_started(0));
    rig.push_message(text_delta("こんに"));
    rig.push_message(text_delta("ちは"));
    rig.push_message(text_delta("、"));
    rig.push_message(text_done("こんにちは。"));
    rig.push_message(audio_delta(&[16384, -16384]));
    rig.push_message(text_delta("你好"));
    rig.push_message(speech_stopped());
    rig.push_message(turn_done(30, 12));
    // 围栏：确保上面那些消息都被消化过了再收摊。
    rig.feed(quiet(BLOCK_48, 2));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "listen_48k_always_open".to_string()),
            ("pipeline", "listen".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "off".to_string()),
            ("gate", "level_0.0".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "default".to_string()),
        ],
        &play,
    )
}

/// 场景 14：听人说话收到热更新。
/// 钉的是：`hot_update` 为假时**一条 `session.update` 都不发**（除了启动握手那一条）。
fn listen_48k_hot_update_ignored() -> String {
    let rig = Rig::build(RATE_48);
    start_listen(&rig, |_| {});
    rig.feed(quiet(BLOCK_48, 1));
    hot_update(&rig, Some("en"), Some("Cherry"));
    rig.feed(quiet(BLOCK_48, 2));
    hot_update(&rig, Some("ja"), None);
    rig.feed(quiet(BLOCK_48, 3));
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "listen_48k_hot_update_ignored".to_string()),
            ("pipeline", "listen".to_string()),
            ("boot", "ledger".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "off".to_string()),
            ("gate", "level_0.0".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("output_device", "default".to_string()),
        ],
        &play,
    )
}

/// 场景 15：纯文字会话（起会话时就没有播放出口）。
/// 钉的是：清单里没有播放出口 → 播放汇一次都不开，服务端推来的译音无处可去。
///
/// 这一组走 `Rig::start`（直接派 `SessionConfig`）而不是账本：`Settings::normalize`
/// 强制 `speak.speak_translation = true`，所以"起会话时就没有音色"在账本那条路上
/// 摆不出来。代价是账本按会话号挡掉了全部回调，**`event` 一节是空的**——`meta` 里的
/// `boot direct` 就是提醒人这一点。
fn speak_48k_text_only() -> String {
    let rig = Rig::build(RATE_48);
    let mut config = speak_config();
    config.voice = None;
    config.gate_active = true;
    rig.start(config);
    rig.push_message(audio_delta(&[16384, -16384]));
    rig.feed(loud(BLOCK_48, 1));
    rig.wait_until(|| rig.wire.audio_frames() >= 1);
    let play = snapshot_play(&rig);
    rig.engine.shutdown();
    render(
        &rig,
        &[
            ("scenario", "speak_48k_text_only".to_string()),
            ("pipeline", "speak".to_string()),
            ("boot", "direct".to_string()),
            ("capture_rate", RATE_48.to_string()),
            ("block_samples", BLOCK_48.to_string()),
            ("denoise", "on".to_string()),
            ("gate", "manual".to_string()),
            ("gate_active_at_boot", "true".to_string()),
            ("translate", "true".to_string()),
            ("voice", "none".to_string()),
            ("output_device", "CABLE_Input".to_string()),
        ],
        &play,
    )
}

// --- 场景表与 16 条用例 -----------------------------------------------------

type Scenario = fn() -> String;

/// 15 组配置的登记表。`docs/plans/S4-B-OP-CHAIN.md` §6.4 的顺序原样保留。
const SCENARIOS: &[(&str, Scenario)] = &[
    ("speak_48k_denoise_manual", speak_48k_denoise_manual),
    ("speak_48k_denoise_off", speak_48k_denoise_off),
    ("speak_48k_gate_level", speak_48k_gate_level),
    ("speak_48k_gate_always_open", speak_48k_gate_always_open),
    ("speak_48k_passthrough", speak_48k_passthrough),
    ("speak_44k_denoise_skipped", speak_44k_denoise_skipped),
    (
        "speak_48k_denoise_factory_fails",
        speak_48k_denoise_factory_fails,
    ),
    ("speak_16k_capture", speak_16k_capture),
    ("speak_48k_hot_update", speak_48k_hot_update),
    ("speak_48k_gate_commands", speak_48k_gate_commands),
    (
        "speak_48k_translation_audio_toggle",
        speak_48k_translation_audio_toggle,
    ),
    ("speak_48k_reconnect", speak_48k_reconnect),
    ("listen_48k_always_open", listen_48k_always_open),
    (
        "listen_48k_hot_update_ignored",
        listen_48k_hot_update_ignored,
    ),
    ("speak_48k_text_only", speak_48k_text_only),
];

/// 跑一个场景。跑两遍结果必须一样——这是差分台自己对自己的一致性检查。
fn run_twice(id: &str) -> (String, String) {
    let scenario = SCENARIOS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, scenario)| *scenario)
        .unwrap_or_else(|| panic!("场景表里没有 {id}"));
    (scenario(), scenario())
}

#[test]
fn speak_48k_denoise_manual_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_denoise_manual");
    verify!("speak_48k_denoise_manual", first);
}

#[test]
fn speak_48k_denoise_off_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_denoise_off");
    verify!("speak_48k_denoise_off", first);
}

#[test]
fn speak_48k_gate_level_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_gate_level");
    verify!("speak_48k_gate_level", first);
}

#[test]
fn speak_48k_gate_always_open_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_gate_always_open");
    verify!("speak_48k_gate_always_open", first);
}

#[test]
fn speak_48k_passthrough_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_passthrough");
    verify!("speak_48k_passthrough", first);
}

#[test]
fn speak_44k_denoise_skipped_matches_its_golden() {
    let (first, _) = run_twice("speak_44k_denoise_skipped");
    verify!("speak_44k_denoise_skipped", first);
}

#[test]
fn speak_48k_denoise_factory_fails_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_denoise_factory_fails");
    verify!("speak_48k_denoise_factory_fails", first);
}

#[test]
fn speak_16k_capture_matches_its_golden() {
    let (first, _) = run_twice("speak_16k_capture");
    verify!("speak_16k_capture", first);
}

#[test]
fn speak_48k_hot_update_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_hot_update");
    verify!("speak_48k_hot_update", first);
}

#[test]
fn speak_48k_gate_commands_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_gate_commands");
    verify!("speak_48k_gate_commands", first);
}

#[test]
fn speak_48k_translation_audio_toggle_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_translation_audio_toggle");
    verify!("speak_48k_translation_audio_toggle", first);
}

#[test]
fn speak_48k_reconnect_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_reconnect");
    verify!("speak_48k_reconnect", first);
}

#[test]
fn listen_48k_always_open_matches_its_golden() {
    let (first, _) = run_twice("listen_48k_always_open");
    verify!("listen_48k_always_open", first);
}

#[test]
fn listen_48k_hot_update_ignored_matches_its_golden() {
    let (first, _) = run_twice("listen_48k_hot_update_ignored");
    verify!("listen_48k_hot_update_ignored", first);
}

#[test]
fn speak_48k_text_only_matches_its_golden() {
    let (first, _) = run_twice("speak_48k_text_only");
    verify!("speak_48k_text_only", first);
}

/// 15 组配置在同一个进程里连跑两遍，轨迹必须逐字相同。
///
/// 这是差分台自己的一致性闸：轨迹里混进了墙钟、线程 id、集合遍历顺序或任何别的
/// 非确定项，这一条会先红——比"金样偶尔对不上"好定位得多。
#[test]
fn goldens_are_deterministic_when_regenerated() {
    for (id, _) in SCENARIOS {
        let (first, second) = run_twice(id);
        assert_eq!(
            first, second,
            "场景 {id} 跑两遍轨迹不一样：轨迹里混进了非确定项（墙钟？集合顺序？）"
        );
    }
}
