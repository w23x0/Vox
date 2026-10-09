use std::cell::RefCell;
use std::collections::VecDeque;

use serde_json::json;

use super::*;

type Seen = Vec<(String, Vec<(String, String)>)>;

/// 脚本化的假 HTTP：按顺序吐响应，并记下每次请求的 URL 与头。
struct Fake {
    script: RefCell<VecDeque<Result<(u16, String), String>>>,
    seen: RefCell<Seen>,
}

impl Fake {
    fn new(script: Vec<Result<(u16, String), String>>) -> Self {
        Self {
            script: RefCell::new(script.into()),
            seen: RefCell::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.seen.borrow().len()
    }
}

impl HttpGet for Fake {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
        self.seen.borrow_mut().push((
            url.to_string(),
            headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ));
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err("script exhausted".into()))
    }
}

fn ok(v: Value) -> Result<(u16, String), String> {
    Ok((200, v.to_string()))
}

fn fresh() -> Cache {
    Mutex::new(BTreeMap::new())
}

fn run(
    fake: &Fake,
    cache: &Cache,
    provider: ModelProvider,
    key: Option<&str>,
    region: AliyunRegion,
    refresh: bool,
) -> ModelList {
    list_models_with(fake, cache, provider, key, region, refresh, Instant::now())
}

/// 阿里云真实字段形状（`MODEL-TABLE.md` §1.1，2026-10-09 真连抓的）。
fn aliyun_sample() -> Value {
    json!({
        "request_id": "x",
        "output": {
            "total": 6,
            "models": [
                {
                    "model": "qwen3.8-livetranslate-flash-realtime",
                    "name": "Qwen3.8 LiveTranslate",
                    "description": "d", "provider": "qwen",
                    "capabilities": ["Realtime-Audio-Translate"],
                    "published_time": "2026-09-11 18:02:53",
                    "inference_metadata": {
                        "request_modality": ["Audio"], "response_modality": ["Text", "Audio"]
                    },
                    "equivalent_snapshot": null,
                    "inference_offline_info": null
                },
                {
                    "model": "qwen3.5-livetranslate-flash-realtime",
                    "name": "Qwen3.5 LiveTranslate",
                    "equivalent_snapshot": "qwen3.5-livetranslate-flash-realtime-2026-05-19",
                    "inference_offline_info": null
                },
                {
                    "model": "qwen3.5-livetranslate-flash-realtime-2026-05-19",
                    "name": "Qwen3.5 LiveTranslate 0519",
                    "equivalent_snapshot": null,
                    "inference_offline_info": null
                },
                {
                    "model": "qwen3-livetranslate-flash-realtime",
                    "name": "Qwen3 LiveTranslate",
                    "equivalent_snapshot": null,
                    "inference_offline_info": {"offline_time": "2026-10-10 00:00:00"}
                },
                {
                    "model": "qwen3-livetranslate-flash-realtime-2025-09-22",
                    "name": "Qwen3 LiveTranslate 0922",
                    "equivalent_snapshot": null,
                    "inference_offline_info": {"offline_time": "2026-10-10 00:00:00"}
                },
                {
                    "model": "gummy-realtime-v1",
                    "name": "Gummy",
                    "equivalent_snapshot": null,
                    "inference_offline_info": {"offline_time": "2026-10-10 00:00:00"}
                }
            ]
        }
    })
}

fn by_id<'a>(list: &'a ModelList, id: &str) -> &'a ModelEntry {
    list.models
        .iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

