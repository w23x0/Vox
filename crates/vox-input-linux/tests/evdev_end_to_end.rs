//! 端到端：造一个虚拟键盘，把真实的按键事件打进内核，看监听器认不认。
//!
//! 需要 root（写 `/dev/uinput`）并且能读 `/dev/input/event*`。跑法：
//!
//! ```text
//! BIN=$(cargo test -p vox-input-linux --no-run --message-format=json \
//!       | python3 -c 'import json,sys; [print(json.loads(l)["executable"]) for l in sys.stdin if "executable" in l]' | tail -1)
//! sudo "$BIN" --ignored --nocapture
//! ```
//!
//! 这条测试是**唯一**能证明"evdev 这条路真的通"的东西：单元测试只验码表与状态机，
//! 真正读设备、`poll` 唤醒、`KEY_F8` 落到位、松开事件也在，全在这一条里。

#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyCode, KeyEvent};
use vox_core::hotkey::Hotkey;
use vox_core::ports::{HotkeyBindings, HotkeyEvent};
use vox_input_linux::HotkeyListener;

#[test]
#[ignore = "需要 root（/dev/uinput）与 input 组权限"]
fn virtual_keyboard_f8_reaches_the_listener() {
    let events = Arc::new(Mutex::new(Vec::<HotkeyEvent>::new()));

    // 先造虚拟键盘：监听器是在 `start()` 时枚举设备的，设备得先存在。
    //
    // 声明成"像键盘"的样子（含 A-Z）：监听器只认键盘与鼠标，只报一个 F8 的设备
    // 会被当成电源键之类的东西滤掉——真实键盘不会这么声明。
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in [KeyCode::KEY_A, KeyCode::KEY_Z, KeyCode::KEY_F8] {
        keys.insert(code);
    }
    let mut keyboard = VirtualDevice::builder()
        .expect("建 uinput 设备失败（需要 root 与 /dev/uinput）")
        .name("vox-test-keyboard")
        .with_keys(&keys)
        .expect("声明按键能力失败")
        .build()
        .expect("uinput 设备上线失败");

    let sink = Arc::clone(&events);
    let listener = HotkeyListener::start(
        HotkeyBindings {
            speak: Some(Hotkey::plain("F8")),
            listen: None,
        },
        Box::new(move |event| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(event);
            }
        }),
    )
    .expect("起热键监听失败");

    // 等监听线程把设备都打开（枚举 + poll 起来需要一点点时间）。
    std::thread::sleep(Duration::from_millis(300));
    // value：1 = 按下，0 = 松开。
    keyboard
        .emit(&[
            KeyEvent::new(KeyCode::KEY_F8, 1).into(),
            KeyEvent::new(KeyCode::KEY_F8, 0).into(),
        ])
        .expect("发按键事件失败");
    std::thread::sleep(Duration::from_millis(300));

    listener.stop();

    let got = events.lock().expect("事件表被毒了").clone();
    assert_eq!(
        got,
        vec![HotkeyEvent::SpeakPressed, HotkeyEvent::SpeakReleased],
        "F8 的按下与松开都该收到（按住说话就靠这个 release）"
    );
}
