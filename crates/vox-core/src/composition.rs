//! 组合清单：一台设备 / 一个端点怎么被装配。
//!
//! 清单是**数据**：能序列化、能比对、能被界面显示、能被 Agent 生成。它只描述"用哪些组件、
//! 按什么顺序、往哪去"，不含任何平台 API，也不含界面文案。
//!
//! 两条流水线**都在架构里**（`DIRECTIONS.md` §10.5-1）：某档宿主做不到的，是**位报 `false`**、
//! 那一格的条目**进不了清单**（[`Composition::of`] 的开门/关门）+ 界面照实说，
//! **不是**把这条腿从类型里删掉。
//!
//! 唯一入口是 [`Composition::of`]（本机派生）与 [`Composition::endpoint`]（端点）；
//! 作业单只由清单构造（`Plan::from`），所以清单不会变成"第二份真源"。

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, CapabilityReport, CapabilityScope, HostFacts};
use crate::cloud::SessionParams;
use crate::event::Pipeline;
use crate::gate::GateConfig;
use crate::ports::PortResult;
use crate::runtime::SessionConfig;
use crate::settings::ModelProvider;

/// 清单格式版本。沿用 `catalog/*.json` 的既有做法。
pub const COMPOSITION_SCHEMA_VERSION: u32 = 1;

/// 一台设备 / 一个端点怎么被装配。
///
/// 注意：**没有** `Eq`——`ops` 里的 [`Op::Gate`] 带 `f32` 阈值，`Eq` 落不下来（设计稿里那行
/// derive 是笔误，这里以类型系统为准）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Composition {
    pub schema_version: u32,
    /// 哪一档宿主这份清单是给谁的（见 [`HostKind`]）。装到别的档位上会被 `missing_on` 挡下。
    pub host: HostKind,
    /// 声音从哪来。JSON 键名固定为 `"in"`。
    pub r#in: Vec<Input>,
    /// 算子链。**数组顺序 = 执行顺序**，缺项即"不装这一节"。
    pub ops: Vec<Op>,
    /// 声音 / 文字往哪去。
    pub out: Vec<Output>,
    pub life: Life,
    pub ui: Ui,
    pub control: Vec<Control>,
    /// `None` = 不接云端（原声直通 / 纯中继端点）。
    pub session: Option<SessionSpec>,
}

/// **哪一档宿主**（不是"哪个进程形态"）。四档**并列**：桌面（Windows / Linux）、手机（Android）、
/// 无屏（Linux ARM64）。
///
/// 不定义 wasm / browser / mcu（MCU 已砍；插件宿主排后）。档位由**外壳自己**声明
/// （`platform::host_kind()`）——只有外壳知道自己是哪一份构建；它是数据（字符串枚举），
/// 芯读它不违反"芯不碰平台 API"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Windows,
    LinuxDesktop,
    Android,
    LinuxHeadless,
}

/// 桌面两档的控制面通道：芯的进程内 API + 外壳的 IPC 命令。
const DESKTOP_CONTROL: &[Control] = &[Control::InprocApi, Control::Ipc];
/// 手机：进程内 API + 本机 loopback 的 http（手机没有 CLI，§2.3.3）。
const ANDROID_CONTROL: &[Control] = &[Control::InprocApi, Control::Http];
/// 无屏：MCP + CLI + 配置文件——没人点界面，三样都从外面来（§2.4）。
const HEADLESS_CONTROL: &[Control] = &[Control::Mcp, Control::Cli, Control::ConfigFile];

/// 档位的**外壳形状**：谁管它的命 / 有没有屏 / 谁在操控它（清单里那三格）。
///
/// 与 [`crate::capability::host_ceiling`] 同一层——**档位属性**，四档各一行写在芯里。
/// 外壳只声明自己是哪一档（[`HostKind`]），这三格由芯按档位算：外壳不填、界面不改写。
///
/// 依据：S0 §2.3.1（桌面）/ §2.3.3（Android）/ §2.4（无屏端点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierShell {
    pub life: Life,
    pub ui: Ui,
    /// 这一档上控制面开了哪些通道。`&'static`：表只有这一份。
    pub control: &'static [Control],
}

impl HostKind {
    /// 四档。上限表、界面文案、S1 的 `describe` 都按它遍历。
    pub const ALL: [HostKind; 4] = [
        Self::Windows,
        Self::LinuxDesktop,
        Self::Android,
        Self::LinuxHeadless,
    ];

    /// 这一档的外壳形状。**清单的 `life` / `ui` / `control` 只有这一个来源**：
    /// [`Composition::of`]（两条腿）与 [`Composition::endpoint`] 都走它。
    pub const fn shell(self) -> TierShell {
        match self {
            // 桌面：一个交互进程 + 托盘 + Tauri/React 界面 + 芯的 API 与外壳的 IPC 命令。
            Self::Windows | Self::LinuxDesktop => TierShell {
                life: Life::Interactive,
                ui: Ui::Gui,
                control: DESKTOP_CONTROL,
            },
            // 手机：麦克风型前台服务（Android 14 起必须从可见 Activity 启动）。
            Self::Android => TierShell {
                life: Life::ForegroundService,
                ui: Ui::Gui,
                control: ANDROID_CONTROL,
            },
            // 无屏：交给 systemd / 容器管，没有屏幕，配置从文件 / 接口来。
            Self::LinuxHeadless => TierShell {
                life: Life::Daemon,
                ui: Ui::None,
                control: HEADLESS_CONTROL,
            },
        }
    }
}

/// 声音从哪来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// 本机麦克风。`device = None` 时用系统默认。
    Mic {
        device: Option<String>,
        /// 切块粒度（ms）。现状由采集组件实现（`vox-dsp` 的 `Blocker`）。
        block_ms: u32,
    },
    /// 抓某个程序放出来的声音（进程环回）。
    ProcessLoopback {
        executable: String,
        include_tree: bool,
        block_ms: u32,
    },
    /// 网络声音进（端点用）。**[S3 目标，现状未实现]**
    NetIn { pipe: String, block_ms: u32 },
    /// 宿主喂进来的声音（插件 / 网页用）。**[目标，现状未实现]**
    HostFeed { pipe: String, block_ms: u32 },
}

impl Input {
    fn kind(&self) -> &'static str {
        match self {
            Self::Mic { .. } => "mic",
            Self::ProcessLoopback { .. } => "process_loopback",
            Self::NetIn { .. } => "net_in",
            Self::HostFeed { .. } => "host_feed",
        }
    }

    /// 这个条目要哪一位（§2.5.3 的映射表）。`None` = 永远可用。
    fn required_capability(&self) -> Option<Capability> {
        match self {
            Self::Mic { .. } => Some(Capability::Mic),
            Self::ProcessLoopback { .. } => Some(Capability::ProgramTap),
            Self::NetIn { .. } => Some(Capability::NetIn),
            // 宿主自己给的声音，永远可用。
            Self::HostFeed { .. } => None,
        }
    }
}

