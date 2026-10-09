//! 离屏自检：不开窗口，用跟真窗口**完全相同**的 `Renderer` + `Canvas` 画出像素，
//! 合成到刻意刁难的背景上写成 PNG，然后人眼过一遍。
//!
//! 为什么要合成到背景上：分层窗的像素是预乘 BGRA，单看 RGB 通道偏黑，跟屏幕上
//! 的样子不是一回事。只有按 `UpdateLayeredWindow` 的 `AC_SRC_ALPHA` 规则
//! （`dst = src + dst * (1 - a)`）混一遍，看到的才是真效果。
//!
//! 背景选了黑到白的横向渐变 + 12 像素棋盘格，两样都是探针：
//! - 渐变：忘了预乘的光晕会在暗处露出亮边，反过来在亮处露出暗边
//! - 棋盘格：实心底块会把格子盖掉，一眼就能看出来
//!
//! 跑法：`cargo run -p vox-overlay-win --example snapshot`
//! 产物：`target/overlay-snapshots/*.png`

#[path = "../support/png.rs"]
mod png;

use std::path::PathBuf;

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, SubtitleTiming, SubtitleTrack, Track};
use vox_overlay_win::canvas::Canvas;
use vox_overlay_win::render::{FrameInput, Renderer};

/// 快照统一用默认窗口宽度，出来的图跟实际窗口一样宽。
const WIDTH: i32 = 880;
const DPI: u32 = 96;
/// 棋盘格边长。比字画细节大一圈，盖没了很显眼。
const CHECKER: i32 = 12;

/// 一张要出的图。
struct Scene {
    /// 文件名前缀。
    name: &'static str,
    /// 这张图要证明什么，打印出来提醒自己该看哪儿。
    what: &'static str,
    frame: SubtitleFrame,
}

pub fn main() {
    let dir = out_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("建输出目录 {} 失败: {e}", dir.display());
        std::process::exit(1);
    }

    let settings = SubtitleSettings::default();
    let mut renderer = match Renderer::new(&settings, DPI, vox_overlay_win::font_factory()) {
        Ok(r) => r,
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
            client_height: None,
            now: std::time::Instant::now(),
            dpi: DPI,
        };
        let out = renderer.draw(&mut canvas, &input);
        problems.extend(inspect(&scene, &canvas));
        if out.client_height != canvas.height() {
            problems.push(format!("{}: 输出高度和画布高度不一致", scene.name));
        }

        let bg = backdrop(canvas.width(), canvas.height());
        let over = composite(&canvas, &bg);
        let path = dir.join(format!("{}.png", scene.name));
        if let Err(e) = png::write_rgba(&path, canvas.width() as u32, canvas.height() as u32, &over)
        {
            problems.push(format!("{}: 写 PNG 失败 {e}", scene.name));
            continue;
        }
        // 单独出一张 alpha 灰度图：底衬和字的形状一目了然，实心块无处可藏。
        let mask = dir.join(format!("{}-alpha.png", scene.name));
        let gray = alpha_channel(&canvas);
        if let Err(e) = png::write_gray(&mask, canvas.width() as u32, canvas.height() as u32, &gray)
        {
            problems.push(format!("{}: 写 alpha PNG 失败 {e}", scene.name));
        }
        println!(
            "  {:<16} {:>4}x{:<4} 该看: {}",
            scene.name,
            canvas.width(),
            canvas.height(),
            scene.what
        );
    }

    if problems.is_empty() {
        println!("\n机检全过。剩下的靠眼睛：把上面那几张 PNG 打开看。");
    } else {
        eprintln!("\n机检发现 {} 个问题:", problems.len());
        for p in &problems {
            eprintln!("  - {p}");
        }
        std::process::exit(1);
    }
}

