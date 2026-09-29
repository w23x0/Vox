//! `CaptureSource` 的 Linux 实现：PipeWire 采集流。
//!
//! 两种目标走两条路：
//!
//! | 目标 | 怎么接 | 谁负责连线 |
//! | --- | --- | --- |
//! | `Microphone(name)` | 普通采集流，`target.object` 指定设备，不指定就走默认源 | wireplumber 的策略 |
//! | `ProcessLoopback{executable}` | 找那个程序正在放音的节点，把它的输出端口连到我们的输入端口 | **我们自己**（见下） |
//!
//! 实测结论（决定这里怎么写）：
//!
//! - **按程序抓音不能靠 `target.object`**：把它设成某个程序的播放流节点，
//!   wireplumber 会**忽略**它、按策略把采集流连到默认源上（本机插上 USB 耳机后
//!   暴露得很清楚：`target.object` 明明在 props 里，链路却连到 `alsa_input...`，
//!   录到的是环境噪声）。早先"抓到了"是巧合——那会儿默认源是 HDMI 的 monitor，
//!   而目标程序正好在往 HDMI 放音。
//! - 所以按程序抓音一律 **`node.autoconnect=false` + 自己用 `link-factory` 按
//!   node/port id 显式建链**（`link_keeper.rs`）。麦克风才走自动连接。
//!
//! **同一程序多条流**（Chromium 每个标签页一条）：`target.object` 一次只认一个节点，
//! 所以主目标交给 session manager 连，**其余交给 `link_keeper`** 显式接进来混音。
//! 建链**不能在本线程的主循环上做**——`probe::roundtrip()` 会把它 quit 掉，之后
//! `run()` 立刻返回、流当场被拆（实测启动必然超时），所以守护线程自己开一个连接。
//!
//! **故意不设 `RT_PROCESS`**：PipeWire 的 process 回调默认跑在实时线程上，而内核的
//! `on_chunk` 会加锁 + 分配 `AudioChunk`（`ports.rs` 定的接口形状），在实时线程里干这个
//! 是给自己找优先级反转。Windows 侧也一样——采集发生在自己起的普通线程上。

use std::collections::HashSet;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use vox_core::ports::{
    AudioChunk, CaptureFormat, CaptureSource, CaptureTarget, PortError, PortResult,
};
use vox_dsp::chunk::Blocker;

use crate::link_keeper::LinkKeeper;
use crate::probe::{
    self, as_f32_slice, attach_quit, map_err, Cmd, CLASS_OUTPUT_STREAM, REQUEST_CHANNELS,
    REQUEST_RATE,
};

/// 等流进入 Streaming 的上限（跟 Windows 侧同一个量级）。
const START_TIMEOUT: Duration = Duration::from_secs(8);
/// 采集流名字前缀。**每条流带唯一后缀**：`stream.node_id()` 在连上之前是
/// `PW_ID_ANY`，所以链路守护只能按名字找自己（见 `link_keeper.rs`）。
const NODE_NAME_PREFIX: &str = "vox-capture";

/// 采集计划：`start` 时定下来，之后线程照着做。
#[derive(Debug)]
enum Plan {
    /// 麦克风（`None` = 默认源）。
    Microphone(Option<String>),
    /// 按程序抓音：要抓的流节点（可能多条，全部由 `link_keeper` 显式连进来）。
    Process { targets: Vec<u32>, label: String },
}

struct Shared {
    stop: std::sync::atomic::AtomicBool,
    /// 启动回报通道。协商结果、启动错误都从这儿出去；谁先拿到谁负责回报。
    report: std::sync::Mutex<Option<mpsc::Sender<PortResult<CaptureFormat>>>>,
}

struct Running {
    shared: Arc<Shared>,
    cmd: pw::channel::Sender<Cmd>,
    thread: JoinHandle<()>,
}

/// 采集源。`start` 之后音频块通过回调推给内核，`stop` 保证回调不再触发。
pub struct LinuxCapture {
    running: Option<Running>,
}