/// 算子链的一节。数组顺序即执行顺序，缺项即"不装这一节"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    /// 交织多声道 → 单声道（`AudioChunk::to_mono`）。
    Mono,
    /// 降噪。`Listen` 现状不装（数字源本来就干净）。
    Denoise,
    /// 音量阀门。字段就是 `GateConfig`。
    ///
    /// 配置放在 `config` 子对象下：`GateConfig` 自己有一个叫 `kind` 的字段，内联会和 tag 撞名。
    Gate { config: GateConfig },
    /// 重采样。`from` 是来源记号，`to` 是协议要的率。
    Resample { from: RateRef, to: RateRef },
}

impl Op {
    /// 规定顺序里的名字（[`Composition::OP_ORDER`] 用的就是这套名字）。
    fn kind(&self) -> &'static str {
        match self {
            Self::Mono => "mono",
            Self::Denoise => "denoise",
            Self::Gate { .. } => "gate",
            Self::Resample { .. } => "resample",
        }
    }
}

/// 采样率引用。不写死数字：数字由 provider 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RateRef {
    /// 采集协商到的率（`CaptureFormat::sample_rate`）。
    Capture,
    /// `Session::input_sample_rate()`（Aliyun/Gemini 16k、GPT 24k）。
    Session,
    /// `OUTPUT_SAMPLE_RATE`（24k）。
    Playback,
}

/// 声音 / 文字往哪去。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Output {
    /// 播放汇。虚拟麦 / 耳机 / 回听**是同一个组件的三种角色**。
    Playback {
        role: PlaybackRole,
        device: Option<String>,
        source: EdgeSource,
    },
    /// 字幕轨（文本出口）。
    Captions {
        track: crate::subtitle::Track,
        source: EdgeSource,
    },
    /// 网络对端（端点用）。**[S3 目标，现状未实现]**
    NetOut { pipe: String, source: EdgeSource },
    /// 宿主收走（插件用）。**[目标，现状未实现]**
    HostSink { pipe: String, source: EdgeSource },
}

impl Output {
    fn kind(&self) -> &'static str {
        match self {
            Self::Playback { .. } => "playback",
            Self::Captions { .. } => "captions",
            Self::NetOut { .. } => "net_out",
            Self::HostSink { .. } => "host_sink",
        }
    }

    fn source(&self) -> EdgeSource {
        match self {
            Self::Playback { source, .. }
            | Self::Captions { source, .. }
            | Self::NetOut { source, .. }
            | Self::HostSink { source, .. } => *source,
        }
    }

    /// 这个条目要哪一位（§2.5.3 的映射表）。`None` = 永远可用。
    fn required_capability(&self) -> Option<Capability> {
        match self {
            Self::Playback { role, .. } => match role {
                PlaybackRole::VirtualMic => Some(Capability::VirtualMic),
                PlaybackRole::Speaker | PlaybackRole::Monitor => None,
            },
            Self::Captions { .. } => Some(Capability::Captions),
            Self::NetOut { .. } => Some(Capability::NetOut),
            Self::HostSink { .. } => None,
        }
    }
}

/// 播放汇的三种角色。**功能不删**：某档宿主做不到虚拟麦就位报 `false` + 界面降级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PlaybackRole {
    /// 译音灌进虚拟麦，给别的程序当麦克风（Windows: VB-CABLE / Linux: PipeWire sink）。
    VirtualMic,
    /// 普通出声（耳机 / 系统默认设备）。
    Speaker,
    /// 额外回听一份到系统默认设备。
    Monitor,
}

/// 这条出口的数据从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EdgeSource {
    /// 算子链直接产出（直通原声走这条）。
    Chain,
    /// 云端会话返回（译音 / 译文走这条）。
    Session,
}

/// 谁管它的命。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Life {
    Interactive,
    Daemon,
    ForegroundService,
    Hosted,
}

/// 界面形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Ui {
    Gui,
    Tui,
    Web,
    None,
}

/// 谁在操控它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Control {
    InprocApi,
    Ipc,
    Cli,
    Mcp,
    Http,
    ConfigFile,
}

/// 云端会话：链的终点，也是 [`EdgeSource::Session`] 那些出口的源头。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SessionSpec {
    pub provider: ModelProvider,
    /// 认不认不重连的热更新（`Plan::hot_update`：只有 Speak 认）。
    pub hot_update: bool,
    /// 上行率。由 provider 决定，不写死数字。
    pub uplink_rate: RateRef,
    /// 下行率。
    pub downlink_rate: RateRef,
    /// 协议参数。**直接复用现有 `SessionParams`**。
    pub params: SessionParams,
}

/// 清单装不上 / 不合法的地方。**一次给全部**，不是遇错就返回（界面与 Agent 都要可读的错误）。
///
/// 线上形状由 [`Serialize`] 的实现定死：**判别键 `kind` + 明细字段**（见那个 `impl`）；
/// 界面的文案与 S1 的 `structuredContent.error.detail` 都从这一份形状读，不另拼字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositionError {
    /// 某个条目的实现依赖一个这台机器没有的能力位。
    MissingCapability { bit: Capability, entry: String },
    /// `ops` 顺序不是规定顺序的子序列（例如 resample 跑到 mono 前面）。
    BadOpOrder { at: usize },
    /// 同一角色出现两次（两个 `virtual_mic`）、或该有出口却没有。
    DuplicateRole(PlaybackRole),
    /// 端点 / 直通清单里出现了 `EdgeSource::Session`，但没有 `session`。
    SessionEdgeWithoutSession,
    /// 有 `session` 却没有任何 `EdgeSource::Session` 的出口（译文凭空消失）。
    SessionWithoutConsumer,
    /// 清单里一个输入口都没有：声音从哪来都没说。
    ///
    /// 外部提交的清单缺输入必须在**第一道闸**（[`Composition::validate`]）就被拒，
    /// 不许拖到 `Plan::from` 才炸。
    MissingInput,
    /// 这份清单不是给这台机器的档位的（拿错了清单）。
    HostMismatch {
        manifest: HostKind,
        machine: HostKind,
    },
}

