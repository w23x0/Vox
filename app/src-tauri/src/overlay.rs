//! 纯显示悬浮字幕窗的装配：spawn 悬浮窗 + 字幕帧线程。
//!
//! 窗口自带线程和消息泵，主线程只属于 Tauri 事件循环。字幕帧线程按约 30fps
//! 计算淡出；有内容时持续提交以完成换行上移动画，空内容或隐藏时退化到低频轮询。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vox_core::capability::{CapabilityStatus, UnavailableReason};
use vox_core::ports::SubtitleFrame;

use crate::state::{AppState, OverlayHandle};

/// 有内容且淡出可能变化时的帧间隔（约 30fps）。
const FRAME_INTERVAL_ACTIVE: Duration = Duration::from_millis(33);
/// 连续空帧超过此阈值后切入省电模式。
const IDLE_THRESHOLD: u32 = 10;
/// 空内容或悬浮窗隐藏时的轮询间隔。
const FRAME_INTERVAL_IDLE: Duration = Duration::from_millis(200);

/// 帧线程停止旗。进程退出时调 `stop()` 来置位。
static STOP: AtomicBool = AtomicBool::new(false);

/// 字幕帧线程句柄，`stop()` 里 join。
static FRAME_THREAD: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>> =
    parking_lot::Mutex::new(None);

/// 让帧线程收工并等它退出（最多一个空闲轮询周期）。
pub fn stop() {
    STOP.store(true, Ordering::Relaxed);
    if let Some(h) = FRAME_THREAD.lock().take() {
        let _ = h.join();
    }
}

/// `captions` 这一位的定义者（§2.5.4）：**窗口起来了 × 字幕帧线程起来了**才算 `ON`。
///
/// 抽成纯函数（两件证据 → 位）是为了**能被单测钉住**：这一段就是"判定"本身，
/// 单测直接喂四种组合即可；`start` 里的两段 IO 只负责产出这两件证据，不做任何判断。
/// 第七轮复核的变异（把定义者改成恒 `ON`）当时全绿，根因就是判定散在 `start` 的函数体里、
/// 没有任何一条单测看得到它。
///
/// - 窗口没起来：`reason` 原样带走（Windows `unsupported` / Linux `not_wired`）；
/// - 窗口在、帧线程没起来：只有窗口没人往里推帧，画面上永远是空的——那种"能画"是假的，
///   报 `not_wired`（实现是有的，是装配层这段路没接上）。
pub(crate) fn captions_outcome(
    window: Result<(), UnavailableReason>,
    frame_thread_up: bool,
) -> CapabilityStatus {
    match (window, frame_thread_up) {
        (Err(reason), _) => CapabilityStatus::off(reason),
        (Ok(()), true) => CapabilityStatus::ON,
        (Ok(()), false) => CapabilityStatus::off(UnavailableReason::NotWired),
    }
}

/// 起悬浮窗 + 字幕帧线程。失败不致命——设置窗照样能用。
///
/// 返回值就是 `captions` 这一位的定义者（§2.5.4）：**窗口起来了、字幕帧线程也起来了**
/// 才算 `ON`——只有窗口而没人往里推帧，画面上永远是空的，那种"能画"是假的。
/// 判定本身在 [`captions_outcome`] 里（纯函数，可单测）；装配层把返回值记进观测槽
/// （`platform::record_captions`），`host_facts()` 读它。
pub fn start(state: &Arc<AppState>) -> CapabilityStatus {
    let settings = state.runtime.settings();
    let rt = state.runtime.clone();
    let geometry_rt = rt.clone();
    let geometry_callback: crate::platform::GeometryCallback =
        std::sync::Arc::new(move |geometry| {
            // 几何回调发生在悬浮窗自己的线程；这里只更新 Runtime，持久化和
            // 其他监听器仍沿用设置事件的现有路径，不跨线程直接碰 Tauri 状态。
            geometry_rt.update_settings(|s| s.subtitle.geometry = Some(geometry));
        });

    let overlay = match crate::platform::spawn_overlay(&settings.subtitle, geometry_callback) {
        Ok(o) => o,
        Err(failure) => {
            state
                .runtime
                .notify(vox_core::event::Notice::warning(format!(
                    "悬浮字幕窗未能启动：{}",
                    failure.error
                )));
            // 没窗就没有帧线程可言：位只能跟着建窗失败的原因走。
            return captions_outcome(Err(failure.reason), false);
        }
    };

    let _ = state.overlay.set(Arc::clone(&overlay));

    let frame_rt = rt.clone();
    let notify_rt = rt.clone();
    let frame_thread = std::thread::Builder::new()
        .name("vox-subtitle".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                subtitle_loop(frame_rt, overlay)
            }));
            if result.is_err() {
                notify_rt.notify(vox_core::event::Notice::error(
                    "字幕刷新线程异常退出，悬浮窗内容不再更新。重启应用可恢复。".to_string(),
                ));
            }
        })
        .ok();
    let frame_thread_up = frame_thread.is_some();
    if !frame_thread_up {
        state.runtime.notify(vox_core::event::Notice::warning(
            "字幕刷新线程未能启动，悬浮窗不会更新内容".to_string(),
        ));
    }
    *FRAME_THREAD.lock() = frame_thread;

    captions_outcome(Ok(()), frame_thread_up)
}

