//! 悬浮字幕显示逻辑的性质测试（施工稿 `docs/plans/OVERLAY-CAPTIONS.md` §1 / §3）。
//!
//! 这里钉的是**用户肉眼能看见的不变量**，不是某个函数的返回值：
//!
//! - I1 位置不动：内容没被改掉的字，(行号, x) 不变；
//! - I2 已封口的行不变：下面长出新行之后，老行的字集合与每个字的 x 不变；
//! - I3 只整行上浮：一条轨内所有行永远左对齐、行距恒定（没有横向运动）；
//! - I4 轨道独立：一条轨上的事件不改变另一条轨的任何坐标；
//! - I5 不丢旧句：定稿过的句子永远留在轨上，且不打头重排。
//!
//! 用**固定种子的伪随机事件序列**跑（每种文字 2000 条 × 每条多步），每一步之后逐条检查。
//! 文字分四种（`Mode`）：拉丁/中文、纯从右往左（阿拉伯语 + 希伯来语）、双向混排、
//! 带附标的组合字形（泰语、印地语、希伯来语点号）。I1 在**逻辑顺序**上成立：
//! 位置 = (行号, 簇的左边缘 x)，从右往左的轨靠右对齐，所以新字长在左边、老字不动。
//! 没有引入 proptest：这里的随机源只需要"稳定可复现"，自己写一个 LCG 更省依赖。

use std::time::{Duration, Instant};

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{confirmed_char_count, RenderedChar, SubtitleTiming, Subtitles, Track};
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_core::text::GlyphSource;

mod common;
use common::{is_mark, FakeFont};

/// 从左往右的轨，行首字的绝对 x（窗口内边距 18 + 底衬内边距 14）。
const LEFT_EDGE: i32 = 32;

/// 视口宽度（像素）。固定值：换宽度会整块重排，不属于"内容变了不许动"的范畴。
const WIDTH: i32 = 900;
/// 每条轨放几行。
const LINES_PER_TRACK: u32 = 3;
/// 事件序列条数（施工稿 §3：至少 2000）。四种文字各 2000 条。
const SEQUENCES: u64 = 8000;
/// 每条序列的步数上限。
const STEPS: u64 = 14;
/// 时间刻度：一步 = 120 ms。
const STEP_MS: u64 = 120;

/// 每条轨放几行之外，还要把画面推到"停稳"状态：上浮动画是 200ms，
/// 这里每步喂 400ms，测的永远是最终位置。
const SETTLE_MS: u64 = 400;

/// 逐字 TTL 早就被删了（施工稿 C8 改成整块淡出）；这里把静默期拉得很长，
/// 让不变量由**内容变化**触发，而不是被时间流逝打断。
fn timing() -> SubtitleTiming {
    SubtitleTiming {
        char_ttl_ms: 600_000,
        char_fade_ms: 200,
        dim_zeros: false,
        dim_alpha: 0.3,
    }
}

/// 序列用哪种文字。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// 拉丁字母 + 中文（原有的覆盖）。
    Ltr,
    /// 阿拉伯语 + 希伯来语，整条字幕从右往左。
    Rtl,
    /// 阿拉伯语里夹英文单词与数字（双向混排）。
    Mixed,
    /// 泰语、印地语、希伯来语点号：附标并进前一个簇，簇不可拆。
    Marks,
}

