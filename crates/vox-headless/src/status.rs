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
//! S4-A 之后这三个出口的**实现**都在共享宿主层 `vox_host`（`report::{capabilities_json,
//! composition_json}` 与 `events::LogSink`），桌面档走的是**同一份**——本文件只剩"这一档
//! 在哪儿调用它"。留下的是**入口的调用点**（`--print-capabilities` / `--dry-run` /
//! `--print-composition` 打的就是这三个），不是第二份实现。
//!
//! 两条纪律跟着实现搬进了 `vox_host::events`（那里是它们的家，头注释里逐条写着）：
//!
//! - **字幕文本不进日志**。`SubtitleDelta` / `SourceDetected` 这一类的正文是**用户说的话**，
//!   不该在磁盘上再留一份（桌面档 `sys/log.rs` 头注释同一条理由；journal 是磁盘）。
//! - **高频事件不记**：`GateStatus`（音频块级）与 `LatencyChanged` 每 500 ms 一次，
//!   进日志只会把有用的那些冲掉。

use vox_core::runtime::Runtime;

/// 能力位报告的 JSON（缩进过，方便人直接看；`jq -c` 一行照样能用）。
///
/// 形状（键名与嵌套）见 `vox_host::report::capabilities_json` 的定义；桌面档打的是同一份
/// （`vox_host` 共享层），所以"这台机器能做什么"只有一处组装。
pub fn capabilities_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    vox_host::report::capabilities_json(runtime)
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
/// 实现（组装、拼 `errors`、缩进 JSON）全在 `vox_host::report::composition_json`——**桌面档
/// （`app/src-tauri/src/composition.rs`）转发的是同一个函数**。两个外壳各留一份组装的坏处
/// 正在于同一个形状要有两处人守着；`vox-host` 收掉它之后只剩两处一行转发。
///
/// 失败只有一种出口：`serde_json::Error`（清单本身派不出来是**领域失败**，由共享层
/// 写进 `errors`，不是这里的错误）。
pub fn composition_json(runtime: &Runtime) -> Result<String, serde_json::Error> {
    vox_host::report::composition_json(runtime)
}

/// 把芯的事件接到日志上。装在流水线起来**之前**（不然错过启动那几步）。
///
/// 实现是 `vox_host::events::LogSink`——共享层里那一个结构化日志出口（桌面档那一半是
/// 前端通道 `FrontendSink`，不是它）。两条纪律（字幕正文不进日志、高频事件不记）在
/// `vox_host::events` 的头注释里。
pub fn wire(runtime: &Runtime) {
    vox_host::LogSink::attach(runtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    // S4-A：字幕正文不进日志那条用例（`subtitle_text_never_reaches_the_log`）连同它那个
    // 给 tracing 用的 `Sink` 落点一起搬进了 `vox_host::events`（实现搬了，实现的契约就得
    // 跟着搬）。余下 4 条断言的是**无屏档的事实**（档位、两条腿的形状、`wire` 挂得上），
    // 留在这里，函数体一个字没动。
    use vox_core::event::Notice;
    use vox_core::usage::UsageLedger;
    use vox_core::Settings;

    fn runtime() -> Runtime {
        let runtime = Runtime::new(Settings::default(), vox_host::clock::local_clock());
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
        let runtime = Runtime::new(settings, vox_host::clock::local_clock());
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

    /// `wire` 把监听器挂上了：事件会走到日志（不发事件时它什么都不做）。
    #[test]
    fn wiring_is_idempotent() {
        let runtime = Runtime::new(Settings::default(), vox_host::clock::local_clock());
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
}
