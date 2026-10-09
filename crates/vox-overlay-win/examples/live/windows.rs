//! 真开一个纯显示悬浮窗，灌假字幕进去，手动看效果。
//!
//! ```text
//! cargo run -p vox-overlay-win --example live        # 默认 120 秒
//! cargo run -p vox-overlay-win --example live -- 15  # 只跑 15 秒
//! ```
//!
//! 检查点：空白时鼠标穿透、有字幕时可以拖动/缩放、字幕后面没有实心方块、
//! 两行分色且逐字淡出。

use std::time::{Duration, Instant};

use vox_core::ports::{SubtitleFrame, SubtitleLine, SubtitleView};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{SubtitleTiming, Subtitles, Track};
use vox_overlay_win::Overlay;

const SCRIPT: &[(u64, Track, &str)] = &[
    (300, Track::Listen, "こんにちは、"),
    (900, Track::Listen, "はじめまして。"),
    (1600, Track::Speak, "Hi, "),
    (2100, Track::Speak, "nice to meet you too."),
    (3000, Track::Listen, "翻译过来是：你好，初次见面。"),
    (4200, Track::Speak, "我这边说的话会走暖白那一行。"),
    (5600, Track::Listen, "안녕하세요, 한국어도 됩니다."),
    (7000, Track::Speak, "混排 mixed 123 全角＆半角ｶﾅ"),
    (
        8600,
        Track::Listen,
        "这一句故意写得很长很长，长到会超过窗口宽度，好看看左边会不会滚掉老字。",
    ),
    (10500, Track::Speak, "OK."),
];
const LOOP_MS: u64 = 13_000;
const TICK: Duration = Duration::from_millis(33);

pub fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    let settings = SubtitleSettings::default();
    let overlay = match Overlay::spawn(&settings) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("起悬浮窗失败: {}", e.message);
            std::process::exit(1);
        }
    };

    println!("纯显示悬浮窗已启动，将运行 {seconds} 秒；有字幕时可拖动/缩放窗口。");

    let mut subs = Subtitles::new(SubtitleTiming::default());
    let epoch = Instant::now();
    let mut fed = 0usize;
    let mut round = 0u64;
    let mut previous: Option<SubtitleFrame> = None;

    loop {
        let now = epoch.elapsed().as_millis() as u64;
        if now >= seconds * 1000 {
            break;
        }

        if now - round * LOOP_MS >= LOOP_MS {
            round += 1;
            fed = 0;
            subs.speak.clear();
            subs.listen.clear();
        }
        let in_round = now - round * LOOP_MS;
        while let Some(&(at, track, text)) = SCRIPT.get(fed) {
            if in_round < at {
                break;
            }
            let slot = subs.track_mut(track);
            let full = format!("{}{}", slot.open_source(), text);
            slot.update_sentence(&full, full.chars().count(), now);
            fed += 1;
        }

        subs.prune(now);
        let frame = frame_at(&subs, now);
        if previous.as_ref() != Some(&frame) {
            overlay.render(frame.clone());
            previous = Some(frame);
        }
        std::thread::sleep(TICK);
    }

    overlay.shutdown();
    println!("收工。");
}

fn frame_at(subs: &Subtitles, now_ms: u64) -> SubtitleFrame {
    let mut lines = Vec::new();
    for (track, color) in [(Track::Listen, "#eef6ff"), (Track::Speak, "#fff4de")] {
        let chars = subs.track(track).render(now_ms);
        if !chars.is_empty() {
            lines.push(SubtitleLine {
                track,
                origin: subs.track(track).origin(),
                chars,
                color: color.into(),
            });
        }
    }
    SubtitleFrame { lines }
}
