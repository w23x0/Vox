//! 任意采样率互转。
//!
//! 音频回调给的块大小不固定，这里负责把散碎的输入攒够 rubato 要求的帧长，
//! 产出所有已就绪的输出样本。同率时完全透传，不分配 rubato 实例。
//!
//! # 两档
//!
//! * [`ResampleQuality::Sinc`]（**缺省**，[`Resampler::new`]）—— 选 `SincFixedIn` 而非
//!   `FastFixedIn`：语音场景对高频伪影敏感，sinc 有抗混叠滤波器，延迟多约 5 ms
//!   但音质明显好于多项式插值。**降采样（`out < in`）只有这一档。**
//! * [`ResampleQuality::Linear`]（**便宜档**，[`Resampler::cheap_upsampler`]）—— 线性插值，
//!   没有滤波器。**只对升采样方向成立**（`output_rate <= input_rate` 时构造不出来）。
//!
//! # 为什么便宜档只对升采样
//!
//! 线性插值没有低通。**降采样**时高频会混叠着折进语音带——实测 48k→16k 的 10 kHz
//! 折到 6 kHz 只剩 −0.0 dBFS（全幅，见 `docs/plans/S4-C-GATE-RESAMPLE.md` §3.1 表二），
//! 而 6 kHz 正落在语音带里，且混叠不可逆。**升采样**时同一件事只会在输出带外留下镜像
//! （24 kHz 带限的信号插到 48 kHz，镜像是 24–48 kHz 那一段，小喇叭基本发不出来），
//! 换来的却是播放链的绝大部分开销（同表：24k→48k 这一档占播放链的八成）。
//! 所以便宜档是**一路定向的**：它在 `out <= in` 上根本不存在，不给运行期静默降级
//! （`.omp/RULES.md` #4）。

use rubato::{
    Resampler as RubatoResampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// 音频回调给的块不知道多大，这个 chunk_size 只是 rubato 内部的处理粒度。
/// 选小一点减少延迟，480 样本 = 10 ms @ 48 kHz、30 ms @ 16 kHz，够小。
const CHUNK_SIZE: usize = 480;

/// sinc 插值参数：128 点滤波器 + cubic 插值，实测 CPU 开销可忽略。
fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// 重采样档。**缺省 [`Self::Sinc`]**，与本模块引入前一模一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResampleQuality {
    /// 128 点 sinc + 抗混叠（`rubato::SincFixedIn`）。**降采样唯一的合格档。**
    #[default]
    Sinc,
    /// 线性插值。**只对升采样方向成立**（见 [`Resampler::cheap_upsampler`] 的文档）。
    Linear,
}

/// 流式重采样器。内部缓冲使调用方可以任意长度喂入。
pub struct Resampler {
    inner: ResamplerInner,
}

enum ResamplerInner {
    /// 输入输出采样率相同，直接透传。
    Passthrough,
    /// 需要实际重采样。
    Active {
        resampler: Box<SincFixedIn<f32>>,
        /// 等待凑满一帧的输入缓冲。
        buffer: Vec<f32>,
        /// 复用的处理帧，避免每次 `process` 都分配临时 Vec。
        chunk: Vec<f32>,
        /// rubato 每次要求的输入帧数。
        frames_needed: usize,
    },
    /// 便宜档：线性插值，没有滤波器（只有升采样方向会走到这一支）。
    Linear {
        /// 每个输出样本在输入里前进多少（= `input_rate / output_rate`，升采样时 `< 1`）。
        step: f64,
        /// 下一个输出样本在 `buffer` 里的位置。
        pos: f64,
        /// 还没被插值覆盖的输入尾巴（跨块保留，最多一个样本 + 不足一步的余量）。
        buffer: Vec<f32>,
    },
}

