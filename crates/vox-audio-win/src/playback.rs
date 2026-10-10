//! 播放：`PlaybackSink` 的 Windows 实现。
//!
//! 目标设备通常是 `CABLE Input (VB-Audio Virtual Cable)`——译文送进虚拟声卡，
//! VRChat 那边把 `CABLE Output` 当麦克风用。
//!
//! 分工：
//! - `push`（流水线线程）负责重采样 + 单声道铺开 + 写环形缓冲，永不阻塞；
//! - 渲染线程只做一次定长复制，缓冲空了就补静音。
//!
//! 流一开就一直跑，没数据时输出静音。这样 VRChat 那边看到的是一个一直在线的麦，
//! 不会因为我们停流而出现设备闪断。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use vox_core::pipeline::ResampleFactory;
use vox_core::ports::Resample;
use vox_core::ports::{PlaybackSink, PlaybackStats, PortError, PortResult};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    IAudioClient, IAudioRenderClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

use crate::com::{ComGuard, OwnedHandle, WinContext};
use crate::devices;
use crate::rates::{choose_output_rate, RateChoice};
use crate::wave::{duplicate_mono, f32_to_bytes, float_format, parse_format, SampleKind};
use vox_dsp::ring::{should_warn, DropRing};

/// 环形缓冲的长度（秒，按设备率和声道数算）。
///
/// 云端 TTS 是突发到达的：一句话的音频可能几百毫秒就全到了，而播放只能按实时速度出。
/// 缓冲太小会一直丢（听感是断续），太大则打断说话时残留太多（已经用 flush 解决）。
/// 5 秒是折中：常见的一两句话能全放下，真正的长段落溢出时会打日志提醒。
const RING_SECONDS: usize = 5;

/// 渲染线程等事件的超时。超时只是回到循环顶部检查停止标志。
const RENDER_WAIT_MS: u32 = 200;

/// 打开设备的握手上限。
const OPEN_TIMEOUT: Duration = Duration::from_secs(8);

struct Shared {
    ring: DropRing,
    stop: AtomicBool,
    rendered_samples: AtomicU64,
    device_latency_ms: AtomicU64,
    wake: OwnedHandle,
}

