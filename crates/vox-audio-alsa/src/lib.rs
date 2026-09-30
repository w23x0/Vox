//! ALSA 音频 I/O：麦克风采集、播放、设备目录（无屏档缺省后端）。
//!
//! 实现 `vox_core::ports` 里的 `CaptureSource` / `PlaybackSink` / `DeviceRegistry`。
//! 贯穿全 crate 的规矩与 `vox-audio-linux` / `vox-audio-win` 同口径：
//! - 设备线程只搬数据，不分配、不加锁、不打日志；
//! - 错误一律翻成中文 `PortError`，永不 panic；
//! - 库里没有 `unwrap()` / `expect()`，测试里可以有。
//!
//! 与 PipeWire 那份的分工见 `docs/plans/S4-C-ALSA.md` §6：**无屏档缺省 ALSA**，
//! 桌面档保持 PipeWire。ALSA 没有"按程序抓音"也没有虚拟麦，所以
//! `CaptureTarget::ProcessLoopback` 在这一侧是报错的，不是静默降级。
//!
//! 非 Linux 上编译成空 lib，理由同 `vox-audio-linux` / `vox-audio-win`：
//! 装配层按 `cfg(target_os)` 只挑一个依赖，`alsa-sys` 的 `build.rs` 根本不会跑。

#![cfg(target_os = "linux")]

mod probe;
mod registry;

pub use registry::AlsaDeviceRegistry;

/// ALSA 在不在 = **能不能打开一条 PCM**。装配层用它决定"能不能装音频后端"。
///
/// 不缓存：USB 声卡随时可能被拔，缓存下来的 `true` 是假事实
/// （与 `vox_core::capability` 模块头"位 = 事实，不是期望"同一条纪律）。
pub fn alsa_available() -> bool {
    probe::alsa_available()
}

/// PipeWire 在 ALSA 这边铺的桥（`pipewire-alsa` 那套 pcm 定义）在不在。
///
/// **只用来发启动提示，不作能力判据**：它既不能证明 PipeWire 在跑
/// （daemon 可能没起、可能只有 Pulse），也不能证明 ALSA 打不开设备
/// （`hw:` 直开根本不走那个桥）。判定规则见 `docs/plans/S4-C-ALSA.md` §6。
pub fn pipewire_alsa_bridge_present() -> bool {
    probe::pipewire_alsa_bridge_present()
}
