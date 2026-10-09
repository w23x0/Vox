/**
 * 后端契约的 TypeScript 镜像。
 *
 * 字段名一律照抄 Rust 侧的 serde 序列化结果（snake_case），不许改名、不许加驼峰别名。
 * 对应来源：crates/vox-core/src/{settings,runtime,event,usage,gate,ports,hotkey}.rs
 */

export type PipelineName = "speak" | "listen";

/** Rust: ModelProvider，后续接入服务商时在这里追加。 */
export type ModelProvider = "aliyun" | "gemini" | "gpt";

/** Rust: PipelineState，serde rename_all = "snake_case"。 */
export type PipelineState =
  | "idle"
  | "starting"
  | "ready"
  | "active"
  | "reconnecting"
  | "failed";

/** Rust: Track，serde 小写。 */
export type Track = "speak" | "listen";

/** Rust: ActivationMode，serde 小写。 */
export type ActivationMode = "toggle" | "hold";

/** Rust: Severity，serde 小写。 */
export type Severity = "info" | "warning" | "error";

/** Rust: GateKind，serde 小写。 */
export type GateKind = "manual" | "level";

/** Rust: GateState，serde rename_all = "snake_case"。 */
export type GateState =
  | "empty"
  | "manual"
  | "released"
  | "waiting"
  | "always"
  | "speech"
  | "tail"
  | "tail_end"
  | "silence";

export interface Hotkey {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

export interface ListenTarget {
  executable: string;
  display_name: string;
  include_process_tree: boolean;
}

export interface Geometry {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SpeakSettings {
  enabled: boolean;
  provider: ModelProvider;
  model_name: string;
  target_language: string;
  voice: string;
  voice_by_language: Record<string, string>;
  voice_clone_frequency: number | null;
  input_device: string | null;
  output_device: string | null;
  show_translation: boolean;
  speak_translation: boolean;
  /** 是否走实时翻译；关掉 = 不调用云端 API，把原声直通到输出设备。 */
  translate: boolean;
  monitor_translation: boolean;
  activation_mode: ActivationMode;
  hotkey: Hotkey;
  gate_threshold: number;
  denoise: boolean;
}

export interface ListenSettings {
  enabled: boolean;
  provider: ModelProvider;
  model_name: string;
  target: ListenTarget | null;
  /** 译成哪种语言；老配置缺省中文。 */
  target_language: string;
  output_device: string | null;
  show_translation: boolean;
  speak_translation: boolean;
  voice: string;
  hotkey: Hotkey | null;
  /** 源语言代码；`null` = 服务端自动识别（默认）。 */
  source_language: string | null;
}

export interface SubtitleSettings {
  visible: boolean;
  vr_overlay_enabled: boolean;
  font_family: string;
  font_size: number;
  speak_color: string;
  listen_color: string;
  background_alpha: number;
  char_ttl_ms: number;
  char_fade_ms: number;
  /** 每条轨的区域放几行（1–4）。 */
  lines_per_track: number;
  /** 0 类字（纯噪声/填充词/无意义发音）始终以 `dim_alpha` 淡显，随整块一起淡出清空。 */
  dim_zeros: boolean;
  /** 0 类字的显示 alpha（0..1），越小越淡。 */
  dim_alpha: number;
  geometry: Geometry | null;
}

/**
 * Rust: `ControlSettings`（Agent 控制面 / MCP）。
 *
 * 名字逐字对齐 `crates/vox-core/src/settings.rs`，`vox-mcp` 的 `Grants` 按名字读它。
 * 默认**全关**：老配置文件里没有这一段，读出来也是全关（fail-closed），不是"缺字段 = 允许"。
 */
export interface ControlSettings {
  /** 控制面总开关。关着时服务不监听、四个授权位也一位都不开。 */
  enabled: boolean;
  /** 监听端口；`0` = 由系统分配（默认）。小于 1024 的会被芯归 0（保留段）。 */
  port: number;
  /** 允许 Agent 用麦克风。 */
  allow_microphone: boolean;
  /** 允许 Agent 抓某个程序的声音。 */
  allow_system_audio: boolean;
  /** 允许 Agent 往可听输出放音。 */
  allow_audible_output: boolean;
  /** 允许控制面改用户配置。 */
  allow_config_write: boolean;
  /** 字幕订阅的通知去抖间隔（毫秒）。 */
  transcript_notify_ms: number;
}

export interface Settings {
  version: number;
  speak: SpeakSettings;
  listen: ListenSettings;
  subtitle: SubtitleSettings;
  autostart: boolean;
  start_minimized: boolean;
  /** 界面显示语言（UI locale）：`zh-CN` / `en`。与翻译目标/源语言无关。 */
  ui_language: string;
  /** Agent 控制面的开关与授权位。 */
  control: ControlSettings;
  /** 阿里云百炼地区：`cn` 国内站（北京）/ `intl` 国际站（新加坡）。Key 只在注册地区有效。 */
  aliyun_region: AliyunRegion;
  /**
   * 阿里云百炼自定义接入点（工作空间专属 `wss://…` 地址）；空串 / 缺席 = 按地区缺省。
   * 待 Rust 侧「接入点配置」落地后进快照；在那之前真后端的设置里没有这个键，所以是可选的。
   */
  aliyun_endpoint?: string;
}

export type AliyunRegion = "cn" | "intl";

/* ---- list_models（待 Rust 侧 list_models 落地后进 types.snapshot.ts）---- */

/** 一个服务商的模型清单。契约见 docs/plans/UI-REDESIGN.md §6。 */
export interface ModelList {
  provider: ModelProvider;
  /** official = 本次 / 缓存里来自服务商官方列表接口；builtin = 内置列表。 */
  source: "official" | "builtin";
  /** 毫秒时间戳；builtin 时为 null。 */
  fetched_at: number | null;
  /** 拉取失败的原因；成功为 null。失败时 `models` 是内置列表。 */
  error: ModelListError | null;
  models: ModelEntry[];
}

export interface ModelListError {
  kind: "auth" | "network" | "other";
  /** 连不上的主机名，网络错误时用来提示用户。 */
  host: string;
  message: string;
}

export interface ModelEntry {
  /** 调用用的模型 ID。 */
  id: string;
  /** 展示名（官方给的，没有就用 id）。 */
  label: string;
  /** 有对应协议方言才为 true；false 不可选。 */
  adapted: boolean;
  recommended: boolean;
  /** 是某稳定版的快照时，指向该稳定版的 ID。 */
  snapshot_of: string | null;
  /** 官方预计下线日 YYYY-MM-DD。 */
  offline_at: string | null;
  capabilities: {
    voice_selection: boolean;
    voice_clone: boolean;
    source_language: boolean;
    hot_update_language: boolean;
  };
  /** 语言代码；服务商没给语言明细时为 null。 */
  languages: { audio: string[]; text_only: string[] } | null;
}
