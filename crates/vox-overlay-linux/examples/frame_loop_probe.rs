//! 真机验收：**非建窗线程**驱动悬浮窗，证明字幕真的画到了屏幕上。
//!
//! 复刻的是产品路径那一条循环的形状（`app/src-tauri/src/overlay.rs::subtitle_loop`）：
//! 帧线程每 33ms 读一次存活探针（产品里是 `platform::overlay_running()`，Linux 侧就是
//! `Overlay::is_running`），然后把最新一帧塞进邮箱；主线程跑 GTK 主循环负责真正画。
//!
//! 它钉住的是第九轮 D1/D2 的根因：**"窗还在不在"必须是进程级事实**。第七轮那版
//! `is_running` 读的是建窗线程的 `thread_local`，于是帧线程第一句就 `break`、
//! 字幕永不渲染（渲染次数恒 0），4 秒轮询线程读到的也是另一个答案。
//!
//! ```text
//! cargo run -p vox-overlay-linux --example frame_loop_probe -- 6
//! ```
//!
//! 前置：能连上显示（`DISPLAY`，X11 / XWayland）、`xwininfo` 与 ImageMagick 的 `import`
//! （截图用；没装也不影响渲染计数那部分输出）。
//!
//! 看三件事：
//!
//! 1. `渲染次数` 随时间涨（约 30 帧/秒）——帧循环没有秒退；
//! 2. 建窗线程 / 帧线程 / 第三条线程读到的 `is_running()` 一致；
//! 3. 两张截图的像素差：`/tmp/vox-frame-loop-on.png` 里有设置里那两种字幕色，
//!    `hide()` 之后那张里一个都没有。
//!
//! ```text
//! convert /tmp/vox-frame-loop-on.png  txt:- | grep -c '#FF0000'   # 听人说话那行
//! convert /tmp/vox-frame-loop-on.png  txt:- | grep -c '#00FF00'   # 对外说话那行
//! convert /tmp/vox-frame-loop-off.png txt:- | grep -c '#FF0000'   # 隐藏后：0
//! ```

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glib::ControlFlow;
use vox_core::ports::{SubtitleFrame, SubtitleLine, SubtitleView};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};

/// 帧线程节拍：跟产品里的 `FRAME_INTERVAL_ACTIVE` 一致。
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// 画着字幕时的那张截图。
const SHOT_ON: &str = "/tmp/vox-frame-loop-on.png";
/// 同一扇窗 `hide()` 之后的那张截图（对照：字应该全没了）。
const SHOT_OFF: &str = "/tmp/vox-frame-loop-off.png";

