//! Linux 音频 I/O：**PipeWire**。
//!
//! 实现 `vox_core::ports` 里的 `CaptureSource` / `PlaybackSink` / `DeviceRegistry`，
//! 对应 Windows 侧的 `vox-audio-win`。锚定范围见 `docs/platform/LINUX.md`：
//! **PipeWire 必需**（主流发行版的默认音频服务），PulseAudio / 纯 ALSA 环境明确不支持
//! ——那边没有"按进程抓音"这个概念，硬降级成整机环回会把用户自己的麦也翻一遍。
//!
//! 几条贯穿全 crate 的规矩（跟 `vox-audio-win` 对齐）：
//! - 回调线程只搬数据，不分配、不加锁、不打日志；
//! - 错误一律翻成中文 `PortError`，永不 panic；
//! - 库里没有 `unwrap()` / `expect()`，测试里可以有。
//!
//! 非 Linux 上编译成空 lib，理由同 `vox-audio-win`：装配层按 `cfg(target_os)` 只挑一个依赖。

#![cfg(target_os = "linux")]

use vox_core::ports::PortResult;

mod capture;
mod link_keeper;
mod playback;
mod probe;
mod registry;
mod virtual_sink;

pub use capture::LinuxCapture;
pub use playback::LinuxPlayback;
pub use registry::LinuxDeviceRegistry;
pub use virtual_sink::{
    description as virtual_mic_description, VirtualSink, NODE_NAME as VIRTUAL_MIC_NODE_NAME,
};

/// PipeWire 在不在。装配层用它决定"能不能装音频后端"，连不上就该在界面上说清楚，
/// 而不是等用户开了流水线才发现抓不到声音。
pub fn pipewire_available() -> bool {
    probe::available()
}

/// 缺省采集口 / 缺省播放口的 **PipeWire 节点名**（`node.name`）。
///
/// 为什么要单问一次：设备条目（`DeviceInfo.name`）里装的是 `node.description`
/// ——给人看的标签（`USB 麦克风`），不是稳定标识，从它身上拿不到卡名。而
/// `docs/plans/S4-C-AEC.md` §4.3 的 B2（"采集与播放同不同卡"）要的正是卡名。
///
/// 任一侧取不到就给 `None`：**"认不出" ≠ "是同一张卡"**，判据照样不成立
/// （与 `vox-audio-alsa::same_card` 同一条纪律）。
pub fn default_source_and_sink_nodes() -> PortResult<(Option<String>, Option<String>)> {
    let snapshot = probe::snapshot()?;
    Ok((
        snapshot.default_source.clone(),
        snapshot.default_sink.clone(),
    ))
}

pub use registry::card_key_of_node;