/// 要出的几张图。每张都盯一个具体的失败模式。
fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            name: "1-passthrough",
            what: "两行分色（上冷白下暖白）+ 右侧未确认的尾巴（0.7 倍不透明度）；四角必须透，底衬能透出棋盘格",
            frame: SubtitleFrame {
                lines: vec![
                    line(
                        Track::Listen,
                        "#eef6ff",
                        tail_dimmed("你好，这是听人说话那一行的字幕。", 8),
                    ),
                    line(
                        Track::Speak,
                        "#fff4de",
                        tail_dimmed("Hello VRChat, my mic is live 24/7.", 18),
                    ),
                ],
            },
        },
        Scene {
            name: "2-two-rows",
            what: "纯显示双行：只有字幕底衬和文字，不应出现控件、状态或整窗面板",
            frame: SubtitleFrame {
                lines: vec![
                    line(Track::Listen, "#eef6ff", opaque("对面刚说完的一句话。")),
                    line(Track::Speak, "#fff4de", opaque("我这边正在说的一句话。")),
                ],
            },
        },
        Scene {
            name: "3-cjk-mixed",
            what: "中日韩混排 + 半角/全角/标点/数字，不许出豆腐块（□）",
            frame: SubtitleFrame {
                lines: vec![
                    line(
                        Track::Listen,
                        "#eef6ff",
                        opaque("日本語のテキスト、한국어 텍스트、中文文本 mixed 123"),
                    ),
                    line(
                        Track::Speak,
                        "#fff4de",
                        opaque("ｱｲｳ 半角ｶﾅ ＆ 全角記号「引用」…—"),
                    ),
                ],
            },
        },
        Scene {
            name: "4-single-row",
            what: "只有一行时窗口变矮，另一行不留空位",
            frame: SubtitleFrame {
                lines: vec![line(
                    Track::Listen,
                    "#eef6ff",
                    opaque("只有听人说话这一行在跑。"),
                )],
            },
        },
        Scene {
            name: "5-overflow",
            what: "超长行从左边滚掉最老的字，最新的字贴着右边界，底衬跟着文字宽",
            frame: SubtitleFrame {
                lines: vec![line(
                    Track::Listen,
                    "#eef6ff",
                    opaque(
                        "这一行故意写得非常长，长到超过窗口宽度，用来验证左侧会不会把最老的字滚掉，\
                         而最新说出来的这几个字必须始终留在可见范围里。",
                    ),
                )],
            },
        },
    ]
}

/// 拼一行。`color` 是给应用层看的字段，真正用哪个色由 `SubtitleSettings` 定。
fn line(track: Track, color: &str, chars: Vec<RenderedChar>) -> SubtitleLine {
    SubtitleLine {
        track,
        chars,
        color: color.into(),
    }
}

/// 全不透明的一行，用来看排版和字形。
fn opaque(text: &str) -> Vec<RenderedChar> {
    text.chars()
        .map(|ch| RenderedChar { ch, alpha: 1.0 })
        .collect()
}

/// 走**内核真正的字幕模型**：前 `confirmed` 个字已确认（全不透明），其后是还会变的
/// 尾巴（0.7 倍不透明度）。这样快照证明"未确认尾巴更暗"这条链路是通的。
fn tail_dimmed(text: &str, confirmed: usize) -> Vec<RenderedChar> {
    let mut track = SubtitleTrack::new(SubtitleTiming::default());
    track.update_sentence(text, confirmed, 0);
    track.render(0)
}

