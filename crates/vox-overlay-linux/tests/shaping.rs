//! 复杂文字的端到端验证：真字体（系统里的 Noto 等）+ 真整形（rustybuzz）+ 真排版。
//!
//! 这里钉的是用户眼睛看得见的三件事：阿拉伯语**连写**、双向文字**方向正确**、
//! 泰语/印地语的组合字形**不拆**。系统里缺对应文字的字体时，那一条测试打印原因后跳过
//! （机器上没有字体，不是代码的错）。

use vox_core::ports::{SubtitleFrame, SubtitleLine};
use vox_core::settings::SubtitleSettings;
use vox_core::subtitle::{RenderedChar, Track};
use vox_overlay_core::render::{Cell, FrameInput, Renderer};
use vox_overlay_core::text::{GlyphSource, Shaped};
use vox_overlay_linux::font_factory;

fn font() -> Box<dyn GlyphSource> {
    font_factory()("", 32, 96).expect("系统里总该有个无衬线字体")
}

fn shape(font: &mut dyn GlyphSource, text: &str, rtl: bool) -> Shaped {
    let mut out = Shaped::default();
    font.shape(text, rtl, 0, &mut out);
    out
}

/// 这段文字的第一个字符有没有字体能显示（主字体或 fontconfig 回退）。没有就跳过该测试。
fn has_font_for(sample: char) -> bool {
    let mut f = font();
    let run = shape(f.as_mut(), &sample.to_string(), false);
    // 字形 id 0 = .notdef 豆腐块：主字体和回退字体都没有这个字。
    let covered = run.glyphs.iter().all(|g| g.id != 0);
    if !covered {
        eprintln!("跳过：系统里没有能显示 U+{:04X} 的字体", sample as u32);
    }
    covered
}

fn frame(track: Track, text: &str) -> SubtitleFrame {
    SubtitleFrame {
        lines: vec![SubtitleLine {
            track,
            origin: 0,
            chars: text
                .chars()
                .map(|ch| RenderedChar {
                    ch,
                    alpha: 1.0,
                    idle_ms: 0,
                })
                .collect(),
            color: "#ffffff".into(),
        }],
    }
}

/// 排一帧，返回每个视觉行里按显示顺序排好的簇，以及行信息。
fn layout_cells(track: Track, text: &str, width: i32) -> Vec<(bool, i32, Vec<Cell>)> {
    let settings = SubtitleSettings {
        lines_per_track: 4,
        ..SubtitleSettings::default()
    };
    let mut renderer = Renderer::new(&settings, 96, font_factory()).expect("系统里该有字体");
    let frame = frame(track, text);
    let layout = renderer.plan(&FrameInput {
        frame: &frame,
        settings: &settings,
        client_width: width,
        client_height: None,
        dpi: 96,
        now: std::time::Instant::now(),
    });
    let region = match track {
        Track::Listen => layout.listen,
        Track::Speak => layout.speak,
    }
    .expect("有字就该有区域");
    region
        .lines
        .iter()
        .map(|line| {
            (
                line.rtl,
                line.plate_x + line.plate_w,
                renderer.cells(track, line),
            )
        })
        .collect()
}

#[test]
fn arabic_letters_join_into_contextual_forms() {
    if !has_font_for('ب') {
        return;
    }
    let mut f = font();
    // 单独的 ب 是"独立形"；放进词里依次是"词首形、词中形、词中形、词尾形"。
    let alone = shape(f.as_mut(), "ب", true).glyphs[0].id;
    let word = shape(f.as_mut(), "بببب", true);
    // 簇按逻辑顺序：第 0 个簇是词首字母。
    let id_of = |k: usize| word.glyphs[word.clusters[k].glyph_start as usize].id;
    let (initial, medial_a, medial_b, fin) = (id_of(0), id_of(1), id_of(2), id_of(3));
    assert_eq!(medial_a, medial_b, "两个词中形应当是同一个字形");
    assert!(
        initial != medial_a && medial_a != fin && initial != fin,
        "词首/词中/词尾必须是三种不同的字形（连写）：{initial} {medial_a} {fin}"
    );
    assert!(
        [initial, medial_a, fin].iter().all(|id| *id != alone),
        "词里的字形都不该是独立形 {alone}（没连写就会是独立形）"
    );
}

#[test]
fn lam_alef_forms_one_ligature_cluster() {
    if !has_font_for('ل') {
        return;
    }
    let mut f = font();
    let run = shape(f.as_mut(), "لا", true);
    assert_eq!(
        run.clusters.len(),
        1,
        "لا 是一个连字，两个字符只占一个排版单元：{:?}",
        run.clusters
    );
}

