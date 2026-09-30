//! Linux 音频三个工厂：全部来自 `vox-audio-linux`（PipeWire）。
//!
//! 跟 Windows 侧 `platform/win.rs` 的三个工厂一一对应，唯一差别是播放汇的
//! 重采样器工厂：那边要按 WASAPI 探测到的设备率转，这边请求 48 kHz 让 PipeWire
//! 在图里转，但接口形状一样（`LinuxPlayback::new(ResampleFactory)`）。

use std::sync::Arc;

use vox_core::pipeline::{CaptureFactory, PlaybackFactory};
use vox_core::ports::DeviceRegistry;

/// 采集：麦克风与按程序抓音都走这一个实现，具体抓谁看 `CaptureTarget`。
pub fn capture_factory() -> CaptureFactory {
    Box::new(|| Box::new(vox_audio_linux::LinuxCapture::new()))
}

pub fn playback_factory() -> PlaybackFactory {
    Box::new(|| {
        let rf = vox_dsp::ports::resample_factory();
        Box::new(vox_audio_linux::LinuxPlayback::new(rf))
    })
}

/// 设备枚举：PipeWire 图里数节点、按程序名合并。无状态，全进程共用一个。
pub fn registry() -> Arc<dyn DeviceRegistry> {
    Arc::new(vox_audio_linux::LinuxDeviceRegistry::new())
}
