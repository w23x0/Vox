//! 静态目录数据：语言、模型、音色、按键。
//!
//! 服务商元数据来自仓库根目录 `catalog/*.json`。构建脚本校验三家的方言表/模型表
//! 以及 Aliyun 的语言/音色表，再生成下面 include 的常量。前端直接导入同一份 JSON，
//! 避免两边各抄一份后悄悄漂移。
//!
//! **模型 → 方言**：每个模型在 `models[]` 里点名自己的方言；方言另外带一组
//! `match_prefixes`，用来把官方列表里带发布日期快照后缀的同族模型
//! （`…-2026-05-19`）也认成同一个方言。匹配得到方言的模型 ID 一律**保留**
//! （连快照后缀一起），匹配不到才回这家的推荐模型。

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, CapabilitySet};
use crate::settings::ModelProvider;

// --- 模型 ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ModelInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    /// 这个模型用哪个方言（`DIALECTS` 里的 id）。
    pub dialect: &'static str,
    /// 这家**当前推荐**的模型（每家恰好一个，构建期钉死）。
    pub recommended: bool,
}

/// 一条协议方言：同一族模型的会话长什么样。
///
/// 语言表、音色表、能力位**不在这里**：那三样是服务商级别的（`Settings`、界面、
/// 能力位报告都在没有方言的上下文里读它们），两条阿里云方言今天也完全一致。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DialectInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    /// 会话形状。只有阿里云有（它们的帧由 `cloud::protocol` 拼）；Gemini / GPT
    /// 的帧在各自模块里造，没有"换形状"这回事，所以是 `None`。
    pub shape: Option<SessionShape>,
    /// 模型 ID 前缀。用来把带发布日期后缀的快照模型认成同一条方言。
    pub match_prefixes: &'static [&'static str],
    /// 显式指定源文识别模型。`None` = 不发这个字段（服务端用缺省）。
    pub transcription_model: Option<&'static str>,
    /// 这条方言会说哪些语言（规范码）、每个目标语言出不出语音、规范码 → 线上码。
    pub languages: DialectLanguages,
}

/// 一条方言的语言能力。码一律是 `catalog/languages.json` 登记的规范码；
/// 只有 `wire` 里写了的才和线上码不同名。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DialectLanguages {
    pub source: &'static [&'static str],
    /// 目标语言里"语音+文字"的那些。
    pub target_audio: &'static [&'static str],
    /// 目标语言里"仅文字"的那些。
    pub target_text_only: &'static [&'static str],
    /// 规范码 → 该服务商线上码（只列不同名的）。
    pub wire: &'static [(&'static str, &'static str)],
    pub default_speak_target: &'static str,
    pub default_listen_target: &'static str,
}

/// 目标语言的输出形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetMode {
    /// 语音 + 文字。
    Audio,
    /// 仅文字（没有译音）。
    TextOnly,
}

impl DialectLanguages {
    /// 目标语言在这条方言里的输出形态；不支持返回 `None`。
    pub fn target_mode(&self, code: &str) -> Option<TargetMode> {
        if self.target_audio.contains(&code) {
            Some(TargetMode::Audio)
        } else if self.target_text_only.contains(&code) {
            Some(TargetMode::TextOnly)
        } else {
            None
        }
    }

    pub fn supports_source(&self, code: &str) -> bool {
        self.source.contains(&code)
    }

    /// 规范码 → 线上码（目标语言）。不支持的目标语言返回 `None`。
    pub fn wire_target<'a>(&self, code: &'a str) -> Option<&'a str> {
        self.target_mode(code)?;
        Some(self.wire_of(code))
    }

    /// 规范码 → 线上码（源语言）。不支持的源语言返回 `None`。
    pub fn wire_source<'a>(&self, code: &'a str) -> Option<&'a str> {
        self.supports_source(code).then(|| self.wire_of(code))
    }

    fn wire_of<'a>(&self, code: &'a str) -> &'a str {
        self.wire
            .iter()
            .find(|(canonical, _)| *canonical == code)
            .map_or(code, |(_, wire)| *wire)
    }
}

