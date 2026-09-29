//! 状态往哪出（EMBEDDED §3.5-②）。
//!
//! 无屏设备没有屏幕，所以"现在到底在不在跑"只能从两处看，本文件就是这两处：
//!
//! 1. **两条状态出口**：[`capabilities_json`] —— `CapabilityReport` 的 JSON（`--print-capabilities`
//!    与 `--dry-run` 打的就是它）；[`composition_json`] —— **两份清单 + 有效位**（`--print-composition`
//!    打的就是它，S0 §4.3-A）。这是**机器读**的那一面：谁想知道"这台盒子能做什么、两条腿会怎么装"，
//!    读它们，不用问界面（也没有界面）。
//! 2. **结构化日志**：[`wire`] —— 把芯的事件翻成 tracing（字段是结构化的，systemd 收进
//!    journal）。这是**人读**的那一面：流水线阶段、连不上云端、还差个密钥，都在这里。
//!
//! 两条纪律：
//!
//! - **字幕文本不进日志**。`SubtitleDelta` / `SourceDetected` 这一类的正文是**用户说的话**，
//!   不该在磁盘上再留一份（桌面档 `sys/log.rs` 头注释同一条理由；journal 是磁盘）。
//!   字幕的正经出口是 S1 的资源面（`resources` + 订阅），不是日志——所以这里只记
//!   "来了一段字幕"这件事本身。
//! - **高频事件不记**：`GateStatus`（音频块级）与 `LatencyChanged` 每 500 ms 一次，
//!   进日志只会把有用的那些冲掉。

use std::sync::Arc;

use vox_core::event::{Event, Notice, Severity};
use vox_core::runtime::{Listener, Runtime};
use vox_core::subtitle::Track;
use vox_mcp::Ledger;

/// 能力位报告的 JSON（缩进过，方便人直接看；`jq -c` 一行照样能用）。
pub fn capabilities_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&runtime.capabilities())
}

/// 清单的 JSON：**两条腿 + 当前有效的能力位**（S0 §4.3-A，`--print-composition` 打的就是它）。
///
/// 形状（键名就是 S0 §4.3-A 里那几个，缩进过——`jq -c` 一行照样能用）：
///
/// ```text
/// {
///   "capabilities": <CapabilityReport>,   // 当前**有效**位（档位上限 − 关掉的）
///   "speak":  <Composition> | null,       // 这条腿现在派出来的清单
///   "listen": <Composition> | null,       //   派不出来就是 null，理由进 errors
///   "errors": [ { endpoint, code, message, detail? } ]
/// }
/// ```
///
/// **两条腿都打**：位为假的格子在这一步就已经被芯关掉了（`Composition::of` 的开门/关门），
/// 所以无屏档打出来的"听人说话"里 `in` 是空的（`program_tap` 在这一档的上限之外）——
/// 那正是"位是事实"的证据，不是打错了。清单**派不出来**（比如还没选要抓的程序）时那个键是
/// `null`、理由进 `errors`：打一份看起来像清单的假清单比打 `null` 糟得多。
///
/// 派生走 `vox_mcp::endpoints::manifest`——**与 S1 的 `describe_endpoint` 同一个函数**，
/// 所以这份打印与 Agent 面看到的是同一份清单（S0 §4.3-A：它同时是 describe 出口的原型）。
/// 组装（遍历两条腿、拼 `errors`、缩进 JSON）走 `vox_mcp::endpoints::document`——桌面档
/// （`app/src-tauri/src/composition.rs`）用的是**同一个函数**：两边打出来的**形状**（键名、嵌套、
/// 键顺序）逐字相同，**取值随档位与宿主事实本就不同**（位上限、`host`、设备名都该不一样——
/// 同形说的是骨架，不是内容；把取值也读成"逐字相同"就会把两档该有的差别当成 bug）。两个外壳
/// 各留一份组装的坏处正在于同一个形状要有两处人守着。清单的线上形态也由它走
/// `vox_mcp::endpoints::wire`（文本往返一趟），与 S1 报给客户端的那一份逐字同形。
///
/// 失败只有一种出口：`serde_json::Error`（清单本身派不出来是**领域失败**，由 `document`
/// 写进 `errors`，不是这里的错误）。
pub fn composition_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    let ledger: &dyn Ledger = runtime;
    vox_mcp::endpoints::document(ledger, &mut |endpoint, error| {
        tracing::warn!(endpoint = %endpoint.as_str(), reason = %error.message, "这条腿现在派不出清单");
    })
}

/// 把芯的事件接到日志上。装在流水线起来**之前**（不然错过启动那几步）。
pub fn wire(runtime: &Runtime) {
    let listener: Listener = Arc::new(log_event);
    runtime.add_listener(listener);
}