/// 线上形状：`kind`（判别键，`snake_case` 变体名）+ 明细字段，**不嵌套、不加别的键**。
/// 例：`{"kind":"host_mismatch","manifest":"android","machine":"windows"}`。
///
/// 手写而不用 `#[serde(tag = "kind")]`：`DuplicateRole` 是 newtype 变体，内层 [`PlaybackRole`]
/// 序列化成**字符串**，serde 的内部标签对"装字符串的 newtype 变体"在运行时会报
/// "cannot serialize tagged newtype variant … containing a string"——那种错要等到真报错那一刻
/// 才炸，而 `data.errors` 正是错误路径，歪在这里最贵。手写一份顺带把键名钉成上面那张表。
impl Serialize for CompositionError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::MissingCapability { bit, entry } => {
                map.serialize_entry("kind", "missing_capability")?;
                map.serialize_entry("bit", bit)?;
                map.serialize_entry("entry", entry)?;
            }
            Self::BadOpOrder { at } => {
                map.serialize_entry("kind", "bad_op_order")?;
                map.serialize_entry("at", at)?;
            }
            Self::DuplicateRole(role) => {
                map.serialize_entry("kind", "duplicate_role")?;
                map.serialize_entry("role", role)?;
            }
            Self::SessionEdgeWithoutSession => {
                map.serialize_entry("kind", "session_edge_without_session")?;
            }
            Self::SessionWithoutConsumer => {
                map.serialize_entry("kind", "session_without_consumer")?;
            }
            Self::MissingInput => {
                map.serialize_entry("kind", "missing_input")?;
            }
            Self::HostMismatch { manifest, machine } => {
                map.serialize_entry("kind", "host_mismatch")?;
                map.serialize_entry("manifest", manifest)?;
                map.serialize_entry("machine", machine)?;
            }
        }
        map.end()
    }
}

impl Composition {
    /// 规定顺序：`mono` → `denoise` → `gate` → `resample`。**缺项合法，换序不合法**。
    pub const OP_ORDER: [&'static str; 4] = ["mono", "denoise", "gate", "resample"];

    /// 本机派生（§2.5.3 上下文①）：`SessionConfig` + **这台机器的事实** → 清单。
    ///
    /// 事实只影响两处：① 位为假的条目**不进清单**（Android 那份里就没有 `virtual_mic`）；
    /// ② Speak 走虚拟麦且用户没选设备时，`device` 缺省用 `HostFacts::virtual_mic_device`。
    /// 除此之外清单只由 `SessionConfig` 决定——它**不是**设置的第二份拷贝（视图开关之类的
    /// 设置项进不来）。
    ///
    /// 参数是**事实**而不是报告：位从事实现算（`effective`），缺省设备名也只存在于事实里。
    pub fn of(config: &SessionConfig, facts: &HostFacts) -> PortResult<Self> {
        match config.pipeline {
            Pipeline::Speak => Ok(crate::pipeline::speak::composition(config, facts)),
            Pipeline::Listen => crate::pipeline::listen::composition(config, facts),
        }
    }

    /// 这条清单需要哪些能力位（条目 → 位的映射表见 §2.5.3）。
    ///
    /// 顺序稳定（按 [`Capability::ALL`]），且**去重**：同一份清单里多个条目要同一位只报一次。
    pub fn required_capabilities(&self) -> Vec<Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(|bit| self.requirement(*bit).is_some())
            .collect()
    }

    /// 结构性校验。一次给**全部**问题。
    pub fn validate(&self) -> Result<(), Vec<CompositionError>> {
        let mut errors = Vec::new();

        // 声音从哪来都没说：外部提交的清单在这里就被挡下（第一道闸），不等 `Plan::from`。
        if self.r#in.is_empty() {
            errors.push(CompositionError::MissingInput);
        }

        // 算子链必须是规定顺序的**子序列**：缺项合法，换序（或重复）不合法。
        let mut previous: Option<usize> = None;
        for (at, op) in self.ops.iter().enumerate() {
            let Some(index) = Self::OP_ORDER.iter().position(|kind| *kind == op.kind()) else {
                errors.push(CompositionError::BadOpOrder { at });
                continue;
            };
            if previous.is_some_and(|previous| index <= previous) {
                errors.push(CompositionError::BadOpOrder { at });
            }
            previous = Some(index);
        }

        // 同一角色只能出现一次（两个 `virtual_mic` 没有意义）。
        let mut roles = Vec::new();
        for output in &self.out {
            if let Output::Playback { role, .. } = output {
                if roles.contains(role) {
                    errors.push(CompositionError::DuplicateRole(*role));
                } else {
                    roles.push(*role);
                }
            }
        }

