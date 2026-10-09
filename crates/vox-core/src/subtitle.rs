//! 字幕模型：**句子序列** + 整块淡出。
//!
//! 两条流水线各占一个区域（对外说话暖白、听人说话冷白），听在上、说在下，
//! 谁也不推谁。模型只回答两件事："这条轨现在有哪些字、每个字多亮"，
//! 怎么摆、怎么上浮是 `vox-overlay-core` 的事。
//!
//! ## 为什么是"句子序列"而不是"逐字流入"
//!
//! 服务端的 delta 带的是**到目前为止的整句**，而且整句会被改译、纠错。逐字追加的
//! 模型碰上改译只能整轨 `clear()` 再从头排——屏幕上表现为整块字跳回左上重新爬。
//! 所以这里把一条轨拆成"若干已定稿句 + 一个进行中句"：
//!
//! - [`SubtitleTrack::update_sentence`] 推进进行中句：与旧整句**按字**求公共前缀，
//!   前缀内的字保留身份与 `born_ms`，其后的字换成新字；
//! - [`SubtitleTrack::finish_sentence`] 同样 diff 一次，然后把这句话定稿。
//!
//! 定稿过的句子再也不会被服务端改动，于是它后面的字一旦摆上屏就永远不挪位置。
//!
//! 时间一律用"启动至今的毫秒数"传进来，不在这里读系统时钟——测试可以直接喂时间。
//!
//! ## 0 类字
//!
//! 纯噪声/填充词/无意义发音。转写用一组成对的 [`NOISE_DELIM`] 把 0 类段夹起来；
//! 夹在中间的字标记 `is_noise`。开启 `dim_zeros` 之后这些字**始终**以 `dim_alpha`
//! 绘制，并随整块一起淡出清空（新模型没有逐字生命周期，"永久留"这个语义已删除）。

use serde::{Deserialize, Serialize};

/// 0 类段的定界符：成对出现，夹在中间的就是 0 类段。
pub const NOISE_DELIM: char = '⌀';

/// 未确认尾巴的不透明度（C3）：读者要能看出"这段还会变"。
const UNCONFIRMED_ALPHA: f32 = 0.7;

/// 哪条流水线的字幕。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Track {
    /// 对外说话（暖白）。
    Speak,
    /// 听人说话（冷白）。
    Listen,
}

/// 一个待显示的字符。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitleChar {
    pub ch: char,
    /// 这个字流入的时间（启动至今毫秒）。改译时**不**重置前缀里已有的字。
    pub born_ms: u64,
    /// 0 类字（纯噪声/填充词/无意义发音）。
    pub is_noise: bool,
    /// 服务端已经确认、不会再改的字。没确认的画 `UNCONFIRMED_ALPHA` 倍不透明度。
    pub confirmed: bool,
}

/// 渲染用的一个字：字形 + 当前不透明度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderedChar {
    pub ch: char,
    /// 0.0..=1.0。
    pub alpha: f32,
}

/// 一条字幕区域的时间参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtitleTiming {
    /// **静默多久之后**整块开始淡出（旧的"字符 Lifetime"，现在按区域算）。
    pub char_ttl_ms: u32,
    /// 整块淡出的时长。
    pub char_fade_ms: u32,
    /// "0 类字幕"开关：开了之后 0 类字按 `dim_alpha` 画。
    pub dim_zeros: bool,
    /// 0 类字的目标 alpha（0..1）。
    pub dim_alpha: f32,
}

impl Default for SubtitleTiming {
    fn default() -> Self {
        Self {
            char_ttl_ms: 2600,
            char_fade_ms: 900,
            dim_zeros: false,
            dim_alpha: 0.3,
        }
    }
}

/// 一句话。定稿之后它的字一个都不会再变。
#[derive(Debug, Clone)]
struct Sentence {
    chars: Vec<SubtitleChar>,
    /// 这句话开始时是否处在未闭合的 0 类段里（`⌀` 只来了一半）。
    start_noise: bool,
    /// 这句话扫到结尾时是否处在未闭合的 0 类段里——下一句从这里接着算。
    end_noise: bool,
    /// 与上一句之间补的那个分隔空格（`None` = 不补，见 [`needs_space_after`]）。
    lead: Option<char>,
    /// 上一句的最后一个字，用来判断两句之间要不要补空格。
    lead_prev: Option<char>,
    finalized: bool,
    /// 最近一次喂进来的整句原文（含 `⌀` 定界符），给增量入口拼整句用。
    source: String,
}

