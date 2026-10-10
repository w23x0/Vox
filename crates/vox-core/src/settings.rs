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
    LISTEN_TARGET_LANGUAGE,
};
use crate::hotkey::Hotkey;

/// 配置结构版本号，用于迁移。
pub const SETTINGS_VERSION: u32 = 3;

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

/// 阿里云百炼的地区。API Key 只在注册它的地区有效：国内站的 Key 连北京，
/// 国际站（海外用户）的 Key 连新加坡。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AliyunRegion {
    /// 国内站（北京）。
    #[default]
    Cn,
    /// 国际站（新加坡）。
    Intl,
}

/// 一家服务商的网络出口选择。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// 不经任何代理，直接连。
    #[default]
    Direct,
    /// 跟随系统代理（Linux：`HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`；Windows：注册表）。
    /// 读不到就直连。
    System,
    /// 用 `proxy` 里填的地址。
    Custom,
}

/// 一家服务商的网络设置。
///
/// `proxy` 可以带 `user:pass@`，所以**密码只存在这一个字段里**：`Debug` 不打印它，
/// 对外快照只给脱敏后的地址与"是否带账号"（见 `app/src-tauri/src/dto.rs`）。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct NetworkSetting {
    pub mode: NetworkMode,
    /// `mode = custom` 时用：`http://host:port`、`socks5://…`、`socks5h://…`（远端解析 DNS），
    /// 可带 `user:pass@`。其它模式下忽略但保留，切回来不用重填。
    pub proxy: String,
}

impl NetworkSetting {
    pub fn new(mode: NetworkMode) -> Self {
        Self {
            mode,
            proxy: String::new(),
        }
    }

    /// 去掉 `user:pass@` 之后的代理地址——唯一允许离开设置账本的形态。
    pub fn proxy_display(&self) -> String {
        split_proxy_credentials(&self.proxy).0
    }

    /// 代理地址里是否带了账号（给界面显示"已设置账号密码"）。
    pub fn proxy_has_auth(&self) -> bool {
        split_proxy_credentials(&self.proxy).1
    }
}

/// 把 `scheme://user:pass@host:port` 拆成（去掉 userinfo 的地址，是否带过 userinfo）。
/// 不带 scheme 的 `user:pass@host:port` 同样处理。
pub fn split_proxy_credentials(url: &str) -> (String, bool) {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (host, had) = match authority.rfind('@') {
        Some(at) => (&authority[at + 1..], true),
        None => (authority, false),
    };
    let stripped = match scheme {
        Some(scheme) => format!("{scheme}://{host}{tail}"),
        None => format!("{host}{tail}"),
    };
    (stripped, had)
}

impl std::fmt::Debug for NetworkSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkSetting")
            .field("mode", &self.mode)
            .field(
                "proxy",
                &if self.proxy.is_empty() {
                    "unset"
                } else {
                    "redacted"
                },
            )
            .finish()
    }
}

/// 按服务商分开的网络设置。缺省：阿里云直连（国内站不该绕代理），
/// OpenAI / Gemini 跟随系统代理（国内多数人要靠代理才连得上）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct NetworkSettings {
    pub aliyun: NetworkSetting,
    pub gpt: NetworkSetting,
    pub gemini: NetworkSetting,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            aliyun: NetworkSetting::new(NetworkMode::Direct),
            gpt: NetworkSetting::new(NetworkMode::System),
            gemini: NetworkSetting::new(NetworkMode::System),
        }
    }
}

impl NetworkSettings {
    pub fn of(&self, provider: ModelProvider) -> &NetworkSetting {
        match provider {
            ModelProvider::Aliyun => &self.aliyun,
            ModelProvider::Gpt => &self.gpt,
            ModelProvider::Gemini => &self.gemini,
        }
    }

    pub fn entries_mut(&mut self) -> [&mut NetworkSetting; 3] {
        [&mut self.aliyun, &mut self.gpt, &mut self.gemini]
    }
}

