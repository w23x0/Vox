//! GDI 字形栅格化：按**字形 id** 把一个字形画成 8 位覆盖率掩码，交给 `Canvas` 自己合成。
//!
//! ## 整形在哪做
//!
//! 整形（连写、附标定位、印地语连字）不在 GDI 里做，而在 core 的 `FaceSet`（rustybuzz）里做，
//! 输入是**同一份字体文件**：这里用 `GetFontData` 把 GDI 选中的字体整份读出来交给 core。
//! core 吐出字形 id，我们用 `ExtTextOutW` + `ETO_GLYPH_INDEX` 按 id 画——GDI 的字形索引
//! 就是字体文件里的 glyph id，所以两边对得上，Linux 与 Windows 看到的是同一组字形。
//!
//! 字体回退：GDI 在按字形 id 画时不会替我们做字体链接（font linking），所以回退也走 core 的
//! `FaceSet`：缺字时按一张固定的候选表（Segoe UI / Nirmala UI / Leelawadee UI / …）逐个试，
//! 取第一个覆盖该字符的系统字体，同样读整份文件、同样按字形 id 画。候选都没有就画 `.notdef` 豆腐块。
//!
//! ## 为什么不让 GDI 直接往分层窗的位图上写字
//!
//! GDI 完全不管 alpha 通道。开抗锯齿时它把字的边缘跟位图里**已有的颜色**混（我们的位图是
//! 全透明黑），于是半覆盖的边缘像素变成"一半文字色 + 一半黑"、alpha 却还是 0 或者被原样留着，
//! 贴到屏幕上就是一圈黑边——中日韩字笔画多、边缘像素占比高，黑边尤其明显。
//!
//! 所以走覆盖率路线：白字画在纯黑底上，读回任一颜色通道就是 0..255 的覆盖率，
//! 再由 `Canvas::blend_mask` 算 `alpha = 覆盖率 × 逐字 alpha` 并**手工预乘**文字色。
//! GDI 从头到尾没碰过真正要上屏的那块位图的 alpha。
//!
//! 另外故意用 `ANTIALIASED_QUALITY` 而不是 ClearType：ClearType 的 RGB 三个通道
//! 是三份不同的次像素覆盖率，压不进单一 alpha 通道，硬用就会出彩边。

use std::collections::HashMap;

use vox_core::ports::{PortError, PortResult};
use vox_overlay_core::shape::{pick_collection_face, FaceData, FaceSet, MAX_FACES};
use vox_overlay_core::text::{FontMetrics, Glyph, GlyphSource, Shaped};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, ExtTextOutW,
    GdiFlush, GetFontData, GetTextFaceW, GetTextMetricsW, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, ETO_GLYPH_INDEX, ETO_OPAQUE, FF_DONTCARE,
    FW_DEMIBOLD, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, OPAQUE, OUT_TT_PRECIS, TEXTMETRICW,
};

/// 字形掩码不是无限缓存：长时间跑多语种字幕时，不同字形可能持续增长。
const MAX_CACHED_GLYPHS: usize = 4096;

/// `GetFontData` 的 `dwTable`：字体集合（.ttc）的整份文件。小端的 "ttcf"。
const TTC_TAG: u32 = 0x6663_7474;
/// `GetFontData` 出错的返回值（`GDI_ERROR`）。
const GDI_ERROR_U32: u32 = u32::MAX;

/// 回退字体候选，按顺序试。都是随 Windows 发行的字体：
/// Segoe UI（拉丁/希腊/西里尔/阿拉伯/希伯来/…）、Nirmala UI（天城文等南亚文字）、
/// Leelawadee UI（泰文/老挝文/高棉文）、Microsoft YaHei UI / Yu Gothic UI / Malgun Gothic（中日韩）、
/// Segoe UI Symbol（符号）。
const FALLBACK_FAMILIES: [&str; 8] = [
    "Segoe UI",
    "Nirmala UI",
    "Leelawadee UI",
    "Microsoft YaHei UI",
    "Yu Gothic UI",
    "Malgun Gothic",
    "Segoe UI Symbol",
    "Arial Unicode MS",
];

struct CachedGlyph {
    glyph: Glyph,
    last_used: u64,
}

