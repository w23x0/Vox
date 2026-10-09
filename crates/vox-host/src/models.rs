//! `list_models`：从服务商**官方接口**现场拉模型清单，对上方言表，交给界面。
//!
//! 契约见 `docs/plans/UI-REDESIGN.md` §6 / `app/ui/src/types.ts` 的 `ModelList`（字段名逐字一致）。
//! 官方给的只有 id / 名字 / 发布与下线时间；协议方言、语言、能力位来自 `catalog/*.json`。
//!
//! 约束：
//! - 没有密钥不去问官方：直接给内置列表、不报错（和前端假后端同口径）。
//! - 拉失败 → 内置列表 + `error`（`auth` 401/403 / `network` 连不上 / `other`），带 `host`。
//! - 成功结果在进程内缓存 10 分钟（失败不缓存）；`refresh = true` 强制重拉。
//! - 密钥只放请求头，**不进 URL、不进错误信息、不进日志**。
//! - 阿里云列表按地区走（接入点 `aliyun_endpoint` 只影响 WebSocket）。
//! - 阻塞式 HTTP（`ureq` + rustls/ring），不引 tokio；调用方放到工作线程里跑。

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use vox_core::catalog;
use vox_core::settings::{AliyunRegion, ModelProvider};

const CACHE_TTL: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// 翻页硬上限：官方列表远小于此，防服务端回环。
const MAX_PAGES: usize = 20;

const ALIYUN_LIST_BASE_CN: &str = "https://dashscope.aliyuncs.com";
const ALIYUN_LIST_BASE_INTL: &str = "https://dashscope-intl.aliyuncs.com";
const OPENAI_LIST_URL: &str = "https://api.openai.com/v1/models";
const GEMINI_LIST_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

// --- 契约类型 ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListSource {
    Official,
    Builtin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Auth,
    Network,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelListError {
    pub kind: ErrorKind,
    pub host: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryCapabilities {
    pub voice_selection: bool,
    pub voice_clone: bool,
    pub source_language: bool,
    pub hot_update_language: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryLanguages {
    pub audio: Vec<String>,
    pub text_only: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub label: String,
    pub adapted: bool,
    pub recommended: bool,
    pub snapshot_of: Option<String>,
    pub offline_at: Option<String>,
    pub capabilities: EntryCapabilities,
    pub languages: Option<EntryLanguages>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelList {
    pub provider: ModelProvider,
    pub source: ListSource,
    pub fetched_at: Option<u64>,
    pub error: Option<ModelListError>,
    pub models: Vec<ModelEntry>,
}

// --- HTTP 抽象（测试注入假的；生产走 ureq）------------------------------------

/// 一次 GET 的结果：状态码 + 正文。传输层失败走 `Err(message)`。
pub trait HttpGet {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String>;
}

struct UreqHttp;

impl HttpGet for UreqHttp {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let mut req = agent.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut resp = req.call().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok((status, body))
    }
}

// --- 入口 -------------------------------------------------------------------

type CacheKey = (ModelProvider, String, u64);
type Cache = Mutex<BTreeMap<CacheKey, (Instant, ModelList)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 共享实现。`key` 为 `None` / 空 = 没配密钥 → 内置列表。
pub fn list_models(
    provider: ModelProvider,
    key: Option<&str>,
    region: AliyunRegion,
    refresh: bool,
) -> ModelList {
    list_models_with(
        &UreqHttp,
        cache(),
        provider,
        key,
        region,
        refresh,
        Instant::now(),
    )
}

fn list_models_with(
    http: &dyn HttpGet,
    cache: &Cache,
    provider: ModelProvider,
    key: Option<&str>,
    region: AliyunRegion,
    refresh: bool,
    now: Instant,
) -> ModelList {
    let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) else {
        return builtin_list(provider, None);
    };
    let base = base_of(provider, region);
    let cache_key: CacheKey = (provider, base.to_string(), key_fingerprint(key));
    if !refresh {
        if let Some((at, list)) = cache.lock().get(&cache_key) {
            if now.saturating_duration_since(*at) < CACHE_TTL {
                return list.clone();
            }
        }
    }
    match fetch_official(http, provider, key, base) {
        Ok(models) => {
            let list = ModelList {
                provider,
                source: ListSource::Official,
                fetched_at: Some(unix_ms()),
                error: None,
                models,
            };
            cache.lock().insert(cache_key, (now, list.clone()));
            list
        }
        Err(error) => builtin_list(provider, Some(error)),
    }
}

fn base_of(provider: ModelProvider, region: AliyunRegion) -> &'static str {
    match provider {
        ModelProvider::Aliyun => match region {
            AliyunRegion::Cn => ALIYUN_LIST_BASE_CN,
            AliyunRegion::Intl => ALIYUN_LIST_BASE_INTL,
        },
        ModelProvider::Gpt => OPENAI_LIST_URL,
        ModelProvider::Gemini => GEMINI_LIST_URL,
    }
}