impl Shared {
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        // SAFETY: 句柄由 OwnedHandle 持有；SetEvent 线程安全。
        unsafe {
            let _ = SetEvent(self.wake.raw());
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// 渲染线程报回来的实际格式。
#[derive(Debug, Clone, Copy)]
struct OpenReport {
    rate: u32,
    channels: u16,
}

/// Windows 播放端。
pub struct WinPlayback {
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
    resampler: Option<Box<dyn Resample>>,
    resample_factory: ResampleFactory,
    target_rate: u32,
    channels: u16,
    interleave: Vec<f32>,
    source_rate: u32,
}

impl WinPlayback {
    pub fn new(resample_factory: ResampleFactory) -> Self {
        Self {
            shared: None,
            thread: None,
            resampler: None,
            resample_factory,
            target_rate: 0,
            channels: 1,
            interleave: Vec::new(),
            source_rate: 0,
        }
    }
}

impl PlaybackSink for WinPlayback {
    fn open(&mut self, device: Option<&str>, source_rate: u32) -> PortResult<u32> {
        self.close();
        if source_rate == 0 {
            return Err(PortError::new(
                "the playback source sample rate cannot be 0",
            ));
        }

        // SAFETY: 手动复位事件，句柄交给 OwnedHandle 独占。
        let wake = unsafe { CreateEventW(None, true, false, None) }.ctx("创建播放停止事件失败")?;

        // 环形缓冲按最坏情况（48 kHz 立体声）一次建好，实际率确定后不重建。
        // 480_000 个槽 × 4 字节 = 1.83 MiB，换来的是设备率比 48k 低时容量只会更宽松，
        // 而且 open 里不用二次分配。设备是 44.1 kHz 立体声时这些槽相当于 5.4 秒。
        let shared = Arc::new(Shared {
            ring: DropRing::new(48_000 * 2 * RING_SECONDS),
            stop: AtomicBool::new(false),
            rendered_samples: AtomicU64::new(0),
            device_latency_ms: AtomicU64::new(0),
            wake: OwnedHandle::new(wake),
        });

        let (tx, rx) = mpsc::channel::<PortResult<OpenReport>>();
        let device = device.map(|s| s.to_string());
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("vox-playback".into())
            .spawn(move || render_thread(device, thread_shared, tx))
            .map_err(|e| PortError::new(format!("failed to spawn the playback thread: {e}")))?;

        let report = match rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(report)) => report,
            Ok(Err(e)) => {
                shared.request_stop();
                let _ = thread.join();
                return Err(e);
            }
            Err(RecvTimeoutError::Timeout) => {
                shared.request_stop();
                let _ = thread.join();
                return Err(PortError::new(
                    "opening the playback device timed out (not ready after 8 s)",
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                return Err(PortError::new("the playback thread exited unexpectedly"));
            }
        };

        self.channels = report.channels.max(1);
        self.source_rate = source_rate;
        self.target_rate = report.rate;
        self.resampler = Some((self.resample_factory)(source_rate, report.rate));
        self.shared = Some(shared);
        self.thread = Some(thread);
        Ok(report.rate)
    }

    fn push(&mut self, samples: &[f32]) {
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        if samples.is_empty() {
            return;
        }
        // 重采样和铺声道都在这条线程上做（流水线线程，允许分配和日志），
        // 渲染线程那边就只剩一次复制。
        if self.source_rate == self.target_rate {
            // 直通：直接用原始样本，不复制。
            let mono = samples;
            self.interleave.clear();
            duplicate_mono(mono, self.channels, &mut self.interleave);
        } else {
            let resampled = match self.resampler.as_mut() {
                Some(r) => r.process(samples),
                None => return,
            };
            self.interleave.clear();
            duplicate_mono(&resampled, self.channels, &mut self.interleave);
        };
        let dropped = shared.ring.write(&self.interleave);
        if dropped > 0 {
            let events = shared.ring.drop_events();
            if should_warn(events) {
                tracing::warn!(
                    "播放缓冲满了，已累计丢弃 {} 个样本（第 {} 次）",
                    shared.ring.dropped_samples(),
                    events
                );
            }
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
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
        self.interleave.clear();
    }

    fn close(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.request_stop();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.resampler = None;
        self.interleave.clear();
        self.channels = 1;
        self.source_rate = 0;
    }
}

impl Drop for WinPlayback {
    fn drop(&mut self) {
        self.close();
    }
}

/// 渲染线程主体。
fn render_thread(
    device: Option<String>,
    shared: Arc<Shared>,
    tx: mpsc::Sender<PortResult<OpenReport>>,
) {
    // 自己初始化 COM。
    let _com = match ComGuard::mta() {
        Ok(g) => g,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };

    let opened = match open_render(device.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };

    shared
        .device_latency_ms
        .store(opened.stream_latency_ms, Ordering::Release);

    let _ = tx.send(Ok(OpenReport {
        rate: opened.rate,
        channels: opened.channels,
    }));

    render_loop(&opened, &shared);
}

struct OpenRender {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: OwnedHandle,
    rate: u32,
    channels: u16,
    kind: SampleKind,
    block_align: usize,
    stream_latency_ms: u64,
    /// 设备缓冲的总帧数。渲染端可以放心用这个值——只有进程环回那个伪设备上
    /// `GetBufferSize` 才会给垃圾。
    buffer_frames: u32,
}

/// 打开输出设备并按探测顺序定采样率。
fn open_render(device_name: Option<&str>) -> PortResult<OpenRender> {
    let device = devices::find_device(devices::RENDER, device_name)?;
    // SAFETY: device 有效。
    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.ctx("打开输出设备失败")?;

    // SAFETY: GetMixFormat 返回 COM 分配的格式块，解析后立刻释放。
    let (mix, mix_bytes) = unsafe {
        let ptr = client.GetMixFormat().ctx("读输出设备混音格式失败")?;
        let parsed = parse_format(ptr);
        let byte_len = if ptr.is_null() {
            0
        } else {
            std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>()
                + (*ptr).cbSize as usize
        };
        let bytes = if byte_len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr.cast::<u8>(), byte_len).to_vec()
        };
        CoTaskMemFree(Some(ptr as *const _));
        (parsed?, bytes)
    };

    // 按 设备默认 → 24000 → 48000 → 44100 探。声道数跟着设备走，不去改它。
    let choice: RateChoice = choose_output_rate(mix.sample_rate, |rate| {
        let candidate = float_format(rate, mix.channels);
        // SAFETY: client 有效；格式头是本地结构体，调用期间有效；
        // 不要 closest match，只关心“行不行”。
        let hr =
            unsafe { client.IsFormatSupported(AUDCLNT_SHAREMODE_SHARED, &candidate.Format, None) };
        hr.is_ok() && hr.0 == 0
    });

    let format = float_format(choice.rate, mix.channels);
    // 先尝试 Windows 10+ 的最短共享周期。失败过的 client 不重复 Initialize，
    // 兼容路径会重新 Activate 一份全新的接口。
    let init = unsafe {
        crate::client::initialize_min_period(
            &client,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            &format.Format,
        )
    };

    let (client, rate, channels, kind, block_align) = match init {
        Ok(_period) => (
            client,
            choice.rate,
            mix.channels,
            SampleKind::F32,
            mix.channels as usize * SampleKind::F32.bytes(),
        ),
        Err(first) => {
            // 探测说行、Initialize 却不干的情况见过（某些蓝牙驱动）。
            // 退路：重新拿一个 client，按系统默认共享周期初始化设备混音格式。
            tracing::warn!(
                "输出设备不接受 {} Hz（HRESULT 0x{:08X}），退回混音格式 {} Hz",
                choice.rate,
                first.code().0 as u32,
                mix.sample_rate
            );
            // SAFETY: device 有效；失败过的 client 不能重复 Initialize，只能再要一个。
            let retry: IAudioClient =
                unsafe { device.Activate(CLSCTX_ALL, None) }.ctx("重开输出设备失败")?;
            let mix_ptr = mix_bytes
                .as_ptr()
                .cast::<windows::Win32::Media::Audio::WAVEFORMATEX>();
            // SAFETY: retry 是新的未初始化 client；mix_bytes 是 GetMixFormat 返回块的完整副本，
            // 在 Initialize 调用期间有效。用设备原生 PCM 格式能兼容 VB-CABLE 的普通 24-bit 端点。
            unsafe {
                crate::client::initialize_default_period(
                    &retry,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    mix_ptr,
                )
            }
            .ctx("初始化输出流失败")?;
            (
                retry,
                mix.sample_rate,
                mix.channels,
                mix.kind,
                mix.block_align,
            )
        }
    };

    // SAFETY: 自动复位事件，句柄交给 OwnedHandle 独占。
    let event = OwnedHandle::new(
        unsafe { CreateEventW(None, false, false, None) }.ctx("创建播放事件失败")?,
    );
    // SAFETY: client 已初始化；事件在 OpenRender 存活期间有效。
    unsafe { client.SetEventHandle(event.raw()) }.ctx("绑定播放事件失败")?;
    // SAFETY: client 已初始化，取渲染服务接口。
    let render: IAudioRenderClient = unsafe { client.GetService() }.ctx("获取播放渲染接口失败")?;
    // SAFETY: client 已初始化。
    let buffer_frames = unsafe { client.GetBufferSize() }.ctx("读播放缓冲大小失败")?;
    let stream_latency_ms = unsafe { client.GetStreamLatency() }
        .map(|hns| (hns.max(0) as u64).div_ceil(10_000))
        .unwrap_or(0);
    // SAFETY: 一切就绪。
    unsafe { client.Start() }.ctx("启动播放流失败")?;

    Ok(OpenRender {
        client,
        render,
        event,
        rate,
        channels,
        kind,
        block_align,
        stream_latency_ms,
        buffer_frames,
    })
}

/// 渲染循环。这是音频回调线程：只做复制，不分配、不加锁、不打日志。
fn render_loop(opened: &OpenRender, shared: &Shared) {
    let channels = opened.channels.max(1) as usize;
    // 一次最多要填满整个设备缓冲，先把暂存区备好，循环里再不分配。
    let mut scratch = vec![0.0f32; opened.buffer_frames as usize * channels];

    while !shared.stopping() {
        // SAFETY: 事件句柄有效；超时是预期路径。
        let wait = unsafe { WaitForSingleObject(opened.event.raw(), RENDER_WAIT_MS) };
        if shared.stopping() {
            break;
        }
        if wait == WAIT_TIMEOUT {
            continue;
        }
        if wait != WAIT_OBJECT_0 {
            break;
        }

        // SAFETY: client 有效；padding 是已排队但还没播的帧数。
        let padding = match unsafe { opened.client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(_) => break,
        };
        let available = opened.buffer_frames.saturating_sub(padding);
        if available == 0 {
            continue;
        }

        let want = (available as usize * channels).min(scratch.len());
        let slice = &mut scratch[..want];
        // 缓冲空的时候 read_into 会把剩下的位置补零，所以欠载自动变静音。
        let read = shared.ring.read_into(slice);
        if read > 0 {
            shared
                .rendered_samples
                .fetch_add(read as u64, Ordering::Release);
        }

        let frames = (slice.len() / channels) as u32;
        if frames == 0 {
            continue;
        }
        // SAFETY: frames <= available，符合 GetBuffer 的契约；
        // 返回的指针指向至少 frames * channels * 4 字节的可写内存，
        // 在 ReleaseBuffer 之前有效。
        let ptr = match unsafe { opened.render.GetBuffer(frames) } {
            Ok(p) => p,
            Err(_) => break,
        };
        if !ptr.is_null() {
            let byte_len = frames as usize * opened.block_align;
            // SAFETY: GetBuffer 返回至少 frames * nBlockAlign 字节，在 ReleaseBuffer 前有效。
            let target = unsafe { std::slice::from_raw_parts_mut(ptr, byte_len) };
            f32_to_bytes(slice, opened.kind, target);
        }
        // SAFETY: 与上面的 GetBuffer 一一配对。
        unsafe {
            let _ = opened.render.ReleaseBuffer(frames, 0);
        }
    }

    // SAFETY: client 有效；停流失败也无所谓，接口马上要释放了。
    unsafe {
        let _ = opened.client.Stop();
    }
}

#[cfg(test)]
pub(crate) fn test_resample_factory() -> ResampleFactory {
    Box::new(|_, _| Box::new(TestPassthrough) as Box<dyn Resample>)
}

/// 测试用透传重采样器：不做真正的重采样，只保证接口能跑通。
#[cfg(test)]
struct TestPassthrough;

#[cfg(test)]
impl Resample for TestPassthrough {
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        samples.to_vec()
    }
    fn flush(&mut self) -> Vec<f32> {
        Vec::new()
    }
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cable;