impl Settings {
    /// 能交给界面/事件通道的副本：代理地址里的账号密码去掉。落盘用原值，对外用这个。
    pub fn without_secrets(&self) -> Settings {
        let mut shown = self.clone();
        for entry in shown.network.entries_mut() {
            entry.proxy = entry.proxy_display();
        }
        shown
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
    /// 阿里云百炼走哪个地区的接入点。老配置没有这一格，读出来是国内站（行为不变）。
    pub aliyun_region: AliyunRegion,
    /// 自定义接入点（工作空间专属 `wss://…`）。空 = 按 `aliyun_region` 缺省。
    /// 环境变量 `VOX_ALIYUN_ENDPOINT` 优先级更高，但那是调试后门。
    /// 只认 `wss://`，或指向本机回环的 `ws://`；其他地址建连时视为无效。
    /// **只在下次启动流水线（重新开始翻译）时生效**：运行中的会话不热更新接入点。
    #[serde(default)]
    pub aliyun_endpoint: String,
    /// 每家服务商的网络出口（直连 / 系统代理 / 自定义代理）。
    /// **只在下次启动流水线时生效**，与 `aliyun_endpoint` 一致。
    pub network: NetworkSettings,
    /// 首次启动引导走完或跳过了。老配置没有这一格，读出来是 `false`；
    /// 界面据此决定要不要弹引导（已有任一密钥的老用户由界面补写成 `true`，
    /// 密钥不在设置里，核心不知道）。
    pub onboarding_done: bool,
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
            aliyun_region: AliyunRegion::default(),
            aliyun_endpoint: String::new(),
            network: NetworkSettings::default(),
            onboarding_done: false,
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
    /// 译成哪种语言。老配置没有这一格，读出来是中文（行为不变）。
    pub target_language: String,
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
            target_language: LISTEN_TARGET_LANGUAGE.to_string(),
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
    /// 一条轨的区域在最后一次更新后保持可读的时长（静默多久之后开始淡出）。
    pub char_ttl_ms: u32,
    /// 整块字幕的淡出时长。
    pub char_fade_ms: u32,
    /// 每条轨的区域放几行（1–4）。窗口高度 = 区域数 × 行数 × 行距，
    /// 不随内容变；只随在跑轨数、字号或这一个设置变。
    #[serde(default = "default_lines_per_track")]
    pub lines_per_track: u32,
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
            lines_per_track: 2,
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

fn default_lines_per_track() -> u32 {
    2
}

// --- 校验与迁移 ------------------------------------------------------------

/// 阈值允许的范围。上限留 0.2，再高就等于永远闭闸了。
pub const GATE_THRESHOLD_RANGE: (f32, f32) = (0.0, 0.2);
pub const FONT_SIZE_RANGE: (u32, u32) = (12, 96);
pub const CHAR_TTL_RANGE: (u32, u32) = (500, 20_000);
pub const CHAR_FADE_RANGE: (u32, u32) = (0, 5_000);
pub const DIM_ALPHA_RANGE: (f32, f32) = (0.05, 1.0);
/// 每条轨放几行。1 行 = 只留最新的一句；4 行 = 长引文也读得完。
pub const LINES_PER_TRACK_RANGE: (u32, u32) = (1, 4);

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
        if self.version < 3 {
            self.migrate_languages();
        }
        self.version = SETTINGS_VERSION;
    }

    /// v3：设置里的语言值一律改存登记表的规范码（`ja-JP`→`ja`、`no`→`nb`、`tl`→`fil`…，
    /// 规则见 [`catalog::canonical_language`]）。认不出的值原样留着，交给 `normalize`
    /// 回落到方言缺省。`voice_by_language` 的键同样迁移；新旧写法撞键时规范码那一条优先。
    fn migrate_languages(&mut self) {
        fn canon(value: &mut String) {
            if let Some(code) = catalog::canonical_language(value) {
                *value = code.to_string();
            }
        }
        canon(&mut self.speak.target_language);
        canon(&mut self.listen.target_language);
        if let Some(source) = self.listen.source_language.as_mut() {
            canon(source);
        }
        let old = std::mem::take(&mut self.speak.voice_by_language);
        let (already, legacy): (Vec<_>, Vec<_>) = old
            .into_iter()
            .partition(|(key, _)| catalog::canonical_language(key) == Some(key.as_str()));
        self.speak.voice_by_language.extend(already);
        for (key, voice) in legacy {
            let key = catalog::canonical_language(&key).map_or(key, str::to_string);
            self.speak.voice_by_language.entry(key).or_insert(voice);
        }
    }

