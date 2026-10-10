//! 字形数据与光栅器接口（平台中立）。
//!
//! 只有数据形状和 trait 在这儿；**怎么把字变成覆盖率蒙版**是两个平台各自的事：
//! Windows 用 GDI（`vox-overlay-win/src/font.rs`），Linux 用 `swash` + `fontdb`。
//!
//! 排版单元是**字形簇**（`Cluster`），不是 `char`：一个簇可以是阿拉伯语连写的一个词形、
//! 印地语的一个音节、泰语的"字母 + 附标"，也可以只是一个拉丁字母。整形由
//! [`crate::shape::FaceSet`]（rustybuzz）在 core 里做，两个平台只负责"按字形 id 出掩码"。
//! `Glyph` 的字段含义（掩码左上角相对"起笔点 + 基线"的偏移、覆盖率）两个平台一致。

use vox_core::ports::PortResult;

/// 一个字形栅格化出来的结果（按**字形 id** 取，不是按 char）。
#[derive(Debug, Clone)]
pub struct Glyph {
    /// 掩码宽。
    pub w: i32,
    /// 掩码高。
    pub h: i32,
    /// 掩码左上角相对"起笔点 + 基线"的偏移。
    pub off_x: i32,
    pub off_y: i32,
    /// `w * h` 个覆盖率。
    pub cov: Vec<u8>,
}

/// 字体度量。
#[derive(Debug, Clone, Copy, Default)]
pub struct FontMetrics {
    pub line_height: i32,
    pub ascent: i32,
}

/// 整形出来的一个字形：画哪个字体的哪个字形、相对起笔点挪多少、画完起笔点往前走多少。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// 字体面槽位：0 是主字体，其后是回退字体（由 [`crate::shape::FaceSet`] 编号）。
    pub face: u16,
    pub id: u16,
    /// 画之前先挪的量（像素，x 向右为正；附标定位靠它）。
    pub x_offset: i32,
    /// y 向**上**为正（跟 OpenType 一致），画的时候取反。
    pub y_offset: i32,
    pub advance: i32,
}

/// 一个排版单元（字形簇）：折行与对齐的最小单位，不会被拆开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    /// 这个簇在整段文字里的起始**字节**偏移。簇覆盖 `[start, 下一个簇的 start)`。
    pub start: u32,
    /// 簇内所有字形的前进宽度之和。
    pub advance: i32,
    /// 簇的字形在 [`Shaped::glyphs`] 里的区间 `[glyph_start, glyph_end)`，按**从左到右**的画序。
    pub glyph_start: u32,
    pub glyph_end: u32,
}

/// 一段文字整形后的结果。`clusters` 永远按**逻辑顺序**（文字输入的顺序）排列，
/// 哪怕这是一段从右往左的文字——视觉重排是布局的事（UAX #9 的 L2，逐视觉行做）。
#[derive(Debug, Clone, Default)]
pub struct Shaped {
    pub clusters: Vec<Cluster>,
    pub glyphs: Vec<ShapedGlyph>,
}

impl Shaped {
    pub fn clear(&mut self) {
        self.clusters.clear();
        self.glyphs.clear();
    }
}

/// 光栅器：整形、取字形掩码。**只能在创建它的那个线程上用**（GDI 的 DC、
/// swash 的 cache 都不是线程安全的），悬浮窗本来就规定所有绘制在窗口线程上做。
pub trait GlyphSource {
    fn metrics(&self) -> FontMetrics;
    /// 整形一段**同方向**的文字（`rtl` = 这段按从右往左排）。把结果簇**按逻辑顺序**
    /// 追加到 `out`；簇的 `start` 是 `base` + 该簇在 `run` 里的字节偏移。
    fn shape(&mut self, run: &str, rtl: bool, base: u32, out: &mut Shaped);
    /// 取字形掩码；拿不到返回 `None`（调用方跳过这个字形，不 panic）。
    fn glyph(&mut self, face: u16, id: u16) -> Option<&Glyph>;
}

/// 建字体的工厂。设置里的字体族/字号/DPI 一变就要重建光栅器（字形缓存跟着换）。
///
/// 是个 `Box<dyn Fn>` 而不是直接存光栅器：`Renderer::sync_font` 要能重建它，
/// 而重建的方式是平台相关的（Windows 换 HFONT、Linux 换 swash 的 FontRef）。
pub type FontFactory = Box<dyn Fn(&str, u32, u32) -> PortResult<Box<dyn GlyphSource>> + Send>;

/// 把平台光栅器包成工厂的小工具，省得每个调用点都写一遍闭包。
pub fn font_factory<F>(build: F) -> FontFactory
where
    F: Fn(&str, u32, u32) -> PortResult<Box<dyn GlyphSource>> + Send + 'static,
{
    Box::new(build)
}
