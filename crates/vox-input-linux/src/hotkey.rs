//! 全局热键监听：每个输入设备一个读线程，状态变化就推进边沿状态机。
//!
//! 跟 Windows 侧（25 ms 轮询 `GetAsyncKeyState`）的差别：这里是**事件驱动**的。
//! 每次按键状态变化调一次 `EdgeTracker::update`，边沿照样准，而且不用轮询——
//! 代价是要盯着设备文件（读线程阻塞在 `poll` 上，100 ms 一轮，好让 `stop()` 能收工）。
//!
//! 线程模型：
//! - 每个设备一个读线程（键盘、鼠标各一个）；
//! - 所有线程共用一个 `State`：当前按下的码集合 + 边沿状态机 + 事件回调；
//! - `rebind` 只改状态机，不动线程。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use evdev::{Device, EventType};
use parking_lot::Mutex;
use tracing::{debug, warn};

use vox_core::hotkey::{resolve_bindings, EdgeTracker};
use vox_core::notice::{key, Msg};
use vox_core::ports::{HotkeyBindings, HotkeyEvent, HotkeyHost, PortError, PortResult};

use crate::codes::{looks_like_keyboard, looks_like_mouse, main_code, modifier_groups};

/// `poll` 的超时：决定 `stop()` 最坏要等多久。
const POLL_TIMEOUT_MS: i32 = 100;

/// 所有读线程共享的状态。
struct State {
    /// 当前按下的键码（键盘与鼠标共用一张表，码空间不重叠）。
    down: HashSet<u16>,
    tracker: EdgeTracker,
    on_event: Box<dyn FnMut(HotkeyEvent) + Send>,
}

impl State {
    /// 按键状态变了：重算边沿并派发事件。
    fn handle_change(&mut self, code: u16, pressed: bool) {
        if pressed {
            if !self.down.insert(code) {
                return; // 已经是按下（自动重复），状态没变
            }
        } else if !self.down.remove(&code) {
            return;
        }
        let down = &self.down;
        let events = self.tracker.update(|code| down.contains(&code));
        for event in events {
            (self.on_event)(event);
        }
    }
}