/// 登记表里的一种语言。`name_en` 是 `Intl.DisplayNames` 给不出时的英文兜底名。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LanguageInfo {
    pub code: &'static str,
    pub name_en: &'static str,
}

/// 阿里云两条方言的 `session.update` 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShape {
    /// qwen3.8：音频格式与音色收在 `audio.input` / `audio.output` 下，模态叫
    /// `output_modalities`。
    QwenNestedAudio,
    /// qwen3.5：音色平铺，音频格式是 `input_audio_format` / `output_audio_format`。
    QwenFlatAudio,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderCapabilities {
    pub voice_selection: bool,
    pub voice_clone: bool,
    pub source_language: bool,
    pub hot_update_language: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub console_url: &'static str,
    pub default_model: &'static str,
    pub model_label: &'static str,
    pub capabilities: ProviderCapabilities,
}

include!(concat!(env!("OUT_DIR"), "/aliyun_catalog.rs"));

/// 模型名收敛：能匹配到方言就**原样保留**（含快照后缀），匹配不到回这家的推荐模型。
///
/// 收窄这一步以前是"只认表里那一个模型"，现在表里有好几条、而且官方列表里还有
/// 带发布日期后缀的同族模型，一律归到缺省等于把用户选的模型悄悄换掉。
pub fn normalize_model_for(provider: ModelProvider, name: &str) -> String {
    let info = provider_info(provider);
    match dialect_for_model(provider, name) {
        Some(_) => name.to_string(),
        None => info.default_model.to_string(),
    }
}

pub fn provider_info(provider: ModelProvider) -> &'static ProviderInfo {
    PROVIDER_INFOS
        .iter()
        .find(|info| info.id == provider.as_id())
        .unwrap_or_else(|| panic!("catalog is missing provider {}", provider.as_id()))
}

/// 这家登记在册的模型（`catalog/*.json` 的 `models[]`）。
pub fn provider_models(provider: ModelProvider) -> impl Iterator<Item = &'static ModelInfo> {
    let id = provider.as_id();
    MODELS.iter().filter(move |model| model.provider == id)
}

/// 模型 ID → 方言。先按 `models[]` 的显式 id 查，再退回方言的 `match_prefixes`
/// （官方列表里带发布日期后缀的快照模型靠这条路认）。
pub fn dialect_for_model(
    provider: ModelProvider,
    model_name: &str,
) -> Option<&'static DialectInfo> {
    // 模型 ID 会原样拼进建连 URL 的 query：只认 `[A-Za-z0-9._-]`，
    // 带 `&`、空格之类的一律当认不出（`...-realtime&x=1` 不能靠前缀混过去）。
    if !is_valid_model_id(model_name) {
        return None;
    }
    let provider_id = provider.as_id();
    // 先按 `models[]` 的显式 id，再按方言的前缀（官方列表里带发布日期后缀的
    // 快照模型表里没有，只能靠前缀认）。
    let id = MODELS
        .iter()
        .find(|model| model.provider == provider_id && model.name == model_name)
        .map(|model| model.dialect)
        .or_else(|| {
            DIALECTS
                .iter()
                .find(|dialect| dialect.provider == provider_id && claims(dialect, model_name))
                .map(|dialect| dialect.id)
        })?;
    DIALECTS
        .iter()
        .find(|dialect| dialect.id == id && dialect.provider == provider_id)
}

/// 模型 ID 的字符集：`[A-Za-z0-9._-]`，非空。
pub fn is_valid_model_id(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn claims(dialect: &DialectInfo, model_name: &str) -> bool {
    dialect
        .match_prefixes
        .iter()
        .any(|prefix| model_name.starts_with(prefix))
}

/// 同一个模型名**跨方言**地找（不看 provider）。只给 `cloud::protocol` 用：
/// 阿里云的两种方言同属一家，模型名已经足够定位。
pub fn dialect_by_prefix(model_name: &str) -> Option<&'static DialectInfo> {
    if !is_valid_model_id(model_name) {
        return None;
    }
    DIALECTS.iter().find(|dialect| claims(dialect, model_name))
}

