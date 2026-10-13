//! 采集：`CaptureSource` 的 Windows 实现。
//!
//! 三条路：麦克风、按进程环回、整机环回。前两条对应 `CaptureTarget` 的两个变体，
//! 整机环回是进程环回的退路，由 `EndpointLoopbackCapture` 单独提供。
//!
//! 每条路都是“开一个线程，线程自己初始化 COM，事件驱动地拉数据”。
//! 接口对象全部在那个线程上创建和销毁，一个都不跨线程，省掉整类套间问题。

mod endpoint;
mod loopback;
mod mic;
mod shared;

use std::sync::Arc;
use std::thread::JoinHandle;

use vox_core::notice::{key, Msg};
use vox_core::ports::{
    AudioChunk, CaptureFormat, CaptureSource, CaptureTarget, PortError, PortResult,
};

use crate::com::ComGuard;
use crate::proc;
use crate::sessions;

use shared::{await_start, capture_loop, CaptureControl, Handshake};

/// 采集线程的句柄集合。
struct Running {
    control: Arc<CaptureControl>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn stop(mut self) {
        self.control.request_stop();
        if let Some(t) = self.thread.take() {
            // 循环最长等一个超时周期（250 ms）就会看到停止标志，join 不会挂太久。
            let _ = t.join();
        }
    }
}

/// 麦克风 / 进程环回采集。
///
/// 进程环回在老系统上不可用时就报错，让调用方（通常是 UI）决定怎么退。
/// 宽松模式（`with_endpoint_fallback`，自动退整机环回）已移除：主路径用进程
/// 环回、失败直说，不静默降级到抓整机声音。
pub struct WinCapture {
    running: Option<Running>,
}

