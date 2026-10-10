//! 字形光栅器：`fontdb` 找字体、core 的 `FaceSet`（rustybuzz）整形、`swash` 按字形 id 出覆盖率掩码。
//!
//! 为什么不用 fontconfig + FreeType：那两个要系统 `-dev` 包，而 `fontdb` + `swash`
//! 是纯 Rust——装不装开发包都能编，跨发行版也少一个变量（跟选 `nnnoiseless` 而不是
//! DeepFilterNet 是同一个思路）。
//!
//! 输出语义跟 Windows 的 GDI 那版**逐字段对齐**（掩码左上角相对"起笔点 + 基线"的
//! 偏移、0..255 覆盖率；步进宽度来自整形），所以 `vox-overlay-core` 里的布局
//! 与合成代码两个平台共用。
//!
//! 缺字时的字体回退走 fontconfig（`fc-list`），见 [`fallback_face`]。

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use swash::scale::{Render, ScaleContext, Source};
use swash::FontRef;
use vox_core::ports::{PortError, PortResult};
use vox_overlay_core::shape::{FaceData, FaceSet};
use vox_overlay_core::text::{FontMetrics, Glyph, GlyphSource, Shaped};

/// 字形掩码不是无限缓存：长时间跑多语种字幕时，不同字形可能持续增长。
const MAX_CACHED_GLYPHS: usize = 4096;

/// 字体数据缓存：`fontdb` 取出来的字节要自己持有（`FontRef` 借它），
/// 而 CJK 字体动辄十几 MB，同一族反复建光栅器时不该重复拷。
/// 族名 → （字体字节，face index）。
type FontCache = Mutex<HashMap<String, (Arc<Vec<u8>>, u32)>>;

static FONT_DATA: OnceLock<FontCache> = OnceLock::new();
/// 回退字体文件缓存：（路径，face index）→ 字体面。进程内只读一次。
static FALLBACK_DATA: OnceLock<Mutex<HashMap<(String, u32), FaceData>>> = OnceLock::new();

/// swash 光栅器：整形走 core 的 [`FaceSet`]（rustybuzz），这里只按字形 id 出覆盖率掩码。
///
/// 只能在创建它的线程上用（`ScaleContext` 与字形缓存都不是线程安全的），
/// 悬浮窗本来就规定所有绘制在窗口线程上做。
pub struct SwashFont {
    faces: FaceSet,
    size_px: f32,
    metrics: FontMetrics,
    /// （字体槽位，字形 id）→ 掩码。
    cache: HashMap<(u16, u16), Glyph>,
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
        let faces = FaceSet::new(FaceData {
            bytes: data.clone(),
            index,
        })
        .ok_or_else(|| PortError::new(format!("字体「{family}」无法整形")))?;

        Ok(Self {
            faces,
            size_px,
            metrics: FontMetrics {
                line_height: line_height.max(1),
                ascent,
            },
            cache: HashMap::new(),
            scaler_ctx: ScaleContext::new(),
        })
    }

    /// 光栅化一个字形。拿不到轮廓的（空格、字体坏了）给一块零面积掩码，免得每帧重试。
    fn rasterize(&mut self, face: u16, id: u16) -> Glyph {
        let empty = Glyph {
            w: 0,
            h: 0,
            off_x: 0,
            off_y: 0,
            cov: Vec::new(),
        };
        let Some(data) = self.faces.face(face).cloned() else {
            return empty;
        };
        // `FontRef` 借用字体字节，而下面 `self.scaler_ctx` 要可变借用 self——
        // 所以字节的 `Arc` 克隆在局部，让借用落在局部上，两边不打架。
        let Some(font) = FontRef::from_index(&data.bytes, data.index as usize) else {
            return empty;
        };
        let mut scaler = self
            .scaler_ctx
            .builder(font)
            .size(self.size_px)
            .hint(true)
            .build();
        // 只要矢量轮廓 → `Render` 默认就是 `Format::Alpha`，`data` 即覆盖率。
        // **不**请求彩色轮廓/彩色位图：emoji 那种彩色字形压不进单一 alpha 通道，
        // 硬画会出彩边——跟 Windows 侧不用 ClearType 是同一个理由。
        let Some(image) = Render::new(&[Source::Outline]).render(&mut scaler, id) else {
            return empty;
        };
        let placement = image.placement;
        if placement.width == 0 || placement.height == 0 {
            return empty;
        }
        Glyph {
            w: placement.width as i32,
            h: placement.height as i32,
            off_x: placement.left,
            // swash 的 `top` 是"相对基线的上边界"（向上为正），
            // 我们要的是"掩码左上角相对基线的 y 偏移"（向下为正）。
            off_y: -placement.top,
            cov: image.data,
        }
    }
}

impl GlyphSource for SwashFont {
    fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    fn shape(&mut self, run: &str, rtl: bool, base: u32, out: &mut Shaped) {
        self.faces
            .shape(run, rtl, base, self.size_px, &mut fallback_face, out);
    }