/// 一个 GDI 字体 + 画它时要用的度量。
struct GdiFont {
    font: HFONT,
    /// 基线到文字单元格顶边的距离。
    ascent: i32,
    /// 单元格高度。
    height: i32,
}

/// 一个回退候选的状态。
enum Candidate {
    /// 还没探测过。
    Untried,
    /// 探测过但不可用（系统没有这个字体、读不出文件……），不再重试。
    Failed,
    Ready(Probe),
}

/// 探测成功的回退候选。
struct Probe {
    font: GdiFont,
    face: FaceData,
    /// 已经交给 `FaceSet`（有槽位了）。
    registered: bool,
}

/// 回退字体的加载器：创建 GDI 字体、读整份文件。`find` 是交给 `FaceSet` 的查找回调。
///
/// 槽位对应关系：`FaceSet` 里第 `k + 1` 个字体面就是 `order[k]` 号候选——只在
/// `find` 返回的字体面一定会被 `FaceSet` 收下（先过 `is_usable_for`，且没到上限）时才成立。
struct Fallbacks {
    dc: HDC,
    lf_template: LOGFONTW,
    candidates: Vec<Candidate>,
    order: Vec<usize>,
}

impl Fallbacks {
    fn new(dc: HDC, lf_template: LOGFONTW) -> Self {
        Self {
            dc,
            lf_template,
            candidates: FALLBACK_FAMILIES
                .iter()
                .map(|_| Candidate::Untried)
                .collect(),
            order: Vec::new(),
        }
    }

    /// 回调：找一个覆盖 `ch` 的候选。每个候选只探测一次（探测失败也记下，不再重试）。
    fn find(&mut self, ch: char) -> Option<FaceData> {
        if self.order.len() + 1 >= MAX_FACES {
            return None;
        }
        for (i, family) in FALLBACK_FAMILIES.iter().enumerate() {
            if matches!(self.candidates[i], Candidate::Untried) {
                self.candidates[i] = match probe_family(self.dc, &self.lf_template, family) {
                    Some(probe) => Candidate::Ready(probe),
                    None => Candidate::Failed,
                };
            }
            let Candidate::Ready(probe) = &mut self.candidates[i] else {
                continue;
            };
            if probe.registered || !probe.face.is_usable_for(ch) {
                continue;
            }
            probe.registered = true;
            self.order.push(i);
            return Some(probe.face.clone());
        }
        None
    }

    /// 第 `nth` 个回退槽位对应的 GDI 字体。
    fn font(&self, nth: usize) -> Option<&GdiFont> {
        match self.candidates.get(*self.order.get(nth)?)? {
            Candidate::Ready(probe) => Some(&probe.font),
            _ => None,
        }
    }
}

/// 创建 `family` 的 GDI 字体并读出整份文件。GDI 找不到这个字体族时会悄悄换成别的字体，
/// 所以要核对 `GetTextFaceW` 回来的名字——对不上就不是我们要的候选。
fn probe_family(dc: HDC, template: &LOGFONTW, family: &str) -> Option<Probe> {
    let mut lf = *template;
    write_face_name(&mut lf.lfFaceName, family);
    // SAFETY: lf 是本地完整结构，CreateFontIndirectW 只读不留引用。
    let font = unsafe { CreateFontIndirectW(&lf) };
    if font.is_invalid() {
        return None;
    }
    // SAFETY: dc 与 font 都有效；换进去之后马上读完再换回来。
    let previous = unsafe { SelectObject(dc, font.into()) };
    let loaded = selected_face_name(dc)
        .eq_ignore_ascii_case(family)
        .then(|| read_selected_font(dc))
        .flatten();
    // SAFETY: 还原 DC 里原来选中的字体，探测到此为止。
    unsafe {
        SelectObject(dc, previous);
    }
    let Some((face, ascent, height)) = loaded else {
        // SAFETY: font 是上面建的有效对象，已经不在任何 DC 里。
        unsafe {
            let _ = DeleteObject(font.into());
        }
        return None;
    };
    Some(Probe {
        font: GdiFont {
            font,
            ascent,
            height,
        },
        face,
        registered: false,
    })
}

