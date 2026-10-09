use std::{collections::HashSet, env, fs, path::PathBuf};

use serde::Deserialize;

/// catalog JSON 里 label/name/description 的多语对象。构建期前端只用到展示，
/// 后端仅取 zh 作内部常量（前端自行按 UI 语言从 JSON 取）。
#[derive(Deserialize)]
struct L10n {
    zh: String,
    en: String,
    ja: String,
}

#[derive(Deserialize)]
struct Catalog {
    schema_version: i64,
    verified_at: String,
    expected_counts: ExpectedCounts,
    provider: Provider,
    dialects: std::collections::BTreeMap<String, Dialect>,
    models: Vec<CatalogModel>,
    api: Api,
    capabilities: Capabilities,
    defaults: Defaults,
    legacy_removed_voice_ids: Vec<String>,
    voices: Vec<Voice>,
}

/// `catalog/languages.json`：统一语言登记表。
#[derive(Deserialize)]
struct Registry {
    languages: Vec<RegistryLanguage>,
    aliases: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RegistryLanguage {
    code: String,
    en: String,
}

/// 语言集合：显式列表，或 `"all_registered"`（登记表全集——官方没给逐个代码清单的
/// 服务商用它，构建期展开成登记表当时的全部代码）。
#[derive(Deserialize)]
#[serde(untagged)]
enum CodeSet {
    Keyword(String),
    Codes(Vec<String>),
}

impl CodeSet {
    fn resolve(&self, registry: &Registry, what: &str) -> Vec<String> {
        match self {
            CodeSet::Codes(codes) => codes.clone(),
            CodeSet::Keyword(word) => {
                assert_eq!(
                    word, "all_registered",
                    "{what}：未知的语言集合关键字 {word:?}"
                );
                registry.languages.iter().map(|l| l.code.clone()).collect()
            }
        }
    }
}

/// 一条方言的语言能力：源语言集合、目标语言集合（语音+文字 / 仅文字）、
/// 规范码 → 该服务商线上码（只写不同名的）、两条流水线的缺省目标语言。
#[derive(Deserialize)]
struct DialectLanguages {
    source: CodeSet,
    target_audio: CodeSet,
    target_text_only: CodeSet,
    #[serde(default)]
    wire: std::collections::BTreeMap<String, String>,
    default_speak_target: String,
    default_listen_target: String,
    /// `[未核实]` 说明，只给人看。
    #[serde(default)]
    #[allow(dead_code)]
    unverified: Vec<String>,
}

/// `catalog/*.json` 里的一个方言。
///
/// `session_shape` / `transcription_model` 只有阿里云用得到（它们的会话由
/// `cloud::protocol` 拼），Gemini / GPT 的帧在自己的模块里造，所以是可选的。
#[derive(Deserialize)]
struct Dialect {
    label: L10n,
    #[serde(default)]
    session_shape: Option<String>,
    #[serde(default)]
    match_prefixes: Vec<String>,
    #[serde(default)]
    transcription_model: Option<String>,
    languages: DialectLanguages,
}

/// `catalog/*.json` `models[]` 里的一条。**表里只放官方接口给不了的东西**：
/// 发布/下线时间、价格一律不进这里（那些走官方模型列表接口）。
#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    dialect: String,
    #[serde(default)]
    recommended: bool,
    label: L10n,
}

#[derive(Deserialize)]
struct ExpectedCounts {
    voices: usize,
    qwen_target_audio: usize,
    qwen_target_text_only: usize,
}

#[derive(Deserialize)]
struct Provider {
    id: String,
    label: L10n,
    console_url: String,
}

#[derive(Deserialize)]
struct Api {
    legacy_endpoint: String,
    /// 国际站（新加坡）地址：国际站的 API Key 只认这一个。
    intl_endpoint: String,
}

#[derive(Deserialize)]
struct Capabilities {
    voice_selection: bool,
    voice_clone: bool,
    source_language: bool,
    hot_update_language: bool,
}

#[derive(Deserialize)]
struct Defaults {
    voice: String,
}

#[derive(Deserialize)]
struct Voice {
    id: String,
    name: L10n,
    description: L10n,
}