/// 缓存键里区分"换了密钥"用的指纹。只在进程内存里，不落盘、不外传。
fn key_fingerprint(key: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .to_string()
}

// --- 拉取 -------------------------------------------------------------------

fn fetch_official(
    http: &dyn HttpGet,
    provider: ModelProvider,
    key: &str,
    base: &str,
) -> Result<Vec<ModelEntry>, ModelListError> {
    let host = host_of(base);
    let bearer = format!("Bearer {key}");
    let get = |url: &str, headers: &[(&str, &str)]| -> Result<Value, ModelListError> {
        let (status, body) = http.get(url, headers).map_err(|message| ModelListError {
            kind: ErrorKind::Network,
            host: host.clone(),
            message,
        })?;
        if !(200..300).contains(&status) {
            return Err(ModelListError {
                kind: classify_status(status),
                host: host.clone(),
                message: format!("HTTP {status}"),
            });
        }
        serde_json::from_str(&body).map_err(|e| ModelListError {
            kind: ErrorKind::Other,
            host: host.clone(),
            message: format!("响应不是合法 JSON：{e}"),
        })
    };
    let bad_shape = |what: &str| ModelListError {
        kind: ErrorKind::Other,
        host: host.clone(),
        message: format!("响应形状不对：{what}"),
    };

    match provider {
        ModelProvider::Aliyun => {
            let mut all = Vec::new();
            for page in 1..=MAX_PAGES {
                let url = format!(
                    "{base}/api/v1/models?capabilities=Realtime-Audio-Translate&page_size=100&page_no={page}"
                );
                let json = get(&url, &[("Authorization", &bearer)])?;
                let (total, models) =
                    parse_aliyun_page(&json).ok_or_else(|| bad_shape("缺 output.models"))?;
                let empty = models.is_empty();
                all.extend(models);
                if empty || all.len() as u64 >= total {
                    break;
                }
            }
            Ok(build_aliyun_entries(all))
        }
        ModelProvider::Gpt => {
            let json = get(base, &[("Authorization", &bearer)])?;
            let ids = parse_openai(&json).ok_or_else(|| bad_shape("缺 data[]"))?;
            Ok(simple_entries(ModelProvider::Gpt, ids))
        }
        ModelProvider::Gemini => {
            let mut all = Vec::new();
            let mut token: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!("{base}?pageSize=1000");
                if let Some(t) = &token {
                    url.push_str("&pageToken=");
                    url.push_str(t);
                }
                let json = get(&url, &[("x-goog-api-key", key)])?;
                let (ids, next) = parse_gemini(&json).ok_or_else(|| bad_shape("缺 models[]"))?;
                all.extend(ids);
                match next {
                    Some(t) => token = Some(t),
                    None => break,
                }
            }
            Ok(simple_entries(ModelProvider::Gemini, all))
        }
    }
}

fn classify_status(status: u16) -> ErrorKind {
    match status {
        401 | 403 => ErrorKind::Auth,
        _ => ErrorKind::Other,
    }
}

// --- 解析 -------------------------------------------------------------------

/// 阿里云官方列表里的一条（只留我们用的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AliyunRaw {
    id: String,
    name: Option<String>,
    equivalent_snapshot: Option<String>,
    offline_time: Option<String>,
}

/// `{"output":{"total":N,"models":[{model,name,equivalent_snapshot,
/// inference_offline_info:{offline_time}}…]}}`。形状不对返回 `None`。
fn parse_aliyun_page(json: &Value) -> Option<(u64, Vec<AliyunRaw>)> {
    let output = json.get("output")?;
    let models = output.get("models")?.as_array()?;
    let total = output.get("total").and_then(Value::as_u64).unwrap_or(0);
    let str_of = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let raws = models
        .iter()
        .filter_map(|m| {
            Some(AliyunRaw {
                id: str_of(m, "model")?,
                name: str_of(m, "name"),
                equivalent_snapshot: str_of(m, "equivalent_snapshot"),
                offline_time: m
                    .get("inference_offline_info")
                    .and_then(|i| str_of(i, "offline_time")),
            })
        })
        .collect();
    Some((total, raws))
}

/// OpenAI：`{"data":[{"id":…}]}`，id 含 `realtime-translate`。
fn parse_openai(json: &Value) -> Option<Vec<(String, String)>> {
    let data = json.get("data")?.as_array()?;
    Some(
        data.iter()
            .filter_map(|m| m.get("id")?.as_str())
            .filter(|id| id.contains("realtime-translate"))
            .map(|id| (id.to_string(), id.to_string()))
            .collect(),
    )
}

type GeminiPage = (Vec<(String, String)>, Option<String>);