impl WinCapture {
    /// 进程环回不可用就报错。
    pub fn new() -> Self {
        Self { running: None }
    }
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSource for WinCapture {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat> {
        // 重复 start 视为换目标：先把旧的收干净，否则两个流会同时往回调里灌数据。
        self.stop();

        let control = CaptureControl::new()?;
        let (handshake, rx) = Handshake::pair();
        let (plan, what) = match target {
            CaptureTarget::Microphone(name) => {
                (Plan::Microphone(name.clone()), "microphone capture".to_string())
            }
            CaptureTarget::ProcessLoopback {
                executable,
                include_tree,
            } => (
                Plan::Process {
                    executable: executable.clone(),
                    include_tree: *include_tree,
                },
                format!("process loopback capture of {executable}"),
            ),
            // 网络采集目标（媒体面）不归 WASAPI 这条路：它由 `vox-net` 的监听侧实现。
            CaptureTarget::Net { .. } => {
                return Err(PortError::new(
                    "network audio is not a WASAPI capture target (it is served by the vox-net media plane)",
                ))
            }
        };

        let thread_control = Arc::clone(&control);
        let thread = std::thread::Builder::new()
            .name("vox-capture".into())
            .spawn(move || run_capture(plan, block_ms, thread_control, handshake, on_chunk))
            .map_err(|e| PortError::new(format!("failed to spawn the capture thread: {e}")))?;

        self.running = Some(Running {
            control,
            thread: Some(thread),
        });

        match await_start(rx, &what) {
            Ok(format) => Ok(format),
            Err(e) => {
                // 启动失败也要把线程收干净，不能留一个僵尸线程。
                self.stop();
                Err(e)
            }
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 整机环回采集：抓某个输出设备上的全部声音。
///
/// `CaptureTarget` 里没有对应的变体（vox-core 的接口不认识这条路），
/// 所以约定：`Microphone(name)` 的 name 在这里表示**输出设备**名，`None` 表示默认输出。
/// `ProcessLoopback` 传进来时忽略进程名，同样抓默认输出。
pub struct EndpointLoopbackCapture {
    running: Option<Running>,
}

impl EndpointLoopbackCapture {
    pub fn new() -> Self {
        Self { running: None }
    }
}

impl Default for EndpointLoopbackCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSource for EndpointLoopbackCapture {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat> {
        self.stop();
        let device = match target {
            CaptureTarget::Microphone(name) => name.clone(),
            CaptureTarget::ProcessLoopback { .. } => None,
            // 整机环回也是设备采集：网络源不归这条路。
            CaptureTarget::Net { .. } => {
                return Err(PortError::new(
                    "network audio is not a WASAPI capture target (it is served by the vox-net media plane)",
                ))
            }
        };
        let control = CaptureControl::new()?;
        let (handshake, rx) = Handshake::pair();
        let thread_control = Arc::clone(&control);
        let thread = std::thread::Builder::new()
            .name("vox-loopback".into())
            .spawn(move || {
                run_capture(
                    Plan::Endpoint(device),
                    block_ms,
                    thread_control,
                    handshake,
                    on_chunk,
                )
            })
            .map_err(|e| {
                PortError::new(format!("failed to spawn the loopback capture thread: {e}"))
            })?;
        self.running = Some(Running {
            control,
            thread: Some(thread),
        });
        match await_start(rx, "system loopback capture") {
            Ok(format) => Ok(format),
            Err(e) => {
                self.stop();
                Err(e)
            }
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for EndpointLoopbackCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 采集线程要干的活。
enum Plan {
    Microphone(Option<String>),
    Process {
        executable: String,
        include_tree: bool,
    },
    Endpoint(Option<String>),
}

/// 采集线程主体。
fn run_capture(
    plan: Plan,
    block_ms: u32,
    control: Arc<CaptureControl>,
    handshake: Handshake,
    on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) {
    // 自己初始化 COM，不假设调用方做过。MTA 是进程环回激活回调的硬要求。
    let _com = match ComGuard::mta() {
        Ok(g) => g,
        Err(e) => {
            handshake.report(Err(e));
            return;
        }
    };

    let open = match plan {
        Plan::Microphone(name) => mic::open_microphone(name.as_deref()),
        Plan::Endpoint(name) => endpoint::open_endpoint_loopback(name.as_deref()),
        Plan::Process {
            executable,
            include_tree,
        } => open_process(&executable, include_tree),
    };

    let open = match open {
        Ok(o) => o,
        Err(e) => {
            handshake.report(Err(e));
            return;
        }
    };

    handshake.report(Ok(open.format()));
    capture_loop(
        &open.client,
        &open.capture,
        &open.event,
        open.info,
        block_ms,
        &control,
        on_chunk,
    );
}

/// 解析目标进程并打开进程环回。
fn open_process(executable: &str, include_tree: bool) -> PortResult<shared::OpenCapture> {
    let pid = resolve_pid(executable, include_tree)?;
    loopback::open_process_loopback(pid, include_tree)
}

/// exe 名 → 该抓的 PID。
fn resolve_pid(executable: &str, include_tree: bool) -> PortResult<u32> {
    let all = proc::snapshot_processes()?;
    let candidates = proc::matching_processes(&all, executable);
    if candidates.is_empty() {
        return Err(PortError::user(
            Msg::new(key::PROGRAM_NOT_RUNNING).param("name", executable),
            format!("没找到正在运行的进程 {executable}（没启动，或者名字不对）"),
        ));
    }
    let hints = sessions::session_hints(&candidates);
    if hints.is_empty() {
        // 软件开着但从没出过声：只能猜根进程。记一条日志，方便对着现象排查。
        tracing::info!("{executable} 当前没有音频会话，按主进程抓（它开始出声后就会有数据）");
    }
    proc::choose_target_pid(&candidates, &hints, include_tree).ok_or_else(|| {
        PortError::new(format!(
            "{executable} is running, but no process could be chosen to capture"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_process_gives_actionable_error() {
        let _com = ComGuard::mta().unwrap();
        let err = resolve_pid("绝对没这个软件zzz.exe", true).unwrap_err();
        assert!(err.message.contains("没找到正在运行"), "{}", err.message);
    }

    #[test]
    fn resolving_this_process_returns_a_pid() {
        let _com = ComGuard::mta().unwrap();
        // 当前测试进程一定在跑，虽然没有音频会话——正好覆盖“猜主进程”那条路。
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_string();
        let pid = resolve_pid(&name, true).unwrap();
        assert_ne!(pid, 0);
    }

    #[test]
    fn stop_without_start_is_a_noop() {
        let mut cap = WinCapture::new();
        cap.stop();
        cap.stop();
        let mut ep = EndpointLoopbackCapture::new();
        ep.stop();
    }

    #[test]
    fn unknown_microphone_start_fails_and_leaves_no_thread() {
        let mut cap = WinCapture::new();
        let err = cap
            .start(
                &CaptureTarget::Microphone(Some("不存在的麦 zzz".into())),
                20,
                Box::new(|_| {}),
            )
            .unwrap_err();
        assert!(err.message.contains("找不到"), "{}", err.message);
        assert!(cap.running.is_none());
    }

    #[test]
    #[ignore = "需要 Win11/Server2022+ 且有正在跑的目标进程，手动跑：cargo test -p vox-audio-win -- --ignored"]
    fn process_loopback_activates_on_a_live_process() {
        // 这条是整个 crate 里最容易出错的路径（MTA + IAgileObject + 两个 HRESULT +
        // 伪设备上不能用 GetBufferSize），所以专门留一条真机验证。
        // 拿 explorer.exe 当靶子：一定在跑，通常没在出声——正好验证“安静目标不报错”。
        if !crate::osver::process_loopback_available() {
            eprintln!(
                "跳过：当前系统 build {} < {}",
                crate::osver::os_build_number(),
                crate::osver::MIN_PROCESS_LOOPBACK_BUILD
            );
            return;
        }
        let mut cap = WinCapture::new();
        let format = cap
            .start(
                &CaptureTarget::ProcessLoopback {
                    executable: "explorer.exe".into(),
                    include_tree: true,
                },
                20,
                Box::new(|chunk| {
                    assert_eq!(chunk.sample_rate, 48_000);
                    assert_eq!(chunk.channels, 2);
                }),
            )
            .unwrap();
        // 伪设备的格式是我们手搓的，必须原样回来。
        assert_eq!(format.sample_rate, 48_000);
        assert_eq!(format.channels, 2);
        // 安静目标不该报错、也不该卡住；停下来要能干净 join。
        std::thread::sleep(std::time::Duration::from_millis(600));
        cap.stop();
    }

    #[test]
    #[ignore = "会从默认输出发出 0.5 秒轻微蜂鸣，手动跑：--ignored"]
    fn process_loopback_captures_audio_from_the_target() {
        // 上一条只验证了“能激活、安静时不报错”。这条验证真有数据：
        // 让测试进程自己往默认输出播一段音，再对着测试进程自己抓进程环回。
        // 抓到非静音数据才说明伪设备真的在出货。
        use std::sync::atomic::{AtomicU32, Ordering};

        if !crate::osver::process_loopback_available() {
            eprintln!("跳过：系统 build 太老");
            return;
        }
        use vox_core::ports::PlaybackSink;

        let exe = std::env::current_exe().unwrap();
        let me = exe.file_name().unwrap().to_string_lossy().to_string();

        let loud = Arc::new(AtomicU32::new(0));
        let l = Arc::clone(&loud);
        let mut cap = WinCapture::new();
        cap.start(
            &CaptureTarget::ProcessLoopback {
                executable: me,
                include_tree: true,
            },
            20,
            Box::new(move |chunk| {
                if chunk.samples.iter().any(|s| s.abs() > 0.01) {
                    l.fetch_add(1, Ordering::Relaxed);
                }
            }),
        )
        .unwrap();

        let mut p = crate::playback::WinPlayback::new(crate::playback::test_resample_factory());
        p.open(None, 24_000).unwrap();
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
            "进程环回只抓到 {} 块非静音数据",
            loud.load(Ordering::Relaxed)
        );
    }

    #[test]
    #[ignore = "需要真麦克风，手动跑：cargo test -p vox-audio-win -- --ignored"]
    fn microphone_delivers_chunks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let mut cap = WinCapture::new();
        let format = cap
            .start(
                &CaptureTarget::Microphone(None),
                20,
                Box::new(move |chunk| {
                    assert!(!chunk.samples.is_empty());
                    c.fetch_add(1, Ordering::Relaxed);
                }),
            )
            .unwrap();
        assert!(format.sample_rate > 0);
        std::thread::sleep(std::time::Duration::from_millis(600));
        cap.stop();
        assert!(
            count.load(Ordering::Relaxed) >= 10,
            "600 ms 里只收到 {} 块，20 ms 一块该有 ~30 块",
            count.load(Ordering::Relaxed)
        );
    }
}