/// 一个事件 → 一行日志（或不记）。
fn log_event(event: &Event) {
    match event {
        Event::PipelineState { pipeline, state } => {
            tracing::info!(
                pipeline = pipeline.label(),
                state = state.label(),
                "流水线阶段变了"
            );
        }
        Event::Notice { notice } => log_notice(notice),
        Event::MicActive { active } => {
            tracing::info!(active, "麦克风开关变了（无屏档没有热键，只有控制面能改它）");
        }
        Event::DevicesChanged => tracing::debug!("设备列表变了"),
        Event::SettingsChanged { .. } => tracing::debug!("设置变了"),
        Event::UsageChanged { .. } => tracing::debug!("用量涨了"),
        // 下面三个带**说话内容**：只记"来了一段字幕"这个事实，正文一行都不进日志。
        Event::SubtitleDelta { track, done, .. } => {
            tracing::debug!(
                track = track_name(*track),
                done,
                "来了一段字幕（正文不进日志）"
            );
        }
        Event::SourceDetected { .. } => {
            tracing::debug!("识别到源语言（内容不进日志）");
        }
        Event::SubtitleCleared { track } => {
            tracing::debug!(track = track_name(*track), "字幕轨清了");
        }
        // 高频：闸门状态每个音频块、延迟每 500 ms 一次，进日志只会把有用的冲掉。
        Event::GateStatus { .. } | Event::LatencyChanged { .. } => {}
    }
}

fn log_notice(notice: &Notice) {
    // 提示是给人看的中文句子（芯里不存文案，这些是外壳/芯自己拼的行动指引）。
    let pipeline = notice
        .pipeline
        .map(|pipeline| pipeline.label())
        .unwrap_or("-");
    match notice.severity {
        Severity::Error => tracing::error!(pipeline, "{}", notice.text),
        Severity::Warning => tracing::warn!(pipeline, "{}", notice.text),
        Severity::Info => tracing::info!(pipeline, "{}", notice.text),
    }
}

