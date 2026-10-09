//! 字形光栅器：`fontdb` 找字体、`swash` 出覆盖率掩码。
//!
//! 为什么不用 fontconfig + FreeType：那两个要系统 `-dev` 包，而 `fontdb` + `swash`
//! 是纯 Rust——装不装开发包都能编，跨发行版也少一个变量（跟选 `nnnoiseless` 而不是
//! DeepFilterNet 是同一个思路）。
//!
//! 输出语义跟 Windows 的 GDI 那版**逐字段对齐**（掩码左上角相对"起笔点 + 基线"的
//! 偏移、步进宽度、0..255 覆盖率、101.5% 字距），所以 `vox-overlay-core` 里的布局
//! 与合成代码一行都不用改。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use swash::scale::{Render, ScaleContext, Source};
use swash::{FontRef, GlyphId};
use vox_core::ports::{PortError, PortResult};
use vox_overlay_core::text::{FontMetrics, Glyph, GlyphSource};

/// 与 Windows 侧一致的字距：旧版给了 101.5% 的字间距，折算成百分比整数。
const TRACKING_PERCENT: i32 = 1015;
/// 字形掩码不是无限缓存：长时间跑多语种字幕时，不同字符可能持续增长。
const MAX_CACHED_GLYPHS: usize = 2048;

/// 字体数据缓存：`fontdb` 取出来的字节要自己持有（`FontRef` 借它），
/// 而 CJK 字体动辄十几 MB，同一族反复建光栅器时不该重复拷。
/// 族名 → （字体字节，face index）。
type FontCache = Mutex<HashMap<String, (Arc<Vec<u8>>, u32)>>;

static FONT_DATA: OnceLock<FontCache> = OnceLock::new();

/// swash 光栅器。
///
/// 只能在创建它的线程上用（`ScaleContext` 与字形缓存都不是线程安全的），
/// 悬浮窗本来就规定所有绘制在窗口线程上做。
pub struct SwashFont {
    data: Arc<Vec<u8>>,
    index: u32,
    size_px: f32,
    metrics: FontMetrics,
    cache: HashMap<char, Glyph>,
    /// 插入序，用来在缓存满时淘汰最老的（跟 Windows 侧的 clock 等价，够用）。
    order: Vec<char>,
    scaler_ctx: ScaleContext,
}

impl SwashFont {
    /// 建一个字体。`size_pt` 是设置里的字号，`dpi` 是窗口所在显示器的 DPI。
    ///
    /// 字号按 DPI 换算：`px = pt * dpi / 72`（跟 Windows 侧同一个公式）。
    pub fn new(family: &str, size_pt: u32, dpi: u32) -> PortResult<Self> {
        let dpi = if (72..=1200).contains(&dpi) { dpi } else { 96 };
        let size_px = ((size_pt.max(1) as f32 * dpi as f32) / 72.0).max(1.0);

        let (data, index) = load_face(family)?;
        let font = FontRef::from_index(&data, index as usize)
            .ok_or_else(|| PortError::new(format!("字体「{family}」解析失败")))?;
        let metrics = font.metrics(&[]).scale(size_px);
        // swash 的 descent 是正数（基线以下的距离），行高必须是 ascent + descent + leading。
        let line_height = (metrics.ascent + metrics.descent + metrics.leading).round() as i32;
        let ascent = metrics.ascent.round() as i32;

        Ok(Self {
            data,
            index,
            size_px,
            metrics: FontMetrics {
                line_height: line_height.max(1),
                ascent,
            },
            cache: HashMap::new(),
            order: Vec::new(),
            scaler_ctx: ScaleContext::new(),
        })
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }

    fn glyph_id(&self, ch: char) -> Option<GlyphId> {
        let font = self.font()?;
        // `GlyphId` 就是个 u16 别名；0 表示字体里没这个字。
        let id = font.charmap().map(ch);
        (id != 0).then_some(id)
    }

    /// 光栅化一个字，写进缓存。
    fn rasterize(&mut self, ch: char) -> Option<()> {
        // `FontRef` 借用字体字节，而下面 `self.scaler_ctx` 要可变借用 self——
        // 所以先把 `Arc` 克隆到局部，让借用落在局部上，两边不打架。
        let data = Arc::clone(&self.data);
        let font = FontRef::from_index(&data, self.index as usize)?;
        let id = self.glyph_id(ch)?;

        let advance_raw = font
            .glyph_metrics(&[])
            .scale(self.size_px)
            .advance_width(id);
        let advance = ((advance_raw * TRACKING_PERCENT as f32) / 1000.0).round() as i32;

        let mut scaler = self
            .scaler_ctx
            .builder(font)
            .size(self.size_px)
            .hint(true)
            .build();
        // 只要矢量轮廓 → `Render` 默认就是 `Format::Alpha`，`data` 即覆盖率。
        // **不**请求彩色轮廓/彩色位图：emoji 那种彩色字形压不进单一 alpha 通道，
        // 硬画会出彩边——跟 Windows 侧不用 ClearType 是同一个理由。
        let image = Render::new(&[Source::Outline]).render(&mut scaler, id)?;
        let placement = image.placement;
        if placement.width == 0 || placement.height == 0 {
            // 空格之类没有墨迹：仍然要有步进，但没有掩码。
            self.insert(
                ch,
                Glyph {
                    w: 0,
                    h: 0,
                    off_x: 0,
                    off_y: 0,
                    advance,
                    cov: Vec::new(),
                },
            );
            return Some(());
        }
        self.insert(
            ch,
            Glyph {
                w: placement.width as i32,
                h: placement.height as i32,
                off_x: placement.left,
                // swash 的 `top` 是"相对基线的上边界"（向上为正），
                // 我们要的是"掩码左上角相对基线的 y 偏移"（向下为正）。
                off_y: -placement.top,
                advance,
                cov: image.data,
            },
        );
        Some(())
    }

