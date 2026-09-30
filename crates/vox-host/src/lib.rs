//! 共享宿主层：Vox 的**两个装配层**（`app/src-tauri` 桌面档 / `crates/vox-headless` 无屏档）
//! 共用的那一半。
//!
//! 这不是第三个外壳，是"外壳里那部分两个外壳都在写的东西"抽出来的一份：同一份落盘、同一个
//! 配置目录回落、同一套控制面胶水、同一对报告出口。**行为不变**——搬过来的实现与用例都是
//! 从那两份里逐字搬来的，搬完两个入口各自只剩"本机有哪些端口 + 本机能力位 + 入口参数 +
//! 宿主特有那几步"（见 `docs/plans/S4-A-HOST-LAYER.md` §3.6）。
//!
//! ```text
//! paths      配置目录三级回落（Paths / dir_from / dir_from_env）+ 读 settings.json / usage.json
//! persist    设置与用量落盘（去抖 800ms + 原子写 .tmp→rename）
//! clock      LocalClock（chrono），供两个 Linux 入口共用
//! secrets    SecretBackend 枚举 + 三个后端（DPAPI / Secret Service / 0600 文件）
//! control    控制面胶水：Switch / Status / ControlPlane / start / 死凭据清扫
//! events     事件出口：trait EventSink + LogSink（结构化日志那一半）
//! report     两端逐字同形的两个报告出口（能力位 / 清单）
//! core       芯的那一半：Core::assemble（共用装配步骤 1–7）+ scan_devices
//! entry      HostPorts：宿主入口注入进来的那一份 + 引擎那一半（deps / engine）
//! ```
//!
//! **不碰界面那套**（tauri / gtk / webkit / wry / tao / openvr 一个都没有），也**不碰 tokio**
//! ——理由与逐条证据见 `Cargo.toml` 的文件头与 §3.1。平台差异只出现在三处：
//! `secrets/` 的三个后端（各自 `#[cfg]`）、`control::process_alive`（判"那个 pid 还活着吗"
//! 没有跨平台写法）、以及 `Cargo.toml` 里按 target 门控的依赖表。

pub mod clock;
pub mod control;
pub mod core;
pub mod entry;
pub mod events;
pub mod paths;
pub mod persist;
pub mod report;
pub mod secrets;

pub use control::{ControlPlane, Status, Switch, CONTROL_FILE};
pub use core::{scan_devices, Core, Finish, Notes, PersistMode};
pub use entry::HostPorts;
pub use events::{EventSink, LogSink};
pub use paths::{dir_from, dir_from_env, Paths, SETTINGS_FILE, USAGE_FILE};
pub use persist::Persist;
