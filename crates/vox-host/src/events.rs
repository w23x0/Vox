//! 事件往哪出。
//!
//! 芯的事件只有一个出口形状：**一个 `Event → ()` 的分派函数**。至于"外面"是谁——
//! Tauri 的前端通道、结构化日志（journal）、将来的 HTTP 订阅——那是各入口自己的实现，
//! 共享层只出 [`EventSink`] 这一个 trait。
//!
//! [`LogSink`] 是搬自无屏档的那一半（结构化日志）。它**不是**唯一的实现，也**不该**是
//! 唯一的：`Core::assemble` 不挂它——无屏入口在自己的 `run_daemon` 里**最早**挂（"流水线
//! 一起来就可能发事件，晚挂就漏掉启动那几步"）。桌面那一半（`FrontendSink`，包一个
//! `tauri::AppHandle`）留在 `app/src-tauri`，**这是 `vox-host` 之外唯一新增的 trait 实现**——
//! 编译期就保证共享层不碰 Tauri。
//!
//! 两条纪律（跟着 `log_event` 一起搬过来）：
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

/// 事件出口。**只有一个方法**：把一条芯事件送到"外面去"。
///
/// 为什么是 `&Event` 而不是 `Event`：Tauri 的 `Emitter::emit` 要 owned，实现自己 clone；
/// 签名收 `&Event` 让"只读"出口（[`LogSink`]）零分配。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &vox_core::event::Event);
}

/// 结构化日志出口。把芯的事件翻成 tracing（字段是结构化的，systemd 收进 journal）。
pub struct LogSink;

impl EventSink for LogSink {
    fn emit(&self, event: &Event) {
        log_event(event);
    }
}

impl LogSink {
    /// 装在流水线起来**之前**（不然错过启动那几步）。
    pub fn attach(runtime: &Runtime) {
        let sink = Arc::new(LogSink);
        let listener: Listener = Arc::new(move |event: &Event| sink.emit(event));
        runtime.add_listener(listener);
    }
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
