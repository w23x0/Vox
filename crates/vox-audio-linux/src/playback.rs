//! `PlaybackSink` 的 Linux 实现：PipeWire 输出流 + 无锁环缓冲。
//!
//! 分工跟 Windows 侧一模一样（`crates/vox-audio-win/src/playback.rs`）：
//! - **流水线线程**做重采样和铺声道（允许分配），写完塞进 `DropRing`；
//! - **PipeWire 的 process 回调**只做一次定长复制，不分配、不加锁、不打日志；
//! - 环里没数据就补静音（欠载不报错，语音流断一下比崩掉强）。
//!
//! 跟 Windows 的差别只有一处：设备率不用探测。我们对图请求 **48 kHz 立体声 f32**，
//! PipeWire 自己在图里转成设备真正要的格式（`rates.rs` 那套探测顺序在 Linux 上多余）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use vox_core::pipeline::ResampleFactory;
use vox_core::ports::{PlaybackSink, PlaybackStats, PortError, PortResult, Resample};
use vox_dsp::channels::duplicate_mono;
use vox_dsp::ring::{should_warn, DropRing};

use crate::probe::{
    self, as_f32_slice_mut, attach_quit, map_err, Cmd, REQUEST_CHANNELS, REQUEST_RATE,
};

/// 环缓冲容量：5 秒 48 kHz 立体声（跟 Windows 侧同一个量级）。
const RING_SECONDS: usize = 5;
/// 等流进入 Streaming 的上限。超时就报错，不留僵尸线程。
const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
/// 流在会话里的名字（`wpctl status` 里看得到）。
const NODE_NAME: &str = "Vox 播放";

/// 跨线程共享的那点状态。
struct Shared {
    ring: DropRing,
    stop: AtomicBool,
    rendered_samples: AtomicU64,
    device_latency_ms: AtomicU64,
}

pub struct LinuxPlayback {
    resample_factory: ResampleFactory,
    source_rate: u32,
    target_rate: u32,
    channels: u16,
    resampler: Option<Box<dyn Resample>>,
    /// 铺声道用的缓冲，复用避免每次 `push` 都分配。
    interleave: Vec<f32>,
    shared: Option<Arc<Shared>>,
    cmd: Option<pw::channel::Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
}

impl LinuxPlayback {
    pub fn new(resample_factory: ResampleFactory) -> Self {
        Self {
            resample_factory,
            source_rate: 0,
            target_rate: 0,
            channels: 1,
            resampler: None,
            interleave: Vec::new(),
            shared: None,
            cmd: None,
            thread: None,
        }
    }
}

impl PlaybackSink for LinuxPlayback {
    fn open(&mut self, device: Option<&str>, source_rate: u32) -> PortResult<u32> {
        self.close();
        if source_rate == 0 {
            return Err(PortError::new("播放源采样率不能是 0"));
        }

        let shared = Arc::new(Shared {
            ring: DropRing::new(REQUEST_RATE as usize * REQUEST_CHANNELS as usize * RING_SECONDS),
            stop: AtomicBool::new(false),
            rendered_samples: AtomicU64::new(0),
            device_latency_ms: AtomicU64::new(0),
        });

        let (report_tx, report_rx) = mpsc::channel::<PortResult<(u32, u16)>>();
        let (cmd_tx, cmd_rx) = pw::channel::channel::<Cmd>();

        let thread_shared = Arc::clone(&shared);
        let device = device.map(|name| name.to_owned());
        let thread = std::thread::Builder::new()
            .name("vox-playback".into())
            .spawn(move || stream_thread(device, thread_shared, report_tx, cmd_rx))
            .map_err(|e| PortError::new(format!("创建播放线程失败：{e}")))?;

        let (rate, channels) = match report_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(report)) => report,
            Ok(Err(e)) => {
                shared.stop.store(true, Ordering::Release);
                let _ = cmd_tx.send(Cmd::Quit);
                let _ = thread.join();
                return Err(e);
            }
            Err(RecvTimeoutError::Timeout) => {
                shared.stop.store(true, Ordering::Release);
                let _ = cmd_tx.send(Cmd::Quit);
                let _ = thread.join();
                return Err(PortError::new(format!(
                    "打开播放设备超时（等了 {} 秒还没就绪）",
                    OPEN_TIMEOUT.as_secs()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                return Err(PortError::new("播放线程意外退出"));
            }
        };

        self.channels = channels.max(1);
        self.source_rate = source_rate;
        self.target_rate = rate;
        self.resampler = Some((self.resample_factory)(source_rate, rate));
        self.shared = Some(shared);
        self.cmd = Some(cmd_tx);
        self.thread = Some(thread);
        Ok(rate)
    }

    fn push(&mut self, samples: &[f32]) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        if samples.is_empty() {
            return;
        }
        // 重采样与铺声道在流水线线程上做（允许分配、允许日志）。
        self.interleave.clear();
        if self.source_rate == self.target_rate {
            duplicate_mono(samples, self.channels, &mut self.interleave);
        } else {
            let Some(resampler) = self.resampler.as_mut() else {
                return;
            };
            let resampled = resampler.process(samples);
            duplicate_mono(&resampled, self.channels, &mut self.interleave);
        }

        let dropped = shared.ring.write(&self.interleave);
        if dropped > 0 && should_warn(shared.ring.drop_events()) {
            tracing::warn!(
                "播放缓冲满了，已累计丢弃 {} 个样本（第 {} 次）",
                shared.ring.dropped_samples(),
                shared.ring.drop_events()
            );
        }
    }

