//! 宿主入口注入进来的那一份。
//!
//! [`HostPorts`] **只放"本机有什么"，不放"这个进程怎么活"**：档位与事实、时钟、密钥后端、
//! 三个端口工厂、启动提示。窗口 / 托盘 / 热键 / 悬浮 / 虚拟麦 / 头显 / 设备轮询
//! 那些宿主特有的步骤**不进这里**——它们要么只在桌面有，要么只在无屏有，共享一份等于给
//! 另一档塞一个不启动的线程。
//!
//! **传输工厂也不在字段里**，它是 [`HostPorts::engine`] / [`HostPorts::deps`] 的参数：
//! tokio `Handle` 属于"这个进程怎么活"（桌面复用 Tauri 那一个 runtime，无屏自建一个），
//! 而它的句柄**不保活** runtime，保活必须留在入口（无屏的 `Daemon._net`）。做成参数而不是
//! 字段，就是让这一层连"进程怎么活"都碰不着。

use std::sync::Arc;

use vox_core::capability::HostFacts;
use vox_core::composition::HostKind;
use vox_core::pipeline::{
    CaptureFactory, Deps, PipelineEngine, PlaybackFactory, ResampleFactory, TransportFactory,
};
use vox_core::ports::{Clock, DeviceRegistry};
use vox_core::runtime::{PipelineControl, Runtime};

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
    /// 采集 / 播放 / **重采样** / 设备目录四样。**由入口建好后按值交进来**
    /// （`CaptureFactory` 等是 `Box<dyn Fn…>`，不是 `Clone`）。
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    /// **上行重采样工厂**。桌面传 `vox_dsp::ports::resample_factory()`（逐字等于引入这一格之前），
    /// 无屏传便宜档（`cheap_resample_factory()`，或按 `VOX_RESAMPLE_QUALITY` 选出来的那一档）。
    ///
    /// 为什么是字段而不是 [`HostPorts::deps`] 的一个参数：`deps(self, transport)` **消费掉**
    /// `self`，而 `Core::assemble(&ports, …)` 之后还要拿同一份 `ports` 装引擎。加参数就得把
    /// `assemble` 的签名一路改下去——那是**两份装配点都要动**的形状。做成字段则是
    /// **每个装配点自己决定**，与 `capture` / `playback` 两个工厂同一个形状
    /// （`docs/plans/S4-C-GATE-RESAMPLE.md` §3.4）。
    pub resample: ResampleFactory,
    pub registry: Arc<dyn DeviceRegistry>,
    /// 启动期要告诉用户的话（进 `Notice` / journal，不弹窗）。
    pub startup_notes: fn() -> Vec<vox_core::Msg>,
}

impl HostPorts {
    /// 引擎要的五个工厂。**降噪不来自入口**：它在 S4-A 之后由 `vox-dsp` 直接提供
    /// （`vox_dsp::ports::denoise_factory`），入口对那一节没有发言权。
    /// **重采样来自入口**（[`HostPorts::resample`]）：它的档位是"这一档构建"的事实，
    /// 只有装端口的人知道。
    ///
    /// 消费掉 `self`（四个工厂是 `Box<dyn Fn…>`，不是 `Clone`），所以这一份**建一次、
    /// 用一次**：[`Core::assemble`](crate::Core::assemble) 只读它，装引擎才是把它吃掉的那一步。
    pub fn deps(self, transport: TransportFactory) -> Deps {
        Deps {
            transport,
            capture: self.capture,
            playback: self.playback,
            denoise: vox_dsp::ports::denoise_factory(),
            // 回声消除与降噪一样来自 `vox-dsp`（`sonora-aec3` 的那一层在那边），
            // 入口对它没有发言权。
            aec: vox_dsp::ports::aec_factory(),
            resample: self.resample,
        }
    }

    /// `PipelineEngine::new` + 把引擎注入账本（`Runtime` 只认 [`PipelineControl`]）。
    /// 桌面是 `lib.rs` 的第 5 步、无屏是 `Daemon::start`，两边逐条同形。
    pub fn engine(self, runtime: &Runtime, transport: TransportFactory) -> Arc<PipelineEngine> {
        let engine = PipelineEngine::new(runtime.clone(), self.deps(transport));
        runtime.set_control(Arc::clone(&engine) as Arc<dyn PipelineControl>);
        engine
    }
}
