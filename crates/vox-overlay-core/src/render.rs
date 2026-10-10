//! 画一整帧纯字幕：逐行底衬 + 逐字 alpha 的文字。
//!
//! 跟窗口无关——输入是内容和几何，输出是一块预乘 BGRA 的 `Canvas`，所以快照
//! 工具可以不开窗口就复用完全相同的像素路径。
//!
//! 一帧分两步：[`Renderer::plan`] 算布局（含上浮动画的位置），[`Renderer::draw`]
//! 再照着布局上色。拆开是为了让"字摆在哪"这件事能脱离像素单测——不变量测试
//! （`tests/invariants.rs`）盯的就是 `plan` 的输出。

use std::time::Instant;

use unicode_bidi::{BidiInfo, Direction, Level};
use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::Track;

use crate::canvas::{Canvas, Mask};
use crate::color::{alpha_to_u8, parse_hex_rgb_or, Rgb};
use crate::layout::{
    fill_breaks, layout_regions, visual_order, FrameLayout, Metrics, PlacedLine, TrackText,
};
use crate::text::{FontFactory, GlyphSource, Shaped};

/// 字幕底衬的颜色（近黑）。alpha 由设置里的 `background_alpha` 给。
const PLATE_COLOR: Rgb = Rgb::new(0, 0, 0);
/// 整块上浮动画的时长（施工稿 C6：约 200ms，ease-out）。
const SCROLL_ANIMATION_MS: f32 = 200.0;

/// 一帧需要的全部输入。
pub struct FrameInput<'a> {
    pub frame: &'a SubtitleFrame,
    pub settings: &'a SubtitleSettings,
    /// 客户区宽度（物理像素）。
    pub client_width: i32,
    /// 窗口当前高度。内容按**底边对齐**画进这块画布（字幕贴着屏幕下沿长）；
    /// 窗口比内容矮时顶上的区域会被裁掉，这时壳要照 [`FrameOutput::client_height`]
    /// 把窗口长高。`None` = 画布就取内容需要的高度（快照、VR 纹理用）。
    pub client_height: Option<i32>,
    pub dpi: u32,
    /// 这一帧的时刻。上浮动画按它推进，测试可以喂合成时刻。
    pub now: Instant,
}

/// 画完一帧之后的回报。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameOutput {
    /// 内容需要的窗口高度 = 在跑轨数 × 行数 × 行距 + 边距，**不随内容变**（C4）。
    /// 窗口矮于它就该长高，否则最上面一块区域会被裁。
    pub client_height: i32,
}

#[derive(Clone, Copy)]
struct RowStyle {
    color: Rgb,
    plate_alpha: u8,
    radius: f32,
}

/// 一条轨的滚动状态（C6）：`pos` 以 ease-out 追 `target`。
///
/// 爆发输入时 `target` 会一连跳好几行，这时不追动画——先把多出来的部分瞬移掉，
/// 只保留最后一行的动画，否则字幕会一直拖着老行不肯走。
#[derive(Debug, Clone, Copy, Default)]
struct TrackScroll {
    target: f32,
    pos: f32,
    step: f32,
    from: f32,
    elapsed: f32,
    last_ms: Option<u64>,
}