#[derive(Deserialize)]
struct MiniCatalog {
    schema_version: i64,
    verified_at: String,
    provider: Provider,
    dialects: std::collections::BTreeMap<String, Dialect>,
    models: Vec<CatalogModel>,
    api: MiniApi,
    capabilities: Capabilities,
}

#[derive(Deserialize)]
struct MiniApi {
    websocket_endpoint: String,
    #[serde(default)]
    input_sample_rate: Option<u32>,
    #[serde(default)]
    output_sample_rate: Option<u32>,
}

fn lit(value: &str) -> String {
    format!("{value:?}")
}

/// 多语字段三语都要有内容——缺一种语言前端就会显示空白。
fn l10n_filled(label: &L10n) -> bool {
    !label.zh.is_empty() && !label.en.is_empty() && !label.ja.is_empty()
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest.join("../../catalog/aliyun.json");
    let gemini_source = manifest.join("../../catalog/gemini.json");
    let gpt_source = manifest.join("../../catalog/gpt.json");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", gemini_source.display());
    println!("cargo:rerun-if-changed={}", gpt_source.display());
    let registry_source = manifest.join("../../catalog/languages.json");
    println!("cargo:rerun-if-changed={}", registry_source.display());
    let registry_raw =
        fs::read_to_string(&registry_source).expect("读取 catalog/languages.json 失败");
    let registry: Registry =
        serde_json::from_str(&registry_raw).expect("catalog/languages.json 格式不合法");

    let raw = fs::read_to_string(&source).expect("读取 catalog/aliyun.json 失败");
    let catalog: Catalog = serde_json::from_str(&raw).expect("catalog/aliyun.json 格式不合法");
    let gemini_raw = fs::read_to_string(&gemini_source).expect("读取 catalog/gemini.json 失败");
    let gemini: MiniCatalog =
        serde_json::from_str(&gemini_raw).expect("catalog/gemini.json 格式不合法");
    let gpt_raw = fs::read_to_string(&gpt_source).expect("读取 catalog/gpt.json 失败");
    let gpt: MiniCatalog = serde_json::from_str(&gpt_raw).expect("catalog/gpt.json 格式不合法");

    assert_eq!(
        catalog.provider.id, "aliyun",
        "Aliyun 目录 provider.id 必须是 aliyun"
    );
    assert!(
        catalog.schema_version >= 2,
        "catalog/aliyun.json 的 schema_version 至少应为 2（label 已扩成 {{zh,en,ja}}）"
    );
    assert!(
        gemini.schema_version >= 2,
        "catalog/gemini.json 的 schema_version 至少应为 2"
    );
    assert!(
        gpt.schema_version >= 2,
        "catalog/gpt.json 的 schema_version 至少应为 2"
    );
    validate_registry(&registry);
    for voice in &catalog.voices {
        assert!(l10n_filled(&voice.name), "音色 {} 的 name 有空值", voice.id);
        assert!(
            l10n_filled(&voice.description),
            "音色 {} 的 description 有空值",
            voice.id
        );
    }
    assert!(!catalog.provider.label.zh.is_empty(), "provider.label 空值");
    assert_eq!(
        gemini.provider.id, "gemini",
        "Gemini 目录 provider.id 必须是 gemini"
    );
    assert_eq!(gpt.provider.id, "gpt", "GPT 目录 provider.id 必须是 gpt");

    // 方言表 / 模型表：三家同构。每条模型必须指到一个存在的方言，方言的匹配前缀
    // 必须能匹配上**它自己**那条模型（否则那个前缀永远匹配不到东西，是死数据）。
    for (path, dialects, models) in [
        ("catalog/aliyun.json", &catalog.dialects, &catalog.models),
        ("catalog/gemini.json", &gemini.dialects, &gemini.models),
        ("catalog/gpt.json", &gpt.dialects, &gpt.models),
    ] {
        assert!(!models.is_empty(), "{path} 的 models[] 是空的");
        for (id, dialect) in dialects {
            validate_dialect_languages(path, id, dialect, &registry);
        }
        for model in models {
            let dialect = dialects.get(&model.dialect).unwrap_or_else(|| {
                panic!(
                    "{path}：模型 {} 指向的方言 {:?} 不存在",
                    model.id, model.dialect
                )
            });
            assert!(
                l10n_filled(&model.label),
                "{path}：模型 {} 的多语 label 有空值",
                model.id
            );
            assert!(
                dialect
                    .match_prefixes
                    .iter()
                    .any(|p| model.id.starts_with(p.as_str())),
                "{path}：模型 {} 的 id 不匹配它自己的方言 {:?} 的任何 match_prefixes",
                model.id,
                model.dialect
            );
            if let Some(shape) = &dialect.session_shape {
                assert!(
                    matches!(shape.as_str(), "qwen_nested_audio" | "qwen_flat_audio"),
                    "{path}：未知的 session_shape {shape:?}"
                );
            }
        }
        assert_eq!(
            models.iter().filter(|m| m.recommended).count(),
            1,
            "{path}：必须有且只有一个 recommended 模型"
        );
    }

    let aliyun_default = recommended(catalog.models.iter(), "catalog/aliyun.json");
    // 首次启动种子：阿里云推荐模型所属方言的缺省语言。
    let aliyun_langs = &catalog.dialects[&aliyun_default.dialect].languages;
    let gemini_default = recommended(gemini.models.iter(), "catalog/gemini.json");
    let gpt_default = recommended(gpt.models.iter(), "catalog/gpt.json");
    assert_eq!(
        gemini_default.id, "gemini-3.5-live-translate-preview",
        "Gemini Live Translation 模型 ID 发生变化时必须同步协议测试"
    );
    assert_eq!(
        gpt_default.id, "gpt-realtime-translate",
        "GPT 目录模型 ID 必须是 gpt-realtime-translate"
    );
    assert_eq!(
        gpt.api.websocket_endpoint, "wss://api.openai.com/v1/realtime/translations",
        "GPT WebSocket 必须使用 Realtime Translation 官方端点"
    );
    assert_eq!(
        gpt.api.input_sample_rate,
        Some(24000),
        "GPT Realtime Translate 输入音频必须是 24 kHz PCM16"
    );
    assert_eq!(
        gpt.api.output_sample_rate,
        Some(24000),
        "GPT Realtime Translate 输出音频声明为 24 kHz PCM16"
    );
    assert!(
        !gpt.capabilities.voice_selection && !gpt.capabilities.voice_clone,
        "GPT Realtime Translate 目录暂不支持固定音色或复刻"
    );
    assert!(
        !gpt.capabilities.source_language,
        "GPT Realtime Translate 目录暂不暴露源语言选择"
    );
    assert!(
        gpt.capabilities.hot_update_language,
        "GPT Realtime Translate 支持 session.update 热改目标语言"
    );
    assert!(
        gemini
            .api
            .websocket_endpoint
            .starts_with("wss://generativelanguage.googleapis.com/"),
        "Gemini WebSocket 必须使用官方 generativelanguage 端点"
    );
    for id in ["qwen3.8", "qwen3.5"] {
        let langs = &catalog.dialects[id].languages;
        assert_eq!(
            langs.target_audio.resolve(&registry, id).len(),
            catalog.expected_counts.qwen_target_audio,
            "{id}：语音+文字目标语言数量与 expected_counts 不一致"
        );
        assert_eq!(
            langs.target_text_only.resolve(&registry, id).len(),
            catalog.expected_counts.qwen_target_text_only,
            "{id}：仅文字目标语言数量与 expected_counts 不一致"
        );
    }
    assert_eq!(
        catalog.voices.len(),
        catalog.expected_counts.voices,
        "音色数量与 expected_counts 不一致"
    );
    assert!(
        catalog
            .voices
            .iter()
            .any(|v| v.id == catalog.defaults.voice),
        "默认音色不在音色表"
    );

    let mut voice_ids = HashSet::new();
    assert!(
        catalog.voices.iter().all(|v| voice_ids.insert(&v.id)),
        "音色 ID 不应重复"
    );
    assert!(
        catalog
            .legacy_removed_voice_ids
            .iter()
            .all(|id| !voice_ids.contains(id)),
        "已移除旧音色不能同时出现在官方音色表"
    );

    let mut out = String::new();
    out.push_str(&format!(
        "pub const PROVIDER_ID: &str = {};\npub const PROVIDER_LABEL: &str = {};\npub const PROVIDER_CONSOLE_URL: &str = {};\n",
        lit(&catalog.provider.id),
        lit(&catalog.provider.label.zh),
        lit(&catalog.provider.console_url)
    ));
    out.push_str(&format!(
        "pub const GEMINI_PROVIDER_ID: &str = {};\npub const GEMINI_PROVIDER_LABEL: &str = {};\npub const GEMINI_PROVIDER_CONSOLE_URL: &str = {};\npub const GEMINI_CATALOG_VERIFIED_AT: &str = {};\npub const GEMINI_MODEL_NAME: &str = {};\npub const GEMINI_MODEL_LABEL: &str = {};\npub const GEMINI_API_BASE: &str = {};\n",
        lit(&gemini.provider.id),
        lit(&gemini.provider.label.zh),
        lit(&gemini.provider.console_url),
        lit(&gemini.verified_at),
        lit(&gemini_default.id),
        lit(&gemini_default.label.zh),
        lit(&gemini.api.websocket_endpoint)
    ));
    out.push_str(&format!(
        "pub const GPT_PROVIDER_ID: &str = {};\npub const GPT_PROVIDER_LABEL: &str = {};\npub const GPT_PROVIDER_CONSOLE_URL: &str = {};\npub const GPT_CATALOG_VERIFIED_AT: &str = {};\npub const GPT_MODEL_NAME: &str = {};\npub const GPT_MODEL_LABEL: &str = {};\npub const GPT_API_BASE: &str = {};\npub const GPT_INPUT_SAMPLE_RATE: u32 = {};\npub const GPT_OUTPUT_SAMPLE_RATE: u32 = {};\n",
        lit(&gpt.provider.id),
        lit(&gpt.provider.label.zh),
        lit(&gpt.provider.console_url),
        lit(&gpt.verified_at),
        lit(&gpt_default.id),
        lit(&gpt_default.label.zh),
        lit(&gpt.api.websocket_endpoint),
        gpt.api.input_sample_rate.unwrap_or(24_000),
        gpt.api.output_sample_rate.unwrap_or(24_000),
    ));
    out.push_str(&format!(
        "pub const CATALOG_VERIFIED_AT: &str = {};\npub const DEFAULT_MODEL_NAME: &str = {};\npub const DEFAULT_MODEL_LABEL: &str = {};\npub const API_BASE: &str = {};\npub const API_BASE_INTL: &str = {};\n",
        lit(&catalog.verified_at),
        lit(&aliyun_default.id),
        lit(&aliyun_default.label.zh),
        lit(&catalog.api.legacy_endpoint),
        lit(&catalog.api.intl_endpoint)
    ));
    out.push_str(&format!(
        "pub const DEFAULT_TARGET_LANGUAGE: &str = {};\npub const LISTEN_TARGET_LANGUAGE: &str = {};\npub const DEFAULT_VOICE: &str = {};\n",
        lit(&aliyun_langs.default_speak_target),
        lit(&aliyun_langs.default_listen_target),
        lit(&catalog.defaults.voice)
    ));

    out.push_str("pub const PROVIDER_INFOS: &[ProviderInfo] = &[\n");
    push_provider_info(
        &mut out,
        &catalog.provider,
        &aliyun_default.id,
        &aliyun_default.label.zh,
        &catalog.capabilities,
    );
    push_provider_info(
        &mut out,
        &gemini.provider,
        &gemini_default.id,
        &gemini_default.label.zh,
        &gemini.capabilities,
    );
    push_provider_info(
        &mut out,
        &gpt.provider,
        &gpt_default.id,
        &gpt_default.label.zh,
        &gpt.capabilities,
    );
    out.push_str("];\n");

    out.push_str("pub const MODELS: &[ModelInfo] = &[\n");
    for (provider_id, models) in [
        (catalog.provider.id.as_str(), &catalog.models),
        (gemini.provider.id.as_str(), &gemini.models),
        (gpt.provider.id.as_str(), &gpt.models),
    ] {
        for model in models {
            out.push_str(&format!(
                "    ModelInfo {{ name: {}, label: {}, provider: {}, dialect: {}, recommended: {} }},\n",
                lit(&model.id),
                lit(&model.label.zh),
                lit(provider_id),
                lit(&model.dialect),
                model.recommended
            ));
        }
    }
    out.push_str("];\n");

    out.push_str("pub const DIALECTS: &[DialectInfo] = &[\n");
    for (provider_id, dialects) in [
        (catalog.provider.id.as_str(), &catalog.dialects),
        (gemini.provider.id.as_str(), &gemini.dialects),
        (gpt.provider.id.as_str(), &gpt.dialects),
    ] {
        for (id, dialect) in dialects {
            out.push_str(&format!(
                "    DialectInfo {{ id: {}, label: {}, provider: {}, shape: {}, match_prefixes: &[{}], transcription_model: {}, languages: {} }},\n",
                lit(id),
                lit(&dialect.label.zh),
                lit(provider_id),
                dialect
                    .session_shape
                    .as_deref()
                    .map(shape_variant)
                    .unwrap_or("None"),
                dialect
                    .match_prefixes
                    .iter()
                    .map(|p| lit(p))
                    .collect::<Vec<_>>()
                    .join(", "),
                dialect
                    .transcription_model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty())
                    .map_or_else(|| "None".to_string(), lit),
                dialect_languages_literal(id, &dialect.languages, &registry)
            ));
        }
    }
    out.push_str("];\n");
    out.push_str("pub const LANGUAGES: &[LanguageInfo] = &[\n");
    for language in &registry.languages {
        out.push_str(&format!(
            "    LanguageInfo {{ code: {}, name_en: {} }},\n",
            lit(&language.code),
            lit(&language.en)
        ));
    }
    out.push_str("];\n");
    out.push_str("pub const LANGUAGE_ALIASES: &[(&str, &str)] = &[\n");
    for (alias, code) in &registry.aliases {
        out.push_str(&format!("    ({}, {}),\n", lit(alias), lit(code)));
    }
    out.push_str("];\n");

    out.push_str("pub const VOICE_LABELS: &[(&str, &str)] = &[\n");
    for voice in &catalog.voices {
        let label = format!("{}（{}）", voice.name.zh, voice.description.zh);
        out.push_str(&format!("    ({}, {}),\n", lit(&voice.id), lit(&label)));
    }
    out.push_str("];\n");

    out.push_str("pub const LEGACY_REMOVED_VOICE_IDS: &[&str] = &[\n");
    for id in &catalog.legacy_removed_voice_ids {
        out.push_str(&format!("    {},\n", lit(id)));
    }
    out.push_str("];\n");

    let output = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("aliyun_catalog.rs");
    fs::write(output, out).expect("写入生成目录失败");
}

