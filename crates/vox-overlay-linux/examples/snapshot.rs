//! 离屏自检：不开窗口，用跟真窗口**完全相同**的 `Renderer` + `Canvas` 画出像素，
//! 合成到刻意刁难的背景上落成 PNG，然后人眼过一遍。
//!
//! 这一版驱动的是**真的字幕模型**（`Subtitles::update_sentence` / `finish_sentence`）加
//! **真的渲染器**，按时间轴一帧一帧推进，在指定时刻截图：改译、上浮动画的中间帧、
//! 两条轨同时在跑，都是时间轴上的几个截图点，不是手写一串 alpha。
//!
//! 跑法：`cargo run -p vox-overlay-linux --example snapshot [输出目录]`
//! 缺省输出目录：`/tmp/claude-1000/overlay-captions/`
//!
//! 复杂文字一组（阿拉伯语 / 希伯来语 / 泰语 / 印地语 / 双向混排 / RTL 句子增长）：
//! `cargo run -p vox-overlay-linux --example snapshot shaping [输出目录]`
//! 缺省输出目录：`/tmp/claude-1000/overlay-shaping/`
//!
//! 为什么要合成到背景上：分层窗的像素是预乘 BGRA，单看 RGB 通道偏黑，跟屏幕上
//! 的样子不是一回事。只有按 `dst = src + dst * (1 - a)` 混一遍，看到的才是真效果。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{confirmed_char_count, SubtitleTiming, Subtitles, Track};
use vox_overlay_core::canvas::Canvas;
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_linux::font_factory;

const WIDTH: i32 = 880;
const DPI: u32 = 96;
const CHECKER: i32 = 12;
/// 时间轴步长（约 60 fps）。
const TICK_MS: u64 = 16;

