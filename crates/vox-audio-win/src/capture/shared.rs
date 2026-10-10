//! 采集线程的公共部分：事件循环、分块、启动握手。
//!
//! 麦克风、进程环回、整机环回三条路只有“怎么拿到 IAudioClient”不一样，
//! 拿到之后的循环完全相同，所以循环写在这里。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use vox_dsp::chunk::Blocker;

use vox_core::ports::{AudioChunk, CaptureFormat, PortError, PortResult};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

use crate::com::{OwnedHandle, WinContext};
use crate::devices;
use crate::wave::{bytes_to_f32, parse_format, WaveInfo};

/// 等事件的超时。
///
/// 目标软件一声不出时进程环回**根本不会给帧**，所以必须超时轮转，
/// 把 WAIT_TIMEOUT 当成"正常安静"而不是错误、不是掉线。麦克风路径也用同一个值：
/// 设备被拔掉时事件同样不再触发，靠超时才能回到循环顶部检查停止标志。
pub(crate) const WAIT_TIMEOUT_MS: u32 = 250;

/// 打开的采集流。全部接口都属于调用它的那个线程。
pub(crate) struct OpenCapture {
    pub(crate) client: IAudioClient,
    pub(crate) capture: IAudioCaptureClient,
    pub(crate) event: OwnedHandle,
    pub(crate) info: WaveInfo,
}

impl OpenCapture {
    pub(crate) fn format(&self) -> CaptureFormat {
        CaptureFormat {
            sample_rate: self.info.sample_rate,
            channels: self.info.channels,
        }
    }
}

/// 共用的 WASAPI 采集初始化：找设备、激活、GetMixFormat、Initialize（先试
/// 最短周期再退默认）、建事件、SetEventHandle、GetService、Start。麦克风和
/// 整机环回只有 flow 方向和 stream flags 不同，其余完全一样。
///
/// `what` 是错误信息里的设备描述（如 "麦克风"、"输出设备"），拼进各步骤的
/// 中文错误文案，排查时能一眼看出是哪条路炸了。
pub(crate) fn open_capture_client(
    flow: windows::Win32::Media::Audio::EDataFlow,
    device_name: Option<&str>,
    stream_flags: u32,
    what: &str,
) -> PortResult<OpenCapture> {
    use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};

    let device = devices::find_device(flow, device_name)?;
    // SAFETY: device 有效；Activate 只创建 IAudioClient，不带激活参数。
    let candidate: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.ctx(&format!("failed to open the {what}"))?;

    // SAFETY: GetMixFormat 返回 COM 分配的格式块，解析完立刻释放；
    // Initialize 期间指针必须有效，所以释放放在 Initialize 之后。
    let (client, info) = unsafe {
        let mix = candidate
            .GetMixFormat()
            .ctx(&format!("failed to read the {what} mix format"))?;
        let initialized = (|| -> PortResult<(IAudioClient, WaveInfo)> {
            let parsed = parse_format(mix)?;
            let client = match crate::client::initialize_min_period(&candidate, stream_flags, mix) {
                Ok(period) => {
                    tracing::debug!(period_frames = period, "{what}使用 WASAPI 最短共享周期");
                    candidate
                }
                Err(error) => {
                    tracing::debug!(error = %error, "{what}最短共享周期不可用，退回系统默认周期");
                    let fallback: IAudioClient = device
                        .Activate(CLSCTX_ALL, None)
                        .ctx(&format!("failed to reopen the {what}"))?;
                    crate::client::initialize_default_period(&fallback, stream_flags, mix)
                        .ctx(&format!("failed to initialize the {what} stream"))?;
                    fallback
                }
            };
            Ok((client, parsed))
        })();
        CoTaskMemFree(Some(mix as *const _));
        initialized?
    };

    let event = create_stream_event()?;
    // SAFETY: client 已初始化；事件句柄在 OpenCapture 存活期间一直有效，
    // 而 client 会先于它被丢弃（结构体字段声明顺序：client 在 event 之前）。
    unsafe { client.SetEventHandle(event.raw()) }
        .ctx(&format!("failed to bind the {what} event"))?;
    // SAFETY: client 已初始化，取采集服务接口。
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .ctx(&format!("failed to get the {what} capture interface"))?;
    // SAFETY: 一切就绪，开始走流。
    unsafe { client.Start() }.ctx(&format!("failed to start the {what} stream"))?;

    Ok(OpenCapture {
        client,
        capture,
        event,
        info,
    })
}

/// 启动握手的等待上限。COM 激活和设备初始化偶尔会卡几百毫秒，给足余量。
const START_TIMEOUT: Duration = Duration::from_secs(8);

/// 采集线程共享的控制块。
pub(crate) struct CaptureControl {
    pub(crate) stop: AtomicBool,
    /// 停止时也 SetEvent 一下，免得白等一个超时周期。
    pub(crate) wake: OwnedHandle,
}