impl Sentence {
    fn new(start_noise: bool, lead_prev: Option<char>) -> Self {
        Self {
            chars: Vec::new(),
            start_noise,
            end_noise: start_noise,
            lead: None,
            lead_prev,
            finalized: false,
            source: String::new(),
        }
    }

    /// 按字做 diff：公共前缀里的字保留身份与 `born_ms`，其后的换成新字。
    fn apply_full(&mut self, full: &str, confirmed_len: usize, now_ms: u64) {
        // 两句之间要不要补一个空格，要等第一句的字到手才知道，只能在建新字之前判一次。
        if self.chars.is_empty() && self.lead.is_none() {
            let first = full.chars().find(|&c| !is_control(c) && c != NOISE_DELIM);
            if let (Some(prev), Some(first)) = (self.lead_prev, first) {
                if needs_space_after(prev, first) {
                    self.lead = Some(' ');
                }
            }
        }

        self.source.clear();
        self.source.push_str(full);
        let mut fresh: Vec<(char, bool)> = Vec::with_capacity(full.len() + 1);
        // 补出来的空格算已确认：它是我们加的，跟服务端确认与否无关。
        if let Some(lead) = self.lead {
            fresh.push((lead, false));
        }
        let mut is_noise = self.start_noise;
        for ch in full.chars() {
            if is_control(ch) {
                continue;
            }
            if ch == NOISE_DELIM {
                is_noise = !is_noise;
                continue;
            }
            fresh.push((ch, is_noise));
        }
        self.end_noise = is_noise;

        let confirmed_len = confirmed_len
            .min(fresh.len().saturating_sub(usize::from(self.lead.is_some())))
            + usize::from(self.lead.is_some());

        let common = self
            .chars
            .iter()
            .zip(&fresh)
            .take_while(|(c, (n, _))| c.ch == *n)
            .count();
        self.chars.truncate(common);
        for (i, (ch, noise)) in fresh.into_iter().enumerate().skip(common) {
            self.chars.push(SubtitleChar {
                ch,
                born_ms: now_ms,
                is_noise: noise,
                confirmed: i < confirmed_len,
            });
        }
        // 前缀里已有的字也可能刚刚被确认：确认长度会往前走。
        for (i, c) in self.chars.iter_mut().enumerate() {
            c.confirmed = i < confirmed_len;
        }
    }
}

/// 句间分隔：前后都是"按空格分词"的文字才补空格，CJK 之间不补。
fn needs_space_after(prev: char, next: char) -> bool {
    !is_wide_script(prev) && !is_wide_script(next) && !prev.is_whitespace() && !next.is_whitespace()
}

/// 换行不是字：服务端偶尔把 `\r\n` 塞进整句，它不该变成一个字形。
fn is_control(ch: char) -> bool {
    ch == '\r' || ch == '\n'
}

/// 宽字形脚本（日韩、汉字、全角标点）。这些句子之间不补空格。
fn is_wide_script(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x11FF      // 谚文字母
        | 0x2E80..=0x2EFF   // 康熙部首补充
        | 0x2F00..=0x2FDF   // 康熙部首
        | 0x2FF0..=0x303F   // 汉字部首 / CJK 标点
        | 0x3040..=0x30FF   // 平假名 / 片假名
        | 0x3100..=0x312F   // 注音符号
        | 0x3130..=0x318F   // 谚文兼容字母
        | 0x31F0..=0x31FF   // 假名补充 / 竖排标点
        | 0x3400..=0x4DBF   // 扩展 A
        | 0x4E00..=0x9FFF   // 基本汉字
        | 0xA960..=0xA97F   // 扩展 B
        | 0xAC00..=0xD7AF   // 谚文音节
        | 0xF900..=0xFAFF   // 兼容表意文字
        | 0xFE30..=0xFE4F   // 竖排标点 / 小写变体
        | 0xFF00..=0xFF60   // 全角形式
        | 0xFFE0..=0xFFE6) // 全角符号
}