impl Resampler {
    /// 创建重采样器。`input_rate == output_rate` 时零开销透传。
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        if input_rate == output_rate {
            return Self {
                inner: ResamplerInner::Passthrough,
            };
        }
        let ratio = output_rate as f64 / input_rate as f64;
        // max_relative_ratio = 1.0：固定比率，不需要运行时变速。
        let resampler = SincFixedIn::<f32>::new(
            ratio,
            1.0,
            sinc_params(),
            CHUNK_SIZE,
            1, // 单声道
        )
        .expect("rubato 参数合法，不应失败");
        let frames_needed = resampler.input_frames_next();
        Self {
            inner: ResamplerInner::Active {
                resampler: Box::new(resampler),
                buffer: Vec::with_capacity(frames_needed),
                chunk: vec![0.0; frames_needed],
                frames_needed,
            },
        }
    }

    /// 构造一档**便宜**的升采样器。
    ///
    /// 返回 `None` 的两种情形，都是"这一档在这条路上不成立"，**不是失败**：
    ///   * `output_rate <= input_rate` —— 降采样没有抗混叠，线性插值会把高频原样折进
    ///     语音带（实测 10 kHz → 6 kHz 残留 −0.0 dBFS，见 S4-C 稿 §3.1 表二）。
    ///     降采样只有 sinc 一档。
    ///   * `input_rate == output_rate` —— 同率是透传，根本没有插值这回事。
    ///
    /// 本模块刻意不提供静默降级（`RULES.md` #4：不许留假实现），由调用方决定要不要退回 sinc。
    pub fn cheap_upsampler(input_rate: u32, output_rate: u32) -> Option<Self> {
        if output_rate <= input_rate {
            return None;
        }
        Some(Self {
            inner: ResamplerInner::Linear {
                // 升采样 ⇒ step < 1：`process` 的 `pos` 一步跨不过输入末尾，
                // 收尾那点夹取只是防呆（见 `process` 的注释）。
                step: input_rate as f64 / output_rate as f64,
                pos: 0.0,
                buffer: Vec::new(),
            },
        })
    }

    /// 输出端延迟（样本数）。透传时为 0。
    pub fn output_delay(&self) -> usize {
        match &self.inner {
            ResamplerInner::Passthrough => 0,
            ResamplerInner::Active { resampler, .. } => resampler.output_delay(),
            ResamplerInner::Linear { .. } => 0,
        }
    }

    /// 喂入任意长度的单声道样本，返回当前已产出的输出样本。
    /// 输入不够一帧时返回空 Vec，不阻塞。
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        match &mut self.inner {
            ResamplerInner::Passthrough => input.to_vec(),
            ResamplerInner::Active {
                resampler,
                buffer,
                chunk,
                frames_needed,
            } => {
                buffer.extend_from_slice(input);
                let mut output = Vec::with_capacity(buffer.len().saturating_mul(2));
                while buffer.len() >= *frames_needed {
                    let frame_len = *frames_needed;
                    chunk[..frame_len].copy_from_slice(&buffer[..frame_len]);
                    buffer.copy_within(frame_len.., 0);
                    buffer.truncate(buffer.len() - frame_len);
                    // rubato 的 process 接口：&[impl AsRef<[T]>]，每个元素是一个声道。
                    let result = resampler
                        .process(&[&chunk[..frame_len]], None)
                        .expect("输入长度正确，不应失败");
                    output.extend_from_slice(&result[0]);
                    // 下一帧要求的输入长度可能变化（SincFixedIn 实际不变，但接口允许）。
                    *frames_needed = resampler.input_frames_next();
                }
                output
            }
            ResamplerInner::Linear { step, pos, buffer } => {
                buffer.extend_from_slice(input);
                // 输出个数 ≈ 还没插的那段输入 / step：先按这个留够，省掉增长时的重分配。
                let ready = (((buffer.len() as f64) - *pos).max(0.0) / *step).ceil() as usize + 1;
                let mut output = Vec::with_capacity(ready);
                while (*pos as usize + 1) < buffer.len() {
                    let i = *pos as usize;
                    let frac = (*pos - i as f64) as f32;
                    let a = buffer[i];
                    let b = buffer[i + 1];
                    output.push(a + (b - a) * frac);
                    *pos += *step;
                }
                // 收尾：一步之后 `pos` 可能整个跨出 `buffer` 之外（只有 `step > 2` 够得着
                // `len + 1`，即降采样倍数 > 2 的路径），那样 `done > len`，搬运就要越界。
                // 夹到末尾再搬——`done == len` 是合法的空区间，`pos` 的余量留到下一块。
                let done = (*pos as usize).min(buffer.len());
                buffer.copy_within(done.., 0);
                buffer.truncate(buffer.len() - done);
                *pos -= done as f64;
                output
            }
        }
    }

    /// 冲出尾巴：流结束时把内部缓冲里不够一帧的残余补零处理掉。
    pub fn flush(&mut self) -> Vec<f32> {
        match &mut self.inner {
            ResamplerInner::Passthrough => Vec::new(),
            ResamplerInner::Active {
                resampler, buffer, ..
            } => {
                if buffer.is_empty() {
                    return Vec::new();
                }
                // process_partial 会内部补零到所需帧长。
                let input: Vec<f32> = std::mem::take(buffer);
                let result = resampler
                    .process_partial(Some(&[&input]), None)
                    .expect("partial 处理不应失败");
                result[0].clone()
            }
            ResamplerInner::Linear { step, pos, buffer } => {
                let len = buffer.len();
                if len == 0 {
                    return Vec::new();
                }
                // 线性档没有要冲的滤波尾巴：残下的只是"最后一个输入样本还没有右邻居"
                // （不足一个输入样本）。右端点用**末样本保持**顶替，把这一段按输出率插完。
                let last = buffer[len - 1];
                let mut output = Vec::new();
                while (*pos as usize) < len {
                    let i = (*pos as usize).min(len - 1);
                    let frac = (*pos - i as f64) as f32;
                    let a = buffer[i];
                    let b = if i + 1 < len { buffer[i + 1] } else { last };
                    output.push(a + (b - a) * frac);
                    *pos += *step;
                }
                buffer.clear();
                *pos = 0.0;
                output
            }
        }
    }

    /// 复位内部状态，丢掉残余缓冲。新一轮录音开始时调用。
    pub fn reset(&mut self) {
        match &mut self.inner {
            ResamplerInner::Passthrough => {}
            ResamplerInner::Active {
                resampler,
                buffer,
                frames_needed,
                ..
            } => {
                resampler.reset();
                buffer.clear();
                *frames_needed = resampler.input_frames_next();
            }
            ResamplerInner::Linear { pos, buffer, .. } => {
                buffer.clear();
                *pos = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_rates_match() {
        let mut r = Resampler::new(48_000, 48_000);
        let input = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let output = r.process(&input);
        assert_eq!(output, input, "同率透传不该改变数据");
        assert_eq!(r.output_delay(), 0);
    }

    #[test]
    fn downsample_48k_to_16k_output_length_is_roughly_one_third() {
        let mut r = Resampler::new(48_000, 16_000);
        // 喂 4800 样本 = 100 ms @ 48 kHz，期望输出 ~1600 样本。
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.001).sin()).collect();
        let output = r.process(&input);
        let flush = r.flush();
        let total = output.len() + flush.len();
        // 允许 ±10% 的偏差（内部缓冲 + sinc 延迟）。
        let expected = 1600;
        assert!(
            total > expected * 85 / 100 && total < expected * 115 / 100,
            "48k→16k 输出应约为输入的 1/3，实际 {total} vs 期望 ~{expected}"
        );
    }

    #[test]
    fn streaming_odd_chunks_matches_single_big_chunk() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.002).sin()).collect();

        // 一次性处理。
        let mut r1 = Resampler::new(48_000, 24_000);
        let one_shot = r1.process(&input);
        let one_shot_flush = r1.flush();
        let total_one: usize = one_shot.len() + one_shot_flush.len();

        // 散碎块处理（故意选不整除的奇怪尺寸）。
        let mut r2 = Resampler::new(48_000, 24_000);
        let mut total_stream: usize = 0;
        let chunk_sizes = [137, 253, 91, 499, 311, 4800 - 137 - 253 - 91 - 499 - 311];
        let mut offset = 0;
        for &sz in &chunk_sizes {
            let out = r2.process(&input[offset..offset + sz]);
            total_stream += out.len();
            offset += sz;
        }
        total_stream += r2.flush().len();

        // 散碎块和一次性的输出总量应相差不超过几个样本（rubato 内部对齐导致）。
        let diff = (total_one as i64 - total_stream as i64).unsigned_abs();
        assert!(
            diff <= 5,
            "流式与一次性输出长度差异过大: one_shot={total_one}, stream={total_stream}"
        );
    }

    #[test]
    fn flush_drains_the_tail() {
        let mut r = Resampler::new(16_000, 48_000);
        // 只喂 100 样本，不够一帧，process 应返回空。
        let input = vec![0.5; 100];
        let out = r.process(&input);
        // 100 样本在 chunk_size=480 的 SincFixedIn 下不够一帧。
        // flush 应该把它们挤出来。
        let flushed = r.flush();
        let total = out.len() + flushed.len();
        assert!(total > 0, "flush 应该产出被缓冲的尾巴");
    }

    #[test]
    fn reset_clears_internal_state() {
        let mut r = Resampler::new(48_000, 16_000);
        let input = vec![0.1; 200];
        r.process(&input);
        r.reset();
        // reset 之后 flush 应该没有残留。
        let flushed = r.flush();
        assert!(flushed.is_empty(), "reset 后不应有残留");
    }

    #[test]
    fn cheap_upsampler_exists_only_for_upsampling() {
        // 升采样方向：有这一档。
        assert!(Resampler::cheap_upsampler(16_000, 48_000).is_some());
        assert!(Resampler::cheap_upsampler(24_000, 48_000).is_some());
        assert!(Resampler::cheap_upsampler(44_100, 48_000).is_some());
        // 降采样方向与同率：**构造不出来**（降采样需要抗混叠，同率是透传）——
        // 不是"构造出来但退化成 sinc"，是这一档在这条路上不存在。
        assert!(Resampler::cheap_upsampler(48_000, 16_000).is_none());
        assert!(Resampler::cheap_upsampler(48_000, 24_000).is_none());
        assert!(Resampler::cheap_upsampler(48_000, 48_000).is_none());
    }

    #[test]
    fn a_cheap_upsampler_hits_the_target_rate_within_a_few_samples() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.003).sin()).collect();

        // 一次性处理。
        let mut one = Resampler::cheap_upsampler(24_000, 48_000).expect("24k→48k 是升采样");
        let total_one = one.process(&input).len() + one.flush().len();

        // 散碎块处理（故意选不整除的奇怪尺寸）。
        let mut streamed = Resampler::cheap_upsampler(24_000, 48_000).expect("24k→48k 是升采样");
        let mut total_stream = 0usize;
        let chunk_sizes = [137, 253, 91, 499, 311, 4800 - 137 - 253 - 91 - 499 - 311];
        let mut offset = 0;
        for &sz in &chunk_sizes {
            total_stream += streamed.process(&input[offset..offset + sz]).len();
            offset += sz;
        }
        total_stream += streamed.flush().len();

        // 24k→48k 是 1:2，4800 个输入样本该出 ~9600 个。
        let expected = 9600;
        assert!(
            (expected * 98 / 100..=expected * 102 / 100).contains(&total_one),
            "便宜档 24k→48k 的输出应约为输入的 2 倍：{total_one} vs 期望 ~{expected}"
        );

        // 散碎块和一次性的输出总量应相差不超过几个样本（跟 sinc 档同口径的容差）。
        let diff = (total_one as i64 - total_stream as i64).unsigned_abs();
        assert!(
            diff <= 5,
            "流式与一次性输出长度差异过大: one_shot={total_one}, stream={total_stream}"
        );
    }

    #[test]
    fn flush_drains_the_cheap_path() {
        let mut r = Resampler::cheap_upsampler(24_000, 48_000).expect("24k→48k 是升采样");
        // 单个输入样本：插值要两个端点，`process` 一个输出都出不来。
        assert!(r.process(&[0.5]).is_empty(), "一个样本插不出输出");
        // `flush` 用末样本保持把这一段按输出率补完：一个 24k 样本在 48k 上是两个。
        let flushed = r.flush();
        assert_eq!(
            flushed.len(),
            2,
            "flush 应该把最后那一段冲出来：{flushed:?}"
        );
        assert!(flushed.iter().all(|&s| s == 0.5), "末样本保持：{flushed:?}");
        assert!(r.flush().is_empty(), "冲完一次就该空了");
    }

    #[test]
    fn reset_clears_the_cheap_path() {
        let mut r = Resampler::cheap_upsampler(24_000, 48_000).expect("24k→48k 是升采样");
        r.process(&[0.1; 200]);
        r.reset();
        assert!(r.flush().is_empty(), "reset 后不该有残留");
        // 复位之后从零开始：第一个输出就是输入的第一个样本。
        let out = r.process(&[0.25, 0.75]);
        assert_eq!(out.len(), 2, "两个输入样本出两个输出：{out:?}");
        assert_eq!(out[0], 0.25);
    }

    /// 便宜档在升采样方向上的代价是**输出带外的镜像**（见模块头），不在带内。
    /// 这条只钉"带内电平没掉"——**不给它任何抗混叠的承诺**。
    #[test]
    fn the_cheap_path_stays_linear_where_linear_is_better() {
        let hz = 1000.0f64;
        let n = 24_000; // 1 s @24k：播放侧真实的那一段
        let input: Vec<f32> = (0..n)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / 24_000.0).sin() as f32 * 0.5)
            .collect();

        // 取后半段（稳态），避开起振那一段。
        let rms = |out: &[f32]| -> f64 {
            let tail = &out[out.len() / 2..];
            (tail.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / tail.len() as f64).sqrt()
        };

        let mut cheap = Resampler::cheap_upsampler(24_000, 48_000).expect("24k→48k 是升采样");
        let mut cheap_out = cheap.process(&input);
        cheap_out.extend(cheap.flush());

        let mut sinc = Resampler::new(24_000, 48_000);
        let mut sinc_out = sinc.process(&input);
        sinc_out.extend(sinc.flush());

        let (a, b) = (rms(&cheap_out), rms(&sinc_out));
        let db = 20.0 * (a / b).log10();
        assert!(
            db.abs() <= 0.1,
            "便宜档与 sinc 的带内电平差应在 0.1 dB 内：{db} dB（线性 {a} / sinc {b}）"
        );
    }
}