    /// 测试用透传重采样工厂：不做真正的重采样，只保证接口能跑通。
    #[test]
    fn push_before_open_is_ignored() {
        let mut p = WinPlayback::new(test_resample_factory());
        p.push(&[0.1, 0.2]);
        p.flush();
        p.close();
    }

    #[test]
    fn zero_source_rate_is_rejected() {
        let mut p = WinPlayback::new(test_resample_factory());
        let err = p.open(None, 0).unwrap_err();
        assert!(err.message.contains("不能是 0"), "{}", err.message);
    }

    #[test]
    fn unknown_device_fails_with_chinese_message() {
        let mut p = WinPlayback::new(test_resample_factory());
        let err = p.open(Some("不存在的输出设备 zzz"), 24_000).unwrap_err();
        assert!(err.message.contains("找不到"), "{}", err.message);
    }

    #[test]
    #[ignore = "需要真声卡（会向默认输出播 0.3 秒静音），手动跑：--ignored"]
    fn default_output_opens_and_accepts_push() {
        let mut p = WinPlayback::new(test_resample_factory());
        let rate = p.open(None, 24_000).unwrap();
        assert!(rate >= 8_000, "拿到的采样率是 {rate}");
        // 推 0.3 秒静音，不该阻塞也不该丢。
        for _ in 0..15 {
            p.push(&vec![0.0; 480]);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        p.flush();
        p.close();
    }

    #[test]
    #[ignore = "会从默认输出发出 0.5 秒轻微蜂鸣，手动跑：--ignored"]
    fn pushed_tone_comes_back_through_endpoint_loopback() {
        // 端到端：push 的样本真的走到了设备。用整机环回把默认输出录回来，
        // 检查有非静音数据。这是唯一能证明“环形缓冲 → 渲染线程 → 设备”整条链通了的办法。
        use std::sync::atomic::{AtomicU32, Ordering};
        use vox_core::ports::{CaptureSource, CaptureTarget};

        let loud = Arc::new(AtomicU32::new(0));
        let l = Arc::clone(&loud);
        let mut cap = crate::capture::EndpointLoopbackCapture::new();
        cap.start(
            &CaptureTarget::Microphone(None),
            20,
            Box::new(move |chunk| {
                if chunk.samples.iter().any(|s| s.abs() > 0.01) {
                    l.fetch_add(1, Ordering::Relaxed);
                }
            }),
        )
        .unwrap();

        let mut p = WinPlayback::new(test_resample_factory());
        p.open(None, 24_000).unwrap();
        // 24 kHz 单声道 440 Hz，振幅 0.05（很轻）。
        let tone: Vec<f32> = (0..12_000)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 24_000.0).sin() * 0.05)
            .collect();
        for block in tone.chunks(480) {
            p.push(block);
            std::thread::sleep(std::time::Duration::from_millis(18));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        p.close();
        cap.stop();

        assert!(
            loud.load(Ordering::Relaxed) >= 5,
            "环回只录到 {} 块非静音数据，说明播放链路没通（也可能默认输出被静音了）",
            loud.load(Ordering::Relaxed)
        );
    }

    #[test]
    #[ignore = "需要装好 VB-CABLE，手动跑：--ignored"]
    fn cable_input_opens_when_installed() {
        let _com = ComGuard::mta().unwrap();
        if !matches!(cable::detect(), cable::CableStatus::Installed) {
            eprintln!("跳过：这台机器没装 VB-CABLE");
            return;
        }
        let mut p = WinPlayback::new(test_resample_factory());
        let rate = p.open(Some(cable::RENDER_ENDPOINT_NAME), 24_000).unwrap();
        assert!(rate >= 8_000);
        p.push(&vec![0.0; 2400]);
        p.close();
    }
}