/// `confirmed` 里有几个"有效字"落在 `full` 的前缀上。
///
/// 字幕轨是按**字**工作的，所以 `\r` 与 `⌀` 不算数。`confirmed` 不是 `full` 的前缀时
/// （服务端偶尔会这样），只取公共前缀——宁可少认几个已确认的字（画得暗一点），
/// 也不能把还会变的字当成已确认。
/// `None` = 服务商没给确认信息，整句按已确认处理（C3）。
pub fn confirmed_char_count(full: &str, confirmed: Option<&str>) -> usize {
    fn visible(s: &str) -> impl Iterator<Item = char> + '_ {
        s.chars().filter(|&c| !is_control(c) && c != NOISE_DELIM)
    }
    let Some(confirmed) = confirmed else {
        return visible(full).count();
    };
    visible(full)
        .zip(visible(confirmed))
        .take_while(|(a, b)| a == b)
        .count()
}

/// 单条轨道的字幕流：若干已定稿句 + 一个进行中句。
#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    sentences: Vec<Sentence>,
    timing: SubtitleTiming,
    /// 最后一次收到字幕的时间：整块淡出的计时起点。
    last_update_ms: u64,
    /// 还没收到过任何内容（跟"收到过但已经淡出清空"区分开）。
    started: bool,
}

impl SubtitleTrack {
    pub fn new(timing: SubtitleTiming) -> Self {
        Self {
            sentences: Vec::new(),
            timing,
            last_update_ms: 0,
            started: false,
        }
    }

    pub fn set_timing(&mut self, timing: SubtitleTiming) {
        self.timing = timing;
    }

    /// 推进进行中句。`full` 是**到目前为止的整句**（不是增量），
    /// `confirmed_len` 是其中已经确认、不会再改的字数。
    ///
    /// 与旧整句按**字**求公共前缀：前缀里的字保留身份与 `born_ms`，其后的字换成新字。
    /// 所以改译只影响从第一个不同的字开始往后的部分。
    pub fn update_sentence(&mut self, full: &str, confirmed_len: usize, now_ms: u64) {
        if full.is_empty() {
            return;
        }
        self.current_mut().apply_full(full, confirmed_len, now_ms);
        self.last_update_ms = now_ms;
        self.started = true;
    }

    /// 这一句说完了。同样先按字 diff 一次（终稿也可能跟进行中的不一样），然后定稿。
    pub fn finish_sentence(&mut self, final_text: &str, now_ms: u64) {
        if !final_text.is_empty() {
            self.current_mut()
                .apply_full(final_text, final_text.chars().count(), now_ms);
            self.last_update_ms = now_ms;
            self.started = true;
        }
        if let Some(sentence) = self.sentences.last_mut() {
            sentence.finalized = true;
        }
    }

    /// 进行中句最近一次收到的整句原文；没有进行中的句（空轨或上一句已定稿）= 空串。
    ///
    /// 给"只带增量"的入口（`Runtime::on_subtitle_delta`）拼出整句用。
    pub fn open_source(&self) -> &str {
        match self.sentences.last() {
            Some(s) if !s.finalized => &s.source,
            _ => "",
        }
    }

    /// 取进行中的句；没有就新起一句（带上与上一句的分隔）。
    fn current_mut(&mut self) -> &mut Sentence {
        if self.sentences.last().is_some_and(|s| !s.finalized) {
            let last = self.sentences.len() - 1;
            return &mut self.sentences[last];
        }
        let start_noise = self.sentences.last().is_some_and(|s| s.end_noise);
        let lead_prev = self
            .sentences
            .last()
            .and_then(|s| s.chars.last())
            .map(|c| c.ch);
        self.sentences.push(Sentence::new(start_noise, lead_prev));
        let last = self.sentences.len() - 1;
        &mut self.sentences[last]
    }

    /// 清空这一条轨。
    pub fn clear(&mut self) {
        self.sentences.clear();
        self.started = false;
    }

    /// 静默超过 `char_ttl_ms + char_fade_ms` 就把这条轨清空。
    ///
    /// 淡出本身由 [`SubtitleTrack::render`] 按 `now` 算——这里只管"淡完了就别留了"。
    pub fn prune(&mut self, now_ms: u64) {
        if !self.started || self.is_empty() {
            return;
        }
        if self.region_fade(now_ms) <= 0.0 {
            self.clear();
        }
    }