impl TrackScroll {
    /// 推进一帧，返回当前应该用的滚动位移（像素）。
    fn advance(&mut self, total_lines: usize, visible: u32, step: f32, now_ms: u64) -> f32 {
        let target = total_lines.saturating_sub(visible as usize) as f32 * step;
        if target != self.target || step != self.step {
            self.target = target;
            self.from = self.pos;
            self.elapsed = 0.0;
            self.step = step;
        }
        let dt = match self.last_ms {
            Some(last) => now_ms.saturating_sub(last) as f32,
            None => 0.0,
        };
        self.last_ms = Some(now_ms);

        // 落后超过一行：多余的部分瞬移，只留最后一行的动画。
        if self.target - self.pos > step {
            self.pos = self.target - step;
            self.from = self.pos;
            self.elapsed = 0.0;
        }
        if self.pos < self.target {
            self.elapsed += dt;
            let t = (self.elapsed / SCROLL_ANIMATION_MS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            self.pos = self.from + (self.target - self.from) * eased;
            if t >= 1.0 {
                self.pos = self.target;
            }
        } else {
            self.pos = self.target;
        }
        self.pos
    }

    fn is_animating(&self) -> bool {
        (self.target - self.pos).abs() > f32::EPSILON
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 一条轨的排版锚点：排版从这里开始，锚点之前的字不再参与折行。
///
/// **锚点永远落在某个视觉行的行首**，所以从它起重新贪心折行，结果与原来从该行起的
/// 折行逐字相同——丢掉它之前的内容不会让剩下的字重排。这是长时间连续说话时
/// 仍能保住"字不动"的关键：模型会按预算丢掉队首的字，但可见字的排版不依赖它们。
///
/// `abs` 是锚点那个字的绝对下标（`SubtitleLine::origin` + 帧内下标），跨帧稳定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TrackAnchor {
    abs: u64,
    /// 锚点之前已经折出、早已滚出区域的行数。
    lines: usize,
    /// 锚点往前推过之后，整条字幕的基础方向就钉死在推之前的值上：
    /// 否则开头那几个定方向的字被丢掉，剩下的第一个强方向字可能不同，整条字幕会翻面。
    /// `None` = 锚点还在帧首，方向每帧按"第一个强方向字符"重新判。
    rtl_pin: Option<bool>,
}

/// 锚点之后至少再留几行"区域之外"的行才允许把锚点往前推。
///
/// 改译可能让行数缩水，滚出去的行要能回来；留 `lines_per_track + 1` 行余量，
/// 缩水不超过这个数的改译不会让已经丢开的行需要重现。
const ANCHOR_SLACK_EXTRA: usize = 1;

/// 一条轨的排版缓冲：整形、双向分析、度量都只在**文字变了**的时候重做，
/// 动画帧（文字没变）直接复用，所以渲染每帧不做新分配。
#[derive(Debug, Default)]
struct TrackScratch {
    /// 上一次排版用的输入（控制字符已换成空格），逐字比对用来判断文字变没变。
    src: Vec<char>,
    /// `src` 拼成的串，整形与 UAX #14 / UAX #9 都吃它。
    text: String,
    /// 钉死的方向（见 [`TrackAnchor::rtl_pin`]）是否与上次排版时一致。
    pinned: Option<bool>,
    valid: bool,
    rtl: bool,
    shaped: Shaped,
    /// 以下都是**每个单元（字形簇）**一项。
    chars: Vec<char>,
    advances: Vec<i32>,
    breaks: Vec<bool>,
    unit_bytes: Vec<u32>,
    /// 单元的第一个字符在 `src` 里的下标，末尾多一项 = `src.len()`。
    unit_chars: Vec<u32>,
    levels: Vec<u8>,
    line_height: i32,
    ascent: i32,
    /// 本帧这条轨第 0 个字的绝对下标。
    origin: u64,
    /// 排版起点（锚点）在本帧字符序列里的下标。
    base: usize,
    first_line: usize,
}

/// 控制字符与段落分隔符一律当空格：它们没有字形，还会让 UAX #9 把一条字幕劈成多个段落。
fn normalize(ch: char) -> char {
    if ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}' {
        ' '
    } else {
        ch
    }
}

impl TrackScratch {
    /// 量一遍这条轨**从锚点起**的度量，并把 UAX #14 的断行机会填好。
    ///
    /// 锚点对不上本帧（轨被清空、模型把锚点之前的字丢到了锚点后面）就退回帧首，
    /// 返回 `Fresh`，调用方要连带重置上浮动画。
    fn measure(
        &mut self,
        line: Option<&SubtitleLine>,
        anchor: &mut TrackAnchor,
        font: &mut dyn GlyphSource,
    ) -> Measured {
        let Some(line) = line else {
            self.valid = false;
            self.src.clear();
            self.text.clear();
            self.chars.clear();
            self.advances.clear();
            self.breaks.clear();
            let had = *anchor != TrackAnchor::default();
            *anchor = TrackAnchor::default();
            return if had {
                Measured::Gone
            } else {
                Measured::Absent
            };
        };
        let metrics = font.metrics();
        self.line_height = metrics.line_height.max(1);
        self.ascent = metrics.ascent;
        self.origin = line.origin;
        let end = line.origin + line.chars.len() as u64;
        let mut fresh = false;
        if anchor.abs < line.origin || anchor.abs >= end {
            *anchor = TrackAnchor {
                abs: line.origin,
                lines: 0,
                rtl_pin: None,
            };
            fresh = true;
        }
        self.base = (anchor.abs - line.origin) as usize;
        self.first_line = anchor.lines;
        let source = &line.chars[self.base..];
        let unchanged = self.valid
            && self.pinned == anchor.rtl_pin
            && self.src.len() == source.len()
            && source
                .iter()
                .zip(&self.src)
                .all(|(rc, ch)| normalize(rc.ch) == *ch);
        if !unchanged {
            self.relayout(
                source.iter().map(|rc| normalize(rc.ch)),
                anchor.rtl_pin,
                font,
            );
        }
        if fresh {
            Measured::Fresh
        } else {
            Measured::Present
        }
    }

