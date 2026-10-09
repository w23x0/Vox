//! 实时翻译云端会话。
//!
//! 内核不认识 WebSocket 库。socket 由外壳实现 [`Transport`] 提供，这里只做
//! **状态机**：该发什么帧、收到的帧怎么解、断了要等多久重连。这样协议逻辑
//! 全在平台无关的内核里，能单测；平台相关的只剩几十行胶水。
//!
//! 移植旧版时保留的行为：
//! - Aliyun 连上先发 `session.update`；Gemini 连上先发 `setup`。
//! - Aliyun 可热更新语言/音色；Gemini 的 setup 只能是首帧，修改后需重连。
//! - 没连上时来的音频直接丢，不排队——排队只会在重连后灌一堆过时的话。

pub mod gemini;
pub mod gpt;
pub mod protocol;

pub use protocol::{
    AliyunEndpoint, ClientEvent, CloneFrequency, Decoder, ParsedEvent, ServerEvent, SessionParams,
    INPUT_SAMPLE_RATE, OUTPUT_SAMPLE_RATE,
};

use crate::ports::{PortError, PortResult};
use crate::settings::ModelProvider;

/// 连接一条 socket 需要的东西。
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub url: String,
    /// `Authorization` 头的值。
    pub auth_header: String,
}

impl std::fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let url = self.url.split("?key=").next().unwrap_or(&self.url);
        f.debug_struct("ConnectRequest")
            .field("url", &url)
            .field(
                "auth_header",
                &if self.auth_header.is_empty() {
                    "none"
                } else {
                    "redacted"
                },
            )
            .finish()
    }
}

/// 从 socket 收到的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    /// 对端关了，附上原因（给通知栏显示）。
    Closed(String),
}

/// 外壳提供的 WebSocket。实现方只管收发字节，不理解协议。
pub trait Transport: Send {
    /// 连上去。阻塞到握手完成或失败为止。
    fn connect(&mut self, request: &ConnectRequest) -> PortResult<()>;
    /// 发一条文本帧。
    fn send(&mut self, text: &str) -> PortResult<()>;
    /// 收一条。`Ok(None)` = 超时了但连接还在（正常的安静），
    /// `Ok(Some(Closed))` = 对端收摊了。
    fn recv(&mut self, timeout_ms: u32) -> PortResult<Option<Incoming>>;
    /// 关掉。要能被重复调用。
    fn close(&mut self);
}

/// 一次热更新想改什么。`None` 表示不动。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotChange {
    pub target_language: Option<String>,
    /// `Some(Some(v))` = 换音色；`Some(None)` = 关掉语音只要文字。
    pub voice: Option<Option<String>>,
    pub clone_frequency: Option<Option<CloneFrequency>>,
}

impl HotChange {
    pub fn language(lang: impl Into<String>) -> Self {
        Self {
            target_language: Some(lang.into()),
            ..Self::default()
        }
    }

    pub fn voice(voice: impl Into<String>) -> Self {
        Self {
            voice: Some(Some(voice.into())),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.target_language.is_none() && self.voice.is_none() && self.clone_frequency.is_none()
    }
}

/// 一条云端会话的状态机。
///
/// 用法：`connect_request()` 拿地址 → 外壳连上 → `handshake()` 拿第一帧发出去
/// → 循环 `recv` 喂给 `on_message()` → 麦克风数据过 `audio_frame()` 发出去。
#[derive(Debug)]
pub struct Session {
    provider: ModelProvider,
    params: SessionParams,
    api_key: String,
    /// 阿里云接入点（地区 + 自定义）。只有阿里云会读它，另外两家原样带着。
    aliyun_endpoint: AliyunEndpoint,
    decoder: SessionDecoder,
    seq: u64,
    /// 握手发出去了没。没握手就不该上传音频。
    handshaken: bool,
}

#[derive(Debug)]
enum SessionDecoder {
    Aliyun(Decoder),
    Gemini(gemini::Decoder),
    Gpt(gpt::Decoder),
}

impl SessionDecoder {
    fn pending(&self) -> &str {
        match self {
            Self::Aliyun(decoder) => decoder.pending(),
            Self::Gemini(decoder) => decoder.pending(),
            Self::Gpt(decoder) => decoder.pending(),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Aliyun(decoder) => decoder.reset(),
            Self::Gemini(decoder) => decoder.reset(),
            Self::Gpt(decoder) => decoder.reset(),
        }
    }
}

impl Session {
    pub fn new(api_key: impl Into<String>, params: SessionParams) -> Self {
        Self::new_for(
            ModelProvider::Aliyun,
            api_key,
            params,
            AliyunEndpoint::default(),
        )
    }

