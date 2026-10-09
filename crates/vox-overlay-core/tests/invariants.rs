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
//! 用**固定种子的伪随机事件序列**跑（2000 条 × 每条多步），每一步之后逐条检查。
//! 没有引入 proptest：这里的随机源只需要"稳定可复现"，自己写一个 LCG 更省依赖。

use std::time::{Duration, Instant};

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{confirmed_char_count, RenderedChar, SubtitleTiming, Subtitles, Track};
use vox_overlay_core::render::{FrameInput, Renderer};
use vox_overlay_core::text::{FontMetrics, GlyphSource};

/// 视口宽度（像素）。固定值：换宽度会整块重排，不属于"内容变了不许动"的范畴。
const WIDTH: i32 = 900;
/// 每条轨放几行。
const LINES_PER_TRACK: u32 = 3;
/// 事件序列条数（施工稿 §3：至少 2000）。
const SEQUENCES: u64 = 2000;
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

/// 等宽假光栅器：CJK 26 px，其余 14 px；行高 40，基线 32。
/// 不变量只关心"坐标有没有动"，跟真实字形无关，所以量得准不准无所谓。
struct FakeFont;

impl GlyphSource for FakeFont {
    fn metrics(&self) -> FontMetrics {
        FontMetrics {
            line_height: 40,
            ascent: 32,
        }
    }

    fn advance(&mut self, ch: char) -> i32 {
        if is_wide(ch) {
            26
        } else {
            14
        }
    }

    fn glyph(&mut self, _ch: char) -> Option<&vox_overlay_core::text::Glyph> {
        // 只测布局，不需要真的墨迹。
        None
    }
}

fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x303F | 0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xFF00..=0xFF60)
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
/// 几个字母，不带空格：模拟一个词在逐字往后长。
fn letters(rng: &mut Rng) -> String {
    (0..1 + rng.below(6))
        .map(|_| char::from(b'a' + rng.below(26) as u8))
        .collect()
}

const TAILS: [&str; 6] = ["", "。", "，字幕", " 的意思。", "！", "テスト"];

/// 一句假的话。混中英与标点，好让折行/悬挂标点那条路径也被随机走到。
fn phrase(rng: &mut Rng) -> String {
    let mut out = String::new();
    let n = 1 + rng.below(5);
    for _ in 0..n {
        out.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        out.push(' ');
    }
    out.push_str(TAILS[rng.below(TAILS.len() as u64) as usize]);
    out
}

/// 一个视觉行的样子：底衬 x、行里的字与每个字的 x、第一个字的绝对下标。
#[derive(Debug, Clone, PartialEq)]
struct PlacedLine {
    y: f32,
    plate_x: i32,
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
    fn growing_word_start(&self) -> Option<u64> {
        let slots = self.slots();
        let mut start = None;
        for (&abs, &(_, _, ch)) in slots.iter().rev() {
            if ch.is_ascii_alphanumeric() {
                start = Some(abs);
            } else {
                break;
            }
        }
        start
    }
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
            listen: placed_of(layout.listen.as_ref(), &frame, Track::Listen),
            speak: placed_of(layout.speak.as_ref(), &frame, Track::Speak),
            height: layout.client_height,
        }
    }
}

fn placed_of(
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
        let mut x = 0;
        let mut line_xs = Vec::new();
        let mut line_chars = Vec::new();
        for ch in chars.iter().take(line.last).skip(line.first) {
            line_xs.push(x);
            x += if is_wide(*ch) { 26 } else { 14 };
            line_chars.push(*ch);
        }
        placed.lines.push(PlacedLine {
            y: line.y - region.region.0 as f32,
            plate_x: line.plate_x,
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

fn make_step(rng: &mut Rng) -> Step {
    match rng.below(13) {
        0..=3 => Step::Extend(phrase(rng)),
        4..=6 => Step::Grow(letters(rng)),
        7..=9 => Step::Rewrite(phrase(rng)),
        10 => Step::Finish(phrase(rng)),
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

fn check_i1(where_: &str, prev: &Placed, cur: &Placed) -> Result<(), String> {
    let (pm, cm) = (prev.slots(), cur.slots());
    // 分歧点：两帧都有的字里第一个内容不同的绝对下标。
    let d = divergence(&pm, &cm);
    // 例外条款（施工稿 §1 I1）：仅当前行尾、仍在增长的那个词，在增长导致超宽时
    // 整体移到下一行一次；其余字不动。
    let word = prev.growing_word_start();
    let visible_from = prev.total_lines().saturating_sub(LINES_PER_TRACK as usize);
    for (&abs, &(line, x, ch)) in &pm {
        if d.is_some_and(|d| abs >= d) || word.is_some_and(|w| abs >= w) {
            break;
        }
        match cm.get(&abs) {
            Some(&(l2, x2, _)) if (line, x) != (l2, x2) => {
                return Err(format!(
                    "{where_}: I1 位置不动——绝对第 {abs} 个字 {ch:?} 的 (行号,x) 从 {:?} 变成了 {:?}",
                    (line, x),
                    (l2, x2)
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
    // 一条轨里的行必须左对齐到同一个 x，且行距恒定（只允许整块平移）。
    let Some(first) = cur.lines.first() else {
        return Ok(());
    };
    for (li, line) in cur.lines.iter().enumerate() {
        if line.plate_x != first.plate_x {
            return Err(format!(
                "{where_}: I3 不许横向运动——第 {li} 行底衬 x={} 与首行 x={}",
                line.plate_x, first.plate_x
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
        let step = make_step(&mut rng);
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

        check_i1(&where_, &prev[me], &cur[me])?;
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
        check_i1("growing word", &prev, &cur).expect("词之外的字不许动");
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
            check_i1(&where_, &prev, &cur).unwrap_or_else(|e| panic!("{e}"));
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
