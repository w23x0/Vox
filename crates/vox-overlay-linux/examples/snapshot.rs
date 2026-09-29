//! 离屏自检：不开窗口，用跟真窗口**完全相同**的 `Renderer` + `Canvas` 画出像素，
//! 合成到刻意刁难的背景上落盘，然后人眼过一遍。
//!
//! 跟 Windows 侧的 `snapshot` 例子同源，两点不同：
//! - 字形来自 `swash`（Linux 侧光栅器），不是 GDI —— 这一步就是在验它；
//! - 落盘写 **BMP**（30 行、零依赖、任何看图软件都认）。Windows 那边写 PNG 是因为
//!   它已经有那个 helper；为了看一眼图给 crate 拉个 `png` 依赖不值当。
//!
//! 跑法：`cargo run -p vox-overlay-linux --example snapshot`
//! 产物：`target/overlay-snapshots-linux/*.bmp`
//!
//! 为什么要合成到背景上：分层窗的像素是预乘 BGRA，单看 RGB 通道偏黑，跟屏幕上
//! 的样子不是一回事。只有按 `dst = src + dst * (1 - a)` 混一遍，看到的才是真效果。

use std::path::{Path, PathBuf};

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};
use vox_overlay_core::canvas::Canvas;
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_linux::font_factory;

const WIDTH: i32 = 880;
const DPI: u32 = 96;
const CHECKER: i32 = 12;

struct Scene {
    name: &'static str,
    what: &'static str,
    frame: SubtitleFrame,
}

fn main() {
    let dir = out_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("建输出目录 {} 失败: {e}", dir.display());
        std::process::exit(1);
    }

    let settings = SubtitleSettings::default();
    let mut renderer = match Renderer::new(&settings, DPI, font_factory()) {
        Ok(renderer) => renderer,
        Err(e) => {
            eprintln!("建字体失败: {}", e.message);
            std::process::exit(1);
        }
    };
    let mut canvas = Canvas::new(WIDTH, 1);
    let mut problems: Vec<String> = Vec::new();

    println!("输出目录: {}", dir.display());
    for scene in scenes() {
        let input = FrameInput {
            frame: &scene.frame,
            settings: &settings,
            client_width: WIDTH,
            client_height: 0,
            dpi: DPI,
        };
        let out = renderer.draw(&mut canvas, &input);
        if out.client_height != canvas.height() {
            problems.push(format!("{}: 输出高度和画布高度不一致", scene.name));
        }
        if let Some(pixel) = canvas.find_invalid_pixel() {
            problems.push(format!("{}: 画布里有非法像素 {pixel:?}", scene.name));
        }
        if scene.frame.lines.iter().any(|line| !line.chars.is_empty()) {
            let ink = canvas.bytes().iter().filter(|&&b| b != 0).count();
            if ink == 0 {
                problems.push(format!("{}: 一帧有字的画面却是全透明", scene.name));
            }
        }

        let backdrop = backdrop(canvas.width(), canvas.height());
        let composed = composite(&canvas, &backdrop);
        let path = dir.join(format!("{}.bmp", scene.name));
        if let Err(e) = write_bmp(&path, canvas.width(), canvas.height(), &composed) {
            problems.push(format!("{}: 写 BMP 失败 {e}", scene.name));
            continue;
        }
        println!(
            "  {:<14} {:>4}x{:<4} 该看: {}",
            scene.name,
            canvas.width(),
            canvas.height(),
            scene.what
        );
    }

    if problems.is_empty() {
        println!("全部通过。用看图软件打开上面目录里的 BMP 再肉眼过一遍。");
    } else {
        for problem in &problems {
            eprintln!("问题: {problem}");
        }
        std::process::exit(1);
    }
}

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/overlay-snapshots-linux")
}

fn line(track: Track, text: &str, color: &str, alpha: f32) -> SubtitleLine {
    SubtitleLine {
        track,
        chars: text.chars().map(|ch| RenderedChar { ch, alpha }).collect(),
        color: color.into(),
    }
}

