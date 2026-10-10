//! 布局：一条轨一块区域，区域内按 UAX #14 增量折行、整块上浮。
//!
//! 纯算术，没有 Win32，能单测。字宽由调用方量出来（GDI 或假的量尺）再传进来，
//! 这样布局逻辑跟字体引擎解耦。
//!
//! 尺寸一律是**物理像素**：DPI 缩放在算 [`Metrics`] 时一次性折进去，之后的代码
//! 不再关心缩放系数。
//!
//! ## 三条硬规矩
//!
//! 1. **不横向滚动**：一行排不下就换行，绝不从左侧丢掉老字（那是旧 `layout_rows`
//!    的行为，每来一个字整行左移一格）。
//! 2. **同侧对齐**：从左往右的轨，所有视觉行的起笔 x 相同；从右往左的轨，所有视觉行的
//!    **右边缘**相同（右对齐，新字长在左边）。所以换行/上浮都不会产生横向运动。
//! 3. **两条轨各占一个区域**：听在上、说在下，区域高度固定，谁也不推谁。
//!
//! 排版单元是**字形簇**（见 `text::Cluster`）：折行永远在逻辑顺序上做，一行排定之后
//! 才按 UAX #9 的 L1/L2 把这一行里的簇重排成显示顺序（[`visual_order`]）。

use unicode_linebreak::linebreaks;

use crate::geom::RectI;

/// 布局常量，按 DPI 缩放后的物理像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// 窗口四周留白。
    pub padding: i32,
    /// 视觉行之间、以及两个区域之间的间距。
    pub row_gap: i32,
    /// 底衬左右内边距。
    pub plate_pad_x: i32,
    /// 底衬上下内边距。
    pub plate_pad_y: i32,
    /// 底衬圆角。
    pub plate_radius: i32,
}

impl Metrics {
    /// 逻辑像素下的一套值（padding 18 / 盒子 14×4 / 圆角 6）。
    pub const LOGICAL: Self = Self {
        padding: 18,
        row_gap: 6,
        plate_pad_x: 14,
        plate_pad_y: 4,
        plate_radius: 6,
    };

    /// 按 DPI 缩放。`dpi` 为 0 或异常值时退回 96，绝不产生 0 尺寸。
    pub fn for_dpi(dpi: u32) -> Self {
        let base = Self::LOGICAL;
        let dpi = if (72..=1200).contains(&dpi) { dpi } else { 96 };
        let scale = |v: i32| ((v as i64 * dpi as i64) / 96).max(1) as i32;
        Self {
            padding: scale(base.padding),
            row_gap: scale(base.row_gap),
            plate_pad_x: scale(base.plate_pad_x),
            plate_pad_y: scale(base.plate_pad_y),
            plate_radius: scale(base.plate_radius),
        }
    }

    /// 一个视觉行占多高（底衬高度）与相邻行相距多远。
    pub fn plate_height(&self, line_height: i32) -> i32 {
        line_height.max(1) + 2 * self.plate_pad_y
    }

    pub fn line_step(&self, line_height: i32) -> f32 {
        (self.plate_height(line_height) + self.row_gap) as f32
    }
}

/// 一条轨要排的文字 + 度量。
///
/// `chars` 是**排版单元**序列（OVERLAY-CAPTIONS C11 留的口子，OVERLAY-SHAPING 接上了）：
/// 一个单元是一个字形簇——阿拉伯语的一个连写词形、印地语的一个音节、泰语的"字母 + 附标"，
/// 也可以只是一个拉丁字母；`chars[i]` 是该簇的第一个字符。`advances` 与 `breaks` 按同一
/// 下标逐个对应，折行代码只认这三个数组。
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackText<'a> {
    pub chars: &'a [char],
    pub advances: &'a [i32],
    /// `breaks[i]` = 能不能在第 `i` 个单元**之前**断行。长度与 `chars` 一致，
    /// 由 [`fill_breaks`] 按 UAX #14 填好。
    pub breaks: &'a [bool],
    /// `unit_chars[i]` = 第 `i` 个单元的第一个字符在**锚点起的字符序列**里的下标，
    /// 末尾多一项 = 字符总数。空 = 一个单元一个字符（等价于恒等映射）。
    pub unit_chars: &'a [u32],
    /// 每个单元的 UAX #9 嵌入层级（偶数 = 从左往右）。空 = 全部是段落层级。
    pub levels: &'a [u8],
    /// 整条字幕的基础方向：`true` = 从右往左（右对齐）。
    pub rtl: bool,
    pub line_height: i32,
    pub ascent: i32,
    /// 锚点（`chars` 的第 0 个单元）的第一个字符在整帧字符序列里的下标。排版只从渲染侧记住的
    /// 行首锚点开始，锚点之前的字不再参与折行，这里把下标换回整帧的。
    pub base: usize,
    /// 锚点之前已经折出（并且早已滚出区域）的行数。
    pub first_line: usize,
}

