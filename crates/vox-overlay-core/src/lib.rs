//! 悬浮字幕窗的**平台中立**部分：几何、配色、CPU 画布、布局、帧合成。
//!
//! Windows（`vox-overlay-win` 的 Win32 分层窗）和 Linux（`vox-overlay-linux` 的
//! GTK 窗）共用这一份渲染逻辑，各自只提供两样平台相关的东西：
//!
//! | 平台相关 | 谁提供 |
//! | --- | --- |
//! | 字形光栅器（按字形 id 出覆盖率蒙版、找回退字体） | Windows：GDI `ExtTextOutW` + `ETO_GLYPH_INDEX`；Linux：`swash`/`fontdb` + fontconfig |
//! | 窗口 + 呈现（把画好的 BGRA 贴到屏幕上） | Windows：`UpdateLayeredWindow`；Linux：GTK + cairo |
//!
//! 渲染器只认 [`text::GlyphSource`] 这个 trait，所以两边能共用同一套布局与动画
//! （换行上移、双行分色都是这里算的）。复杂文字（连写、附标、从右往左）也在这里：
//! [`shape`] 用 rustybuzz 整形成字形簇，[`render`] 按 UAX #9 分层，[`layout`] 逻辑顺序折行、
//! 逐视觉行重排——细节见 `docs/plans/OVERLAY-SHAPING.md`。
//!
//! 这条分层不是事后补的：`canvas.rs` / `geom.rs` / `layout.rs` 从写下来那天起就是
//! 纯运算、能脱离桌面单测（见各文件头的说明），搬到共享 crate 只是把事实摆正。

pub mod canvas;
pub mod color;
pub mod geom;
pub mod layout;
pub mod render;
pub mod shape;
pub mod text;