/// 机检能机检的那一半：预乘不变式、四角透明、半透明像素存在、未确认尾巴的暗度真的落到
/// 了像素上。
///
/// "字清不清晰、有没有豆腐块"机器判不了，那部分留给 PNG 和眼睛。
fn inspect(scene: &Scene, canvas: &Canvas) -> Vec<String> {
    let mut out = Vec::new();
    let name = scene.name;
    let (w, h) = (canvas.width(), canvas.height());
    if w <= 0 || h <= 0 {
        out.push(format!("{name}: 画布是空的 {w}x{h}"));
        return out;
    }

    // 预乘不变式：颜色通道超过 alpha 就是忘了预乘，屏幕上会冒一圈亮边。
    if let Some((x, y, px)) = canvas.find_invalid_pixel() {
        out.push(format!("{name}: ({x},{y}) 预乘非法 {px:?}，屏幕上会是光晕"));
    }

    // 四角不许有实心底块。分层窗里只要有一块不透明矩形，桌面上就是个方块——
    // 这正是 WebView2 那条路踩的坑，这里当回归测试钉住。
    //
    // 纯显示模式没有整窗面板，四角必须全透。
    const PROBE: i32 = 5;
    let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];
    let bad = corners
        .into_iter()
        .flat_map(|(cx, cy)| {
            let (sx, sy) = (cx.min(w - PROBE).max(0), cy.min(h - PROBE).max(0));
            (0..PROBE).flat_map(move |dy| (0..PROBE).map(move |dx| (sx + dx, sy + dy)))
        })
        .find(|&(x, y)| canvas.pixel(x, y).a != 0);
    if let Some((x, y)) = bad {
        let a = canvas.pixel(x, y).a;
        out.push(format!("{name}: 穿透态角上 ({x},{y}) alpha={a}，不是全透"));
    }

    let mut opaque_px = 0usize;
    let mut partial_px = 0usize;
    for y in 0..h {
        for x in 0..w {
            match canvas.pixel(x, y).a {
                255 => opaque_px += 1,
                0 => {}
                _ => partial_px += 1,
            }
        }
    }
    let total = (w as usize) * (h as usize);
    if opaque_px * 2 > total {
        out.push(format!(
            "{name}: {opaque_px}/{total} 像素全不透明，看着像实心底块"
        ));
    }
    if partial_px == 0 {
        out.push(format!(
            "{name}: 一个半透明像素都没有，抗锯齿和淡出都没生效"
        ));
    }

    // 每列的峰值 alpha。底衬本身是 165，只有字才会把某一列顶到更高。
    let col_max: Vec<u8> = (0..w)
        .map(|x| (0..h).map(|y| canvas.pixel(x, y).a).max().unwrap_or(0))
        .collect();
    let text_cols: Vec<usize> = col_max
        .iter()
        .enumerate()
        .filter(|(_, &a)| a > 175)
        .map(|(i, _)| i)
        .collect();

    // 这一帧本来就有未确认的尾巴，那右边的峰值就必须比左边低——否则说明逐字 alpha
    // 在某一层被抹平成了"整行一个透明度"。
    let fading = scene
        .frame
        .lines
        .iter()
        .flat_map(|l| &l.chars)
        .any(|c| c.alpha < 0.99);
    if fading {
        if text_cols.len() < 20 {
            out.push(format!(
                "{name}: 只找到 {} 列文字像素，渲染大概整体没出来",
                text_cols.len()
            ));
        } else {
            let edge = (text_cols.len() / 5).max(1);
            let peak = |cols: &[usize]| cols.iter().map(|&i| col_max[i]).max().unwrap_or(0);
            let left = peak(&text_cols[..edge]);
            let right = peak(&text_cols[text_cols.len() - edge..]);
            if right >= left {
                out.push(format!(
                    "{name}: 右侧峰值 alpha {right} 不低于左侧 {left}，未确认尾巴的暗度没落到像素上"
                ));
            }
        }
    }

    out.extend(inspect_row_hues(scene, canvas));
    out
}

