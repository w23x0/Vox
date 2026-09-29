//! 手动验一遍 Linux 音频后端到底通不通。跑法：
//!
//! ```text
//! cargo run -p vox-audio-linux --example smoke                    # 只查设备和正在出声的程序
//! cargo run -p vox-audio-linux --example smoke -- app pw-cat 5    # 抓某程序的声音 5 秒，报 RMS
//! cargo run -p vox-audio-linux --example smoke -- mic 3           # 录默认麦克风 3 秒
//! cargo run -p vox-audio-linux --example smoke -- tone 3          # 往默认输出放 3 秒 440 Hz
//! cargo run -p vox-audio-linux --example smoke -- vmic 15         # 建虚拟麦，持续往里放音
//! ```
//!
//! **抓音是自证的**：`app` / `mic` 跑完会算峰值与 RMS，静音直接报 FAIL。
//! `vmic` 只做播放那一半——要验回环，另开一个终端录它的 monitor：
//!
//! ```text
//! pw-record /tmp/vmic.wav &          # 先起录音
//! pw-link vox_virtual_mic:monitor_FL pw-record:input_FL
//! pw-link vox_virtual_mic:monitor_FR pw-record:input_FR
//! ```
//!
//! 然后看 `/tmp/vmic.wav` 不是静音（`sox`/`ffmpeg` 都行；本仓库的验证脚本用的是
//! ffmpeg 的 `astats`）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vox_audio_linux::{LinuxCapture, LinuxDeviceRegistry, LinuxPlayback, VirtualSink};
use vox_core::pipeline::ResampleFactory;
use vox_core::ports::{CaptureSource, CaptureTarget, DeviceRegistry, PlaybackSink, Resample};
use vox_dsp::Resampler;

/// 内核推给播放汇的是 24 kHz 单声道（协议侧定的），这里照抄。
const TONE_RATE: u32 = 24_000;

/// `vox-dsp` 的 `Resampler` 没有实现内核的 `Resample`：端口 impl 归外壳
/// （见 `app/src-tauri/src/dsp.rs` 的说明），example 里套一层同样的 newtype。
struct ResamplerAdapter(Resampler);

impl Resample for ResamplerAdapter {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("devices");

    match mode {
        "devices" => devices(),
        "app" => {
            let binary = args.get(1).ok_or("用法：smoke -- app <程序名> [秒数]")?;
            let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
            capture(
                CaptureTarget::ProcessLoopback {
                    executable: binary.clone(),
                    include_tree: true,
                },
                seconds,
            )
        }
        "mic" => {
            let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
            capture(CaptureTarget::Microphone(None), seconds)
        }
        "tone" => {
            let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
            play(None, seconds)
        }
        "vmic" => {
            let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            virtual_mic(seconds)
        }
        other => {
            eprintln!("不认识的模式：{other}");
            Ok(())
        }
    }
}

fn devices() -> Result<(), Box<dyn std::error::Error>> {
    let registry = LinuxDeviceRegistry::new();
    println!("== 输入设备 ==");
    for device in registry.input_devices()? {
        println!(
            "  {}{}",
            if device.is_default { "* " } else { "  " },
            device.name
        );
    }
    println!("== 输出设备 ==");
    for device in registry.output_devices()? {
        println!(
            "  {}{}",
            if device.is_default { "* " } else { "  " },
            device.name
        );
    }
    println!("== 正在出声的程序 ==");
    for app in registry.audio_apps()? {
        println!(
            "  {}{}  executable={} pid={}",
            if app.active { "* " } else { "  " },
            app.display_name,
            app.executable,
            app.pid
        );
    }
    Ok(())
}