#[test]
fn aliyun_sample_is_parsed_with_real_field_names() {
    let fake = Fake::new(vec![ok(aliyun_sample())]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Aliyun,
        Some("sk-test"),
        AliyunRegion::Cn,
        false,
    );
    assert_eq!(list.source, ListSource::Official);
    assert!(list.error.is_none());
    assert!(list.fetched_at.is_some());
    assert_eq!(list.models.len(), 6);

    let top = by_id(&list, "qwen3.8-livetranslate-flash-realtime");
    assert!(top.adapted && top.recommended);
    assert_eq!(top.label, "Qwen3.8 LiveTranslate");
    assert_eq!(top.offline_at, None);
    assert!(top.languages.is_some());

    // 官方列表有、方言表没有 → 不可选
    for id in [
        "qwen3-livetranslate-flash-realtime",
        "qwen3-livetranslate-flash-realtime-2025-09-22",
        "gummy-realtime-v1",
    ] {
        let m = by_id(&list, id);
        assert!(!m.adapted, "{id}");
        assert!(!m.recommended, "{id}");
        assert_eq!(m.offline_at.as_deref(), Some("2026-10-10"), "{id}");
        assert!(m.languages.is_none());
    }
    // 快照：既认 equivalent_snapshot，也认带日期后缀的同族
    let snap = by_id(&list, "qwen3.5-livetranslate-flash-realtime-2026-05-19");
    assert!(snap.adapted, "快照后缀归到 qwen3.5 方言");
    assert_eq!(
        snap.snapshot_of.as_deref(),
        Some("qwen3.5-livetranslate-flash-realtime")
    );
    let old = by_id(&list, "qwen3-livetranslate-flash-realtime-2025-09-22");
    assert_eq!(
        old.snapshot_of.as_deref(),
        Some("qwen3-livetranslate-flash-realtime")
    );
    assert_eq!(
        by_id(&list, "qwen3-livetranslate-flash-realtime").snapshot_of,
        None
    );
}

#[test]
fn aliyun_requests_use_region_base_bearer_and_paginate() {
    let page1 = json!({"output": {"total": 2, "models": [{"model": "qwen3.8-livetranslate-flash-realtime"}]}});
    let page2 = json!({"output": {"total": 2, "models": [{"model": "gummy-realtime-v1"}]}});
    let fake = Fake::new(vec![ok(page1), ok(page2)]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Aliyun,
        Some("sk-test"),
        AliyunRegion::Intl,
        false,
    );
    assert_eq!(list.models.len(), 2);
    assert_eq!(fake.calls(), 2);
    let seen = fake.seen.borrow();
    assert_eq!(
        seen[0].0,
        "https://dashscope-intl.aliyuncs.com/api/v1/models?capabilities=Realtime-Audio-Translate&page_size=100&page_no=1"
    );
    assert!(seen[1].0.ends_with("page_no=2"));
    assert_eq!(
        seen[0].1,
        vec![("Authorization".to_string(), "Bearer sk-test".to_string())]
    );
    assert!(!seen[0].0.contains("sk-test"), "密钥不进 URL");
}

#[test]
fn aliyun_cn_region_uses_the_domestic_host() {
    // 列表只看地区，与 WebSocket 接入点（`aliyun_endpoint`）无关
    let fake = Fake::new(vec![ok(json!({"output": {"total": 0, "models": []}}))]);
    run(
        &fake,
        &fresh(),
        ModelProvider::Aliyun,
        Some("k"),
        AliyunRegion::Cn,
        false,
    );
    assert!(fake.seen.borrow()[0]
        .0
        .starts_with("https://dashscope.aliyuncs.com/api/v1/models"));
}

#[test]
fn openai_sample_filters_to_realtime_translate() {
    let sample = json!({"object": "list", "data": [
        {"id": "gpt-4o", "object": "model"},
        {"id": "gpt-realtime-translate", "object": "model"},
        {"id": "gpt-realtime-translate-2026-08-01", "object": "model"},
        {"id": "text-embedding-3-small"}
    ]});
    let fake = Fake::new(vec![ok(sample)]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Gpt,
        Some("sk-x"),
        AliyunRegion::Cn,
        false,
    );
    let ids: Vec<_> = list.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gpt-realtime-translate",
            "gpt-realtime-translate-2026-08-01"
        ]
    );
    assert!(by_id(&list, "gpt-realtime-translate").recommended);
    assert!(
        by_id(&list, "gpt-realtime-translate-2026-08-01").adapted,
        "前缀认同族"
    );
    assert!(list.models[0].languages.is_some());
    assert_eq!(fake.seen.borrow()[0].0, "https://api.openai.com/v1/models");
}

