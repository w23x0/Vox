//! 平台分派：装配层只跟这一层打交道。
//!
//! 无屏档只有**一个**平台实现（Linux + PipeWire），但差别的形状跟桌面档一样必须收在这里：
//! 装配层（`headless.rs`）本身不写 `#[cfg]`，芯里更是一行都没有。
//!
//! | 能力 | 无屏档（Linux） | 别的平台 |
//! | --- | --- | --- |
//! | 档位 | `HostKind::LinuxHeadless` | 同（这个二进制没有别的档） |
//! | 音频三件套 | `vox-audio-linux`（PipeWire） | **明确报错**，不静默降级 |
//! | 采集 / 播放 / 设备目录 | 与桌面 Linux 侧同一份实现 | — |
//! | 热键 / 托盘 / 悬浮字幕窗 / 虚拟麦 | **整块不启动**（位在上限之外） | — |
//!
//! [`Platform`] 这个类型放在分派文件里（两个实现都要返回它），实现只提供**值**。
//! `other` 那一份不是"占位实现"：无屏档的产品形态就是 Linux 小盒子，在这个二进制里假装
//! 能做 Windows 音频才是撒谎——所以它只做一件事：**说清楚做不到**。

use std::sync::Arc;

use vox_core::pipeline::{CaptureFactory, PlaybackFactory};
use vox_core::ports::DeviceRegistry;

/// 无屏档要用的音频三件套。工厂而不是实例：每次 Start 都要全新的采集/播放。
pub struct Platform {
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    pub registry: Arc<dyn DeviceRegistry>,
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::*;
