//! 视觉行级生命周期：一行只要还有活字就整行活着，整行都老了才一起淡出。
//!
//! 场景：句 1 占 1.5 行，句 2 从第 2 行中间接着写。句 1 独占的第 1 行按句 1 的时钟淡掉；
//! 与句 2 共用的第 2 行（含句 1 的尾巴）跟着句 2 的时钟，保持原样、左边不留空洞。
//! 用真实字体画（折行位置取决于字体），行位置从 `plan` 里读。

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};
use vox_overlay_core::canvas::Canvas;
use vox_overlay_core::layout::PlacedLine;
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_linux::font_factory;

const WIDTH: i32 = 880;

fn settings() -> SubtitleSettings {
    SubtitleSettings {
        lines_per_track: 3,
        font_family: String::new(),
        char_ttl_ms: 2600,
        char_fade_ms: 900,
        ..SubtitleSettings::default()
    }
}

fn frame(chars: &[char], idle: impl Fn(usize) -> u32) -> SubtitleFrame {
    SubtitleFrame {
        lines: vec![SubtitleLine {
            track: Track::Listen,
            origin: 0,
            chars: chars
                .iter()
                .enumerate()
                .map(|(i, &ch)| RenderedChar {
                    ch,
                    alpha: 1.0,
                    idle_ms: idle(i),
                })
                .collect(),
            color: "#ffffff".into(),
        }],
    }
}

fn draw(settings: &SubtitleSettings, frame: &SubtitleFrame) -> (Canvas, Vec<PlacedLine>) {
    let mut renderer = Renderer::new(settings, 96, font_factory()).expect("系统里该有字体");
    let mut canvas = Canvas::new(WIDTH, 1);
    let input = FrameInput {
        frame,
        settings,
        client_width: WIDTH,
        client_height: None,
        dpi: 96,
        now: std::time::Instant::now(),
    };
    renderer.draw(&mut canvas, &input);
    let lines = renderer.plan(&input).listen.expect("有字就该有区域").lines;
    (canvas, lines)
}

/// 这一行带内有没有任何非零 alpha 像素。
fn inked(c: &Canvas, l: &PlacedLine) -> bool {
    band(c, l).iter().any(|&a| a > 0)
}

fn band(c: &Canvas, l: &PlacedLine) -> Vec<u8> {
    let (y0, y1) = (l.y.round() as i32, l.y.round() as i32 + l.plate_h);
    let mut out = Vec::new();
    for y in y0.max(0)..y1.min(c.height()) {
        for x in 0..c.width() {
            out.push(c.pixel(x, y).a);
        }
    }
    out
}

#[test]
fn a_line_lives_as_long_as_any_of_its_chars() {
    let settings = settings();
    // 先用一整段长文字量出折行，再截成正好 3 行。
    let all: Vec<char> = "这是一段很长很长的字幕用来占满好几行，"
        .repeat(4)
        .chars()
        .collect();
    let (_, probe) = draw(&settings, &frame(&all, |_| 0));
    let probe: Vec<&PlacedLine> = probe.iter().collect();
    assert!(probe.len() >= 3);
    let chars = &all[..probe[2].last];
    // 句 1 = 第 1 行 + 第 2 行的前一半；句 2 = 其余。
    let split = probe[1].first + (probe[1].last - probe[1].first) / 2;

    let reference = draw(&settings, &frame(chars, |_| 0));
    let (fresh, lines) = draw(
        &settings,
        &frame(chars, |i| if i < split { 4000 } else { 100 }),
    );
    assert_eq!(lines.len(), 3);
    assert!(
        !inked(&fresh, &lines[0]),
        "句 1 独占的整行已淡尽：无底衬无墨迹"
    );
    assert_eq!(
        band(&fresh, &lines[1]),
        band(&reference.0, &reference.1[1]),
        "共用行跟着句 2 的时钟：连句 1 的尾巴都完全不透明、与全新一帧逐像素相同"
    );
    assert_eq!(band(&fresh, &lines[2]), band(&reference.0, &reference.1[2]));

    // 之后整块一起老：共用行淡到一半，再到全无。
    let (mid, lines) = draw(&settings, &frame(chars, |_| 2600 + 450));
    assert!(inked(&mid, &lines[1]), "淡出中途还看得见");
    assert_ne!(band(&mid, &lines[1]), band(&reference.0, &reference.1[1]));
    let (gone, lines) = draw(&settings, &frame(chars, |_| 2600 + 900));
    assert!(lines.iter().all(|l| !inked(&gone, l)), "全淡完什么都不剩");
}
