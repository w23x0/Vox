//! Vox 算子 CPU 成本实测台。
//!
//! 只读依赖仓库两个 crate（vox-dsp / vox-core，path 依赖），不写仓库任何文件。
//! 编译：cargo build --release；跑：taskset -c 2 ./target/release/voxbench
//!
//! 口径：合成 30 秒确定性"语音样噪声"，按真实路径的块大小流式喂进去，
//! 用 std::time::Instant 计时，重复 5 次取中位数。报告 "ms每1秒音频" 与 "占单核百分比"。

use std::hint::black_box;
use std::time::Instant;

use vox_core::ports::AudioChunk;
use vox_dsp::chunk::Blocker;
use vox_dsp::ring::DropRing;
use vox_dsp::{Denoiser, Resampler};

/// 每次测量的合成音频时长（秒）。
const AUDIO_SECS: usize = 30;
/// 每个算子的重复次数（取中位数）。
const REPS: usize = 5;

// --- 合成输入 --------------------------------------------------------------

/// 确定性 xorshift64，保证每一轮跑的输入完全一致。
struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

/// 语音样噪声：220 Hz 基音 + 5 个谐波（1/k 衰减）+ 2.5 Hz 音节包络 + 白噪底。
fn synth(rate: u32, secs: usize) -> Vec<f32> {
    let n = rate as usize * secs;
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let env = 0.5 + 0.5 * (std::f64::consts::TAU * 2.5 * t).sin();
        let mut voiced = 0.0f64;
        for k in 1..=5u32 {
            voiced += (std::f64::consts::TAU * 220.0 * k as f64 * t).sin() / k as f64;
        }
        let noise = rng.next_f32();
        out.push((voiced as f32 * 0.18 * env as f32 + noise * 0.03).clamp(-1.0, 1.0));
    }
    out
}

/// 交织立体声：左右差 2%，这样下混不是"抄一列"。
fn synth_stereo(rate: u32, secs: usize) -> Vec<f32> {
    let mono = synth(rate, secs);
    let mut out = Vec::with_capacity(mono.len() * 2);
    for s in mono {
        out.push(s);
        out.push(s * 0.98);
    }
    out
}

// --- 计时 ----------------------------------------------------------------

struct Row {
    name: String,
    secs: f64,
    /// 每次重复的总毫秒数
    runs: Vec<f64>,
}

impl Row {
    fn median(&self) -> f64 {
        let mut v = self.runs.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }
}

/// `prep` 在计时区间之外构造算子状态（构造开销不该算进"处理 1 秒音频"），
/// `body` 是计时区间内真正跑的处理。
fn measure<T>(name: &str, audio_secs: f64, mut prep: impl FnMut() -> T, mut body: impl FnMut(&mut T)) -> Row {
    {
        let mut warm = prep();
        body(&mut warm);
    }
    let mut runs = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let mut st = prep();
        let t0 = Instant::now();
        body(&mut st);
        runs.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    Row {
        name: name.to_string(),
        secs: audio_secs,
        runs,
    }
}

// --- 对照用：线性插值重采样（自写 ~25 行，无抗混叠滤波） --------------------

struct Linear {
    /// 每个输出样本在输入里前进多少（= in_rate / out_rate）
    step: f64,
    pos: f64,
    buf: Vec<f32>,
}

impl Linear {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            buf: Vec::new(),
        }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        while (self.pos as usize + 1) < self.buf.len() {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = self.buf[i];
            let b = self.buf[i + 1];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        let done = self.pos as usize;
        self.buf.copy_within(done.., 0);
        self.buf.truncate(self.buf.len() - done);
        self.pos -= done as f64;
        out
    }
}

// --- 输入准备 --------------------------------------------------------------

fn chunks(interleaved: &[f32], ch: u16, per_block: usize, rate: u32) -> Vec<AudioChunk> {
    interleaved
        .chunks(per_block)
        .filter(|c| c.len() == per_block)
        .map(|c| AudioChunk {
            samples: c.to_vec(),
            sample_rate: rate,
            channels: ch,
        })
        .collect()
}