/// 读 DC 里当前选中字体的整份文件与度量（`FaceData`、基线上高、单元格高）。
fn read_selected_font(dc: HDC) -> Option<(FaceData, i32, i32)> {
    let mut tm = TEXTMETRICW::default();
    // SAFETY: dc 里已选好字体，tm 是本地可写结构。
    if !unsafe { GetTextMetricsW(dc, &mut tm) }.as_bool() {
        return None;
    }
    let bytes = font_file(dc)?;
    let name = selected_face_name(dc);
    let index = pick_collection_face(&bytes, &name, tm.tmWeight.clamp(0, 1000) as u16);
    Some((
        FaceData {
            bytes: std::sync::Arc::new(bytes),
            index,
        },
        tm.tmAscent.max(0),
        tm.tmHeight.max(1),
    ))
}

/// DC 里当前选中字体的族名。
fn selected_face_name(dc: HDC) -> String {
    let mut name = [0u16; 64];
    // SAFETY: name 是本地缓冲，长度随切片传进去，GDI 不会越界写。
    let n = unsafe { GetTextFaceW(dc, Some(&mut name)) };
    let n = (n.max(1) as usize - 1).min(name.len());
    String::from_utf16_lossy(&name[..n])
}

/// 把 DC 里当前选中字体的整份文件读出来：先试字体集合（.ttc），再试单个字体文件。
fn font_file(dc: HDC) -> Option<Vec<u8>> {
    for table in [TTC_TAG, 0u32] {
        // SAFETY: 不给缓冲区时只返回所需字节数。
        let size = unsafe { GetFontData(dc, table, 0, None, 0) };
        if size == GDI_ERROR_U32 || size == 0 {
            continue;
        }
        let mut buf = vec![0u8; size as usize];
        // SAFETY: buf 恰好 size 字节，GDI 最多写这么多。
        let got = unsafe { GetFontData(dc, table, 0, Some(buf.as_mut_ptr().cast()), size) };
        if got == size {
            return Some(buf);
        }
    }
    None
}

/// GDI 字体 + 一块用来画字的临时位图 + 字形缓存。
///
/// 只能在创建它的那个线程上用：GDI 的 DC 不是线程安全的，而且悬浮窗本来就规定
/// 所有绘制都在窗口线程上做。
pub struct FontRaster {
    dc: HDC,
    /// 槽位 0：主字体。回退字体的 GDI 对象在 `fallbacks` 里。
    primary: GdiFont,
    old_font: HGDIOBJ,
    /// 临时位图：画字用的画布，宽高够装下最大的一个字形。
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    scratch_w: i32,
    scratch_h: i32,
    /// 字号（像素）。
    px: i32,
    metrics: FontMetrics,
    faces: FaceSet,
    fallbacks: Fallbacks,
    cache: HashMap<(u16, u16), CachedGlyph>,
    cache_clock: u64,
}

impl FontRaster {
    /// 建一个字体。`size_pt` 是设置里的字号，`dpi` 是窗口所在显示器的 DPI。
    ///
    /// 字号按 DPI 换算：`px = pt * dpi / 72`。进程的 DPI 感知由外层 Tauri 应用设置，
    /// 这里只查不假设，所以传进来的 `dpi` 必须是 `GetDpiForWindow` 的实测值。
    pub fn new(family: &str, size_pt: u32, dpi: u32) -> PortResult<Self> {
        let dpi = if (72..=1200).contains(&dpi) { dpi } else { 96 };
        let px = ((size_pt.max(1) as i64 * dpi as i64) / 72).max(1) as i32;

        // SAFETY: CreateCompatibleDC(None) 用屏幕 DC 做模板，失败返回空句柄，下面立刻查。
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return Err(PortError::new("failed to create the memory DC for fonts"));
        }

        let mut lf = LOGFONTW {
            // 负值表示"字符高度"而不是"含行距的单元格高度"，这样字号跟视觉大小对得上。
            lfHeight: -px,
            lfWeight: FW_DEMIBOLD.0 as i32,
            lfCharSet: DEFAULT_CHARSET,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            // 逐字 alpha 要求单通道覆盖率，ClearType 的次像素覆盖率表达不了。
            lfQuality: ANTIALIASED_QUALITY,
            lfPitchAndFamily: DEFAULT_PITCH.0 | FF_DONTCARE.0,
            ..Default::default()
        };
        write_face_name(&mut lf.lfFaceName, family);