    pub fn new_for(
        provider: ModelProvider,
        api_key: impl Into<String>,
        params: SessionParams,
        aliyun_endpoint: AliyunEndpoint,
    ) -> Self {
        Self {
            provider,
            params,
            api_key: api_key.into(),
            aliyun_endpoint,
            decoder: match provider {
                ModelProvider::Aliyun => SessionDecoder::Aliyun(Decoder::new()),
                ModelProvider::Gemini => SessionDecoder::Gemini(gemini::Decoder::new()),
                ModelProvider::Gpt => SessionDecoder::Gpt(gpt::Decoder::new()),
            },
            seq: 0,
            handshaken: false,
        }
    }

    pub fn params(&self) -> &SessionParams {
        &self.params
    }

    /// The sample rate the pipeline should upload at for this provider.
    pub fn input_sample_rate(&self) -> u32 {
        match self.provider {
            ModelProvider::Aliyun | ModelProvider::Gemini => INPUT_SAMPLE_RATE,
            ModelProvider::Gpt => gpt::input_sample_rate(),
        }
    }

    /// 当前这轮还没说完的半句（重连后要不要接着显示由上层决定）。
    pub fn pending_text(&self) -> &str {
        self.decoder.pending()
    }

    pub fn connect_request(&self) -> ConnectRequest {
        match self.provider {
            ModelProvider::Aliyun => ConnectRequest {
                url: protocol::endpoint_url(&self.params.model_name, &self.aliyun_endpoint),
                auth_header: protocol::auth_header_value(&self.api_key),
            },
            ModelProvider::Gemini => ConnectRequest {
                url: gemini::endpoint_url(&self.api_key),
                auth_header: String::new(),
            },
            ModelProvider::Gpt => ConnectRequest {
                url: gpt::endpoint_url(),
                auth_header: protocol::auth_header_value(&self.api_key),
            },
        }
    }

    fn next_event_id(&mut self, now_ms: u64) -> String {
        self.seq += 1;
        protocol::event_id(now_ms, self.seq)
    }

    /// Aliyun 的会话配置帧。连上就发一次（握手），之后热更新也发这个。
    fn aliyun_session_update(&mut self, now_ms: u64) -> String {
        let event = ClientEvent::SessionUpdate(Box::new(self.params.clone()));
        let id = self.next_event_id(now_ms);
        event.to_json(&id)
    }

    /// 连上之后要发的第一帧。
    pub fn handshake(&mut self, now_ms: u64) -> String {
        self.handshaken = true;
        match self.provider {
            ModelProvider::Aliyun => self.aliyun_session_update(now_ms),
            ModelProvider::Gemini => gemini::setup_frame(&self.params),
            ModelProvider::Gpt => gpt::session_update(&self.params),
        }
    }

    /// 断线了：清掉握手标记和半句，好让重连后从干净状态开始。
    pub fn on_disconnected(&mut self) {
        self.handshaken = false;
        self.decoder.reset();
    }

