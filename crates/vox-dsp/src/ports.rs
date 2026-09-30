//! `Denoise` / `Resample` 两个端口的实现。
//!
//! 两个 trait 住在 `vox_core::ports`，而 [`crate::Denoiser`] / [`crate::Resampler`] 是**本
//! crate 的本地类型**——本地类型实现外部 trait 满足 coherence 要求，所以这里直接 impl，
//! 不用再套一层 newtype 转发（`docs/plans/S4-A-HOST-LAYER.md` §1.4 的核实）。
//!
//! 两个工厂因此也不在装配层里了：桌面（`app/src-tauri`）与无屏（`crates/vox-headless`）
//! 以前各留一份同形的适配器，现在共用本文件这一份。

use vox_core::pipeline::{DenoiseFactory, ResampleFactory};
use vox_core::ports::{Denoise, PortResult, Resample};

use crate::{Denoiser, Resampler};

impl Denoise for Denoiser {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        Denoiser::process(self, samples)
    }

    fn reset(&mut self) {
        Denoiser::reset(self)
    }
}

impl Resample for Resampler {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        Resampler::process(self, samples)
    }

    fn flush(&mut self) -> Vec<f32> {
        Resampler::flush(self)
    }

    fn reset(&mut self) {
        Resampler::reset(self)
    }
}

/// 降噪工厂。失败时内核会退化成不降噪，不会把流水线弄挂。
pub fn denoise_factory() -> DenoiseFactory {
    Box::new(|| -> PortResult<Box<dyn Denoise>> { Ok(Box::new(Denoiser::new()?)) })
}

/// 重采样工厂。同率时 `Resampler` 内部零开销透传。
pub fn resample_factory() -> ResampleFactory {
    Box::new(|from, to| Box::new(Resampler::new(from, to)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denoise_adapter_forwards() {
        let mut d = denoise_factory()().expect("RNNoise 应该能建起来");
        // 48 kHz 一帧 480 个样本；第一帧可能被吞掉，只要不 panic 就行。
        let out = d.process(&vec![0.0f32; 480]);
        assert!(out.len().is_multiple_of(480), "输出应是整帧：{}", out.len());
        d.reset();
    }

    #[test]
    fn resample_adapter_changes_rate() {
        let mut r = resample_factory()(48_000, 16_000);
        let mut produced = r.process(&vec![0.0f32; 4800]).len();
        produced += r.flush().len();
        // 48k -> 16k 是 1/3；允许内部缓冲带来的偏差，只要量级对。
        assert!(
            (1000..=1800).contains(&produced),
            "48k->16k 出来的样本数不对：{produced}"
        );
        r.reset();
    }

    #[test]
    fn resample_same_rate_passes_through() {
        let mut r = resample_factory()(16_000, 16_000);
        assert_eq!(r.process(&vec![0.5f32; 320]).len(), 320);
    }
}