    /// 丢掉队首**已定稿**的句子，直到定稿部分只剩 `keep_chars` 个字。
    ///
    /// 正在说的那一句一个字都不动：它随时可能被改译，丢了就没法做公共前缀 diff。
    /// 调用方要保证 `keep_chars` 明显大于"区域里放得下的字数"，否则会删掉还看得见的字。
    /// 返回丢掉的字数。
    pub fn trim_sealed(&mut self, keep_chars: usize) -> usize {
        let sealed: usize = self
            .sentences
            .iter()
            .filter(|s| s.finalized)
            .map(|s| s.chars.len())
            .sum();
        let mut over = sealed.saturating_sub(keep_chars);
        let mut dropped = 0;
        while over > 0 {
            let Some(first) = self.sentences.first() else {
                break;
            };
            if !first.finalized {
                break;
            }
            let n = first.chars.len();
            self.sentences.remove(0);
            dropped += n;
            over = over.saturating_sub(n);
        }
        dropped
    }

    /// 整块当前的 alpha：静默够久之后线性淡到 0。
    fn region_fade(&self, now_ms: u64) -> f32 {
        let age = now_ms.saturating_sub(self.last_update_ms);
        let idle = self.timing.char_ttl_ms as u64;
        if age < idle {
            return 1.0;
        }
        let fade = self.timing.char_fade_ms as u64;
        if fade == 0 {
            return 0.0;
        }
        (1.0 - (age - idle).min(fade) as f32 / fade as f32).clamp(0.0, 1.0)
    }