impl<'a> TrackText<'a> {
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn len(&self) -> usize {
        self.chars.len().min(self.advances.len())
    }

    /// 第 `unit` 个单元的第一个字符下标（`unit == len` 时是字符总数）。
    pub fn char_at(&self, unit: usize) -> usize {
        self.unit_chars.get(unit).map_or(unit, |c| *c as usize)
    }

    /// 段落层级：从右往左是 1，否则 0。
    pub fn base_level(&self) -> u8 {
        u8::from(self.rtl)
    }

    /// 第 `unit` 个单元的嵌入层级。
    pub fn level_at(&self, unit: usize) -> u8 {
        self.levels.get(unit).copied().unwrap_or(self.base_level())
    }
}

/// 一个视觉行的位置。
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLine {
    /// 底衬顶边的 y（**带小数**：上浮动画就在这个分数上）。
    pub y: f32,
    pub plate_x: i32,
    pub plate_w: i32,
    pub plate_h: i32,
    /// 第一个字的起笔 x。
    pub text_x: i32,
    /// 基线 y（同一个小数偏移）。
    pub baseline_y: f32,
    /// 这一行盖住整帧字符序列里的 `[first, last)`（字符下标，已加上锚点偏移）。
    pub first: usize,
    pub last: usize,
    /// 同一行盖住的排版单元区间 `[unit_first, unit_last)`（锚点起的单元下标）。
    pub unit_first: usize,
    pub unit_last: usize,
    /// 这一行所属字幕的基础方向（`true` = 右对齐的从右往左）。
    pub rtl: bool,
    /// 顶部被区域裁掉的程度：1 = 完整可见，越小越淡。
    pub clip: f32,
}

/// 一条轨的整个区域。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackLayout {
    /// 锚点起的所有视觉行（含正在移出区域顶部的），按行号排列。
    pub lines: Vec<PlacedLine>,
    /// `lines[0]` 在这条轨整个行序列里的行号（锚点之前滚出去的行数）。
    pub first_line: usize,
    /// 区域顶/底（画的时候拿它裁剪，免得上浮中的行画到隔壁区域上）。
    pub region: (i32, i32),
}

/// 一帧的布局结果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FrameLayout {
    pub listen: Option<TrackLayout>,
    pub speak: Option<TrackLayout>,
    /// 这一帧需要的窗口高度（物理像素）。
    pub client_height: i32,
    /// 相邻视觉行相距多远（上浮动画以它为单位）。
    pub line_step: f32,
}

/// 一个视觉行在文字流里的区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub first: usize,
    pub last: usize,
    pub width: i32,
}

/// 按 UAX #14 填出"能不能在第 i 个单元之前断行"。
///
/// `text` 是整段文字，`unit_bytes[i]` 是第 `i` 个单元在 `text` 里的起始**字节**偏移（单调不减）。
/// 落在单元内部的断行机会（比如连写词中间）直接忽略——单元不可拆。这是 UAX #14 的字节位置
/// 换算成单元下标的**唯一**地方。结果写进 `out`（复用缓冲，渲染每帧都会调）。
pub fn fill_breaks(text: &str, unit_bytes: &[u32], out: &mut Vec<bool>) {
    out.clear();
    out.resize(unit_bytes.len(), false);
    let mut unit = 0usize;
    for (pos, _) in linebreaks(text) {
        if pos >= text.len() {
            break;
        }
        while unit < unit_bytes.len() && (unit_bytes[unit] as usize) < pos {
            unit += 1;
        }
        if unit < unit_bytes.len() && unit_bytes[unit] as usize == pos {
            out[unit] = true;
        }
    }
}