pub struct HotkeyListener {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl HotkeyListener {
    /// 启动监听。`on_event` 在读线程上被调用——内核应快速转发到自己的队列。
    ///
    /// 没有任何可读的输入设备（不在 `input` 组、或机器上真没设备）时返回明确错误。
    pub fn start(
        bindings: HotkeyBindings,
        on_event: Box<dyn FnMut(HotkeyEvent) + Send>,
    ) -> PortResult<Arc<Self>> {
        let devices = open_devices()?;
        let state = Arc::new(Mutex::new(State {
            down: HashSet::new(),
            tracker: EdgeTracker::new(resolve_bindings(&bindings, main_code, modifier_groups)),
            on_event,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        for device in devices {
            let name = device.name().unwrap_or("(未命名输入设备)").to_string();
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let handle = thread::Builder::new()
                .name("vox-hotkey".into())
                .spawn(move || read_loop(device, &name, state, stop))
                .map_err(|e| PortError::new(format!("failed to start the hotkey thread: {e}")))?;
            threads.push(handle);
        }

        Ok(Arc::new(Self {
            state,
            stop,
            threads: Mutex::new(threads),
        }))
    }

    /// 停止所有读线程并等它们退出（最坏等一个 `poll` 超时）。可重复调用。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.threads.lock().drain(..) {
            let _ = handle.join();
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
        let mut state = self.state.lock();
        state
            .tracker
            .rebind(resolve_bindings(&bindings, main_code, modifier_groups));
        // 按下状态跟着清掉：换了绑定之后旧的"按住"没有意义（内核也会自己收摊）。
        state.down.clear();
        debug!("热键绑定已更新");
        Ok(())
    }
}

/// 打开所有像键盘/鼠标的输入设备。
///
/// 权限不足时给的是**能直接照做**的错误：用户十有八九不在 `input` 组里。
fn open_devices() -> PortResult<Vec<Device>> {
    let entries = std::fs::read_dir("/dev/input").map_err(|e| {
        PortError::user(
            Msg::new(key::INPUT_PERMISSION),
            format!(
                "cannot read /dev/input: {e} (Linux global hotkeys need input-group access: \
                 sudo usermod -aG input $USER, then log in again)"
            ),
        )
    })?;

    let mut devices = Vec::new();
    let mut denied = 0usize;
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.starts_with("event") {
            continue;
        }
        match Device::open(&path) {
            Ok(device) => {
                let Some(keys) = device.supported_keys() else {
                    continue;
                };
                if looks_like_keyboard(keys) || looks_like_mouse(keys) {
                    debug!(
                        "监听输入设备 {} ({})",
                        path.display(),
                        device.name().unwrap_or("?")
                    );
                    devices.push(device);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied += 1,
            Err(e) => warn!("打开 {} 失败：{e}", path.display()),
        }
    }

    if devices.is_empty() {
        if denied > 0 {
            return Err(PortError::user(
                Msg::new(key::INPUT_PERMISSION),
                "no permission to read /dev/input/event*: Linux global hotkeys need the user in \
                 the input group (sudo usermod -aG input $USER), then log in again",
            ));
        }
        return Err(PortError::new(
            "no keyboard or mouse input device found (no usable event device under /dev/input)",
        ));
    }
    Ok(devices)
}

/// 单个设备的读循环：`poll` 等到有数据才读，`stop` 一到就退出。
fn read_loop(mut device: Device, name: &str, state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    use std::os::fd::AsRawFd;

    let fd = device.as_raw_fd();
    while !stop.load(Ordering::Relaxed) {
        // 用 poll 而不是直接 fetch_events()：后者会一直阻塞，stop() 就 join 不回来。
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd 是本函数的局部变量，nfds=1 与它匹配。
        let ready = unsafe { libc::poll(&mut pfd, 1, POLL_TIMEOUT_MS) };
        if ready <= 0 {
            continue; // 超时或被打断，回去看 stop 旗
        }

        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(e) => {
                // 设备被拔了之类：记一条就退出这个线程，别的设备继续跑。
                warn!("读 {name} 的事件失败，停止监听它：{e}");
                return;
            }
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            // value: 0 = 松开, 1 = 按下, 2 = 自动重复
            match event.value() {
                0 => state.lock().handle_change(event.code(), false),
                1 => state.lock().handle_change(event.code(), true),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_release_through_the_state_machine() {
        // 不开设备，直接喂状态机：验的是"事件 → 边沿"这一段。
        let mut state = State {
            down: HashSet::new(),
            tracker: EdgeTracker::new(resolve_bindings(
                &HotkeyBindings {
                    speak: Some(vox_core::hotkey::Hotkey::plain("F8")),
                    listen: None,
                },
                main_code,
                modifier_groups,
            )),
            on_event: Box::new(|event| {
                PRESSED.lock().push(event);
            }),
        };
        let f8 = main_code("F8").expect("F8 该有码");

        state.handle_change(f8, true);
        state.handle_change(f8, true); // 自动重复
        state.handle_change(f8, false);

        let events = PRESSED.lock().clone();
        PRESSED.lock().clear();
        assert_eq!(
            events,
            vec![HotkeyEvent::SpeakPressed, HotkeyEvent::SpeakReleased]
        );
    }

    #[test]
    fn permission_error_message_tells_the_user_what_to_do() {
        // 本机（未重新登录）大概率就是没权限：错误里必须带上那条命令。
        match open_devices() {
            Ok(devices) => {
                assert!(!devices.is_empty());
            }
            Err(e) => {
                assert!(
                    e.message.contains("input"),
                    "错误里要说明 input 组：{}",
                    e.message
                );
            }
        }
    }

    static PRESSED: Mutex<Vec<HotkeyEvent>> = Mutex::new(Vec::new());
}