pub fn provider_by_id(id: &str) -> Option<crate::settings::ModelProvider> {
    crate::settings::ModelProvider::ALL
        .into_iter()
        .find(|provider| provider.as_id() == id)
}

/// provider 会什么。名字与能力位表一致（`crate::capability::Capability`）。
///
/// **只有 `catalog/*.json` 的 `capabilities` 里那 4 个布尔有值**；待补的 4 位
/// （`usage_reporting` / `speech_activity` / `turn_end` / `source_transcript`）只占名、恒 `false`
/// ——未拍板的方向不落数据文件，否则等于提前把没定的东西变成契约。
pub fn provider_capabilities(provider: ModelProvider) -> CapabilitySet {
    let caps = provider_info(provider).capabilities;
    let mut set = CapabilitySet::empty();
    for (on, bit) in [
        (caps.voice_selection, Capability::VoiceSelection),
        (caps.voice_clone, Capability::VoiceClone),
        (caps.source_language, Capability::SourceLanguage),
        (caps.hot_update_language, Capability::HotUpdateLanguage),
    ] {
        if on {
            set = set.union(CapabilitySet::single(bit));
        }
    }
    set
}

/// 点名查询：这家 provider 有没有这一位。取代原来的 4 个 `supports_*` 自由函数。
pub fn supports(provider: ModelProvider, bit: Capability) -> bool {
    provider_capabilities(provider).contains(bit)
}

// --- 激活方式 --------------------------------------------------------------

/// 对外说话的激活方式。`Toggle` 按一下开、再按一下关（默认）；
/// `Hold` 按住说话，松开即停。音量阀门只在 `Toggle` 下生效。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    #[default]
    Toggle,
    Hold,
}

impl ActivationMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Toggle => "开关",
            Self::Hold => "按住说话",
        }
    }
}

// --- 查询 ------------------------------------------------------------------

/// 登记表里有没有这个规范码。
pub fn is_registered_language(code: &str) -> bool {
    LANGUAGES.iter().any(|l| l.code == code)
}

/// 把任意写法的语言码收成登记表里的规范码。认不出返回 `None`。
///
/// **只用在设置迁移上**（旧设置文件里的值）。迁移之后，运行期各处只认登记表里的
/// 规范码原样（[`is_registered_language`]），不再宽容别的写法。
///
/// 规则（按顺序）：去空白、转小写、`_` 换 `-`；整串是登记码就收下；整串是别名
/// （`no`→`nb`、`tl`→`fil`、`iw`→`he`…）就换；否则取主语言子标签（`ja-JP`→`ja`、
/// `zh-TW`→`zh`、`en_US`→`en`）再按前两条找。书写系统/区域差异登记表没有单独登记，
/// 一律归到主语言。
pub fn canonical_language(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_ascii_lowercase().replace('_', "-");
    let direct = |k: &str| -> Option<&'static str> {
        LANGUAGES
            .iter()
            .find(|l| l.code == k)
            .map(|l| l.code)
            .or_else(|| {
                LANGUAGE_ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == k)
                    .and_then(|(_, code)| LANGUAGES.iter().find(|l| l.code == *code))
                    .map(|l| l.code)
            })
    };
    direct(&key).or_else(|| {
        key.split('-')
            .next()
            .filter(|primary| *primary != key)
            .and_then(direct)
    })
}

/// 登记表级的目标语言归一：不是登记过的规范码就回阿里云缺省。只给没有方言上下文的
/// 调用方（热更新、探针）；有方言的地方用 [`normalize_speak_target`] / [`normalize_listen_target`]。
pub fn normalize_language(code: &str) -> &'static str {
    registered_code(code).unwrap_or(DEFAULT_TARGET_LANGUAGE)
}

