//! 测试共用：不依赖任何真字体的"格子字体"。
//!
//! 它在**簇的划分**上模仿真实整形引擎的行为——附标、元音符号和印地语的"辅音 + virama + 辅音"
//! 并进前一个簇——但宽度是固定值，所以布局测试可以断言精确坐标。真字体下的整形（连写、
//! 附标定位）由 `vox-overlay-linux` 的测试用系统字体验证。

use vox_overlay_core::text::{Cluster, FontMetrics, Glyph, GlyphSource, Shaped, ShapedGlyph};

/// CJK 26 px，其余 14 px，附标 0 px；行高 40，基线 32。
pub struct FakeFont;

pub const WIDE: i32 = 26;
pub const NARROW: i32 = 14;

pub fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x303F | 0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xFF00..=0xFF60)
}

/// 附标：希伯来语点号、阿拉伯语变音符、泰语元音/声调符、天城文元音符号与 virama、组合用附加符。
pub fn is_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F | 0x05B0..=0x05C7 | 0x064B..=0x065F | 0x0E31
        | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x0900..=0x0903 | 0x093A..=0x094F)
}

const VIRAMA: char = '\u{094D}';

impl GlyphSource for FakeFont {
    fn metrics(&self) -> FontMetrics {
        FontMetrics {
            line_height: 40,
            ascent: 32,
        }
    }

    fn shape(&mut self, run: &str, _rtl: bool, base: u32, out: &mut Shaped) {
        let mut prev: Option<char> = None;
        for (i, ch) in run.char_indices() {
            let advance = if is_mark(ch) {
                0
            } else if is_wide(ch) {
                WIDE
            } else {
                NARROW
            };
            let joins = prev.is_some() && (is_mark(ch) || prev == Some(VIRAMA));
            prev = Some(ch);
            let glyph = out.glyphs.len() as u32;
            out.glyphs.push(ShapedGlyph {
                face: 0,
                id: ch as u32 as u16,
                x_offset: 0,
                y_offset: 0,
                advance,
            });
            match out.clusters.last_mut() {
                Some(last) if joins && last.start >= base => {
                    last.advance += advance;
                    last.glyph_end = glyph + 1;
                }
                _ => out.clusters.push(Cluster {
                    start: base + i as u32,
                    advance,
                    glyph_start: glyph,
                    glyph_end: glyph + 1,
                }),
            }
        }
    }

    fn glyph(&mut self, _face: u16, _id: u16) -> Option<&Glyph> {
        // 只测布局，不需要真的墨迹。
        None
    }
}
