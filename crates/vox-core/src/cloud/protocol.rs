//! qwen realtime WebSocket 协议。**收发的 JSON 长什么样，只写在这一个文件里。**
//!
//! 这份实现是旧版 `vrcq/cloud.py` 的逐条移植——那是踩过坑跑通过的东西，
//! 字段名、事件名、采样率都照抄，不"顺手改得更漂亮"。移植时保留的关键事实：
//!
//! 1. 产品只接专用 LiveTranslate 模型，使用原生 `translation.language` 参数。
//! 2. 文字增量事件有**三个**名字（`response.audio_transcript.delta`、
//!    `response.text.delta`、`response.output_text.delta`），字段名还不一样：
//!    transcript 类的在 `transcript`，text 类的在 `delta`。少接一个就丢字幕。
//! 3. 增量对外吐的是**到目前为止拼好的整句**，不是这一小片。字幕层要的是整句。
//! 4. done 事件里的文字可能是空的，这时拿累积的凑数（`or_else`），别直接清空。
//! 5. `session.update` 能热改语言/音色/要不要语音，**换模型不行**——那要新开
//!    socket，由上层重启会话。
//! 6. 输入 16 kHz、输出 24 kHz，都是 PCM16LE 单声道。
//!
//! **方言**：`session.update` 的报文按方言分派（`qwen38_session_object` /
//! `qwen35_session_object`），方言由模型名经 `catalog::dialect_by_prefix` 定。
//! **事件解码不分方言**：两条方言的事件名逐字相同（真连核实过 3.8，3.5 的官方
//! 文档也用同一套 `response.*` / `input_audio_buffer.*`），拆成两份只会让两条
//! 分支各自漂移。真出现差异时，这里才拆。
//!
//! 这里只做纯粹的编解码，不碰网络、不碰线程，好在平台无关的内核里做单测。

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::catalog::{self, DialectInfo, SessionShape, TargetMode};
use crate::settings::AliyunRegion;
use crate::usage::TurnUsage;

/// 上传音频的采样率。
pub const INPUT_SAMPLE_RATE: u32 = 16_000;
/// 服务端回的语音采样率。
pub const OUTPUT_SAMPLE_RATE: u32 = 24_000;

/// 覆盖连接地址的环境变量：填百炼控制台给的工作空间专属地址
/// （`wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime`）。
///
/// 优先级最高，但只是**调试用的后门**——正常路径是设置里的
/// `aliyun_endpoint`，它又优先于 `aliyun_region` 的缺省地址。
pub const ENDPOINT_ENV: &str = "VOX_ALIYUN_ENDPOINT";

/// 阿里云接入点：地区缺省 + 用户自定义。跟着 `Settings` 一路走到建连处。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AliyunEndpoint {
    pub region: AliyunRegion,
    /// 自定义接入点；空 = 按 `region` 缺省。
    pub custom: String,
}

fn region_base(region: AliyunRegion) -> &'static str {
    match region {
        AliyunRegion::Cn => catalog::API_BASE,
        AliyunRegion::Intl => catalog::API_BASE_INTL,
    }
}

/// 拼出连接地址。模型名走 query 参数。
///
/// 三级优先级：环境变量 > 设置里的自定义接入点 > 地区的缺省地址。
pub fn endpoint_url(model_name: &str, endpoint: &AliyunEndpoint) -> String {
    let env = std::env::var(ENDPOINT_ENV).ok();
    endpoint_url_with(env.as_deref(), endpoint, model_name)
}

fn endpoint_url_with(env: Option<&str>, endpoint: &AliyunEndpoint, model_name: &str) -> String {
    // 环境变量 → 设置里的自定义地址 → 地区缺省。**按优先级取第一个用得上的**：
    // 环境变量留了个空值不该把用户的自定义地址一起顶掉。
    let base = [env, Some(endpoint.custom.as_str())]
        .into_iter()
        .flatten()
        .filter_map(usable_base)
        .next()
        .unwrap_or_else(|| region_base(endpoint.region).to_string());
    let model = catalog::normalize_model_for(crate::settings::ModelProvider::Aliyun, model_name);
    format!("{base}?model={model}")
}

/// 挑出一个能用作接入点的地址：`wss://`，或者指向本机回环的 `ws://`（本地调试/代理）。
/// 其他 `ws://` 一律无效——API 密钥会随握手头明文发出去。
/// 用户贴来的地址可能自带 `?model=...`，query 一律去掉——它要按当前模型重拼。
fn usable_base(candidate: &str) -> Option<String> {
    let trimmed = candidate
        .trim()
        .split('?')
        .next()
        .unwrap_or(candidate)
        .trim()
        .trim_end_matches('/');
    let usable = trimmed.starts_with("wss://")
        || trimmed
            .strip_prefix("ws://")
            .is_some_and(is_loopback_authority);
    usable.then(|| trimmed.to_string())
}

/// `host[:port][/path]` 的 host 是不是本机回环：`localhost`、127.0.0.0/8、`::1`。
/// 带 `user@` 的写法一律不认（`ws://localhost@evil.com` 的真主机是后者）。
fn is_loopback_authority(rest: &str) -> bool {
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.contains('@') {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((host, _port)) => host,
            None => return false,
        },
        None => authority.split(':').next().unwrap_or(authority),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// 鉴权头的值（`Authorization: Bearer sk-...`）。
pub fn auth_header_value(api_key: &str) -> String {
    format!("Bearer {api_key}")
}

/// 声音复刻的采样频次。协议里是字符串，设置里存的是次数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum CloneFrequency {
    /// 只用第一段声音复刻。
    Once,
    /// 一直跟着说话人的声音走。
    Always,
}

impl CloneFrequency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
        }
    }

    /// 设置里存的是次数：1 次 = only once，多次 = 一直跟。0/None 表示不复刻。
    pub fn from_count(count: u32) -> Option<Self> {
        match count {
            0 => None,
            1 => Some(Self::Once),
            _ => Some(Self::Always),
        }
    }
}

/// 一次会话的协议参数。跟 `runtime::SessionConfig` 的区别是这里只留协议关心的东西。
///
/// 它同时是组合清单里 `session.params` 那一格（所以要有 `Serialize` / `Deserialize`：
/// 清单是数据，要能存能传——报文体仍然是各协议自己手工拼的，这份 derive 不参与拼报文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SessionParams {
    pub model_name: String,
    pub target_language: String,
    /// `None` = 只要文字不要语音。
    pub voice: Option<String>,
    pub clone_frequency: Option<CloneFrequency>,
    /// 源语言；`None` = 服务端自动识别（默认，行为和旧版一致）。
    /// 填了会发 `input_audio_transcription.language`，把识别锁死到指定语言。
    pub source_language: Option<String>,
}

