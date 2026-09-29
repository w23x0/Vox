//! Win32 原生悬浮字幕窗。**不用 WebView2**——它的透明会在字幕后面留一个实心方块。
//!
//! 实现 `vox_core::ports::SubtitleView`。
//!
//! 形状：有字幕时可拖动/缩放、空帧时自动鼠标穿透的分层窗（`WS_EX_LAYERED`），通过
//! `UpdateLayeredWindow` 走 per-pixel alpha。像素在 CPU 上用 GDI 逐字栅格化后手工
//! 合成。窗口活在自己的线程上；外面只往“最新状态邮箱”写数据再 `PostMessage` 叫醒，
//! 任何线程都不直接操作 HWND。
//!
// 悬浮窗只负责显示和几何操作：没有悬停面板、按钮、状态和 token 计数。所有设置与
// 流水线操作都留在应用主界面。

// 非 Windows 上编译成空 lib，理由同 `vox-audio-win`：Linux 那边是 `vox-overlay-linux`。
#![cfg(windows)]

// 渲染那半边（几何、配色、画布、布局、帧合成）住在平台中立的 `vox-overlay-core` 里，
// 这里转出去，调用方照旧写 `vox_overlay_win::render::Renderer` / `canvas::Canvas`。
// `text` 也是 core 的（字形数据 + `GlyphSource` trait）；GDI 光栅器在本 crate 的 `font`。
pub use vox_overlay_core::{canvas, color, geom, layout, render, text};

mod font;
mod surface;
mod window;

pub use font::FontRaster;
pub use window::GeometryCallback;

/// 平台光栅器工厂：渲染器只认 trait，这里给它 GDI 那版。
pub fn font_factory() -> text::FontFactory {
    vox_overlay_core::text::font_factory(|family, size, dpi| {
        Ok(Box::new(FontRaster::new(family, size, dpi)?))
    })
}

use std::sync::atomic::Ordering;
use std::sync::Arc;

use vox_core::ports::{PortError, PortResult, SubtitleFrame, SubtitleView};
use vox_core::settings::SubtitleSettings;

/// 窗口类名。带 crate 前缀，避免跟宿主进程里别的窗口类撞。
const WINDOW_CLASS: &str = "VoxSubtitleOverlay";

/// 悬浮窗把手。
pub struct Overlay {
    shared: Arc<window::Shared>,
    /// 窗口线程的 join 句柄，`shutdown` 时用。
    thread: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 防止未来从窗口线程误调 `shutdown()` 时 join 自己。
    thread_id: std::sync::OnceLock<std::thread::ThreadId>,
}

impl Overlay {
    /// 起一个纯显示悬浮窗。窗口建好（或者建失败）之后才返回。
    pub fn spawn(initial: &SubtitleSettings) -> PortResult<Arc<Self>> {
        Self::spawn_with_geometry(initial, None)
    }

    /// 起一个悬浮窗，并在用户拖动/缩放结束时回调最新几何。
    pub fn spawn_with_geometry(
        initial: &SubtitleSettings,
        geometry_callback: Option<GeometryCallback>,
    ) -> PortResult<Arc<Self>> {
        let settings = sanitize(initial);
        let shared = Arc::new(window::Shared::new());
        let overlay = Arc::new(Self {
            shared: Arc::clone(&shared),
            thread: parking_lot::Mutex::new(None),
            thread_id: std::sync::OnceLock::new(),
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread_callback = geometry_callback;
        let visible = settings.visible;
        let handle = std::thread::Builder::new()
            .name("vox-overlay".into())
            .spawn(move || window::run(thread_shared, settings, thread_callback, tx))
            .map_err(|e| PortError::new(format!("启动悬浮窗线程失败: {e}")))?;

        let startup = match rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(PortError::new("悬浮窗线程在建窗前退出")),
        };
        if let Err(e) = startup {
            let _ = handle.join();
            return Err(e);
        }
        let _ = overlay.thread_id.set(handle.thread().id());
        *overlay.thread.lock() = Some(handle);

        if visible {
            overlay.show();
        }
        Ok(overlay)
    }

    /// 关掉窗口并等线程结束。重复调用是安全的。
    pub fn shutdown(&self) {
        {
            let mut mail = self.shared.mailbox.lock();
            if mail.shutdown {
                return;
            }
            mail.shutdown = true;
        }
        self.shared.wake();

        let handle = self.thread.lock().take();
        if let Some(handle) = handle {
            if self.thread_id.get() == Some(&std::thread::current().id()) {
                // 丢弃 JoinHandle 即分离线程；不能 `forget`，否则会泄漏系统句柄。
                drop(handle);
            } else {
                let _ = handle.join();
            }
        }
    }

    /// 窗口线程还在跑。
    pub fn is_running(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }
}

impl SubtitleView for Overlay {
    fn show(&self) {
        self.shared.mailbox.lock().visible = Some(true);
        self.shared.wake();
    }

    fn hide(&self) {
        self.shared.mailbox.lock().visible = Some(false);
        self.shared.wake();
    }

    fn render(&self, frame: SubtitleFrame) {
        // 后写覆盖先写：排队只会让窗口画已经过期的帧。
        self.shared.mailbox.lock().frame = Some(frame);
        self.shared.wake();
    }