fn main() {
    let t_start = Instant::now();

    // 采集侧（平台外壳请求 48 kHz 立体声，内核 INPUT_BLOCK_MS = 20 ms）
    let stereo48 = synth_stereo(48_000, AUDIO_SECS);
    let mono48 = synth(48_000, AUDIO_SECS); // 降噪 / 48k→16k 的输入
    let mono16 = synth(16_000, AUDIO_SECS); // 16k→48k 的输入
    let mono24 = synth(24_000, AUDIO_SECS); // 播放侧真路径：TTS 24k→设备 48k

    let c48_stereo = chunks(&stereo48, 2, 1920, 48_000); // 20 ms @48k 立体声
    let c48_mono: Vec<AudioChunk> = mono48
        .chunks(960)
        .map(|c| AudioChunk {
            samples: c.to_vec(),
            sample_rate: 48_000,
            channels: 1,
        })
        .collect();
    let b48: Vec<&[f32]> = mono48.chunks(960).collect(); // 20 ms @48k 单声道
    let b16: Vec<&[f32]> = mono16.chunks(320).collect(); // 20 ms @16k 单声道
    let b24: Vec<&[f32]> = mono24.chunks(480).collect(); // 20 ms @24k 单声道

    let mut rows: Vec<Row> = Vec::new();

    // 1) 单声道下混（AudioChunk::to_mono，2ch→1ch）
    rows.push(measure("下混 2ch→1ch (vox-core AudioChunk::to_mono)", AUDIO_SECS as f64, || (), |_| {
        let mut n = 0usize;
        for c in &c48_stereo {
            n += black_box(c.to_mono()).len();
        }
        black_box(n);
    }));

    // 1b) 单声道透传下混（channels == 1 时是整块 clone）
    rows.push(measure("下混 1ch→1ch (同函数，走 clone 分支)", AUDIO_SECS as f64, || (), |_| {
        let mut n = 0usize;
        for c in &c48_mono {
            n += black_box(c.to_mono()).len();
        }
        black_box(n);
    }));

    // 2) RNNoise 降噪（vox-dsp::Denoiser，48k / 480 帧）
    let den = measure(
        "RNNoise 降噪 (vox-dsp::Denoiser, 48k)",
        AUDIO_SECS as f64,
        || Denoiser::new().expect("Denoiser::new 纯内存分配"),
        |d| {
        let mut n = 0usize;
        for b in &b48 {
            n += black_box(d.process(b)).len();
        }
        black_box(n);
    });
    rows.push(den);

    // 3) rubato sinc 重采样 48k→16k
    rows.push(measure("rubato sinc 48k→16k (vox-dsp::Resampler)", AUDIO_SECS as f64,
        || Resampler::new(48_000, 16_000), |r| {
            let mut n = 0usize;
            for b in &b48 {
                n += black_box(r.process(b)).len();
            }
            n += black_box(r.flush()).len();
            black_box(n);
        }));

    // 3b) rubato sinc 16k→48k
    rows.push(measure("rubato sinc 16k→48k (vox-dsp::Resampler)", AUDIO_SECS as f64,
        || Resampler::new(16_000, 48_000), |r| {
            let mut n = 0usize;
            for b in &b16 {
                n += black_box(r.process(b)).len();
            }
            n += black_box(r.flush()).len();
            black_box(n);
        }));

    // 3c) rubato sinc 24k→48k（播放侧真路径）
    rows.push(measure("rubato sinc 24k→48k (播放侧真路径)", AUDIO_SECS as f64,
        || Resampler::new(24_000, 48_000), |r| {
            let mut n = 0usize;
            for b in &b24 {
                n += black_box(r.process(b)).len();
            }
            n += black_box(r.flush()).len();
            black_box(n);
        }));

    // 4) 自写线性插值对照
    rows.push(measure("线性插值 48k→16k (自写对照)", AUDIO_SECS as f64,
        || Linear::new(48_000, 16_000), |r| {
            let mut n = 0usize;
            for b in &b48 {
                n += black_box(r.process(b)).len();
            }
            black_box(n);
        }));
    rows.push(measure("线性插值 16k→48k (自写对照)", AUDIO_SECS as f64,
        || Linear::new(16_000, 48_000), |r| {
            let mut n = 0usize;
            for b in &b16 {
                n += black_box(r.process(b)).len();
            }
            black_box(n);
        }));

    // 5) chunker：Blocker(48k, 2ch, 20ms) 吃 PipeWire 1024 帧（2048 样本）的量子
    rows.push(measure("Blocker 切块 (48k/2ch/20ms, 喂 2048 样本)", AUDIO_SECS as f64,
        || Blocker::new(48_000, 2, 20), |blk| {
            let mut blocks = 0usize;
            let mut sink = |c: AudioChunk| {
                blocks += c.samples.len();
            };
            for c in stereo48.chunks(2048) {
                blk.feed(c, &mut sink);
            }
            black_box(blocks);
        }));

    // 5b) 对齐情形：设备正好给 20 ms
    rows.push(measure("Blocker 切块 (同上，喂 1920 样本=对齐)", AUDIO_SECS as f64,
        || Blocker::new(48_000, 2, 20), |blk| {
            let mut blocks = 0usize;
            let mut sink = |c: AudioChunk| {
                blocks += c.samples.len();
            };
            for c in stereo48.chunks(1920) {
                blk.feed(c, &mut sink);
            }
            black_box(blocks);
        }));

    // 5c) 环形缓冲：写 20 ms 立体声 + 读两个 10 ms（写读配平，模拟渲染回调按 10 ms 量子取数）
    rows.push(measure("DropRing 写+读 (48k/2ch, 5s 容量)", AUDIO_SECS as f64,
        || DropRing::new(48_000 * 2 * 5), |ring| {
            let mut out = [0.0f32; 960]; // 10 ms @48k 立体声
            let mut total = 0usize;
            for c in &c48_stereo {
                total += ring.write(&c.samples);
                total += ring.read_into(&mut out);
                total += ring.read_into(&mut out);
            }
            black_box((total, ring.dropped_samples()));
        }));

    // 6) 播放侧铺声道：单声道 20 ms → 立体声
    rows.push(measure("duplicate_mono 1ch→2ch (播放侧铺声道)", AUDIO_SECS as f64,
        || Vec::<f32>::with_capacity(1920), |interleave| {
            for b in &b48 {
                interleave.clear();
                vox_dsp::channels::duplicate_mono(b, 2, interleave);
                black_box(interleave.len());
            }
        }));

    // --- 对照自检：各重采样器产出样本数应接近，否则比的是不同的工作量 ---
    {
        let mut r1 = Resampler::new(48_000, 16_000);
        let n1: usize = b48.iter().map(|b| r1.process(b).len()).sum::<usize>() + r1.flush().len();
        let mut r2 = Resampler::new(16_000, 48_000);
        let n2: usize = b16.iter().map(|b| r2.process(b).len()).sum::<usize>() + r2.flush().len();
        let mut l1 = Linear::new(48_000, 16_000);
        let m1: usize = b48.iter().map(|b| l1.process(b).len()).sum();
        let mut l2 = Linear::new(16_000, 48_000);
        let m2: usize = b16.iter().map(|b| l2.process(b).len()).sum();
        println!(
            "自检 产出样本数（30 s 输入）：rubato 48k→16k = {n1}（期望 ~{}），线性 48k→16k = {m1}；rubato 16k→48k = {n2}（期望 ~{}），线性 16k→48k = {m2}\n",
            16_000 * AUDIO_SECS,
            48_000 * AUDIO_SECS
        );

        // 工作量核对：确认每个算子真的处理了预期样本数（防"空跑得低分"）。
        let mono_in: usize = b48.iter().map(|b| b.len()).sum();
        let mut d = Denoiser::new().unwrap();
        let den_out: usize = b48.iter().map(|b| d.process(b).len()).sum();
        let dm_in: usize = c48_stereo.iter().map(|c| c.samples.len()).sum();
        let dm_out: usize = c48_stereo.iter().map(|c| c.to_mono().len()).sum();
        let ring = DropRing::new(48_000 * 2 * 5);
        let (mut w, mut r) = (0usize, 0usize);
        let mut buf = [0.0f32; 960];
        for c in &c48_stereo {
            w += ring.write(&c.samples);
            r += ring.read_into(&mut buf);
            r += ring.read_into(&mut buf);
        }
        let mut blk = Blocker::new(48_000, 2, 20);
        let mut fed = 0usize;
        let mut emitted = 0usize;
        let mut sink = |c: AudioChunk| emitted += c.samples.len();
        for c in stereo48.chunks(2048) {
            fed += c.len();
            blk.feed(c, &mut sink);
        }
        println!(
            "工作量核对：降噪输入 {mono_in} → 输出 {den_out} 样本（3000 帧，扣首帧期望 {}）；下混 {dm_in} → {dm_out} 样本；环缓冲写端丢弃 {w} 样本 / 读 {r} 样本（一写两读配平，未溢出；第二次读走的是补静音分支）；Blocker 喂 {fed} → 出 {emitted} 样本",
            mono_in - 480
        );
    }

    // --- 输出 -------------------------------------------------------------
    println!("## 原始测量（每格 = 一轮 30 s 音频的总毫秒数）\n");
    for r in &rows {
        let ms: Vec<String> = r.runs.iter().map(|v| format!("{v:.2}")).collect();
        println!(
            "{} | 中位 {:.2} ms | 全部 [{}]",
            r.name,
            r.median(),
            ms.join(", ")
        );
    }

    println!("\n## 表格\n");
    println!("| 算子 | ms / 1s 音频 | % 单核 | 备注 |");
    println!("|---|---|---|---|");
    for r in &rows {
        let med = r.median();
        let per_s = med / r.secs;
        let min = r.runs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = r.runs.iter().cloned().fold(0.0f64, f64::max);
        println!(
            "| {} | {:.3} | {:.3}% | 中位 {:.2} ms / {} s；极值 {:.2}–{:.2} ms |",
            r.name,
            per_s,
            per_s / 10.0,
            med,
            r.secs,
            min,
            max
        );
    }

    // 采集路径合账
    let pick = |name: &str| -> f64 {
        rows.iter()
            .find(|r| r.name.starts_with(name))
            .map(|r| r.median() / r.secs)
            .unwrap_or(f64::NAN)
    };
    let capture = pick("下混 2ch→1ch") + pick("RNNoise") + pick("rubato sinc 48k→16k") + pick("Blocker 切块 (48k/2ch/20ms, 喂 2048");
    let playback = pick("rubato sinc 24k→48k") + pick("duplicate_mono") + pick("DropRing");
    println!("\n采集路径（下混 + 降噪 + 48k→16k + 切块）合计: {capture:.3} ms / 1s 音频 = 单核 {:.3}%", capture / 10.0);
    println!("播放路径（24k→48k + 铺声道 + 环缓冲读写）合计: {playback:.3} ms / 1s 音频 = 单核 {:.3}%", playback / 10.0);
    println!("\n总耗时 {:.2} s（含合成输入与预热）", t_start.elapsed().as_secs_f64());
}