impl SessionParams {
    pub fn text_only(model_name: impl Into<String>, target_language: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            target_language: target_language.into(),
            voice: None,
            clone_frequency: None,
            source_language: None,
        }
    }

    /// 这轮到底能不能出语音：要有音色，且目标语言支持语音输出。
    /// 目标语言不支持语音时，硬要 audio 会被服务端拒，所以这里先降级成纯文字。（坑）
    pub fn audio_enabled(&self) -> bool {
        self.voice.is_some()
            && self.dialect().is_some_and(|dialect| {
                dialect.languages.target_mode(&self.target_language) == Some(TargetMode::Audio)
            })
    }

    /// 这个模型的方言。认不出（设置阶段本应归一掉）就是 `None`。
    fn dialect(&self) -> Option<&'static DialectInfo> {
        catalog::dialect_by_prefix(&self.model_name)
    }

    /// 目标语言的**线上码**：规范码经方言的映射表翻成服务商认的写法再发。
    /// 方言不认这个目标语言（或认不出方言）时原样发规范码。
    pub fn wire_target(&self) -> &str {
        self.dialect()
            .and_then(|dialect| dialect.languages.wire_target(&self.target_language))
            .unwrap_or(&self.target_language)
    }

    /// 源语言的线上码。没选、或方言不支持这个源语言时是 `None`（= 自动识别，不锁识别）。
    pub fn wire_source(&self) -> Option<&str> {
        let source = self.source_language.as_deref()?;
        match self.dialect() {
            Some(dialect) => dialect.languages.wire_source(source),
            None => Some(source),
        }
    }

    /// 有效音色。复刻模式下协议要求音色写死 `"default"`。
    pub fn effective_voice(&self) -> Option<&str> {
        if !self.audio_enabled() {
            return None;
        }
        if self.clone_frequency.is_some() {
            Some("default")
        } else {
            self.voice.as_deref()
        }
    }

    /// `session.update` 里那个 `session` 对象。**按方言拼**——方言由模型名定
    /// （`catalog::dialect_by_prefix`），换模型就是换形状，所以这条链路上
    /// 不另存一份"当前方言"（存两份迟早会打架）。
    pub fn to_session_object(&self) -> Value {
        match catalog::dialect_by_prefix(&self.model_name).and_then(|dialect| dialect.shape) {
            // 认不出的模型不该走到建连（设置阶段就归一过）；万一到了这里，
            // 退回最保守的形状：只发翻译语言，不碰任何音频参数。
            None => minimal_session_object(self.wire_target()),
            Some(SessionShape::QwenNestedAudio) => qwen38_session_object(self),
            Some(SessionShape::QwenFlatAudio) => qwen35_session_object(self),
        }
    }

    /// 方言表里登记的识别模型（`None` = 不显式发，用服务端缺省）。
    fn transcription_model(&self) -> Option<&'static str> {
        catalog::dialect_by_prefix(&self.model_name).and_then(|dialect| dialect.transcription_model)
    }

    /// `input_audio_transcription` 对象：源语言与识别模型**都**没配就不发这个字段。
    fn transcription_object(&self) -> Option<Value> {
        let mut object = Map::new();
        if let Some(language) = self.wire_source() {
            object.insert("language".into(), json!(language));
        }
        if let Some(model) = self.transcription_model() {
            object.insert("model".into(), json!(model));
        }
        (!object.is_empty()).then_some(Value::Object(object))
    }

    /// 复刻那两格（`enable_voice_clone` / `voice_clone_options`）。
    /// 两条阿里云方言在这点上一样，放一处；没出语音就不发。
    fn insert_voice_clone(&self, session: &mut Map<String, Value>) {
        let Some(frequency) = self.clone_frequency else {
            return;
        };
        if !self.audio_enabled() {
            return;
        }
        session.insert("enable_voice_clone".into(), json!(true));
        session.insert(
            "voice_clone_options".into(),
            json!({ "frequency": frequency.as_str() }),
        );
    }
}

/// 认不出方言时的兜底：只说翻译语言，其余一律不碰。
fn minimal_session_object(target_language: &str) -> Value {
    json!({ "translation": { "language": target_language } })
}

/// **qwen3.8** 的会话形状：输出模态叫 `output_modalities`，音频格式与音色收在
/// `audio.input` / `audio.output` 下（跟 `session.created` 回显的形状一致）；
/// 不发 `turn_detection`，用服务端缺省（`speaker_detection`）。
/// ASR 恒开；官方文档没写 3.8 的源语种字段，手动选了源语言时沿用
/// `input_audio_transcription.language`（缺省自动识别时不发，报文与官方 3.8 示例一致）。
fn qwen38_session_object(params: &SessionParams) -> Value {
    let audio = params.audio_enabled();
    let mut session = Map::new();
    session.insert(
        "output_modalities".into(),
        if audio {
            json!(["text", "audio"])
        } else {
            json!(["text"])
        },
    );

    let mut output = Map::new();
    output.insert(
        "format".into(),
        json!({ "type": "pcm", "sample_rate": OUTPUT_SAMPLE_RATE }),
    );
    if let Some(voice) = params.effective_voice() {
        output.insert("voice".into(), json!(voice));
    }
    session.insert(
        "audio".into(),
        json!({
            "input": { "format": { "type": "pcm", "sample_rate": INPUT_SAMPLE_RATE } },
            "output": Value::Object(output),
        }),
    );

    session.insert(
        "translation".into(),
        json!({ "language": params.wire_target() }),
    );
    if let Some(transcription) = params.transcription_object() {
        session.insert("input_audio_transcription".into(), transcription);
    }
    params.insert_voice_clone(&mut session);
    Value::Object(session)
}

/// **qwen3.5** 的会话形状：音色**平铺**，音频格式是 `pcm` 字符串
/// （16 kHz 上行 / 24 kHz 下行由服务端缺省），模态用 `modalities`，格式都不再收在 `audio` 下面。
fn qwen35_session_object(params: &SessionParams) -> Value {
    let mut session = Map::new();
    // 服务端缺省是 `["text","audio"]`：不发就关不掉语音，所以文字模式也要显式声明。
    session.insert(
        "modalities".into(),
        if params.audio_enabled() {
            json!(["text", "audio"])
        } else {
            json!(["text"])
        },
    );
    if let Some(voice) = params.effective_voice() {
        session.insert("voice".into(), json!(voice));
    }
    session.insert("input_audio_format".into(), json!("pcm"));
    session.insert("output_audio_format".into(), json!("pcm"));

    session.insert(
        "translation".into(),
        json!({ "language": params.wire_target() }),
    );
    if let Some(transcription) = params.transcription_object() {
        session.insert("input_audio_transcription".into(), transcription);
    }
    params.insert_voice_clone(&mut session);
    Value::Object(session)
}