    fn glyph(&mut self, face: u16, id: u16) -> Option<&Glyph> {
        if !self.cache.contains_key(&(face, id)) {
            if self.cache.len() >= MAX_CACHED_GLYPHS {
                self.cache.clear();
            }
            let glyph = self.rasterize(face, id);
            self.cache.insert((face, id), glyph);
        }
        self.cache.get(&(face, id))
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

/// 系统里找一个覆盖 `ch` 的字体面（S5 字体回退）。找不到返回 `None`——调用方画豆腐块。
///
/// 问 fontconfig（`fc-list`）：列出所有覆盖该字符的字体面，按"无衬线、常规字重、
/// 非等宽"的偏好挑一个——`fc-match` 的最佳匹配可能是衬线体，字幕上很突兀。
/// 没装 fontconfig 命令行工具（或它出错）就是没有回退，不 panic。
fn fallback_face(ch: char) -> Option<FaceData> {
    let output = Command::new("fc-list")
        .args(["-f", "%{file}\t%{index}\t%{family}\t%{style}\n"])
        .arg(format!(":charset={:x}", ch as u32))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let (file, index) = pick_fallback(&listing)?;
    let cache = FALLBACK_DATA.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (file, index);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    let bytes = std::fs::read(&key.0).ok()?;
    let face = FaceData {
        bytes: Arc::new(bytes),
        index: key.1,
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, face.clone());
    }
    Some(face)
}

/// 从 `fc-list` 的输出（`文件\t索引\t族名\t样式`，一行一个面）里挑一个：
/// 常规字重、无衬线、非等宽、Noto 优先。
fn pick_fallback(listing: &str) -> Option<(String, u32)> {
    let mut best: Option<(i32, String, u32)> = None;
    for line in listing.lines() {
        let mut cols = line.split('\t');
        let (Some(file), Some(index), Some(family), Some(style)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let Ok(index) = index.trim().parse::<u32>() else {
            continue;
        };
        let (family, style) = (family.to_lowercase(), style.to_lowercase());
        let mut score = 0;
        if style
            .split(',')
            .any(|s| matches!(s.trim(), "regular" | "book" | "normal"))
        {
            score += 10;
        }
        if family.contains("mono") || family.contains("symbols") {
            score -= 6;
        }
        if family.contains("serif") && !family.contains("sans") {
            score -= 2;
        }
        if family.contains("noto sans") {
            score += 5;
        } else if family.contains("noto") {
            score += 2;
        } else if family.contains("dejavu sans") {
            score += 1;
        }
        // 偏好带 "ui" / "display" 的变体会让行高变形，不加分也不扣分；
        // 同分取先出现的，结果在同一台机器上稳定。
        if best.as_ref().is_none_or(|(top, _, _)| score > *top) {
            best = Some((score, file.to_string(), index));
        }
    }
    best.map(|(_, file, index)| (file, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 整形 `text`，返回结果。
    fn shaped(font: &mut SwashFont, text: &str) -> Shaped {
        let mut out = Shaped::default();
        font.shape(text, false, 0, &mut out);
        out
    }

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
        let run = shaped(&mut font, "A");
        assert_eq!(run.clusters.len(), 1);
        assert!(run.clusters[0].advance > 0, "步进宽度不该是 0");
        let g = run.glyphs[0];
        let glyph = font.glyph(g.face, g.id).expect("字母 A 该有字形");
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
        let run = shaped(&mut font, "字");
        let g = run.glyphs[0];
        let glyph = font.glyph(g.face, g.id).expect("汉字该有字形");
        assert!(glyph.w > 0 && glyph.h > 0, "汉字掩码不该是 0：{glyph:?}");
        assert!(glyph.cov.iter().any(|&v| v > 0), "汉字该有墨迹");
    }

    #[test]
    fn advance_scales_with_dpi() {
        let mut small = SwashFont::new("", 32, 96).unwrap();
        let mut large = SwashFont::new("", 32, 192).unwrap();
        assert!(
            shaped(&mut large, "M").clusters[0].advance
                > shaped(&mut small, "M").clusters[0].advance,
            "DPI 翻倍，字宽该跟着变大"
        );
    }

    #[test]
    fn control_chars_fall_back_to_space() {
        // 控制字符在排版前（`TrackScratch`）就换成空格；这里验证整条路径：
        // 带制表符的一行与带空格的一行排出同样的宽度。
        use vox_core::ports::{SubtitleFrame, SubtitleLine};
        use vox_core::settings::SubtitleSettings;
        use vox_core::subtitle::{RenderedChar, Track};
        use vox_overlay_core::render::{FrameInput, Renderer};
        let settings = SubtitleSettings::default();
        let mut renderer = Renderer::new(&settings, 96, font_factory()).unwrap();
        let mut widths = Vec::new();
        for text in ["a b", "a\tb"] {
            let frame = SubtitleFrame {
                lines: vec![SubtitleLine {
                    track: Track::Listen,
                    origin: 0,
                    chars: text
                        .chars()
                        .map(|ch| RenderedChar { ch, alpha: 1.0 })
                        .collect(),
                    color: "#ffffff".into(),
                }],
            };
            let layout = renderer.plan(&FrameInput {
                frame: &frame,
                settings: &settings,
                client_width: 900,
                client_height: None,
                dpi: 96,
                now: std::time::Instant::now(),
            });
            widths.push(layout.listen.unwrap().lines[0].plate_w);
        }
        assert_eq!(widths[0], widths[1], "控制字符应当按空格量宽度");
    }

    #[test]
    fn fallback_picks_regular_sans_over_mono_and_bold() {
        let listing = "/f/NotoSansMono-Regular.ttf\t0\tNoto Sans Mono\tRegular\n\
                       /f/NotoSansArabic-Bold.ttf\t0\tNoto Sans Arabic\tBold\n\
                       /f/NotoSansArabic-Regular.ttf\t0\tNoto Sans Arabic\tRegular\n\
                       /f/FreeSerif.ttf\t0\tFreeSerif\tMedium\n";
        assert_eq!(
            pick_fallback(listing),
            Some(("/f/NotoSansArabic-Regular.ttf".to_string(), 0))
        );
        assert_eq!(pick_fallback(""), None, "没有候选就没有回退");
    }
}
