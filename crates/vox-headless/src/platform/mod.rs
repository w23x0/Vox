//! 平台分派：装配层只跟这一层打交道。
//!
//! 无屏档只有**一个**平台实现（Linux + PipeWire），但差别的形状跟桌面档一样必须收在这里：
//! 装配层（`headless.rs`）本身不写 `#[cfg]`，芯里更是一行都没有。
//!
//! | 能力 | 无屏档（Linux） | 别的平台 |
//! | --- | --- | --- |
//! | 档位 | `HostKind::LinuxHeadless` | 同（这个二进制没有别的档） |
//! | 音频三件套 | `vox-audio-linux`（PipeWire） | **明确报错**，不静默降级 |
//! | 采集 / 播放 / 设备目录 / 重采样 | 与桌面 Linux 侧同一份实现；**重采样档由本档自己选**（`VOX_RESAMPLE_QUALITY`，缺省便宜档） | — |
//! | 热键 / 托盘 / 悬浮字幕窗 / 虚拟麦 | **整块不启动**（位在上限之外） | — |
//!
//! [`Platform`] 这个类型放在分派文件里（两个实现都要返回它），实现只提供**值**。
//! `other` 那一份不是"占位实现"：无屏档的产品形态就是 Linux 小盒子，在这个二进制里假装
//! 能做 Windows 音频才是撒谎——所以它只做一件事：**说清楚做不到**。

use std::sync::Arc;

use vox_core::pipeline::{CaptureFactory, PlaybackFactory, ResampleFactory};
use vox_core::ports::{DeviceRegistry, PortError, PortResult};

use vox_dsp::ResampleQuality;

/// 无屏档的"上行 / 播放侧用哪一档重采样"的口子。
///
/// **只有无屏档读它**：桌面档不看这个环境变量（`app/src-tauri` 传的是
/// `vox_dsp::ports::resample_factory()`），共享宿主层（`vox-host`）也不读它。
/// 取值只认 `"sinc"` / `"cheap"`，缺省 `"cheap"`；别的值装配期就报错，
/// **不静默回落**（`.omp/RULES.md` #4）。
pub const RESAMPLE_QUALITY_ENV: &str = "VOX_RESAMPLE_QUALITY";

/// 读 [`RESAMPLE_QUALITY_ENV`] 选档。缺省 `cheap`（无屏盒子的缺省）。
pub fn resample_quality_from_env() -> PortResult<ResampleQuality> {
    match std::env::var(RESAMPLE_QUALITY_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(ResampleQuality::Linear),
        Err(std::env::VarError::NotUnicode(raw)) => Err(PortError::new(format!(
            "{RESAMPLE_QUALITY_ENV} 不是合法 UTF-8：{raw:?}"
        ))),
        Ok(value) => match value.as_str() {
            "sinc" => Ok(ResampleQuality::Sinc),
            "cheap" => Ok(ResampleQuality::Linear),
            other => Err(PortError::new(format!(
                "{RESAMPLE_QUALITY_ENV}={other:?} 不是合法取值：只认 \"sinc\" 或 \"cheap\"（缺省 \"cheap\"）"
            ))),
        },
    }
}

/// 按档取工厂。便宜档在降采样 / 同率上仍然落回 sinc
/// （见 `vox_dsp::ports::cheap_resample_factory`：那一半是"这一档在这条路上不存在"）。
pub fn resample_factory_for(quality: ResampleQuality) -> ResampleFactory {
    match quality {
        ResampleQuality::Sinc => vox_dsp::ports::resample_factory(),
        ResampleQuality::Linear => vox_dsp::ports::cheap_resample_factory(),
    }
}

/// 无屏档要用的音频几件套。工厂而不是实例：每次 Start 都要全新的采集/播放。
pub struct Platform {
    pub capture: CaptureFactory,
    pub playback: PlaybackFactory,
    /// 上行重采样工厂。**与 `playback` 内部那一份同档**。
    pub resample: ResampleFactory,
    /// 这一档定的重采样档位（读一次 [`RESAMPLE_QUALITY_ENV`] 的结果）。
    /// 留着是为了让入口能把"实际用的是哪一档"打进启动日志——两处各读一次环境变量
    /// 只会多一个不一致的机会。
    pub quality: ResampleQuality,
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