/// 字幕帧循环。有内容时每帧提交，保证渲染器的平滑上移不会停在半路；
/// 空帧仍按内容变化去重。
fn subtitle_loop(rt: vox_core::Runtime, overlay: OverlayHandle) {
    let mut consecutive_empty: u32 = 0;
    let mut prev_visible: Option<bool> = None;
    let mut previous_frame: Option<SubtitleFrame> = None;

    loop {
        // 窗口自己关了（Windows：用户拖没了窗口 / 窗口线程退出）就收摊。
        if STOP.load(Ordering::Relaxed) || !crate::platform::overlay_running() {
            break;
        }

        let visible = rt.subtitle_visible();
        if prev_visible != Some(visible) {
            if visible {
                overlay.show();
            } else {
                overlay.hide();
                // 重新显示时必须强制推一帧，不能沿用隐藏前的去重基线。
                previous_frame = None;
            }
            prev_visible = Some(visible);
        }

        if !visible {
            std::thread::sleep(FRAME_INTERVAL_IDLE);
            continue;
        }

        rt.prune_subtitles();
        let frame = rt.subtitle_frame();
        let has_content = !frame.lines.is_empty();

        if frame_changed(previous_frame.as_ref(), &frame) || has_content {
            overlay.render(frame.clone());
            previous_frame = Some(frame);
        }

        if has_content {
            consecutive_empty = 0;
        } else {
            consecutive_empty = consecutive_empty.saturating_add(1);
        }
        std::thread::sleep(sleep_duration(consecutive_empty));
    }
}

fn frame_changed(previous: Option<&SubtitleFrame>, current: &SubtitleFrame) -> bool {
    previous != Some(current)
}

fn sleep_duration(consecutive_empty: u32) -> Duration {
    if consecutive_empty > IDLE_THRESHOLD {
        FRAME_INTERVAL_IDLE
    } else {
        FRAME_INTERVAL_ACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::ports::SubtitleLine;
    use vox_core::subtitle::{RenderedChar, Track};

    fn frame(alpha: f32) -> SubtitleFrame {
        SubtitleFrame {
            lines: vec![SubtitleLine {
                track: Track::Listen,
                origin: 0,
                chars: vec![RenderedChar { ch: '字', alpha }],
                color: "#eef6ff".into(),
            }],
        }
    }

    #[test]
    fn first_frame_is_rendered() {
        assert!(frame_changed(None, &frame(1.0)));
    }

    #[test]
    fn identical_frame_is_skipped() {
        let old = frame(1.0);
        assert!(!frame_changed(Some(&old), &old));
    }

    #[test]
    fn alpha_change_is_rendered() {
        let old = frame(1.0);
        assert!(frame_changed(Some(&old), &frame(0.8)));
    }

    #[test]
    fn sleep_duration_active_when_below_threshold() {
        assert_eq!(sleep_duration(0), FRAME_INTERVAL_ACTIVE);
        assert_eq!(sleep_duration(IDLE_THRESHOLD), FRAME_INTERVAL_ACTIVE);
    }

    #[test]
    fn sleep_duration_idle_when_above_threshold() {
        assert_eq!(sleep_duration(IDLE_THRESHOLD + 1), FRAME_INTERVAL_IDLE);
        assert_eq!(sleep_duration(1000), FRAME_INTERVAL_IDLE);
    }

    /// `captions` 的定义者判据：**窗口起来了 × 帧线程起来了**才是 `ON`（§2.5.4）。
    ///
    /// 这条钉的是"判定"本身（第七轮 D3：那时判定散在 `start` 的函数体里，把定义者改成
    /// 恒 `ON` 单测全绿）。**自证**：把 [`captions_outcome`] 改成恒 `ON`（或恒 `busy`），
    /// 这条变红，`platform::tests::captions_bit_follows_the_overlay_start_result` 也跟着红。
    #[test]
    fn captions_definer_needs_both_the_window_and_the_frame_thread() {
        assert_eq!(captions_outcome(Ok(()), true), CapabilityStatus::ON);
        // 只有窗口没人推帧：画面上永远是空的，这种"能画"是假的。
        assert_eq!(
            captions_outcome(Ok(()), false),
            CapabilityStatus::off(UnavailableReason::NotWired)
        );
        // 建窗失败：reason 原样带走，不许被自己的判据改写。
        assert_eq!(
            captions_outcome(Err(UnavailableReason::Unsupported), true),
            CapabilityStatus::off(UnavailableReason::Unsupported)
        );
        assert_eq!(
            captions_outcome(Err(UnavailableReason::NotWired), false),
            CapabilityStatus::off(UnavailableReason::NotWired)
        );
    }
}