fn track_name(track: Track) -> &'static str {
    match track {
        Track::Speak => "speak",
        Track::Listen => "listen",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use vox_core::event::Pipeline as PipelineEvent;
    use vox_core::usage::UsageLedger;
    use vox_core::Settings;

    fn runtime() -> Runtime {
        let runtime = Runtime::new(Settings::default(), crate::sys::clock::local());
        runtime.set_host_facts(crate::platform::host_facts());
        runtime
    }

    /// 状态出口的形状：这份 JSON 就是验收里要贴的那一份，形状不许漂。
    #[test]
    fn the_status_exit_reports_the_headless_tier() {
        let json = capabilities_json(&runtime()).expect("能力报告该能序列化");
        let report: serde_json::Value = serde_json::from_str(&json).expect("合法 JSON");

        assert_eq!(report["tier"], "linux_headless");
        assert_eq!(report["host"]["mic"]["enabled"], true);
        assert_eq!(report["host"]["background_service"]["enabled"], false);
        assert_eq!(report["host"]["background_service"]["reason"], "not_wired");
        // 无屏档做不到的那几位：照实说"这台设备做不到"。
        assert_eq!(report["host"]["captions"]["reason"], "unsupported");
        assert_eq!(report["host"]["tray"]["reason"], "unsupported");
        assert_eq!(report["host"]["program_tap"]["reason"], "unsupported");
        // provider 位两张表都在（speak / listen 各按当前选的服务商解析）。
        assert!(report["speak"].is_object() && report["listen"].is_object());
    }

    /// `--print-composition`（S0 §4.3-A）打的那一份：两条腿 + 有效位。
    ///
    /// 这份 JSON 就是验收里要贴的那一份，键名与形状都不许漂：`.speak.session.provider`、
    /// `.speak.ops[].kind`、`.capabilities.tier` 这几条正是 S0 §4.3-A 给的读法。
    #[test]
    fn the_composition_exit_prints_both_legs_and_the_bits() {
        let json = composition_json(&runtime()).expect("清单该能序列化");
        let document: serde_json::Value = serde_json::from_str(&json).expect("合法 JSON");

        assert_eq!(document["capabilities"]["tier"], "linux_headless");
        // 对外说话：麦克风进、四节算子、译文出声。这一档没有 `virtual_mic` 位，
        // 所以角色退成 `speaker`（"位为假 → 那一格退档"，不是删功能）。
        assert_eq!(document["speak"]["in"][0]["kind"], "mic");
        assert_eq!(document["speak"]["out"][0]["role"], "speaker");
        assert_eq!(document["speak"]["session"]["provider"], "aliyun");
        assert_eq!(
            op_kinds(&document["speak"]),
            ["mono", "denoise", "gate", "resample"]
        );

        // 缺省没有要抓的程序：这条腿现在**派不出来** → `null` + 一句为什么。
        // （打一份看起来像清单的假清单比打 null 糟得多。）
        assert!(document["listen"].is_null(), "{document}");
        let errors = document["errors"].as_array().expect("errors 是数组");
        assert_eq!(errors.len(), 1, "{document}");
        assert_eq!(errors[0]["endpoint"], "listen");
        assert_eq!(errors[0]["code"], "endpoint_unavailable");
        assert!(errors[0]["message"].is_string(), "{document}");
    }

    /// 选了目标程序之后"听人说话"也打得出来，而且**如实**是空 `in`：`program_tap`
    /// 在无屏档的档位上限之外，那一格进不了清单（S0 §2.3.3：门关掉的是那一格，不是这条腿）。
    #[test]
    fn the_listen_leg_prints_with_an_empty_input_and_no_denoise() {
        let settings = Settings::from_json(
            r#"{"listen":{"target":{"executable":"Discord","display_name":"Discord"}}}"#,
        );
        let runtime = Runtime::new(settings, crate::sys::clock::local());
        runtime.set_host_facts(crate::platform::host_facts());

        let json = composition_json(&runtime).expect("清单该能序列化");
        let document: serde_json::Value = serde_json::from_str(&json).expect("合法 JSON");

        assert_eq!(document["listen"]["in"].as_array().map(Vec::len), Some(0));
        // 环回的数字源本来就干净：这条腿不装降噪（与 Speak 的差别就在这儿）。
        assert_eq!(op_kinds(&document["listen"]), ["mono", "gate", "resample"]);
        assert_eq!(document["listen"]["session"]["provider"], "aliyun");
        // 两条腿都派得出来 → 没有可报的错误。
        assert_eq!(document["errors"].as_array().map(Vec::len), Some(0));
    }

    /// 一份清单里的算子名字，按顺序取——S0 §4.3-A 的 `.listen.ops[].kind` 就是它。
    fn op_kinds(composition: &serde_json::Value) -> Vec<String> {
        composition["ops"]
            .as_array()
            .expect("ops 是数组")
            .iter()
            .map(|op| op["kind"].as_str().expect("每节都有 kind").to_string())
            .collect()
    }

    /// 日志里**不许出现说话内容**——这是本文件最要紧的一条契约，用真的 subscriber 抓一遍。
    #[test]
    fn subtitle_text_never_reaches_the_log() {
        const SECRET: &str = "这句话不该出现在日志里";
        let log = std::sync::Arc::new(StdMutex::new(Vec::<u8>::new()));

        // 抓一遍两条事件：一条带正文（字幕）、一条带提示（给用户看的中文句子）。
        let captured = {
            let sink = StdArc::clone(&log);
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || Sink(StdArc::clone(&sink)))
                .with_ansi(false)
                // 缺省的 `fmt()` 只到 INFO，而"来了一段字幕"是 debug 级
                // （出厂缺省过滤器也不打它，理由见模块头：高频/含文本的那一类）。
                .with_max_level(tracing::Level::DEBUG)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                log_event(&Event::SubtitleDelta {
                    track: Track::Speak,
                    text: SECRET.to_string(),
                    done: true,
                    replace: false,
                    confirmed: Some(SECRET.to_string()),
                });
                log_event(&Event::Notice {
                    notice: Notice::error("请先配置 API 密钥").on(PipelineEvent::Speak),
                });
            });
            String::from_utf8(log.lock().expect("日志锁").clone()).expect("UTF-8")
        };

        assert!(
            captured.contains("来了一段字幕"),
            "该记的事实要记：{captured}"
        );
        assert!(
            captured.contains("请先配置 API 密钥"),
            "提示要进日志：{captured}"
        );
        assert!(!captured.contains(SECRET), "说话内容进了日志：{captured}");
    }

    /// `wire` 把监听器挂上了：事件会走到日志（不发事件时它什么都不做）。
    #[test]
    fn wiring_is_idempotent() {
        let runtime = Runtime::new(Settings::default(), crate::sys::clock::local());
        wire(&runtime);
        wire(&runtime);
        runtime.notify(Notice::info("装配好了"));
        // 用量事件也走同一条路（这里只要求"不 panic"）。
        runtime.record_usage(
            "qwen3.5-livetranslate-flash-realtime",
            &vox_core::usage::TurnUsage {
                input_tokens: 1,
                output_tokens: 1,
                total_tokens: 2,
            },
        );
        let _ = runtime.snapshot();
        let _: UsageLedger = runtime.usage();
    }

    /// 给 tracing 用的落点：往一个共享 `Vec<u8>` 写。
    struct Sink(StdArc<StdMutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("日志锁").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