fn push_provider_info(
    out: &mut String,
    provider: &Provider,
    model_id: &str,
    model_label: &str,
    capabilities: &Capabilities,
) {
    out.push_str(&format!(
        "    ProviderInfo {{ id: {}, label: {}, console_url: {}, default_model: {}, model_label: {}, capabilities: ProviderCapabilities {{ voice_selection: {}, voice_clone: {}, source_language: {}, hot_update_language: {} }} }},\n",
        lit(&provider.id),
        lit(&provider.label.zh),
        lit(&provider.console_url),
        lit(model_id),
        lit(model_label),
        capabilities.voice_selection,
        capabilities.voice_clone,
        capabilities.source_language,
        capabilities.hot_update_language,
    ));
}

/// 这家唯一的推荐模型。构建期就钉死"必须有一个"，所以运行时拿到的一定是它。
fn recommended<'a>(models: impl Iterator<Item = &'a CatalogModel>, path: &str) -> &'a CatalogModel {
    let mut recommended = models.filter(|model| model.recommended);
    let first = recommended
        .next()
        .unwrap_or_else(|| panic!("{path} 没有任何 recommended 模型"));
    assert!(
        recommended.next().is_none(),
        "{path} 有多个 recommended 模型"
    );
    first
}

/// `catalog/*.json` 的 `session_shape` 字符串 → 生成代码里的枚举变体。
fn shape_variant(shape: &str) -> &'static str {
    match shape {
        "qwen_nested_audio" => "Some(SessionShape::QwenNestedAudio)",
        "qwen_flat_audio" => "Some(SessionShape::QwenFlatAudio)",
        other => panic!("未知的 session_shape {other:?}"),
    }
}