    /// 当前该画的字 + 每个字的不透明度。
    ///
    /// 不透明度 = 区域淡出 × (0 类淡化) × (未确认尾巴 0.7 倍)。**没有逐字生命周期**：
    /// 字一旦摆上去，在整块淡出之前一直是不透明的。
    pub fn render(&self, now_ms: u64) -> Vec<RenderedChar> {
        let region = self.region_fade(now_ms);
        let dim = if self.timing.dim_zeros {
            self.timing.dim_alpha.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mut out = Vec::with_capacity(self.char_count());
        for sentence in &self.sentences {
            for c in &sentence.chars {
                let base = if c.is_noise { dim } else { 1.0 };
                let alpha = base * if c.confirmed { 1.0 } else { UNCONFIRMED_ALPHA };
                out.push(RenderedChar {
                    ch: c.ch,
                    alpha: (alpha * region).clamp(0.0, 1.0),
                });
            }
        }
        out
    }

    /// 当前该画的纯文本（不含透明度），给 UI 的历史面板用。
    pub fn text(&self, now_ms: u64) -> String {
        self.render(now_ms)
            .into_iter()
            .filter(|c| c.alpha > 0.0)
            .map(|c| c.ch)
            .collect()
    }

    fn char_count(&self) -> usize {
        self.sentences.iter().map(|s| s.chars.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.char_count() == 0
    }
}

/// 两条轨道合起来的字幕状态。
#[derive(Debug, Clone)]
pub struct Subtitles {
    pub speak: SubtitleTrack,
    pub listen: SubtitleTrack,
}

impl Subtitles {
    pub fn new(timing: SubtitleTiming) -> Self {
        Self {
            speak: SubtitleTrack::new(timing),
            listen: SubtitleTrack::new(timing),
        }
    }

    pub fn track_mut(&mut self, track: Track) -> &mut SubtitleTrack {
        match track {
            Track::Speak => &mut self.speak,
            Track::Listen => &mut self.listen,
        }
    }

    pub fn track(&self, track: Track) -> &SubtitleTrack {
        match track {
            Track::Speak => &self.speak,
            Track::Listen => &self.listen,
        }
    }

    pub fn set_timing(&mut self, timing: SubtitleTiming) {
        self.speak.set_timing(timing);
        self.listen.set_timing(timing);
    }

    pub fn prune(&mut self, now_ms: u64) {
        self.speak.prune(now_ms);
        self.listen.prune(now_ms);
    }

    /// 两行都空 = 悬浮窗可以完全不画。
    pub fn is_empty(&self) -> bool {
        self.speak.is_empty() && self.listen.is_empty()
    }
}

impl Default for Subtitles {
    fn default() -> Self {
        Self::new(SubtitleTiming::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing() -> SubtitleTiming {
        SubtitleTiming {
            char_ttl_ms: 1000,
            char_fade_ms: 200,
            dim_zeros: false,
            dim_alpha: 0.3,
        }
    }

    fn track() -> SubtitleTrack {
        SubtitleTrack::new(timing())
    }

    fn born(text: &str, t: &SubtitleTrack) -> Vec<u64> {
        t.sentences
            .iter()
            .flat_map(|s| s.chars.iter())
            .filter(|c| text.contains(c.ch))
            .map(|c| c.born_ms)
            .collect()
    }

    #[test]
    fn rewrite_keeps_the_identity_of_the_common_prefix() {
        let mut t = track();
        t.update_sentence("今天天气", 2, 0);
        t.update_sentence("今天天气不错", 4, 100);
        t.update_sentence("今天天气很好", 4, 200);
        let chars: Vec<(char, u64)> = t
            .sentences
            .iter()
            .flat_map(|s| s.chars.iter())
            .map(|c| (c.ch, c.born_ms))
            .collect();
        assert_eq!(
            chars,
            vec![
                ('今', 0),
                ('天', 0),
                ('天', 0),
                ('气', 0),
                ('很', 200), // 分歧点之后才重新计时
                ('好', 200),
            ],
            "分歧点之前的前缀字必须保留身份与 born_ms，之后的换成新字"
        );
    }

    #[test]
    fn rewriting_the_current_sentence_keeps_finished_sentences() {
        let mut t = track();
        t.update_sentence("第一句。", 4, 0);
        t.finish_sentence("第一句。", 0);
        t.update_sentence("第二句改", 3, 100);
        t.update_sentence("第二句改好了。", 7, 200);
        assert_eq!(
            t.text(200),
            "第一句。第二句改好了。",
            "CJK 之间不补空格，但上一句必须原封不动地留着"
        );
    }

    #[test]
    fn a_space_is_inserted_between_spaced_scripts_only() {
        let mut t = track();
        t.update_sentence("hello", 5, 0);
        t.finish_sentence("hello", 0);
        t.update_sentence("world", 5, 10);
        t.finish_sentence("world", 10);
        assert_eq!(t.text(10), "hello world");

        let mut cjk = track();
        cjk.update_sentence("你好", 2, 0);
        cjk.finish_sentence("你好", 0);
        cjk.update_sentence("世界", 2, 10);
        assert_eq!(cjk.text(10), "你好世界");
    }

    #[test]
    fn unconfirmed_tail_is_dimmed_and_brightens_when_confirmed() {
        let mut t = track();
        t.update_sentence("abcd", 2, 0);
        let r = t.render(0);
        assert_eq!(r.len(), 4);
        assert!(
            r[0].alpha == 1.0 && r[1].alpha == 1.0,
            "已确认的字是全不透明"
        );
        assert!(
            (r[2].alpha - UNCONFIRMED_ALPHA).abs() < 1e-6 && r[3].alpha == r[2].alpha,
            "未确认的尾巴要画 0.7 倍，实际 {:?}",
            &r[2..]
        );
        t.update_sentence("abcd", 4, 100);
        assert!(
            t.render(100).iter().all(|c| c.alpha == 1.0),
            "确认之后恢复全不透明"
        );
    }

    #[test]
    fn the_whole_region_fades_out_then_clears() {
        let mut t = track();
        t.update_sentence("字幕", 2, 0);
        assert!(t.render(999).iter().all(|c| c.alpha == 1.0), "静默期内不淡");
        let mid = t.render(1100)[0].alpha;
        assert!((mid - 0.5).abs() < 0.01, "淡出半程约 0.5，实际 {mid}");
        assert_eq!(t.render(1200)[0].alpha, 0.0, "淡完就该全透明");
        assert!(!t.is_empty(), "淡完但还没 prune 时，字还在模型里");
        t.prune(1300);
        assert!(t.is_empty(), "淡完再 prune 就该清空");
    }

    #[test]
    fn a_new_update_restarts_the_idle_timer() {
        let mut t = track();
        t.update_sentence("字幕", 2, 0);
        t.update_sentence("字幕啊", 3, 900);
        assert!(
            t.render(1500).iter().all(|c| c.alpha == 1.0),
            "还在静默期内"
        );
        t.prune(1900);
        assert!(!t.is_empty(), "静默期还没走完");
        t.prune(2200);
        assert!(t.is_empty(), "静默 1000 + 淡出 200 之后就该清空");
    }

    #[test]
    fn trim_sealed_only_drops_finished_sentences() {
        let mut t = track();
        for i in 0..5 {
            t.update_sentence(&format!("第{i}句。"), 4, i);
            t.finish_sentence(&format!("第{i}句。"), i);
        }
        t.update_sentence("正在说的", 4, 100);
        let dropped = t.trim_sealed(8);
        assert!(dropped > 0, "超预算就该丢掉队首的定稿句");
        assert_eq!(
            t.text(100),
            "第3句。第4句。正在说的",
            "正在说的那一句一个字都不能少"
        );
    }

    #[test]
    fn noise_chars_are_dimmed_and_fade_with_the_region() {
        let mut timing = timing();
        timing.dim_zeros = true;
        timing.dim_alpha = 0.3;
        let mut t = SubtitleTrack::new(timing);
        t.update_sentence("正⌀常⌀常", 6, 0);
        let r = t.render(0);
        assert_eq!(r.len(), 3, "⌀ 本身不显示");
        assert_eq!(r[0].alpha, 1.0);
        assert!((r[1].alpha - 0.3).abs() < 1e-6, "0 类字按 dim_alpha 画");
        assert_eq!(r[2].alpha, 1.0, "⌀ 之后的字恢复普通亮度");
        let mid = t.render(1100);
        assert!(
            (mid[1].alpha - 0.15).abs() < 0.01,
            "0 类字要跟着整块一起淡，实际 {}",
            mid[1].alpha
        );
    }

    #[test]
    fn noise_state_carries_into_the_next_sentence() {
        let mut t = track();
        t.update_sentence("⌀开始", 3, 0);
        t.finish_sentence("⌀开始", 0);
        t.update_sentence("结尾", 2, 10);
        let flags: Vec<bool> = t
            .sentences
            .iter()
            .flat_map(|s| s.chars.iter())
            .map(|c| c.is_noise)
            .collect();
        assert_eq!(flags, [true, true, true, true], "0 类段没闭合就跨到下一句");
    }

    #[test]
    fn rewrite_inside_a_sentence_reopens_the_unconfirmed_tail() {
        let mut t = track();
        t.update_sentence("abcdef", 6, 0);
        t.update_sentence("abcXef", 3, 100);
        let confirmed: Vec<bool> = t
            .sentences
            .iter()
            .flat_map(|s| s.chars.iter())
            .map(|c| c.confirmed)
            .collect();
        assert_eq!(
            confirmed,
            [true, true, true, false, false, false],
            "分歧点之后的字又变回未确认"
        );
    }

    #[test]
    fn carriage_returns_are_dropped() {
        let mut t = track();
        t.update_sentence("a\r\nb", 3, 0);
        assert_eq!(t.text(0), "ab");
    }

    #[test]
    fn clear_resets_the_track() {
        let mut t = track();
        t.update_sentence("abc", 3, 0);
        t.finish_sentence("abc", 0);
        t.clear();
        assert!(t.is_empty());
        assert!(born("a", &t).is_empty());
    }

    #[test]
    fn two_tracks_are_independent() {
        let mut s = Subtitles::new(timing());
        s.track_mut(Track::Speak).update_sentence("hello", 5, 0);
        s.track_mut(Track::Listen).update_sentence("你好", 2, 0);
        assert_eq!(s.track(Track::Speak).text(0), "hello");
        assert_eq!(s.track(Track::Listen).text(0), "你好");
        s.track_mut(Track::Speak).clear();
        assert!(s.track(Track::Speak).is_empty());
        assert!(!s.is_empty(), "只清一条，另一条还在");
        s.track_mut(Track::Listen).clear();
        assert!(s.is_empty());
    }

    #[test]
    fn wide_script_detection_covers_cjk_and_punctuation() {
        for ch in ['你', 'あ', 'ア', '한', '。', '（', '，', 'Ａ'] {
            assert!(is_wide_script(ch), "{ch} 应该算宽字形");
        }
        for ch in ['a', '1', ' ', '!', 'Ω', '—'] {
            assert!(!is_wide_script(ch), "{ch} 不该算宽字形");
        }
    }

    #[test]
    fn needs_space_between_scripts_only() {
        assert!(needs_space_after('o', 'w'));
        assert!(!needs_space_after('你', '好'));
        assert!(!needs_space_after(' ', 'w'), "左边已经是空格就不补");
        assert!(!needs_space_after('o', ' '), "右边已经是空格就不补");
    }
}