impl LinuxCapture {
    pub fn new() -> Self {
        Self { running: None }
    }
}

impl Default for LinuxCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureSource for LinuxCapture {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat> {
        // 重复 start 视为换目标：先把旧的收干净，否则两个流会同时往回调里灌数据。
        self.stop();

        let plan = resolve_plan(target)?;
        let (report_tx, report_rx) = mpsc::channel::<PortResult<CaptureFormat>>();
        let shared = Arc::new(Shared {
            stop: std::sync::atomic::AtomicBool::new(false),
            report: std::sync::Mutex::new(Some(report_tx)),
        });
        let (cmd_tx, cmd_rx) = pw::channel::channel::<Cmd>();

        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("vox-capture".into())
            .spawn(move || capture_thread(plan, block_ms, thread_shared, cmd_rx, on_chunk))
            .map_err(|e| PortError::new(format!("创建采集线程失败：{e}")))?;

        self.running = Some(Running {
            shared,
            cmd: cmd_tx,
            thread,
        });

        let format = match report_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(format)) => format,
            Ok(Err(e)) => {
                self.stop();
                return Err(e);
            }
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                return Err(PortError::new(format!(
                    "采集启动超时（等了 {} 秒还没就绪）",
                    START_TIMEOUT.as_secs()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.stop();
                return Err(PortError::new("采集线程意外退出"));
            }
        };

        Ok(format)
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running
                .shared
                .stop
                .store(true, std::sync::atomic::Ordering::Release);
            let _ = running.cmd.send(Cmd::Quit);
            // 线程退出即代表回调不会再触发——这是 trait 契约里唯一能给的保证。
            //
            // **有界等待**：`pw::channel` 的唤醒依赖 PipeWire 主循环（pipewire-rs 自己的
            // 文档里那个例子就挂在 issue #19 上），万一它没把 Quit 送进循环，无界 join 会把
            // 整个流水线卡死在 stop 上。宁可漏一个线程，也不能让用户点"停止"没反应。
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !running.thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if running.thread.is_finished() {
                let _ = running.thread.join();
            } else {
                tracing::warn!("采集线程没在 2 秒内退出，先放它自生自灭（不再 join）");
            }
        }
    }
}

impl Drop for LinuxCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 把内核给的 `CaptureTarget` 翻译成"抓哪些节点"。
fn resolve_plan(target: &CaptureTarget) -> PortResult<Plan> {
    match target {
        CaptureTarget::Microphone(name) => {
            if let Some(name) = name.as_deref() {
                let snapshot = probe::snapshot()?;
                let known = snapshot
                    .nodes_of_class(probe::CLASS_SOURCE)
                    .iter()
                    .chain(snapshot.nodes_of_class(probe::CLASS_SOURCE_VIRTUAL).iter())
                    .any(|node| node.name == name || node.description == name);
                if !known {
                    return Err(PortError::new(format!("找不到输入设备「{name}」")));
                }
            }
            Ok(Plan::Microphone(name.clone()))
        }
        CaptureTarget::ProcessLoopback {
            executable,
            include_tree,
        } => {
            let snapshot = probe::snapshot()?;
            let streams = snapshot.output_streams_of(executable);
            if streams.is_empty() {
                return Err(PortError::new(format!(
                    "「{executable}」现在没有在放声音（按程序抓音只抓它正在播的流）"
                )));
            }
            let mut pids: HashSet<u32> = HashSet::new();
            for stream in &streams {
                if let Some(pid) = snapshot.pid_of(stream) {
                    pids.insert(pid);
                }
            }
            if *include_tree {
                // Chromium 那种一个程序十几个进程的：子进程放的音也要抓。
                for pid in pids.clone() {
                    pids.extend(descendants(pid));
                }
            }
            let targets: Vec<u32> = snapshot
                .nodes
                .iter()
                .filter(|(_, node)| node.media_class == CLASS_OUTPUT_STREAM)
                .filter(|(_, node)| snapshot.pid_of(node).is_some_and(|pid| pids.contains(&pid)))
                .map(|(id, _)| *id)
                .collect();
            if targets.is_empty() {
                return Err(PortError::new(format!(
                    "找到了「{executable}」的音频流，但定位不到它的进程（拿不到 pid）"
                )));
            }
            Ok(Plan::Process {
                targets,
                label: executable.clone(),
            })
        }
        // 网络采集目标（媒体面）不归 PipeWire 这条路：它由 `vox-net` 的监听侧实现。
        CaptureTarget::Net { .. } => Err(PortError::new(
            "网络音频不归 PipeWire 采集实现（由 vox-net 的媒体面提供）",
        )),
    }
}

