//! 全局热键轮询线程。
//!
//! 25 ms 一轮 GetAsyncKeyState，只用高位（0x8000）判断当前物理状态，
//! 边沿检测交给内核的 `vox_core::hotkey::EdgeTracker`（两个平台共用同一份状态机）。
//! 不用 RegisterHotKey 不用钩子。

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use parking_lot::Mutex;
use tracing::debug;

use vox_core::hotkey::{resolve_bindings, EdgeTracker};
use vox_core::ports::{HotkeyBindings, HotkeyEvent, HotkeyHost, PortError, PortResult};

use crate::vk::{main_code, modifier_groups};

/// 把绑定集解析成 VK 码。
fn codes_for(bindings: &HotkeyBindings) -> Vec<vox_core::hotkey::BindingCode> {
    resolve_bindings(bindings, main_code, modifier_groups)
}

/// 轮询间隔（毫秒）。25 ms 既足以捕捉快速点按，又不占明显 CPU。
const POLL_INTERVAL_MS: u64 = 25;

/// 共享绑定状态。轮询线程每轮开头快照一次，改绑定的线程随时可写。
struct Shared {
    bindings: HotkeyBindings,
    /// 递增版本号，轮询线程用来判断是否需要重建 EdgeTracker。
    version: u64,
}

pub struct HotkeyListener {
    shared: Arc<Mutex<Shared>>,
    /// `None` 表示已 stop 或 join 了。
    handle: Mutex<Option<JoinHandle<()>>>,
    /// 通知轮询线程退出。
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl HotkeyListener {
    /// 启动热键轮询。`on_event` 在轮询线程上被调用——内核应快速转发到自己的队列。
    pub fn start(
        bindings: HotkeyBindings,
        mut on_event: Box<dyn FnMut(HotkeyEvent) + Send>,
    ) -> PortResult<Arc<Self>> {
        let shared = Arc::new(Mutex::new(Shared {
            bindings: bindings.clone(),
            version: 1,
        }));
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let shared_clone = Arc::clone(&shared);
        let stop_clone = Arc::clone(&stop_flag);

        let handle = thread::Builder::new()
            .name("vox-hotkey".into())
            .spawn(move || {
                poll_loop(shared_clone, stop_clone, &mut on_event);
            })
            .map_err(|e| PortError::new(format!("failed to start the hotkey thread: {e}")))?;

        Ok(Arc::new(Self {
            shared,
            handle: Mutex::new(Some(handle)),
            stop_flag,
        }))
    }

    /// 停止线程并等它退出。可重复调用。
    pub fn stop(&self) {
        self.stop_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.lock().take() {
            let _ = h.join();
        }
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop();
    }
}

impl HotkeyHost for HotkeyListener {
    fn rebind(&self, bindings: HotkeyBindings) -> PortResult<()> {
        let mut guard = self.shared.lock();
        guard.bindings = bindings;
        guard.version += 1;
        debug!("热键绑定已更新，version={}", guard.version);
        Ok(())
    }
}

/// 轮询主循环。
fn poll_loop(
    shared: Arc<Mutex<Shared>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    on_event: &mut dyn FnMut(HotkeyEvent),
) {
    let mut tracker = {
        let guard = shared.lock();
        EdgeTracker::new(codes_for(&guard.bindings))
    };
    let mut known_version: u64 = 1;

    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        // 检查绑定是否改过
        {
            let guard = shared.lock();
            if guard.version != known_version {
                tracker.rebind(codes_for(&guard.bindings));
                known_version = guard.version;
            }
        }

        let events = tracker.update(is_key_down);
        for ev in events {
            on_event(ev);
        }

        thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// 判断某个 VK 当前是否物理按下。只看高位，不碰低位。
///
/// # Safety
/// GetAsyncKeyState 是线程安全的，对任意 VK 值调用不会崩。
#[cfg(windows)]
fn is_key_down(vk: u16) -> bool {
    // SAFETY: GetAsyncKeyState 接受任何 i32 参数，返回 SHORT。
    // 不需要窗口句柄或任何上下文，文档明确允许任意线程调用。
    let state = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) };
    (state & -32768i16) != 0 // 0x8000 的 i16 表示是 -32768
}

#[cfg(not(windows))]
fn is_key_down(_vk: u16) -> bool {
    false
}