/// Gemini：`{"models":[{name:"models/…",displayName,supportedGenerationMethods}],nextPageToken}`，
/// name 含 `live-translate` 且方法含 `bidiGenerateContent`。
fn parse_gemini(json: &Value) -> Option<GeminiPage> {
    let models = json.get("models")?.as_array()?;
    let ids = models
        .iter()
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            let live = m
                .get("supportedGenerationMethods")?
                .as_array()?
                .iter()
                .any(|x| x.as_str() == Some("bidiGenerateContent"));
            if !(id.contains("live-translate") && live) {
                return None;
            }
            let label = m
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(id);
            Some((id.to_string(), label.to_string()))
        })
        .collect();
    let next = json
        .get("nextPageToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some((ids, next))
}

// --- 拼条目 -----------------------------------------------------------------

/// `YYYY-MM-DD HH:MM:SS` → `YYYY-MM-DD`；形状不对返回 `None`。
fn date_part(time: &str) -> Option<String> {
    let d = time.get(..10)?;
    let ok = d.char_indices().all(|(i, c)| match i {
        4 | 7 => c == '-',
        _ => c.is_ascii_digit(),
    });
    ok.then(|| d.to_string())
}

/// `foo-2026-05-19` → `Some("foo")`。
fn strip_snapshot_suffix(id: &str) -> Option<&str> {
    let split = id.len().checked_sub(11)?;
    let (base, date) = (id.get(..split)?, id.get(split..)?);
    let date = date.strip_prefix('-')?;
    (date_part(date).as_deref() == Some(date)).then_some(base)
}

fn provider_caps(provider: ModelProvider) -> EntryCapabilities {
    let c = catalog::provider_info(provider).capabilities;
    EntryCapabilities {
        voice_selection: c.voice_selection,
        voice_clone: c.voice_clone,
        source_language: c.source_language,
        hot_update_language: c.hot_update_language,
    }
}

/// 语言明细只有阿里云的表里有（其余家只有一句"70+"，没有逐个代码）。
fn provider_languages(provider: ModelProvider) -> Option<EntryLanguages> {
    if provider != ModelProvider::Aliyun {
        return None;
    }
    let audio: Vec<String> = catalog::AUDIO_OUTPUT_LANGUAGES
        .iter()
        .map(|c| c.to_string())
        .collect();
    let text_only = catalog::LANGUAGE_LABELS
        .iter()
        .map(|(c, _)| c.to_string())
        .filter(|c| !audio.contains(c))
        .collect();
    Some(EntryLanguages { audio, text_only })
}

fn is_recommended(provider: ModelProvider, id: &str) -> bool {
    catalog::provider_models(provider).any(|m| m.name == id && m.recommended)
}

fn entry(
    provider: ModelProvider,
    id: &str,
    label: &str,
    snapshot_of: Option<String>,
    offline_at: Option<String>,
) -> ModelEntry {
    let adapted = catalog::dialect_for_model(provider, id).is_some();
    ModelEntry {
        id: id.to_string(),
        label: label.to_string(),
        adapted,
        recommended: adapted && is_recommended(provider, id),
        snapshot_of,
        offline_at,
        capabilities: if adapted {
            provider_caps(provider)
        } else {
            EntryCapabilities {
                voice_selection: false,
                voice_clone: false,
                source_language: false,
                hot_update_language: false,
            }
        },
        languages: if adapted {
            provider_languages(provider)
        } else {
            None
        },
    }
}

fn build_aliyun_entries(raws: Vec<AliyunRaw>) -> Vec<ModelEntry> {
    let ids: HashSet<&str> = raws.iter().map(|r| r.id.as_str()).collect();
    // 稳定版的 `equivalent_snapshot` 指向它当前等价的快照：反过来就是快照的 `snapshot_of`。
    let mut stable_of: BTreeMap<&str, &str> = BTreeMap::new();
    for r in &raws {
        if let Some(snap) = r.equivalent_snapshot.as_deref() {
            stable_of.insert(snap, r.id.as_str());
        }
    }
    raws.iter()
        .map(|r| {
            let snapshot_of = stable_of
                .get(r.id.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    strip_snapshot_suffix(&r.id)
                        .filter(|base| ids.contains(base))
                        .map(str::to_string)
                });
            entry(
                ModelProvider::Aliyun,
                &r.id,
                r.name.as_deref().unwrap_or(&r.id),
                snapshot_of,
                r.offline_time.as_deref().and_then(date_part),
            )
        })
        .collect()
}

fn simple_entries(provider: ModelProvider, items: Vec<(String, String)>) -> Vec<ModelEntry> {
    items
        .iter()
        .map(|(id, label)| entry(provider, id, label, None, None))
        .collect()
}

fn builtin_list(provider: ModelProvider, error: Option<ModelListError>) -> ModelList {
    ModelList {
        provider,
        source: ListSource::Builtin,
        fetched_at: None,
        error,
        models: catalog::provider_models(provider)
            .map(|m| entry(provider, m.name, m.label, None, None))
            .collect(),
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