    /// 文字变了：重新判方向、双向分层、整形、分簇、填断行机会。
    fn relayout(
        &mut self,
        chars: impl Iterator<Item = char>,
        pin: Option<bool>,
        font: &mut dyn GlyphSource,
    ) {
        self.src.clear();
        self.src.extend(chars);
        self.text.clear();
        self.text.extend(self.src.iter());
        self.pinned = pin;
        self.valid = true;
        self.shaped.clear();
        self.chars.clear();
        self.advances.clear();
        self.unit_bytes.clear();
        self.unit_chars.clear();
        self.levels.clear();
        self.breaks.clear();
        if self.text.is_empty() {
            self.rtl = pin.unwrap_or(false);
            return;
        }
        // 整条字幕的基础方向：第一个强方向字符定（全是中性字符就从左往右）。
        self.rtl = pin.unwrap_or_else(|| {
            unicode_bidi::get_base_direction(self.text.as_str()) == Direction::Rtl
        });
        let base_level = if self.rtl { Level::rtl() } else { Level::ltr() };
        let info = BidiInfo::new(&self.text, Some(base_level));
        // 按层级切成同方向的段，逐段整形；簇永远按逻辑顺序追加。
        let mut run_start = 0usize;
        let mut run_level = info.levels.first().copied().unwrap_or(base_level);
        for (byte, _) in self.text.char_indices().skip(1) {
            if info.levels[byte] != run_level {
                font.shape(
                    &self.text[run_start..byte],
                    run_level.is_rtl(),
                    run_start as u32,
                    &mut self.shaped,
                );
                run_start = byte;
                run_level = info.levels[byte];
            }
        }
        font.shape(
            &self.text[run_start..],
            run_level.is_rtl(),
            run_start as u32,
            &mut self.shaped,
        );

        // 簇 → 单元数组。簇的 `start` 单调不减，字节偏移换字符下标用双指针。
        let (mut ci, mut bp) = (0usize, 0usize);
        for cluster in &self.shaped.clusters {
            let start = cluster.start as usize;
            while ci < self.src.len() && bp + self.src[ci].len_utf8() <= start {
                bp += self.src[ci].len_utf8();
                ci += 1;
            }
            let first = self.src.get(ci).copied().unwrap_or(' ');
            self.chars.push(first);
            self.advances.push(cluster.advance.max(0));
            self.unit_bytes.push(cluster.start);
            self.unit_chars.push(ci as u32);
            let level = info.levels.get(start).copied().unwrap_or(base_level);
            self.levels.push(level.number());
        }
        self.unit_chars.push(self.src.len() as u32);
        fill_breaks(&self.text, &self.unit_bytes, &mut self.breaks);
    }

    fn text(&self) -> TrackText<'_> {
        TrackText {
            chars: &self.chars,
            advances: &self.advances,
            breaks: &self.breaks,
            unit_chars: &self.unit_chars,
            levels: &self.levels,
            rtl: self.rtl,
            line_height: self.line_height,
            ascent: self.ascent,
            base: self.base,
            first_line: self.first_line,
        }
    }
}

/// `TrackScratch::measure` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measured {
    /// 本帧没有这条轨，上一帧也没有。
    Absent,
    /// 本帧没有这条轨，上一帧有（要把上浮动画归零）。
    Gone,
    /// 有，沿用上一帧的锚点。
    Present,
    /// 有，但锚点作废、从帧首重新排（新内容出现 / 清空后重来）。
    Fresh,
}

impl Measured {
    fn present(self) -> bool {
        matches!(self, Measured::Present | Measured::Fresh)
    }
}

/// 把锚点往前推到"已经滚出区域、且留够余量"的最后一个行首。
fn advance_anchor(
    anchor: &mut TrackAnchor,
    origin: u64,
    rtl: bool,
    region: &crate::layout::TrackLayout,
    lines_per_track: usize,
) {
    let slack = lines_per_track + ANCHOR_SLACK_EXTRA;
    let Some(k) = region.lines.len().checked_sub(lines_per_track + slack) else {
        return;
    };
    if k == 0 {
        return;
    }
    // 第 k 行的行首就是新锚点：它之前的 k 行都在区域上方、且留够了余量。
    anchor.abs = origin + region.lines[k].first as u64;
    anchor.lines = region.first_line + k;
    anchor.rtl_pin = Some(rtl);
}

