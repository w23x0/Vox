//! 把同一程序的**其它**音频流接到我们的采集流上（混音）。
//!
//! 场景：Chromium 那种一个程序多条播放流（每个标签页一条），`target.object` 一次
//! 只认一个节点，剩下的要靠显式建链接进来。
//!
//! 为什么单独开一个连接 + 线程（而不是在采集线程里顺手做）：
//! **采集线程的主循环不能被 `roundtrip()` 用掉**——`probe::roundtrip()` 会
//! `main_loop.quit()`，之后再 `run()` 会立刻返回，采集流当场被拆掉（实测：启动
//! 必然 8 秒超时）。这里自己开一个连接（自己的主循环），只做三件事：
//!
//! 1. 等采集流的输入端口出现在图里（最多 2 秒）；
//! 2. 用 `link-factory` 把每个额外目标的输出端口连到我们的输入端口；
//! 3. **一直持有 link 代理**——drop 掉代理，服务端那边的链路也会被删。
//!
//! 建链是"尽力而为"：连不上只记警告，不影响主目标采集（主目标由 session manager
//! 连，见 `capture.rs` 模块头）。

use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use vox_core::ports::{PortError, PortResult};

use crate::probe::{self, attach_quit, Cmd};

/// 等端口出现的上限：采集流连上之后端口基本立刻就有，2 秒是兜底。
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);
/// 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 链路守护。`stop()` 之后链路消失（代理被 drop）。
pub struct LinkKeeper {
    cmd: pw::channel::Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl LinkKeeper {
    /// 起守护：把 `targets`（目标流的节点 id）连到名字为 `our_node_name` 的采集流上。
    ///
    /// 用**名字**而不是节点 id 找自己：`stream.node_id()` 在服务端把节点建出来之前
    /// 返回的是 `PW_ID_ANY`（实测 4294967295），而"等到节点建出来"需要 roundtrip——
    /// 那正是会拆掉采集流的东西（见模块头）。所以采集流用一个唯一名字，守护按名字找。
    ///
    /// **立刻返回、不等结果**：调用方（采集线程）要接着跑自己的主循环，节点才会被
    /// 建出来；在这里等就成了死锁（实测：守护等不到节点、采集线程等守护，2 秒后超时）。
    /// 连了几条链路由守护线程自己打日志。
    pub fn start(our_node_name: String, targets: Vec<u32>) -> PortResult<Self> {
        let (cmd, cmd_rx) = pw::channel::channel::<Cmd>();
        let thread = thread::Builder::new()
            .name("vox-link".into())
            .spawn(move || keeper_thread(our_node_name, targets, cmd_rx))
            .map_err(|e| PortError::new(format!("failed to spawn the link keeper thread: {e}")))?;
        Ok(Self {
            cmd,
            thread: Some(thread),
        })
    }

    /// 收工：停守护线程，链路随之消失。可重复调用。
    pub fn stop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LinkKeeper {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 守护线程：自己的连接 + 主循环，建完链就一直挂着。
fn keeper_thread(our_node_name: String, targets: Vec<u32>, cmd_rx: pw::channel::Receiver<Cmd>) {
    probe::init();
    let (main_loop, core) = match probe::connect() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("链路守护连不上 PipeWire：{}", e.message);
            return;
        }
    };

    let _attached = attach_quit(&main_loop, cmd_rx);

    let links = match link_all(&core, &our_node_name, &targets) {
        Ok(links) => links,
        Err(e) => {
            tracing::warn!("目标音频流没接上（这条采集会听不到声音）：{}", e.message);
            return;
        }
    };
    tracing::debug!("目标音频流连上 {} 条链路", links.len());

    // 挂着：`links` 必须活到 Quit（drop 代理 = 服务端删链路）。
    main_loop.run();
    drop(links);
}

/// 等采集流的端口出现，然后把每个目标连上来。
fn link_all(
    core: &pw::core::CoreRc,
    our_node_name: &str,
    targets: &[u32],
) -> PortResult<Vec<pw::link::Link>> {
    // 等采集流的节点与输入端口出现。用 `probe::snapshot()` 而不是在本循环上
    // roundtrip：后者会把我们这个主循环 quit 掉（采集线程那边就是这么踩坑的）。
    let deadline = std::time::Instant::now() + WAIT_TIMEOUT;
    let mut our_node = None;
    let mut our_inputs: Vec<u32> = Vec::new();
    while std::time::Instant::now() < deadline {
        let snapshot = probe::snapshot()?;
        our_node = snapshot
            .nodes
            .iter()
            .find(|(_, node)| node.name == our_node_name)
            .map(|(id, _)| *id);
        if let Some(node) = our_node {
            our_inputs = snapshot
                .ports_of_dir(node, "in")
                .into_iter()
                .map(|port| port.id)
                .collect();
            if !our_inputs.is_empty() {
                break;
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    let Some(our_node) = our_node else {
        return Err(PortError::new(format!(
            "timed out waiting for capture stream '{our_node_name}' to appear in the graph"
        )));
    };
    if our_inputs.is_empty() {
        return Err(PortError::new(format!(
            "timed out waiting for the input port of capture stream '{our_node_name}' (node {our_node})"
        )));
    }

    let snapshot = probe::snapshot()?;
    let mut links = Vec::new();
    for target in targets {
        let outs: Vec<u32> = snapshot
            .ports_of_dir(*target, "out")
            .into_iter()
            .map(|port| port.id)
            .collect();
        for (index, out_port) in outs.iter().enumerate() {
            // 声道数对不上时（单声道目标 vs 立体声采集）按最后一个端口复用。
            let in_port = our_inputs[index.min(our_inputs.len() - 1)];
            let props = properties! {
                "link.output.node" => target.to_string(),
                "link.output.port" => out_port.to_string(),
                "link.input.node" => our_node.to_string(),
                "link.input.port" => in_port.to_string(),
            };
            match core.create_object::<pw::link::Link>("link-factory", &props) {
                Ok(link) => links.push(link),
                Err(e) => tracing::warn!("额外音频流连一条链路失败（忽略）：{e}"),
            }
        }
    }
    Ok(links)
}