impl CaptureControl {
    pub(crate) fn new() -> PortResult<Arc<Self>> {
        // SAFETY: 建一个无名的手动复位事件；句柄交给 OwnedHandle 独占，Drop 时关闭。
        let handle = unsafe { CreateEventW(None, true, false, None) }
            .ctx("failed to create the stop event")?;
        Ok(Arc::new(Self {
            stop: AtomicBool::new(false),
            wake: OwnedHandle::new(handle),
        }))
    }

    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        // SAFETY: 句柄由 OwnedHandle 持有，未关闭；SetEvent 线程安全。
        unsafe {
            let _ = SetEvent(self.wake.raw());
        }
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// 采集线程跑起来之后回传给调用方的东西。
pub(crate) type StartReport = PortResult<CaptureFormat>;

/// 采集线程的启动握手通道。
pub(crate) struct Handshake {
    tx: mpsc::Sender<StartReport>,
}

impl Handshake {
    pub(crate) fn pair() -> (Self, mpsc::Receiver<StartReport>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx }, rx)
    }

    /// 报告启动结果。发送失败说明调用方已经不等了，直接忽略。
    pub(crate) fn report(&self, report: StartReport) {
        let _ = self.tx.send(report);
    }
}

/// 等采集线程的启动结果。
pub(crate) fn await_start(
    rx: mpsc::Receiver<StartReport>,
    what: &str,
) -> PortResult<CaptureFormat> {
    match rx.recv_timeout(START_TIMEOUT) {
        Ok(report) => report,
        Err(RecvTimeoutError::Timeout) => Err(PortError::new(format!(
            "{what} start timed out (not ready after {} s)",
            START_TIMEOUT.as_secs()
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(PortError::new(format!(
            "the {what} thread exited unexpectedly"
        ))),
    }
}

/// 采集事件循环。
///
/// 这个函数跑在我们自己开的采集线程上，是 ARCHITECTURE §6 说的“音频回调线程”。
/// 循环体里只做三件事：把包拉出来、转成 f32 交给回调、释放包。不打日志、不加锁。
/// （`AudioChunk` 带 `Vec` 是 vox-core 定的接口形状，这个分配躲不掉；
/// 除此之外没有额外分配——staging 缓冲只在头几块里长大。）
pub(crate) fn capture_loop(
    client: &IAudioClient,
    capture: &IAudioCaptureClient,
    event: &OwnedHandle,
    info: WaveInfo,
    block_ms: u32,
    control: &CaptureControl,
    mut on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) {
    let mut blocker = Blocker::new(info.sample_rate, info.channels, block_ms);
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);

    while !control.stopping() {
        // SAFETY: 事件句柄有效；超时是预期路径，不当错误。
        let wait = unsafe { WaitForSingleObject(event.raw(), WAIT_TIMEOUT_MS) };
        if control.stopping() {
            break;
        }
        if wait == WAIT_TIMEOUT {
            // 目标一声不出就是这个分支：安静而已，继续等。
            continue;
        }
        if wait != WAIT_OBJECT_0 {
            // 句柄失效之类的硬错误，退出循环。上层通过 stop() 收尾。
            break;
        }

        loop {
            // SAFETY: capture 客户端有效；GetNextPacketSize 只读下一个包的帧数。
            let next = match unsafe { capture.GetNextPacketSize() } {
                Ok(n) => n,
                Err(_) => break,
            };
            if next == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            // SAFETY: 三个出参都指向本地变量；成功返回后 data 指向至少
            // frames * nBlockAlign 字节的可读内存，且在 ReleaseBuffer 之前一直有效。
            let got = unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) };
            if got.is_err() {
                break;
            }
            if frames > 0 {
                scratch.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    // 驱动说这段是静音，指针内容未定义，必须自己补零而不是去读。
                    scratch.resize(frames as usize * info.channels as usize, 0.0);
                } else if !data.is_null() {
                    let len = frames as usize * info.block_align;
                    // SAFETY: WASAPI 保证这段内存在 ReleaseBuffer 之前有效，
                    // 长度就是帧数乘块对齐（格式来自同一个 client 的协商结果）。
                    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
                    bytes_to_f32(bytes, info.kind, &mut scratch);
                }
                if !scratch.is_empty() {
                    blocker.feed(&scratch, &mut *on_chunk);
                }
            }
            // SAFETY: 与上面成功的 GetBuffer 一一配对，帧数原样交回。
            unsafe {
                let _ = capture.ReleaseBuffer(frames);
            }
        }
    }

    // SAFETY: client 有效；停流失败也没什么可做的，接口马上就要释放了。
    unsafe {
        let _ = client.Stop();
    }
}

/// 建一个给 `SetEventHandle` 用的自动复位事件。
pub(crate) fn create_stream_event() -> PortResult<OwnedHandle> {
    // SAFETY: 自动复位、初始未触发的无名事件；句柄交给 OwnedHandle 独占。
    let handle = unsafe { CreateEventW(None, false, false, None) }
        .ctx("failed to create the audio event")?;
    Ok(OwnedHandle::new(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_stop_is_visible_and_idempotent() {
        let c = CaptureControl::new().unwrap();
        assert!(!c.stopping());
        c.request_stop();
        assert!(c.stopping());
        c.request_stop();
        assert!(c.stopping());
    }

    #[test]
    fn await_start_reports_thread_death() {
        let (hs, rx) = Handshake::pair();
        drop(hs);
        let err = await_start(rx, "测试").unwrap_err();
        assert!(
            err.message.contains("exited unexpectedly"),
            "{}",
            err.message
        );
    }

    #[test]
    fn await_start_passes_format_through() {
        let (hs, rx) = Handshake::pair();
        hs.report(Ok(CaptureFormat {
            sample_rate: 48_000,
            channels: 2,
        }));
        let f = await_start(rx, "测试").unwrap();
        assert_eq!(f.sample_rate, 48_000);
        assert_eq!(f.channels, 2);
    }
}