#[test]
fn gemini_sample_needs_live_translate_and_bidi() {
    let sample = json!({"models": [
        {"name": "models/gemini-3.5-flash", "supportedGenerationMethods": ["generateContent"]},
        {"name": "models/gemini-3.5-live-translate-preview", "displayName": "Gemini 3.5 Live Translate",
         "supportedGenerationMethods": ["bidiGenerateContent"]},
        {"name": "models/gemini-9-live-translate-x", "supportedGenerationMethods": ["generateContent"]}
    ]});
    let fake = Fake::new(vec![ok(sample)]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Gemini,
        Some("AIza-x"),
        AliyunRegion::Cn,
        false,
    );
    assert_eq!(list.models.len(), 1);
    let m = &list.models[0];
    assert_eq!(m.id, "gemini-3.5-live-translate-preview");
    assert_eq!(m.label, "Gemini 3.5 Live Translate");
    assert!(m.adapted);
    let seen = fake.seen.borrow();
    assert!(!seen[0].0.contains("AIza-x"), "密钥不进 URL");
    assert_eq!(
        seen[0].1,
        vec![("x-goog-api-key".to_string(), "AIza-x".to_string())]
    );
}

#[test]
fn gemini_follows_next_page_token() {
    let p1 = json!({"models": [], "nextPageToken": "T2"});
    let p2 = json!({"models": [{"name": "models/gemini-3.5-live-translate-preview",
        "supportedGenerationMethods": ["bidiGenerateContent"]}]});
    let fake = Fake::new(vec![ok(p1), ok(p2)]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Gemini,
        Some("k"),
        AliyunRegion::Cn,
        false,
    );
    assert_eq!(list.models.len(), 1);
    assert!(fake.seen.borrow()[1].0.ends_with("&pageToken=T2"));
}

#[test]
fn errors_are_classified_and_fall_back_to_builtin() {
    for (resp, kind) in [
        (Ok((401, "{}".to_string())), ErrorKind::Auth),
        (Ok((403, "{}".to_string())), ErrorKind::Auth),
        (Ok((500, "{}".to_string())), ErrorKind::Other),
        (Ok((200, "not json".to_string())), ErrorKind::Other),
        (Ok((200, "{\"x\":1}".to_string())), ErrorKind::Other),
        (Err("dns error".to_string()), ErrorKind::Network),
    ] {
        let fake = Fake::new(vec![resp]);
        let list = run(
            &fake,
            &fresh(),
            ModelProvider::Aliyun,
            Some("sk-secret"),
            AliyunRegion::Cn,
            false,
        );
        let err = list.error.expect("应带错误");
        assert_eq!(err.kind, kind);
        assert_eq!(err.host, "dashscope.aliyuncs.com");
        assert!(!err.message.contains("sk-secret"));
        assert_eq!(list.source, ListSource::Builtin);
        assert!(list.fetched_at.is_none());
        assert!(!list.models.is_empty(), "失败回内置列表");
        assert!(list.models.iter().all(|m| m.adapted));
    }
}

#[test]
fn no_key_means_builtin_without_asking_the_network() {
    for key in [None, Some(""), Some("   ")] {
        let fake = Fake::new(vec![]);
        let list = run(
            &fake,
            &fresh(),
            ModelProvider::Gpt,
            key,
            AliyunRegion::Cn,
            true,
        );
        assert_eq!(fake.calls(), 0);
        assert_eq!(list.source, ListSource::Builtin);
        assert!(list.error.is_none());
        assert_eq!(list.models.len(), 1);
        assert!(list.models[0].recommended);
    }
}

#[test]
fn cache_serves_for_ten_minutes_and_refresh_bypasses_it() {
    let one = || json!({"output": {"total": 1, "models": [{"model": "qwen3.8-livetranslate-flash-realtime"}]}});
    let cache = fresh();
    let fake = Fake::new(vec![ok(one()), ok(one()), ok(one())]);
    let t0 = Instant::now();
    let call = |refresh, key, now| {
        list_models_with(
            &fake,
            &cache,
            ModelProvider::Aliyun,
            Some(key),
            AliyunRegion::Cn,
            refresh,
            now,
        )
    };
    call(false, "k1", t0);
    call(false, "k1", t0 + Duration::from_secs(599));
    assert_eq!(fake.calls(), 1, "10 分钟内走缓存");
    call(true, "k1", t0 + Duration::from_secs(10));
    assert_eq!(fake.calls(), 2, "refresh 强制重拉");
    call(false, "k2", t0 + Duration::from_secs(11));
    assert_eq!(fake.calls(), 3, "换密钥不吃旧缓存");

    let fake2 = Fake::new(vec![ok(one()), ok(one())]);
    let c2 = fresh();
    let go = |now| {
        list_models_with(
            &fake2,
            &c2,
            ModelProvider::Aliyun,
            Some("k"),
            AliyunRegion::Cn,
            false,
            now,
        )
    };
    go(t0);
    go(t0 + Duration::from_secs(601));
    assert_eq!(fake2.calls(), 2, "过 10 分钟重拉");
}