/// 抓一段时间音频，报峰值/RMS。静音 = FAIL。
fn capture(target: CaptureTarget, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let what = match &target {
        CaptureTarget::Microphone(name) => format!("麦克风 {}", name.clone().unwrap_or_default()),
        CaptureTarget::ProcessLoopback { executable, .. } => format!("程序 {executable}"),
        CaptureTarget::Net { pipe } => format!("网络管子 {pipe}（不归本采集实现，必然报错）"),
    };
    println!("抓 {what}，{seconds} 秒…");

    let peak_bits = Arc::new(AtomicU64::new(0));
    let sum_squares = Arc::new(AtomicU64::new(0));
    let count = Arc::new(AtomicU64::new(0));

    let mut source = LinuxCapture::new();
    let format = source.start(
        &target,
        20,
        Box::new({
            let peak_bits = Arc::clone(&peak_bits);
            let sum_squares = Arc::clone(&sum_squares);
            let count = Arc::clone(&count);
            move |chunk| {
                let mono = chunk.to_mono();
                let peak = mono.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
                let peak_bits_now = peak.to_bits() as u64;
                peak_bits.fetch_max(peak_bits_now, Ordering::Relaxed);
                let sum: f64 = mono.iter().map(|s| (*s as f64) * (*s as f64)).sum();
                sum_squares.fetch_add((sum * 1_000_000.0) as u64, Ordering::Relaxed);
                count.fetch_add(mono.len() as u64, Ordering::Relaxed);
            }
        }),
    )?;
    println!(
        "  协商到 {} Hz / {} 声道",
        format.sample_rate, format.channels
    );

    std::thread::sleep(Duration::from_secs(seconds));
    source.stop();

    let peak = f32::from_bits(peak_bits.load(Ordering::Relaxed) as u32);
    let samples = count.load(Ordering::Relaxed).max(1);
    let rms = ((sum_squares.load(Ordering::Relaxed) as f64 / 1_000_000.0) / samples as f64).sqrt();
    println!("  样本 {samples} 个，峰值 {peak:.4}，RMS {rms:.4}");
    if peak > 0.001 {
        println!("  结果：PASS（抓到真声音了）");
    } else {
        println!("  结果：FAIL（全程静音——目标没在出声，或者链路没连上）");
    }
    Ok(())
}

/// 往某个输出放一段 440 Hz 正弦。
fn play(device: Option<&str>, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let factory: ResampleFactory =
        Box::new(|from, to| Box::new(ResamplerAdapter(Resampler::new(from, to))));
    let mut sink = LinuxPlayback::new(factory);
    let rate = sink.open(device, TONE_RATE)?;
    println!("打开播放：设备率 {rate} Hz（我们请求 48 kHz，图负责转）");

    let block = (TONE_RATE as usize * 20) / 1000;
    let mut phase = 0.0f64;
    let step = std::f64::consts::TAU * 440.0 / TONE_RATE as f64;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let mut buffer = Vec::with_capacity(block);
        for _ in 0..block {
            buffer.push((phase.sin() * 0.3) as f32);
            phase += step;
            if phase >= std::f64::consts::TAU {
                phase -= std::f64::consts::TAU;
            }
        }
        sink.push(&buffer);
        std::thread::sleep(Duration::from_millis(20));
    }
    // 让缓冲里的尾巴放完再关。
    std::thread::sleep(Duration::from_millis(300));
    let stats = sink.stats();
    println!(
        "  排队 {} 样本 / 已渲染 {} 样本 / 丢弃 {} 样本 / 设备延迟 {} ms",
        stats.queued_samples,
        stats.rendered_samples,
        stats.dropped_samples,
        stats.device_latency_ms
    );
    sink.close();
    if stats.rendered_samples > 0 {
        println!("  结果：PASS（渲染线程真的把样本取走了）");
    } else {
        println!("  结果：FAIL（渲染线程一个样本都没取——流没跑起来）");
    }
    Ok(())
}

/// 建虚拟麦，然后**整段时间**都往里放音（方便另一头录 monitor 验回环）。
fn virtual_mic(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    if VirtualSink::exists()? {
        eprintln!("系统里已经有同名虚拟麦了，先关掉再跑");
        return Ok(());
    }
    let sink_node = VirtualSink::create()?;
    println!(
        "虚拟麦已建立（{}），接下来 {seconds} 秒一直往里放 440 Hz。",
        sink_node.node_name()
    );
    println!("另一头验回环：把它的 monitor 连到 pw-record 的输入端口上录一段（见文件头）。");

    let play_result = play(Some(sink_node.node_name()), seconds);
    sink_node.destroy();
    println!("已删除虚拟麦。");
    play_result
}
