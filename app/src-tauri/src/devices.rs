//! 设备枚举线程。
//!
//! 枚举要走 COM，一次几十毫秒，绝不能在 Tauri 命令里同步做（会卡住 UI 线程）。
//! 所以：起个自己的线程，先立刻扫一次，之后低频轮询——插拔耳机、开关某个程序
//! 都会改变可选项，用户不该为了看到新设备去点刷新。
//!
//! 轮询而不是订阅 `IMMNotificationClient`：那个要 COM 回调对象和消息泵，
//! 复杂度换来的只是几秒的延迟差，不值。
//!
//! 每个 tick 还顺手刷新一次**宿主事实**（能力位，S0 §2.5.0 第 3 步）：设备/宿主侧的事实
//! 跟设备列表一样会变，界面按能力位降级，所以两者搭同一个 tick 一起报。
//!
//! **扫设备目录那一份实现住在共享宿主层**（`vox_host::scan_devices`）：它与无屏档装配那一份
//! 原本是逐字同形的两个 8 行函数（S4-A §1.2-D9），合并成一份，两个外壳都不留第二份。
//! 留在本文件的只有**线程**——无屏档刻意不装它（没有界面要秒级反映插拔）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use vox_core::runtime::Runtime;

use crate::state::AppState;

/// 两次自动扫描之间隔多久。够快能感知插拔，够慢不至于一直占着 COM。
const POLL_INTERVAL: Duration = Duration::from_secs(4);

static STOP: AtomicBool = AtomicBool::new(false);

/// 轮询线程句柄，`stop()` 里 join。
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

pub fn start(state: &Arc<AppState>) {
    let state = Arc::clone(state);
    let notify_rt = state.runtime.clone();
    let handle = std::thread::Builder::new()
        .name("vox-devices".into())
        .spawn(move || {
            // catch_unwind：这里 panic 的默认表现是"设备列表停止刷新"——插拔耳机
            // 不再出现在下拉框里，而界面上一切正常，用户只会觉得设备识别很烂。
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    let snapshot = vox_host::scan_devices(state.registry.as_ref());
                    state.runtime.set_devices(snapshot);
                    if refresh_host_facts(&state.runtime) {
                        // 事实变了 ⇒ 能力位可能变了。设备列表没变时 `set_devices` 的去重会把
                        // 这一声吞掉，界面就还拿着旧位降级（"位说能用、点下去没反应"的老毛病）。
                        // 位变了的刷新走同一条通道（S0 §2.6 R7）：补发一声，前端重取快照。
                        state.runtime.touch_devices();
                    }
                    // 分段睡，好让退出时最多等 250 ms 而不是一整个周期。
                    let mut slept = Duration::ZERO;
                    while slept < POLL_INTERVAL {
                        if STOP.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                        slept += Duration::from_millis(250);
                    }
                }
            }));
            if result.is_err() {
                notify_rt.notify(vox_core::event::Notice::warning(
                    "设备枚举线程异常退出，设备列表不再自动刷新。可以手动点「重新扫描设备」。"
                        .to_string(),
                ));
            }
        })
        .ok(); // 起不来不致命：设备列表就一直是空的，UI 少几个选项，但应用能用。
    if handle.is_none() {
        tracing::warn!("设备枚举线程起不来，设备列表将为空");
    }
    *THREAD.lock() = handle;
}

/// 让轮询线程收工并等它退出（最多 250 ms）。进程退出时调。
///
/// 必须 join：这个线程会 `set_devices` → emit 事件 → listener 可能把账本标脏。
/// 只竖旗就 flush 的话，最后 250 ms 内的改动会静默丢掉。
pub fn stop() {
    STOP.store(true, Ordering::Relaxed);
    if let Some(h) = THREAD.lock().take() {
        let _ = h.join();
    }
}

/// 顺带刷新一次宿主事实（§2.5.0 第 3 步）：**这台机器的事实会变**——虚拟麦节点被别的东西
/// 删了、PipeWire 断了、托盘宿主装上/卸掉、VB-CABLE 装完重启过——变了就得让能力位跟着变。
///
/// 跟 `set_devices` 同一个 tick、同一个理由：能力位是设备/宿主侧的事实，界面按它降级。
/// **只在真的变了才注入**，免得每个 tick 都写一次账本。
///
/// 返回值 = 事实真的变了（调用方据此补发一声 `DevicesChanged`，见轮询循环里的注释）。
fn refresh_host_facts(runtime: &Runtime) -> bool {
    let facts = crate::platform::host_facts();
    if runtime.host_facts() == facts {
        return false;
    }
    tracing::debug!("宿主事实变了，重新注入能力位");
    runtime.set_host_facts(facts);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::Settings;

    /// 事实没变就不许再注入（每 4 秒一次，注入一次就多一条事件）；变了必须报出来——
    /// 调用方据此补发一声 `DevicesChanged`，界面才会重取快照里的能力位（§2.6 R7）。
    #[test]
    fn host_facts_refresh_reports_only_real_changes() {
        // 观测槽是进程级的，而这条用例要前后比两次 `host_facts()`：与写槽的用例
        // （`platform::tests` 里那两条）串起来跑，免得把测试并行造的假象当成竞态。
        let _guard = crate::platform::OBSERVED_BIT_LOCK.lock();
        let runtime = Runtime::new(Settings::default(), crate::platform::clock());

        // 账本刚建好时是芯的缺省事实（`HostFacts::uninjected`，fail-closed 那一份），
        // 跟这台机器的真实事实**不是**同一个值：第一次刷新必须报"变了"。
        assert!(
            refresh_host_facts(&runtime),
            "首次刷新要把外壳的事实注入进去，并报出这次变化"
        );
        assert_eq!(runtime.host_facts(), crate::platform::host_facts());

        // 第二次：同一份事实再报一次不算变化（否则每个 tick 都会补发事件）。
        assert!(
            !refresh_host_facts(&runtime),
            "事实没变就不该再注入、也不该再补发事件"
        );
    }
}
