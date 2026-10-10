//! 整形：把一段同方向的文字变成"字形簇 + 字形 id + 前进宽度"。
//!
//! 引擎是 `rustybuzz`（HarfBuzz 的纯 Rust 移植）。整形放在 core 里、两个平台共用同一份
//! 字体字节，所以 Linux 与 Windows 看到的是同一组字形 id（理由见 `docs/plans/OVERLAY-SHAPING.md`）。
//!
//! 一次 [`FaceSet::shape`] 干三件事：
//!
//! 1. **选字体**：逐字找第一个覆盖它的字体面（主字体 → 已加载的回退字体 → 向平台要一个新的
//!    回退字体）。一个字谁都不覆盖就留在主字体里，画出 `.notdef` 豆腐块，不报错。
//! 2. **切分段**：同一字体、同一文字（脚本）的连续字为一段——HarfBuzz 要求一段一个脚本。
//! 3. **整形并分簇**：每段交给 rustybuzz，输出按簇归并。簇永远按**逻辑顺序**排；
//!    从右往左的段，rustybuzz 给的是视觉序，这里把簇序倒回逻辑序，簇内字形仍是从左到右的画序。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustybuzz::ttf_parser::{self, Tag};
use rustybuzz::{script, Direction, Face, Script, UnicodeBuffer};
use unicode_script::{Script as UScript, UnicodeScript};

use crate::text::{Cluster, Shaped, ShapedGlyph};

/// 同时装几个字体面（主字体 + 回退）。回退字体是按需加的，封个顶免得坏字体拖垮内存。
pub const MAX_FACES: usize = 8;
/// 覆盖查询缓存的上限（按字符数）；超过就整张清掉重来。
const MAX_COVER_CACHE: usize = 16_384;

/// 一份字体文件里的一个字体面。字节用 `Arc` 共享：CJK 字体动辄十几 MB，不能每次都拷。
#[derive(Debug, Clone)]
pub struct FaceData {
    pub bytes: Arc<Vec<u8>>,
    /// 字体集合（.ttc）里的第几个面；单个字体是 0。
    pub index: u32,
}

impl FaceData {
    /// 整形引擎打得开这份字体，并且字体里有 `ch` 的字形。平台的回退查找返回前必须自己过一遍
    /// 这个检查：[`FaceSet`] 只收这样的字体面，平台侧按返回顺序给字体面编槽位才对得上。
    pub fn is_usable_for(&self, ch: char) -> bool {
        Face::from_slice(&self.bytes, self.index).is_some()
            && ttf_parser::Face::parse(&self.bytes, self.index)
                .ok()
                .and_then(|f| f.glyph_index(ch))
                .is_some()
    }
}

/// 字体集合（.ttc）里挑出跟 `family` 同族、字重最接近 `weight` 的那个面的下标；
/// 不是集合文件或没有同族的面就是 0。Windows 从 GDI 取回的是整份文件，靠它认出 GDI 选中的那个面。
pub fn pick_collection_face(bytes: &[u8], family: &str, weight: u16) -> u32 {
    let Some(count) = ttf_parser::fonts_in_collection(bytes) else {
        return 0;
    };
    let mut best: Option<(u32, u32)> = None;
    for index in 0..count {
        let Ok(face) = ttf_parser::Face::parse(bytes, index) else {
            continue;
        };
        // 名字表里 1 = 族名，16 = 排版族名。
        let same_family = face.names().into_iter().any(|name| {
            matches!(name.name_id, 1 | 16)
                && name
                    .to_string()
                    .is_some_and(|n| n.eq_ignore_ascii_case(family))
        });
        if !same_family {
            continue;
        }
        let distance = (i32::from(face.weight().to_number()) - i32::from(weight)).unsigned_abs();
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, index));
        }
    }
    best.map_or(0, |(_, index)| index)
}

/// 平台给的"找一个覆盖这个字的字体"。找不到返回 `None`。
pub type FallbackFinder<'a> = &'a mut dyn FnMut(char) -> Option<FaceData>;

/// 一组字体面 + 整形缓冲。槽位 0 永远是主字体。
pub struct FaceSet {
    faces: Vec<FaceData>,
    /// `covers[slot][ch]`：该字体面有没有这个字的字形。
    covers: Vec<HashMap<char, bool>>,
    /// 向平台要过、没要到（或要到了却不覆盖）的字：不再重复要。
    missing: HashSet<char>,
    buffer: Option<UnicodeBuffer>,
    char_slot: Vec<u16>,
    char_script: Vec<UScript>,
}