    /// 把所有越界值拽回合法范围。
    pub fn normalize(&mut self) {
        self.model_name = DEFAULT_MODEL_NAME.to_string();

        // 界面语言只认白名单；手改配置、拼错一律回退中文。
        if !UI_LANGUAGES.contains(&self.ui_language.as_str()) {
            self.ui_language = DEFAULT_UI_LANGUAGE.to_string();
        }

        // 接入点只裁空白：合法性交给建连处（那里认不出 ws(s):// 就退回地区缺省），
        // 设置这边不替用户改地址，只是不让纯空白被当成"用户填过"。
        self.aliyun_endpoint = self.aliyun_endpoint.trim().to_string();
        // 代理地址同理只裁空白；格式合法性由建连处与界面校验。
        for entry in self.network.entries_mut() {
            entry.proxy = entry.proxy.trim().to_string();
        }

        // 对外说话
        let speak = &mut self.speak;
        // 对外说话的产品定义就是把译音送进目标应用，不能退化成纯字幕。
        speak.speak_translation = true;
        speak.model_name =
            catalog::normalize_model_for(speak.provider, &speak.model_name).to_string();
        speak.target_language = catalog::normalize_speak_target(
            speak.provider,
            &speak.model_name,
            &speak.target_language,
        )
        .to_string();
        if !catalog::voice_available(&speak.target_language, &speak.voice) {
            speak.voice = catalog::default_voice_for_language(&speak.target_language);
        }
        speak.voice_by_language.retain(|lang, voice| {
            catalog::is_registered_language(lang) && catalog::voice_available(lang, voice)
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

        // 听人说话：目标语言表外的代码退回中文（不是对外说话那份缺省）。
        let listen = &mut self.listen;
        // 听人说话始终保留字幕，只把是否念出来交给用户选择。
        listen.show_translation = true;
        listen.model_name =
            catalog::normalize_model_for(listen.provider, &listen.model_name).to_string();
        listen.target_language = catalog::normalize_listen_target(
            listen.provider,
            &listen.model_name,
            &listen.target_language,
        )
        .to_string();
        if !catalog::voice_available(&listen.target_language, &listen.voice) {
            listen.voice = catalog::default_voice_for_language(&listen.target_language);
        }
        listen.output_device = normalize_device(listen.output_device.take());
        if let Some(target) = &listen.target {
            if target.executable.trim().is_empty() {
                listen.target = None;
            }
        }
        listen.hotkey = listen.hotkey.take().filter(|hk| hk.is_valid());
        // 源语言只认当前模型的方言支持的那些；表外的代码（手改配置、拼错）一律丢回自动识别。
        // 宁可不锁识别，也不要把一个服务端不认的代码发上去换一次报错。
        listen.source_language = listen.source_language.take().and_then(|lang| {
            catalog::normalize_source(listen.provider, &listen.model_name, &lang)
                .map(str::to_string)
        });
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
        // 0 是老配置里没有这一项时 serde 兜出来的值，夹回合法区间。
        sub.lines_per_track = sub
            .lines_per_track
            .clamp(LINES_PER_TRACK_RANGE.0, LINES_PER_TRACK_RANGE.1);
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
        assert_eq!(s.version, SETTINGS_VERSION);
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
    fn old_configs_without_onboarding_flag_read_as_not_done() {
        let s: Settings = serde_json::from_str(r#"{"version":3,"ui_language":"en"}"#).unwrap();
        assert!(!s.onboarding_done);
        let s: Settings = serde_json::from_str(r#"{"onboarding_done":true}"#).unwrap();
        assert!(s.onboarding_done);
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

    /// 接入点走设置这一格：老配置没有它（读出来是空 = 按地区缺省），空白不算"填过"。
    #[test]
    fn custom_endpoint_is_optional_and_whitespace_is_not_a_value() {
        assert_eq!(Settings::default().aliyun_endpoint, "");
        let legacy = Settings::from_json(r#"{"version":2,"speak":{"enabled":true}}"#);
        assert_eq!(legacy.aliyun_endpoint, "", "老配置读出来要能连上");

        let mut s = Settings {
            aliyun_endpoint: "  wss://ws1.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime \n"
                .to_string(),
            ..Settings::default()
        };
        s.normalize();
        assert_eq!(
            s.aliyun_endpoint,
            "wss://ws1.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime"
        );
        assert_eq!(
            Settings::from_json(&s.to_json()).aliyun_endpoint,
            s.aliyun_endpoint,
            "接入点要能原样存回去"
        );

        s.aliyun_endpoint = "   ".into();
        s.normalize();
        assert_eq!(s.aliyun_endpoint, "", "纯空白当没填");
    }

    #[test]
    fn network_defaults_are_per_provider_and_the_password_never_prints() {
        let s = Settings::default();
        assert_eq!(
            s.network.of(ModelProvider::Aliyun).mode,
            NetworkMode::Direct
        );
        assert_eq!(s.network.of(ModelProvider::Gpt).mode, NetworkMode::System);
        assert_eq!(
            s.network.of(ModelProvider::Gemini).mode,
            NetworkMode::System
        );
        let legacy = Settings::from_json(r#"{"version":2}"#);
        assert_eq!(legacy.network, NetworkSettings::default(), "老配置按缺省");

        let mut s = Settings::default();
        s.network.gpt = NetworkSetting {
            mode: NetworkMode::Custom,
            proxy: "  socks5h://alice:hunter2@127.0.0.1:1080 ".into(),
        };
        s.normalize();
        assert_eq!(
            s.network.gpt.proxy,
            "socks5h://alice:hunter2@127.0.0.1:1080"
        );
        assert_eq!(Settings::from_json(&s.to_json()).network, s.network);
        let printed = format!("{s:?}");
        assert!(!printed.contains("hunter2"), "Debug 不许带密码：{printed}");

        let shown = s.without_secrets();
        assert_eq!(shown.network.gpt.proxy, "socks5h://127.0.0.1:1080");
        assert!(!serde_json::to_string(&shown).unwrap().contains("hunter2"));
        assert!(s.network.gpt.proxy_has_auth());
        assert!(!shown.network.gpt.proxy_has_auth());
    }

    #[test]
    fn credentials_are_split_off_whatever_the_shape() {
        for (input, want, had) in [
            ("http://u:p@h:8080", "http://h:8080", true),
            ("socks5://u:p@h:1080/", "socks5://h:1080/", true),
            ("u:p@h:8080", "h:8080", true),
            ("http://u:p@w@h:8080", "http://h:8080", true),
            ("http://h:8080", "http://h:8080", false),
            ("", "", false),
        ] {
            assert_eq!(
                split_proxy_credentials(input),
                (want.to_string(), had),
                "{input}"
            );
        }
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

    #[test]
    fn v2_language_values_migrate_to_canonical_codes() {
        // 现存的每一个取值（登记表 60 码）原样保留。
        for language in catalog::LANGUAGES {
            let text = format!(
                r#"{{"version":2,"speak":{{"target_language":"{c}"}},"listen":{{"target_language":"{c}","source_language":"{c}"}}}}"#,
                c = language.code
            );
            let s = Settings::from_json(&text);
            assert_eq!(s.speak.target_language, language.code);
            assert_eq!(s.listen.target_language, language.code);
            assert_eq!(s.listen.source_language.as_deref(), Some(language.code));
        }
        // 区域写法、旧 ISO 码、大小写、下划线 → 规范码；voice_by_language 的键一并迁移，
        // 新旧写法撞键时规范码那一条优先。
        let s = Settings::from_json(
            r#"{"version":2,
                "speak":{"target_language":"ko-KR",
                         "voice_by_language":{"zh-CN":"Old","zh":"Keep","no":"Nor","ja_JP":"Ja"}},
                "listen":{"target_language":"EN_us","source_language":"tl"}}"#,
        );
        assert_eq!(s.version, SETTINGS_VERSION);
        assert_eq!(s.speak.target_language, "ko");
        assert_eq!(s.listen.target_language, "en");
        assert_eq!(s.listen.source_language.as_deref(), Some("fil"));
        assert_eq!(
            s.speak.voice_by_language.get("zh").map(String::as_str),
            Some("Keep")
        );
        assert_eq!(
            s.speak.voice_by_language.get("nb").map(String::as_str),
            Some("Nor")
        );
        assert!(!s.speak.voice_by_language.contains_key("zh-CN"));
        assert!(!s.speak.voice_by_language.contains_key("no"));
        // 认不出的值不会被乱猜，落到方言缺省。
        let s = Settings::from_json(
            r#"{"version":2,"speak":{"target_language":"klingon"},"listen":{"target_language":"??","source_language":"??"}}"#,
        );
        assert_eq!(s.speak.target_language, DEFAULT_TARGET_LANGUAGE);
        assert_eq!(s.listen.target_language, LISTEN_TARGET_LANGUAGE);
        assert_eq!(s.listen.source_language, None);
    }

    #[test]
    fn language_not_supported_by_the_chosen_model_falls_back_to_its_default() {
        // GPT 的方言没有粤语目标：存着粤语 + GPT 的设置，加载后回 GPT 方言缺省。
        let s = Settings::from_json(
            r#"{"version":3,
                "speak":{"provider":"gpt","model_name":"gpt-realtime-translate","target_language":"yue"},
                "listen":{"provider":"gpt","model_name":"gpt-realtime-translate","target_language":"sv"}}"#,
        );
        assert_eq!(s.speak.target_language, "ja");
        assert_eq!(s.listen.target_language, "zh");
        // 同样的语言在阿里云方言里是合法的，原样保留。
        let s = Settings::from_json(
            r#"{"version":3,"speak":{"target_language":"yue"},"listen":{"target_language":"sv"}}"#,
        );
        assert_eq!(s.speak.target_language, "yue");
        assert_eq!(s.listen.target_language, "sv");
    }
}