/// 时间轴上的一个事件。
enum Event {
    /// 服务端推来"到目前为止的整句"，`confirmed` 是其中已确认的前缀。
    Update(Track, &'static str, &'static str),
    /// 这一句说完了，定稿。
    Finish(Track, &'static str),
    /// 模型按预算丢掉队首的定稿句，只留 `usize` 个定稿字（模拟 Runtime 的截断）。
    Trim(Track, usize),
}

struct Scene {
    name: &'static str,
    what: &'static str,
    events: Vec<(u64, Event)>,
    /// 截图时刻（毫秒）。
    shot_at: u64,
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let shaping = args.first().is_some_and(|a| a == "shaping");
    if shaping {
        args.remove(0);
    }
    let dir = args.first().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(if shaping {
            "/tmp/claude-1000/overlay-shaping"
        } else {
            "/tmp/claude-1000/overlay-captions"
        })
    });
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("建输出目录 {} 失败: {e}", dir.display());
        std::process::exit(1);
    }

    let mut problems: Vec<String> = Vec::new();
    let mut before_trim: Option<Vec<u8>> = None;
    println!("输出目录: {}", dir.display());
    for scene in if shaping { shaping_scenes() } else { scenes() } {
        let settings = SubtitleSettings::default();
        let mut renderer = match Renderer::new(&settings, DPI, font_factory()) {
            Ok(renderer) => renderer,
            Err(e) => {
                eprintln!("建字体失败: {}", e.message);
                std::process::exit(1);
            }
        };
        let mut canvas = Canvas::new(WIDTH, 1);
        let timing = SubtitleTiming {
            char_ttl_ms: 60_000,
            char_fade_ms: 900,
            dim_zeros: false,
            dim_alpha: 0.3,
        };
        let mut subs = Subtitles::new(timing);
        let epoch = Instant::now();
        let mut next = 0usize;
        let mut now = 0u64;
        loop {
            while let Some((at, event)) = scene.events.get(next) {
                if *at > now {
                    break;
                }
                match event {
                    Event::Update(track, full, confirmed) => {
                        let n = confirmed_char_count(full, Some(confirmed));
                        subs.track_mut(*track).update_sentence(full, n, now);
                    }
                    Event::Finish(track, text) => subs.track_mut(*track).finish_sentence(text, now),
                    Event::Trim(track, keep) => {
                        subs.track_mut(*track).trim_sealed(*keep);
                    }
                }
                next += 1;
            }
            let frame = frame_at(&subs, now);
            let input = FrameInput {
                frame: &frame,
                settings: &settings,
                client_width: WIDTH,
                client_height: None,
                dpi: DPI,
                now: epoch + Duration::from_millis(now),
            };
            // 每个节拍都画：上浮动画靠一帧一帧推进，跳着画就没有中间帧了。
            renderer.draw(&mut canvas, &input);
            if now >= scene.shot_at {
                break;
            }
            now += TICK_MS;
        }

        if let Some(pixel) = canvas.find_invalid_pixel() {
            problems.push(format!("{}: 画布里有非法像素 {pixel:?}", scene.name));
        }
        let ink = canvas.bytes().iter().filter(|&&b| b != 0).count();
        if ink == 0 {
            problems.push(format!("{}: 一帧有字的画面却是全透明", scene.name));
        }
        // 截断前后对比：同一段内容，模型丢掉队首之后画面必须逐像素相同。
        if scene.name.starts_with("5a") {
            before_trim = Some(canvas.bytes().to_vec());
        }
        if scene.name.starts_with("5b") {
            match &before_trim {
                Some(before) if before.as_slice() == canvas.bytes() => {
                    println!("  截断前后画面逐像素相同");
                }
                _ => problems.push("5b: 模型截断队首之后画面和截断前不一致（可见字重排了）".into()),
            }
        }
        let backdrop = backdrop(canvas.width(), canvas.height());
        let composed = composite(&canvas, &backdrop);
        let path = dir.join(format!("{}.png", scene.name));
        if let Err(e) = write_png(&path, canvas.width(), canvas.height(), &composed) {
            problems.push(format!("{}: 写 PNG 失败 {e}", scene.name));
            continue;
        }
        println!(
            "  {:<22} {:>4}x{:<4} 动画中: {:<5} 该看: {}",
            scene.name,
            canvas.width(),
            canvas.height(),
            renderer.is_animating(),
            scene.what
        );
    }

    if problems.is_empty() {
        println!("全部通过。用看图软件打开上面目录里的 PNG 再肉眼过一遍。");
    } else {
        for problem in &problems {
            eprintln!("问题: {problem}");
        }
        std::process::exit(1);
    }
}

