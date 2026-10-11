//! 降噪 + 重采样 + 回声消除 + 播放侧环形缓冲。平台无关，但依赖原生 DSP 库，所以单独成 crate。
//!
//! * [`Resampler`] — 任意采样率互转，内部缓冲流式处理，同率时零开销透传。
//!   [`ResampleQuality`] 是它的两档：缺省 sinc，便宜档（线性插值）只对升采样方向成立。
//! * [`Denoiser`] — RNNoise 降噪（nnnoiseless），48 kHz / 480 帧。
//! * [`aec::SonoraAec`] — 回声消除（`sonora-aec3`，WebRTC AEC3 的纯 Rust 移植），
//!   16 kHz 单频带 / 48 kHz 三频带。
//! * [`ring::DropRing`] — 播放侧的无锁环形缓冲（两个平台共用）。
//! * [`chunk::Blocker`] — 采集侧把连续样本切成定长块（两个平台共用）。
//! * [`ports`] — 上面几个类型对 `vox-core` 端口的 impl 与工厂，装配层直接用。

pub mod aec;
pub mod channels;
pub mod chunk;
mod denoise;
pub mod ports;
mod resample;
pub mod ring;

pub use denoise::{backend_name, Denoiser, NATIVE_SAMPLE_RATE};
pub use resample::{ResampleQuality, Resampler};