fn main() {
    // GNOME Wayland 会话下 GTK 客户端不能自定坐标 / 置顶，装配层也是这么切的
    // （`app/src-tauri/src/platform/linux/mod.rs::pre_main`）。
    if std::env::var_os("DISPLAY").is_some() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    if gtk::init().is_err() {
        eprintln!("GTK 初始化失败：需要能连上显示（DISPLAY）");
        std::process::exit(1);
    }

    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(6);

    // 字号 48 与纯红 / 纯绿：像素断言好认（渲染器的颜色来自
    // `settings.{listen,speak}_color`，不是 `SubtitleLine.color`）。
    let settings = SubtitleSettings {
        font_size: 48,
        listen_color: "#ff0000".to_string(),
        speak_color: "#00ff00".to_string(),
        ..SubtitleSettings::default()
    };
    let overlay = match vox_overlay_linux::spawn(&settings) {
        Ok(overlay) => overlay,
        Err(error) => {
            eprintln!("起悬浮窗失败：{}", error.message);
            std::process::exit(1);
        }
    };
    overlay.show();

    let window_thread_alive = overlay.is_running();

    let renders = Arc::new(AtomicUsize::new(0));
    let probe_false = Arc::new(AtomicBool::new(false));
    let frame_thread_alive = Arc::new(AtomicBool::new(false));
    {
        let overlay = Arc::clone(&overlay);
        let renders = Arc::clone(&renders);
        let probe_false = Arc::clone(&probe_false);
        let frame_thread_alive = Arc::clone(&frame_thread_alive);
        std::thread::Builder::new()
            .name("probe-subtitle".into())
            .spawn(move || {
                let mut n = 0usize;
                loop {
                    // ↓↓↓ 产品里的第一句：`if STOP || !crate::platform::overlay_running() { break }`
                    if !overlay.is_running() {
                        probe_false.store(true, Ordering::Relaxed);
                        break;
                    }
                    frame_thread_alive.store(true, Ordering::Relaxed);
                    overlay.render(frame(n));
                    renders.fetch_add(1, Ordering::Relaxed);
                    n = n.wrapping_add(1);
                    std::thread::sleep(FRAME_INTERVAL);
                }
            })
            .expect("帧线程");
    }

    let started = Instant::now();
    let mut shot_on = false;
    let mut hidden = false;
    let mut shot_off = false;
    let tick_overlay = Arc::clone(&overlay);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let elapsed = started.elapsed();
        if !shot_on && elapsed >= Duration::from_millis(1500) {
            shot_on = true;
            println!("t=1.5s 渲染次数={}", renders.load(Ordering::Relaxed));
            shoot(SHOT_ON);
        }
        if !hidden && elapsed >= Duration::from_millis(2500) {
            hidden = true;
            // 对照：同一扇窗、同一个探针，只是把可见性关掉（`paint()` 走全透明那条路）。
            tick_overlay.hide();
        }
        if hidden && !shot_off && elapsed >= Duration::from_millis(3200) {
            shot_off = true;
            shoot(SHOT_OFF);
        }
        if elapsed >= Duration::from_secs(seconds) {
            let third_thread_alive = {
                let overlay = Arc::clone(&tick_overlay);
                std::thread::spawn(move || overlay.is_running())
                    .join()
                    .expect("第三条线程")
            };
            println!("t={seconds}s 渲染次数={}", renders.load(Ordering::Relaxed));
            println!("建窗线程   is_running() = {window_thread_alive}");
            println!(
                "帧线程     is_running() = {}（探针读到过 false 吗：{}）",
                frame_thread_alive.load(Ordering::Relaxed),
                probe_false.load(Ordering::Relaxed)
            );
            println!("第三条线程 is_running() = {third_thread_alive}");
            gtk::main_quit();
            return ControlFlow::Break;
        }
        ControlFlow::Continue
    });

    gtk::main();

    println!("关窗前 is_running() = {}", overlay.is_running());
    overlay.shutdown();
    println!("关窗之后 is_running() = {}", overlay.is_running());
}

/// 给悬浮窗拍一张：X11 下按窗口 id 抓（根窗口在 XWayland 上没有合成结果）。
fn shoot(path: &str) {
    let listing = std::process::Command::new("xwininfo")
        .args(["-root", "-tree"])
        .output();
    let id = listing
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            text.lines()
                .find(|line| line.contains("\"Vox 字幕\""))
                .and_then(|line| line.split_whitespace().next().map(str::to_string))
        });
    let Some(id) = id else {
        println!("没在 X11 树里找到悬浮窗（截图跳过）");
        return;
    };
    let shot = std::process::Command::new("import")
        .args(["-window", &id, path])
        .output();
    match shot {
        Ok(out) if out.status.success() => println!("截图已存：{path}（window {id}）"),
        Ok(out) => println!(
            "截图失败：{} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => println!("截图失败（import 没跑起来）：{e}"),
    }
}

/// 一帧：两行（Listen / Speak），颜色取设置里的纯红 / 纯绿。
///
/// 文本**短到每条轨只占一个视觉行**：视口模式的布局把整条行序列排在窗口底部，
/// 总高超过窗口高度时最上面的行会被画布裁掉（`layout_rows_in_viewport`）——
/// 字太长会让 Listen 那行整个跑到窗口外，截图上就只剩 Speak 一行。
fn frame(n: usize) -> SubtitleFrame {
    let line = |track: Track, text: String| SubtitleLine {
        track,
        chars: text
            .chars()
            .map(|ch| RenderedChar { ch, alpha: 1.0 })
            .collect(),
        color: String::new(),
    };
    SubtitleFrame {
        lines: vec![
            line(Track::Listen, format!("听人说话这一行 {n}")),
            line(Track::Speak, format!("对外说话这一行 {n}")),
        ],
    }
}
