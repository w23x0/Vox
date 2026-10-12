//! `Denoise` / `Resample` 两个端口的实现。
//!
//! 两个 trait 住在 `vox_core::ports`，而 [`crate::Denoiser`] / [`crate::Resampler`] 是**本
//! crate 的本地类型**——本地类型实现外部 trait 满足 coherence 要求，所以这里直接 impl，
//! 不用再套一层 newtype 转发（`docs/plans/S4-A-HOST-LAYER.md` §1.4 的核实）。
//!
//! 两个工厂因此也不在装配层里了：桌面（`app/src-tauri`）与无屏（`crates/vox-headless`）
//! 以前各留一份同形的适配器，现在共用本文件这一份。

use vox_core::pipeline::{AecFactory, DenoiseFactory, ResampleFactory};
use vox_core::ports::{Aec, Denoise, PortResult, Resample};

use crate::aec::SonoraAec;
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

/// 回声消除工厂，参数是**采集率**。造不出来（采集率不是 16000 / 48000、
/// 或者分配失败）返回 `Err`，此时链上只是**不消回声**，不让整条流水线挂掉。
///
/// 与降噪同一条：这一节由 `vox-dsp` 直接提供，入口对它没有发言权
/// （`vox_host::entry::HostPorts::deps` 里与 `denoise_factory()` 并排）。
pub fn aec_factory() -> AecFactory {
    Box::new(|rate| -> PortResult<Box<dyn Aec>> { Ok(Box::new(SonoraAec::new(rate)?)) })
}

/// 便宜档工厂：**只在升采样方向**给线性插值，其余情况给 sinc。
///
/// 落回 sinc **不是降级**，是"这一档在这条路上不存在"（降采样需要抗混叠，见 S4-C 稿 §3.3）。
/// 到底是哪一档，由装这个工厂的入口在启动时记一行日志说清楚（vox-dsp 不依赖 tracing，
/// 所以日志在 `vox-headless` / `app/src-tauri` 那一头打）。
pub fn cheap_resample_factory() -> ResampleFactory {
    Box::new(|from, to| match Resampler::cheap_upsampler(from, to) {
        Some(resampler) => Box::new(resampler) as Box<dyn Resample>,
        None => Box::new(Resampler::new(from, to)) as Box<dyn Resample>,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aec_factory_only_builds_the_supported_rates() {
        // `AEC_RATES` 的**内容**也要有钉子：只钉"判据用同一个源"拦不住"往源里加一个
        // 驱不动的率"。32 kHz 不在表里（S4-C-AEC §10 R12：上游的块级 API 驱不动它），
        // 所以工厂必须报 Err；谁要往 `AEC_RATES` 里加 32000，这条会红着要他先证明 32 k 能用。
        for rate in vox_core::pipeline::AEC_RATES {
            assert!(aec_factory()(rate).is_ok(), "{rate} Hz 该建得起来");
        }
        assert!(
            aec_factory()(32_000).is_err(),
            "32 kHz 不在 AEC_RATES 里，工厂必须报 Err"
        );
    }

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

    /// 回声消除工厂：支持的采集率建得出来、20 ms 一拍的输入出满 20 ms；
    /// 不支持的率**直接报错**（链上那一档由 `Chain::build` 降级成 `AecSkip::Rate`，
    /// 两条判据用的是同一份 `AEC_RATES`）。
    #[test]
    fn aec_adapter_builds_and_forwards() {
        let mut aec = aec_factory()(48_000).expect("48 kHz 要能建起来");
        aec.push_reference(&vec![0.0f32; 20 * 24]);
        let out = aec.process(&vec![0.0f32; 20 * 48]);
        assert_eq!(out.len(), 20 * 48, "20 ms 一拍的输入该出满 20 ms");
        aec.reset();

        let err = match aec_factory()(44_100) {
            Ok(_) => panic!("44.1 kHz 不该建得出来"),
            Err(err) => err,
        };
        assert!(
            err.message.contains("16000") && err.message.contains("44"),
            "错误要说清支持的档与收到的率：{}",
            err.message
        );
    }
}