/// 造几条有代表性的字幕：双行分色、纯中文、中英混排、逐字淡出中途、全透明。
fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            name: "two-rows",
            what: "听人（冷白）在上、对外（暖白）在下，行间距均匀",
            frame: SubtitleFrame {
                lines: vec![
                    line(Track::Listen, "こんにちは、はじめまして。", "#eef6ff", 1.0),
                    line(Track::Speak, "你好，很高兴认识你。", "#fff4de", 1.0),
                ],
            },
        },
        Scene {
            name: "cjk-only",
            what: "汉字笔画多，边缘不该有黑边或彩边",
            frame: SubtitleFrame {
                lines: vec![line(
                    Track::Listen,
                    "字幕是给眼睛看的，不是给机器看的。",
                    "#eef6ff",
                    1.0,
                )],
            },
        },
        Scene {
            name: "mixed",
            what: "中英数字混排，标点与半角字符的宽度是否协调",
            frame: SubtitleFrame {
                lines: vec![line(
                    Track::Speak,
                    "Vox 0.1.4 · 实时语音翻译 (Tauri + Rust)",
                    "#fff4de",
                    1.0,
                )],
            },
        },
        Scene {
            name: "fading",
            what: "逐字淡出中途：靠后的字更淡，且没有硬边",
            frame: SubtitleFrame {
                lines: vec![line(Track::Listen, "淡出中的这一行文字", "#eef6ff", 0.35)],
            },
        },
        Scene {
            name: "listen-overflow",
            what: "超长行（听人轨）从左边滚掉最老的字，最新的字留在可见范围里",
            frame: SubtitleFrame {
                lines: vec![line(
                    Track::Listen,
                    "这一行故意写得非常长，长到超过窗口宽度，用来验证左侧会不会把最老的字滚掉，\
                     而最新说出来的这几个字必须始终留在可见范围里。",
                    "#eef6ff",
                    1.0,
                )],
            },
        },
        Scene {
            name: "empty",
            what: "空帧：整块应当全透明（只剩背景棋盘格）",
            frame: SubtitleFrame { lines: Vec::new() },
        },
    ]
}

/// 黑到白横向渐变 + 棋盘格：前者探预乘错误（忘了预乘会在暗处露亮边），
/// 后者探"实心底块"（半透明底衬会把格子盖掉）。
fn backdrop(w: i32, h: i32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / (w.max(1) - 1).max(1) as f32;
            let base = (t * 255.0) as u8;
            let checker = ((x / CHECKER) + (y / CHECKER)) % 2 == 0;
            let v = if checker {
                base
            } else {
                base.saturating_sub(40)
            };
            let idx = ((y * w + x) * 4) as usize;
            out[idx] = v;
            out[idx + 1] = v;
            out[idx + 2] = v;
            out[idx + 3] = 255;
        }
    }
    out
}

/// 按分层窗的规则把画布混到背景上：`dst = src + dst * (1 - a)`（源是预乘的）。
fn composite(canvas: &Canvas, backdrop: &[u8]) -> Vec<u8> {
    let mut out = backdrop.to_vec();
    let bytes = canvas.bytes();
    for (pixel, source) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let alpha = source[3] as u32;
        let idx = pixel * 4;
        for channel in 0..3 {
            let src = source[channel] as u32;
            let dst = out[idx + channel] as u32;
            out[idx + channel] = (src + dst * (255 - alpha) / 255).min(255) as u8;
        }
    }
    out
}

/// 写一张 24 位 BMP（BGR 顺序、行按 4 字节对齐）。零依赖，任何看图软件都认。
fn write_bmp(path: &Path, w: i32, h: i32, rgba: &[u8]) -> std::io::Result<()> {
    let row_bytes = w * 3;
    let padding = (4 - row_bytes % 4) % 4;
    let image_size = (row_bytes + padding) * h;
    let file_size = 54 + image_size;

    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // BMP 的行是自下而上存的。
    for y in (0..h).rev() {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            out.push(rgba[idx + 2]);
            out.push(rgba[idx + 1]);
            out.push(rgba[idx]);
        }
        out.extend(std::iter::repeat_n(0u8, padding as usize));
    }

    std::fs::write(path, out)
}