/// 事件 id。服务端只要求唯一，用毫秒时间戳 + 递增号；时钟由外壳传进来。
pub fn event_id(now_ms: u64, seq: u64) -> String {
    format!("event_{now_ms}_{seq}")
}

/// 我们往上发的东西。
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// 推会话配置（连上就发一次，之后热更新也发这个）。
    SessionUpdate(Box<SessionParams>),
    /// 追加一段上行音频（PCM16LE 单声道 16 kHz）。
    AppendAudio(Vec<u8>),
}

impl ClientEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SessionUpdate(_) => "session.update",
            Self::AppendAudio(_) => "input_audio_buffer.append",
        }
    }

    /// 序列化成要发出去的 JSON 文本。
    pub fn to_json(&self, event_id: &str) -> String {
        let value = match self {
            Self::SessionUpdate(params) => json!({
                "event_id": event_id,
                "type": self.type_name(),
                "session": params.to_session_object(),
            }),
            Self::AppendAudio(pcm) => json!({
                "event_id": event_id,
                "type": self.type_name(),
                "audio": base64::engine::general_purpose::STANDARD.encode(pcm),
            }),
        };
        value.to_string()
    }
}

/// 服务端回过来的东西。认得的都拆开，认不得的原样留着，方便调试面板显示。
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    /// 译文增量。`text` 是**到目前为止拼好的整句**（渲染值，含临时 stash 尾巴），
    /// `confirmed` 那部分不再会变、可安全交给外部整行替换（如 VRChat ChatBox）：
    /// 已确认前缀（LiveTranslate 的 `.text` 事件）或已累积的整句（delta 类型）。
    TextDelta {
        text: String,
        confirmed: Option<String>,
    },
    /// 这一句完了。
    TextDone { text: String },
    /// 服务端回报的**源文识别语种**。自动识别（`source_language` 为 None）时才有；
    /// 只给用户看识别到了什么语言，不改任何行为。
    SourceDetected { language: String },
    /// 源文转写的增量文本（GPT translation 开启 input transcription 时）。
    SourceTranscriptDelta { text: String },
    /// 一段译文语音（已解出的 PCM16LE 24 kHz 单声道字节）。
    AudioDelta { pcm: Vec<u8> },
    /// 服务端 VAD 在上传音频时间线上检测到语音开始。
    SpeechStarted {
        audio_start_ms: u64,
        item_id: Option<String>,
    },
    /// 服务端 VAD 检测到本轮语音结束。
    SpeechStopped {
        audio_end_ms: u64,
        item_id: Option<String>,
    },
    /// 一轮结束，带 token 用量。
    TurnDone { usage: TurnUsage },
    /// 会话配置已生效。
    SessionUpdated,
    /// 服务端报错。
    Error {
        code: Option<String>,
        message: String,
    },
    /// 没接的事件类型，留着给调试面板看。
    Other { event_type: String },
}

/// 解析出的东西 + 原始事件名（日志/调试面板要显示原名）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEvent {
    pub event_type: String,
    pub event: ServerEvent,
}

/// 增量拼装器。服务端一小片一小片地给，字幕层要的是整句。
#[derive(Debug, Default)]
pub struct Decoder {
    parts: String,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前拼到哪了。
    pub fn pending(&self) -> &str {
        &self.parts
    }

    /// 丢掉没说完的半句（换会话、被抢占时用）。
    pub fn reset(&mut self) {
        self.parts.clear();
    }