/// 登记表自检：码不重复、en 兜底名非空、别名键规范（小写、`-`）且指向登记过的码。
fn validate_registry(registry: &Registry) {
    let mut seen = HashSet::new();
    for language in &registry.languages {
        assert!(
            !language.en.is_empty(),
            "语言 {} 的 en 兜底名为空",
            language.code
        );
        assert!(
            seen.insert(language.code.as_str()),
            "语言代码重复：{}",
            language.code
        );
    }
    for (alias, code) in &registry.aliases {
        assert_eq!(
            alias,
            &alias.to_ascii_lowercase().replace('_', "-"),
            "别名键 {alias:?} 必须是小写且用 `-`"
        );
        assert!(
            seen.contains(code.as_str()),
            "别名 {alias:?} 指向未登记的码 {code:?}"
        );
        assert!(
            !seen.contains(alias.as_str()),
            "别名 {alias:?} 与登记码冲突"
        );
    }
}

/// 一条方言的语言表自检：全部在登记表里、集合内不重复、语音/仅文字两组不相交、
/// 缺省目标在各自的目标集合里、wire 表的键是本方言能说的码。
fn validate_dialect_languages(path: &str, id: &str, dialect: &Dialect, registry: &Registry) {
    let what = format!("{path} 方言 {id}");
    let registered: HashSet<&str> = registry.languages.iter().map(|l| l.code.as_str()).collect();
    let langs = &dialect.languages;
    let source = langs.source.resolve(registry, &what);
    let audio = langs.target_audio.resolve(registry, &what);
    let text = langs.target_text_only.resolve(registry, &what);
    for (name, set) in [
        ("source", &source),
        ("target_audio", &audio),
        ("target_text_only", &text),
    ] {
        let mut seen = HashSet::new();
        for code in set {
            assert!(
                registered.contains(code.as_str()),
                "{what}：{name} 里的 {code:?} 不在登记表"
            );
            assert!(
                seen.insert(code.as_str()),
                "{what}：{name} 里 {code:?} 重复"
            );
        }
    }
    assert!(
        audio.iter().all(|c| !text.contains(c)),
        "{what}：同一个码不能既是语音+文字又是仅文字"
    );
    assert!(
        !audio.is_empty() || !text.is_empty(),
        "{what}：目标语言集合是空的"
    );
    for (kind, default) in [
        ("default_speak_target", &langs.default_speak_target),
        ("default_listen_target", &langs.default_listen_target),
    ] {
        assert!(
            audio.contains(default) || text.contains(default),
            "{what}：{kind} {default:?} 不在目标语言集合里"
        );
    }
    for (canonical, wire) in &langs.wire {
        assert!(
            source.contains(canonical) || audio.contains(canonical) || text.contains(canonical),
            "{what}：wire 表的 {canonical:?} 不是本方言支持的码"
        );
        assert!(
            !wire.is_empty(),
            "{what}：wire 表里 {canonical:?} 的线上码为空"
        );
    }
}

fn codes_literal(codes: &[String]) -> String {
    format!(
        "&[{}]",
        codes.iter().map(|c| lit(c)).collect::<Vec<_>>().join(", ")
    )
}

fn dialect_languages_literal(id: &str, langs: &DialectLanguages, registry: &Registry) -> String {
    let wire = langs
        .wire
        .iter()
        .map(|(canonical, wire)| format!("({}, {})", lit(canonical), lit(wire)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "DialectLanguages {{ source: {}, target_audio: {}, target_text_only: {}, wire: &[{}], default_speak_target: {}, default_listen_target: {} }}",
        codes_literal(&langs.source.resolve(registry, id)),
        codes_literal(&langs.target_audio.resolve(registry, id)),
        codes_literal(&langs.target_text_only.resolve(registry, id)),
        wire,
        lit(&langs.default_speak_target),
        lit(&langs.default_listen_target),
    )
}
