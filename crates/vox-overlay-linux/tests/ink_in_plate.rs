//! 回归：字幕墨迹必须完整落在底衬圆角框内（含上下内边距）。
//!
//! 做法：同一帧画两次——字符 alpha=1（有墨迹）与 alpha≈1/255（几乎只有底衬，布局完全相同）。
//! 两张画布不同的像素就是墨迹；底衬矩形取只含底衬那张里 alpha>0 的像素。
//! 用真实的系统字体（swash 光栅器），因为 bug 出在字体度量上。

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};
use vox_overlay_core::canvas::Canvas;
use vox_overlay_core::layout::Metrics;
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_linux::font_factory;

const WIDTH: i32 = 900;

fn frame(lines: &[(Track, &str)], alpha: f32) -> SubtitleFrame {
    SubtitleFrame {
        lines: lines
            .iter()
            .map(|(track, text)| SubtitleLine {
                track: *track,
                origin: 0,
                chars: text
                    .chars()
                    .map(|ch| RenderedChar {
                        ch,
                        alpha,
                        idle_ms: 0,
                    })
                    .collect(),
                color: "#ffffff".into(),
            })
            .collect(),
    }
}

fn render(settings: &SubtitleSettings, dpi: u32, frame: &SubtitleFrame) -> Canvas {
    let mut renderer = Renderer::new(settings, dpi, font_factory()).expect("系统里该有字体");
    let mut canvas = Canvas::new(WIDTH, 1);
    renderer.draw(
        &mut canvas,
        &FrameInput {
            frame,
            settings,
            client_width: WIDTH,
            client_height: None,
            now: std::time::Instant::now(),
            dpi,
        },
    );
    canvas
}

fn alpha_at(c: &Canvas, x: i32, y: i32) -> u8 {
    c.bytes()[((y * c.width() + x) * 4 + 3) as usize]
}

/// 底衬与墨迹的包围盒检查；返回违规描述。
fn check(lines: &[(Track, &str)], font_size: u32, dpi: u32) -> Vec<String> {
    let settings = SubtitleSettings {
        font_size,
        ..SubtitleSettings::default()
    };
    // 「只有底衬」这一帧不能用 alpha=0：整行全透明时渲染器不再画底衬（逐句淡出的行为），
    // 所以用肉眼不可见、但量化后不为 0 的 alpha（≈1/255），墨迹贡献的像素差仍远小于 alpha=1。
    let plates = render(&settings, dpi, &frame(lines, 1.0 / 255.0));
    let full = render(&settings, dpi, &frame(lines, 1.0));
    assert_eq!(plates.height(), full.height());
    let (w, h) = (plates.width(), plates.height());
    let pad_y = Metrics::for_dpi(dpi).plate_pad_y;

    // 底衬按 y 方向连续的"有 alpha 的行"分组：(top, bottom_excl, left, right_excl)。
    let mut bands: Vec<(i32, i32, i32, i32)> = Vec::new();
    for y in 0..h {
        let xs: Vec<i32> = (0..w).filter(|&x| alpha_at(&plates, x, y) > 0).collect();
        let (Some(&l), Some(&r)) = (xs.first(), xs.last()) else {
            continue;
        };
        match bands.last_mut() {
            Some(b) if b.1 == y => {
                b.1 = y + 1;
                b.2 = b.2.min(l);
                b.3 = b.3.max(r + 1);
            }
            _ => bands.push((y, y + 1, l, r + 1)),
        }
    }
    let mut problems = Vec::new();
    // 一段字可能折成好几个视觉行（大字号 / 高 DPI 时），所以底衬数只会 >= 段数。
    if bands.len() < lines.len() {
        problems.push(format!("底衬数 {} < 段数 {}", bands.len(), lines.len()));
        return problems;
    }
    // 墨迹像素：两张画布不同。归入垂直中心最近的底衬带。
    let mut ink: Vec<Option<(i32, i32, i32, i32)>> = vec![None; bands.len()];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if plates.bytes()[i..i + 4] == full.bytes()[i..i + 4] {
                continue;
            }
            let k = bands
                .iter()
                .enumerate()
                .min_by_key(|(_, b)| (y - (b.0 + b.1) / 2).abs())
                .map(|(k, _)| k)
                .unwrap();
            let e = ink[k].get_or_insert((y, y + 1, x, x + 1));
            e.0 = e.0.min(y);
            e.1 = e.1.max(y + 1);
            e.2 = e.2.min(x);
            e.3 = e.3.max(x + 1);
        }
    }
    for (k, (b, i)) in bands.iter().zip(&ink).enumerate() {
        let Some(i) = i else {
            problems.push(format!("行 {k} 没有墨迹"));
            continue;
        };
        // 墨迹必须在底衬内缩一个上下内边距之内。
        if i.0 < b.0 + pad_y || i.1 > b.1 - pad_y {
            problems.push(format!(
                "行 {k} size={font_size} dpi={dpi}: 墨迹 y=[{},{}) 超出底衬 y=[{},{}) 内缩 {pad_y}px",
                i.0, i.1, b.0, b.1
            ));
        }
        if i.2 < b.2 || i.3 > b.3 {
            problems.push(format!(
                "行 {k}: 墨迹 x=[{},{}) 超出底衬 x=[{},{})",
                i.2, i.3, b.2, b.3
            ));
        }
    }
    problems
}

fn assert_clean(lines: &[(Track, &str)], sizes: &[u32], dpis: &[u32]) {
    let mut all = Vec::new();
    for &s in sizes {
        for &d in dpis {
            all.extend(check(lines, s, d));
        }
    }
    assert!(all.is_empty(), "墨迹越出底衬：\n{}", all.join("\n"));
}

#[test]
fn english_descenders_stay_inside_plate() {
    assert_clean(
        &[(Track::Speak, "typography jumping gypsy quickly")],
        &[16, 24, 32, 48],
        &[96, 144, 192],
    );
}

#[test]
fn cjk_stays_inside_plate() {
    assert_clean(
        &[(Track::Listen, "字幕是给眼睛看的，不是给机器看的。")],
        &[16, 24, 32, 48],
        &[96, 144, 192],
    );
}

#[test]
fn mixed_two_rows_stay_inside_plates() {
    assert_clean(
        &[
            (Track::Listen, "こんにちは gypsy 世界、よろしく"),
            (Track::Speak, "Vox 实时翻译 (Tauri + Rust) jpqy,;"),
        ],
        &[20, 32, 40],
        &[96, 120, 192],
    );
}