/// 某个进程的所有后代（沿 `/proc/<pid>/task/*/children` 递归）。
///
/// 只读 /proc，不需要额外依赖；拿不到就返回空表（宁可少抓，不能抓错）。
fn descendants(root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut queue = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = queue.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            for child in children.split_whitespace() {
                if let Ok(child) = child.parse::<u32>() {
                    if seen.insert(child) {
                        out.push(child);
                        queue.push(child);
                    }
                }
            }
        }
    }
    out
}

/// 采集线程：建流 → 连流 → （按程序抓音时）自己建链 → 跑主循环。
fn capture_thread(
    plan: Plan,
    block_ms: u32,
    shared: Arc<Shared>,
    cmd_rx: pw::channel::Receiver<Cmd>,
    on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) {
    let result = (|| -> PortResult<()> {
        probe::init();
        let (main_loop, core) = probe::connect()?;
        let _attached = attach_quit(&main_loop, cmd_rx);

        // 名字唯一：进程号 + 纳秒时间戳（同一个进程里连开两次也不会撞）。
        let node_name = format!(
            "{NODE_NAME_PREFIX}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => node_name.as_str(),
        };
        match &plan {
            Plan::Microphone(Some(device)) => {
                props.insert("target.object", device.as_str());
            }
            Plan::Microphone(None) => {}
            Plan::Process { .. } => {
                // 不让 session manager 插手：链路全部由 `link_keeper` 显式建。
                props.insert(*pw::keys::NODE_AUTOCONNECT, "false");
            }
        }

        let stream = pw::stream::StreamBox::new(&core, &node_name, props).map_err(map_err)?;

        struct UserData {
            shared: Arc<Shared>,
            negotiated: Option<CaptureFormat>,
            blocker: Option<Blocker>,
            block_ms: u32,
            on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
        }

        let _listener = stream
            .add_local_listener_with_user_data(UserData {
                shared: Arc::clone(&shared),
                negotiated: None,
                blocker: None,
                block_ms,
                on_chunk,
            })
            .param_changed(|_stream, data, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let mut info = spa::param::audio::AudioInfoRaw::new();
                if info.parse(param).is_err() {
                    return;
                }
                let rate = info.rate();
                let channels = info.channels() as u16;
                if rate == 0 || channels == 0 {
                    return;
                }
                data.negotiated = Some(CaptureFormat {
                    sample_rate: rate,
                    channels,
                });
                data.blocker = Some(Blocker::new(rate, channels, data.block_ms));
                report_started(
                    &data.shared,
                    CaptureFormat {
                        sample_rate: rate,
                        channels,
                    },
                );
            })
            .state_changed(|_stream, data, _old, new| {
                if new == pw::stream::StreamState::Streaming && data.negotiated.is_none() {
                    // 协商没回来就进 Streaming：按请求值开工，别让上层干等超时。
                    data.negotiated = Some(CaptureFormat {
                        sample_rate: REQUEST_RATE,
                        channels: REQUEST_CHANNELS,
                    });
                    data.blocker =
                        Some(Blocker::new(REQUEST_RATE, REQUEST_CHANNELS, data.block_ms));
                    report_started(
                        &data.shared,
                        CaptureFormat {
                            sample_rate: REQUEST_RATE,
                            channels: REQUEST_CHANNELS,
                        },
                    );
                }
                if let pw::stream::StreamState::Error(message) = new {
                    report_failed(&data.shared, format!("采集流出错：{message}"));
                }
            })
            .process(|stream, data| {
                if data.shared.stop.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data_buf) = datas.first_mut() else {
                    return;
                };
                // **必须只看 chunk.size() 指的那一段**：`data()` 给的是整个映射缓冲，
                // 后面的字节是上一轮的残留。按整块算会把样本数放大十几倍（实测过）。
                let (offset, size) = {
                    let chunk = data_buf.chunk();
                    (chunk.offset() as usize, chunk.size() as usize)
                };
                // 采样格式是 f32 交错，所以按字节切就够：声道数这里用不上——
                // 块由 `Blocker` 按协商到的声道数切（见上面的 `Blocker::new`）。
                let Some(slice) = data_buf.data() else {
                    return;
                };
                let end = (offset + size).min(slice.len());
                if end <= offset {
                    return;
                }
                let samples = as_f32_slice(&slice[offset..end]);
                // 分开借：blocker 和 on_chunk 都在 user data 里。
                let UserData {
                    blocker, on_chunk, ..
                } = data;
                if let Some(blocker) = blocker.as_mut() {
                    blocker.feed(samples, &mut **on_chunk);
                }
            })
            .register()
            .map_err(map_err)?;

        // 麦克风走自动连接（wireplumber 认设备节点）；按程序抓音不自动连，
        // 链路由下面的守护显式建（见模块头）。
        let flags = match &plan {
            Plan::Microphone(_) => {
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS
            }
            Plan::Process { .. } => pw::stream::StreamFlags::MAP_BUFFERS,
        };
        probe::connect_request_format(&stream, spa::utils::Direction::Input, flags)?;

        // 链路守护：自己开连接，把目标程序的输出端口连到我们的输入端口。
        // 它必须在本线程 `run()` **之前**起：不连上就没有格式协商，流到不了
        // Streaming，`start()` 会等到超时。守护随本线程结束一起收工（Drop 会停它，
        // 代理一 drop 服务端就删链路）。
        let _keeper = match &plan {
            Plan::Process { targets, label } => {
                tracing::debug!("「{label}」交给链路守护（{} 条流）", targets.len());
                Some(LinkKeeper::start(node_name.clone(), targets.clone())?)
            }
            Plan::Microphone(_) => None,
        };

        main_loop.run();
        Ok(())
    })();

    if let Err(e) = result {
        // 还没回报过就交给 start()（它在等这个通道）；已经开工了就只记日志。
        if !report_failed(&shared, e.message.clone()) {
            tracing::error!("采集线程退出：{e}");
        }
    }
}

