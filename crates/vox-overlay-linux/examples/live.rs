//! 真开一个悬浮字幕窗，灌脚本化的假字幕，手动看效果。
//!
//! ```text
//! cargo run -p vox-overlay-linux --example live            # 默认 120 秒
//! cargo run -p vox-overlay-linux --example live -- 15      # 只跑 15 秒
//! ```
//!
//! 检查点（对着屏幕看 + 用 `xwininfo` / `xprop` 查）：
//! - 字幕出现在屏幕底部居中、置顶、无边框；
//! - 字幕后面没有实心方块（真透明）；
//! - 鼠标点上去穿透到底下的窗口（`_NET_WM_STATE` 里没有 focus，输入域为空）；
//! - 两行分色、逐字淡出。
//!
//! 这个例子自己跑 GTK 主循环（不依赖 Tauri），所以能单独验窗口这一层。

use std::time::{Duration, Instant};

use vox_core::ports::{SubtitleFrame, SubtitleLine, SubtitleView};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};

/// 脚本化的字幕流：(毫秒, 轨, 文本)。
const SCRIPT: &[(u64, Track, &str)] = &[
    (0, Track::Listen, "こんにちは、"),
    (700, Track::Listen, "こんにちは、はじめまして。"),
    (1600, Track::Speak, "你好，很高兴认识你。"),
    (2600, Track::Listen, "今天天气不错，要不要出去走走？"),
    (3800, Track::Speak, "好啊，等我把这段代码写完。"),
    (5000, Track::Listen, "字幕是给眼睛看的，不是给机器看的。"),
    (
        6200,
        Track::Speak,
        "Vox 0.1.4 · 实时语音翻译 (Tauri + Rust)",
    ),
];

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(120);

    if gtk::init().is_err() {
        eprintln!("GTK 初始化失败：需要能连上显示（DISPLAY / WAYLAND_DISPLAY）");
        std::process::exit(1);
    }

    let settings = SubtitleSettings::default();
    let overlay = match vox_overlay_linux::spawn(&settings) {
        Ok(overlay) => overlay,
        Err(e) => {
            eprintln!("起悬浮窗失败：{}", e.message);
            std::process::exit(1);
        }
    };
    println!("悬浮窗已起。跑 {seconds} 秒，期间用 xwininfo / xprop 查 Vox 字幕。");
    overlay.show();

    let started = Instant::now();
    let overlay_tick = std::sync::Arc::clone(&overlay);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let elapsed = started.elapsed().as_millis() as u64;
        let mut lines: Vec<SubtitleLine> = Vec::new();
        for (at, track, text) in SCRIPT {
            if elapsed < *at {
                continue;
            }
            // 每行只留最近 2.6 秒的内容，模拟逐字淡出后的行尾。
            let age = elapsed - at;
            let alpha = if age > 2600 {
                0.0
            } else if age > 1700 {
                1.0 - (age - 1700) as f32 / 900.0
            } else {
                1.0
            };
            if alpha <= 0.0 {
                continue;
            }
            let existing = lines.iter_mut().find(|line| line.track == *track);
            let chars: Vec<RenderedChar> =
                text.chars().map(|ch| RenderedChar { ch, alpha }).collect();
            match existing {
                Some(line) => line.chars = chars,
                None => lines.push(SubtitleLine {
                    track: *track,
                    chars,
                    color: match track {
                        Track::Listen => settings.listen_color.clone(),
                        Track::Speak => settings.speak_color.clone(),
                    },
                }),
            }
        }

        overlay_tick.render(SubtitleFrame { lines });

        if started.elapsed() >= Duration::from_secs(seconds) {
            println!("时间到，关窗退出。");
            overlay_tick.hide();
            gtk::main_quit();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });

    gtk::main();
    overlay.shutdown();
}