        // SAFETY: lf 是本地栈上的完整结构，CreateFontIndirectW 只读不留引用。
        let font = unsafe { CreateFontIndirectW(&lf) };
        if font.is_invalid() {
            // SAFETY: dc 是上面刚建的有效 DC，出错路径上必须还回去。
            unsafe {
                let _ = DeleteDC(dc);
            }
            return Err(PortError::new(format!(
                "failed to create font: {family} {size_pt}pt"
            )));
        }
        // SAFETY: dc 和 font 都有效；返回的旧字体句柄留着，销毁前要还原。
        let old_font = unsafe { SelectObject(dc, font.into()) };

        let release = |dc: HDC, old: HGDIOBJ, font: HFONT| {
            // SAFETY: 逐个还原并释放刚建的 GDI 对象。
            unsafe {
                SelectObject(dc, old);
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(dc);
            }
        };
        let Some((primary_face, ascent, height)) = read_selected_font(dc) else {
            release(dc, old_font, font);
            return Err(PortError::new(format!(
                "failed to read metrics or file data of font '{family}' (shaping needs the font file)"
            )));
        };
        let Some(faces) = FaceSet::new(primary_face) else {
            release(dc, old_font, font);
            return Err(PortError::new(format!(
                "font '{family}' is not a TrueType/OpenType font the shaping engine can open"
            )));
        };
        let mut tm = TEXTMETRICW::default();
        // SAFETY: dc 里已选好字体，tm 是本地可写结构。
        let _ = unsafe { GetTextMetricsW(dc, &mut tm) };
        let metrics = FontMetrics {
            line_height: (tm.tmHeight + tm.tmExternalLeading).max(1),
            ascent,
        };