        // 出口与云端会话必须互相成立：有 `session` 的边就得有 `session`，反之亦然。
        let session_edges = self
            .out
            .iter()
            .any(|output| output.source() == EdgeSource::Session);
        if session_edges && self.session.is_none() {
            errors.push(CompositionError::SessionEdgeWithoutSession);
        }
        if self.session.is_some() && !session_edges {
            errors.push(CompositionError::SessionWithoutConsumer);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// 这条清单**装不到**这台机器上的地方（空 = 可以装配）。
    ///
    /// 两类：`MissingCapability`（条目要的位这台机器没有）与 `HostMismatch`（清单的 `host`
    /// 不是这台机器的档位）。**不替调用方改写清单**——本机派生走 [`Composition::of`] 的
    /// 开门/关门，外部提交（S1 的 `compose_endpoint`）走这里硬报错。
    pub fn missing_on(&self, caps: &CapabilityReport) -> Vec<CompositionError> {
        let mut errors = Vec::new();
        if caps.tier != self.host {
            errors.push(CompositionError::HostMismatch {
                manifest: self.host,
                machine: caps.tier,
            });
        }
        for bit in self.required_capabilities() {
            let available = match bit.scope() {
                CapabilityScope::Host => caps.host.get(&bit).is_some_and(|s| s.enabled),
                // provider 位问**清单自己点的那家 provider**：报告里 `speak` / `listen` 两张表
                // 是"当前两条腿各选了什么"的视图，与这份清单未必是同一家。
                CapabilityScope::Provider => self
                    .session
                    .as_ref()
                    .is_some_and(|session| crate::catalog::supports(session.provider, bit)),
            };
            if !available {
                errors.push(CompositionError::MissingCapability {
                    bit,
                    entry: self.requirement(bit).unwrap_or_default(),
                });
            }
        }
        errors
    }

    /// 端点 = 同一模型的最简实例：只有 `in` + `out`，没有 `ops`、没有 `session`、没有界面。
    ///
    /// 档位默认无屏档；装到别的档位由调用方改（改成 `Mic` / `Speaker` 那一套也是同一套类型）。
    pub fn endpoint(pipe_in: &str, pipe_out: &str) -> Self {
        // 三格取无屏档的形状：端点就是无屏这一档的形态（§2.4）。
        let shell = HostKind::LinuxHeadless.shell();
        Self {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            host: HostKind::LinuxHeadless,
            r#in: vec![Input::NetIn {
                pipe: pipe_in.to_string(),
                block_ms: crate::pipeline::INPUT_BLOCK_MS,
            }],
            ops: Vec::new(),
            out: vec![Output::NetOut {
                pipe: pipe_out.to_string(),
                source: EdgeSource::Chain,
            }],
            life: shell.life,
            ui: shell.ui,
            control: shell.control.to_vec(),
            session: None,
        }
    }

    /// "要这一位的条目"叫什么（§2.5.3 那张枢纽表）。`None` = 没有条目要它。
    ///
    /// **正向与反向查同一处**：`required_capabilities`、`missing_on` 的 `entry` 都走它，
    /// 免得"清单要什么"和"报错指谁"变成两张会漂移的表。
    fn requirement(&self, bit: Capability) -> Option<String> {
        for (at, input) in self.r#in.iter().enumerate() {
            if input.required_capability() == Some(bit) {
                return Some(format!("in[{at}]: {}", input.kind()));
            }
        }
        for (at, output) in self.out.iter().enumerate() {
            if output.required_capability() == Some(bit) {
                return Some(format!("out[{at}]: {}", output.kind()));
            }
        }
        let session = self.session.as_ref()?;
        let params = &session.params;
        if bit == Capability::VoiceSelection && params.voice.as_ref().is_some_and(|v| !v.is_empty())
        {
            return Some("session.params.voice".to_string());
        }
        if bit == Capability::VoiceClone && params.clone_frequency.is_some() {
            return Some("session.params.clone_frequency".to_string());
        }
        if bit == Capability::SourceLanguage
            && params
                .source_language
                .as_ref()
                .is_some_and(|l| !l.is_empty())
        {
            return Some("session.params.source_language".to_string());
        }
        if bit == Capability::HotUpdateLanguage && session.hot_update {
            return Some("session.hot_update".to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::UnavailableReason;
    use crate::cloud::protocol::CloneFrequency;
    use crate::pipeline::tests::{listen_config, speak_config};
    use crate::pipeline::Plan;
    use crate::ports::CaptureTarget;
    use crate::subtitle::Track;
    use std::collections::BTreeMap;

    /// 桌面档、一个门都不关（已发货的那两档在采集/播放/字幕上就是这个口径）。
    fn desktop_facts() -> HostFacts {
        HostFacts::all_wired(HostKind::Windows)
    }

    /// Android：没有虚拟麦、抓不了程序（§2.3.2）。
    fn android_facts() -> HostFacts {
        HostFacts {
            host: HostKind::Android,
            off: BTreeMap::from([
                (Capability::ProgramTap, UnavailableReason::Unsupported),
                (Capability::VirtualMic, UnavailableReason::Unsupported),
            ]),
            virtual_mic_device: None,
        }
    }

    fn report(facts: &HostFacts) -> CapabilityReport {
        CapabilityReport::of(facts, ModelProvider::Aliyun, ModelProvider::Aliyun)
    }

    fn derived(config: &SessionConfig, facts: &HostFacts) -> (Composition, Plan) {
        let composition = Composition::of(config, facts).expect("本机派生的清单");
        composition.validate().expect("本机派生出来的清单必须合法");
        let plan = Plan::from(&composition).expect("清单派生出作业单");
        (composition, plan)
    }

    #[test]
    fn composition_errors_go_on_the_wire_as_kind_tagged_objects() {
        // S1 的 `-32602` 把整份 `Vec<CompositionError>` 塞进 `data.errors`：判别键 + 明细字段，
        // 键名与取值都是线路契约（`tests/protocol.rs` 按 `kind` 断形），一格都不许漂。
        let cases: [(CompositionError, serde_json::Value); 7] = [
            (
                CompositionError::MissingCapability {
                    bit: Capability::ProgramTap,
                    entry: "in[0]: process_loopback".to_string(),
                },
                serde_json::json!({
                    "kind": "missing_capability",
                    "bit": "program_tap",
                    "entry": "in[0]: process_loopback",
                }),
            ),
            (
                CompositionError::BadOpOrder { at: 2 },
                serde_json::json!({ "kind": "bad_op_order", "at": 2 }),
            ),
            (
                CompositionError::DuplicateRole(PlaybackRole::VirtualMic),
                serde_json::json!({ "kind": "duplicate_role", "role": "virtual_mic" }),
            ),
            (
                CompositionError::SessionEdgeWithoutSession,
                serde_json::json!({ "kind": "session_edge_without_session" }),
            ),
            (
                CompositionError::SessionWithoutConsumer,
                serde_json::json!({ "kind": "session_without_consumer" }),
            ),
            (
                CompositionError::MissingInput,
                serde_json::json!({ "kind": "missing_input" }),
            ),
            (
                CompositionError::HostMismatch {
                    manifest: HostKind::Android,
                    machine: HostKind::Windows,
                },
                serde_json::json!({
                    "kind": "host_mismatch",
                    "manifest": "android",
                    "machine": "windows",
                }),
            ),
        ];

        let mut kinds = Vec::new();
        for (error, expected) in cases {
            let json = serde_json::to_value(&error).expect("错误要能送上线路");
            assert_eq!(json, expected, "{error:?}");
            let kind = json["kind"].as_str().expect("每条错误都带 kind");
            assert!(
                !kinds.iter().any(|seen| seen == kind),
                "两种变体共用一个 kind：{kind}"
            );
            kinds.push(kind.to_string());
        }
        // 变体数变了就得同步这张表（`Serialize` 是穷尽 match，漏一个变体编译期就红）。
        assert_eq!(kinds.len(), 7, "少了变体：{kinds:?}");
    }

    #[cfg(feature = "json-schema")]
    #[test]
    fn the_manifest_schema_is_generated_from_the_type() {
        // S0 的约束：`Composition` 的 serde 形态**就是** `compose_endpoint` 的 `inputSchema`，
        // 不另写一份。这里验的是"schema 真的描述了那个类型"——必填格、条目的判别键形状。
        let schema =
            serde_json::to_value(schemars::schema_for!(Composition)).expect("schema 是数据");

        // `session` 可缺（直通清单没有会话），其余八格必填；`Option` 不许被写成必填。
        assert_eq!(
            schema["required"],
            serde_json::json!([
                "control",
                "host",
                "in",
                "life",
                "ops",
                "out",
                "schema_version",
                "ui",
            ])
        );
        assert_eq!(
            schema["properties"]["control"]["items"]["$ref"],
            serde_json::json!("#/definitions/Control")
        );

        // 条目都是**带 `kind` 的对象**（S0 §2.1）：三个枚举各有自己的 `kind` 取值集合。
        for (definition, kinds) in [
            (
                "Input",
                vec!["mic", "process_loopback", "net_in", "host_feed"],
            ),
            ("Op", vec!["mono", "denoise", "gate", "resample"]),
            (
                "Output",
                vec!["playback", "captions", "net_out", "host_sink"],
            ),
        ] {
            let variants = schema["definitions"][definition]["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{definition} 该是 oneOf"));
            let actual: Vec<&str> = variants
                .iter()
                .filter_map(|variant| variant["properties"]["kind"]["enum"][0].as_str())
                .collect();
            assert_eq!(actual, kinds, "{definition} 的 kind 取值");
        }
    }

    #[test]
    fn the_manifest_round_trips() {
        let (composition, _) = derived(&speak_config(), &desktop_facts());
        let json = serde_json::to_string_pretty(&composition).expect("清单是数据，要能存能传");
        let back: Composition = serde_json::from_str(&json).expect("也要能原样读回");
        assert_eq!(back, composition);

        // 形状对齐 §2.3.1：`in`/`ops`/`out` 条目**一律带 `kind`**，不做裸字符串简写；
        // `gate` 的配置在 `config` 子对象下（内联会和 tag 撞名）。
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["schema_version"], COMPOSITION_SCHEMA_VERSION);
        assert_eq!(value["host"], "windows");
        assert_eq!(value["in"][0]["kind"], "mic");
        assert_eq!(value["in"][0]["block_ms"], crate::pipeline::INPUT_BLOCK_MS);
        assert_eq!(value["ops"][0]["kind"], "mono");
        assert_eq!(value["ops"][2]["kind"], "gate");
        assert_eq!(value["ops"][2]["config"]["kind"], "manual");
        assert_eq!(value["out"][0]["kind"], "playback");
        assert_eq!(value["out"][0]["role"], "virtual_mic");
        assert_eq!(value["out"][1]["track"], "speak");
        assert_eq!(value["life"], "interactive");
        assert_eq!(value["ui"], "gui");
        assert_eq!(value["control"], serde_json::json!(["inproc_api", "ipc"]));
        assert_eq!(value["session"]["uplink_rate"], "session");
        assert_eq!(value["session"]["params"]["target_language"], "ja");
    }

    /// 清单 → **线上文本** → 解析回来的值：比的是"客户端看到的形状"。
    ///
    /// 中间那一趟 `to_string` 是有意的：`f32` 打印成 JSON 时是 `0.012`，而 `to_value`
    /// 会把 `f32` 先摊成 `f64`（0.012000000104308128）。线上走的是前一种，比的也得是它。
    fn wire(composition: &Composition) -> serde_json::Value {
        let text = serde_json::to_string(composition).expect("清单要能序列化");
        serde_json::from_str(&text).expect("清单要能读回")
    }

    #[test]
    fn the_manifests_match_the_design_documents_instances() {
        // §2.3.1 的两份实例**逐字**：serde 形状就是清单的形状——键名、取值、嵌套一层都不许漂。
        let mut speak = speak_config();
        speak.input_device = None;
        speak.output_device = Some("CABLE Input (VB-Audio Virtual Cable)".to_string());
        speak.gate = crate::gate::GateConfig::level(0.012);
        let actual = wire(&Composition::of(&speak, &desktop_facts()).expect("清单"));
        let expected = serde_json::json!({
            "schema_version": 1,
            "host": "windows",
            "in": [{ "kind": "mic", "device": null, "block_ms": 20 }],
            "ops": [
                { "kind": "mono" },
                { "kind": "denoise" },
                { "kind": "gate", "config": {
                    "kind": "level", "threshold": 0.012, "tail_ms": 600, "preroll_ms": 200
                } },
                { "kind": "resample", "from": "capture", "to": "session" }
            ],
            "out": [
                { "kind": "playback", "role": "virtual_mic",
                  "device": "CABLE Input (VB-Audio Virtual Cable)", "source": "session" },
                { "kind": "captions", "track": "speak", "source": "session" }
            ],
            "life": "interactive",
            "ui": "gui",
            "control": ["inproc_api", "ipc"],
            "session": {
                "provider": "aliyun",
                "hot_update": true,
                "uplink_rate": "session",
                "downlink_rate": "playback",
                "params": {
                    "model_name": "qwen3.5-livetranslate-flash-realtime",
                    "target_language": "ja",
                    "voice": "Tina",
                    "clone_frequency": null,
                    "source_language": null
                }
            }
        });
        assert_eq!(actual, expected, "对外说话那份实例与设计稿漂了");

        let actual = wire(&Composition::of(&listen_config(), &desktop_facts()).expect("清单"));
        let expected = serde_json::json!({
            "schema_version": 1,
            "host": "windows",
            "in": [{ "kind": "process_loopback", "executable": "Discord.exe",
                     "include_tree": true, "block_ms": 20 }],
            "ops": [
                { "kind": "mono" },
                { "kind": "gate", "config": {
                    "kind": "level", "threshold": 0.0, "tail_ms": 600, "preroll_ms": 200
                } },
                { "kind": "resample", "from": "capture", "to": "session" }
            ],
            "out": [
                { "kind": "playback", "role": "speaker", "device": null, "source": "session" },
                { "kind": "captions", "track": "listen", "source": "session" }
            ],
            "life": "interactive",
            "ui": "gui",
            "control": ["inproc_api", "ipc"],
            "session": {
                "provider": "aliyun",
                "hot_update": false,
                "uplink_rate": "session",
                "downlink_rate": "playback",
                "params": {
                    "model_name": "qwen3.5-livetranslate-flash-realtime",
                    "target_language": "zh",
                    "voice": "Tina",
                    "clone_frequency": null,
                    "source_language": null
                }
            }
        });
        assert_eq!(actual, expected, "听人说话那份实例与设计稿漂了");
    }

    #[test]
    fn plan_is_derived_from_the_manifest() {
        // §2.3.1 的两份实例 → 作业单：**逐字段与现状相等**（"清单中转不改行为"的硬证据）。
        let facts = desktop_facts();

        let speak = speak_config();
        let (_, plan) = derived(&speak, &facts);
        assert_eq!(plan.target, CaptureTarget::Microphone(None));
        assert!(plan.denoise, "麦克风收的是空气声，必须降噪");
        assert!(!plan.passthrough);
        assert_eq!(plan.playback_device, Some(Some("CABLE Input".to_string())));
        assert!(!plan.monitor_translation);
        assert!(plan.hot_update, "只有对外说话认热更新");
        assert_eq!(plan.params.model_name, speak.model_name);
        assert_eq!(plan.params.target_language, "ja");
        assert_eq!(plan.params.voice.as_deref(), Some("Tina"));
        assert_eq!(plan.params.clone_frequency, None);
        assert_eq!(plan.params.source_language, None, "对外说话不做源文识别");

        let listen = listen_config();
        let (_, plan) = derived(&listen, &facts);
        assert_eq!(
            plan.target,
            CaptureTarget::ProcessLoopback {
                executable: "Discord.exe".to_string(),
                include_tree: true,
            }
        );
        assert!(!plan.denoise, "数字源本来就干净，不降噪");
        assert!(!plan.passthrough);
        assert_eq!(plan.playback_device, Some(None));
        assert!(!plan.monitor_translation);
        assert!(!plan.hot_update, "听的方向永远译成中文，不许热改");
        assert_eq!(plan.params.target_language, "zh");
        assert_eq!(plan.params.voice.as_deref(), Some("Tina"));

        // 枚举 `pipeline × translate × 有没有音色`：每一格都要 validate 通过，且
        // 派生结果与"清单里那一格"一一对上（§4.2-2 的对照表）。
        for pipeline in [Pipeline::Speak, Pipeline::Listen] {
            for translate in [true, false] {
                for voice in [true, false] {
                    let mut config = match pipeline {
                        Pipeline::Speak => speak_config(),
                        Pipeline::Listen => listen_config(),
                    };
                    config.translate = translate;
                    if !voice {
                        config.voice = None;
                    }
                    let (composition, plan) = derived(&config, &facts);
                    let label = format!("{pipeline:?} translate={translate} voice={voice}");

                    // 直通 = 没有云端会话。听人说话这条腿不看 `translate`（现状就是恒接云端）。
                    assert_eq!(plan.passthrough, composition.session.is_none(), "{label}");
                    assert_eq!(
                        plan.passthrough,
                        !translate && pipeline == Pipeline::Speak,
                        "{label}"
                    );
                    assert_eq!(
                        plan.denoise,
                        composition.ops.iter().any(|op| matches!(op, Op::Denoise)),
                        "{label}"
                    );
                    assert_eq!(
                        plan.hot_update,
                        translate && pipeline == Pipeline::Speak,
                        "{label}"
                    );
                    assert_eq!(
                        plan.hot_update,
                        composition
                            .session
                            .as_ref()
                            .is_some_and(|session| session.hot_update),
                        "{label}"
                    );
                    // 回听默认关着，所以清单里不该有 `Monitor` 那一格。
                    assert!(!plan.monitor_translation, "{label}");
                    assert!(composition.out.iter().all(|output| !matches!(
                        output,
                        Output::Playback {
                            role: PlaybackRole::Monitor,
                            ..
                        }
                    )));
                    // 播放设备：直通恒出声（送原声），带翻译时跟着"有没有音色"走。
                    let silent = !voice && (translate || pipeline == Pipeline::Listen);
                    assert_eq!(
                        plan.playback_device,
                        if silent {
                            None
                        } else {
                            Some(config.output_device.clone())
                        },
                        "{label}"
                    );
                    // 参数：有会话时就是清单里那份。
                    if let Some(session) = &composition.session {
                        assert_eq!(plan.params, session.params, "{label}");
                    }
                }
            }
        }
    }

    #[test]
    fn speak_manifest_names_the_virtual_mic() {
        let (composition, _) = derived(&speak_config(), &desktop_facts());
        let ops: Vec<&str> = composition.ops.iter().map(Op::kind).collect();
        assert_eq!(
            ops,
            Composition::OP_ORDER.to_vec(),
            "采集 → mono → denoise → gate → resample"
        );

        let playback = composition
            .out
            .iter()
            .find_map(|output| match output {
                Output::Playback {
                    role,
                    device,
                    source,
                } => Some((*role, device.clone(), *source)),
                _ => None,
            })
            .expect("对外说话要有播放出口");
        assert_eq!(playback.0, PlaybackRole::VirtualMic);
        assert_eq!(playback.1, Some("CABLE Input".to_string()));
        assert_eq!(playback.2, EdgeSource::Session, "译音来自云端会话");
    }

    #[test]
    fn listen_manifest_has_no_denoise_and_an_always_open_gate() {
        let (composition, _) = derived(&listen_config(), &desktop_facts());
        let ops: Vec<&str> = composition.ops.iter().map(Op::kind).collect();
        assert_eq!(ops, vec!["mono", "gate", "resample"]);
        match &composition.ops[1] {
            Op::Gate { config } => {
                assert_eq!(config.kind, crate::gate::GateKind::Level);
                assert_eq!(config.threshold, 0.0, "环回复制的底噪是假信号，无条件放行");
            }
            other => panic!("第二节该是阀门，结果是 {other:?}"),
        }
        // 中文语音走系统默认输出：**不能**推虚拟麦，不然对方会听到自己的译文。
        assert!(composition.out.iter().all(|output| !matches!(
            output,
            Output::Playback {
                role: PlaybackRole::VirtualMic,
                ..
            }
        )));
        assert!(composition.out.iter().any(|output| matches!(
            output,
            Output::Playback {
                role: PlaybackRole::Speaker,
                ..
            }
        )));
        assert!(composition.out.iter().any(|output| matches!(
            output,
            Output::Captions {
                track: Track::Listen,
                ..
            }
        )));
    }

    #[test]
    fn passthrough_manifest_drops_the_session_and_the_captions() {
        let mut config = speak_config();
        config.translate = false;
        let (composition, plan) = derived(&config, &desktop_facts());

        assert!(composition.session.is_none(), "关掉翻译就不接云端");
        assert!(plan.passthrough);
        assert!(
            composition
                .out
                .iter()
                .all(|output| output.source() == EdgeSource::Chain),
            "没有会话，就没有任何边挂在会话上"
        );
        assert!(
            !composition
                .out
                .iter()
                .any(|output| matches!(output, Output::Captions { .. })),
            "没接云端就没有文字，字幕出口也不该在"
        );
        assert!(
            composition.ops.iter().any(|op| matches!(op, Op::Denoise)),
            "降噪照旧：直通的还是麦克风收的空气声"
        );
        assert!(
            !composition
                .ops
                .iter()
                .any(|op| matches!(op, Op::Resample { .. })),
            "直通不上传，不需要重采样到协议率"
        );
        // 直通送的是原声，但仍然送往设置的输出设备（一般是虚拟麦）。
        assert_eq!(plan.playback_device, Some(Some("CABLE Input".to_string())));
    }

    #[test]
    fn text_only_leg_has_no_playback() {
        for mut config in [speak_config(), listen_config()] {
            config.voice = None;
            let (composition, plan) = derived(&config, &desktop_facts());
            assert!(
                !composition
                    .out
                    .iter()
                    .any(|output| matches!(output, Output::Playback { .. })),
                "{:?}: 不要语音就不该开播放汇",
                config.pipeline
            );
            assert!(plan.playback_device.is_none());
            assert!(plan.params.voice.is_none());
        }
    }

    #[test]
    fn the_monitor_entry_becomes_the_monitor_flag() {
        let mut config = speak_config();
        config.monitor_translation = true;
        let (composition, plan) = derived(&config, &desktop_facts());
        assert!(composition.out.iter().any(|output| matches!(
            output,
            Output::Playback {
                role: PlaybackRole::Monitor,
                ..
            }
        )));
        assert!(plan.monitor_translation);

        // 没有译音就没有回听：这一格不该进清单。
        config.voice = None;
        let (composition, plan) = derived(&config, &desktop_facts());
        assert!(!composition.out.iter().any(|output| matches!(
            output,
            Output::Playback {
                role: PlaybackRole::Monitor,
                ..
            }
        )));
        assert!(!plan.monitor_translation);
    }

    #[test]
    fn an_endpoint_is_the_minimal_instance() {
        let endpoint = Composition::endpoint("default", "default");
        endpoint.validate().expect("端点清单必须合法");
        assert!(endpoint.ops.is_empty());
        assert_eq!(endpoint.ui, Ui::None);
        assert_eq!(endpoint.life, Life::Daemon);
        assert_eq!(endpoint.host, HostKind::LinuxHeadless);
        assert!(endpoint.session.is_none());
        assert!(
            !endpoint.control.contains(&Control::Ipc),
            "手机/无屏没有 IPC"
        );
        assert_eq!(
            endpoint.r#in,
            vec![Input::NetIn {
                pipe: "default".to_string(),
                block_ms: crate::pipeline::INPUT_BLOCK_MS
            }]
        );
        // 端点今天**跑不起来**：`net_in` / `net_out` 还没有定义者（S3）。
        assert!(
            Plan::from(&endpoint).is_err(),
            "网络进/网络出还没实现，不许被装成今天的作业单"
        );
    }

    /// `life` / `ui` / `control` 三格 = **档位形状**，不是芯的常量。
    ///
    /// 钉的是第十一轮复核那条星号：无屏档的 `describe_endpoint.manifest` 与
    /// `list_endpoints.device.control` 都从 `Composition::of` 派生，而这三格一度写死成桌面档。
    /// 依据：S0 §2.3.1（桌面）/ §2.3.3（Android）/ §2.4（无屏端点）。
    #[test]
    fn the_shell_cells_follow_the_tier_not_a_core_constant() {
        let expected: [(HostKind, Life, Ui, &[Control]); 4] = [
            (
                HostKind::Windows,
                Life::Interactive,
                Ui::Gui,
                &[Control::InprocApi, Control::Ipc],
            ),
            (
                HostKind::LinuxDesktop,
                Life::Interactive,
                Ui::Gui,
                &[Control::InprocApi, Control::Ipc],
            ),
            (
                HostKind::Android,
                Life::ForegroundService,
                Ui::Gui,
                &[Control::InprocApi, Control::Http],
            ),
            (
                HostKind::LinuxHeadless,
                Life::Daemon,
                Ui::None,
                &[Control::Mcp, Control::Cli, Control::ConfigFile],
            ),
        ];
        // 一档不少：加了第五档却忘了给它一行，这条先炸。
        let covered: Vec<HostKind> = expected.iter().map(|(host, ..)| *host).collect();
        assert_eq!(covered, HostKind::ALL, "四档各要一行");

        for (host, life, ui, control) in expected {
            let facts = HostFacts::all_wired(host);
            // 两条腿都要报同一档的形状：`list_endpoints.device.control` 取 Speak 那一份，
            // Listen 那一份则由 `describe_endpoint` 原样送出去。
            for manifest in [
                Composition::of(&speak_config(), &facts).expect("对外说话的清单"),
                Composition::of(&listen_config(), &facts).expect("听人说话的清单"),
            ] {
                assert_eq!(manifest.host, host);
                assert_eq!(
                    (manifest.life, manifest.ui),
                    (life, ui),
                    "{host:?} 的 life/ui"
                );
                assert_eq!(manifest.control, control, "{host:?} 的 control");
            }
        }
    }

    #[test]
    fn op_order_must_be_a_subsequence_of_the_canonical_order() {
        let (composition, _) = derived(&speak_config(), &desktop_facts());

        let mut dropped = composition.clone();
        dropped.ops.retain(|op| !matches!(op, Op::Mono));
        assert!(dropped.validate().is_ok(), "缺项合法");

        let mut swapped = composition.clone();
        swapped.ops.swap(0, 3);
        let errors = swapped
            .validate()
            .expect_err("resample 跑到 mono 前面就该报出来");
        assert_eq!(errors[0], CompositionError::BadOpOrder { at: 1 });
        assert!(errors
            .iter()
            .all(|error| matches!(error, CompositionError::BadOpOrder { .. })));

        let mut twice = composition.clone();
        twice.ops.push(Op::Denoise);
        assert!(
            matches!(twice.validate(), Err(errors) if errors.contains(&CompositionError::BadOpOrder { at: 4 })),
            "同一节装两次也算换序"
        );
    }

    #[test]
    fn a_manifest_from_another_schema_version_is_not_installable() {
        let (mut composition, _) = derived(&speak_config(), &desktop_facts());
        composition.schema_version = COMPOSITION_SCHEMA_VERSION + 1;
        let err = match Plan::from(&composition) {
            Err(err) => err,
            Ok(_) => panic!("不认识版本的清单不许被装起来"),
        };
        assert!(
            err.message.contains("版本"),
            "报错要说清是版本问题：{}",
            err.message
        );
    }

    #[test]
    fn a_role_may_only_appear_once() {
        let (mut composition, _) = derived(&speak_config(), &desktop_facts());
        let playback = composition
            .out
            .iter()
            .find(|output| matches!(output, Output::Playback { .. }))
            .cloned()
            .expect("有播放出口");
        composition.out.push(playback);
        assert_eq!(
            composition.validate(),
            Err(vec![CompositionError::DuplicateRole(
                PlaybackRole::VirtualMic
            )])
        );
    }

    #[test]
    fn a_session_edge_without_a_session_is_rejected() {
        let mut composition = Composition::endpoint("default", "default");
        composition.out.push(Output::Captions {
            track: Track::Speak,
            source: EdgeSource::Session,
        });
        assert_eq!(
            composition.validate(),
            Err(vec![CompositionError::SessionEdgeWithoutSession])
        );
    }

    #[test]
    fn a_session_without_a_consumer_is_rejected() {
        let (mut composition, _) = derived(&speak_config(), &desktop_facts());
        composition
            .out
            .retain(|output| output.source() != EdgeSource::Session);
        assert_eq!(
            composition.validate(),
            Err(vec![CompositionError::SessionWithoutConsumer])
        );
    }

    #[test]
    fn a_manifest_without_an_input_is_rejected() {
        // `in: []` = 声音从哪来都没说。外部提交的清单必须在第一道闸（`validate`）就被拒，
        // 不许拖到 `Plan::from` 才炸。
        let (mut composition, _) = derived(&speak_config(), &desktop_facts());
        composition.r#in.clear();
        assert_eq!(
            composition.validate(),
            Err(vec![CompositionError::MissingInput])
        );
    }

    #[test]
    fn missing_capabilities_are_reported_per_entry() {
        let (composition, _) = derived(&listen_config(), &desktop_facts());
        assert!(
            composition.missing_on(&report(&desktop_facts())).is_empty(),
            "装在本机（桌面档）上不该报缺东西"
        );

        let errors = composition.missing_on(&report(&android_facts()));
        // ① 档位不对：这份清单不是给手机的。
        assert!(errors.contains(&CompositionError::HostMismatch {
            manifest: HostKind::Windows,
            machine: HostKind::Android,
        }));
        // ② 条目要 `program_tap`，手机没有——错误要指到**具体条目**。
        assert!(errors.contains(&CompositionError::MissingCapability {
            bit: Capability::ProgramTap,
            entry: "in[0]: process_loopback".to_string(),
        }));
        assert_eq!(errors.len(), 2, "别多报：{errors:?}");
    }

    #[test]
    fn a_manifest_for_another_tier_is_rejected() {
        // Hosted 宿主喂、宿主收：不要任何能力位，所以"装不上"只可能是档位不对。
        let manifest = Composition {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            host: HostKind::Android,
            r#in: vec![Input::HostFeed {
                pipe: "in".to_string(),
                block_ms: 20,
            }],
            ops: Vec::new(),
            out: vec![Output::HostSink {
                pipe: "out".to_string(),
                source: EdgeSource::Chain,
            }],
            life: Life::Hosted,
            ui: Ui::Web,
            control: vec![Control::Mcp],
            session: None,
        };
        manifest.validate().expect("清单是合法的");
        assert_eq!(
            manifest.missing_on(&report(&desktop_facts())),
            vec![CompositionError::HostMismatch {
                manifest: HostKind::Android,
                machine: HostKind::Windows,
            }]
        );
    }

    #[test]
    fn the_speak_manifest_drops_the_virtual_mic_entry_on_android() {
        let (composition, plan) = derived(&speak_config(), &android_facts());
        assert!(
            composition
                .r#in
                .iter()
                .any(|input| matches!(input, Input::Mic { .. })),
            "手机能采麦克风（授权后），这一格在"
        );
        let roles: Vec<PlaybackRole> = composition
            .out
            .iter()
            .filter_map(|output| match output {
                Output::Playback { role, .. } => Some(*role),
                _ => None,
            })
            .collect();
        assert!(
            !roles.contains(&PlaybackRole::VirtualMic),
            "这台设备给不了虚拟麦 → 那一格不进清单"
        );
        assert!(
            roles.contains(&PlaybackRole::Speaker),
            "但不是把这条腿删了：换成戴耳机听译音"
        );
        // 腿照跑：门关掉的是那一格，不是整条清单。
        assert_eq!(plan.playback_device, Some(Some("CABLE Input".to_string())));
        assert!(!plan.passthrough);
    }

    #[test]
    fn a_leg_without_its_input_bit_cannot_be_installed() {
        // 手机抓不了程序（§2.3.2）：`in[]` 那一格进不了清单 → 这份清单一个输入口都没有。
        // 界面要说"这台设备做不到"，而不是偷偷少装一格。
        let composition =
            Composition::of(&listen_config(), &android_facts()).expect("清单本身能造出来");
        assert!(composition.r#in.is_empty());
        assert_eq!(
            composition.validate(),
            Err(vec![CompositionError::MissingInput])
        );
        assert!(
            Plan::from(&composition).is_err(),
            "没有输入的清单不能变成作业单"
        );
    }

    #[test]
    fn the_virtual_mic_device_is_filled_in_only_when_the_user_did_not_pick_one() {
        let mut facts = desktop_facts();
        facts.virtual_mic_device = Some("vox_virtual_mic".to_string());

        // 用户没选设备：用这台机器报的虚拟麦节点名（Linux 接线后的样子）。
        let mut config = speak_config();
        config.output_device = None;
        let (composition, plan) = derived(&config, &facts);
        assert_eq!(
            composition.out.iter().find_map(|output| match output {
                Output::Playback {
                    role: PlaybackRole::VirtualMic,
                    device,
                    ..
                } => Some(device.clone()),
                _ => None,
            }),
            Some(Some("vox_virtual_mic".to_string()))
        );
        assert_eq!(
            plan.playback_device,
            Some(Some("vox_virtual_mic".to_string()))
        );

        // 用户自己选了：听他的，不许被缺省解析盖掉。
        config.output_device = Some("我的耳机".to_string());
        let (_, plan) = derived(&config, &facts);
        assert_eq!(plan.playback_device, Some(Some("我的耳机".to_string())));
    }

    #[test]
    fn a_bit_that_is_off_means_the_entry_does_not_enter_the_manifest() {
        // 接线前的 Linux：`virtual_mic` 位报 `false(not_wired)`（§3.2 的中间态）。
        let facts = HostFacts {
            host: HostKind::LinuxDesktop,
            off: BTreeMap::from([(Capability::VirtualMic, UnavailableReason::NotWired)]),
            virtual_mic_device: None,
        };
        let composition = Composition::of(&speak_config(), &facts).expect("清单");
        assert!(
            !composition.out.iter().any(|output| matches!(
                output,
                Output::Playback {
                    role: PlaybackRole::VirtualMic,
                    ..
                }
            )),
            "位为假 → 那一格不进清单（§4.3-A 的 `length => 0`）"
        );
        assert!(
            composition.out.iter().any(|output| matches!(
                output,
                Output::Playback {
                    role: PlaybackRole::Speaker,
                    ..
                }
            )),
            "退成普通出声，不是不出声"
        );
        // 上限里仍有这一位 ⇒ 这是"还没接上"，不是"平台做不到"。
        assert!(crate::capability::host_ceiling(HostKind::LinuxDesktop)
            .contains(Capability::VirtualMic));
    }

    #[test]
    fn the_manifest_lists_only_capabilities_it_actually_needs() {
        let (composition, _) = derived(&speak_config(), &desktop_facts());
        assert_eq!(
            composition.required_capabilities(),
            vec![
                Capability::Mic,
                Capability::VirtualMic,
                Capability::Captions,
                Capability::VoiceSelection,
                Capability::HotUpdateLanguage,
            ],
        );
        // 位挂在哪一格上也查得到（报错要指到具体条目，不是含糊地说"缺能力"）。
        assert_eq!(
            composition.missing_on(&report(&HostFacts {
                host: HostKind::Windows,
                off: BTreeMap::from([(Capability::VirtualMic, UnavailableReason::NotInstalled)]),
                virtual_mic_device: None,
            })),
            vec![CompositionError::MissingCapability {
                bit: Capability::VirtualMic,
                entry: "out[0]: playback".to_string(),
            }],
        );

        // 关掉翻译：不要字幕、不要热更新、不要音色（云端会话整块没了）；
        // 但原声还是往虚拟麦送（这就是"直通"的用法）——所以 `virtual_mic` 还留着。
        let mut config = speak_config();
        config.translate = false;
        let (composition, _) = derived(&config, &desktop_facts());
        assert_eq!(
            composition.required_capabilities(),
            vec![Capability::Mic, Capability::VirtualMic]
        );

        // 复刻频次也要一位。
        let mut config = speak_config();
        config.voice_clone_frequency = Some(1);
        let (composition, _) = derived(&config, &desktop_facts());
        assert!(composition
            .required_capabilities()
            .contains(&Capability::VoiceClone));
        assert_eq!(
            composition
                .session
                .as_ref()
                .map(|s| s.params.clone_frequency),
            Some(Some(CloneFrequency::Once))
        );
    }
}