#[test]
fn rtl_sentence_runs_right_to_left_and_aligns_right() {
    if !has_font_for('ب') {
        return;
    }
    let lines = layout_cells(Track::Listen, "بببب ببب ببببب", 880);
    let (rtl, plate_right, cells) = &lines[0];
    assert!(*rtl, "第一个强方向字符是阿拉伯字母，整条字幕是从右往左");
    // 逻辑上第一个簇（词首）在最右，往后依次往左。
    let mut logical = cells.clone();
    logical.sort_by_key(|c| c.first);
    for pair in logical.windows(2) {
        assert!(
            pair[0].x > pair[1].x,
            "后面的字必须在前面的字左边：{pair:?}"
        );
    }
    let right_edge = cells.iter().map(|c| c.x + c.width).max().unwrap();
    assert!(
        *plate_right - right_edge <= 20 && right_edge <= *plate_right,
        "文字右边缘贴着底衬右边缘（只留内边距）：文字 {right_edge}，底衬 {plate_right}"
    );
}

#[test]
fn mixed_text_keeps_latin_left_to_right_inside_an_arabic_line() {
    if !has_font_for('ب') {
        return;
    }
    // 整句从右往左；"ab cd" 是嵌进去的从左往右的片段，片段自己仍是 a 在 b 左边、ab 在 cd 左边。
    let text = "بب ab cd بب";
    let lines = layout_cells(Track::Listen, text, 880);
    let (_, _, cells) = &lines[0];
    let chars: Vec<char> = text.chars().collect();
    let x_of = |ch: char, nth: usize| {
        let idx = chars
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == ch)
            .nth(nth)
            .map(|(i, _)| i)
            .unwrap();
        cells
            .iter()
            .find(|c| c.first <= idx && idx < c.last)
            .unwrap()
            .x
    };
    assert!(x_of('a', 0) < x_of('b', 0), "a 在 b 左边");
    assert!(x_of('a', 0) < x_of('c', 0), "ab 在 cd 左边");
    assert!(x_of('c', 0) < x_of('d', 0), "c 在 d 左边");
    // 逻辑上第一个阿拉伯词在最右，最后一个阿拉伯词在最左。
    assert!(x_of('ب', 0) > x_of('a', 0), "开头的阿拉伯词在拉丁片段右边");
    assert!(x_of('ب', 3) < x_of('a', 0), "结尾的阿拉伯词在拉丁片段左边");
}

#[test]
fn latin_sentence_stays_left_to_right_and_left_aligned() {
    let lines = layout_cells(Track::Speak, "hello world", 880);
    let (rtl, _, cells) = &lines[0];
    assert!(!*rtl);
    let mut logical = cells.clone();
    logical.sort_by_key(|c| c.first);
    for pair in logical.windows(2) {
        assert!(pair[0].x < pair[1].x, "拉丁字母从左往右：{pair:?}");
    }
}

#[test]
fn thai_marks_stay_inside_their_cluster() {
    if !has_font_for('ส') {
        return;
    }
    let mut f = font();
    // สวัสดี = ส ว ั ส ด ี：两个附标各跟着前一个辅音，四个簇。
    let run = shape(f.as_mut(), "สวัสดี", false);
    assert_eq!(
        run.clusters.len(),
        4,
        "泰语附标不能自成一个簇：{:?}",
        run.clusters
    );
    let starts: Vec<u32> = run.clusters.iter().map(|c| c.start).collect();
    let text = "สวัสดี";
    for start in starts {
        let ch = text[start as usize..].chars().next().unwrap();
        assert!(
            !matches!(ch as u32, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E),
            "簇不能以附标 {ch:?} 开头"
        );
    }
}

#[test]
fn devanagari_conjunct_is_one_cluster() {
    if !has_font_for('क') {
        return;
    }
    let mut f = font();
    // क्ष = क + ् + ष：一个连字音节；त्रि = त + ् + र + ि：一个音节。
    let run = shape(f.as_mut(), "क्षत्रिय", false);
    assert_eq!(
        run.clusters.len(),
        3,
        "क्ष / त्रि / य 三个音节，实际 {:?}",
        run.clusters
    );
}

#[test]
fn a_char_nobody_has_draws_tofu_without_panicking() {
    let mut f = font();
    // U+10FFFD 是私用区末尾，系统字体不可能有。
    let run = shape(f.as_mut(), "a\u{10FFFD}b", false);
    assert_eq!(run.clusters.len(), 3, "三个字符三个簇，一个都不能丢");
    assert!(
        run.glyphs.iter().any(|g| g.id == 0),
        "缺字画 .notdef 豆腐块（字形 id 0）"
    );
    // 豆腐块也能取到掩码，不 panic。
    for g in &run.glyphs {
        let _ = f.glyph(g.face, g.id);
    }
}

#[test]
fn missing_script_falls_back_to_a_system_font() {
    // 主字体（缺省的无衬线 / CJK）通常没有阿拉伯字母；整形必须找到别的字体，而不是画豆腐块。
    if !has_font_for('ب') {
        return;
    }
    let mut f = font();
    let run = shape(f.as_mut(), "ب", true);
    assert!(run.glyphs[0].id != 0, "阿拉伯字母不该是豆腐块");
    assert!(
        f.glyph(run.glyphs[0].face, run.glyphs[0].id)
            .is_some_and(|g| g.w > 0 && g.cov.iter().any(|v| *v > 0)),
        "回退字体里的字形要真的有墨迹"
    );
}