    /// 吃一条服务端消息。解析不出 JSON 就返回 `None`——网络上的垃圾不该让流水线炸。
    pub fn decode(&mut self, message: &str) -> Option<ParsedEvent> {
        let value: Value = serde_json::from_str(message).ok()?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let event = match event_type.as_str() {
            // qwen3.8 的 transcript 增量在 `delta`；老形状放在 `transcript`，两个都认。（坑 2）
            "response.audio_transcript.delta" => self
                .accumulate(str_field(&value, "delta").or_else(|| str_field(&value, "transcript"))),
            "response.text.delta" | "response.output_text.delta" => {
                self.accumulate(str_field(&value, "delta"))
            }
            // LiveTranslate 的流式译文走 `.text`，字段是 `text` + 可能被改写的 `stash`。
            // `text` 是已确认的前缀（**整句替换**累积的那一半），`stash` 是临时尾巴。
            "response.text.text"
            | "response.output_text.text"
            | "response.audio_transcript.text" => self.set_text_full(
                str_field(&value, "text").unwrap_or_default(),
                str_field(&value, "stash").unwrap_or_default(),
            ),
            "response.audio.delta" => audio_delta(&value, &event_type),
            "input_audio_buffer.speech_started" => ServerEvent::SpeechStarted {
                audio_start_ms: value
                    .get("audio_start_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                item_id: str_field(&value, "item_id").map(str::to_string),
            },
            "input_audio_buffer.speech_stopped" => ServerEvent::SpeechStopped {
                audio_end_ms: value
                    .get("audio_end_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                item_id: str_field(&value, "item_id").map(str::to_string),
            },
            "response.audio_transcript.done" => self.finish(str_field(&value, "transcript")),
            "response.text.done" | "response.output_text.done" => {
                self.finish(str_field(&value, "text"))
            }
            "response.done" => {
                // 一轮说完了，半句作废。
                self.parts.clear();
                ServerEvent::TurnDone {
                    usage: usage_from_response(&value),
                }
            }
            "session.updated" => ServerEvent::SessionUpdated,
            // 源文转写回报识别到的语种（自动识别时才有）。字段名没钉死过，
            // 顶层有 `language` 就认，嵌套在 `input_audio_transcription` 里也认。
            // 这只是给用户看的小字，识别不到就当成没接过的事件，绝不打断流水线。
            "conversation.item.input_audio_transcription.delta"
            | "conversation.item.input_audio_transcription.done"
            | "conversation.item.input_audio_transcription.completed" => {
                let language = str_field(&value, "language")
                    .or_else(|| {
                        value
                            .get("input_audio_transcription")
                            .and_then(|v| v.get("language"))
                            .and_then(Value::as_str)
                    })
                    .filter(|l| !l.is_empty());
                match language {
                    Some(lang) => ServerEvent::SourceDetected {
                        language: lang.to_string(),
                    },
                    None => ServerEvent::Other {
                        event_type: event_type.clone(),
                    },
                }
            }
            "error" | "response.error" => error_event(&value, "服务端返回了错误，但没说原因"),
            _ => ServerEvent::Other {
                event_type: event_type.clone(),
            },
        };

        Some(ParsedEvent { event_type, event })
    }

    /// 攒一片，吐出到目前为止的整句。（坑 3）
    fn accumulate(&mut self, piece: Option<&str>) -> ServerEvent {
        match assembled(&mut self.parts, piece) {
            Some(text) => ServerEvent::TextDelta {
                // delta 类型的累积是单调增长的整句，整句就是已确认部分。
                confirmed: Some(text.clone()),
                text,
            },
            // 空增量当没发生过。
            None => ServerEvent::Other {
                event_type: "response.text.delta".to_string(),
            },
        }
    }

    /// LiveTranslate 的 `.text` 事件：`confirmed` 是**已确认的整句前缀**（整体替换
    /// 累积那半），`stash` 是可能被改写的临时尾巴。渲染值 = `confirmed + stash`。
    ///
    /// `text` 是确认前缀而不是增量片段，所以**替换** `self.parts`（不能往上追加），
    /// 否则订正会叠字。`stash` 每次来都重算、不落进 `parts`——它只临时拼进渲染值，
    /// 最后以 `.done` 的终稿为准（§5.0）。
    fn set_text_full(&mut self, confirmed: &str, stash: &str) -> ServerEvent {
        let render = format!("{confirmed}{stash}");
        // 空渲染值（例如只剩空 stash）当没接过，别把已确认的字清掉。
        if render.is_empty() {
            return ServerEvent::Other {
                event_type: "response.text.text".to_string(),
            };
        }
        self.parts = confirmed.to_string();
        ServerEvent::TextDelta {
            text: render,
            // 渲染值里只有 confirmed 是稳的（stash 每次都会被服务端重写），
            // 对外逐字推进只该用 confirmed 这段，别拿会变的尾巴占位置。
            confirmed: Some(confirmed.to_string()),
        }
    }

    /// 收尾。done 里没带文字就拿累积的凑。（坑 4）
    fn finish(&mut self, text: Option<&str>) -> ServerEvent {
        let final_text = match text.filter(|t| !t.is_empty()) {
            Some(t) => t.to_string(),
            None => std::mem::take(&mut self.parts),
        };
        self.parts.clear();
        ServerEvent::TextDone { text: final_text }
    }
}

/// 取一个 JSON 字符串字段。OpenAI 那个解码器（`cloud/gpt.rs`）也用它。
pub(super) fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// 把一片增量攒进 `parts`，返回**到目前为止拼好的整句**（不是这一小片）；
/// 空增量返回 `None`，当没发生过。（坑 3）
///
/// Aliyun 的 text/audio transcript 和 OpenAI 的 output/input transcript
/// 累积方式一模一样，共用这一份。
pub(super) fn assembled(parts: &mut String, piece: Option<&str>) -> Option<String> {
    parts.push_str(piece.filter(|p| !p.is_empty())?);
    Some(parts.clone())
}

/// `*.audio.delta`：base64 → PCM16LE 字节。
///
/// 空的或解不开的当"没接过的事件"处理，**不要伪造静音塞给播放器**。
/// Aliyun 和 OpenAI 两家的字段名、载荷都一样，只有事件名不同，所以共用一份。
pub(super) fn audio_delta(value: &Value, event_type: &str) -> ServerEvent {
    match str_field(value, "delta") {
        Some(b64) => match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(pcm) if !pcm.is_empty() => ServerEvent::AudioDelta { pcm },
            _ => ServerEvent::Other {
                event_type: event_type.to_string(),
            },
        },
        None => ServerEvent::Other {
            event_type: event_type.to_string(),
        },
    }
}

/// 错误事件：错误对象有时在 `error` 字段里，有时就是顶层对象本身。
/// 没带 message 时用 `fallback` 兜底——两家的措辞不同，由调用方给。
pub(super) fn error_event(value: &Value, fallback: &str) -> ServerEvent {
    let err = value.get("error").unwrap_or(value);
    ServerEvent::Error {
        code: str_field(err, "code").map(str::to_string),
        message: str_field(err, "message").unwrap_or(fallback).to_string(),
    }
}

/// 从 `response.done` 里掏 usage。字段名跟服务端一致，缺的当 0。
/// 服务端偶尔把 usage 放在顶层而不是 `response` 里，两处都看一眼。
fn usage_from_response(value: &Value) -> TurnUsage {
    let usage = value
        .get("response")
        .and_then(|r| r.get("usage"))
        .or_else(|| value.get("usage"));
    usage
        .and_then(|u| serde_json::from_value::<TurnUsage>(u.clone()).ok())
        .unwrap_or_default()
}

// --- PCM 转换 --------------------------------------------------------------

/// f32 → PCM16LE。**照抄旧版的做法**：先夹到 ±1.0，负半轴乘 32768、
/// 正半轴乘 32767，这样 -1.0 正好落在 i16::MIN 而 +1.0 不会溢出翻符号。
pub fn float_to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clipped = s.clamp(-1.0, 1.0);
        let scaled = if clipped < 0.0 {
            clipped * 32768.0
        } else {
            clipped * 32767.0
        };
        out.extend_from_slice(&(scaled as i16).to_le_bytes());
    }
    out
}

