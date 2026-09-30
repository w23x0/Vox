//! 降噪 + 重采样 + 播放侧环形缓冲。平台无关，但依赖原生 DSP 库，所以单独成 crate。
//!
//! * [`Resampler`] — 任意采样率互转，内部缓冲流式处理，同率时零开销透传。
//! * [`Denoiser`] — RNNoise 降噪（nnnoiseless），48 kHz / 480 帧。
//! * [`ring::DropRing`] — 播放侧的无锁环形缓冲（两个平台共用）。
//! * [`chunk::Blocker`] — 采集侧把连续样本切成定长块（两个平台共用）。
//! * [`ports`] — 上面两个类型对 `vox-core` 端口的 impl 与工厂，装配层直接用。

pub mod channels;
pub mod chunk;
mod denoise;
pub mod ports;
mod resample;
pub mod ring;

pub use denoise::{backend_name, Denoiser, NATIVE_SAMPLE_RATE};
pub use resample::Resampler;