/// 把协商结果回报给 `start()`。只有第一次有效（通道被取走后就没了）。
fn report_started(shared: &Shared, format: CaptureFormat) {
    if let Some(report) = shared.report.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = report.send(Ok(format));
    }
}

/// 把失败回报给 `start()`。返回 `true` 表示确实发出去了（说明还没开工）。
fn report_failed(shared: &Shared, message: String) -> bool {
    match shared.report.lock().ok().and_then(|mut slot| slot.take()) {
        Some(report) => {
            let _ = report.send(Err(PortError::new(message)));
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_of_own_process_is_empty_or_plausible() {
        // 拿自己的 pid 试：结果里不该出现自己（避免自环），也不该 panic。
        let me = std::process::id();
        let kids = descendants(me);
        assert!(!kids.contains(&me));
    }

    #[test]
    fn unknown_microphone_is_rejected_with_a_clear_message() {
        // 不存在的设备名必须报错，而不是悄悄抓默认源。
        let err = resolve_plan(&CaptureTarget::Microphone(Some(
            "definitely-not-a-device".to_string(),
        )))
        .expect_err("不存在的设备该报错");
        // 没有 PipeWire 的环境（CI runner）会在更早一步失败，那条错误信息同样说清了原因。
        assert!(
            err.message.contains("找不到输入设备") || err.message.contains("PipeWire"),
            "错误要说清原因：{}",
            err.message
        );
    }
}