    fn insert(&mut self, ch: char, glyph: Glyph) {
        if self.cache.len() >= MAX_CACHED_GLYPHS {
            if let Some(oldest) = self.order.first().copied() {
                self.order.remove(0);
                self.cache.remove(&oldest);
            }
        }
        self.cache.insert(ch, glyph);
        self.order.push(ch);
    }
}

impl GlyphSource for SwashFont {
    fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    fn advance(&mut self, ch: char) -> i32 {
        match self.glyph(ch) {
            Some(glyph) => glyph.advance,
            None => 0,
        }
    }

    fn glyph(&mut self, ch: char) -> Option<&Glyph> {
        // 控制字符统一当空格，免得跑出诡异的字形或者负宽度（跟 Windows 侧一致）。
        let ch = if ch.is_control() { ' ' } else { ch };
        if !self.cache.contains_key(&ch) {
            self.rasterize(ch)?;
        }
        self.cache.get(&ch)
    }
}

/// 平台光栅器工厂：渲染器只认 trait，这里给它 swash 那版。
pub fn font_factory() -> vox_overlay_core::text::FontFactory {
    vox_overlay_core::text::font_factory(|family, size, dpi| {
        Ok(Box::new(SwashFont::new(family, size, dpi)?))
    })
}

/// 在系统字体里找一个能用的字体面。
///
/// 优先按设置里的字体族名找；找不到就按"常见 CJK 字体 → 无衬线"的顺序退。
/// 一个字都没有就报错——字幕画不出来这件事必须让用户看见。
fn load_face(family: &str) -> PortResult<(Arc<Vec<u8>>, u32)> {
    let cache = FONT_DATA.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(family).cloned()) {
        return Ok(hit);
    }

    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let id = query_font(&db, family).ok_or_else(|| {
        PortError::new(format!(
            "系统里找不到字体「{family}」，也没找到可用的中文回退字体"
        ))
    })?;
    let loaded = db
        .with_face_data(id, |data, index| (Arc::new(data.to_vec()), index))
        .ok_or_else(|| PortError::new(format!("读取字体「{family}」失败")))?;

    if let Ok(mut guard) = cache.lock() {
        guard.insert(family.to_string(), loaded.clone());
    }
    Ok(loaded)
}

/// 设置里的字体族 → 系统里真实存在的字体面。
fn query_font(db: &fontdb::Database, family: &str) -> Option<fontdb::ID> {
    let by_name = |name: &str| {
        db.query(&fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            ..Default::default()
        })
    };
    if !family.trim().is_empty() {
        if let Some(id) = by_name(family) {
            return Some(id);
        }
    }
    // 回退顺序：先找能写中文的，再退到系统无衬线。
    for fallback in [
        "Noto Sans CJK SC",
        "Noto Sans CJK JP",
        "Source Han Sans SC",
        "WenQuanYi Micro Hei",
        "Droid Sans Fallback",
    ] {
        if let Some(id) = by_name(fallback) {
            return Some(id);
        }
    }
    db.query(&fontdb::Query {
        families: &[fontdb::Family::SansSerif],
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_font_and_reports_sane_metrics() {
        let font = SwashFont::new("", 32, 96).expect("系统里总该有个无衬线字体");
        let metrics = font.metrics();
        assert!(metrics.line_height > 0, "行高不该是 0：{metrics:?}");
        assert!(metrics.ascent > 0, "ascent 不该是 0：{metrics:?}");
    }

    #[test]
    fn ascii_glyph_has_ink_and_advance() {
        let mut font = SwashFont::new("", 32, 96).unwrap();
        let glyph = font.glyph('A').expect("字母 A 该有字形");
        assert!(glyph.advance > 0, "步进宽度不该是 0");
        assert!(glyph.w > 0 && glyph.h > 0, "掩码尺寸不该是 0");
        assert_eq!(
            glyph.cov.len(),
            (glyph.w * glyph.h) as usize,
            "覆盖率长度要等于 w*h"
        );
        assert!(glyph.cov.iter().any(|&v| v > 0), "字母 A 该有墨迹");
    }

    #[test]
    fn cjk_glyph_is_not_empty() {
        // 字幕主要是中日韩：回退字体挑错了，这里会直接暴露（拿不到字形或掩码全 0）。
        let mut font = SwashFont::new("", 32, 96).unwrap();
        let glyph = font.glyph('字').expect("汉字该有字形");
        assert!(glyph.w > 0 && glyph.h > 0, "汉字掩码不该是 0：{glyph:?}");
        assert!(glyph.cov.iter().any(|&v| v > 0), "汉字该有墨迹");
    }

    #[test]
    fn advance_scales_with_dpi() {
        let mut small = SwashFont::new("", 32, 96).unwrap();
        let mut large = SwashFont::new("", 32, 192).unwrap();
        assert!(
            large.advance('M') > small.advance('M'),
            "DPI 翻倍，字宽该跟着变大"
        );
    }

    #[test]
    fn control_chars_fall_back_to_space() {
        let mut font = SwashFont::new("", 32, 96).unwrap();
        let space = font.glyph(' ').map(|g| g.advance).unwrap_or(0);
        let tab = font.glyph('\t').map(|g| g.advance).unwrap_or(0);
        assert_eq!(tab, space, "控制字符应当按空格量宽度");
    }
}