    /// 把一段麦克风音频（16 kHz 单声道 f32）打成要发的帧。
    /// 没握手或者没数据时返回 `None`——没连上时的音频直接丢，不排队。
    pub fn audio_frame(&mut self, samples: &[f32], now_ms: u64) -> Option<String> {
        if !self.handshaken || samples.is_empty() {
            return None;
        }
        let pcm = protocol::float_to_pcm16(samples);
        Some(match self.provider {
            ModelProvider::Aliyun => {
                let id = self.next_event_id(now_ms);
                ClientEvent::AppendAudio(pcm).to_json(&id)
            }
            ModelProvider::Gemini => gemini::audio_frame(&pcm),
            ModelProvider::Gpt => gpt::audio_frame(&pcm),
        })
    }

    /// For providers that support graceful protocol-level close, returns the frame
    /// the shell should send before closing the WebSocket.
    pub fn close_frame(&self) -> Option<String> {
        if !self.handshaken {
            return None;
        }
        match self.provider {
            ModelProvider::Gpt => Some(gpt::close_frame()),
            _ => None,
        }
    }

    /// 热更新。真有东西变了才返回要发的帧，白改不发（省一次往返）。
    pub fn hot_update(&mut self, change: &HotChange, now_ms: u64) -> Option<String> {
        let mut dirty = false;
        if let Some(lang) = &change.target_language {
            let normalized = crate::catalog::normalize_language(lang);
            if self.params.target_language != normalized {
                self.params.target_language = normalized.to_string();
                dirty = true;
            }
        }
        if let Some(voice) = &change.voice {
            if &self.params.voice != voice {
                self.params.voice = voice.clone();
                dirty = true;
            }
        }
        if let Some(freq) = change.clone_frequency {
            if self.params.clone_frequency != freq {
                self.params.clone_frequency = freq;
                dirty = true;
            }
        }
        if !dirty {
            return None;
        }
        // 没握手就别发；重连后的 handshake 会带上新参数。
        if !self.handshaken {
            return None;
        }
        if !crate::catalog::supports(
            self.provider,
            crate::capability::Capability::HotUpdateLanguage,
        ) {
            return None;
        }
        match self.provider {
            ModelProvider::Aliyun => Some(self.aliyun_session_update(now_ms)),
            ModelProvider::Gpt => Some(gpt::session_update(&self.params)),
            // Gemini's setup message is only valid as the first frame; its catalog
            // capability is false, so this arm is only reached if catalog data changes.
            ModelProvider::Gemini => None,
        }
    }

    /// 吃一条服务端消息。
    pub fn on_message(&mut self, message: &str) -> Option<ParsedEvent> {
        self.on_messages(message).into_iter().next()
    }

    pub fn on_messages(&mut self, message: &str) -> Vec<ParsedEvent> {
        match &mut self.decoder {
            SessionDecoder::Aliyun(decoder) => decoder.decode(message).into_iter().collect(),
            SessionDecoder::Gemini(decoder) => decoder.decode(message),
            SessionDecoder::Gpt(decoder) => decoder.decode(message),
        }
    }
}

// --- 重连节奏 --------------------------------------------------------------

/// 重连退避。首次失败马上重试一次（多半是瞬断），之后翻倍，封顶 15 秒。
///
/// 上限存在的理由：网络真断了或者密钥错了，不能每 200 ms 敲一次服务端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub const FIRST_MS: u32 = 400;
    pub const MAX_MS: u32 = 15_000;

    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// 试过几次了。
    pub fn attempt(self) -> u32 {
        self.attempt
    }

    /// 记一次失败，返回这次该等多久。
    pub fn fail(&mut self) -> u32 {
        let wait = Self::FIRST_MS
            .saturating_mul(1u32 << self.attempt.min(6))
            .min(Self::MAX_MS);
        self.attempt = self.attempt.saturating_add(1);
        wait
    }

