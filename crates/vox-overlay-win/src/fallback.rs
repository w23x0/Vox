//! 降级光栅器：`FontRaster` 建不起来时（`GetFontData` 拿不到字体文件、字体不是
//! rustybuzz 能打开的 TrueType/OpenType 等）字幕仍然要显示，而不是整块消失。
//!
//! 做法：不整形、不按字形 id 画，而是把文字切成"词"（空白分隔；中日韩假名谚文逐字一簇，
//! 这样不带空格的文字也能折行），每个词用 `ExtTextOutW` 按**字符串**画——**不带**
//! `ETO_GLYPH_INDEX`，GDI 会自己走 Uniscribe 整形并做字体链接（font linking），
//! 阿拉伯语连写、印地语音节、缺字回退都由系统处理。
//!
//! 代价：词表占 `u16` id（`ShapedGlyph.id`），满了就整张表清空重来（几万个不同的词，
//! 要连续说很久才会碰到）；每个新词第一次出现时才栅格化，之后走字形缓存。
//! 度量与覆盖率路线（白字黑底读回单通道）与 [`crate::font::FontRaster`] 相同。

use std::collections::HashMap;

use vox_core::ports::{PortError, PortResult};
use vox_overlay_core::text::{Cluster, FontMetrics, Glyph, GlyphSource, Shaped, ShapedGlyph};
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, ExtTextOutW,
    GdiFlush, GetTextExtentPoint32W, GetTextMetricsW, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, ETO_OPAQUE, FF_DONTCARE, FW_DEMIBOLD, HBITMAP,
    HDC, HFONT, HGDIOBJ, LOGFONTW, OPAQUE, OUT_TT_PRECIS, TEXTMETRICW,
};

use crate::font::{trim, write_face_name};

/// 词表上限（`ShapedGlyph.id` 是 `u16`）。
const MAX_WORDS: usize = u16::MAX as usize;

pub struct TextOutRaster {
    dc: HDC,
    font: HFONT,
    old_font: HGDIOBJ,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    scratch_w: i32,
    scratch_h: i32,
    px: i32,
    ascent: i32,
    metrics: FontMetrics,
    /// id -> 词；`index` 是反查。
    words: Vec<String>,
    index: HashMap<String, u16>,
    cache: HashMap<u16, Glyph>,
}