#[test]
fn failures_are_not_cached() {
    let cache = fresh();
    let fake = Fake::new(vec![
        Ok((401, "{}".into())),
        ok(json!({"output": {"total": 0, "models": []}})),
    ]);
    let a = run(
        &fake,
        &cache,
        ModelProvider::Aliyun,
        Some("k"),
        AliyunRegion::Cn,
        false,
    );
    assert!(a.error.is_some());
    let b = run(
        &fake,
        &cache,
        ModelProvider::Aliyun,
        Some("k"),
        AliyunRegion::Cn,
        false,
    );
    assert!(b.error.is_none());
    assert_eq!(fake.calls(), 2);
}

#[test]
fn serialized_shape_matches_the_frontend_contract() {
    let fake = Fake::new(vec![ok(aliyun_sample())]);
    let list = run(
        &fake,
        &fresh(),
        ModelProvider::Aliyun,
        Some("k"),
        AliyunRegion::Cn,
        false,
    );
    let v = serde_json::to_value(&list).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["error", "fetched_at", "models", "provider", "source"]
    );
    assert_eq!(v["provider"], "aliyun");
    assert_eq!(v["source"], "official");
    let mut mk: Vec<_> = v["models"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    mk.sort_unstable();
    assert_eq!(
        mk,
        [
            "adapted",
            "capabilities",
            "id",
            "label",
            "languages",
            "offline_at",
            "recommended",
            "snapshot_of"
        ]
    );
    assert!(v["models"][0]["languages"]["audio"].is_array());
    assert!(v["models"][0]["languages"]["text_only"].is_array());
    let err = serde_json::to_value(ModelListError {
        kind: ErrorKind::Auth,
        host: "h".into(),
        message: "m".into(),
    })
    .unwrap();
    assert_eq!(err["kind"], "auth");
}

#[test]
fn date_helpers() {
    assert_eq!(
        date_part("2026-10-10 00:00:00").as_deref(),
        Some("2026-10-10")
    );
    assert_eq!(date_part("garbage"), None);
    assert_eq!(strip_snapshot_suffix("a-b-2026-05-19"), Some("a-b"));
    assert_eq!(strip_snapshot_suffix("gummy-realtime-v1"), None);
}

/// 真连国内站官方列表。密钥先从 Secret Service 读（`secret-service` feature 开着时，
/// service `com.vox.app` / user `api-key.aliyun`，与桌面档同一条），没有再退回环境变量
/// `DASHSCOPE_API_KEY`。只打印 id / adapted / offline_at / snapshot_of / label，不打印密钥。
///
/// `cargo test -p vox-host --features secret-service --ignored live_aliyun_list -- --nocapture`
#[test]
#[ignore = "needs a stored Aliyun key (Secret Service or DASHSCOPE_API_KEY) and network"]
fn live_aliyun_list() {
    let key = live_key().expect("no key in Secret Service nor DASHSCOPE_API_KEY");
    let list = list_models(ModelProvider::Aliyun, Some(&key), AliyunRegion::Cn, true);
    assert!(list.error.is_none(), "{:?}", list.error);
    for m in &list.models {
        println!(
            "{} adapted={} offline_at={:?} snapshot_of={:?} label={:?}",
            m.id, m.adapted, m.offline_at, m.snapshot_of, m.label
        );
    }
    assert!(list.models.iter().any(|m| m.adapted));
}

#[cfg(all(target_os = "linux", feature = "secret-service"))]
fn live_key() -> Option<String> {
    use vox_core::ports::SecretStore;
    let from_store = crate::secrets::service::SecretServiceStore::new()
        .load_api_key()
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty());
    from_store.or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
}

#[cfg(not(all(target_os = "linux", feature = "secret-service")))]
fn live_key() -> Option<String> {
    std::env::var("DASHSCOPE_API_KEY").ok()
}
