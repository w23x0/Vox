//! 画一整帧纯字幕：逐行底衬 + 逐字 alpha 的文字。
//!
//! 跟窗口无关——输入是内容和几何，输出是一块预乘 BGRA 的 `Canvas`，所以快照
//! 工具可以不开窗口就复用完全相同的像素路径。
//!
//! 一帧分两步：[`Renderer::plan`] 算布局（含上浮动画的位置），[`Renderer::draw`]
//! 再照着布局上色。拆开是为了让"字摆在哪"这件事能脱离像素单测——不变量测试
//! （`tests/invariants.rs`）盯的就是 `plan` 的输出。

use std::time::Instant;

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};

use crate::canvas::{Canvas, Mask};
use crate::color::{alpha_to_u8, parse_hex_rgb_or, Rgb};
use crate::layout::{fill_breaks, layout_regions, FrameLayout, Metrics, PlacedLine, TrackText};
use crate::text::{FontFactory, GlyphSource};

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
}

/// 锚点之后至少再留几行"区域之外"的行才允许把锚点往前推。
///
/// 改译可能让行数缩水，滚出去的行要能回来；留 `lines_per_track + 1` 行余量，
/// 缩水不超过这个数的改译不会让已经丢开的行需要重现。
const ANCHOR_SLACK_EXTRA: usize = 1;

/// 一条轨每帧复用的缓冲。渲染每帧都会跑，**这里不做新分配**。
#[derive(Debug, Default)]
struct TrackScratch {
    text: String,
    chars: Vec<char>,
    advances: Vec<i32>,
    breaks: Vec<bool>,
    line_height: i32,
    ascent: i32,
    /// 本帧这条轨第 0 个字的绝对下标。
    origin: u64,
    /// 排版起点在本帧字序列里的下标（锚点换算出来的）。
    base: usize,
    first_line: usize,
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
        self.chars.clear();
        self.advances.clear();
        self.breaks.clear();
        self.text.clear();
        let Some(line) = line else {
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
            };
            fresh = true;
        }
        self.base = (anchor.abs - line.origin) as usize;
        self.first_line = anchor.lines;
        for rc in &line.chars[self.base..] {
            self.chars.push(rc.ch);
            self.advances.push(font.advance(rc.ch).max(0));
            self.text.push(rc.ch);
        }
        fill_breaks(&self.text, &self.chars, &mut self.breaks);
        if fresh {
            Measured::Fresh
        } else {
            Measured::Present
        }
    }

    fn text(&self) -> TrackText<'_> {
        TrackText {
            chars: &self.chars,
            advances: &self.advances,
            breaks: &self.breaks,
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
    epoch: Instant,
    last_frame_ms: u64,
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
                region,
                lines_per_track as usize,
            );
        }
        if let Some(region) = &layout.speak {
            advance_anchor(
                &mut self.speak_anchor,
                self.speak_scratch.origin,
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
            canvas.push_clip(region.region.0 + dy as i32, region.region.1 + dy as i32);
            for placed in &region.lines {
                self.draw_line(canvas, placed, line, style, dy);
            }
            canvas.pop_clip();
        }

        FrameOutput {
            client_height: layout.client_height,
        }
    }

    fn draw_line(
        &mut self,
        canvas: &mut Canvas,
        placed: &PlacedLine,
        line: &SubtitleLine,
        style: RowStyle,
        dy: f32,
    ) {
        // 正在移出区域顶部的那一行按被裁的比例淡出：底衬和墨迹一起淡。
        let fade = placed.clip;
        if fade <= 0.0 {
            return;
        }
        let y = (placed.y + dy).round() as i32;
        let plate_alpha = (style.plate_alpha as f32 * fade).round() as u8;
        if plate_alpha > 0 {
            canvas.fill_round_rect(
                crate::geom::RectI::new(placed.plate_x, y, placed.plate_w, placed.plate_h),
                style.radius,
                PLATE_COLOR,
                plate_alpha,
            );
        }
        let baseline_y = (placed.baseline_y + dy).round() as i32;
        let mut x = placed.text_x;
        for rc in line.chars.iter().take(placed.last).skip(placed.first) {
            x += draw_char_with(
                self.font.as_mut(),
                canvas,
                x,
                baseline_y,
                rc,
                style.color,
                fade,
            );
        }
    }
}

/// 画一个字，返回下一个字的起笔步进。
fn draw_char_with(
    font: &mut dyn GlyphSource,
    canvas: &mut Canvas,
    pen_x: i32,
    baseline_y: i32,
    rc: &RenderedChar,
    color: Rgb,
    extra_fade: f32,
) -> i32 {
    let alpha = alpha_to_u8(rc.alpha * extra_fade);
    let Some(g) = font.glyph(rc.ch) else {
        return 0;
    };
    let advance = g.advance;
    if alpha == 0 || g.w <= 0 || g.h <= 0 {
        return advance;
    }
    canvas.blend_mask(
        (pen_x + g.off_x, baseline_y + g.off_y),
        Mask {
            w: g.w,
            h: g.h,
            cov: &g.cov,
        },
        color,
        alpha,
    );
    advance
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

        fn advance(&mut self, _ch: char) -> i32 {
            self.advance
        }

        fn glyph(&mut self, _ch: char) -> Option<&crate::text::Glyph> {
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
                        .map(|ch| RenderedChar { ch, alpha: 1.0 })
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
    fn last_frame_time_is_recorded_for_the_animation() {
        let mut r = renderer(30);
        let settings = settings();
        let epoch = r.epoch + Duration::from_millis(1234);
        let frame = frame(&[(Track::Listen, "abc")]);
        r.plan(&input(&frame, &settings, epoch));
        assert_eq!(r.last_frame_ms, 1234);
    }
}
