//! 宿主入口注入进来的那一份。
//!
//! [`HostPorts`] **只放"本机有什么"，不放"这个进程怎么活"**：档位与事实、时钟、密钥后端、
//! 三个端口工厂、传输工厂、启动提示。窗口 / 托盘 / 热键 / 悬浮 / 虚拟麦 / 头显 / 设备轮询
//! 那些宿主特有的步骤**不进这里**——它们要么只在桌面有，要么只在无屏有，共享一份等于给
//! 另一档塞一个不启动的线程。

use std::sync::Arc;

use vox_core::capability::HostFacts;
use vox_core::composition::HostKind;
use vox_core::pipeline::{CaptureFactory, PlaybackFactory, TransportFactory};
use vox_core::ports::{Clock, DeviceRegistry};

use crate::secrets::SecretBackend;

/// 宿主入口注入进来的那一份。
///
/// 用 `fn()` 而不是值：`host_facts()` 在桌面每 4 秒被设备轮询线程读一次，函数指针让那份
/// "每 tick 重算"的语义保持原样。
///
/// **没有 `Clone`**：三个工厂是 `Box<dyn Fn…>`，按 §3.3 的说法它们"不是 `Clone`"，
/// 所以这一份只能**按值建好、按引用用**（`Core::assemble(&ports, …)`）。设计稿 §3.3 上那行
/// `#[derive(Clone)]` 与它自己的注释互相矛盾——按注释办。
pub struct HostPorts {
    /// 声明自己是哪一档。桌面 = `platform::host_kind`；无屏 = 自己的 `platform::host_kind`。
    pub kind: fn() -> HostKind,
    /// 报这台机器现在的事实（**只报"关掉的位"**，位由芯算）。
    pub facts: fn() -> HostFacts,
    /// 本机时钟。
    pub clock: Arc<dyn Clock>,
    /// 密钥后端。见 [`SecretBackend`]。
    pub secret: SecretBackend,
    /// 采集 / 播放 / 设备目录三个工厂。**由入口建好后按值交进来**
    /// （`CaptureFactory` 等是 `Box<dyn Fn…>`，不是 `Clone`）。
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    pub registry: Arc<dyn DeviceRegistry>,
    /// 传输工厂。**tokio `Handle` 由入口先备好再闭包捕获**（桌面复用 Tauri 的，
    /// 无屏自建 runtime），runtime 的保活留在入口。
    pub transport: TransportFactory,
    /// 启动期要告诉用户的话（进 `Notice` / journal，不弹窗）。
    pub startup_notes: fn() -> Vec<String>,
}