impl FaceSet {
    /// 用主字体建。字节解析不了（不是字体）返回 `None`。
    pub fn new(primary: FaceData) -> Option<Self> {
        Face::from_slice(&primary.bytes, primary.index)?;
        Some(Self {
            faces: vec![primary],
            covers: vec![HashMap::new()],
            missing: HashSet::new(),
            buffer: Some(UnicodeBuffer::new()),
            char_slot: Vec::new(),
            char_script: Vec::new(),
        })
    }

    /// 槽位对应的字体面。
    pub fn face(&self, slot: u16) -> Option<&FaceData> {
        self.faces.get(slot as usize)
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// 槽位 `slot` 的字体里有没有 `ch` 的字形（有缓存）。
    pub fn covers(&mut self, slot: usize, ch: char) -> bool {
        let Some(face) = self.faces.get(slot) else {
            return false;
        };
        if let Some(hit) = self.covers[slot].get(&ch) {
            return *hit;
        }
        let has = ttf_parser::Face::parse(&face.bytes, face.index)
            .ok()
            .and_then(|f| f.glyph_index(ch))
            .is_some();
        let cache = &mut self.covers[slot];
        if cache.len() >= MAX_COVER_CACHE {
            cache.clear();
        }
        cache.insert(ch, has);
        has
    }

    /// 整形 `run`（同方向的一段），簇按逻辑顺序追加到 `out`。
    ///
    /// `size_px` 是字号（像素）；`base` 加到每个簇的 `start` 上，让多段拼成一整段文字的偏移。
    pub fn shape(
        &mut self,
        run: &str,
        rtl: bool,
        base: u32,
        size_px: f32,
        fallback: FallbackFinder<'_>,
        out: &mut Shaped,
    ) {
        if run.is_empty() {
            return;
        }
        self.assign(run, fallback);
        let mut seg_start = 0usize;
        let mut cur: Option<(u16, UScript)> = None;
        for (k, (byte, _)) in run.char_indices().enumerate() {
            let key = (self.char_slot[k], self.char_script[k]);
            match cur {
                None => cur = Some(key),
                Some(c) if c == key => {}
                Some((slot, scr)) => {
                    self.shape_segment(
                        &run[seg_start..byte],
                        seg_start as u32 + base,
                        rtl,
                        slot,
                        scr,
                        size_px,
                        out,
                    );
                    seg_start = byte;
                    cur = Some(key);
                }
            }
        }
        if let Some((slot, scr)) = cur {
            self.shape_segment(
                &run[seg_start..],
                seg_start as u32 + base,
                rtl,
                slot,
                scr,
                size_px,
                out,
            );
        }
    }

    /// 给每个字定字体槽位与脚本，结果在 `char_slot` / `char_script`。
    fn assign(&mut self, run: &str, fallback: FallbackFinder<'_>) {
        self.char_slot.clear();
        self.char_script.clear();
        let mut sticky = 0usize;
        for ch in run.chars() {
            let slot = if is_ignorable(ch) || self.covers(sticky, ch) {
                // 黏着当前字体：数字、标点、附标跟着前面的字走，不在字体间来回切。
                sticky
            } else {
                self.slot_for(ch, fallback)
            };
            sticky = slot;
            self.char_slot.push(slot as u16);
            self.char_script.push(ch.script());
        }
        // 通用/继承脚本的字（标点、空格、附标）跟着前面的真脚本走；开头的跟着后面第一个。
        let mut last = None;
        for s in self.char_script.iter_mut() {
            if is_real(*s) {
                last = Some(*s);
            } else if let Some(prev) = last {
                *s = prev;
            }
        }
        let first_real = self.char_script.iter().copied().find(|s| is_real(*s));
        if let Some(real) = first_real {
            for s in self.char_script.iter_mut() {
                if is_real(*s) {
                    break;
                }
                *s = real;
            }
        }
    }

    /// 找覆盖 `ch` 的字体面槽位；谁都不覆盖就是 0（画豆腐块）。
    fn slot_for(&mut self, ch: char, fallback: FallbackFinder<'_>) -> usize {
        for slot in 0..self.faces.len() {
            if self.covers(slot, ch) {
                return slot;
            }
        }
        if self.faces.len() < MAX_FACES && !self.missing.contains(&ch) {
            if let Some(found) = fallback(ch) {
                if found.is_usable_for(ch) {
                    self.faces.push(found);
                    self.covers.push(HashMap::new());
                    return self.faces.len() - 1;
                }
            }
            self.missing.insert(ch);
        }
        0
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_segment(
        &mut self,
        text: &str,
        base: u32,
        rtl: bool,
        slot: u16,
        script: UScript,
        size_px: f32,
        out: &mut Shaped,
    ) {
        let Some(data) = self.faces.get(slot as usize) else {
            return;
        };
        let Some(face) = Face::from_slice(&data.bytes, data.index) else {
            return;
        };
        let scale = size_px / (face.units_per_em().max(1) as f32);
        let mut buffer = self.buffer.take().unwrap_or_default();
        for (i, ch) in text.char_indices() {
            buffer.add(ch, base + i as u32);
        }
        buffer.set_direction(if rtl {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        });
        buffer.set_script(hb_script(script));
        let shaped = rustybuzz::shape(&face, &[], buffer);

        let first_cluster = out.clusters.len();
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        let mut i = 0usize;
        while i < infos.len() {
            let cluster = infos[i].cluster;
            let mut j = i;
            let glyph_start = out.glyphs.len() as u32;
            let mut advance = 0i32;
            while j < infos.len() && infos[j].cluster == cluster {
                let pos = &positions[j];
                let step = (pos.x_advance as f32 * scale).round() as i32;
                out.glyphs.push(ShapedGlyph {
                    face: slot,
                    id: infos[j].glyph_id as u16,
                    x_offset: (pos.x_offset as f32 * scale).round() as i32,
                    y_offset: (pos.y_offset as f32 * scale).round() as i32,
                    advance: step,
                });
                advance += step;
                j += 1;
            }
            out.clusters.push(Cluster {
                start: cluster,
                advance,
                glyph_start,
                glyph_end: out.glyphs.len() as u32,
            });
            i = j;
        }
        if rtl {
            // rustybuzz 给从右往左的段是视觉序；簇序倒回逻辑序（簇内字形仍是画序）。
            out.clusters[first_cluster..].reverse();
        }
        self.buffer = Some(shaped.clear());
    }
}

/// 默认忽略字符（零宽连接/非连接、方向标记、变体选择符……）：字体通常没有它们的字形，
/// 但 HarfBuzz 认识它们。它们必须留在前后文字所在的段里，否则会把一个连写词切成两段。
fn is_ignorable(ch: char) -> bool {
    matches!(ch as u32,
        0x00AD | 0x034F | 0x180B..=0x180E | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x2064 | 0xFE00..=0xFE0F | 0xFEFF | 0xE0100..=0xE01EF)
}

fn is_real(s: UScript) -> bool {
    !matches!(s, UScript::Common | UScript::Inherited | UScript::Unknown)
}

/// `unicode-script` 的文字 → HarfBuzz 的文字（走 ISO 15924 四字母代码）。
fn hb_script(s: UScript) -> Script {
    let name = s.short_name().as_bytes();
    if let [a, b, c, d] = *name {
        if let Some(found) = Script::from_iso15924_tag(Tag::from_bytes(&[a, b, c, d])) {
            return found;
        }
    }
    script::COMMON
}

#[cfg(test)]
mod tests {
    use super::*;

    fn garbage() -> FaceData {
        FaceData {
            bytes: Arc::new(b"not a font at all".to_vec()),
            index: 0,
        }
    }

    #[test]
    fn bytes_that_are_not_a_font_never_become_a_face_set() {
        assert!(FaceSet::new(garbage()).is_none());
        assert!(!garbage().is_usable_for('a'));
    }

    #[test]
    fn a_non_collection_file_picks_face_zero() {
        assert_eq!(pick_collection_face(b"not a font", "Segoe UI", 400), 0);
    }

    #[test]
    fn default_ignorable_chars_stay_with_their_neighbours() {
        for ch in ['\u{200C}', '\u{200D}', '\u{200F}', '\u{FE0F}'] {
            assert!(is_ignorable(ch), "{ch:?} 必须留在前后文字的段里");
        }
        assert!(!is_ignorable('a') && !is_ignorable('ب') && !is_ignorable(' '));
    }

    #[test]
    fn script_names_map_to_harfbuzz_scripts() {
        assert_eq!(hb_script(UScript::Arabic), script::ARABIC);
        assert_eq!(hb_script(UScript::Thai), script::THAI);
        assert_eq!(hb_script(UScript::Devanagari), script::DEVANAGARI);
        assert_eq!(hb_script(UScript::Common), script::COMMON);
    }
}