/// 一行里的簇按显示顺序排好：`order` 里是**行内单元下标**（0 = `first`），从左到右。
///
/// UAX #9 的 L1 + L2：行尾空白回到段落层级；然后从最高层级到最低奇数层级，
/// 逐层把连续的 ≥ 该层级的片段整体倒过来。`is_space(unit)` 说第 `unit` 个单元是不是空白。
/// `levels` 是调用方给的复用缓冲。
pub fn visual_order(
    text: &TrackText<'_>,
    first: usize,
    last: usize,
    is_space: impl Fn(usize) -> bool,
    order: &mut Vec<u32>,
    levels: &mut Vec<u8>,
) {
    order.clear();
    levels.clear();
    for unit in first..last {
        order.push((unit - first) as u32);
        levels.push(text.level_at(unit));
    }
    // L1：行尾的空白按段落层级排，免得它们被拽到行的另一头。
    let base = text.base_level();
    for unit in (first..last).rev() {
        if !is_space(unit) {
            break;
        }
        levels[unit - first] = base;
    }
    let Some(&highest) = levels.iter().max() else {
        return;
    };
    let Some(lowest_odd) = levels.iter().copied().filter(|l| l % 2 == 1).min() else {
        return;
    };
    // L2：从最高层往下，到最低的奇数层为止。
    for level in (lowest_odd..=highest).rev() {
        let mut i = 0usize;
        while i < order.len() {
            if levels[order[i] as usize] < level {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < order.len() && levels[order[j] as usize] >= level {
                j += 1;
            }
            order[i..j].reverse();
            i = j;
        }
    }
}

/// 这些标点不许出现在行首（禁则）。UAX #14 本身也禁止在它们之前断行，
/// 这里多判一层是为了**行尾悬挂**：宁可让标点伸出右侧内边距，也不把它推到下一行。
fn no_line_start(ch: char) -> bool {
    matches!(
        ch,
        '、' | '。'
            | '，'
            | '．'
            | '！'
            | '？'
            | '：'
            | '；'
            | '）'
            | '］'
            | '｝'
            | '〕'
            | '〉'
            | '》'
            | '」'
            | '』'
            | '】'
            | '”'
            | '’'
            | '·'
            | '…'
            | '‥'
            | '～'
            | '〜'
            | '،'
            | '؛'
            | '؟'
            | '۔'
            | '％'
            | '℃'
            | '\''
            | '"'
    )
}

/// 贪心折行：按 UAX #14 的断行机会填行。
///
/// - `avail` = 一行可用的文字宽度；
/// - `hang` = 允许标点伸出行宽多少像素（施工稿 C5：伸进行尾预留的悬挂区，
///   **绝不回头改已封口的行**）；
/// - 一个词比整行还宽时按字断。
///
/// 折行只由 `first` 之前的内容决定，所以后续追加的字不会回头改动已经封口的行。
pub fn wrap(text: &TrackText<'_>, avail: i32, hang: i32) -> Vec<Segment> {
    let n = text.len();
    let mut out = Vec::new();
    if n == 0 || avail <= 0 {
        return out;
    }
    let mut first = 0usize;
    loop {
        let mut i = first;
        let mut width = 0i32;
        // 最近一个"可以断在这里"的位置，连同断点处已经排到的宽度。
        let mut cut: Option<(usize, i32)> = None;
        while i < n {
            let advance = text.advances[i].max(0);
            if i > first && width + advance > avail {
                let hangs = no_line_start(text.chars[i]) && width + advance <= avail + hang;
                if !hangs {
                    break;
                }
            }
            width += advance;
            i += 1;
            if i < n && text.breaks.get(i).copied().unwrap_or(false) {
                cut = Some((i, width));
            }
        }
        if i >= n {
            out.push(Segment {
                first,
                last: n,
                width,
            });
            return out;
        }
        let (last, cut_width) = match cut {
            Some((c, w)) if c > first => (c, w),
            // 整行里一个断点都没有 = 一个词比整行还宽，按字断。
            _ => (i.max(first + 1), width),
        };
        out.push(Segment {
            first,
            last,
            width: cut_width,
        });
        first = last;
    }
}

/// 摆一帧：每条在跑的轨一个固定高度的区域，听在上、说在下。
///
/// `scroll_for` 拿到"这条轨折出来几行"，返回该用的滚动位移（像素）。滚动动画的
/// 状态由 `render::TrackScroll` 持有并在这里推进——折行和上浮在同一趟里算完，
/// 所以每帧只折一次行、也不额外分配。
///
/// 窗口高度 = `区域数 × 行数 × 行距`，**不随内容变**（施工稿 C4）：
/// 只随在跑轨数、字号、行数设置变化。
pub fn layout_regions<S>(
    listen: Option<TrackText<'_>>,
    speak: Option<TrackText<'_>>,
    mut scroll_for: S,
    client_width: i32,
    lines_per_track: u32,
    metrics: Metrics,
) -> FrameLayout
where
    S: FnMut(bool, usize) -> f32,
{
    let width = client_width.max(1);
    let inner_width = (width - 2 * metrics.padding).max(1);
    let lines_per_track = lines_per_track.max(1);

    let active = [listen, speak];
    let line_height = active
        .iter()
        .flatten()
        .map(|t| t.line_height.max(1))
        .max()
        .unwrap_or(1);
    let step = metrics.line_step(line_height);
    let plate_h = metrics.plate_height(line_height);
    // 行尾预留一个全角字宽（用行高当它的上界）给悬挂标点：标点伸进这块预留区，
    // 永远不会越出窗口，也不用回头改已封口的行。
    let hang = line_height;
    let avail = (inner_width - 2 * metrics.plate_pad_x - hang).max(1);

    let count = active
        .iter()
        .filter(|t| t.is_some_and(|t| !t.is_empty()))
        .count();
    // 空帧缩成 1px 全透明窗口；分层窗的 DIB 尺寸不能是 0。
    if count == 0 {
        return FrameLayout {
            client_height: 1,
            line_step: step,
            ..Default::default()
        };
    }
    let region_h = (lines_per_track as f32 * step - metrics.row_gap as f32).round() as i32;
    let client_height =
        (2 * metrics.padding + count as i32 * region_h + (count as i32 - 1) * metrics.row_gap)
            .max(1);

    let mut out = FrameLayout {
        client_height,
        line_step: step,
        ..Default::default()
    };
    let mut top = metrics.padding;
    for (slot, text) in active.iter().enumerate() {
        let Some(text) = text else { continue };
        if text.is_empty() {
            continue;
        }
        let segments = wrap(text, avail, hang);
        let scroll = scroll_for(slot == 0, text.first_line + segments.len());
        let bottom = top + region_h;
        let mut track = TrackLayout {
            first_line: text.first_line,
            region: (top, bottom),
            ..Default::default()
        };
        for (index, segment) in segments.iter().enumerate() {
            let y = top as f32 + (text.first_line + index) as f32 * step - scroll;
            // 正在移出区域顶部的那一行按被裁的比例淡出。
            let visible = (y + plate_h as f32 - top as f32).max(0.0) / plate_h as f32;
            // 从右往左的轨右对齐：所有行共用右边缘，新字长在左边；底衬贴着文字。
            let (plate_x, text_x, plate_w) = if text.rtl {
                let text_x = metrics.padding + inner_width - metrics.plate_pad_x - segment.width;
                (
                    text_x - metrics.plate_pad_x,
                    text_x,
                    segment.width + 2 * metrics.plate_pad_x,
                )
            } else {
                (
                    metrics.padding,
                    metrics.padding + metrics.plate_pad_x,
                    (segment.width + 2 * metrics.plate_pad_x)
                        .min(inner_width + metrics.plate_pad_x),
                )
            };
            track.lines.push(PlacedLine {
                y,
                plate_x,
                plate_w: plate_w.max(1),
                plate_h,
                text_x,
                baseline_y: y + metrics.plate_pad_y as f32 + text.ascent as f32,
                first: text.base + text.char_at(segment.first),
                last: text.base + text.char_at(segment.last),
                unit_first: segment.first,
                unit_last: segment.last,
                rtl: text.rtl,
                clip: visible.clamp(0.0, 1.0),
            });
        }
        match slot {
            0 => out.listen = Some(track),
            _ => out.speak = Some(track),
        }
        top = bottom + metrics.row_gap;
    }
    out
}

/// 首次运行的默认摆位：屏幕工作区底部居中，抬高 `bottom_margin` 让开任务栏。
///
/// `work` 用工作区（`rcWork`）而不是整块屏幕，这样天然避开任务栏。
pub fn default_placement(work: RectI, width: i32, height: i32, bottom_margin: i32) -> RectI {
    let width = width.min(work.w.max(1));
    let x = work.x + (work.w - width) / 2;
    let y = work.bottom() - height - bottom_margin;
    // 屏幕比窗口还矮时至少保证左上角在工作区里，别把窗口丢到看不见的地方。
    RectI::new(x.max(work.x), y.max(work.y), width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Metrics {
        Metrics::LOGICAL
    }

    /// 跑一遍 UAX #14，拿到等宽假度量（每字 30 px）下的 chars/advances/breaks。
    fn uax(text: &str) -> (Vec<char>, Vec<i32>, Vec<bool>) {
        let chars: Vec<char> = text.chars().collect();
        let advances: Vec<i32> = chars.iter().map(|_| 30).collect();
        let mut breaks = vec![false; chars.len()];
        let bytes: Vec<u32> = text.char_indices().map(|(i, _)| i as u32).collect();
        fill_breaks(text, &bytes, &mut breaks);
        (chars, advances, breaks)
    }

    fn track_of<'a>(chars: &'a [char], advances: &'a [i32], breaks: &'a [bool]) -> TrackText<'a> {
        TrackText {
            chars,
            advances,
            breaks,
            line_height: 40,
            ascent: 32,
            ..Default::default()
        }
    }

    #[test]
    fn english_breaks_between_words_not_inside_them() {
        let (chars, advances, breaks) = uax("hello world again");
        let segments = wrap(&track_of(&chars, &advances, &breaks), 300, 30);
        let lines: Vec<String> = segments
            .iter()
            .map(|s| chars[s.first..s.last].iter().collect())
            .collect();
        assert_eq!(lines, vec!["hello ", "world ", "again"]);
        assert!(
            segments.iter().all(|s| s.width <= 300),
            "每行都不许超宽：{segments:?}"
        );
    }

    #[test]
    fn a_word_wider_than_the_line_breaks_by_char() {
        // 一个 12 字的词，行只放得下 6 个字：必须按字断，不能整行消失。
        let (chars, advances, breaks) = uax("supercalifra");
        let segments = wrap(&track_of(&chars, &advances, &breaks), 180, 30);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].last, 6);
        assert_eq!(segments[1].first, 6);
        assert_eq!(segments[1].last, chars.len());
    }

    #[test]
    fn closing_punctuation_hangs_instead_of_starting_the_next_line() {
        // 一行正好排满 10 个字，第 11 个是"。"：它不许落到下一行行首。
        let mut s = String::new();
        for i in 0..10 {
            s.push(char::from(b'a' + i));
        }
        s.push('。');
        let (chars, advances, breaks) = uax(&s);
        let segments = wrap(&track_of(&chars, &advances, &breaks), 300, 30);
        assert_eq!(segments.len(), 1, "标点悬挂，行数不变");
        assert_eq!(segments[0].last, chars.len());
        assert!(
            segments[0].width <= 300 + 30,
            "只许伸进悬挂区那么多：{}",
            segments[0].width
        );
    }

    #[test]
    fn sealed_lines_never_move_when_more_text_arrives() {
        let mut s: String = std::iter::repeat_n('a', 10).collect();
        let (c0, a0, b0) = uax(&s);
        let first_pass = wrap(&track_of(&c0, &a0, &b0), 300, 30);
        s.push('b');
        let (c1, a1, b1) = uax(&s);
        let second_pass = wrap(&track_of(&c1, &a1, &b1), 300, 30);
        assert_eq!(
            first_pass[0], second_pass[0],
            "追加一个字不许改动已封口的行"
        );
    }

    #[test]
    fn tracks_get_equal_independent_regions() {
        let (chars, advances, breaks) = uax("0123456789012345");
        let listen = track_of(&chars, &advances, &breaks);
        let both = layout_regions(Some(listen), None, |_, _| 0.0, 800, 2, m());
        assert!(both.listen.is_some() && both.speak.is_none());
        let both = layout_regions(Some(listen), Some(listen), |_, _| 0.0, 800, 2, m());
        let l = both.listen.expect("听人区域");
        let s = both.speak.expect("对外区域");
        assert_eq!(l.region.0, m().padding, "听在上");
        assert!(s.region.0 >= l.region.1, "两个区域不许重叠");
        assert_eq!(
            s.region.1 - s.region.0,
            l.region.1 - l.region.0,
            "两个区域一样高"
        );
    }

    #[test]
    fn window_height_does_not_follow_content() {
        let (short_c, short_a, short_b) = uax("abc");
        let one = layout_regions(
            Some(track_of(&short_c, &short_a, &short_b)),
            None,
            |_, _| 0.0,
            800,
            3,
            m(),
        );
        let (long_c, long_a, long_b) = uax(&std::iter::repeat_n('a', 200).collect::<String>());
        let two = layout_regions(
            Some(track_of(&long_c, &long_a, &long_b)),
            None,
            |_, _| 0.0,
            800,
            3,
            m(),
        );
        assert_eq!(one.client_height, two.client_height);
        assert!(
            two.listen.expect("区域").lines.len() > 3,
            "内容长就该折出更多行"
        );
    }

    #[test]
    fn scroll_moves_every_line_by_the_same_amount() {
        let (chars, advances, breaks) = uax(&std::iter::repeat_n('a', 200).collect::<String>());
        let text = track_of(&chars, &advances, &breaks);
        let still = layout_regions(Some(text), None, |_, _| 0.0, 800, 2, m());
        let scrolled = layout_regions(Some(text), None, |_, _| 17.0, 800, 2, m());
        let a = &still.listen.expect("区域").lines;
        let b = &scrolled.listen.expect("区域").lines;
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!(
                (x.y - 17.0 - y.y).abs() < 1e-3,
                "整块上浮，行与行的相对位置不许变：{} vs {}",
                x.y,
                y.y
            );
        }
    }

    #[test]
    fn a_line_leaving_the_top_fades_out_proportionally() {
        let (chars, advances, breaks) = uax(&std::iter::repeat_n('a', 200).collect::<String>());
        let text = track_of(&chars, &advances, &breaks);
        let top = m().padding;
        let settled = layout_regions(
            Some(text),
            None,
            |_, total| (total - 2) as f32 * m().line_step(40),
            800,
            2,
            m(),
        )
        .listen
        .expect("区域");
        assert_eq!(
            settled.lines.iter().filter(|l| l.clip == 1.0).count(),
            2,
            "停稳之后区域里的两行满不透明"
        );
        assert!(
            settled.lines.iter().all(|l| l.clip == 1.0 || l.clip == 0.0),
            "停稳之后不该有半透明的行"
        );
        let half = layout_regions(
            Some(text),
            None,
            |_, total| (total - 2) as f32 * m().line_step(40) - 30.0,
            800,
            2,
            m(),
        )
        .listen
        .expect("区域");
        let leaving = half
            .lines
            .iter()
            .find(|l| l.clip > 0.0 && l.clip < 1.0)
            .expect("总有一行在往上走");
        assert!(
            leaving.clip > 0.0 && leaving.clip < 1.0,
            "被裁一半就该淡一半，实际 {}",
            leaving.clip
        );
        assert!(
            leaving.y < top as f32,
            "被裁的行一定在区域顶上：{}",
            leaving.y
        );
    }

    #[test]
    fn every_line_is_left_aligned() {
        let (chars, advances, breaks) = uax("The quick brown fox jumps over the lazy dog");
        let layout = layout_regions(
            Some(track_of(&chars, &advances, &breaks)),
            None,
            |_, _| 0.0,
            800,
            4,
            m(),
        );
        for line in &layout.listen.expect("区域").lines {
            assert_eq!(line.text_x, m().padding + m().plate_pad_x);
            assert_eq!(line.plate_x, m().padding);
        }
    }

    #[test]
    fn fill_breaks_maps_uax_positions_to_char_indices() {
        let chars: Vec<char> = "hello world".chars().collect();
        let bytes: Vec<u32> = (0..chars.len() as u32).collect();
        let mut breaks = Vec::new();
        fill_breaks("hello world", &bytes, &mut breaks);
        assert_eq!(breaks.len(), chars.len());
        assert!(!breaks[0], "行首不能是断点");
        assert!(breaks[6], "hello 之后可以断");
        assert!(!breaks[5], "单词内部不能断");
    }

    #[test]
    fn cjk_breaks_between_han_characters() {
        let s = "字幕是给眼睛看的";
        let (chars, advances, breaks) = uax(s);
        let segments = wrap(&track_of(&chars, &advances, &breaks), 180, 30);
        assert!(segments.len() > 1);
        for seg in &segments {
            assert!(
                !no_line_start(chars[seg.first]),
                "第 {} 行不许以 {:?} 开头",
                seg.first,
                chars[seg.first]
            );
        }
    }

    #[test]
    fn empty_frame_collapses_to_one_pixel() {
        let layout = layout_regions(None, None, |_, _| 0.0, 800, 2, m());
        assert_eq!(layout.client_height, 1);
        assert!(layout.listen.is_none() && layout.speak.is_none());
    }

    #[test]
    fn non_positive_width_does_not_panic() {
        let (chars, advances, breaks) = uax("abc");
        let layout = layout_regions(
            Some(track_of(&chars, &advances, &breaks)),
            None,
            |_, _| 0.0,
            0,
            2,
            m(),
        );
        assert!(layout.client_height > 0);
        assert!(
            !layout.listen.expect("区域").lines.is_empty(),
            "再窄也得把字排出来"
        );
    }

    #[test]
    fn metrics_scale_with_dpi_and_reject_nonsense() {
        let at96 = Metrics::for_dpi(96);
        assert_eq!(at96, Metrics::LOGICAL);
        let at144 = Metrics::for_dpi(144); // 150%
        assert_eq!(at144.padding, 27);
        assert_eq!(Metrics::for_dpi(0), Metrics::LOGICAL, "0 DPI 退回 96");
        assert_eq!(
            Metrics::for_dpi(99_999),
            Metrics::LOGICAL,
            "离谱 DPI 退回 96"
        );
        let tiny = Metrics::for_dpi(72);
        assert!(tiny.row_gap >= 1 && tiny.plate_radius >= 1);
    }

    #[test]
    fn default_placement_centers_near_the_bottom() {
        let work = RectI::new(0, 0, 1920, 1040); // 1080 减掉任务栏
        let r = default_placement(work, 880, 170, 80);
        assert_eq!(r.w, 880);
        assert_eq!(r.center_x(), work.center_x(), "水平居中");
        assert_eq!(r.bottom(), 1040 - 80, "底部留出 80 给任务栏");
        assert!(r.y > work.y);
    }

    #[test]
    fn default_placement_honors_monitor_offset() {
        let work = RectI::new(-1920, -200, 1600, 900);
        let r = default_placement(work, 880, 170, 80);
        assert_eq!(r.center_x(), work.center_x());
        assert!(r.x >= work.x && r.right() <= work.right());
        assert_eq!(r.bottom(), work.bottom() - 80);
    }

    /// 给一行造一组带层级的单元，返回显示顺序（行内单元下标）。
    fn ordered(levels: &[u8], rtl: bool, space: &[usize]) -> Vec<u32> {
        let chars: Vec<char> = levels.iter().map(|_| 'x').collect();
        let advances: Vec<i32> = levels.iter().map(|_| 10).collect();
        let text = TrackText {
            chars: &chars,
            advances: &advances,
            levels,
            rtl,
            ..Default::default()
        };
        let (mut order, mut scratch) = (Vec::new(), Vec::new());
        visual_order(
            &text,
            0,
            levels.len(),
            |unit| space.contains(&unit),
            &mut order,
            &mut scratch,
        );
        order
    }

    #[test]
    fn visual_order_reverses_a_pure_rtl_line() {
        assert_eq!(ordered(&[1, 1, 1, 1], true, &[]), vec![3, 2, 1, 0]);
    }

    #[test]
    fn visual_order_keeps_an_ltr_line_as_is() {
        assert_eq!(ordered(&[0, 0, 0], false, &[]), vec![0, 1, 2]);
    }

    #[test]
    fn visual_order_reverses_only_the_embedded_rtl_run() {
        // 从左往右的句子里嵌了一段从右往左的文字（单元 2..5）。
        assert_eq!(
            ordered(&[0, 0, 1, 1, 1, 0], false, &[]),
            vec![0, 1, 4, 3, 2, 5]
        );
    }

    #[test]
    fn visual_order_keeps_an_ltr_run_inside_an_rtl_line_readable() {
        // 从右往左的句子里嵌了一段数字/拉丁（层级 2，单元 2..4）：
        // 先把层级 2 的片段倒一次、再把整行倒一次，结果片段自己仍是从左往右读。
        assert_eq!(ordered(&[1, 1, 2, 2, 1], true, &[]), vec![4, 2, 3, 1, 0]);
    }

    #[test]
    fn trailing_whitespace_goes_back_to_the_paragraph_level() {
        // 行尾的空格（单元 2）不跟着前面的从右往左片段走：它留在行尾（右边）。
        assert_eq!(ordered(&[1, 1, 1], false, &[2]), vec![1, 0, 2]);
    }

    #[test]
    fn rtl_lines_share_the_right_edge_and_hug_their_text() {
        let chars: Vec<char> = "abcdefghij klmn".chars().collect();
        let advances: Vec<i32> = chars.iter().map(|_| 30).collect();
        let mut breaks = vec![false; chars.len()];
        let bytes: Vec<u32> = (0..chars.len() as u32).collect();
        let text_str: String = chars.iter().collect();
        fill_breaks(&text_str, &bytes, &mut breaks);
        let text = TrackText {
            chars: &chars,
            advances: &advances,
            breaks: &breaks,
            rtl: true,
            line_height: 40,
            ascent: 32,
            ..Default::default()
        };
        let layout = layout_regions(Some(text), None, |_, _| 0.0, 300, 4, m());
        let lines = layout.listen.expect("区域").lines;
        assert!(lines.len() >= 2, "300px 宽放不下，要折行");
        let right = lines[0].plate_x + lines[0].plate_w;
        for line in &lines {
            assert!(line.rtl);
            assert_eq!(line.plate_x + line.plate_w, right, "所有行共用右边缘");
            assert_eq!(
                line.text_x + (line.plate_w - 2 * m().plate_pad_x),
                right - m().plate_pad_x,
                "文字贴着底衬右内边距"
            );
        }
        assert!(
            lines[0].plate_w != lines[1].plate_w,
            "底衬贴合各行文字宽度（不是统一宽度）"
        );
    }

    #[test]
    fn breaks_inside_a_cluster_are_ignored() {
        // "ab cd"：UAX #14 在 "cd" 前（字节 3）有断点；第二个单元从字节 4 起，
        // 断点落在第一个单元内部，必须忽略，不能挪到别的单元上。
        let mut breaks = Vec::new();
        fill_breaks("ab cd", &[0, 4], &mut breaks);
        assert_eq!(breaks, vec![false, false]);
        fill_breaks("ab cd", &[0, 3], &mut breaks);
        assert_eq!(breaks, vec![false, true]);
    }

    #[test]
    fn units_map_back_to_frame_chars_when_a_cluster_spans_several_chars() {
        // 三个单元覆盖 2 + 1 + 3 个字符；折行后的字符区间要按单元边界换算。
        let chars = ['a', 'b', 'c'];
        let advances = [100, 100, 100];
        let breaks = [false, true, true];
        let unit_chars = [0u32, 2, 3, 6];
        let text = TrackText {
            chars: &chars,
            advances: &advances,
            breaks: &breaks,
            unit_chars: &unit_chars,
            line_height: 40,
            ascent: 32,
            base: 10,
            ..Default::default()
        };
        let layout = layout_regions(Some(text), None, |_, _| 0.0, 250, 4, m());
        let lines = layout.listen.expect("区域").lines;
        let spans: Vec<(usize, usize)> = lines.iter().map(|l| (l.first, l.last)).collect();
        assert!(
            spans.iter().all(|(f, l)| *f >= 10 && *l <= 16 && f < l),
            "字符区间落在 [base, base+6)：{spans:?}"
        );
        assert_eq!(spans.first().map(|s| s.0), Some(10));
        assert_eq!(spans.last().map(|s| s.1), Some(16));
        for pair in spans.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "相邻两行的字符区间首尾相接");
        }
    }

    #[test]
    fn default_placement_clamps_on_tiny_screens() {
        let work = RectI::new(0, 0, 400, 120);
        let r = default_placement(work, 880, 170, 80);
        assert_eq!(r.w, 400, "比屏幕宽就压到屏幕宽");
        assert_eq!(r.x, work.x);
        assert!(r.y == work.y, "屏幕太矮时至少顶到工作区上边");
    }
}