/// 一帧一帧往 `Canvas` 上画。持有字体，所以只能在窗口线程上用。
pub struct Renderer {
    font: Box<dyn GlyphSource>,
    /// 设置里的字体族/字号/DPI 变了要重建光栅器，所以工厂也存着。
    font_factory: FontFactory,
    /// 记住建字体时的参数，`sync_font` 只在真的变了的时候重建。
    font_family: String,
    font_size: u32,
    dpi: u32,
    /// 字体度量随字体走，布局要用。
    line_height: i32,
    ascent: i32,
    listen_scratch: TrackScratch,
    speak_scratch: TrackScratch,
    listen_scroll: TrackScroll,
    speak_scroll: TrackScroll,
    listen_anchor: TrackAnchor,
    speak_anchor: TrackAnchor,
    /// 一行重排成显示顺序时用的复用缓冲。
    order: Vec<u32>,
    order_levels: Vec<u8>,
    epoch: Instant,
    last_frame_ms: u64,
}

/// 一个排好版的单元（字形簇）落在哪：测试与画图共用这一份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// 簇覆盖整帧字符序列里的 `[first, last)`。
    pub first: usize,
    pub last: usize,
    /// 簇的左边缘与宽度（物理像素，已按显示顺序摆好）。
    pub x: i32,
    pub width: i32,
}

impl Renderer {
    /// `font_factory` 由平台提供（Windows：GDI；Linux：swash）。
    pub fn new(
        settings: &SubtitleSettings,
        dpi: u32,
        font_factory: FontFactory,
    ) -> vox_core::ports::PortResult<Self> {
        let font = font_factory(&settings.font_family, settings.font_size, dpi)?;
        let metrics = font.metrics();
        let epoch = Instant::now();
        Ok(Self {
            font,
            font_factory,
            font_family: settings.font_family.clone(),
            font_size: settings.font_size,
            dpi,
            line_height: metrics.line_height,
            ascent: metrics.ascent,
            listen_scratch: TrackScratch::default(),
            speak_scratch: TrackScratch::default(),
            listen_scroll: TrackScroll::default(),
            speak_scroll: TrackScroll::default(),
            listen_anchor: TrackAnchor::default(),
            speak_anchor: TrackAnchor::default(),
            order: Vec::new(),
            order_levels: Vec::new(),
            epoch,
            last_frame_ms: 0,
        })
    }

    /// 字体或 DPI 变了才换字体；没变就保留现有字形缓存。
    pub fn sync_font(
        &mut self,
        settings: &SubtitleSettings,
        dpi: u32,
    ) -> vox_core::ports::PortResult<()> {
        if self.font_family == settings.font_family
            && self.font_size == settings.font_size
            && self.dpi == dpi
        {
            return Ok(());
        }
        self.font = (self.font_factory)(&settings.font_family, settings.font_size, dpi)?;
        let metrics = self.font.metrics();
        self.font_family = settings.font_family.clone();
        self.font_size = settings.font_size;
        self.dpi = dpi;
        self.line_height = metrics.line_height;
        self.ascent = metrics.ascent;
        // A font/DPI change invalidates every measured boundary. Let the next frame
        // establish a fresh layout rather than carrying pixel positions across metrics.
        self.listen_scratch = TrackScratch::default();
        self.speak_scratch = TrackScratch::default();
        self.listen_scroll.reset();
        self.speak_scroll.reset();
        self.listen_anchor = TrackAnchor::default();
        self.speak_anchor = TrackAnchor::default();
        Ok(())
    }

    /// 还有行在往上浮吗？只有这时才需要继续请求重绘。
    pub fn is_animating(&self) -> bool {
        self.listen_scroll.is_animating() || self.speak_scroll.is_animating()
    }