        // 临时位图留足余量：整形出来的字形（阿拉伯连字、带附标的音节）最宽可以到两倍字身，
        // 附标可以高出行高，所以宽取三倍字身、高取两倍行高再加一个字身。
        let scratch_w = (px * 3).max(16);
        let scratch_h = (metrics.line_height * 2 + px).max(16);
        let mut raster = Self {
            dc,
            primary: GdiFont {
                font,
                ascent,
                height,
            },
            old_font,
            bitmap: HBITMAP::default(),
            old_bitmap: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            scratch_w,
            scratch_h,
            px,
            metrics,
            faces,
            fallbacks: Fallbacks::new(dc, lf),
            cache: HashMap::new(),
            cache_clock: 0,
        };
        raster.create_scratch()?;
        Ok(raster)
    }

    fn create_scratch(&mut self) -> PortResult<()> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.scratch_w,
                // 负高度 = 自上而下排布，行序跟 Canvas 一致，省一次翻转。
                biHeight: -self.scratch_h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: info 描述的就是下面要用的尺寸；ppvbits 收到的指针由这块位图拥有，
        // 生命周期跟 self.bitmap 绑定，Drop 里一起释放。
        let bitmap =
            unsafe { CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|e| {
                PortError::new(format!("failed to create the glyph scratch bitmap: {e}"))
            })?;
        if bits.is_null() {
            // SAFETY: bitmap 刚建成功，这条路径上要还回去。
            unsafe {
                let _ = DeleteObject(bitmap.into());
            }
            return Err(PortError::new(
                "the glyph scratch bitmap returned no pixel pointer",
            ));
        }
        // SAFETY: dc 有效，bitmap 刚建好且尚未被选进任何 DC。
        let old_bitmap = unsafe { SelectObject(self.dc, bitmap.into()) };
        self.bitmap = bitmap;
        self.old_bitmap = old_bitmap;
        self.bits = bits.cast();

        // SAFETY: dc 里已选好位图和字体，下面三个 Set* 只改 DC 状态。
        unsafe {
            SetBkMode(self.dc, OPAQUE);
            SetBkColor(self.dc, COLORREF(0x0000_0000));
            SetTextColor(self.dc, COLORREF(0x00ff_ffff));
        }
        Ok(())
    }

    /// 槽位对应的 GDI 字体。
    fn font_of(&self, slot: u16) -> Option<&GdiFont> {
        match slot {
            0 => Some(&self.primary),
            k => self.fallbacks.font(k as usize - 1),
        }
    }

    /// 按字形 id 画一个字形，读回覆盖率。
    fn rasterize(&mut self, slot: u16, id: u16) -> Option<Glyph> {
        let (font, ascent, height) = {
            let f = self.font_of(slot)?;
            (f.font, f.ascent, f.height)
        };
        // SAFETY: dc 与 font 都有效；每次画之前把这个槽位的字体选进去。
        unsafe {
            SelectObject(self.dc, font.into());
        }
        // 起笔点留出左边与上边的余量：附标常常伸到起笔点左边（负的左侧间隙）或行高之上。
        let left = (self.px / 2).max(2);
        let top = (self.px / 4).max(2);
        let w = self.scratch_w;
        let h = (top + height + self.px / 4).min(self.scratch_h);
        if w <= 0 || h <= 0 {
            return None;
        }
        let rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let glyph_index = [id];
        // SAFETY: rect 在临时位图范围内；ETO_OPAQUE 先用背景色(黑)刷掉上一个字形的残留，
        // 所以不需要额外清屏；ETO_GLYPH_INDEX 让 GDI 把这 1 个 u16 当字形索引而不是字符。
        let drawn = unsafe {
            ExtTextOutW(
                self.dc,
                left,
                top,
                ETO_OPAQUE | ETO_GLYPH_INDEX,
                Some(&rect),
                windows::core::PCWSTR(glyph_index.as_ptr()),
                1,
                None,
            )
        }
        .as_bool();
        if !drawn {
            return None;
        }
        // GDI 的绘制是排队的，CPU 直接读位图前必须让它落地。
        // SAFETY: 无参数，只是刷新当前线程的 GDI 批处理队列。
        unsafe {
            let _ = GdiFlush();
        }

        let stride = (self.scratch_w * 4) as usize;
        let mut cov = vec![0u8; (w * h) as usize];
        for y in 0..h {
            // SAFETY: bits 指向 scratch_w * scratch_h * 4 字节，w/h 已夹到该范围内，
            // 每行按 stride 定位，读的都是这块位图自己的内存。
            let row =
                unsafe { std::slice::from_raw_parts(self.bits.add(y as usize * stride), stride) };
            for x in 0..w {
                // 白字黑底，任一通道即覆盖率；取蓝通道（BGRA 的第 0 字节）。
                let v = row.get((x * 4) as usize).copied().unwrap_or(0);
                if let Some(slot) = cov.get_mut((y * w + x) as usize) {
                    *slot = v;
                }
            }
        }

        Some(trim(Glyph {
            w,
            h,
            off_x: -left,
            off_y: -(top + ascent),
            cov,
        }))
    }
}