/// 两行的字色到底是不是一冷一暖——机检，不靠眼睛判「这个白是偏蓝还是偏黄」。
///
/// 判据：`listen` 是 `#eef6ff`（b > r），`speak` 是 `#fff4de`（r > b）。预乘之后
/// 通道等比缩放，所以 b 和 r 的**大小关系**不受 alpha 影响，全不透明的字像素上
/// 直接比就行。两行分别取自己那半边最亮的字像素。
fn inspect_row_hues(scene: &Scene, canvas: &Canvas) -> Vec<String> {
    let mut out = Vec::new();
    let tracks: Vec<Track> = scene.frame.lines.iter().map(|l| l.track).collect();
    if !tracks.contains(&Track::Listen) || !tracks.contains(&Track::Speak) {
        return out; // 只有一行时没得比。
    }
    let (w, h) = (canvas.width(), canvas.height());
    // 上半找听人行、下半找对外行。行序本身另有单测钉着（listen 在上）。
    let brightest = |y0: i32, y1: i32| {
        let mut best = (0i32, None);
        for y in y0..y1 {
            for x in 0..w {
                let p = canvas.pixel(x, y);
                if p.a < 250 {
                    continue; // 只看全不透明的字芯，抗锯齿边缘的比例不可靠。
                }
                let lum = p.b as i32 + p.g as i32 + p.r as i32;
                if lum > best.0 {
                    best = (lum, Some(p));
                }
            }
        }
        best.1
    };
    let mid = h / 2;
    for (what, px, want_cool) in [
        ("听人行", brightest(0, mid), true),
        ("对外行", brightest(mid, h), false),
    ] {
        let Some(p) = px else {
            out.push(format!("{}: {what}找不到不透明的字像素", scene.name));
            continue;
        };
        let cool = p.b > p.r;
        if cool != want_cool {
            let want = if want_cool {
                "冷白 b>r"
            } else {
                "暖白 r>b"
            };
            out.push(format!(
                "{}: {what}的字色是 b={} g={} r={}，该是{want}——两行分色错了",
                scene.name, p.b, p.g, p.r
            ));
        }
    }
    out
}

/// 刁难用的背景：左黑右白的横向渐变 + 棋盘格。RGBA，不透明。
///
/// 渐变让光晕（暗处的亮边）和反向错误（亮处的暗边）都藏不住；棋盘格是"底衬到底
/// 是半透明还是实心"的探针——半透明的话格子会透过来。
fn backdrop(w: i32, h: i32) -> Vec<u8> {
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let mut px = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let ramp = if w > 1 { x * 255 / (w - 1) } else { 0 };
            let checker = if (x / CHECKER + y / CHECKER) % 2 == 0 {
                22
            } else {
                -22
            };
            let v = (ramp + checker).clamp(0, 255) as u8;
            px.extend_from_slice(&[v, v, v, 255]);
        }
    }
    px
}

/// 按分层窗的规则把预乘像素合到背景上：`dst = src + dst * (1 - a)`。
///
/// src 已经预乘，所以这里是直接相加而不是再乘一次 alpha——多乘一次就是常见的
/// "字发灰"，少预乘一次就是光晕。
fn composite(canvas: &Canvas, bg: &[u8]) -> Vec<u8> {
    let (w, h) = (canvas.width(), canvas.height());
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(bg.len());
    for (y, row) in bg.chunks_exact((w as usize) * 4).enumerate() {
        for (x, dst) in row.as_chunks::<4>().0.iter().enumerate() {
            let s = canvas.pixel(x as i32, y as i32);
            let inv = 255 - s.a as u32;
            let mix = |src: u8, d: u8| (src as u32 + d as u32 * inv / 255).min(255) as u8;
            out.extend_from_slice(&[mix(s.r, dst[0]), mix(s.g, dst[1]), mix(s.b, dst[2]), 255]);
        }
    }
    out
}

/// 只把 alpha 通道抠出来当灰度图。看形状用：底衬的圆角、字的轮廓、有没有实心块。
fn alpha_channel(canvas: &Canvas) -> Vec<u8> {
    let (w, h) = (canvas.width(), canvas.height());
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| canvas.pixel(x, y).a)
        .collect()
}

/// 快照写到 target 里，跟构建产物放一块，不脏工作树。
fn out_dir() -> PathBuf {
    // current_exe 是 `<target>/<profile>/examples/snapshot.exe`，往上三级正好是 target。
    if let Some(target) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.ancestors().nth(3).map(PathBuf::from))
    {
        return target.join("overlay-snapshots");
    }
    PathBuf::from("target/overlay-snapshots")
}
