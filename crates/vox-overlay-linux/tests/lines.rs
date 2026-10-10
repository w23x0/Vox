//! 「每轨行数」设置端到端：行数 3、4 时区域真的排出并画出 3、4 行，窗口需要的高度随之增长。
//!
//! 背景：用户把 `lines_per_track` 设成 3，Linux 字幕窗却只显示 2 行。核心渲染这边由本文件钉住
//! 是对的（需要的高度、可见行数、像素都跟着行数走）；真正的毛病在 GTK 窗口没长高
//! （`window.rs`：不可缩放窗口 + 只长不缩），修法见 `placement.rs` 的 `shown_rect`。

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};
use vox_overlay_core::canvas::Canvas;
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_linux::font_factory;

const WIDTH: i32 = 880;

fn long_frame(track: Track) -> SubtitleFrame {
    let text = "这是一段很长很长的字幕用来占满好几行，".repeat(12);
    SubtitleFrame {
        lines: vec![SubtitleLine {
            track,
            origin: 0,
            chars: text
                .chars()
                .map(|ch| RenderedChar {
                    ch,
                    alpha: 1.0,
                    idle_ms: 0,
                })
                .collect(),
            color: "#ffffff".into(),
        }],
    }
}

/// 画一帧，返回 (需要的窗口高度, 可见的完整行数, 这些行里有像素的行数)。
fn draw(lines: u32, track: Track) -> (i32, usize, usize) {
    let settings = SubtitleSettings {
        lines_per_track: lines,
        font_family: String::new(),
        ..SubtitleSettings::default()
    };
    let mut renderer = Renderer::new(&settings, 96, font_factory()).expect("系统里该有字体");
    let frame = long_frame(track);
    let mut canvas = Canvas::new(WIDTH, 1);
    let mut input = FrameInput {
        frame: &frame,
        settings: &settings,
        client_width: WIDTH,
        client_height: None,
        dpi: 96,
        now: std::time::Instant::now(),
    };
    let required = renderer.draw(&mut canvas, &input).client_height;
    // 窗口恰好是需要的高度再画一遍，数可见行。
    input.client_height = Some(required);
    let out = renderer.draw(&mut canvas, &input);
    assert_eq!(out.client_height, required, "需要的高度不随内容变");
    let layout = renderer.plan(&input);
    let region = match track {
        Track::Listen => layout.listen,
        Track::Speak => layout.speak,
    }
    .expect("有字就该有区域");
    let visible: Vec<_> = region
        .lines
        .iter()
        .filter(|l| l.clip >= 1.0 && l.y >= region.region.0 as f32 - 0.5)
        .filter(|l| l.y + l.plate_h as f32 <= region.region.1 as f32 + 0.5)
        .collect();
    let inked = visible
        .iter()
        .filter(|l| {
            let y = (l.y + l.plate_h as f32 / 2.0) as i32;
            (0..canvas.width()).any(|x| canvas.pixel(x, y).a > 0)
        })
        .count();
    (required, visible.len(), inked)
}

#[test]
fn window_height_and_visible_lines_follow_lines_per_track() {
    let mut last = 0;
    for lines in 1..=4u32 {
        let (required, visible, inked) = draw(lines, Track::Listen);
        assert_eq!(
            visible, lines as usize,
            "行数 {lines}：应当完整露出 {lines} 行"
        );
        assert_eq!(inked, visible, "行数 {lines}：每个可见行都该真画出了像素");
        assert!(
            required > last,
            "行数 {lines}：需要的高度 {required} 应大于上一档 {last}"
        );
        last = required;
    }
}

/// 默认窗口高度是 170：一条轨 2 行就已经贴着它（174，只裁掉顶上 4px 空白），
/// 3 行高出一整行——不长高就只剩 2 行可见，正是用户看到的症状。
#[test]
fn three_lines_need_more_than_the_default_window_height() {
    let (two, ..) = draw(2, Track::Speak);
    let (three, ..) = draw(3, Track::Speak);
    assert!(two <= 180, "2 行一条轨基本放得进默认 170：{two}");
    assert!(
        three > 170 + 40,
        "3 行一条轨高出默认 170 一整行，窗口必须长高：{three}"
    );
}