impl Drop for FontRaster {
    fn drop(&mut self) {
        // SAFETY: 先把 DC 里选中的对象换回原来的，再删自己建的对象——顺序反了会泄漏。
        unsafe {
            if !self.old_bitmap.is_invalid() {
                SelectObject(self.dc, self.old_bitmap);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.old_font.is_invalid() {
                SelectObject(self.dc, self.old_font);
            }
            if !self.primary.font.is_invalid() {
                let _ = DeleteObject(self.primary.font.into());
            }
            for candidate in &self.fallbacks.candidates {
                if let Candidate::Ready(probe) = candidate {
                    let _ = DeleteObject(probe.font.font.into());
                }
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

/// 把掩码四周全 0 的行列裁掉：缓存小一点，合成时少扫一片空白。
pub(crate) fn trim(g: Glyph) -> Glyph {
    if g.w <= 0 || g.h <= 0 {
        return g;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (g.w, g.h, -1, -1);
    for y in 0..g.h {
        for x in 0..g.w {
            if g.cov.get((y * g.w + x) as usize).copied().unwrap_or(0) != 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if max_x < 0 {
        return Glyph {
            w: 0,
            h: 0,
            cov: Vec::new(),
            ..g
        };
    }
    let (nw, nh) = (max_x - min_x + 1, max_y - min_y + 1);
    let mut cov = vec![0u8; (nw * nh) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let src = ((y + min_y) * g.w + (x + min_x)) as usize;
            if let (Some(d), Some(s)) = (cov.get_mut((y * nw + x) as usize), g.cov.get(src)) {
                *d = *s;
            }
        }
    }
    Glyph {
        w: nw,
        h: nh,
        off_x: g.off_x + min_x,
        off_y: g.off_y + min_y,
        cov,
    }
}

/// 往 `LOGFONTW.lfFaceName` 里写字体名。字段是定长 32 且必须以 0 结尾，超长直接截断
/// ——GDI 自己也是这么处理的，截断后匹配不上会退回系统默认字体，不会失败。
pub(crate) fn write_face_name(dst: &mut [u16; 32], family: &str) {
    let mut i = 0;
    for u in family.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

/// 光栅器接口：渲染器只认这个 trait，Windows 用 GDI，Linux 用 swash。
impl GlyphSource for FontRaster {
    fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// 整形交给 core 的 `FaceSet`（rustybuzz，用的是从 GDI 读出来的同一份字体文件）。
    fn shape(&mut self, run: &str, rtl: bool, base: u32, out: &mut Shaped) {
        let fallbacks = &mut self.fallbacks;
        self.faces.shape(
            run,
            rtl,
            base,
            self.px as f32,
            &mut |ch| fallbacks.find(ch),
            out,
        );
    }

    /// 取一个字形的覆盖率掩码，失败返回 `None`（这一个字形不画，别拖垮整帧）。
    fn glyph(&mut self, face: u16, id: u16) -> Option<&Glyph> {
        let key = (face, id);
        self.cache_clock = self.cache_clock.wrapping_add(1);
        let now = self.cache_clock;
        if self.cache.contains_key(&key) {
            let cached = self.cache.get_mut(&key)?;
            cached.last_used = now;
            return Some(&cached.glyph);
        }

        let glyph = self.rasterize(face, id)?;
        if self.cache.len() >= MAX_CACHED_GLYPHS {
            let oldest = self
                .cache
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(&key, _)| key);
            if let Some(oldest) = oldest {
                self.cache.remove(&oldest);
            }
        }
        self.cache.insert(
            key,
            CachedGlyph {
                glyph,
                last_used: now,
            },
        );
        self.cache.get(&key).map(|cached| &cached.glyph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 整形一段文字，取第一个字形。
    fn first_glyph(f: &mut FontRaster, text: &str) -> (u16, u16, i32) {
        let mut out = Shaped::default();
        f.shape(text, false, 0, &mut out);
        let g = out.glyphs[0];
        (g.face, g.id, out.clusters[0].advance)
    }

    #[test]
    fn face_name_is_null_terminated() {
        let mut buf = [0xffffu16; 32];
        write_face_name(&mut buf, "Microsoft YaHei UI");
        let end = buf.iter().position(|&u| u == 0).expect("必须有终止符");
        assert_eq!(end, "Microsoft YaHei UI".len());
        let round: String = String::from_utf16_lossy(&buf[..end]);
        assert_eq!(round, "Microsoft YaHei UI");
    }

    #[test]
    fn face_name_truncates_instead_of_overflowing() {
        let mut buf = [0xffffu16; 32];
        write_face_name(&mut buf, &"字".repeat(100));
        assert_eq!(buf[31], 0, "最后一格必须是终止符");
        let end = buf.iter().position(|&u| u == 0).expect("必须有终止符");
        assert!(end <= 31);
    }

    #[test]
    fn trim_drops_empty_border() {
        // 5x3，只有中间一个像素有覆盖率。
        const W: usize = 5;
        let (row, col) = (1usize, 2usize);
        let mut cov = vec![0u8; 15];
        cov[row * W + col] = 200;
        let g = trim(Glyph {
            w: 5,
            h: 3,
            off_x: 0,
            off_y: -10,
            cov,
        });
        assert_eq!((g.w, g.h), (1, 1));
        assert_eq!(g.cov, vec![200]);
        assert_eq!(g.off_x, 2, "裁掉的空白要折进偏移里");
        assert_eq!(g.off_y, -9);
    }

    #[test]
    fn trim_of_blank_glyph_yields_zero_area() {
        let g = trim(Glyph {
            w: 4,
            h: 4,
            off_x: 0,
            off_y: 0,
            cov: vec![0u8; 16],
        });
        assert_eq!((g.w, g.h), (0, 0));
        assert!(g.cov.is_empty());
    }

    /// 需要 GDI，跑在有桌面的机器上：`cargo test -p vox-overlay-win -- --ignored`
    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn rasterizes_cjk_and_latin_with_sane_coverage() {
        let mut f = FontRaster::new("Microsoft YaHei UI", 30, 96).unwrap();
        assert!(f.metrics().line_height > 0 && f.metrics().ascent > 0);
        for text in ["测", "试", "A", "g", "，"] {
            let (face, id, advance) = first_glyph(&mut f, text);
            let g = f.glyph(face, id).unwrap().clone();
            assert!(advance > 0, "{text} 步进为 0");
            assert!(g.w > 0 && g.h > 0, "{text} 没有可见像素");
            assert_eq!(g.cov.len(), (g.w * g.h) as usize);
            assert!(
                g.cov.iter().any(|&c| c > 200),
                "{text} 应该有接近全覆盖的笔画中心"
            );
            assert!(
                g.cov.iter().any(|&c| (1..=200).contains(&c)),
                "{text} 应该有抗锯齿的中间覆盖率，否则说明抗锯齿没开"
            );
        }
        // 空格没有可见像素但要占宽度。
        let (face, id, advance) = first_glyph(&mut f, " ");
        let sp = f.glyph(face, id).unwrap().clone();
        assert!(advance > 0 && sp.w == 0);
        // 缓存命中不该改变结果。
        let (face, id, _) = first_glyph(&mut f, "测");
        let a = f.glyph(face, id).unwrap().clone();
        let b = f.glyph(face, id).unwrap().clone();
        assert_eq!(a.cov, b.cov);
    }

    /// 阿拉伯字母在主字体里没有时走回退字体，连写后的字形有墨迹。
    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn arabic_shapes_through_a_fallback_font() {
        let mut f = FontRaster::new("Microsoft YaHei UI", 30, 96).unwrap();
        let mut out = Shaped::default();
        f.shape("بببب", true, 0, &mut out);
        assert_eq!(out.clusters.len(), 4);
        let ids: Vec<u16> = out
            .clusters
            .iter()
            .map(|c| out.glyphs[c.glyph_start as usize].id)
            .collect();
        assert!(
            ids[0] != ids[1] && ids[1] != ids[3],
            "词首/词中/词尾字形不同：{ids:?}"
        );
        let g = out.glyphs[0];
        assert!(
            f.glyph(g.face, g.id).is_some_and(|m| m.w > 0),
            "回退字体里的字形要有墨迹"
        );
    }

    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn dpi_scaling_makes_bigger_glyphs() {
        let mut lo = FontRaster::new("Microsoft YaHei UI", 30, 96).unwrap();
        let mut hi = FontRaster::new("Microsoft YaHei UI", 30, 192).unwrap();
        assert!(
            first_glyph(&mut hi, "测").2 > first_glyph(&mut lo, "测").2,
            "200% 缩放下字应当更宽"
        );
        assert!(hi.metrics().line_height > lo.metrics().line_height);
    }

    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn missing_font_falls_back_instead_of_failing() {
        // GDI 匹配不上字体名会退回系统字体，这是我们想要的行为：设置里填错不该让窗口起不来。
        let mut f = FontRaster::new("This Font Does Not Exist 中文", 24, 96).unwrap();
        assert!(first_glyph(&mut f, "测").2 > 0);
    }

    #[test]
    #[ignore = "需要真实 GDI 环境"]
    fn glyph_cache_is_bounded() {
        let mut f = FontRaster::new("Microsoft YaHei UI", 24, 96).unwrap();
        for offset in 0..(MAX_CACHED_GLYPHS as u32 + 64) {
            if let Some(ch) = char::from_u32(0x4e00 + offset) {
                let mut out = Shaped::default();
                f.shape(&ch.to_string(), false, 0, &mut out);
                if let Some(g) = out.glyphs.first() {
                    let _ = f.glyph(g.face, g.id);
                }
            }
        }
        assert!(
            f.cache.len() <= MAX_CACHED_GLYPHS,
            "字形缓存不能随运行时间无限增长"
        );
    }
}