    /// 连上了，重新计数。
    pub fn succeed(&mut self) {
        self.attempt = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// 服务端的错误里，哪些是重连也没用的（密钥错、模型没权限、余额不足）。
/// 这类要直接失败并弹通知，别闷头重试。
pub fn is_fatal_error(code: Option<&str>, message: &str) -> bool {
    // 归一化后的写法：服务端的错误码大小写和分隔符都不统一，见过 `InvalidApiKey`、
    // `invalid_api_key`、`Arrearage` 混着来，所以比对前先拍平。
    const FATAL_CODES: &[&str] = &[
        "invalidapikey",
        "invalidauthorization",
        "authenticationerror",
        "accessdenied",
        "modelnotfound",
        "invalidmodel",
        "arrearage",
        "insufficientquota",
        "allocatedquotaexceeded",
    ];
    if let Some(code) = code {
        let flat = normalize_code(code);
        if FATAL_CODES.iter().any(|c| flat.contains(c)) {
            return true;
        }
    }
    let lower = message.to_ascii_lowercase();
    ["invalid api-key", "invalid api key", "incorrect api key"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// 小写化并去掉分隔符，好让 `InvalidApiKey` 和 `invalid_api_key` 比得上。
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 把服务端错误翻译成给人看的话。密钥类的错最常见，说清楚该去哪改。
pub fn explain_error(code: Option<&str>, message: &str) -> String {
    let flat = normalize_code(code.unwrap_or_default());
    if flat.contains("resourceexhausted") || flat.contains("ratelimit") {
        return format!("模型服务请求过快或项目额度暂时耗尽，请稍后再试：{message}");
    }
    if is_fatal_error(code, message) {
        let lower = message.to_ascii_lowercase();
        if flat.contains("arrearage") || flat.contains("quota") || lower.contains("quota") {
            return format!("模型服务账户欠费或额度用完了：{message}");
        }
        if flat.contains("model") {
            return format!("这个模型不可用，换一个试试：{message}");
        }
        return format!("API 密钥不对，去设置里重新填一次：{message}");
    }
    match code {
        Some(code) => format!("翻译服务报错（{code}）：{message}"),
        None => format!("翻译服务报错：{message}"),
    }
}

/// 服务端主动 close 的原因里，哪些是**重连也没用的**（额度用完、密钥错）。
/// 命中时返回一句给用户看的话——顺带把原文带在里面，服务商给的原因往往比
/// 我们的翻译更具体。
///
/// 认得出的那几类（`free tier` / `quota` / `arrearage` / 余额不足）是真连踩到的：
/// 百炼在额度用尽时用 close 帧回一条超长的说明（见 `MODEL-TABLE.md` §1.3），
/// 不接住它用户只看到一个看不懂的协议错误。
pub fn explain_close(reason: &str) -> Option<String> {
    let lower = reason.to_ascii_lowercase();
    let quota = [
        "free tier",
        "quota",
        "arrearage",
        "余额不足",
        "insufficient balance",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if quota {
        return Some(format!(
            "该模型的额度已用完，请到服务商控制台检查额度或关闭『仅使用免费额度』。服务端原话：{reason}"
        ));
    }
    if is_fatal_error(None, reason) {
        return Some(format!("翻译服务拒绝了这次连接：{reason}"));
    }
    None
}

/// 服务端错误 → 外壳能往上抛的失败。非错误事件返回 `None`。
pub fn error_to_port_error(event: &ServerEvent) -> Option<PortError> {
    match event {
        ServerEvent::Error { code, message } => {
            Some(PortError::new(explain_error(code.as_deref(), message)))
        }
        _ => None,
    }
}

/// 走一遍"连上 → 握手"，把两步的失败合成一个结果。
pub fn open(transport: &mut dyn Transport, session: &mut Session, now_ms: u64) -> PortResult<()> {
    transport.connect(&session.connect_request())?;
    let frame = session.handshake(now_ms);
    if let Err(err) = transport.send(&frame) {
        session.on_disconnected();
        transport.close();
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const LIVE: &str = "qwen3.8-livetranslate-flash-realtime";

    fn session() -> Session {
        Session::new(
            "sk-test",
            SessionParams {
                model_name: LIVE.to_string(),
                target_language: "ja".to_string(),
                voice: Some("Tina".to_string()),
                clone_frequency: None,
                source_language: None,
            },
        )
    }

    fn gemini_session() -> Session {
        Session::new_for(
            ModelProvider::Gemini,
            "AIza-test+/=",
            SessionParams {
                model_name: crate::catalog::GEMINI_MODEL_NAME.to_string(),
                target_language: "ja".to_string(),
                voice: Some("provider-default".to_string()),
                clone_frequency: None,
                source_language: None,
            },
            AliyunEndpoint::default(),
        )
    }

    fn gpt_session() -> Session {
        Session::new_for(
            ModelProvider::Gpt,
            "sk-test-gpt",
            SessionParams {
                model_name: crate::catalog::GPT_MODEL_NAME.to_string(),
                target_language: "ja".to_string(),
                voice: Some("auto".to_string()),
                clone_frequency: None,
                source_language: None,
            },
            AliyunEndpoint::default(),
        )
    }

    fn parse(frame: &str) -> Value {
        serde_json::from_str(frame).expect("发出去的必须是合法 JSON")
    }

    /// 记下发出去的每一帧的假 socket。
    #[derive(Default)]
    struct FakeTransport {
        sent: Vec<String>,
        connected: Vec<ConnectRequest>,
        closes: u32,
        fail_connect: Option<String>,
        fail_send: Option<String>,
        inbox: Vec<Option<Incoming>>,
    }

    impl Transport for FakeTransport {
        fn connect(&mut self, request: &ConnectRequest) -> PortResult<()> {
            if let Some(err) = &self.fail_connect {
                return Err(PortError::new(err.clone()));
            }
            self.connected.push(request.clone());
            Ok(())
        }

        fn send(&mut self, text: &str) -> PortResult<()> {
            if let Some(err) = &self.fail_send {
                return Err(PortError::new(err.clone()));
            }
            self.sent.push(text.to_string());
            Ok(())
        }

        fn recv(&mut self, _timeout_ms: u32) -> PortResult<Option<Incoming>> {
            Ok(if self.inbox.is_empty() {
                None
            } else {
                self.inbox.remove(0)
            })
        }

        fn close(&mut self) {
            self.closes += 1;
        }
    }

    #[test]
    fn connect_request_carries_url_and_bearer_token() {
        let req = session().connect_request();
        assert!(req.url.ends_with(LIVE));
        assert_eq!(req.auth_header, "Bearer sk-test");
    }

    #[test]
    fn gemini_connects_with_a_redacted_query_key_and_setup_frame() {
        let mut session = gemini_session();
        let request = session.connect_request();
        assert!(request.url.starts_with(crate::catalog::GEMINI_API_BASE));
        assert!(request.url.ends_with("?key=AIza-test%2B%2F%3D"));
        assert!(request.auth_header.is_empty());
        assert!(!format!("{request:?}").contains("AIza"));

        let setup = parse(&session.handshake(0));
        assert_eq!(
            setup["setup"]["model"],
            "models/gemini-3.5-live-translate-preview"
        );
        assert_eq!(
            setup["setup"]["generationConfig"]["translationConfig"]["targetLanguageCode"],
            "ja"
        );
    }

    #[test]
    fn gpt_connects_with_bearer_and_realtime_frames() {
        let mut session = gpt_session();
        let request = session.connect_request();
        assert_eq!(request.url, gpt::endpoint_url());
        assert_eq!(request.auth_header, "Bearer sk-test-gpt");

        let handshake = parse(&session.handshake(0));
        assert_eq!(handshake["type"], "session.update");
        assert_eq!(handshake["session"]["audio"]["output"]["language"], "ja");
        assert!(handshake["session"].get("modalities").is_none());

        let audio = session.audio_frame(&[0.1, -0.1], 5).expect("握手后该发");
        let parsed = parse(&audio);
        assert_eq!(parsed["type"], "session.input_audio_buffer.append");
        assert!(parsed["audio"].as_str().is_some());

        assert_eq!(
            session.close_frame().as_deref(),
            Some(r#"{"type":"session.close"}"#)
        );
    }

    #[test]
    fn gpt_close_frame_is_only_offered_after_handshake() {
        let mut gpt = gpt_session();
        assert!(gpt.close_frame().is_none(), "没握手不该发 session.close");
        gpt.handshake(0);
        assert_eq!(
            gpt.close_frame().as_deref(),
            Some(r#"{"type":"session.close"}"#)
        );
        assert!(session().close_frame().is_none());
    }

    #[test]
    fn input_sample_rate_is_16k_for_alias_gemini_and_24k_for_gpt() {
        assert_eq!(session().input_sample_rate(), 16_000);
        assert_eq!(gemini_session().input_sample_rate(), 16_000);
        assert_eq!(gpt_session().input_sample_rate(), 24_000);
    }

    #[test]
    fn open_connects_then_pushes_the_session_config() {
        let mut s = session();
        let mut t = FakeTransport::default();
        open(&mut t, &mut s, 1_000).unwrap();

        assert_eq!(t.connected.len(), 1);
        assert_eq!(t.sent.len(), 1, "连上就该发且只发一次配置");
        let frame = parse(&t.sent[0]);
        assert_eq!(frame["type"], "session.update");
        assert_eq!(frame["session"]["translation"]["language"], "ja");
    }

    #[test]
    fn a_failed_handshake_closes_the_socket_and_forgets_it() {
        let mut s = session();
        let mut t = FakeTransport {
            fail_send: Some("管道断了".into()),
            ..Default::default()
        };
        let err = open(&mut t, &mut s, 0).unwrap_err();
        assert!(err.message.contains("管道断了"));
        assert_eq!(t.closes, 1, "握手失败要关掉半开的 socket");
        // 没握手成功就不该上传音频。
        assert!(s.audio_frame(&[0.1, 0.2], 0).is_none());
    }

    #[test]
    fn a_failed_connect_never_sends_anything() {
        let mut s = session();
        let mut t = FakeTransport {
            fail_connect: Some("连不上".into()),
            ..Default::default()
        };
        assert!(open(&mut t, &mut s, 0).is_err());
        assert!(t.sent.is_empty());
    }

    #[test]
    fn audio_is_dropped_before_the_handshake_and_sent_after() {
        let mut s = session();
        assert!(
            s.audio_frame(&[0.1], 0).is_none(),
            "没连上时的音频要丢掉，不能排队"
        );

        s.handshake(0);
        let frame = s.audio_frame(&[0.1, -0.1], 5).expect("握手后该发");
        let parsed = parse(&frame);
        assert_eq!(parsed["type"], "input_audio_buffer.append");
        assert!(parsed["audio"].as_str().is_some());

        // 空数据不发。
        assert!(s.audio_frame(&[], 6).is_none());
    }

    #[test]
    fn disconnect_stops_uploads_until_the_next_handshake() {
        let mut s = session();
        s.handshake(0);
        assert!(s.audio_frame(&[0.1], 1).is_some());
        s.on_disconnected();
        assert!(s.audio_frame(&[0.1], 2).is_none());
        s.handshake(3);
        assert!(s.audio_frame(&[0.1], 4).is_some());
    }

    #[test]
    fn disconnect_drops_the_half_sentence() {
        let mut s = session();
        s.handshake(0);
        s.on_message(r#"{"type":"response.text.delta","delta":"半句"}"#);
        assert_eq!(s.pending_text(), "半句");
        s.on_disconnected();
        assert_eq!(s.pending_text(), "", "重连后不该接着上一句的尾巴");
    }

    #[test]
    fn event_ids_never_repeat_within_a_session() {
        let mut s = session();
        let mut ids = vec![parse(&s.handshake(1_000))["event_id"]
            .as_str()
            .unwrap()
            .to_string()];
        for _ in 0..3 {
            // 同一毫秒连发，靠序号区分。
            let frame = s.audio_frame(&[0.1], 1_000).unwrap();
            ids.push(parse(&frame)["event_id"].as_str().unwrap().to_string());
        }
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "event_id 撞了：{ids:?}");
    }

    #[test]
    fn language_change_is_a_hot_update_not_a_reconnect() {
        let mut s = session();
        s.handshake(0);
        let frame = s
            .hot_update(&HotChange::language("en"), 1)
            .expect("换语言该发热更新");
        let parsed = parse(&frame);
        assert_eq!(parsed["type"], "session.update");
        assert_eq!(parsed["session"]["translation"]["language"], "en");
        assert_eq!(s.params().target_language, "en");
    }

    #[test]
    fn a_no_op_hot_update_sends_nothing() {
        let mut s = session();
        s.handshake(0);
        assert!(
            s.hot_update(&HotChange::language("ja"), 1).is_none(),
            "改成一样的值不该发帧"
        );
        assert!(s.hot_update(&HotChange::default(), 1).is_none());
        assert!(HotChange::default().is_empty());
    }

    #[test]
    fn hot_update_before_the_handshake_only_records_the_intent() {
        let mut s = session();
        assert!(
            s.hot_update(&HotChange::language("en"), 0).is_none(),
            "还没握手时不发帧"
        );
        assert_eq!(s.params().target_language, "en", "但参数要记下来");
        // 握手时带上新语言。
        let frame = parse(&s.handshake(1));
        assert_eq!(frame["session"]["translation"]["language"], "en");
    }

    #[test]
    fn voice_can_be_swapped_or_turned_off_live() {
        let mut s = session();
        s.handshake(0);

        let frame = parse(&s.hot_update(&HotChange::voice("Chelsie"), 1).unwrap());
        assert_eq!(frame["session"]["audio"]["output"]["voice"], "Chelsie");

        // Some(None) = 只要文字。
        let off = HotChange {
            voice: Some(None),
            ..HotChange::default()
        };
        let frame = parse(&s.hot_update(&off, 2).unwrap());
        assert_eq!(
            frame["session"]["output_modalities"],
            serde_json::json!(["text"])
        );
        assert!(frame["session"]["audio"]["output"].get("voice").is_none());
    }

    #[test]
    fn hot_update_normalizes_a_bogus_language() {
        let mut s = session();
        s.handshake(0);
        s.hot_update(&HotChange::language("这不是语言代码"), 1);
        assert_eq!(
            s.params().target_language,
            crate::catalog::normalize_language("这不是语言代码")
        );
    }

    #[test]
    fn messages_flow_through_to_parsed_events() {
        let mut s = session();
        s.handshake(0);
        let ev = s
            .on_message(r#"{"type":"response.audio_transcript.delta","transcript":"hi"}"#)
            .unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::TextDelta {
                text: "hi".into(),
                confirmed: Some("hi".into())
            }
        );
        assert!(s.on_message("垃圾").is_none());
    }

    #[test]
    fn backoff_starts_quick_then_doubles_up_to_the_cap() {
        let mut b = Backoff::new();
        assert_eq!(b.attempt(), 0);
        assert_eq!(b.fail(), 400);
        assert_eq!(b.fail(), 800);
        assert_eq!(b.fail(), 1_600);
        assert_eq!(b.fail(), 3_200);
        assert_eq!(b.attempt(), 4);

        // 一直失败也不会超过上限，也不会溢出。
        for _ in 0..200 {
            assert!(b.fail() <= Backoff::MAX_MS);
        }
        assert_eq!(b.fail(), Backoff::MAX_MS);

        b.succeed();
        assert_eq!(b.attempt(), 0);
        assert_eq!(b.fail(), 400, "连上过以后要从头开始退避");
    }

    #[test]
    fn key_and_quota_errors_are_fatal_but_hiccups_are_not() {
        assert!(is_fatal_error(Some("invalid_api_key"), "whatever"));
        assert!(is_fatal_error(Some("InvalidApiKey"), "whatever"));
        assert!(is_fatal_error(Some("Arrearage"), "欠费了"));
        assert!(is_fatal_error(Some("model_not_found"), ""));
        assert!(is_fatal_error(None, "Invalid API-key provided."));

        assert!(!is_fatal_error(Some("server_error"), "internal error"));
        assert!(!is_fatal_error(Some("rate_limit_exceeded"), "too fast"));
        assert!(!is_fatal_error(None, "connection reset"));
    }

    /// 服务端的错误码大小写/分隔符不统一，三种写法都得认出来。
    #[test]
    fn fatal_codes_are_matched_regardless_of_casing_and_separators() {
        for code in ["InvalidApiKey", "invalid_api_key", "invalid-api-key"] {
            assert!(is_fatal_error(Some(code), ""), "{code} 应该是致命错误");
            assert!(explain_error(Some(code), "bad").contains("密钥"));
        }
        assert!(explain_error(Some("InsufficientQuota"), "x").contains("欠费"));
        assert!(explain_error(Some("ModelNotFound"), "x").contains("换一个"));
    }

    #[test]
    fn error_explanations_point_at_the_fix() {
        assert!(explain_error(Some("invalid_api_key"), "bad key").contains("设置里重新填"));
        assert!(explain_error(Some("arrearage"), "no money").contains("欠费"));
        assert!(explain_error(Some("model_not_found"), "nope").contains("换一个"));
        let generic = explain_error(Some("server_error"), "boom");
        assert!(generic.contains("server_error") && generic.contains("boom"));
        assert!(explain_error(None, "boom").contains("boom"));
    }

    #[test]
    fn only_error_events_convert_to_a_failure() {
        let err = error_to_port_error(&ServerEvent::Error {
            code: Some("invalid_api_key".into()),
            message: "bad".into(),
        })
        .unwrap();
        assert!(err.message.contains("密钥"));
        assert!(error_to_port_error(&ServerEvent::SessionUpdated).is_none());
    }

    #[test]
    fn transport_reports_quiet_and_closed_apart() {
        let mut t = FakeTransport {
            inbox: vec![
                Some(Incoming::Text("{}".into())),
                None,
                Some(Incoming::Closed("对端关了".into())),
            ],
            ..Default::default()
        };
        assert_eq!(t.recv(250).unwrap(), Some(Incoming::Text("{}".into())));
        assert_eq!(t.recv(250).unwrap(), None, "安静不是错误");
        assert_eq!(
            t.recv(250).unwrap(),
            Some(Incoming::Closed("对端关了".into()))
        );
    }

    /// 百炼额度用尽时 close 帧里的原话（2026-10-09 真连抓到，182 字节）。
    const QUOTA_CLOSE: &str = "The free tier of the model has been exhausted. If you wish to \
        continue access the model on a paid basis, please disable the \"use free tier only\" \
        mode in the management console.";

    /// 额度类 close 要说清楚"去哪儿改"，并附上服务端原话——只把协议错误
    /// 转述给用户等于没修。
    #[test]
    fn an_exhausted_quota_close_is_explained_with_its_own_words() {
        let explained = explain_close(QUOTA_CLOSE).expect("额度用尽要给友好提示");
        assert!(explained.contains("额度"), "{explained}");
        assert!(explained.contains("控制台"), "{explained}");
        assert!(explained.contains("仅使用免费额度"), "{explained}");
        assert!(
            explained.contains(QUOTA_CLOSE),
            "服务端原话要原样附上：{explained}"
        );
    }

    /// 普通断开（没有额度/密钥字样）不该被判成致命错误——那是重连的信号。
    #[test]
    fn ordinary_closes_are_left_to_the_reconnect_logic() {
        assert!(explain_close("服务端关闭连接（1000）：再见").is_none());
        assert!(explain_close("连接意外断开（读循环已退出）").is_none());
        assert!(explain_close("connection reset by peer").is_none());
        // 密钥类仍然要拦：重连一万次也是这个结果。
        assert!(explain_close("Invalid API-key provided.").is_some());
    }
}