    fn restyle(&self, settings: &SubtitleSettings) {
        let s = sanitize(settings);
        let visible = s.visible;
        {
            let mut mail = self.shared.mailbox.lock();
            mail.settings = Some(s);
            mail.visible = Some(visible);
        }
        self.shared.wake();
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 把设置夹到本 crate 能安全渲染的范围。
fn sanitize(settings: &SubtitleSettings) -> SubtitleSettings {
    let mut s = settings.clone();
    s.font_size = s.font_size.clamp(
        vox_core::settings::FONT_SIZE_RANGE.0,
        vox_core::settings::FONT_SIZE_RANGE.1,
    );
    if let Some(g) = &mut s.geometry {
        // 尺寸上下限跟窗口线程那侧同一组常量（`window::clamp_geometry`），别在这边另写一份数字。
        (g.width, g.height) = window::clamp_geometry(g.width, g.height);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::ports::SubtitleLine;
    use vox_core::subtitle::{RenderedChar, Track};

    /// 需要真实桌面：`cargo test -p vox-overlay-win -- --ignored`
    #[test]
    #[ignore = "需要真实桌面"]
    fn spawn_render_and_shutdown() {
        let settings = SubtitleSettings::default();
        let overlay = Overlay::spawn(&settings).unwrap();
        assert!(overlay.is_running());
        overlay.render(SubtitleFrame {
            lines: vec![SubtitleLine {
                track: Track::Listen,
                chars: "悬浮字幕测试"
                    .chars()
                    .map(|ch| RenderedChar { ch, alpha: 1.0 })
                    .collect(),
                color: "#eef6ff".into(),
            }],
        });
        overlay.show();
        std::thread::sleep(std::time::Duration::from_millis(300));
        overlay.hide();
        overlay.shutdown();
        assert!(!overlay.is_running());
        overlay.shutdown();
    }

    #[test]
    #[ignore = "需要真实桌面"]
    fn render_from_many_threads_is_safe() {
        let settings = SubtitleSettings::default();
        let overlay = Overlay::spawn(&settings).unwrap();
        overlay.show();
        let mut handles = Vec::new();
        for worker in 0..4 {
            let o = Arc::clone(&overlay);
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    o.render(SubtitleFrame {
                        lines: vec![SubtitleLine {
                            track: Track::Speak,
                            chars: format!("{worker}-{i}")
                                .chars()
                                .map(|ch| RenderedChar { ch, alpha: 1.0 })
                                .collect(),
                            color: "#fff4de".into(),
                        }],
                    });
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(overlay.is_running(), "高频跨线程 render 不该把窗口线程搞挂");
        overlay.shutdown();
    }
}

#[cfg(test)]
mod gdi_render_tests {
    //! 需要真实 GDI 的渲染测试：核心渲染逻辑在 `vox-overlay-core` 里，那边没有
    //! 真字体可用，所以这两个"画一帧、查像素"的测试留在有 GDI 的这一侧。

    use super::*;
    use vox_core::ports::{SubtitleFrame, SubtitleLine};
    use vox_core::settings::SubtitleSettings;
    use vox_core::subtitle::{RenderedChar, Track};
    use vox_overlay_core::canvas::Canvas;
    use vox_overlay_core::render::{FrameInput, Renderer};

    fn line(track: Track, text: &str) -> SubtitleLine {
        SubtitleLine {
            track,
            chars: text
                .chars()
                .map(|ch| RenderedChar { ch, alpha: 1.0 })
                .collect(),
            color: "#ffffff".into(),
        }
    }

    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn draws_two_rows_with_no_invalid_pixels() {
        let settings = SubtitleSettings::default();
        let mut renderer = Renderer::new(&settings, 96, font_factory()).unwrap();
        let frame = SubtitleFrame {
            lines: vec![
                line(Track::Listen, "听人说话"),
                line(Track::Speak, "对外说话"),
            ],
        };
        let mut canvas = Canvas::new(0, 0);
        let out = renderer.draw(
            &mut canvas,
            &FrameInput {
                frame: &frame,
                settings: &settings,
                client_width: 880,
                client_height: 170,
                dpi: 96,
            },
        );
        assert_eq!(canvas.height(), out.client_height);
        assert!(out.client_height > 1);
        assert!(canvas.find_invalid_pixel().is_none());
    }

    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn zero_alpha_chars_leave_no_text_pixels() {
        let settings = SubtitleSettings {
            background_alpha: 0,
            ..SubtitleSettings::default()
        };
        let mut renderer = Renderer::new(&settings, 96, font_factory()).unwrap();
        let frame = SubtitleFrame {
            lines: vec![SubtitleLine {
                track: Track::Listen,
                chars: "完全透明"
                    .chars()
                    .map(|ch| RenderedChar { ch, alpha: 0.0 })
                    .collect(),
                color: "#ffffff".into(),
            }],
        };
        let mut canvas = Canvas::new(0, 0);
        renderer.draw(
            &mut canvas,
            &FrameInput {
                frame: &frame,
                settings: &settings,
                client_width: 880,
                client_height: 170,
                dpi: 96,
            },
        );
        assert!(canvas.bytes().iter().all(|&v| v == 0));
    }
}
