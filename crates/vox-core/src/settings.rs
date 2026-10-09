//! 设置：数据形状、默认值、校验、版本迁移。
//!
//! 反序列化时缺字段一律走 `#[serde(default)]` 取默认值，所以老版本的配置文件
//! 加新字段不会读崩。读进来之后一定要过一遍 [`Settings::normalize`]，把越界值
//! 拽回合法范围——UI 和 Runtime 都假定手里的 `Settings` 已经是合法的。
//!
//! API 密钥**不在这里**，走单独的密钥存储（见 [`crate::ports::SecretStore`]），
//! 免得配置文件被随手分享时把密钥漏出去。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::capability::Capability;
use crate::catalog::{
    self, ActivationMode, DEFAULT_MODEL_NAME, DEFAULT_TARGET_LANGUAGE, DEFAULT_VOICE,
};
use crate::hotkey::Hotkey;

/// 配置结构版本号，用于迁移。
pub const SETTINGS_VERSION: u32 = 2;

/// 界面显示语言（UI locale）允许的取值。
pub const UI_LANGUAGES: [&str; 3] = ["zh-CN", "ja-JP", "en"];
/// 默认界面语言。
pub const DEFAULT_UI_LANGUAGE: &str = "zh-CN";

/// 字幕订阅通知的默认去抖间隔（毫秒）：逐 token 的字幕 delta 按它收敛成一次通知。
pub const DEFAULT_TRANSCRIPT_NOTIFY_MS: u32 = 250;

/// 实时翻译服务商。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    #[default]
    Aliyun,
    Gemini,
    Gpt,
}

impl ModelProvider {
    pub const ALL: [Self; 3] = [Self::Aliyun, Self::Gemini, Self::Gpt];

    pub fn as_id(self) -> &'static str {
        match self {
            Self::Aliyun => "aliyun",
            Self::Gemini => "gemini",
            Self::Gpt => "gpt",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        crate::catalog::provider_by_id(id)
    }
}

/// 全部设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// v1 的两条流水线共用模型。仅用于把旧配置迁移到 speak/listen，v2 不再写盘。
    #[serde(skip_serializing)]
    pub model_name: String,
    pub speak: SpeakSettings,
    pub listen: ListenSettings,
    pub subtitle: SubtitleSettings,
    /// 开机自启。
    pub autostart: bool,
    /// 启动时最小化到托盘。
    pub start_minimized: bool,
    /// 界面显示语言（UI locale），二选一：`zh-CN` / `en`。
    /// 与翻译功能的目标/源语言无关——那是业务翻译语种，不是界面语言。
    pub ui_language: String,
    /// Agent 控制面（MCP / CLI）的开关与授权位。
    pub control: ControlSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            model_name: DEFAULT_MODEL_NAME.to_string(),
            speak: SpeakSettings::default(),
            listen: ListenSettings::default(),
            subtitle: SubtitleSettings::default(),
            autostart: false,
            start_minimized: false,
            ui_language: DEFAULT_UI_LANGUAGE.to_string(),
            control: ControlSettings::default(),
        }
    }
}

/// Agent 控制面（MCP / CLI）的设置：**默认全关**。
///
/// 控制面是"把这台设备交给 Agent"的开关，所以装完不开，必须用户主动打开
/// （S1 §2.5.2 的闸门①：用户位默认全 `false`，`vox-mcp` 的 `Grants` 每次调用前读这里）。
/// 字段名逐字对齐 S1 §2.5.2，`vox-mcp` 按名字读它，**不许**在外壳里另立一份。
///
/// 老配置文件没有这一段：`#[serde(default)]` 让它读出来就是"全关"（fail-closed），
/// 而不是"缺字段 = 允许"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlSettings {
    /// 控制面总开关。
    pub enabled: bool,
    /// 监听端口；`0` = 每次启动由系统分配（默认，也是唯一安全的缺省）。
    pub port: u16,
    /// 允许 Agent 用麦克风（`Permission::Microphone`）。
    pub allow_microphone: bool,
    /// 允许 Agent 抓某个程序的声音（`Permission::SystemAudio`）。
    pub allow_system_audio: bool,
    /// 允许 Agent 往可听输出放音（`Permission::AudibleOutput`）。
    pub allow_audible_output: bool,
    /// 允许控制面改用户配置（`compose_endpoint` 的写权限）。
    pub allow_config_write: bool,
    /// 字幕订阅的通知去抖间隔（毫秒）。
    pub transcript_notify_ms: u32,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 0,
            allow_microphone: false,
            allow_system_audio: false,
            allow_audible_output: false,
            allow_config_write: false,
            transcript_notify_ms: DEFAULT_TRANSCRIPT_NOTIFY_MS,
        }
    }
}