    /// 算一帧的布局，不画。返回的 [`FrameLayout`] 里是每个字最终落在哪。
    pub fn plan(&mut self, input: &FrameInput<'_>) -> FrameLayout {
        let metrics = Metrics::for_dpi(input.dpi);
        let listen_state = self.listen_scratch.measure(
            find_line(input.frame, Track::Listen),
            &mut self.listen_anchor,
            self.font.as_mut(),
        );
        let speak_state = self.speak_scratch.measure(
            find_line(input.frame, Track::Speak),
            &mut self.speak_anchor,
            self.font.as_mut(),
        );
        // 轨消失或锚点作废 = 内容换了一茬，上浮动画从头来，别拖着上一茬的位移。
        for (state, scroll) in [
            (listen_state, &mut self.listen_scroll),
            (speak_state, &mut self.speak_scroll),
        ] {
            if matches!(state, Measured::Gone | Measured::Fresh) {
                scroll.reset();
            }
        }
        let (has_listen, has_speak) = (listen_state.present(), speak_state.present());

        let now_ms = input.now.saturating_duration_since(self.epoch).as_millis() as u64;
        let lines_per_track = input.settings.lines_per_track.max(1);
        let step = metrics.line_step(self.line_height);
        self.last_frame_ms = now_ms;

        let listen = &mut self.listen_scroll;
        let speak = &mut self.speak_scroll;
        let layout = layout_regions(
            has_listen.then(|| self.listen_scratch.text()),
            has_speak.then(|| self.speak_scratch.text()),
            |is_listen, total_lines| {
                let visible = if is_listen { has_listen } else { has_speak };
                let visible = if visible { lines_per_track } else { 0 };
                if is_listen {
                    listen.advance(total_lines, visible, step, now_ms)
                } else {
                    speak.advance(total_lines, visible, step, now_ms)
                }
            },
            input.client_width,
            lines_per_track,
            metrics,
        );
        // 排版的结果就是下一帧锚点的依据：已经滚出区域并留够余量的行不再参与折行。
        if let Some(region) = &layout.listen {
            advance_anchor(
                &mut self.listen_anchor,
                self.listen_scratch.origin,
                self.listen_scratch.rtl,
                region,
                lines_per_track as usize,
            );
        }
        if let Some(region) = &layout.speak {
            advance_anchor(
                &mut self.speak_anchor,
                self.speak_scratch.origin,
                self.speak_scratch.rtl,
                region,
                lines_per_track as usize,
            );
        }
        layout
    }

    /// 把一帧画到 `canvas` 上。`resize` 本身会清空画布，不再重复清屏。
    pub fn draw(&mut self, canvas: &mut Canvas, input: &FrameInput<'_>) -> FrameOutput {
        let metrics = Metrics::for_dpi(input.dpi);
        let layout = self.plan(input);
        let height = input.client_height.unwrap_or(layout.client_height).max(1);
        canvas.resize(input.client_width.max(1), height);
        // 底边对齐：窗口比内容高就把内容压到底下，矮就从顶上裁。
        let dy = if layout.client_height > 1 {
            (height - layout.client_height) as f32
        } else {
            0.0
        };

        let listen_color = parse_hex_rgb_or(&input.settings.listen_color, Rgb::WHITE);
        let speak_color = parse_hex_rgb_or(&input.settings.speak_color, Rgb::WHITE);
        for (track, region, color) in [
            (Track::Listen, layout.listen.as_ref(), listen_color),
            (Track::Speak, layout.speak.as_ref(), speak_color),
        ] {
            let (Some(region), Some(line)) = (region, find_line(input.frame, track)) else {
                continue;
            };
            let style = RowStyle {
                color,
                plate_alpha: input.settings.background_alpha,
                radius: metrics.plate_radius as f32,
            };
            let scratch = match track {
                Track::Listen => &self.listen_scratch,
                Track::Speak => &self.speak_scratch,
            };
            canvas.push_clip(region.region.0 + dy as i32, region.region.1 + dy as i32);
            for placed in &region.lines {
                draw_line(
                    self.font.as_mut(),
                    &mut self.order,
                    &mut self.order_levels,
                    canvas,
                    placed,
                    line,
                    scratch,
                    style,
                    dy,
                );
            }
            canvas.pop_clip();
        }

        FrameOutput {
            client_height: layout.client_height,
        }
    }

    /// 最近一次 [`Renderer::plan`] / [`Renderer::draw`] 之后，`placed` 这一行里每个簇落在哪
    /// （按显示顺序，从左到右）。不变量测试靠它读"字最终摆在哪"。
    pub fn cells(&mut self, track: Track, placed: &PlacedLine) -> Vec<Cell> {
        let scratch = match track {
            Track::Listen => &self.listen_scratch,
            Track::Speak => &self.speak_scratch,
        };
        let mut out = Vec::new();
        for_each_cell(
            &mut self.order,
            &mut self.order_levels,
            placed,
            scratch,
            |unit, x| {
                out.push(Cell {
                    first: scratch.base + scratch.unit_chars[unit] as usize,
                    last: scratch.base + scratch.unit_chars[unit + 1] as usize,
                    x,
                    width: scratch.advances[unit],
                });
            },
        );
        out
    }
}