/// PCM16LE → f32，给播放侧用。半个样本的尾巴直接丢掉。
pub fn pcm16_to_float(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = "qwen3.8-livetranslate-flash-realtime";
    const LIVE35: &str = "qwen3.5-livetranslate-flash-realtime";

    fn cn() -> AliyunEndpoint {
        AliyunEndpoint {
            region: AliyunRegion::Cn,
            custom: String::new(),
        }
    }

    fn speak_params() -> SessionParams {
        SessionParams {
            model_name: LIVE.to_string(),
            target_language: "ja".to_string(),
            voice: Some("Tina".to_string()),
            clone_frequency: None,
            source_language: None,
        }
    }

    fn speak_params_for(model: &str) -> SessionParams {
        SessionParams {
            model_name: model.to_string(),
            ..speak_params()
        }
    }

    #[test]
    fn endpoint_carries_the_model_and_normalizes_junk() {
        assert_eq!(
            endpoint_url(LIVE, &cn()),
            format!("{}?model={LIVE}", catalog::API_BASE)
        );
        // 认不出的模型名要归一到推荐模型，不能原样拼进 URL。
        assert!(endpoint_url("不存在的模型", &cn()).ends_with(catalog::DEFAULT_MODEL_NAME));
        // 认得出的（含快照后缀）要原样带上。
        let snapshot = "qwen3.5-livetranslate-flash-realtime-2026-05-19";
        assert!(endpoint_url(snapshot, &cn()).ends_with(snapshot));
    }

    /// 接入点的三级优先级：环境变量 > 设置里的自定义 > 地区缺省。
    #[test]
    fn endpoint_priority_is_env_then_setting_then_region() {
        let custom = "wss://ws-abc.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime";
        let from_env = "wss://env.example.com/api-ws/v1/realtime";
        let setting = AliyunEndpoint {
            region: AliyunRegion::Intl,
            custom: custom.to_string(),
        };

        // 环境变量赢：连地区和自定义一起压过去。
        assert_eq!(
            endpoint_url_with(Some(from_env), &setting, LIVE),
            format!("{from_env}?model={LIVE}")
        );
        // 没有环境变量才轮到设置里的自定义地址。
        assert_eq!(
            endpoint_url_with(None, &setting, LIVE),
            format!("{custom}?model={LIVE}")
        );
        // 自定义为空 / 不合法：落到地区缺省（国际站的 Key 连北京会被拒，
        // 所以地区必须真的换掉主机，而不是只换个名字）。
        for junk in ["", "  ", "https://example.com", "?model=x"] {
            assert_eq!(
                endpoint_url_with(
                    None,
                    &AliyunEndpoint {
                        region: AliyunRegion::Intl,
                        custom: junk.into()
                    },
                    LIVE
                ),
                format!("{}?model={LIVE}", catalog::API_BASE_INTL),
                "{junk:?}"
            );
        }
        // 环境变量本身不合法时不该把垃圾拼进 URL，要落到设置这一级。
        assert_eq!(
            endpoint_url_with(Some("  "), &setting, LIVE),
            format!("{custom}?model={LIVE}")
        );
    }

    /// `ws://` 会把密钥明文发出去：只有本机回环放行，其余（含 `localhost@evil` 这类
    /// 把真主机藏在后面的写法）一律落回地区缺省。
    #[test]
    fn plaintext_ws_is_only_allowed_for_loopback() {
        let url_for = |custom: &str| {
            endpoint_url_with(
                None,
                &AliyunEndpoint {
                    region: AliyunRegion::Cn,
                    custom: custom.into(),
                },
                LIVE,
            )
        };
        for ok in [
            "ws://localhost:8080/realtime",
            "ws://LOCALHOST/realtime",
            "ws://127.0.0.1:9000",
            "ws://127.9.8.7/x",
            "ws://[::1]:9000/x",
        ] {
            assert!(url_for(ok).starts_with(ok), "{ok} 应放行：{}", url_for(ok));
        }
        for bad in [
            "ws://example.com/realtime",
            "ws://10.0.0.5:8080",
            "ws://localhost@evil.com/x",
            "ws://127.0.0.1.evil.com/x",
            "ws://[::2]/x",
        ] {
            assert_eq!(
                url_for(bad),
                format!("{}?model={LIVE}", catalog::API_BASE),
                "{bad} 不该放行"
            );
        }
        // wss 不受影响。
        assert!(url_for("wss://example.com/x").starts_with("wss://example.com/x"));
    }

    /// 模型 ID 要原样进 query：带 `&` 的名字不能靠前缀被保留下来夹带参数。
    #[test]
    fn a_model_id_cannot_smuggle_query_parameters() {
        for junk in [
            "qwen3.5-livetranslate-flash-realtime&x=1",
            "qwen3.8-livetranslate-flash-realtime a",
            "qwen3.8-livetranslate-flash-realtime#frag",
            "",
        ] {
            let url = endpoint_url_with(None, &cn(), junk);
            assert_eq!(
                url,
                format!("{}?model={LIVE}", catalog::API_BASE),
                "{junk:?}"
            );
        }
    }

    #[test]
    fn endpoint_override_takes_the_workspace_url_and_rebuilds_the_query() {
        let ws = "wss://ws-abc.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime";
        // 控制台贴来的地址自带 `?model=`（甚至是别的模型）：去掉重拼，不能拼出两个 `?`。
        let setting = AliyunEndpoint {
            region: AliyunRegion::Intl,
            custom: format!(" {ws}/?model=old "),
        };
        assert_eq!(
            endpoint_url_with(None, &setting, LIVE),
            format!("{ws}?model={LIVE}")
        );
    }

    #[test]
    fn region_picks_the_matching_default_endpoint() {
        assert_eq!(
            endpoint_url_with(
                None,
                &AliyunEndpoint {
                    region: AliyunRegion::Intl,
                    custom: String::new()
                },
                LIVE
            ),
            format!("{}?model={LIVE}", catalog::API_BASE_INTL)
        );
        assert_eq!(
            endpoint_url_with(None, &cn(), LIVE),
            format!("{}?model={LIVE}", catalog::API_BASE)
        );
        assert_ne!(catalog::API_BASE, catalog::API_BASE_INTL);
    }

    #[test]
    fn auth_header_is_a_bearer_token() {
        assert_eq!(auth_header_value("sk-abc"), "Bearer sk-abc");
    }

    #[test]
    fn livetranslate_uses_the_native_translation_param() {
        let session = speak_params().to_session_object();
        assert_eq!(session["translation"]["language"], "ja");
        assert_eq!(session["output_modalities"], json!(["text", "audio"]));
        assert_eq!(session["audio"]["output"]["voice"], "Tina");
        assert_eq!(
            session["audio"]["input"]["format"],
            json!({ "type": "pcm", "sample_rate": INPUT_SAMPLE_RATE })
        );
        assert_eq!(
            session["audio"]["output"]["format"],
            json!({ "type": "pcm", "sample_rate": OUTPUT_SAMPLE_RATE })
        );
        // 3.5 形状的字段一个都不能留：3.8 会话里它们不认。
        for old in [
            "modalities",
            "voice",
            "input_audio_format",
            "output_audio_format",
            "sample_rate",
            "turn_detection",
            "input_audio_transcription",
        ] {
            assert!(session.get(old).is_none(), "不该再发 {old}");
        }
        // 专用翻译模型不靠提示词驱动。
        assert!(session.get("instructions").is_none());
    }

    /// 3.5 的形状与 3.8 **逐字不同**：音色平铺、音频格式是 `pcm` 字符串。
    /// 这一条防的是"方言没分派、两条模型发同一份报文"。
    #[test]
    fn qwen35_uses_the_flat_audio_shape() {
        let session = speak_params_for(LIVE35).to_session_object();
        assert_eq!(session["voice"], "Tina");
        assert_eq!(session["input_audio_format"], "pcm");
        assert_eq!(session["output_audio_format"], "pcm");
        assert_eq!(session["translation"]["language"], "ja");
        // 3.8 那些收在 audio 下的字段在 3.5 里不认，一个都不能出现。
        for nested in ["audio", "output_modalities", "turn_detection"] {
            assert!(session.get(nested).is_none(), "3.5 不该发 {nested}");
        }
        assert_eq!(session["modalities"], json!(["text", "audio"]));
    }

    /// 3.5 缺省会出语音：关语音（无音色 / 目标语言不支持语音）必须显式发 `["text"]`，
    /// 否则服务端照样回音频。
    #[test]
    fn qwen35_text_only_asks_for_text_modality_explicitly() {
        let session = SessionParams::text_only(LIVE35, "ja").to_session_object();
        assert_eq!(session["modalities"], json!(["text"]));
        assert!(session.get("voice").is_none(), "不要语音就别报音色");

        let mut unsupported = speak_params_for(LIVE35);
        unsupported.target_language = "yue".to_string();
        let session = unsupported.to_session_object();
        assert_eq!(session["modalities"], json!(["text"]));
        assert!(session.get("voice").is_none());
    }

    /// 3.5 的声音复刻：音色写死 default，复刻两格按开关发出，关语音时不发。
    #[test]
    fn qwen35_voice_clone_fields_follow_the_switch() {
        let mut params = speak_params_for(LIVE35);
        params.clone_frequency = Some(CloneFrequency::Once);
        let session = params.to_session_object();
        assert_eq!(session["voice"], "default");
        assert_eq!(session["enable_voice_clone"], true);
        assert_eq!(session["voice_clone_options"]["frequency"], "once");

        params.clone_frequency = None;
        let session = params.to_session_object();
        assert!(session.get("enable_voice_clone").is_none());
        assert!(session.get("voice_clone_options").is_none());

        let mut text = SessionParams::text_only(LIVE35, "ja");
        text.clone_frequency = Some(CloneFrequency::Always);
        assert!(text.to_session_object().get("enable_voice_clone").is_none());
    }

    /// 快照模型也得走它自己的方言：3.5 的快照不能被当成 3.8 发出去。
    #[test]
    fn dated_snapshots_pick_their_own_dialect() {
        let session =
            speak_params_for("qwen3.5-livetranslate-flash-realtime-2026-05-19").to_session_object();
        assert_eq!(session["voice"], "Tina");
        assert!(session.get("audio").is_none());
    }

    /// 认不出的模型不发任何音频参数——宁可少发，也不要发一条服务端不认的字段。
    #[test]
    fn an_unmatched_model_only_asks_for_the_translation() {
        let session = speak_params_for("gummy-realtime-v1").to_session_object();
        assert_eq!(
            session,
            json!({ "translation": { "language": "ja" } }),
            "认不出的模型只能发最保守的一格"
        );
    }

    #[test]
    fn livetranslate_sends_source_language_when_opted_in() {
        let mut params = speak_params();
        params.source_language = Some("en".to_string());
        let session = params.to_session_object();
        assert_eq!(session["input_audio_transcription"]["language"], "en");
        // 两条方言都用同一个字段名，3.5 也不例外。
        let mut params35 = speak_params_for(LIVE35);
        params35.source_language = Some("en".to_string());
        assert_eq!(
            params35.to_session_object()["input_audio_transcription"]["language"],
            "en"
        );
    }

    #[test]
    fn text_only_session_defaults_to_auto_detected_source_language() {
        let text_only = SessionParams::text_only(LIVE, "en");
        assert!(text_only
            .to_session_object()
            .get("input_audio_transcription")
            .is_none());
    }

    #[test]
    fn text_only_session_asks_for_no_audio_modality() {
        let session = SessionParams::text_only(LIVE, "en").to_session_object();
        assert_eq!(session["output_modalities"], json!(["text"]));
        assert!(
            session["audio"]["output"].get("voice").is_none(),
            "不要语音就别报音色"
        );
    }

    #[test]
    fn language_without_voice_support_degrades_to_text() {
        let mut params = speak_params();
        // 挑一个只支持文字的目标语言（方言的 `target_text_only` 里有它）。
        params.target_language = "yue".to_string();
        assert!(
            !catalog::supports_audio_output("yue"),
            "前提：这个语言不支持语音输出"
        );
        assert!(!params.audio_enabled());
        let session = params.to_session_object();
        assert_eq!(session["output_modalities"], json!(["text"]));
        assert!(session["audio"]["output"].get("voice").is_none());
    }

    #[test]
    fn voice_clone_pins_the_voice_to_default() {
        let mut params = speak_params();
        params.clone_frequency = Some(CloneFrequency::Always);
        let session = params.to_session_object();
        assert_eq!(
            session["audio"]["output"]["voice"], "default",
            "复刻时音色必须写死 default"
        );
        assert_eq!(session["enable_voice_clone"], true);
        assert_eq!(session["voice_clone_options"]["frequency"], "always");
        assert_eq!(params.effective_voice(), Some("default"));
    }

    #[test]
    fn clone_frequency_maps_from_the_settings_count() {
        assert_eq!(CloneFrequency::from_count(0), None);
        assert_eq!(CloneFrequency::from_count(1), Some(CloneFrequency::Once));
        assert_eq!(CloneFrequency::from_count(9), Some(CloneFrequency::Always));
        assert_eq!(CloneFrequency::Once.as_str(), "once");
    }

    #[test]
    fn session_update_wraps_the_config_with_an_event_id() {
        let event = ClientEvent::SessionUpdate(Box::new(speak_params()));
        let sent: Value = serde_json::from_str(&event.to_json("event_1_1")).unwrap();
        assert_eq!(sent["type"], "session.update");
        assert_eq!(sent["event_id"], "event_1_1");
        assert_eq!(sent["session"]["translation"]["language"], "ja");
    }

    #[test]
    fn audio_append_is_base64_pcm() {
        let pcm = float_to_pcm16(&[0.0, 0.5]);
        let event = ClientEvent::AppendAudio(pcm.clone());
        let sent: Value = serde_json::from_str(&event.to_json("e")).unwrap();
        assert_eq!(sent["type"], "input_audio_buffer.append");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(sent["audio"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn event_ids_are_unique_within_a_millisecond() {
        assert_ne!(event_id(1_700_000, 1), event_id(1_700_000, 2));
    }

    /// 坑 2 + 坑 3：三个增量事件名都要认，且吐的是拼好的整句。
    #[test]
    fn all_three_delta_event_names_accumulate_into_one_sentence() {
        let mut dec = Decoder::new();

        let a = dec
            .decode(r#"{"type":"response.audio_transcript.delta","transcript":"こん"}"#)
            .unwrap();
        assert_eq!(
            a.event,
            ServerEvent::TextDelta {
                text: "こん".into(),
                confirmed: Some("こん".into())
            }
        );

        let b = dec
            .decode(r#"{"type":"response.text.delta","delta":"にち"}"#)
            .unwrap();
        assert_eq!(
            b.event,
            ServerEvent::TextDelta {
                text: "こんにち".into(),
                confirmed: Some("こんにち".into())
            }
        );

        let c = dec
            .decode(r#"{"type":"response.output_text.delta","delta":"は"}"#)
            .unwrap();
        assert_eq!(
            c.event,
            ServerEvent::TextDelta {
                text: "こんにちは".into(),
                confirmed: Some("こんにちは".into())
            },
            "增量要吐到目前为止的整句"
        );
        assert_eq!(dec.pending(), "こんにちは");
    }

    #[test]
    fn done_finalizes_and_clears_the_buffer() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.text.delta","delta":"hel"}"#);
        let done = dec
            .decode(r#"{"type":"response.text.done","text":"hello"}"#)
            .unwrap();
        assert_eq!(
            done.event,
            ServerEvent::TextDone {
                text: "hello".into()
            }
        );
        assert_eq!(dec.pending(), "", "收尾后不能留半句");
    }

    /// qwen3.8：`response.audio_transcript.delta` 的增量在 `delta` 字段，按序累加。
    #[test]
    fn qwen38_audio_transcript_delta_reads_the_delta_field() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.audio_transcript.delta","delta":"Hel"}"#);
        let b = dec
            .decode(r#"{"type":"response.audio_transcript.delta","delta":"lo"}"#)
            .unwrap();
        assert_eq!(
            b.event,
            ServerEvent::TextDelta {
                text: "Hello".into(),
                confirmed: Some("Hello".into())
            }
        );
    }

    /// 坑 4：done 里的文字是空的，要拿累积的凑，不能吐空串。
    #[test]
    fn empty_done_falls_back_to_the_accumulated_text() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.audio_transcript.delta","transcript":"你好"}"#);
        let done = dec
            .decode(r#"{"type":"response.audio_transcript.done","transcript":""}"#)
            .unwrap();
        assert_eq!(
            done.event,
            ServerEvent::TextDone {
                text: "你好".into()
            }
        );
        assert_eq!(dec.pending(), "");
    }

    /// LiveTranslate 的流式文字走 `.text`：`text` 是确认前缀（整体替换累积那半），
    /// `stash` 是可能被改写的尾巴。对外吐的是渲染值 `text + stash`（§5.0）。
    #[test]
    fn livetranslate_text_event_renders_confirmed_plus_stash() {
        let mut dec = Decoder::new();
        let first = dec
            .decode(r#"{"type":"response.text.text","text":"こん","stash":"にち"}"#)
            .unwrap();
        assert_eq!(
            first.event,
            ServerEvent::TextDelta {
                text: "こんにち".into(),
                confirmed: Some("こん".into())
            },
            "渲染值 = 确认前缀 + 尾巴；已确认段单独漏出来给外部逐字推进"
        );
        assert_eq!(dec.pending(), "こん", "确认前缀才进累积，stash 不落账");

        // 前缀长出来、stash 缩回去，整段还是完整一句。
        let second = dec
            .decode(r#"{"type":"response.text.text","text":"こんにち","stash":"は"}"#)
            .unwrap();
        assert_eq!(
            second.event,
            ServerEvent::TextDelta {
                text: "こんにちは".into(),
                confirmed: Some("こんにち".into())
            }
        );
        assert_eq!(dec.pending(), "こんにち");
    }

    /// 服务端订正整句时，`.text` 的 `text` 前缀整体变了——必须替换而不是追加。
    /// 上层的 `push_text` 靠 `strip_prefix` 失败触发 replace，这里保证 Decoder
    /// 把确认前缀整体换掉，不残留旧句的尾巴。
    #[test]
    fn text_event_replaces_the_accumulated_prefix_on_rewrite() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.text.text","text":"これは最初の訳","stash":""}"#);
        assert_eq!(dec.pending(), "これは最初の訳");
        let corrected = dec
            .decode(r#"{"type":"response.text.text","text":"訂正後の訳","stash":"文"}"#)
            .unwrap();
        assert_eq!(
            corrected.event,
            ServerEvent::TextDelta {
                text: "訂正後の訳文".into(),
                confirmed: Some("訂正後の訳".into())
            }
        );
        assert_eq!(
            dec.pending(),
            "訂正後の訳",
            "前缀被整体替换，不能残留旧的确认部分"
        );
    }

    #[test]
    fn audio_transcript_text_is_rendered_the_same_way() {
        let mut dec = Decoder::new();
        let ev = dec
            .decode(r#"{"type":"response.audio_transcript.text","text":"你好","stash":"呀"}"#)
            .unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::TextDelta {
                text: "你好呀".into(),
                confirmed: Some("你好".into())
            }
        );
        assert_eq!(dec.pending(), "你好");
    }

    /// 服务端回报识别到的源文语种（自动识别下才有）。顶层或嵌套
    /// `input_audio_transcription.language` 都认；没有就不当回事。
    #[test]
    fn source_language_is_read_from_transcription_events() {
        let mut dec = Decoder::new();
        let flat = dec
            .decode(r#"{"type":"conversation.item.input_audio_transcription.delta","language":"ja","delta":"こん"}"#)
            .unwrap();
        assert_eq!(
            flat.event,
            ServerEvent::SourceDetected {
                language: "ja".into()
            }
        );

        let nested = dec
            .decode(r#"{"type":"conversation.item.input_audio_transcription.done","input_audio_transcription":{"language":"en"}}"#)
            .unwrap();
        assert_eq!(
            nested.event,
            ServerEvent::SourceDetected {
                language: "en".into()
            }
        );

        // 没有 language 字段就当没接过，绝不炸流水线。
        let silent = dec
            .decode(r#"{"type":"conversation.item.input_audio_transcription.delta"}"#)
            .unwrap();
        assert!(matches!(silent.event, ServerEvent::Other { .. }));
    }

    #[test]
    fn speech_boundaries_keep_audio_offsets_and_item_ids() {
        let mut dec = Decoder::new();
        let started = dec
            .decode(
                r#"{"type":"input_audio_buffer.speech_started","audio_start_ms":568,"item_id":"item-a"}"#,
            )
            .unwrap();
        assert_eq!(
            started.event,
            ServerEvent::SpeechStarted {
                audio_start_ms: 568,
                item_id: Some("item-a".into())
            }
        );
        let stopped = dec
            .decode(
                r#"{"type":"input_audio_buffer.speech_stopped","audio_end_ms":3900,"item_id":"item-a"}"#,
            )
            .unwrap();
        assert_eq!(
            stopped.event,
            ServerEvent::SpeechStopped {
                audio_end_ms: 3900,
                item_id: Some("item-a".into())
            }
        );
    }

    #[test]
    fn audio_delta_is_decoded_into_pcm_bytes() {
        let pcm = float_to_pcm16(&[0.25, -0.25]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm);
        let mut dec = Decoder::new();
        let ev = dec
            .decode(&format!(
                r#"{{"type":"response.audio.delta","delta":"{b64}"}}"#
            ))
            .unwrap();
        assert_eq!(ev.event, ServerEvent::AudioDelta { pcm });
    }

    #[test]
    fn broken_audio_delta_does_not_become_fake_silence() {
        let mut dec = Decoder::new();
        let ev = dec
            .decode(r#"{"type":"response.audio.delta","delta":"!!!not-base64!!!"}"#)
            .unwrap();
        assert!(
            matches!(ev.event, ServerEvent::Other { .. }),
            "解不开的音频不该塞给播放器，实际是 {:?}",
            ev.event
        );
    }

    #[test]
    fn response_done_carries_usage_and_wipes_the_half_sentence() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.text.delta","delta":"半句"}"#);
        let ev = dec
            .decode(
                r#"{"type":"response.done","response":{"usage":{"input_tokens":30,"output_tokens":12,"total_tokens":42}}}"#,
            )
            .unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::TurnDone {
                usage: TurnUsage {
                    input_tokens: 30,
                    output_tokens: 12,
                    total_tokens: 42,
                }
            }
        );
        assert_eq!(dec.pending(), "");
    }

    #[test]
    fn usage_is_also_read_from_the_top_level_and_tolerates_missing_fields() {
        let mut dec = Decoder::new();
        let ev = dec
            .decode(r#"{"type":"response.done","usage":{"input_tokens":5}}"#)
            .unwrap();
        let ServerEvent::TurnDone { usage } = ev.event else {
            panic!("应该是 TurnDone");
        };
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.resolved_total(), 5, "服务端没给总数就自己加");

        // 完全没有 usage 的 response.done 也要收得下。
        let ev = dec
            .decode(r#"{"type":"response.done","response":{}}"#)
            .unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::TurnDone {
                usage: TurnUsage::default()
            }
        );
    }

    #[test]
    fn error_events_surface_code_and_message() {
        let mut dec = Decoder::new();
        let ev = dec
            .decode(
                r#"{"type":"error","error":{"code":"invalid_api_key","message":"Incorrect API key provided."}}"#,
            )
            .unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::Error {
                code: Some("invalid_api_key".into()),
                message: "Incorrect API key provided.".into(),
            }
        );
    }

    #[test]
    fn error_without_a_message_still_says_something() {
        let mut dec = Decoder::new();
        let ev = dec.decode(r#"{"type":"error"}"#).unwrap();
        let ServerEvent::Error { message, code } = ev.event else {
            panic!("应该是 Error");
        };
        assert!(code.is_none());
        assert!(!message.is_empty(), "错误消息不能是空的");
    }

    #[test]
    fn session_updated_and_unknown_events_are_kept_apart() {
        let mut dec = Decoder::new();
        assert_eq!(
            dec.decode(r#"{"type":"session.updated","session":{}}"#)
                .unwrap()
                .event,
            ServerEvent::SessionUpdated
        );
        let ev = dec.decode(r#"{"type":"response.created"}"#).unwrap();
        assert_eq!(
            ev.event,
            ServerEvent::Other {
                event_type: "response.created".into()
            }
        );
        assert_eq!(ev.event_type, "response.created", "原名要留着给调试面板");
    }

    #[test]
    fn garbage_json_is_dropped_without_panicking() {
        let mut dec = Decoder::new();
        assert!(dec.decode("这不是 JSON").is_none());
        assert!(dec.decode("").is_none());
        // 是 JSON 但没有 type，当未知事件收下。
        assert_eq!(dec.decode(r#"{"foo":1}"#).unwrap().event_type, "unknown");
    }

    #[test]
    fn reset_drops_the_half_sentence() {
        let mut dec = Decoder::new();
        dec.decode(r#"{"type":"response.text.delta","delta":"半句"}"#);
        dec.reset();
        assert_eq!(dec.pending(), "");
    }

    #[test]
    fn pcm_conversion_hits_the_rails_without_wrapping() {
        // -1.0 落在 i16::MIN，+1.0 落在 i16::MAX，都不能翻符号。
        let bytes = float_to_pcm16(&[-1.0, 1.0, 0.0]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), i16::MIN);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), 0);

        // 超范围的输入先夹再转。
        let clipped = float_to_pcm16(&[-9.0, 9.0]);
        assert_eq!(i16::from_le_bytes([clipped[0], clipped[1]]), i16::MIN);
        assert_eq!(i16::from_le_bytes([clipped[2], clipped[3]]), i16::MAX);
    }

    #[test]
    fn pcm_roundtrips_closely_enough_and_ignores_a_dangling_byte() {
        let original = [0.0f32, 0.5, -0.5, 0.999];
        let back = pcm16_to_float(&float_to_pcm16(&original));
        assert_eq!(back.len(), original.len());
        for (a, b) in original.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
        // 奇数字节数（网络上截断的包）不能 panic。
        assert_eq!(pcm16_to_float(&[0x00, 0x00, 0x7f]).len(), 1);
    }

    #[test]
    fn sample_rates_match_the_old_client() {
        assert_eq!(INPUT_SAMPLE_RATE, 16_000);
        assert_eq!(OUTPUT_SAMPLE_RATE, 24_000);
    }

    #[test]
    fn target_and_source_languages_go_out_as_wire_codes_and_text_only_is_per_dialect() {
        // 同一个目标语言，各方言按自己的表判断出不出语音：阿里云的 sv 出语音，
        // GPT 方言根本不认 sv（不在表里 → 不当语音目标）。
        let mut aliyun = speak_params();
        aliyun.target_language = "sv".to_string();
        assert!(aliyun.audio_enabled());
        let mut gpt = speak_params();
        gpt.model_name = "gpt-realtime-translate".to_string();
        gpt.target_language = "sv".to_string();
        assert!(!gpt.audio_enabled());
        // 线上码：真数据同名；认不出方言的模型名原样发规范码。
        assert_eq!(aliyun.wire_target(), "sv");
        let mut unknown = speak_params();
        unknown.model_name = "nope".to_string();
        unknown.source_language = Some("en".to_string());
        assert_eq!(unknown.wire_target(), "ja");
        assert_eq!(unknown.wire_source(), Some("en"));
        // 方言不支持的源语言不发（= 自动识别）。
        let mut gpt_src = speak_params();
        gpt_src.model_name = "gpt-realtime-translate".to_string();
        gpt_src.source_language = Some("klingon".to_string());
        assert_eq!(gpt_src.wire_source(), None);
    }
}