/// 对外说话：我的声音 → 外语语音灌进别的软件的麦克风。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeakSettings {
    pub enabled: bool,
    pub provider: ModelProvider,
    /// 模型名在连接 URL 中；当前由能力表固定，保留字段只为兼容旧配置。
    pub model_name: String,
    pub target_language: String,
    /// 当前音色。按语言分别记忆见 `voice_by_language`。
    pub voice: String,
    /// 每种目标语言上次用的音色，切语言时自动带回来。
    pub voice_by_language: BTreeMap<String, String>,
    /// 声音复刻的采样频次；`None` 表示不用复刻。
    pub voice_clone_frequency: Option<u32>,
    /// 麦克风设备名；`None` 表示自动选。
    pub input_device: Option<String>,
    /// 译文语音送去的设备名，正常填 `CABLE Input (VB-Audio Virtual Cable)`。
    pub output_device: Option<String>,
    /// 是否把译文推到字幕轨。
    pub show_translation: bool,
    /// 是否让服务端合成译文语音并推到输出设备。对外说话固定为 `true`；
    /// 字段保留用于兼容设置契约。
    pub speak_translation: bool,
    /// 是否走实时翻译。关掉 = 不调用云端 API，把麦克风原声直通到输出设备
    /// （受激活方式/音量闸门控制，仅在"说话"时送出）。
    pub translate: bool,
    /// 把译文语音额外回放到系统默认播放设备，供本人戴耳机测试。
    pub monitor_translation: bool,
    pub activation_mode: ActivationMode,
    pub hotkey: Hotkey,
    /// 音量阀门阈值（RMS，0..1）。只在 `Toggle` 模式下生效。
    pub gate_threshold: f32,
    /// 本地降噪（RNNoise）。
    pub denoise: bool,
}

impl Default for SpeakSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: ModelProvider::Aliyun,
            model_name: DEFAULT_MODEL_NAME.to_string(),
            target_language: DEFAULT_TARGET_LANGUAGE.to_string(),
            voice: DEFAULT_VOICE.to_string(),
            voice_by_language: BTreeMap::new(),
            voice_clone_frequency: None,
            input_device: None,
            output_device: None,
            show_translation: true,
            speak_translation: true,
            translate: true,
            monitor_translation: false,
            activation_mode: ActivationMode::Toggle,
            hotkey: Hotkey::plain("V"),
            gate_threshold: 0.012,
            denoise: true,
        }
    }
}

/// 听人说话：指定程序的声音 → 中文语音 + 字幕。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenSettings {
    pub enabled: bool,
    pub provider: ModelProvider,
    /// 当前由能力表固定，保留字段只为兼容旧配置。
    pub model_name: String,
    /// 要抓哪个程序的声音。`None` = 还没选。
    pub target: Option<ListenTarget>,
    /// 中文语音播出去的设备名；`None` 表示系统默认（耳机）。
    pub output_device: Option<String>,
    /// 是否把译文推到字幕轨。
    pub show_translation: bool,
    /// 要不要合成中文语音。关掉就只有字幕，省 token。
    pub speak_translation: bool,
    pub voice: String,
    pub hotkey: Option<Hotkey>,
    /// 源语言代码；`None` = 服务端自动识别（默认）。选过就用它锁死识别，
    /// 否则自动识别在短句/混说时可能认错语种（见 docs/architecture/DECISIONS.md B5）。
    pub source_language: Option<String>,
}

impl Default for ListenSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: ModelProvider::Aliyun,
            model_name: DEFAULT_MODEL_NAME.to_string(),
            target: None,
            output_device: None,
            show_translation: true,
            speak_translation: true,
            voice: DEFAULT_VOICE.to_string(),
            hotkey: None,
            source_language: None,
        }
    }
}

