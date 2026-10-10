//! 「调整位置」模式冒烟：起窗，1 秒后进编辑模式，几何回写时打印；到点自动退出编辑并关窗。
//!
//! ```text
//! cargo run -p vox-overlay-linux --example edit_probe -- 20
//! ```
//! 期间可用 xdotool 移动/缩放窗口（`xdotool search --name "Vox 字幕"`），看有没有 `GEOMETRY ...` 输出。

use std::sync::Arc;
use std::time::Duration;

use vox_core::ports::SubtitleView;
use vox_core::settings::SubtitleSettings;

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(20);
    if gtk::init().is_err() {
        eprintln!("GTK 初始化失败：需要能连上显示");
        std::process::exit(1);
    }
    let overlay = vox_overlay_linux::spawn_with_geometry(
        &SubtitleSettings::default(),
        Arc::new(|g| println!("GEOMETRY {} {} {} {}", g.x, g.y, g.width, g.height)),
    )
    .expect("起悬浮窗");
    overlay.show();
    let o = Arc::clone(&overlay);
    glib::timeout_add_local_once(Duration::from_secs(1), move || {
        println!("EDIT ON");
        o.set_edit_mode(true);
    });
    let o = Arc::clone(&overlay);
    glib::timeout_add_local_once(Duration::from_secs(seconds), move || {
        o.set_edit_mode(false);
        println!("EDIT OFF");
        glib::timeout_add_local_once(Duration::from_millis(500), gtk::main_quit);
    });
    gtk::main();
    overlay.shutdown();
}