/// 按显示顺序走一遍这一行的簇，给出每个簇（单元下标）的左边缘 x。
fn for_each_cell(
    order: &mut Vec<u32>,
    levels: &mut Vec<u8>,
    placed: &PlacedLine,
    scratch: &TrackScratch,
    mut visit: impl FnMut(usize, i32),
) {
    let text = scratch.text();
    let (first, last) = (placed.unit_first, placed.unit_last.min(scratch.chars.len()));
    visual_order(
        &text,
        first,
        last,
        |unit| scratch.chars[unit].is_whitespace(),
        order,
        levels,
    );
    let mut x = placed.text_x;
    for &k in order.iter() {
        let unit = first + k as usize;
        visit(unit, x);
        x += scratch.advances[unit];
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    font: &mut dyn GlyphSource,
    order: &mut Vec<u32>,
    levels: &mut Vec<u8>,
    canvas: &mut Canvas,
    placed: &PlacedLine,
    line: &SubtitleLine,
    scratch: &TrackScratch,
    style: RowStyle,
    dy: f32,
) {
    // 正在移出区域顶部的那一行按被裁的比例淡出：底衬和墨迹一起淡。
    let fade = placed.clip;
    if fade <= 0.0 {
        return;
    }
    let y = (placed.y + dy).round() as i32;
    // 整行的字都已淡成透明（逐句淡出时前面的句子先走）就不画底衬：
    // 行位置不动，只是不留一块空底衬杵在上面。
    let (c0, c1) = (placed.first, placed.last);
    let all_clear = c1 > c0
        && (c0..c1).all(|i| {
            line.chars
                .get(i)
                .is_some_and(|rc| alpha_to_u8(rc.alpha) == 0)
        });
    let plate_alpha = if all_clear {
        0
    } else {
        (style.plate_alpha as f32 * fade).round() as u8
    };
    if plate_alpha > 0 {
        canvas.fill_round_rect(
            crate::geom::RectI::new(placed.plate_x, y, placed.plate_w, placed.plate_h),
            style.radius,
            PLATE_COLOR,
            plate_alpha,
        );
    }
    let baseline_y = (placed.baseline_y + dy).round() as i32;
    for_each_cell(order, levels, placed, scratch, |unit, x| {
        // 簇的透明度取它第一个字符的（未确认尾巴是逐字 0.7 的，簇很少跨过那条边界）。
        let char_index = scratch.base + scratch.unit_chars[unit] as usize;
        let alpha = line.chars.get(char_index).map_or(1.0, |rc| rc.alpha);
        let alpha = alpha_to_u8(alpha * fade);
        if alpha == 0 {
            return;
        }
        let cluster = scratch.shaped.clusters[unit];
        let mut pen = x;
        for glyph in
            &scratch.shaped.glyphs[cluster.glyph_start as usize..cluster.glyph_end as usize]
        {
            if let Some(g) = font.glyph(glyph.face, glyph.id) {
                if g.w > 0 && g.h > 0 {
                    canvas.blend_mask(
                        (
                            pen + glyph.x_offset + g.off_x,
                            baseline_y - glyph.y_offset + g.off_y,
                        ),
                        Mask {
                            w: g.w,
                            h: g.h,
                            cov: &g.cov,
                        },
                        style.color,
                        alpha,
                    );
                }
            }
            pen += glyph.advance;
        }
    });
}

fn find_line(frame: &SubtitleFrame, track: Track) -> Option<&SubtitleLine> {
    frame.lines.iter().find(|l| l.track == track)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 等宽假光栅器：每个字固定步进，掩码是一块实心方块。跟字形引擎无关。
    struct FakeFont {
        advance: i32,
        line_height: i32,
        ascent: i32,
    }

    impl GlyphSource for FakeFont {
        fn metrics(&self) -> crate::text::FontMetrics {
            crate::text::FontMetrics {
                line_height: self.line_height,
                ascent: self.ascent,
            }
        }

        fn shape(&mut self, run: &str, _rtl: bool, base: u32, out: &mut crate::text::Shaped) {
            for (i, _) in run.char_indices() {
                let glyph = out.glyphs.len() as u32;
                out.glyphs.push(crate::text::ShapedGlyph {
                    face: 0,
                    id: 0,
                    x_offset: 0,
                    y_offset: 0,
                    advance: self.advance,
                });
                out.clusters.push(crate::text::Cluster {
                    start: base + i as u32,
                    advance: self.advance,
                    glyph_start: glyph,
                    glyph_end: glyph + 1,
                });
            }
        }

        fn glyph(&mut self, _face: u16, _id: u16) -> Option<&crate::text::Glyph> {
            None
        }
    }

    fn settings() -> SubtitleSettings {
        SubtitleSettings::default()
    }

    fn renderer(advance: i32) -> Renderer {
        Renderer::new(
            &settings(),
            96,
            crate::text::font_factory(move |_, size, _| {
                Ok(Box::new(FakeFont {
                    advance: advance.max(1) * (size as i32 / 30).max(1),
                    line_height: 40 * (size as i32 / 30).max(1),
                    ascent: 32 * (size as i32 / 30).max(1),
                }) as Box<dyn GlyphSource>)
            }),
        )
        .expect("假字体不会失败")
    }

    fn frame(lines: &[(Track, &str)]) -> SubtitleFrame {
        SubtitleFrame {
            lines: lines
                .iter()
                .map(|(track, text)| SubtitleLine {
                    track: *track,
                    origin: 0,
                    chars: text
                        .chars()
                        .map(|ch| vox_core::subtitle::RenderedChar { ch, alpha: 1.0 })
                        .collect(),
                    color: "#ffffff".into(),
                })
                .collect(),
        }
    }

    fn input<'a>(
        frame: &'a SubtitleFrame,
        settings: &'a SubtitleSettings,
        now: Instant,
    ) -> FrameInput<'a> {
        FrameInput {
            frame,
            settings,
            client_width: 880,
            client_height: None,
            dpi: 96,
            now,
        }
    }

    #[test]
    fn find_line_picks_the_right_track() {
        let frame = frame(&[(Track::Speak, "a"), (Track::Listen, "b")]);
        assert_eq!(find_line(&frame, Track::Speak).unwrap().chars[0].ch, 'a');
        assert_eq!(find_line(&frame, Track::Listen).unwrap().chars[0].ch, 'b');
    }

    #[test]
    fn scroll_settles_at_target_after_the_animation() {
        let mut scroll = TrackScroll::default();
        scroll.advance(10, 2, 46.0, 0);
        assert!(scroll.is_animating(), "目标变了就该在追");
        let mut now = 0u64;
        for _ in 0..40 {
            now += 16;
            scroll.advance(10, 2, 46.0, now);
        }
        assert!(
            !scroll.is_animating(),
            "200ms 之后必须停稳，实际 pos={} target={}",
            scroll.pos,
            scroll.target
        );
    }

    #[test]
    fn burst_input_never_lags_more_than_one_line() {
        let mut scroll = TrackScroll::default();
        scroll.advance(1, 2, 46.0, 0);
        // 爆发：一帧里多出十行。
        for frame in 1..=20u64 {
            scroll.advance(12, 2, 46.0, frame * 16);
            assert!(
                scroll.target - scroll.pos <= 46.0 + 1e-3,
                "落后必须不超过一行，实际落后 {}",
                scroll.target - scroll.pos
            );
        }
    }

    #[test]
    fn empty_frame_is_one_transparent_pixel() {
        let mut r = renderer(30);
        let settings = settings();
        let frame = SubtitleFrame { lines: Vec::new() };
        let mut canvas = Canvas::new(880, 1);
        let out = r.draw(&mut canvas, &input(&frame, &settings, Instant::now()));
        assert_eq!(out.client_height, 1);
        assert!(canvas.bytes().iter().all(|b| *b == 0));
        assert!(!r.is_animating(), "空帧不该还在动画");
    }

    #[test]
    fn height_follows_regions_and_line_count_only() {
        let mut r = renderer(30);
        let settings = settings();
        let short = frame(&[(Track::Listen, "短")]);
        let long = frame(&[(Track::Listen, &"很长的一句话".repeat(20))]);
        let a = r.plan(&input(&short, &settings, Instant::now()));
        let b = r.plan(&input(&long, &settings, Instant::now()));
        assert_eq!(a.client_height, b.client_height, "高度不跟内容走");
        assert!(
            b.listen.expect("区域").lines.len() > a.listen.expect("区域").lines.len(),
            "但行数要跟着内容涨"
        );
    }

    #[test]
    fn one_track_never_moves_the_other() {
        let mut r = renderer(30);
        let settings = settings();
        let listen_only = frame(&[(Track::Listen, "听人说话")]);
        let layout_a = r.plan(&input(&listen_only, &settings, Instant::now()));
        let both = frame(&[(Track::Listen, "听人说话"), (Track::Speak, "对外说话")]);
        let layout_b = r.plan(&input(&both, &settings, Instant::now()));
        assert_eq!(
            layout_a.listen, layout_b.listen,
            "对外那条轨有内容，不该挪动听人那条轨"
        );
    }

    #[test]
    fn base_direction_follows_the_first_strong_char() {
        let mut r = renderer(30);
        let settings = settings();
        let arabic = frame(&[(Track::Listen, "\u{0628}\u{0628} abc")]);
        let latin = frame(&[(Track::Listen, "abc \u{0628}\u{0628}")]);
        let a = r.plan(&input(&arabic, &settings, Instant::now()));
        assert!(
            a.listen.expect("区域").lines[0].rtl,
            "阿拉伯字母打头 = 从右往左"
        );
        let b = r.plan(&input(&latin, &settings, Instant::now()));
        assert!(
            !b.listen.expect("区域").lines[0].rtl,
            "拉丁字母打头 = 从左往右"
        );
        let digits = frame(&[(Track::Listen, "123 ...")]);
        let c = r.plan(&input(&digits, &settings, Instant::now()));
        assert!(
            !c.listen.expect("区域").lines[0].rtl,
            "全是中性字符就从左往右"
        );
    }

    #[test]
    fn direction_stays_pinned_after_the_anchor_moves_past_the_first_strong_char() {
        // 阿拉伯字母打头（整条字幕从右往左），后面是很长的英文：锚点推到英文里之后，
        // 从锚点起的第一个强方向字符是拉丁字母——方向若不钉住，整条字幕会翻面、所有行换边。
        let mut r = renderer(30);
        let settings = settings();
        let text = format!("\u{0628} {}", "word ".repeat(400));
        let f = frame(&[(Track::Listen, &text)]);
        let first = r.plan(&input(&f, &settings, Instant::now()));
        assert!(first.listen.expect("区域").lines[0].rtl);
        for _ in 0..3 {
            let layout = r.plan(&input(&f, &settings, Instant::now()));
            let region = layout.listen.expect("区域");
            assert!(
                region.first_line > 0,
                "锚点必须真的往前推过，否则这条测试没测到东西"
            );
            assert!(
                region.lines.iter().all(|l| l.rtl),
                "锚点推过之后方向不许翻：{:?}",
                region.lines.iter().map(|l| l.rtl).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn cells_follow_the_visual_order_of_a_mixed_line() {
        // 假字体每个字符一个簇、宽 30。"ab" + 两个阿拉伯字母 + "cd"，从左往右的句子：
        // 阿拉伯字母这一小段在视觉上整体反向。
        let mut r = renderer(30);
        let settings = settings();
        let f = frame(&[(Track::Listen, "ab\u{0628}\u{062A}cd")]);
        let layout = r.plan(&input(&f, &settings, Instant::now()));
        let line = layout.listen.expect("区域").lines[0].clone();
        let cells = r.cells(Track::Listen, &line);
        let visual: Vec<usize> = cells.iter().map(|c| c.first).collect();
        assert_eq!(visual, vec![0, 1, 3, 2, 4, 5], "第 2、3 个字符在视觉上互换");
        let xs: Vec<i32> = cells.iter().map(|c| c.x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "x 严格递增：{xs:?}");
    }

    #[test]
    fn control_chars_never_split_the_line_into_paragraphs() {
        let mut r = renderer(30);
        let settings = settings();
        let f = frame(&[(Track::Listen, "ab\ncd\u{2029}ef")]);
        let layout = r.plan(&input(&f, &settings, Instant::now()));
        let line = layout.listen.expect("区域").lines[0].clone();
        assert_eq!((line.first, line.last), (0, 8), "八个字符一个不丢");
    }

    #[test]
    fn last_frame_time_is_recorded_for_the_animation() {
        let mut r = renderer(30);
        let settings = settings();
        let epoch = r.epoch + Duration::from_millis(1234);
        let frame = frame(&[(Track::Listen, "abc")]);
        r.plan(&input(&frame, &settings, epoch));
        assert_eq!(r.last_frame_ms, 1234);
    }
}