/// 要抓的目标程序。存可执行名而不是 PID，这样重启对方进程也还认得。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListenTarget {
    /// 可执行文件名，如 `Discord.exe`。
    pub executable: String,
    /// 给人看的名字，如 `Discord`。
    pub display_name: String,
    /// 连带抓它的子进程（浏览器那种多进程的必须开）。
    #[serde(default = "default_true")]
    pub include_process_tree: bool,
}

/// 悬浮字幕窗的外观与行为。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub visible: bool,
    /// 在 SteamVR/OpenVR 头显中复制显示 Listen 字幕。
    #[serde(default)]
    pub vr_overlay_enabled: bool,
    pub font_family: String,
    pub font_size: u32,
    /// 对外说话那行的字色（暖白）。
    pub speak_color: String,
    /// 听人说话那行的字色（冷白）。
    pub listen_color: String,
    /// 底衬透明度 0..255。
    pub background_alpha: u8,
    /// 一句字幕在最近一次更新后保持可读的时长。
    pub char_ttl_ms: u32,
    /// 整句字幕的淡出时长。
    pub char_fade_ms: u32,
    /// 0 类字（纯噪声/填充词/无意义发音）Lifetime 结束后永久淡到浅灰而非消失：
    /// Lifetime 到了之后用一段比 `char_fade_ms` 更短的淡出压到 `dim_alpha`，位置保留。
    #[serde(default)]
    pub dim_zeros: bool,
    /// 0 类字永久淡化的目标 alpha（0.0..1.0），0 = 跟消失没区别。
    #[serde(default = "default_dim_alpha")]
    pub dim_alpha: f32,
    /// 窗口位置大小；`None` 表示还没摆过，用默认位置。
    pub geometry: Option<OverlayGeometry>,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self {
            visible: true,
            vr_overlay_enabled: false,
            font_family: "Microsoft YaHei UI".to_string(),
            font_size: 30,
            speak_color: "#fff4de".to_string(),
            listen_color: "#eef6ff".to_string(),
            background_alpha: 165,
            char_ttl_ms: 2600,
            char_fade_ms: 900,
            dim_zeros: false,
            dim_alpha: default_dim_alpha(),
            geometry: None,
        }
    }
}

/// 默认字体族。
///
/// Windows 上点名雅黑；Linux 上留空——那边的光栅器（swash）会按
/// Noto CJK → 无衬线 的顺序挑系统里真实存在的字体。写死一个 Windows 字体名
/// 只会让 Linux 用户拿到"找不到字体"的回退路径。
#[cfg(windows)]
pub const DEFAULT_FONT_FAMILY: &str = "Microsoft YaHei UI";
#[cfg(not(windows))]
pub const DEFAULT_FONT_FAMILY: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn default_true() -> bool {
    true
}

fn default_dim_alpha() -> f32 {
    0.3
}

// --- 校验与迁移 ------------------------------------------------------------

/// 阈值允许的范围。上限留 0.2，再高就等于永远闭闸了。
pub const GATE_THRESHOLD_RANGE: (f32, f32) = (0.0, 0.2);
pub const FONT_SIZE_RANGE: (u32, u32) = (12, 96);
pub const CHAR_TTL_RANGE: (u32, u32) = (500, 20_000);
pub const CHAR_FADE_RANGE: (u32, u32) = (0, 5_000);
pub const DIM_ALPHA_RANGE: (f32, f32) = (0.05, 1.0);

/// 控制面端口的合法下界：`0`（每次启动随机）或 ≥ 它（非特权端口）。特权端口（< 1024）
/// 绑不上，也不该由配置文件随手指定；上界就是 `u16` 的 65535。
pub const CONTROL_PORT_MIN: u16 = 1024;
/// 字幕通知的去抖间隔允许范围（毫秒）：下限防"等于没去抖"，上限防"字幕卡半拍"。
pub const TRANSCRIPT_NOTIFY_MS_RANGE: (u32, u32) = (50, 5_000);

