//! `Denoise` / `Resample` 两个端口的适配器。
//!
//! `vox-dsp` 里的 `Denoiser` / `Resampler` 方法签名跟端口对得上，但它们没有 impl 那两个
//! trait（trait 在 `vox-core`、类型在 `vox-dsp`，两个都不是**本** crate 的，孤儿规则不让
//! 我们直接 impl）。所以这里套一层 newtype——纯转发，零逻辑。
//!
//! 跟桌面装配层的 `app/src-tauri/src/dsp.rs` 逐行同形：两份外壳都要这件胶水，
//! 而这一层里有 `tokio`/Tauri 之类不能共享的东西，所以各留各的一份。

use vox_core::pipeline::{DenoiseFactory, ResampleFactory};
use vox_core::ports::{Denoise, PortResult, Resample};

struct DenoiseAdapter(vox_dsp::Denoiser);

impl Denoise for DenoiseAdapter {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        self.0.process(samples)
    }

    fn reset(&mut self) {
        self.0.reset()
    }
}

struct ResampleAdapter(vox_dsp::Resampler);

impl Resample for ResampleAdapter {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        self.0.process(samples)
    }

    fn flush(&mut self) -> Vec<f32> {
        self.0.flush()
    }

    fn reset(&mut self) {
        self.0.reset()
    }
}

/// 降噪工厂。失败时内核会退化成不降噪，不会把流水线弄挂。
pub fn denoise_factory() -> DenoiseFactory {
    Box::new(|| -> PortResult<Box<dyn Denoise>> {
        Ok(Box::new(DenoiseAdapter(vox_dsp::Denoiser::new()?)))
    })
}

/// 重采样工厂。同率时 `Resampler` 内部零开销透传。
pub fn resample_factory() -> ResampleFactory {
    Box::new(|from, to| Box::new(ResampleAdapter(vox_dsp::Resampler::new(from, to))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denoise_adapter_forwards() {
        let mut denoise = denoise_factory()().expect("RNNoise 应该能建起来");
        // 48 kHz 一帧 480 个样本；第一帧可能被吞掉，只要不 panic 就行。
        let out = denoise.process(&vec![0.0f32; 480]);
        assert!(out.len().is_multiple_of(480), "输出应是整帧：{}", out.len());
        denoise.reset();
    }

    #[test]
    fn resample_adapter_changes_rate() {
        let mut resample = resample_factory()(48_000, 16_000);
        let mut produced = resample.process(&vec![0.0f32; 4800]).len();
        produced += resample.flush().len();
        // 48k -> 16k 是 1/3；允许内部缓冲带来的偏差，只要量级对。
        assert!(
            (1000..=1800).contains(&produced),
            "48k->16k 出来的样本数不对：{produced}"
        );
        resample.reset();
    }

    #[test]
    fn resample_same_rate_passes_through() {
        let mut resample = resample_factory()(16_000, 16_000);
        assert_eq!(resample.process(&vec![0.5f32; 320]).len(), 320);
    }
}