fn registered_code(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|l| l.code == code).map(|l| l.code)
}

/// 阿里云推荐模型的方言下，这个目标语言出不出译音。给没有模型上下文的调用方
/// （探针）；会话里的判断走 `SessionParams::audio_enabled`，看自己模型的方言。
pub fn supports_audio_output(language: &str) -> bool {
    dialect_by_prefix(provider_info(ModelProvider::Aliyun).default_model)
        .is_some_and(|d| d.languages.target_mode(language) == Some(TargetMode::Audio))
}

/// 对外说话的目标语言：规范化后必须是这个模型的方言支持的目标语言，否则回该方言缺省。
pub fn normalize_speak_target(provider: ModelProvider, model: &str, raw: &str) -> &'static str {
    normalize_target(
        provider,
        model,
        raw,
        |l| l.default_speak_target,
        DEFAULT_TARGET_LANGUAGE,
    )
}

/// 听人说话的目标语言，规则同上，缺省取方言的听取缺省。
pub fn normalize_listen_target(provider: ModelProvider, model: &str, raw: &str) -> &'static str {
    normalize_target(
        provider,
        model,
        raw,
        |l| l.default_listen_target,
        LISTEN_TARGET_LANGUAGE,
    )
}

fn normalize_target(
    provider: ModelProvider,
    model: &str,
    raw: &str,
    pick_default: fn(&DialectLanguages) -> &'static str,
    no_dialect_default: &'static str,
) -> &'static str {
    let canonical = registered_code(raw);
    match dialect_for_model(provider, model) {
        Some(dialect) => canonical
            .filter(|code| dialect.languages.target_mode(code).is_some())
            .unwrap_or_else(|| pick_default(&dialect.languages)),
        // 认不出方言的模型设置阶段就被归一掉了；万一到这里，只做登记表级判断。
        None => canonical.unwrap_or(no_dialect_default),
    }
}

/// 源语言（听人说话）：规范化后必须是这个模型的方言支持的源语言，否则 `None`（自动识别）。
/// 宁可不锁识别，也不把服务端不认的码发上去换一次报错。
pub fn normalize_source(provider: ModelProvider, model: &str, raw: &str) -> Option<&'static str> {
    let canonical = registered_code(raw)?;
    dialect_for_model(provider, model)?
        .languages
        .supports_source(canonical)
        .then_some(canonical)
}

/// 音色 id 的显示名。只有 [`ordered_voices`] 用，不对外暴露。
fn voice_label(id: &str) -> Option<&'static str> {
    VOICE_LABELS.iter().find(|(v, _)| *v == id).map(|(_, l)| *l)
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceOption {
    pub id: String,
    pub label: String,
    pub recommended: bool,
}

/// 音色列表。官方默认音色排第一，其余保持维护表顺序。
///
/// `current` 若不在列表里会被钉到最前，保证以前存过的自定义音色不会从菜单里消失。
pub fn ordered_voices(_language: &str, current: Option<&str>) -> Vec<VoiceOption> {
    let mut ids: Vec<&str> = Vec::with_capacity(VOICE_LABELS.len() + 1);
    for (id, _) in VOICE_LABELS {
        ids.push(id);
    }
    if let Some(cur) = current {
        if !cur.is_empty() && !ids.contains(&cur) {
            ids.insert(0, cur);
        }
    }
    ids.into_iter()
        .map(|id| VoiceOption {
            id: id.to_string(),
            label: voice_label(id).unwrap_or(id).to_string(),
            recommended: id == DEFAULT_VOICE,
        })
        .collect()
}

pub fn voice_available(_language: &str, voice: &str) -> bool {
    !voice.is_empty() && !LEGACY_REMOVED_VOICE_IDS.contains(&voice)
}