impl Settings {
    /// 从 JSON 读，坏了就退回默认（不让一个坏配置卡住启动）。
    pub fn from_json(text: &str) -> Self {
        let mut settings: Self = serde_json::from_str(text).unwrap_or_default();
        settings.migrate();
        settings.normalize();
        settings
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 老版本配置往当前版本搬：推版本号 + 把 v0/v1 的共享模型名分给两条流水线。
    ///
    /// v0/v1 两条流水线共用顶层 `model_name`，v2 起各存一份，所以这里必须显式分配。
    /// 不靠 [`Self::normalize`] 兜底：normalize 会把未知模型名拉回默认值，只有
    /// **当前目录里该服务商只有一个模型**时它和这里的分配才等价；`catalog/*.json`
    /// 一旦给某家加上第二个模型，`normalize_model_for` 就会保留调用方给的合法名字，
    /// 届时靠 normalize 兜底会把用户的旧模型选择悄悄换成默认模型。
    fn migrate(&mut self) {
        if self.version < 2 {
            let legacy =
                catalog::normalize_model_for(self.speak.provider, &self.model_name).to_string();
            self.speak.model_name = legacy.clone();
            self.listen.model_name = legacy;
        }
        self.version = SETTINGS_VERSION;
    }

    /// 把所有越界值拽回合法范围。
    pub fn normalize(&mut self) {
        self.model_name = DEFAULT_MODEL_NAME.to_string();

        // 界面语言只认白名单；手改配置、拼错一律回退中文。
        if !UI_LANGUAGES.contains(&self.ui_language.as_str()) {
            self.ui_language = DEFAULT_UI_LANGUAGE.to_string();
        }

        // 对外说话
        let speak = &mut self.speak;
        // 对外说话的产品定义就是把译音送进目标应用，不能退化成纯字幕。
        speak.speak_translation = true;
        speak.model_name =
            catalog::normalize_model_for(speak.provider, &speak.model_name).to_string();
        speak.target_language = catalog::normalize_language(&speak.target_language).to_string();
        if !catalog::voice_available(&speak.target_language, &speak.voice) {
            speak.voice = catalog::default_voice_for_language(&speak.target_language);
        }
        speak.voice_by_language.retain(|lang, voice| {
            catalog::language_label(lang).is_some() && catalog::voice_available(lang, voice)
        });
        speak
            .voice_by_language
            .insert(speak.target_language.clone(), speak.voice.clone());
        speak.voice_clone_frequency = speak.voice_clone_frequency.filter(|f| *f > 0);
        if !catalog::supports(speak.provider, Capability::VoiceClone) {
            speak.voice_clone_frequency = None;
        }
        speak.gate_threshold = clamp_f32(speak.gate_threshold, GATE_THRESHOLD_RANGE);
        speak.hotkey = speak.hotkey.normalized(&SpeakSettings::default().hotkey);
        speak.input_device = normalize_device(speak.input_device.take());
        speak.output_device = normalize_device(speak.output_device.take());

        // 听人说话：目标语言固定中文，只校音色和设备。
        let listen = &mut self.listen;
        // 听人说话始终保留字幕，只把是否念出来交给用户选择。
        listen.show_translation = true;
        listen.model_name =
            catalog::normalize_model_for(listen.provider, &listen.model_name).to_string();
        if !catalog::voice_available(catalog::LISTEN_TARGET_LANGUAGE, &listen.voice) {
            listen.voice = catalog::default_voice_for_language(catalog::LISTEN_TARGET_LANGUAGE);
        }
        listen.output_device = normalize_device(listen.output_device.take());
        if let Some(target) = &listen.target {
            if target.executable.trim().is_empty() {
                listen.target = None;
            }
        }
        listen.hotkey = listen.hotkey.take().filter(|hk| hk.is_valid());
        // 源语言只认界面那份语言表；表外的代码（手改配置、拼错）一律丢回自动识别。
        // 宁可不锁识别，也不要把一个服务端不认的代码发上去换一次报错。
        listen.source_language = listen
            .source_language
            .take()
            .filter(|lang| catalog::language_label(lang).is_some());
        if !catalog::supports(listen.provider, Capability::SourceLanguage) {
            listen.source_language = None;
        }

        // 字幕
        let sub = &mut self.subtitle;
        if sub.font_family.trim().is_empty() {
            sub.font_family = SubtitleSettings::default().font_family;
        }
        sub.font_size = sub.font_size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1);
        sub.char_ttl_ms = sub.char_ttl_ms.clamp(CHAR_TTL_RANGE.0, CHAR_TTL_RANGE.1);
        sub.char_fade_ms = sub
            .char_fade_ms
            .clamp(CHAR_FADE_RANGE.0, CHAR_FADE_RANGE.1)
            .min(sub.char_ttl_ms);
        if !sub.dim_alpha.is_finite() {
            sub.dim_alpha = SubtitleSettings::default().dim_alpha;
        }
        sub.dim_alpha = sub.dim_alpha.clamp(DIM_ALPHA_RANGE.0, DIM_ALPHA_RANGE.1);
        sub.speak_color =
            normalize_color(&sub.speak_color, &SubtitleSettings::default().speak_color);
        sub.listen_color =
            normalize_color(&sub.listen_color, &SubtitleSettings::default().listen_color);
        if let Some(geo) = &mut sub.geometry {
            geo.width = geo.width.max(160);
            geo.height = geo.height.max(60);
        }

        // Agent 控制面：授权位是布尔，不用夹；只有端口与去抖间隔要拽回合法范围。
        let control = &mut self.control;
        if control.port != 0 && control.port < CONTROL_PORT_MIN {
            control.port = 0;
        }
        control.transcript_notify_ms = control
            .transcript_notify_ms
            .clamp(TRANSCRIPT_NOTIFY_MS_RANGE.0, TRANSCRIPT_NOTIFY_MS_RANGE.1);
    }