    fn stats(&self) -> PlaybackStats {
        let Some(shared) = self.shared.as_ref() else {
            return PlaybackStats::default();
        };
        PlaybackStats {
            queued_samples: shared.ring.len(),
            sample_rate: self.target_rate.max(1),
            channels: self.channels,
            rendered_samples: shared.rendered_samples.load(Ordering::Acquire),
            dropped_samples: shared.ring.dropped_samples(),
            device_latency_ms: shared.device_latency_ms.load(Ordering::Acquire),
        }
    }

    fn flush(&mut self) {
        if let Some(shared) = self.shared.as_ref() {
            shared.ring.clear();
        }
        // 重采样器的跨块状态也要清，否则残留尾巴会接到下一句开头。
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        self.interleave.clear();
    }

    fn close(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.stop.store(true, Ordering::Release);
        }
        if let Some(cmd) = self.cmd.take() {
            let _ = cmd.send(Cmd::Quit);
        }
        if let Some(thread) = self.thread.take() {
            // 流线程收到 Quit 就会退出主循环，join 不会挂住。
            let _ = thread.join();
        }
        self.resampler = None;
        self.interleave.clear();
    }
}

impl Drop for LinuxPlayback {
    fn drop(&mut self) {
        self.close();
    }
}

/// 流线程：建流 → 连流 → 等协商 → 跑主循环。
fn stream_thread(
    device: Option<String>,
    shared: Arc<Shared>,
    report: mpsc::Sender<PortResult<(u32, u16)>>,
    cmd_rx: pw::channel::Receiver<Cmd>,
) {
    let mut reported = false;
    let mut report = Some(report);

    let result = (|| -> PortResult<()> {
        probe::init();
        let (main_loop, core) = probe::connect()?;
        let _attached = attach_quit(&main_loop, cmd_rx);

        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => NODE_NAME,
        };
        if let Some(device) = device.as_deref() {
            // `target.object`：pipewire-rs 把它挂在 `v0_3_44` feature 后面，而我们的
            // 最低要求（PipeWire 1.0+）远高于它——不值得为一个键名开 feature 开关。
            props.insert("target.object", device);
        }

        let stream = pw::stream::StreamBox::new(&core, NODE_NAME, props).map_err(map_err)?;

        // 协商结果（`param_changed` 里解析）与回报通道一起放进 user data。
        struct UserData {
            shared: Arc<Shared>,
            report: Option<mpsc::Sender<PortResult<(u32, u16)>>>,
            negotiated: Option<(u32, u16)>,
            format: spa::param::audio::AudioInfoRaw,
        }

        let _listener = stream
            .add_local_listener_with_user_data(UserData {
                shared: Arc::clone(&shared),
                report: report.take(),
                negotiated: None,
                format: spa::param::audio::AudioInfoRaw::new(),
            })
            .param_changed(|_stream, data, id, param| {
                // 只认 Format 参数；服务端协商出来的实际格式在这里。
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                if data.format.parse(param).is_err() {
                    return;
                }
                let rate = data.format.rate();
                let channels = data.format.channels() as u16;
                if rate == 0 || channels == 0 {
                    return;
                }
                data.negotiated = Some((rate, channels));
                if let Some(report) = data.report.take() {
                    let _ = report.send(Ok((rate, channels)));
                }
            })
            .state_changed(|_stream, data, _old, new| {
                // 协商还没回来就进入 Streaming，说明服务端给了默认格式；
                // 这时候回报一个"请求值"，让上层能开工而不是干等超时。
                if new == pw::stream::StreamState::Streaming && data.negotiated.is_none() {
                    data.negotiated = Some((REQUEST_RATE, REQUEST_CHANNELS));
                    if let Some(report) = data.report.take() {
                        let _ = report.send(Ok((REQUEST_RATE, REQUEST_CHANNELS)));
                    }
                }
                if let pw::stream::StreamState::Error(message) = new {
                    if let Some(report) = data.report.take() {
                        let _ = report.send(Err(PortError::new(format!("播放流出错：{message}"))));
                    }
                }
            })
            .process(|stream, data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data_buf) = datas.first_mut() else {
                    return;
                };
                let channels = data.negotiated.map(|(_, c)| c).unwrap_or(REQUEST_CHANNELS);
                let stride = std::mem::size_of::<f32>() * channels as usize;

                let filled = match data_buf.data() {
                    Some(slice) => {
                        let frames = slice.len() / stride;
                        // 欠载时剩下的由 `read_into` 补静音（不报错：语音流断一下比崩掉强），
                        // 这里只记数。
                        let out = as_f32_slice_mut(slice);
                        let read = data.shared.ring.read_into(out);
                        data.shared
                            .rendered_samples
                            .fetch_add(read as u64, Ordering::Release);
                        frames
                    }
                    None => 0,
                };

                // 流延迟（毫秒）直接问 PipeWire 要，给内核做延迟统计。
                if let Ok(time) = stream.time() {
                    let rate = data
                        .negotiated
                        .map(|(r, _)| r)
                        .unwrap_or(REQUEST_RATE)
                        .max(1);
                    let delay = time.delay().max(0) as u64;
                    let ms = delay.saturating_mul(1000) / rate as u64;
                    data.shared.device_latency_ms.store(ms, Ordering::Release);
                }

                let chunk = data_buf.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = stride as _;
                *chunk.size_mut() = (stride * filled) as _;
            })
            .register()
            .map_err(map_err)?;

        // 请求 48 kHz 立体声 f32；图里谁要别的格式由 PipeWire 转。
        probe::connect_request_format(
            &stream,
            spa::utils::Direction::Output,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
        )?;

        reported = true;
        main_loop.run();
        Ok(())
    })();

    // 建流过程中出错，而且还没回报过 → 把错误交给 open()。
    if let Err(e) = result {
        if !reported {
            if let Some(report) = report.take() {
                let _ = report.send(Err(e));
            }
        } else {
            tracing::error!("播放流线程退出：{e}");
        }
    }
}