fn frame_at(subs: &Subtitles, now: u64) -> SubtitleFrame {
    let mut lines = Vec::new();
    for (track, color) in [(Track::Listen, "#eef6ff"), (Track::Speak, "#fff4de")] {
        let chars: Vec<_> = subs
            .track(track)
            .render(now)
            .into_iter()
            .filter(|c| c.alpha > 0.0)
            .collect();
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

const LONG: &str = "The quick brown fox jumps over the lazy dog, 然后它又跳过了第二只更懒的狗，\
                    and then something unexpected happened 就在那一刻字幕必须折行而且不能乱跳。";

/// 连续说话：每 120ms 一句，不静默。返回事件序列与最后一句结束的时刻。
fn nonstop_speech() -> (Vec<(u64, Event)>, u64) {
    const SENTENCES: [&str; 6] = [
        "The quick brown fox jumps over the lazy dog, 然后它又跳过了第二只狗。",
        "字幕必须折行而且不能乱跳，and every word keeps its place on screen.",
        "实时翻译 is hard when 人们说话不停顿，but the caption just glides up.",
        "好的，supercalifragilistic 这个词很长，so it must wrap by whole words.",
        "你好，hello world，今天天气不错，let us go for a walk in the park.",
        "最后一句话，this is the last sentence before we trim the history.",
    ];
    let mut events = Vec::new();
    let mut t = 0u64;
    for i in 0..18 {
        let text = SENTENCES[i % SENTENCES.len()];
        events.push((t, Event::Update(Track::Listen, text, text)));
        events.push((t + 60, Event::Finish(Track::Listen, text)));
        t += 120;
    }
    (events, t)
}

const ARABIC: &str = "مرحبا بكم في ترجمة الصوت الفورية، هذا اختبار للخط العربي المتصل";
const HEBREW: &str = "שלום עולם, זוהי בדיקה של כתיבה מימין לשמאל עם ניקוד: בְּרֵאשִׁית בָּרָא";
const THAI: &str = "สวัสดีครับ ยินดีที่ได้รู้จัก ภาษาไทยมีวรรณยุกต์และสระซ้อนกัน เช่น ผู้ใหญ่ ก็ ที่นี่";
const HINDI: &str = "नमस्ते दुनिया, यह एक परीक्षण है। क्षत्रिय श्रीमान् हिन्दी में संयुक्ताक्षर होते हैं";
const BIDI: &str = "Vox 实时翻译 مرحبا بالعالم (2026) works with العربية and 中文 mixed";

/// 复杂文字：连写、方向、附标、混排、RTL 句子增长的中间帧。
fn shaping_scenes() -> Vec<Scene> {
    use Event::Update;
    use Track::{Listen, Speak};
    let growth = [
        "مرحبا بكم في",
        "مرحبا بكم في ترجمة الصوت الفورية، هذا",
        "مرحبا بكم في ترجمة الصوت الفورية، هذا اختبار للخط العربي المتصل عندما تطول الجملة وتلتف على أكثر من سطر",
    ];
    vec![
        Scene {
            name: "s1-arabic",
            what: "阿拉伯语：字母连写、从右往左、右对齐",
            events: vec![(0, Update(Listen, ARABIC, ARABIC))],
            shot_at: 600,
        },
        Scene {
            name: "s2-hebrew",
            what: "希伯来语：从右往左、右对齐、点号（niqqud）落在字母上",
            events: vec![(0, Update(Listen, HEBREW, HEBREW))],
            shot_at: 600,
        },
        Scene {
            name: "s3-thai",
            what: "泰语：元音与声调符号叠在辅音上方/下方，不散开",
            events: vec![(0, Update(Listen, THAI, THAI))],
            shot_at: 600,
        },
        Scene {
            name: "s4-hindi",
            what: "印地语：连字（क्ष、श्री）、短 i 的 matra 在辅音左边、上方的横线连成一片",
            events: vec![(0, Update(Listen, HINDI, HINDI))],
            shot_at: 600,
        },
        Scene {
            name: "s5-bidi-mixed",
            what: "中英阿混排（整句从左往右）：英文/中文保持从左往右，阿拉伯词整体反向，括号与数字位置正确",
            events: vec![(0, Update(Listen, BIDI, BIDI))],
            shot_at: 600,
        },
        Scene {
            name: "s6a-rtl-grow-1",
            what: "RTL 句子增长（1/3）：短句靠右",
            events: vec![(0, Update(Speak, growth[0], growth[0]))],
            shot_at: 300,
        },
        Scene {
            name: "s6b-rtl-grow-2",
            what: "RTL 句子增长（2/3）：已显示的字右边缘不动，新字长在左边",
            events: vec![
                (0, Update(Speak, growth[0], growth[0])),
                (200, Update(Speak, growth[1], growth[1])),
            ],
            shot_at: 500,
        },
        Scene {
            name: "s6c-rtl-grow-3",
            what: "RTL 句子增长（3/3）：折成两行，两行共用右边缘，上一行不再变",
            events: vec![
                (0, Update(Speak, growth[0], growth[0])),
                (200, Update(Speak, growth[1], growth[1])),
                (400, Update(Speak, growth[2], growth[2])),
            ],
            shot_at: 900,
        },
        Scene {
            name: "s7-two-tracks",
            what: "两条轨：听（阿拉伯语，右对齐）在上，说（印地语，左对齐）在下",
            events: vec![
                (0, Update(Listen, ARABIC, ARABIC)),
                (0, Update(Speak, HINDI, HINDI)),
            ],
            shot_at: 700,
        },
    ]
}

fn scenes() -> Vec<Scene> {
    use Event::{Finish, Update};
    use Track::{Listen, Speak};
    vec![
        Scene {
            name: "1-mixed",
            what: "中英混排：英文按词换行不拦腰截断；末尾未确认的字更暗；字都在底衬里",
            events: vec![(0, Update(Listen, LONG, "The quick brown fox jumps over the lazy dog, 然后它又"))],
            shot_at: 600,
        },
        Scene {
            name: "2a-before-rewrite",
            what: "第一句已定稿，第二句正在说",
            events: vec![
                (0, Finish(Listen, "第一句话已经说完了。")),
                (100, Update(Listen, "今天天气不错，我们出去散步吧", "今天天气不")),
            ],
            shot_at: 600,
        },
        Scene {
            name: "2b-after-rewrite",
            what: "改译中段：「今天天气」没动，第一句没动，只有分歧点之后换了",
            events: vec![
                (0, Finish(Listen, "第一句话已经说完了。")),
                (100, Update(Listen, "今天天气不错，我们出去散步吧", "今天天气不")),
                (300, Update(Listen, "今天天气很好，我们一起去公园玩吧", "今天天气很")),
            ],
            shot_at: 900,
        },
        Scene {
            name: "3a-scroll-start",
            what: "新的一行刚冒出来：整块即将上浮，此刻旧字还在原位",
            events: vec![
                (0, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and", "")),
                (400, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and then the second line starts here and goes on and on until it also fills up the whole row and more", "")),
            ],
            shot_at: 416,
        },
        Scene {
            name: "3b-scroll-mid",
            what: "上浮动画中间帧：所有行同步平移，没有横向位移，顶行半透明",
            events: vec![
                (0, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and", "")),
                (400, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and then the second line starts here and goes on and on until it also fills up the whole row and more", "")),
            ],
            shot_at: 500,
        },
        Scene {
            name: "3c-scroll-done",
            what: "动画结束：停稳在目标位置",
            events: vec![
                (0, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and", "")),
                (400, Update(Speak, "This is the first line of a long caption and it fills the row nicely, and then the second line starts here and goes on and on until it also fills up the whole row and more", "")),
            ],
            shot_at: 900,
        },
        Scene {
            name: "5a-long-before-trim",
            what: "连续说了十几句（几百字、无静默）：只看得见最后几行，字都在底衬里",
            events: nonstop_speech().0,
            shot_at: nonstop_speech().1 + 400,
        },
        Scene {
            name: "5b-long-after-trim",
            what: "模型把队首的定稿句丢掉之后：画面与 5a 逐像素相同，没有一个字挪位置",
            events: {
                let (mut events, end) = nonstop_speech();
                events.push((end + 100, Event::Trim(Track::Listen, 600)));
                events
            },
            shot_at: nonstop_speech().1 + 800,
        },
        Scene {
            name: "4a-two-tracks",
            what: "两条轨同时在跑：听在上、说在下，各占一个区域",
            events: vec![
                (0, Update(Listen, "こんにちは、今日はいい天気ですね。", "")),
                (0, Update(Speak, "你好，很高兴认识你。", "")),
            ],
            shot_at: 600,
        },
        Scene {
            name: "4b-two-tracks-grown",
            what: "听那条轨长出新行并上浮时，说那条轨的位置一点没动",
            events: vec![
                (0, Update(Listen, "こんにちは、今日はいい天気ですね。", "")),
                (0, Update(Speak, "你好，很高兴认识你。", "")),
                (300, Update(Listen, LONG, "")),
            ],
            shot_at: 900,
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

/// 写 PNG：未压缩的 deflate（stored 块）+ CRC32 + Adler32，零依赖。
fn write_png(path: &Path, w: i32, h: i32, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            raw.extend_from_slice(&rgba[idx..idx + 3]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &z);
    push_chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in &out[start..] {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}