/// 假名、谚文、汉字、全角：这些文字不连写，逐字一簇。
fn is_unjoined(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

impl TextOutRaster {
    pub fn new(family: &str, size_pt: u32, dpi: u32) -> PortResult<Self> {
        let dpi = if (72..=1200).contains(&dpi) { dpi } else { 96 };
        let px = ((size_pt.max(1) as i64 * dpi as i64) / 72).max(1) as i32;

        // SAFETY: CreateCompatibleDC(None) 用屏幕 DC 做模板，失败返回空句柄，下面立刻查。
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return Err(PortError::new("failed to create the memory DC for fonts"));
        }
        let mut lf = LOGFONTW {
            lfHeight: -px,
            lfWeight: FW_DEMIBOLD.0 as i32,
            lfCharSet: DEFAULT_CHARSET,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: ANTIALIASED_QUALITY,
            lfPitchAndFamily: DEFAULT_PITCH.0 | FF_DONTCARE.0,
            ..Default::default()
        };
        write_face_name(&mut lf.lfFaceName, family);
        // SAFETY: lf 是本地栈上的完整结构，CreateFontIndirectW 只读不留引用。
        let font = unsafe { CreateFontIndirectW(&lf) };
        if font.is_invalid() {
            // SAFETY: dc 刚建成功，出错路径上还回去。
            unsafe {
                let _ = DeleteDC(dc);
            }
            return Err(PortError::new(format!(
                "failed to create font: {family} {size_pt}pt"
            )));
        }
        // SAFETY: dc 与 font 有效；旧字体句柄留着，销毁前还原。
        let old_font = unsafe { SelectObject(dc, font.into()) };
        let mut tm = TEXTMETRICW::default();
        // SAFETY: dc 里已选好字体，tm 是本地可写结构。
        let _ = unsafe { GetTextMetricsW(dc, &mut tm) };
        let metrics = FontMetrics {
            line_height: (tm.tmHeight + tm.tmExternalLeading).max(1),
            ascent: tm.tmAscent,
        };
        let mut raster = Self {
            dc,
            font,
            old_font,
            bitmap: HBITMAP::default(),
            old_bitmap: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            scratch_w: 0,
            scratch_h: 0,
            px,
            ascent: tm.tmAscent,
            metrics,
            words: Vec::new(),
            index: HashMap::new(),
            cache: HashMap::new(),
        };
        // 先备一块够一个字身三倍宽的画布；更长的词栅格化时再扩。
        raster.ensure_scratch((px * 3).max(16), (metrics.line_height * 2 + px).max(16))?;
        Ok(raster)
    }

    /// 画布不够就换一块大的（旧的还回去）。
    fn ensure_scratch(&mut self, w: i32, h: i32) -> PortResult<()> {
        if !self.bits.is_null() && self.scratch_w >= w && self.scratch_h >= h {
            return Ok(());
        }
        let (w, h) = (w.max(self.scratch_w), h.max(self.scratch_h));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: info 描述的就是下面要用的尺寸；位图拥有 bits，生命周期跟 self.bitmap 绑定。
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
        // SAFETY: dc 有效；先把旧位图选出来再删，新位图尚未被选进任何 DC。
        unsafe {
            let previous = SelectObject(self.dc, bitmap.into());
            if self.bitmap.is_invalid() {
                self.old_bitmap = previous;
            } else {
                let _ = DeleteObject(self.bitmap.into());
            }
            SetBkMode(self.dc, OPAQUE);
            SetBkColor(self.dc, COLORREF(0x0000_0000));
            SetTextColor(self.dc, COLORREF(0x00ff_ffff));
        }
        self.bitmap = bitmap;
        self.bits = bits.cast();
        self.scratch_w = w;
        self.scratch_h = h;
        Ok(())
    }

    fn extent(&self, wide: &[u16]) -> i32 {
        let mut size = SIZE::default();
        // SAFETY: dc 里已选好字体，wide 与 size 都是本地有效内存。
        if unsafe { GetTextExtentPoint32W(self.dc, wide, &mut size) }.as_bool() {
            size.cx
        } else {
            self.px
        }
    }

    fn intern(&mut self, word: &str) -> u16 {
        if let Some(&id) = self.index.get(word) {
            return id;
        }
        if self.words.len() >= MAX_WORDS {
            self.words.clear();
            self.index.clear();
            self.cache.clear();
        }
        let id = self.words.len() as u16;
        self.words.push(word.to_owned());
        self.index.insert(word.to_owned(), id);
        id
    }

    fn rasterize(&mut self, id: u16) -> Option<Glyph> {
        let wide: Vec<u16> = self.words.get(id as usize)?.encode_utf16().collect();
        if wide.is_empty() {
            return None;
        }
        let left = (self.px / 2).max(2);
        let top = (self.px / 4).max(2);
        let w = self.extent(&wide) + left * 2;
        let h = top + self.metrics.line_height + self.px / 4;
        self.ensure_scratch(w, h).ok()?;
        let rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        // SAFETY: rect 在画布范围内；ETO_OPAQUE 先用黑底刷掉上一个词的残留。
        // 不带 ETO_GLYPH_INDEX：wide 当字符串，GDI 经 Uniscribe 整形并做字体链接。
        let drawn = unsafe {
            ExtTextOutW(
                self.dc,
                left,
                top,
                ETO_OPAQUE,
                Some(&rect),
                windows::core::PCWSTR(wide.as_ptr()),
                wide.len() as u32,
                None,
            )
        }
        .as_bool();
        if !drawn {
            return None;
        }
        // SAFETY: 无参数，只是刷新当前线程的 GDI 批处理队列。
        unsafe {
            let _ = GdiFlush();
        }
        let stride = (self.scratch_w * 4) as usize;
        let mut cov = vec![0u8; (w * h) as usize];
        for y in 0..h {
            // SAFETY: bits 指向 scratch_w * scratch_h * 4 字节，w/h 不超过画布，
            // 每行按 stride 定位，读的都是这块位图自己的内存。
            let row =
                unsafe { std::slice::from_raw_parts(self.bits.add(y as usize * stride), stride) };
            for x in 0..w {
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
            off_y: -(top + self.ascent),
            cov,
        }))
    }
}

impl GlyphSource for TextOutRaster {
    fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    fn shape(&mut self, run: &str, _rtl: bool, base: u32, out: &mut Shaped) {
        let mut start = 0usize;
        let push = |this: &mut Self, from: usize, to: usize, out: &mut Shaped| {
            let word = &run[from..to];
            let wide: Vec<u16> = word.encode_utf16().collect();
            let advance = this.extent(&wide);
            let id = this.intern(word);
            let glyph_start = out.glyphs.len() as u32;
            out.glyphs.push(ShapedGlyph {
                face: 0,
                id,
                x_offset: 0,
                y_offset: 0,
                advance,
            });
            out.clusters.push(Cluster {
                start: base + from as u32,
                advance,
                glyph_start,
                glyph_end: out.glyphs.len() as u32,
            });
        };
        for (i, c) in run.char_indices() {
            let end = i + c.len_utf8();
            if c.is_whitespace() {
                if start < i {
                    push(self, start, i, out);
                }
                push(self, i, end, out);
                start = end;
            } else if is_unjoined(c) {
                if start < i {
                    push(self, start, i, out);
                }
                push(self, i, end, out);
                start = end;
            }
        }
        if start < run.len() {
            push(self, start, run.len(), out);
        }
    }

    fn glyph(&mut self, face: u16, id: u16) -> Option<&Glyph> {
        if face != 0 {
            return None;
        }
        if !self.cache.contains_key(&id) {
            let glyph = self.rasterize(id)?;
            self.cache.insert(id, glyph);
        }
        self.cache.get(&id)
    }
}

impl Drop for TextOutRaster {
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
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words_and_cjk_into_wrappable_clusters() {
        let mut raster = TextOutRaster::new("Microsoft YaHei UI", 24, 96).unwrap();
        let mut shaped = Shaped::default();
        raster.shape("ab 测试 cd", false, 10, &mut shaped);
        let starts: Vec<u32> = shaped.clusters.iter().map(|c| c.start).collect();
        // "ab" " " "测" "试" " " "cd"
        assert_eq!(starts, [10, 12, 13, 16, 19, 20]);
        assert!(raster.glyph(0, shaped.glyphs[0].id).is_some());
    }
}