    /// 切目标语言：带回该语言上次用的音色。
    pub fn set_speak_language(&mut self, language: &str) {
        let language = catalog::normalize_language(language).to_string();
        let voice = self
            .speak
            .voice_by_language
            .get(&language)
            .cloned()
            .unwrap_or_else(|| catalog::default_voice_for_language(&language));
        self.speak.target_language = language;
        self.speak.voice = voice;
        self.normalize();
    }
}

fn clamp_f32(value: f32, range: (f32, f32)) -> f32 {
    if value.is_finite() {
        value.clamp(range.0, range.1)
    } else {
        range.0
    }
}

/// 空字符串的设备名当成"自动选"。
fn normalize_device(value: Option<String>) -> Option<String> {
    value.filter(|d| !d.trim().is_empty())
}

/// 只接受 `#rrggbb`；不合法就用兜底色。
fn normalize_color(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    let ok = trimmed.len() == 7
        && trimmed.starts_with('#')
        && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        trimmed.to_ascii_lowercase()
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_already_normalized() {
        let mut a = Settings::default();
        let b = a.clone();
        a.normalize();
        assert_eq!(a.speak.target_language, b.speak.target_language);
        assert_eq!(a.subtitle, b.subtitle);
        // 默认日语，normalize 会把当前音色记进 voice_by_language。
        assert_eq!(
            a.speak.voice_by_language.get("ja").map(String::as_str),
            Some("Tina")
        );
    }

    #[test]
    fn empty_json_gives_defaults() {
        let s = Settings::from_json("{}");
        assert_eq!(s.version, SETTINGS_VERSION);
        assert_eq!(s.speak.model_name, DEFAULT_MODEL_NAME);
        assert_eq!(s.listen.model_name, DEFAULT_MODEL_NAME);
        assert_eq!(s.speak.activation_mode, ActivationMode::Toggle);
        assert!(s.speak.denoise);
        assert!(s.speak.show_translation);
        assert!(s.speak.speak_translation);
        assert!(s.listen.show_translation);
        assert!(s.listen.speak_translation);
    }

    #[test]
    fn garbage_json_falls_back_instead_of_panicking() {
        let s = Settings::from_json("not json at all");
        assert_eq!(s.speak.model_name, DEFAULT_MODEL_NAME);
        assert_eq!(s.listen.model_name, DEFAULT_MODEL_NAME);
    }

    #[test]
    fn v1_shared_model_migrates_to_both_pipelines() {
        let s = Settings::from_json(r#"{"version":1,"model_name":"retired-model"}"#);
        assert_eq!(s.version, 2);
        assert_eq!(s.speak.model_name, DEFAULT_MODEL_NAME);
        assert_eq!(s.listen.model_name, DEFAULT_MODEL_NAME);
        let saved: serde_json::Value = serde_json::from_str(&s.to_json()).unwrap();
        assert!(saved.get("model_name").is_none(), "v1 顶层字段不应再写盘");
        assert_eq!(saved["speak"]["model_name"], saved["listen"]["model_name"]);
    }

    #[test]
    fn partial_json_keeps_given_fields_and_defaults_the_rest() {
        let s = Settings::from_json(r#"{"speak":{"target_language":"ko","gate_threshold":5.0}}"#);
        assert_eq!(s.speak.target_language, "ko");
        assert_eq!(
            s.speak.gate_threshold, GATE_THRESHOLD_RANGE.1,
            "越界要被夹回来"
        );
        assert_eq!(
            s.speak.voice, DEFAULT_VOICE,
            "音色跨语言通用，存过的不该被悄悄换掉"
        );
        assert_eq!(s.subtitle.font_size, 30, "没给的字段走默认");
    }

    #[test]
    fn older_json_gets_translation_switch_defaults() {
        let s = Settings::from_json(
            r#"{"version":2,"speak":{"enabled":true},"listen":{"enabled":true}}"#,
        );
        assert!(s.speak.show_translation);
        assert!(s.speak.speak_translation);
        assert!(s.listen.show_translation);
        assert!(s.listen.speak_translation);
    }

    #[test]
    fn gemini_provider_pins_its_model_and_clears_unsupported_options() {
        let settings = Settings::from_json(
            r#"{
                "speak": {
                    "provider": "gemini",
                    "model_name": "qwen3.8-livetranslate-flash-realtime",
                    "voice_clone_frequency": 5
                },
                "listen": {
                    "provider": "gemini",
                    "source_language": "en"
                }
            }"#,
        );
        assert_eq!(settings.speak.model_name, catalog::GEMINI_MODEL_NAME);
        assert_eq!(settings.listen.model_name, catalog::GEMINI_MODEL_NAME);
        assert_eq!(settings.speak.voice_clone_frequency, None);
        assert_eq!(settings.listen.source_language, None);
    }

    #[test]
    fn gpt_provider_pins_catalog_model_and_clears_unsupported_options() {
        let settings = Settings::from_json(
            r#"{
                "speak": {
                    "provider": "gpt",
                    "model_name": "qwen3.8-livetranslate-flash-realtime",
                    "voice_clone_frequency": 3
                },
                "listen": {
                    "provider": "gpt",
                    "source_language": "en"
                }
            }"#,
        );
        assert_eq!(settings.speak.model_name, catalog::GPT_MODEL_NAME);
        assert_eq!(settings.listen.model_name, catalog::GPT_MODEL_NAME);
        assert_eq!(settings.speak.voice_clone_frequency, None);
        assert_eq!(settings.listen.source_language, None);
        assert_eq!(ModelProvider::from_id("gpt"), Some(ModelProvider::Gpt));
        assert_eq!(ModelProvider::Gpt.as_id(), "gpt");
    }

    #[test]
    fn out_of_range_values_get_clamped() {
        let mut s = Settings::default();
        s.speak.gate_threshold = -1.0;
        s.subtitle.font_size = 999;
        s.subtitle.char_ttl_ms = 10;
        s.subtitle.char_fade_ms = 99_999;
        s.subtitle.dim_alpha = 9.0;
        s.subtitle.speak_color = "红色".into();
        s.normalize();
        assert_eq!(s.speak.gate_threshold, 0.0);
        assert_eq!(s.subtitle.font_size, FONT_SIZE_RANGE.1);
        assert_eq!(s.subtitle.char_ttl_ms, CHAR_TTL_RANGE.0);
        assert!(
            s.subtitle.char_fade_ms <= s.subtitle.char_ttl_ms,
            "淡出不能比存活还长"
        );
        assert_eq!(s.subtitle.dim_alpha, DIM_ALPHA_RANGE.1);
        assert_eq!(s.subtitle.speak_color, "#fff4de");
    }

    #[test]
    fn nan_threshold_does_not_survive() {
        let mut s = Settings::default();
        s.speak.gate_threshold = f32::NAN;
        s.subtitle.dim_alpha = f32::NAN;
        s.normalize();
        assert!(s.speak.gate_threshold.is_finite());
        assert!(s.subtitle.dim_alpha.is_finite());
    }

    #[test]
    fn language_switch_remembers_voice_per_language() {
        let mut s = Settings::default();
        s.set_speak_language("zh");
        assert_eq!(s.speak.voice, DEFAULT_VOICE);
        s.speak.voice = "Cindy".into();
        s.normalize();

        s.set_speak_language("en");
        assert_eq!(s.speak.voice, "Tina");

        s.set_speak_language("zh");
        assert_eq!(s.speak.voice, "Cindy", "切回来要带上次那个音色");
    }

    #[test]
    fn invalid_hotkeys_fall_back() {
        let mut s = Settings::default();
        s.speak.hotkey = Hotkey::plain("Tab");
        s.listen.hotkey = Some(Hotkey::plain("Tab"));
        s.normalize();
        assert_eq!(s.speak.hotkey, Hotkey::plain("V"));
        assert!(s.listen.hotkey.is_none(), "非法的可选热键直接清空");
    }

    #[test]
    fn blank_device_names_mean_auto() {
        let mut s = Settings::default();
        s.speak.input_device = Some("   ".into());
        s.speak.output_device = Some("CABLE Input (VB-Audio Virtual Cable)".into());
        s.normalize();
        assert!(s.speak.input_device.is_none());
        assert!(s.speak.output_device.is_some());
    }

    #[test]
    fn invalid_ui_language_falls_back() {
        let mut s = Settings::default();
        assert_eq!(s.ui_language, "zh-CN");
        s.ui_language = "fr".into();
        s.normalize();
        assert_eq!(s.ui_language, "zh-CN", "白名单外的语言码回退中文");
        s.ui_language = "en".into();
        s.normalize();
        assert_eq!(s.ui_language, "en", "白名单内的语言码保留");
        s.ui_language = "ja-JP".into();
        s.normalize();
        assert_eq!(s.ui_language, "ja-JP", "白名单内的语言码保留");
    }

    #[test]
    fn control_starts_disabled_and_old_configs_stay_disabled() {
        // 控制面默认全关：装完不开，必须用户主动打开（S1 §2.5.2 闸门①）。
        let control = ControlSettings::default();
        assert!(!control.enabled);
        assert!(!control.allow_microphone);
        assert!(!control.allow_system_audio);
        assert!(!control.allow_audible_output);
        assert!(!control.allow_config_write);
        assert_eq!(control.port, 0, "缺省不占端口，由系统分配");
        assert_eq!(control.transcript_notify_ms, DEFAULT_TRANSCRIPT_NOTIFY_MS);
        assert_eq!(Settings::default().control, control);

        // 老配置文件里根本没有 `control` 这一段：读出来就是全关（fail-closed），
        // 而不是"缺字段 = 允许"。
        assert_eq!(
            Settings::from_json(r#"{"version":2,"speak":{"enabled":true}}"#).control,
            ControlSettings::default(),
        );

        // 只写了一半的段：给了的留住，没给的按默认补齐——授权位一个都不许自己开。
        let partial =
            Settings::from_json(r#"{"version":2,"control":{"port":47123,"enabled":true}}"#);
        assert_eq!(
            partial.control,
            ControlSettings {
                enabled: true,
                port: 47123,
                ..ControlSettings::default()
            },
        );
    }

    #[test]
    fn control_port_and_notify_interval_get_clamped() {
        let mut s = Settings::default();
        s.control.port = 80;
        s.control.transcript_notify_ms = 0;
        s.normalize();
        assert_eq!(s.control.port, 0, "特权端口绑不上，回退成系统分配");
        assert_eq!(
            s.control.transcript_notify_ms, TRANSCRIPT_NOTIFY_MS_RANGE.0,
            "间隔太小等于没去抖"
        );

        s.control.port = 47123;
        s.control.transcript_notify_ms = 99_999;
        s.normalize();
        assert_eq!(s.control.port, 47123, "合法端口要留住");
        assert_eq!(s.control.transcript_notify_ms, TRANSCRIPT_NOTIFY_MS_RANGE.1);
    }

    #[test]
    fn control_settings_survive_a_round_trip() {
        let mut original = Settings::default();
        original.control.enabled = true;
        original.control.port = 47123;
        original.control.allow_microphone = true;
        original.control.allow_config_write = true;
        original.control.transcript_notify_ms = 500;
        original.normalize();
        assert_eq!(Settings::from_json(&original.to_json()), original);
    }

    #[test]
    fn roundtrips_through_json() {
        let mut original = Settings::default();
        original.speak.enabled = true;
        original.listen.target = Some(ListenTarget {
            executable: "Discord.exe".into(),
            display_name: "Discord".into(),
            include_process_tree: true,
        });
        original.subtitle.geometry = Some(OverlayGeometry {
            x: 100,
            y: 900,
            width: 800,
            height: 120,
        });
        original.normalize();
        let restored = Settings::from_json(&original.to_json());
        assert_eq!(restored, original);
    }
}