impl Mode {
    fn of(seed: u64) -> Self {
        match (seed / 2) % 4 {
            0 => Mode::Ltr,
            1 => Mode::Rtl,
            2 => Mode::Mixed,
            _ => Mode::Marks,
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

const WORDS: [&str; 10] = [
    "hello",
    "world",
    "translation",
    "yes",
    "overlay",
    "a",
    "supercalifragilistic",
    "你好",
    "实时",
    "翻译",
];
const RTL_WORDS: [&str; 10] = [
    "مرحبا",
    "بالعالم",
    "ترجمة",
    "شكرا",
    "שלום",
    "עולם",
    "תרגום",
    "نعم",
    "الترجمة",
    "בְּרֵאשִׁית",
];
const MIXED_WORDS: [&str; 10] = [
    "مرحبا",
    "hello",
    "ترجمة",
    "2026",
    "بالعالم",
    "overlay",
    "שלום",
    "yes",
    "نعم",
    "supercalifragilistic",
];
const MARK_WORDS: [&str; 10] = [
    "สวัสดี",
    "ภาษาไทย",
    "ที่นี่",
    "नमस्ते",
    "हिन्दी",
    "क्षत्रिय",
    "कृपया",
    "คำแปล",
    "world",
    "भाषा",
];

fn words_of(mode: Mode) -> &'static [&'static str; 10] {
    match mode {
        Mode::Ltr => &WORDS,
        Mode::Rtl => &RTL_WORDS,
        Mode::Mixed => &MIXED_WORDS,
        Mode::Marks => &MARK_WORDS,
    }
}

/// 几个字母，不带空格：模拟一个词在逐字往后长。
fn letters(rng: &mut Rng, mode: Mode) -> String {
    let pool: &[char] = match mode {
        Mode::Ltr => &[],
        Mode::Rtl => &['ب', 'ت', 'ا', 'ل', 'ם', 'ש', 'ر', 'ن'],
        Mode::Mixed => &['ب', 'a', 'ت', 'b', 'ا', 'c', '7'],
        Mode::Marks => &['ส', 'ว', 'ั', 'ด', 'ี', 'न', 'म', 'स', '्', 'े', 'क'],
    };
    let mut out: String = (0..1 + rng.below(6))
        .map(|_| {
            if pool.is_empty() {
                char::from(b'a' + rng.below(26) as u8)
            } else {
                pool[rng.below(pool.len() as u64) as usize]
            }
        })
        .collect();
    // 一个词不会以附标开头（附标没有可依附的字母）。
    if out.chars().next().is_some_and(is_mark) {
        out.insert(0, 'ส');
    }
    out
}

const TAILS: [&str; 6] = ["", "。", "，字幕", " 的意思。", "！", "テスト"];
const RTL_TAILS: [&str; 5] = ["", "،", "؟", " (ok)", "."];

/// 一句假的话。混中英与标点，好让折行/悬挂标点那条路径也被随机走到。
fn phrase(rng: &mut Rng, mode: Mode) -> String {
    let words = words_of(mode);
    let mut out = String::new();
    let n = 1 + rng.below(5);
    for _ in 0..n {
        out.push_str(words[rng.below(words.len() as u64) as usize]);
        out.push(' ');
    }
    match mode {
        Mode::Ltr | Mode::Marks => out.push_str(TAILS[rng.below(TAILS.len() as u64) as usize]),
        Mode::Rtl | Mode::Mixed => {
            out.push_str(RTL_TAILS[rng.below(RTL_TAILS.len() as u64) as usize])
        }
    }
    out
}

/// 一个视觉行的样子：底衬 x、行里的字与每个字的 x、第一个字的绝对下标。
#[derive(Debug, Clone, PartialEq)]
struct PlacedLine {
    y: f32,
    plate_x: i32,
    /// 底衬右边缘。从右往左的轨，所有行的右边缘必须相同。
    plate_right: i32,
    rtl: bool,
    chars: Vec<char>,
    xs: Vec<i32>,
    /// 第一个字的**绝对下标**（`SubtitleLine::origin` + 帧内下标），模型丢队首也不变。
    first_abs: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Placed {
    lines: Vec<PlacedLine>,
    /// 区域顶边的屏幕 y。只在"在跑轨数"变化时才允许变（C4）。
    top: i32,
    /// `lines[0]` 在这条轨整个行序列里的行号：渲染侧丢开早已滚出的行之后，
    /// 看"相对行号"必须加上它，否则截断前后对不上。
    first_line: usize,
}

/// 一个字的落点：(绝对行号, x, 字)。
type Slot = (usize, i32, char);

impl Placed {
    fn total_lines(&self) -> usize {
        self.first_line + self.lines.len()
    }

    /// 绝对字下标 → 落点。
    fn slots(&self) -> std::collections::BTreeMap<u64, Slot> {
        let mut out = std::collections::BTreeMap::new();
        for (li, line) in self.lines.iter().enumerate() {
            for (k, (&x, &ch)) in line.xs.iter().zip(&line.chars).enumerate() {
                out.insert(line.first_abs + k as u64, (self.first_line + li, x, ch));
            }
        }
        out
    }

    fn line_at(&self, abs_line: usize) -> Option<&PlacedLine> {
        abs_line
            .checked_sub(self.first_line)
            .and_then(|i| self.lines.get(i))
    }

    fn first_char(&self) -> u64 {
        self.lines.first().map_or(0, |l| l.first_abs)
    }

    /// 末尾那个还在增长的词（连续字母数字）的起点；末尾不是词就没有。
    fn growing_word_start(&self, mode: Mode) -> Option<u64> {
        let slots = self.slots();
        if mode != Mode::Ltr {
            // UAX #14 的"词"可以带着空格（空格后面跟 `.` `،` 这类不许在前面断的标点），
            // 所以整词挪行时挪的是"最后一个断点之后"的全部；双向混排还要再加上末尾那段反向文字。
            let unit = last_break_start(&slots);
            return Some(if mode == Mode::Mixed {
                unit.min(trailing_run_start(&slots))
            } else {
                unit
            });
        }
        let mut start = None;
        for (&abs, &(_, _, ch)) in slots.iter().rev() {
            let in_word = match mode {
                Mode::Ltr => ch.is_ascii_alphanumeric(),
                _ => !ch.is_whitespace(),
            };
            if in_word {
                start = Some(abs);
            } else {
                break;
            }
        }
        start
    }
}

/// 字的方向类别：L = 拉丁字母，R = 阿拉伯/希伯来字母，其余是中性（空格、标点）。
fn strong_class(ch: char) -> Option<bool> {
    // 数字没有自己的方向：跟着前面的强方向字符走（阿拉伯字母后面的数字归 RTL 块）。
    if ch.is_ascii_alphabetic() {
        Some(false)
    } else if matches!(ch as u32, 0x0590..=0x06FF) && ch != '،' && !ch.is_whitespace() {
        Some(true)
    } else {
        None
    }
}

/// 末尾那一段同方向的文字（连同夹在里面的空格标点）的起点：双向文字里，这一段每长一个字，
/// 它自己的老字都会被推开（LTR 轨里的 RTL 段靠左贴着长，RTL 轨里的 LTR 段靠右贴着长）。
fn trailing_run_start(slots: &std::collections::BTreeMap<u64, Slot>) -> u64 {
    let mut run_class = None;
    let mut start = slots.keys().next_back().map_or(0, |k| k + 1);
    for (&abs, &(_, _, ch)) in slots.iter().rev() {
        match (strong_class(ch), run_class) {
            (Some(c), None) => run_class = Some(c),
            (Some(c), Some(rc)) if c != rc => break,
            _ => {}
        }
        start = abs;
    }
    start
}

/// 最后一个 UAX #14 断点之后的那一段（正在增长、折行时不可拆的"词"）的起点。
fn last_break_start(slots: &std::collections::BTreeMap<u64, Slot>) -> u64 {
    let abs: Vec<u64> = slots.keys().copied().collect();
    let text: String = slots.values().map(|s| s.2).collect();
    let mut start_byte = 0usize;
    for (pos, _) in unicode_linebreak::linebreaks(&text) {
        if pos < text.len() {
            start_byte = pos;
        }
    }
    let index = text[..start_byte].chars().count();
    abs.get(index)
        .copied()
        .unwrap_or_else(|| abs.last().map_or(0, |k| k + 1))
}

/// 一帧的完整样子。
#[derive(Clone, PartialEq, Debug)]
struct View {
    listen: Placed,
    speak: Placed,
    height: i32,
}

struct World {
    subs: Subtitles,
    renderer: Renderer,
    settings: SubtitleSettings,
    epoch: Instant,
    now: u64,
    /// 每条轨"到目前为止的整句"（服务端视角）。
    sentence: [String; 2],
    /// 每条轨已经定稿的句子（按顺序，I5 的基线）。
    committed: [Vec<String>; 2],
    /// 这条序列要不要模拟"服务端只确认一部分"（C3）。
    partial_confirm: bool,
    mode: Mode,
}

impl World {
    fn new(seed: u64) -> Self {
        let settings = SubtitleSettings {
            lines_per_track: LINES_PER_TRACK,
            ..SubtitleSettings::default()
        };
        Self {
            subs: Subtitles::new(timing()),
            renderer: Renderer::new(
                &settings,
                96,
                vox_overlay_core::text::font_factory(|_, _, _| {
                    Ok(Box::new(FakeFont) as Box<dyn GlyphSource>)
                }),
            )
            .expect("假光栅器不会失败"),
            settings,
            epoch: Instant::now(),
            now: 0,
            sentence: [String::new(), String::new()],
            committed: [Vec::new(), Vec::new()],
            partial_confirm: seed.is_multiple_of(2),
            mode: Mode::of(seed),
        }
    }

    fn index(track: Track) -> usize {
        match track {
            Track::Listen => 0,
            Track::Speak => 1,
        }
    }

    /// 服务端把"到目前为止的整句"推过来。
    ///
    /// 字幕轨按**字**求新旧整句的公共前缀，前缀里的字保留身份，改译只影响分歧点
    /// 之后的部分——这条不变量就是从这条路径上长出来的。
    fn feed(&mut self, track: Track, full: &str) {
        let i = World::index(track);
        let confirmed = if self.partial_confirm {
            Some(self.sentence[i].clone())
        } else {
            None
        };
        let confirmed_len = confirmed_char_count(full, confirmed.as_deref());
        self.subs
            .track_mut(track)
            .update_sentence(full, confirmed_len, self.now);
        self.sentence[i] = full.to_string();
    }

    /// 这一句说完了。
    fn commit(&mut self, track: Track, text: &str) {
        self.subs.track_mut(track).finish_sentence(text, self.now);
        let i = World::index(track);
        self.committed[i].push(text.to_string());
        self.sentence[i].clear();
    }

    fn tick(&mut self, ms: u64) {
        self.now += ms;
        self.subs.prune(self.now);
    }

    fn frame(&self) -> SubtitleFrame {
        let mut lines = Vec::new();
        for track in [Track::Listen, Track::Speak] {
            let chars: Vec<RenderedChar> = self
                .subs
                .track(track)
                .render(self.now)
                .into_iter()
                .collect();
            if !chars.is_empty() {
                lines.push(SubtitleLine {
                    track,
                    origin: self.subs.track(track).origin(),
                    chars,
                    color: "#ffffff".into(),
                });
            }
        }
        SubtitleFrame { lines }
    }

    /// 推进到"停稳"再取布局：上浮动画是 200ms，这里每步喂 400ms。
    fn view(&mut self) -> View {
        for _ in 0..3 {
            let frame = self.frame();
            self.renderer.plan(&FrameInput {
                frame: &frame,
                settings: &self.settings,
                client_width: WIDTH,
                client_height: None,
                dpi: 96,
                now: self.epoch + Duration::from_millis(self.now + SETTLE_MS),
            });
        }
        let frame = self.frame();
        let layout = self.renderer.plan(&FrameInput {
            frame: &frame,
            settings: &self.settings,
            client_width: WIDTH,
            client_height: None,
            dpi: 96,
            now: self.epoch + Duration::from_millis(self.now + SETTLE_MS * 4),
        });
        View {
            listen: placed_of(
                &mut self.renderer,
                layout.listen.as_ref(),
                &frame,
                Track::Listen,
            ),
            speak: placed_of(
                &mut self.renderer,
                layout.speak.as_ref(),
                &frame,
                Track::Speak,
            ),
            height: layout.client_height,
        }
    }
}

fn placed_of(
    renderer: &mut Renderer,
    region: Option<&vox_overlay_core::layout::TrackLayout>,
    frame: &SubtitleFrame,
    track: Track,
) -> Placed {
    let Some(region) = region else {
        return Placed::default();
    };
    let origin = frame
        .lines
        .iter()
        .find(|l| l.track == track)
        .map_or(0, |l| l.origin);
    let chars: Vec<char> = frame
        .lines
        .iter()
        .find(|l| l.track == track)
        .map(|l| l.chars.iter().map(|c| c.ch).collect())
        .unwrap_or_default();
    let mut placed = Placed {
        top: region.region.0,
        first_line: region.first_line,
        ..Placed::default()
    };
    for line in &region.lines {
        // 渲染器给的是显示顺序的簇；不变量按**逻辑顺序**认字，所以按字符下标排回来。
        let mut cells = renderer.cells(track, line);
        cells.sort_by_key(|c| c.first);
        let mut line_xs = Vec::new();
        let mut line_chars = Vec::new();
        for cell in &cells {
            for ch in &chars[cell.first..cell.last] {
                // 相对左内边距：从左往右的行首字恒为 0；从右往左的行是靠右对齐的绝对坐标。
                line_xs.push(cell.x - LEFT_EDGE);
                line_chars.push(*ch);
            }
        }
        placed.lines.push(PlacedLine {
            y: line.y - region.region.0 as f32,
            plate_x: line.plate_x,
            plate_right: line.plate_x + line.plate_w,
            rtl: line.rtl,
            chars: line_chars,
            xs: line_xs,
            first_abs: origin + line.first as u64,
        });
    }
    placed
}

/// 一条事件。喂给**模型**，与它最终怎么落字无关。
enum Step {
    /// 服务端把整句往前推：新增若干字。
    Extend(String),
    /// 一个词逐字往后长（没有尾空格）：词长到超宽时整体挪到下一行一次。
    Grow(String),
    /// 服务端改译/纠错：新整句不以旧整句为前缀。
    Rewrite(String),
    /// 这一句说完了，定稿。
    Finish(String),
    /// 时间推进。
    Wait(u64),
}

fn make_step(rng: &mut Rng, mode: Mode) -> Step {
    match rng.below(13) {
        0..=3 => Step::Extend(phrase(rng, mode)),
        4..=6 => Step::Grow(letters(rng, mode)),
        7..=9 => Step::Rewrite(phrase(rng, mode)),
        10 => Step::Finish(phrase(rng, mode)),
        _ => Step::Wait(STEP_MS * (1 + rng.below(3))),
    }
}

// ---------------------------------------------------------------------------
// 不变量检查
// ---------------------------------------------------------------------------

/// 分歧点：两帧都有的字里第一个内容不同的绝对下标；新帧比旧帧短（尾巴被改没了）
/// 时，分歧点就是新帧的末尾。
fn divergence(
    pm: &std::collections::BTreeMap<u64, Slot>,
    cm: &std::collections::BTreeMap<u64, Slot>,
) -> Option<u64> {
    let differing = pm
        .iter()
        .find(|(abs, (_, _, ch))| cm.get(abs).is_some_and(|c| c.2 != *ch))
        .map(|(abs, _)| *abs);
    let cur_end = cm.keys().next_back().map(|k| k + 1);
    let shortened = cur_end.filter(|end| pm.contains_key(end));
    match (differing, shortened) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn check_i1(where_: &str, mode: Mode, prev: &Placed, cur: &Placed) -> Result<(), String> {
    let (pm, cm) = (prev.slots(), cur.slots());
    // 分歧点：两帧都有的字里第一个内容不同的绝对下标。
    let d = divergence(&pm, &cm);
    // 双向混排：改动落在一段反向文字里时，这一段里更早的字也会被推开（整段是一个视觉块），
    // 所以分歧点往前退到它所在那一段的起点。
    let d = match (mode, d) {
        (Mode::Mixed, Some(d)) => {
            let before: std::collections::BTreeMap<u64, Slot> =
                pm.range(..d).map(|(k, v)| (*k, *v)).collect();
            Some(trailing_run_start(&before).min(d))
        }
        (_, d) => d,
    };
    // 例外条款（施工稿 §1 I1）：仅当前行尾、仍在增长的那个词，在增长导致超宽时
    // 整体移到下一行一次；其余字不动。
    // 折行时不可拆的"词"整体跳到下一行，才算例外发生了。
    let unit = if mode == Mode::Ltr {
        prev.growing_word_start(mode)
    } else {
        Some(last_break_start(&pm))
    };
    let hop = unit.filter(|w| hopped(prev, cur, *w));
    // 双向混排里，行尾那段反向文字一边长一边把自己的老字推开——那是双向文字的本质，
    // 所以整段豁免。其余文字里，词没换行就一个字都不许动（只有"整词跳到下一行"才豁免）。
    let word = if mode == Mode::Mixed {
        prev.growing_word_start(mode)
    } else {
        hop
    };
    // 双向混排里整词跳行，还会把它原来那一行里的反向块拆开重排（块的逻辑尾巴走了，
    // 块里更早的字跟着挪），所以那一行整行豁免。其余文字没有这个豁免。
    let hop_line = if mode == Mode::Mixed {
        hop.and_then(|w| pm.get(&w).map(|s| s.0))
    } else {
        None
    };
    let visible_from = prev.total_lines().saturating_sub(LINES_PER_TRACK as usize);
    for (&abs, &(line, x, ch)) in &pm {
        if d.is_some_and(|d| abs >= d) || word.is_some_and(|w| abs >= w) {
            break;
        }
        match cm.get(&abs) {
            Some(&(l2, x2, _)) if (line, x) != (l2, x2) && hop_line != Some(line) => {
                return Err(format!(
                    "{where_}: I1 位置不动——绝对第 {abs} 个字 {ch:?} 的 (行号,x) 从 {:?} 变成了 {:?}\n前一帧 {:?}\n这一帧 {:?}",
                    (line, x),
                    (l2, x2),
                    prev.lines.iter().map(|l| l.chars.iter().collect::<String>()).collect::<Vec<_>>(),
                    cur.lines.iter().map(|l| l.chars.iter().collect::<String>()).collect::<Vec<_>>()
                ));
            }
            Some(_) => {}
            None if abs < cur.first_char() && line >= visible_from => {
                return Err(format!(
                    "{where_}: I1 位置不动——可见字 {ch:?}（绝对第 {abs} 个，第 {line} 行）被截掉了"
                ));
            }
            None => {}
        }
    }
    Ok(())
}

/// 词 `start` 开头的那个字换了行。
fn hopped(prev: &Placed, cur: &Placed, start: u64) -> bool {
    let (pm, cm) = (prev.slots(), cur.slots());
    match (pm.get(&start), cm.get(&start)) {
        (Some(a), Some(b)) => a.0 != b.0,
        _ => true,
    }
}

fn check_i2(where_: &str, prev: &Placed, cur: &Placed) -> Result<(), String> {
    if prev.lines.len() < 2 {
        return Ok(());
    }
    let (pm, cm) = (prev.slots(), cur.slots());
    let d = divergence(&pm, &cm);
    // 分歧点落在第几行；从那一行起允许重排，更上面的行必须冻结。
    let divergent_line = match d {
        Some(d) => pm.get(&d).map_or(prev.total_lines(), |s| s.0),
        None => prev.total_lines(),
    };
    // 最后一行底下还没有新行，不算封口：它还在往右长。
    let sealed = prev.total_lines() - 1;
    for (li, old) in prev.lines.iter().enumerate() {
        let abs_line = prev.first_line + li;
        // 折行要往后看一个不可断的词：分歧点落在下一行开头的词里，上一行的收尾
        // 也可能跟着变（新词更短就能挤上来）。所以分歧行的前一行也不算封口。
        if abs_line + 1 >= divergent_line || abs_line >= sealed {
            break;
        }
        // 被渲染侧丢开（早已滚出区域）的行不在了，不算变。
        let Some(new) = cur.line_at(abs_line) else {
            if abs_line < cur.first_line {
                continue;
            }
            return Err(format!(
                "{where_}: I2 已封口的行不该消失——第 {abs_line} 行 {old:?} 没了"
            ));
        };
        if old.chars != new.chars || old.xs != new.xs {
            return Err(format!(
                "{where_}: I2 已封口的行不该变——第 {abs_line} 行从 {old:?} 变成 {new:?}"
            ));
        }
    }
    Ok(())
}

fn check_i3(where_: &str, cur: &Placed) -> Result<(), String> {
    // 一条轨里的行必须同侧对齐（从左往右的靠左，从右往左的靠右），且行距恒定（只允许整块平移）。
    let Some(first) = cur.lines.first() else {
        return Ok(());
    };
    for (li, line) in cur.lines.iter().enumerate() {
        let (edge, first_edge) = if first.rtl {
            (line.plate_right, first.plate_right)
        } else {
            (line.plate_x, first.plate_x)
        };
        if line.rtl != first.rtl || edge != first_edge {
            return Err(format!(
                "{where_}: I3 不许横向运动——第 {li} 行底衬靠边 x={edge} 与首行 x={first_edge}"
            ));
        }
    }
    if cur.lines.len() >= 2 {
        let step = cur.lines[1].y - cur.lines[0].y;
        for pair in cur.lines.windows(2) {
            let gap = pair[1].y - pair[0].y;
            if (gap - step).abs() > 0.5 {
                return Err(format!(
                    "{where_}: I3 行距必须恒定——观察到 {gap}，基准 {step}"
                ));
            }
        }
    }
    Ok(())
}

fn check_i4(
    where_: &str,
    track: Track,
    prev: &Placed,
    cur: &Placed,
    count_changed: bool,
) -> Result<(), String> {
    let moved = |what: String| {
        Err(format!(
            "{where_}: I4 轨道独立——只动了 {track:?}，另一条轨的{what}：{prev:?} 变成 {cur:?}"
        ))
    };
    if prev.lines.is_empty() != cur.lines.is_empty() {
        return moved("内容凭空出现/消失".into());
    }
    if prev.total_lines() != cur.total_lines() {
        return moved("行数变了".into());
    }
    // 区域顶边只允许在 `me` 这条轨出现/消失（在跑轨数变）时变。
    if !count_changed && prev.top != cur.top {
        return moved("区域顶边变了".into());
    }
    // 渲染侧可能各自把早已滚出的行丢开，所以只比两边都还在的行（按绝对行号）。
    for (li, old) in prev.lines.iter().enumerate() {
        if let Some(new) = cur.line_at(prev.first_line + li) {
            if old != new {
                return moved(format!("第 {} 行变了", prev.first_line + li));
            }
        }
    }
    Ok(())
}

fn check_i5(where_: &str, world: &World, track: Track) -> Result<(), String> {
    let flat: Vec<char> = world.subs.track(track).text(world.now).chars().collect();
    let flat_str: String = flat.iter().collect();
    let committed = &world.committed[World::index(track)];
    let mut at = 0usize;
    for sentence in committed {
        let want: Vec<char> = sentence.chars().collect();
        match flat[at..]
            .windows(want.len().max(1))
            .position(|w| w == &want[..])
        {
            Some(offset) => at += offset + want.len(),
            None => {
                return Err(format!(
                "{where_}: I5 不丢旧句——定稿句 {sentence:?} 在轨上找不到了，当前内容 {flat_str:?}"
            ))
            }
        }
    }
    Ok(())
}

/// 簇不可拆：附标永远不会落在一行的开头（它得跟着前面的字母走）。
fn check_marks(where_: &str, cur: &Placed) -> Result<(), String> {
    for (li, line) in cur.lines.iter().enumerate() {
        if let Some(&ch) = line.chars.first() {
            if is_mark(ch) {
                return Err(format!(
                    "{where_}: 簇被拆开——第 {li} 行以附标 {ch:?} 开头：{line:?}"
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------

fn run_sequence(seed: u64) -> Result<(), String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut world = World::new(seed);
    let mut prev = [Placed::default(), Placed::default()];

    for step_index in 0..STEPS {
        let track = if rng.below(2) == 0 {
            Track::Listen
        } else {
            Track::Speak
        };
        let step = make_step(&mut rng, world.mode);
        let where_ = format!("seq#{seed} step#{step_index} {track:?}");
        match step {
            Step::Extend(text) => {
                let mut full = world.sentence[World::index(track)].clone();
                full.push_str(&text);
                world.feed(track, &full);
            }
            Step::Grow(text) => {
                let mut full = world.sentence[World::index(track)].clone();
                full.push_str(&text);
                world.feed(track, &full);
            }
            Step::Rewrite(text) => world.feed(track, &text),
            Step::Finish(text) => world.commit(track, &text),
            Step::Wait(ms) => world.tick(ms),
        }

        let cur_view = world.view();
        let cur = [cur_view.listen.clone(), cur_view.speak.clone()];
        let me = World::index(track);
        let other = 1 - me;

        check_i1(&where_, world.mode, &prev[me], &cur[me])?;
        check_marks(&where_, &cur[me])?;
        check_i2(&where_, &prev[me], &cur[me])?;
        check_i3(&where_, &cur[me])?;
        // I4：另一条轨一个坐标都不能动。
        // 区域顶边只允许在 `me` 这条轨出现/消失（在跑轨数变）时变；其余情况一个坐标都不许动。
        let count_changed = prev[me].lines.is_empty() != cur[me].lines.is_empty();
        check_i4(&where_, track, &prev[other], &cur[other], count_changed)?;
        check_i5(&where_, &world, track)?;
        check_i5(
            &where_,
            &world,
            if me == 0 { Track::Speak } else { Track::Listen },
        )?;

        prev = cur;
    }
    Ok(())
}

#[test]
fn random_sequences_keep_every_display_invariant() {
    // 每条不变量各留第一条反例：红的时候能一次看全是哪几条被破坏，而不是只看第一条。
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut broken = 0u64;
    for seed in 0..SEQUENCES {
        if let Err(e) = run_sequence(seed) {
            broken += 1;
            let tag: &'static str = if e.contains("I1 位置不动") {
                "I1"
            } else if e.contains("I2") {
                "I2"
            } else if e.contains("I3") {
                "I3"
            } else if e.contains("I4") {
                "I4"
            } else if e.contains("I5") {
                "I5"
            } else {
                "?"
            };
            if !seen.iter().any(|(t, _)| *t == tag) {
                seen.push((tag, e));
            }
        }
    }
    assert!(
        seen.is_empty(),
        "{} / {} 条随机序列违反了不变量（{} 类）：\n{}",
        broken,
        SEQUENCES,
        seen.len(),
        seen.iter()
            .map(|(tag, msg)| format!("[{tag}] {msg}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// 定向：改译当前句**不能**让上一句消失（I5 的最小复现）。
#[test]
fn rewriting_the_current_sentence_keeps_previous_sentences() {
    let mut world = World::new(0);
    world.feed(Track::Speak, "第一句已经说完了。");
    world.commit(Track::Speak, "第一句已经说完了。");
    world.feed(Track::Speak, "第二句改");
    world.feed(Track::Speak, "第二句被改译了。");
    let text = world.subs.track(Track::Speak).text(world.now);
    assert!(
        text.contains("第一句已经说完了。"),
        "改译当前句时上一句必须留在轨上，实际 {text:?}"
    );
}

/// 定向：改译中段只重排分歧点之后的部分（I1 / I2 的最小复现）。
#[test]
fn rewriting_the_middle_only_reshuffles_after_the_divergence() {
    let mut world = World::new(0);
    let before_text = "翻译字幕应该保持位置不动";
    world.feed(Track::Listen, before_text);
    let before = world.view().listen;
    // 分歧点在第 6 个字。
    world.feed(Track::Listen, "翻译字幕永远保持位置不动啊");
    let after = world.view().listen;
    let (b, a) = (before.slots(), after.slots());
    let d = b
        .iter()
        .find(|(abs, slot)| a.get(abs).is_some_and(|x| x.2 != slot.2))
        .map(|(abs, _)| *abs)
        .expect("改译后必有分歧点");
    assert!(d >= 4, "分歧点在第 6 个字附近，实际 {d}");
    for (abs, slot) in &b {
        if *abs >= d {
            break;
        }
        assert_eq!(
            Some(slot),
            a.get(abs),
            "分歧点之前（绝对第 {abs} 个字）不许挪位置、不许换字"
        );
    }
}

/// 定向：超宽的一行不许从左侧滚掉老字（R4 / I3）。
#[test]
fn an_overlong_line_keeps_every_char_from_the_first_one() {
    let mut world = World::new(0);
    let long: String = std::iter::repeat_n("字", 200).collect();
    world.feed(Track::Listen, &long);
    let view = world.view();
    assert_eq!(
        view.listen.slots().len(),
        200,
        "整行 200 个字都该被排进某个视觉行，不许被丢掉（旧行为是从左侧丢老字）"
    );
    assert!(
        view.listen.lines.len() > LINES_PER_TRACK as usize,
        "200 个字至少要折出好几行"
    );
}

/// 定向：两条轨各占一个区域，谁也不推谁（R3 / I4）。
#[test]
fn tracks_get_independent_regions() {
    let mut world = World::new(0);
    world.feed(Track::Listen, "听");
    let before = world.view();
    world.feed(Track::Speak, "说");
    let after = world.view();
    assert_eq!(
        before.listen, after.listen,
        "对外那条轨多一个字，不该挪动听人那条轨的任何坐标"
    );
    assert!(
        after.height > before.height,
        "多一条在跑的轨，窗口就多一个区域的高度（C4：只随在跑轨数变）"
    );
}

/// 定向：行数固定，窗口高度不随内容变（C4）。
#[test]
fn window_height_follows_regions_not_content() {
    let mut world = World::new(0);
    world.feed(Track::Listen, "短");
    let short = world.view().height;
    world.feed(Track::Listen, &"很长的一句话".repeat(20));
    let long = world.view().height;
    assert_eq!(
        short, long,
        "同一条轨的窗口高度不能因为内容变长而变（高度 = 区域数 × 行数 × 行距）"
    );
}

/// 定向：静默够久之后整块淡出并清空（C8）。
#[test]
fn silence_fades_the_whole_region_out_and_clears_it() {
    let mut world = World::new(0);
    world.feed(Track::Listen, "这句话会淡掉");
    assert!(world.view().height > 1);
    // 静默 600s + 淡出 200ms 之后。
    world.tick(600_000 + 300);
    assert!(
        world.subs.track(Track::Listen).is_empty(),
        "淡完就该把这条轨清空"
    );
    assert_eq!(world.view().height, 1, "清空之后窗口缩成 1px 全透明");
}

/// 定向：未确认的尾巴画得暗一些（C3）。
#[test]
fn unconfirmed_tail_is_dimmed_and_confirmed_tail_is_not() {
    let mut world = World::new(0);
    world.partial_confirm = true;
    world.feed(Track::Listen, "确认过了");
    world.sentence[World::index(Track::Listen)] = "确认过了".to_string();
    world.feed(Track::Listen, "确认过了继续说");
    let chars = world.subs.track(Track::Listen).render(world.now);
    let confirmed = chars[..4].iter().map(|c| c.alpha).collect::<Vec<_>>();
    let unconfirmed = chars[4..].iter().map(|c| c.alpha).collect::<Vec<_>>();
    assert!(
        confirmed.iter().all(|a| (*a - 1.0).abs() < 1e-6),
        "已确认的字全不透明：{confirmed:?}"
    );
    assert!(
        unconfirmed.iter().all(|a| (*a - 0.7).abs() < 1e-6),
        "未确认的尾巴画 0.7 倍：{unconfirmed:?}"
    );
}

/// 定向：一个词逐字往后长、没有尾空格（I1 的例外条款）。
///
/// 词长到让行超宽时，它整体挪到下一行**一次**；除它之外的字一个都不动。
#[test]
fn a_growing_word_hops_to_the_next_line_once_and_nothing_else_moves() {
    let mut world = World::new(0);
    let mut sentence = "ab cd ef ".repeat(17);
    world.feed(Track::Listen, &sentence);
    let mut prev = world.view().listen;
    let word_start = prev.slots().len() as u64;
    let mut hops = 0;
    let mut last_home: Option<(usize, i32)> = None;
    let mut first_home: Option<(usize, i32)> = None;
    for i in 0..14 {
        sentence.push(char::from(b'a' + i));
        world.feed(Track::Listen, &sentence);
        let cur = world.view().listen;
        check_i1("growing word", Mode::Ltr, &prev, &cur).expect("词之外的字不许动");
        check_i2("growing word", &prev, &cur).expect("封口的行不许变");
        let home = cur.slots().get(&word_start).map(|s| (s.0, s.1));
        if last_home.is_some() && home != last_home {
            hops += 1;
        }
        first_home = first_home.or(home);
        last_home = home;
        prev = cur;
    }
    assert_eq!(
        hops, 1,
        "整词只许挪一次，实际 {hops} 次（落点 {last_home:?}）"
    );
    assert_eq!(
        last_home.map(|h| (h.0, h.1)),
        first_home.map(|h| (h.0 + 1, 0)),
        "挪到了下一行行首"
    );
}

/// 定向：长时间连续说话（无静默、几千字、中英混排）。模型按预算丢队首、渲染侧把锚点
/// 推到行首，全程可见字的 (绝对行号, x) 一个都不许变。
#[test]
fn five_thousand_chars_of_nonstop_speech_never_reflow_visible_chars() {
    const PIECES: [&str; 8] = [
        "the quick brown fox ",
        "然后它又跳过了一只狗，",
        "and then something happened ",
        "字幕必须折行而且不能乱跳。",
        "hello world ",
        "实时翻译 ",
        "supercalifragilistic ",
        "好的，",
    ];
    // 模拟 Runtime 的预算：已定稿的字留这么多，更早的定稿句整句丢掉。
    const KEEP_SEALED: usize = 1000;
    let mut world = World::new(0);
    let mut rng = Rng(0xC0FFEE);
    let mut prev = Placed::default();
    let mut fed = 0usize;
    let mut checked = 0usize;
    while fed < 5200 {
        let mut sentence = String::new();
        for _ in 0..(3 + rng.below(4)) {
            sentence.push_str(PIECES[rng.below(PIECES.len() as u64) as usize]);
            world.now += 40;
            world.feed(Track::Listen, &sentence);
            let cur = world.view().listen;
            let where_ = format!("fed={fed} chars={}", sentence.chars().count());
            check_i1(&where_, Mode::Ltr, &prev, &cur).unwrap_or_else(|e| panic!("{e}"));
            check_i2(&where_, &prev, &cur).unwrap_or_else(|e| panic!("{e}"));
            check_i3(&where_, &cur).unwrap_or_else(|e| panic!("{e}"));
            prev = cur;
            checked += 1;
        }
        fed += sentence.chars().count();
        world.commit(Track::Listen, &sentence);
        world.subs.track_mut(Track::Listen).trim_sealed(KEEP_SEALED);
    }
    assert!(checked > 300, "该检查了几百帧，实际 {checked}");
    let track = world.subs.track(Track::Listen);
    assert!(
        track.origin() > 3000,
        "模型必须真的丢过队首，实际 origin={}",
        track.origin()
    );
    assert!(
        prev.first_line > 20,
        "渲染侧必须真的把锚点往前推过，实际丢开了 {} 行",
        prev.first_line
    );
    assert!(
        prev.lines.len() <= 2 * LINES_PER_TRACK as usize + 3,
        "锚点之后只该剩区域 + 余量那么几行，实际 {}",
        prev.lines.len()
    );
}
