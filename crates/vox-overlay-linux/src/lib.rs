//! Linux 悬浮字幕窗：GTK 透明窗 + XWayland 定位置顶 + swash 字形光栅。
//!
//! 实现 `vox_core::ports::SubtitleView`。渲染逻辑在平台中立的 `vox-overlay-core`
//! 里，这里只做两件平台相关的事：**字形光栅**（`font.rs`，swash）与**窗口+呈现**
//! （`window.rs`，GTK + cairo）。
//!
//! 非 Linux 上编译成空 lib，理由同其它 `-win`/`-linux` 兄弟。

#![cfg(target_os = "linux")]

mod font;
mod mailbox;
mod placement;
mod window;

use std::sync::Arc;

use vox_core::ports::{PortResult, SubtitleFrame, SubtitleView};
use vox_core::settings::SubtitleSettings;

pub use font::font_factory;
pub use window::{GeometryCallback, Overlay};

impl SubtitleView for Overlay {
    fn show(&self) {
        if let Ok(mut mailbox) = self.mailbox().lock() {
            mailbox.visible = true;
        }
        self.request_redraw();
    }

    fn hide(&self) {
        if let Ok(mut mailbox) = self.mailbox().lock() {
            mailbox.visible = false;
        }
        self.request_redraw();
    }

    /// 推一帧。**线程安全**：只写邮箱 + 叫醒主线程，不碰任何 GTK 对象。
    fn render(&self, frame: SubtitleFrame) {
        if let Ok(mut mailbox) = self.mailbox().lock() {
            mailbox.set_frame(frame);
        }
        self.request_redraw();
    }

    /// 外观变了（字体、字号、底衬透明度、几何）。真正换字体在主线程的重画里做。
    fn restyle(&self, settings: &SubtitleSettings) {
        if let Ok(mut mailbox) = self.mailbox().lock() {
            mailbox.set_settings(settings);
        }
        // 几何也在设置里：设置页改位置/大小、「重置默认」都靠这里实时搬窗。
        self.apply_settings_geometry(settings.geometry);
        self.request_redraw();
    }
}

/// 起一个悬浮窗。**必须在 GTK 主线程上调**（装配层的 `assemble()` 就在主线程）。
pub fn spawn(settings: &SubtitleSettings) -> PortResult<Arc<Overlay>> {
    Overlay::spawn(settings, font_factory(), None)
}

/// 同 [`spawn`]，另带几何回写回调（目前无人触发，见 [`GeometryCallback`]）。
pub fn spawn_with_geometry(
    settings: &SubtitleSettings,
    on_geometry: GeometryCallback,
) -> PortResult<Arc<Overlay>> {
    Overlay::spawn(settings, font_factory(), Some(on_geometry))
}
