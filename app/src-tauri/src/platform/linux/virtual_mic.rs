//! Linux 虚拟麦**接线**（S0 §3.2 ①）：把 `vox-audio-linux` 的虚拟 sink 挂进装配流程。
//!
//! 接线之前这一位报 `false(not_wired)`——实现早就有了（`crates/vox-audio-linux/src/
//! virtual_sink.rs`），是装配层从没调用过它：系统里根本没有这个设备，界面却让用户
//! "去目标程序里选 Vox Virtual Mic"。接线之后这条路的凭据变成**真的持有句柄**
//! （§2.5.4 的 R6）：位为 `true` 的唯一理由是这个进程把节点建出来了、且它还活着。
//!
//! 为什么长这样（两个约束，都是平台事实）：
//!
//! 1. **句柄必须留在建它的线程**：`VirtualSink` 里握着 `pipewire` 的 `MainLoopRc` /
//!    `CoreRc` / `Node`，三者都不是 `Send`（`Proxy` 是裸指针 + `Drop` 里删对象），
//!    所以只能放 `thread_local`。建与删都在 Tauri 主线程（`assemble` / 退出回调），
//!    天然满足。
//! 2. **别的线程要能读这一位**：设备轮询线程要重报事实（§2.5.0 第 3 步），它读的是
//!    `STATUS` 那份纯数据，不碰句柄。
//!
//! 节点与句柄同命：`VirtualSink` drop 掉节点就没了（`virtual_sink.rs` 故意不开
//! `object.linger`），所以"句柄活着"和"设备在系统里"是同一件事。

use std::cell::RefCell;
use std::sync::Mutex;

use vox_audio_linux::{VirtualSink, VIRTUAL_MIC_NODE_NAME};
use vox_core::capability::{CapabilityStatus, UnavailableReason};

thread_local! {
    /// 建好的节点。**它活着 = 设备在系统里**。
    static SINK: RefCell<Option<VirtualSink>> = const { RefCell::new(None) };
}

/// 这一位的事实。装配线程写，任何线程读（设备轮询要读）。
///
/// 初值是 `off(not_wired)`：`ensure()` 还没跑过 = 这条路还没接上——这正是
/// `UnavailableReason::NotWired` 的语义（"不是平台做不到，是装配层还没接"）。
static STATUS: Mutex<CapabilityStatus> =
    Mutex::new(CapabilityStatus::off(UnavailableReason::NotWired));

/// 拿句柄槽。TLS 已经拆了（进程正在退出）时返回 `None`——不 panic。
fn with_sink<R>(f: impl FnOnce(&mut Option<VirtualSink>) -> R) -> Option<R> {
    SINK.try_with(|slot| f(&mut slot.borrow_mut())).ok()
}

/// 按需把虚拟麦建出来，返回这一位现在的状态。装配时调一次（主线程）。
///
/// 已经持有句柄时先确认节点还在图里：有人在别处把它删了（`pw-cli destroy`）就重建。
pub fn ensure() -> CapabilityStatus {
    let status = with_sink(|slot| {
        if slot.is_some() && node_in_graph() {
            return CapabilityStatus::ON;
        }
        // 旧句柄（如果还有）在这里 drop：节点已经不在图里，drop 只是收摊。
        *slot = None;
        match VirtualSink::create() {
            Ok(sink) => {
                *slot = Some(sink);
                tracing::info!("虚拟麦克风已建立：{VIRTUAL_MIC_NODE_NAME}");
                CapabilityStatus::ON
            }
            Err(e) => {
                // 建不出来（PipeWire 不在、名字被占）就如实报假——**不许**位说 ON
                // 而设备不存在，那正是接线前的老毛病。
                tracing::warn!("虚拟麦克风建不起来，这一位报假：{e}");
                CapabilityStatus::off(UnavailableReason::Unsupported)
            }
        }
    });
    let status = status.unwrap_or(CapabilityStatus::off(UnavailableReason::NotWired));
    *lock_status() = status;
    status
}

/// 复核一次这一位：位是 ON 就确认节点还在图里，掉了如实翻假。
///
/// **不重建**：句柄不是 `Send`，设备轮询线程建不了，重建留给下一次启动。
/// 装配时（主线程）走的是 [`ensure()`]，那条路会重建。
pub fn recheck() -> CapabilityStatus {
    let recorded = *lock_status();
    if !recorded.enabled || node_in_graph() {
        return recorded;
    }
    let off = CapabilityStatus::off(UnavailableReason::Unsupported);
    *lock_status() = off;
    tracing::warn!("虚拟麦克风节点不在图里了，虚拟麦这一位翻假");
    off
}

/// 退出时删掉节点。
///
/// **必须排在 `engine.shutdown()` 之后**（见 `lib.rs` 的 `shutdown` 顺序）：播放流还挂在
/// 节点上时先删节点，会留下一条指向不存在节点的悬挂 stream。
pub fn shutdown() {
    let destroyed = with_sink(|slot| slot.take().is_some()).unwrap_or(false);
    if destroyed {
        // 句柄没了，这一位就该跟着翻假：路不再开着（`NotWired` = "这个进程没在持着它"）。
        *lock_status() = CapabilityStatus::off(UnavailableReason::NotWired);
        tracing::info!("虚拟麦克风已删除");
    }
}

/// 节点现在还在图里吗。**任何线程都能问**：内部连一次 PipeWire、收一轮图快照。
fn node_in_graph() -> bool {
    // 连不上 PipeWire（或快照失败）就当不在——位为真必须有凭据，问不到凭据就是没有。
    VirtualSink::exists().unwrap_or(false)
}

fn lock_status() -> std::sync::MutexGuard<'static, CapabilityStatus> {
    // 这个锁只护一个 `Copy` 值、没有嵌套获取，中毒只可能是别处 panic 过；那时
    // 继续用里面的值比在启动路径上再 panic 一次好。
    STATUS.lock().unwrap_or_else(|e| e.into_inner())
}