pub fn default_voice_for_language(_language: &str) -> String {
    DEFAULT_VOICE.to_string()
}

// --- 按键 ------------------------------------------------------------------

/// 合法键名返回规范写法（如 "v" → "V"），非法返回 `None`。
pub fn normalize_key(name: &str) -> Option<String> {
    let upper = name.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return None;
    }
    if upper.len() == 1 {
        let c = upper.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(upper);
        }
        return None;
    }
    match upper.as_str() {
        "SPACE" => Some("Space".to_string()),
        "XBUTTON1" => Some("XButton1".to_string()),
        "XBUTTON2" => Some("XButton2".to_string()),
        _ => upper
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=12).contains(n))
            .map(|n| format!("F{n}")),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyOption {
    pub id: String,
    pub label: String,
}

/// 非字母数字键的显示名。UI 的按键枚举（[`key_options`]）和显示当前绑定的
/// [`crate::hotkey::Hotkey::label`] 必须说同一个词，所以这张小表只留这一份。
pub(crate) const SPECIAL_KEY_LABELS: &[(&str, &str)] = &[
    ("Space", "空格"),
    ("XButton1", "鼠标侧键1"),
    ("XButton2", "鼠标侧键2"),
];

/// 供 UI 枚举的按键选项。
pub fn key_options() -> Vec<KeyOption> {
    let mut out = Vec::new();
    let mut push = |id: &str, label: &str| {
        out.push(KeyOption {
            id: id.to_string(),
            label: label.to_string(),
        })
    };
    for c in 'A'..='Z' {
        push(&c.to_string(), &c.to_string());
    }
    for c in '0'..='9' {
        push(&c.to_string(), &c.to_string());
    }
    for n in 1..=12 {
        push(&format!("F{n}"), &format!("F{n}"));
    }
    for (id, label) in SPECIAL_KEY_LABELS {
        push(id, label);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_lookup_is_id_driven_and_covers_third_provider() {
        assert_eq!(
            provider_by_id("gpt"),
            Some(crate::settings::ModelProvider::Gpt)
        );
        for provider in crate::settings::ModelProvider::ALL {
            assert_eq!(
                provider_info(provider).id,
                provider.as_id(),
                "catalog id must match the settings provider id"
            );
            assert_eq!(provider_by_id(provider.as_id()), Some(provider));
        }
        assert_eq!(provider_by_id("missing"), None);
    }

    #[test]
    fn gpt_catalog_points_to_the_realtime_translation_model() {
        let info = provider_info(crate::settings::ModelProvider::Gpt);
        assert_eq!(info.default_model, "gpt-realtime-translate");
        assert_eq!(info.model_label, "GPT Realtime Translate");
        assert_eq!(info.default_model, GPT_MODEL_NAME);
        assert_eq!(info.model_label, GPT_MODEL_LABEL);
        assert_eq!(GPT_INPUT_SAMPLE_RATE, 24_000);
        assert_eq!(GPT_OUTPUT_SAMPLE_RATE, 24_000);
        assert!(!info.capabilities.voice_selection);
        assert!(!info.capabilities.voice_clone);
        assert!(!info.capabilities.source_language);
        assert!(info.capabilities.hot_update_language);
    }

    #[test]
    fn every_model_points_at_a_dialect_that_claims_it() {
        for provider in ModelProvider::ALL {
            let models: Vec<_> = provider_models(provider).collect();
            assert!(!models.is_empty(), "{provider:?} 一个模型都没登记");
            assert_eq!(
                models.iter().filter(|m| m.recommended).count(),
                1,
                "{provider:?} 必须有且只有一个推荐模型"
            );
            for model in models {
                let dialect = dialect_for_model(provider, model.name)
                    .unwrap_or_else(|| panic!("{} 匹配不到方言", model.name));
                assert_eq!(dialect.provider, provider.as_id());
                assert!(
                    dialect
                        .match_prefixes
                        .iter()
                        .any(|prefix| model.name.starts_with(prefix)),
                    "{} 的前缀匹配不上它自己的方言",
                    model.name
                );
            }
        }
    }

    /// 官方列表里的快照模型（`-YYYY-MM-DD` 后缀）要认成同一条方言，
    /// 并且**原样保留**整个模型 ID。
    #[test]
    fn dated_snapshots_resolve_to_their_dialect_and_are_kept() {
        let snapshot = "qwen3.5-livetranslate-flash-realtime-2026-05-19";
        let dialect = dialect_for_model(ModelProvider::Aliyun, snapshot)
            .expect("带日期后缀的同族模型要认得出来");
        assert_eq!(dialect.id, "qwen3.5");
        assert_eq!(
            dialect.shape,
            Some(SessionShape::QwenFlatAudio),
            "3.5 走平铺音频格式那条形状"
        );
        assert_eq!(
            normalize_model_for(ModelProvider::Aliyun, snapshot),
            snapshot
        );
    }

    #[test]
    fn model_normalization_falls_back() {
        let default = provider_info(ModelProvider::Aliyun).default_model;
        for junk in [
            "gummy-realtime-v1",
            "qwen3-livetranslate-flash-realtime",
            "qwen3-livetranslate-flash-realtime-2025-09-22",
            "nope",
            "",
        ] {
            assert_eq!(
                normalize_model_for(ModelProvider::Aliyun, junk),
                default,
                "{junk:?} 匹配不到方言，要回推荐模型"
            );
        }
        assert_eq!(
            normalize_model_for(ModelProvider::Gpt, "qwen3.8-livetranslate-flash-realtime"),
            crate::catalog::GPT_MODEL_NAME,
            "别家的模型名不能留在阿里的配置里"
        );
    }

    /// M8：明天下线的 gummy / qwen3 **不进维护表**。进了表它们就会一直被当成可选模型，
    /// 而它们的方言也匹配不上（那两个前缀不在任何方言的 match_prefixes 里）。
    #[test]
    fn retired_models_are_kept_out_of_the_catalog() {
        assert!(provider_models(ModelProvider::Aliyun)
            .all(|model| !model.name.starts_with("gummy")
                && !model.name.starts_with("qwen3-livetranslate")));
        assert!(dialect_by_prefix("gummy-realtime-v1").is_none());
        assert!(dialect_by_prefix("qwen3-livetranslate-flash-realtime").is_none());
    }

    #[test]
    fn normalization_rejects_ids_outside_the_safe_charset() {
        let provider = ModelProvider::Aliyun;
        let default = provider_info(provider).default_model;
        for junk in [
            "qwen3.5-livetranslate-flash-realtime&x=1",
            "qwen3.8-livetranslate-flash-realtime ",
            "qwen3.8-livetranslate-flash-realtime/../x",
        ] {
            assert_eq!(normalize_model_for(provider, junk), default, "{junk:?}");
        }
        // 合法的快照后缀仍然保留。
        let snapshot = "qwen3.5-livetranslate-flash-realtime-2026-05-19";
        assert_eq!(normalize_model_for(provider, snapshot), snapshot);
    }

    #[test]
    fn dialect_prefix_lookup_is_shared_by_the_two_aliyun_dialects() {
        assert_eq!(
            dialect_by_prefix("qwen3.8-livetranslate-flash-realtime").and_then(|d| d.shape),
            Some(SessionShape::QwenNestedAudio)
        );
        assert_eq!(
            dialect_by_prefix("qwen3.5-livetranslate-flash-realtime-2026-05-19")
                .and_then(|d| d.shape),
            Some(SessionShape::QwenFlatAudio)
        );
        assert!(dialect_by_prefix("gummy-realtime-v1").is_none());
        // 3.8 的识别模型留了字段但今天不发（2026-10-09 真连结论）。
        let qwen38 = dialect_by_prefix("qwen3.8-livetranslate-flash-realtime").unwrap();
        assert_eq!(qwen38.transcription_model, None);
    }

    #[test]
    fn model_normalization_keeps_every_adapted_aliyun_model() {
        for model in provider_models(ModelProvider::Aliyun) {
            assert_eq!(
                normalize_model_for(ModelProvider::Aliyun, model.name),
                model.name,
                "表里登记的模型不该被归一掉"
            );
        }
        assert_eq!(provider_models(ModelProvider::Aliyun).count(), 2);
    }

    #[test]
    fn language_normalization_falls_back_to_japanese() {
        assert_eq!(normalize_language("xx"), "ja");
        assert_eq!(normalize_language("ko"), "ko");
        assert!(supports_audio_output("ja"));
        assert!(!supports_audio_output("xx"));
    }

    #[test]
    fn default_voice_comes_first_and_custom_is_pinned() {
        let zh = ordered_voices("zh", None);
        assert_eq!(zh[0].id, DEFAULT_VOICE);
        assert!(zh[0].recommended);
        assert_eq!(zh.len(), VOICE_LABELS.len(), "不该有重复项");

        let with_custom = ordered_voices("ja", Some("MyClone"));
        assert_eq!(with_custom[0].id, "MyClone");
        assert!(!with_custom[0].recommended);

        assert_eq!(default_voice_for_language("ja"), DEFAULT_VOICE);
        assert_eq!(default_voice_for_language("th"), DEFAULT_VOICE);
        assert!(voice_available("zh", "Tina"));
        assert!(!voice_available("zh", "Zhixia"), "旧版无效音色要迁走");
        assert!(
            voice_available("zh", "my-cloned-voice"),
            "自定义复刻音色要保留"
        );
        assert!(!voice_available("zh", ""));
    }

    #[test]
    fn key_names_normalize_like_the_old_mapping() {
        // 键名是内核唯一的键标识（键码映射在平台 crate 里），所以这里只验规范化。
        assert_eq!(normalize_key("v").as_deref(), Some("V"));
        assert_eq!(normalize_key("7").as_deref(), Some("7"));
        assert_eq!(normalize_key("F1").as_deref(), Some("F1"));
        assert_eq!(normalize_key("f12").as_deref(), Some("F12"));
        assert_eq!(normalize_key("space").as_deref(), Some("Space"));
        assert_eq!(normalize_key("XButton1").as_deref(), Some("XButton1"));
        assert_eq!(normalize_key("xbutton2").as_deref(), Some("XButton2"));
        assert_eq!(normalize_key("Tab"), None, "故意不支持 Tab");
        assert_eq!(normalize_key("F13"), None);
        assert_eq!(normalize_key(""), None);
        assert_eq!(key_options().len(), 26 + 10 + 12 + 3);
    }

    // --- 语言登记表 / 方言语言映射 ---------------------------------------

    #[test]
    fn canonical_language_keeps_every_registered_code_and_folds_legacy_spellings() {
        assert_eq!(LANGUAGES.len(), 60);
        for language in LANGUAGES {
            assert_eq!(canonical_language(language.code), Some(language.code));
        }
        for (raw, want) in [
            ("ja-JP", "ja"),
            ("zh-CN", "zh"),
            ("zh-TW", "zh"),
            ("zh-Hant-TW", "zh"),
            ("EN_us", "en"),
            (" Ko ", "ko"),
            ("no", "nb"),
            ("nn", "nb"),
            ("tl", "fil"),
            ("iw", "he"),
            ("in", "id"),
            ("cmn", "zh"),
        ] {
            assert_eq!(canonical_language(raw), Some(want), "{raw:?}");
        }
        for junk in ["", "xx", "这不是语言代码", "-", "xx-JP"] {
            assert_eq!(canonical_language(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn dialect_target_modes_come_from_the_table() {
        let qwen35 = dialect_by_prefix("qwen3.5-livetranslate-flash-realtime").unwrap();
        assert_eq!(qwen35.languages.target_audio.len(), 29);
        assert_eq!(qwen35.languages.target_text_only.len(), 31);
        assert_eq!(qwen35.languages.target_mode("ja"), Some(TargetMode::Audio));
        assert_eq!(
            qwen35.languages.target_mode("yue"),
            Some(TargetMode::TextOnly)
        );
        assert_eq!(qwen35.languages.target_mode("xx"), None);

        // GPT 只有 13 个目标语言，瑞典语不在其中（阿里云的 29 个里有）。
        let gpt = dialect_by_prefix("gpt-realtime-translate").unwrap();
        assert_eq!(gpt.languages.target_audio.len(), 13);
        assert_eq!(gpt.languages.target_mode("sv"), None);
        assert_eq!(gpt.languages.target_mode("ja"), Some(TargetMode::Audio));
        assert_eq!(qwen35.languages.target_mode("sv"), Some(TargetMode::Audio));

        // 每条方言的缺省目标都在自己的目标集合里（构建期也钉死，这里防运行时数据漂移）。
        for dialect in DIALECTS {
            let l = &dialect.languages;
            assert!(
                l.target_mode(l.default_speak_target).is_some(),
                "{}",
                dialect.id
            );
            assert!(
                l.target_mode(l.default_listen_target).is_some(),
                "{}",
                dialect.id
            );
        }
    }

    #[test]
    fn wire_codes_follow_the_dialect_table_and_default_to_the_canonical_code() {
        // 真数据里三家今天都同名（wire 表为空）。
        for dialect in DIALECTS {
            assert!(dialect.languages.wire.is_empty(), "{}", dialect.id);
            assert_eq!(dialect.languages.wire_target("ja"), Some("ja"));
        }
        // 机制本身：表里写了的翻，没写的原样，方言不支持的是 None。
        let langs = DialectLanguages {
            source: &["nb", "ja", "en"],
            target_audio: &["ja"],
            target_text_only: &["nb"],
            wire: &[("nb", "no")],
            default_speak_target: "ja",
            default_listen_target: "ja",
        };
        assert_eq!(langs.wire_target("nb"), Some("no"));
        assert_eq!(langs.wire_target("ja"), Some("ja"));
        assert_eq!(langs.wire_target("en"), None, "en 只是源语言，不是目标语言");
        assert_eq!(langs.wire_source("nb"), Some("no"));
        assert_eq!(langs.wire_source("en"), Some("en"));
        assert_eq!(langs.wire_source("de"), None);
    }

    #[test]
    fn switching_model_falls_back_to_the_new_dialects_default() {
        let aliyun = (
            ModelProvider::Aliyun,
            "qwen3.8-livetranslate-flash-realtime",
        );
        let gpt = (ModelProvider::Gpt, "gpt-realtime-translate");
        // 粤语阿里云能当目标（仅文字），GPT 不支持 → 回 GPT 方言的缺省。
        assert_eq!(normalize_speak_target(aliyun.0, aliyun.1, "yue"), "yue");
        assert_eq!(normalize_speak_target(gpt.0, gpt.1, "yue"), "ja");
        assert_eq!(normalize_listen_target(gpt.0, gpt.1, "yue"), "zh");
        assert_eq!(normalize_speak_target(gpt.0, gpt.1, "ko"), "ko");
        // 运行期只认规范码原样：别的写法归迁移管，到这里就当认不出。
        assert_eq!(normalize_speak_target(gpt.0, gpt.1, "ko-KR"), "ja");
        // 源语言：阿里云 60 种都行，认不出的回自动识别。
        assert_eq!(normalize_source(aliyun.0, aliyun.1, "en"), Some("en"));
        assert_eq!(normalize_source(aliyun.0, aliyun.1, "EN_us"), None);
        assert_eq!(normalize_source(aliyun.0, aliyun.1, "xx"), None);
    }
}
