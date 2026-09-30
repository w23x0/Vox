//! 无屏档（小主板 ARM64 Linux）入口。
//!
//! 这是跟 `app/src-tauri`（桌面档）**并列的第二个外壳**，不是它的分支：同一份芯
//! （`vox-core`）、同一份 PipeWire 后端（`vox-audio-linux`），差别只有"入口"与"起了哪些东西"。
//! 无屏三件（配置从哪进 / 状态往哪出 / 进程怎么活）见 `docs/platform/EMBEDDED.md` §3.5。
//!
//! ```text
//! main          解析命令行 + 装日志 + 分派（薄，没有业务）
//! headless      装配（Assembly）与跑起来的那一份（Daemon）——装配层本体
//! config        这一档的密钥文件名（目录回落与读设置在 `vox_host::paths`）
//! status        状态出口：能力位报告 JSON + 清单 JSON（S0 §4.3-A）+ 芯事件 → 结构化日志
//! platform      `cfg(target_os)` 分流：LinuxHeadless 的档位、事实、音频三件套
//! sys           日志（跟 OS 打交道的小件）
//! ```
//!
//! 落盘（`Persist`）、配置目录三级回落、读 `settings.json` / `usage.json`、三个密钥后端、
//! `LocalClock`、控制面胶水（`vox_host::control`）、事件出口（`vox_host::events::LogSink`）
//! 与两个报告出口（`vox_host::report`）全都在共享宿主层 `vox_host`（S4-A），与桌面档同一份
//! ——本 crate 只留"这一档挑哪个"与"这一档的文件名"。
//!
//! **跟桌面档的三处刻意差别**（都是有理由的，不是"还没写"）：
//!
//! 1. **不引入 Tauri**：无屏盒子上装不了也不需要 GTK/WebKitGTK。`cargo tree -p
//!    vox-headless | grep -c tauri` = 0 是验收项。
//! 2. **不启动热键 / 托盘 / 悬浮字幕窗**：这三位在 `linux_headless` 档的 `host_ceiling`
//!    之外（位恒 `false(unsupported)`），起了也没人看（§3.7）。
//! 3. **不建虚拟麦节点**：无屏档的出口是网络/声卡，不给别的程序当麦克风；`virtual_mic`
//!    同样在档位上限之外，`HostFacts::virtual_mic_device` 恒 `None`。
//!
//! 芯里一行 `#[cfg]` 都没有：平台差异只出现在 `platform/`（这个 crate 内）与 `Cargo.toml`
//! 的 `target.'cfg(target_os = "linux")'` 依赖表里。

pub mod cli;
pub mod config;
pub mod headless;
pub mod platform;
pub mod status;
pub mod sys;

pub use headless::{run, Assembly, Daemon, Error, Result};
