/** 运行时快照 / 用量 / 事件的类型镜像。见 types.ts 顶部说明。 */

import type {
  GateKind,
  GateState,
  PipelineName,
  PipelineState,
  Settings,
  Severity,
  Track,
} from "./types";

export interface GateStatus {
  kind: GateKind;
  state: GateState;
  /** 0..1 左右的均方根电平，level 门控才有意义。 */
  rms: number;
  active: boolean;
  ended: boolean;
}

export interface PipelineSnapshot {
  state: PipelineState;
  /** 后端给的中文态标签，直接显示，不要自己再翻一遍。 */
  state_label: string;
  running: boolean;
  gate: GateStatus | null;
  /** 逐轮延迟与队列健康度（后端 `PipelineSnapshotDto` 同形映射）。 */
  latency: LatencySnapshot;
}

export interface DeviceInfo {
  name: string;
  is_default: boolean;
}

export interface AudioApp {
  executable: string;
  display_name: string;
  pid: number;
  active: boolean;
}

export interface DeviceSnapshot {
  inputs: DeviceInfo[];
  outputs: DeviceInfo[];
  apps: AudioApp[];
  /**
   * **安装器状态**：VB-CABLE 的两端都装出来了。
   *
   * **不是**"能不能用"的判据——那个只看能力位 `capabilities.host.virtual_mic`（界面
   * `src/capabilities.ts` 的 `hostBit(snapshot, "virtual_mic")`）。界面上一个地方都不读它。
   */
  virtual_cable_installed: boolean;
  /**
   * 安装器 / 平台形态：`not_applicable` 是 Linux：PipeWire 原生就能建虚拟 sink，
   * 没有"装驱动"这一步（界面据此决定摆不摆安装/卸载那一套）。
   * 它同样**不是**"能不能用"——那看 `capabilities.host.virtual_mic`。
   */
  virtual_cable_status:
    | "installed"
    | "install_pending_reboot"
    | "uninstall_incomplete"
    | "not_installed"
    | "not_applicable";
  virtual_cable_16ch_status: "visible" | "hidden" | "absent";
}

/** 后端通知。文案不在这里：`key` + `params` + `detail`（原文）由界面按语言渲染（`i18n/notice.ts`）。 */
export interface Notice {
  severity: Severity;
  key: string;
  params: Record<string, string>;
  /** 服务端 / 系统给的原文，不翻译。 */
  detail?: string;
  pipeline: PipelineName | null;
}

/** 宿主档位（内核 `HostKind`）。四档**并列**，界面只拿它显示，**不许**按它分支降级。 */
export type HostTier = "windows" | "linux_desktop" | "android" | "linux_headless";

/** 宿主 / 设备位（内核 `Capability::HOST`，13 个）。名字与内核一字不差（`Capability::id()`）。 */
export type HostCapability =
  | "mic"
  | "program_tap"
  | "virtual_mic"
  | "captions"
  | "global_hotkey"
  | "tray"
  | "background_service"
  | "vr_captions"
  | "net_in"
  | "net_out"
  | "file_config"
  | "hw_aec"
  | "soft_aec";

/** provider 位（内核 `Capability::PROVIDER`，8 个）。界面侧另有 `catalog.ts` 读同一份 JSON。 */
export type ProviderCapability =
  | "voice_selection"
  | "voice_clone"
  | "source_language"
  | "hot_update_language"
  | "usage_reporting"
  | "speech_activity"
  | "turn_end"
  | "source_transcript";

export type CapabilityId = HostCapability | ProviderCapability;

/**
 * 为什么没有这一位（内核 `UnavailableReason`）。**不是句子**——芯不许带文案，
 * `(位, reason) → 句子` 的映射在 `src/capabilities.ts` + `i18n/{zh,en}.ts`。
 */
export type UnavailableReason =
  | "unsupported"
  | "not_installed"
  | "permission"
  | "not_built"
  | "not_wired"
  | "pending_reboot"
  | "busy";

/** 一位的状态：关着必带 reason（芯侧不变量 `CapabilityStatus::is_consistent`）。 */
export interface CapabilityStatus {
  enabled: boolean;
  reason: UnavailableReason | null;
}

/**
 * 内核 `CapabilityReport`：**界面降级的唯一依据**（S0 §2.5.0 第 5–6 步）。
 *
 * `tier` 是"哪一档宿主"，`host` 是宿主机位表——两个键名不许互换。每位都有条目，
 * 缺条目时按"关着"算（保守，跟芯的 `host_enabled` 同口径）。
 */
export interface Capabilities {
  tier: HostTier;
  host: Record<HostCapability, CapabilityStatus>;
  speak: Record<ProviderCapability, CapabilityStatus>;
  listen: Record<ProviderCapability, CapabilityStatus>;
}

export interface UsageTotals {
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  turns: number;
}

/** ModelUsage：Rust 侧 total 是 #[serde(flatten)]，所以总计字段就摊在顶层。 */
export interface ModelUsage extends UsageTotals {
  daily: UsageTotals;
  /** "YYYY-MM-DD" */
  daily_date: string;
  monthly: UsageTotals;
  /** "YYYY-MM" */
  monthly_month: string;
  /** unix 秒 */
  updated_at: number;
}

/** UsageLedger 是 #[serde(transparent)]，线上就是一个以模型名为键的裸对象。 */
export type UsageLedger = Record<string, ModelUsage>;

/** 单项延迟指标：最近一次 / 中位数 / p95；没采到样时全是 null。 */
export interface LatencyMetric {
  last_ms: number | null;
  p50_ms: number | null;
  p95_ms: number | null;
  samples: number;
}

/** 一条流水线的延迟与队列健康度快照（对应 Rust `LatencySnapshot`）。 */
export interface LatencySnapshot {
  /** TCP/TLS/WebSocket + session.update 发出的冷启动时间。 */
  connect_ms: number | null;
  /** 从开始连接到收到 session.updated 的时间。 */
  session_ready_ms: number | null;
  input_queue: LatencyMetric;
  upload_send: LatencyMetric;
  server_vad: LatencyMetric;
  first_text: LatencyMetric;
  first_audio: LatencyMetric;
  first_playback: LatencyMetric;
  turn_complete: LatencyMetric;
  completed_turns: number;
  input_queue_depth: number;
  input_queue_oldest_ms: number;
  playback_queue_ms: number;
  processed_chunks: number;
  dropped_chunks: number;
}

/**
 * 控制面（Agent 面）**现在的样子**：Rust `mcp::Status` 的镜像。
 *
 * 与 `Capabilities` 同一个身份：**事实**，不是设置。界面按它说"在跑 / 没在跑、为什么"，
 * **不许**拿 `settings.control.enabled` 自己推"应该"在跑（要什么是用户的事，起没起是事实）。
 */
export interface ControlStatus {
  /** 后端最后一次按下去的开关（`settings.control.enabled` 的观察值）。 */
  enabled: boolean;
  /** 同一次按下去的端口；`0` = 由系统分配。 */
  port: number;
  /** 事实：真的在监听吗。 */
  running: boolean;
  /** 实际绑上的端口（`port` 是 0 时只有这里才知道）；没在跑时 null。 */
  bound_port: number | null;
  /** 起不来的原因（给人看的那句）；没失败过 = null。 */
  error: string | null;
  /** 握手文件路径（Agent 的端口与 token 在里面）。 */
  state_file: string;
}

export interface Snapshot {
  settings: Settings;
  api_keys: Record<string, boolean>;
  /** 每家密钥的末 4 位；没配密钥（或密钥短到不该露）为 null。只用来认钥匙，不是密钥本身。 */
  api_key_tails: Record<string, string | null>;
  speak: PipelineSnapshot;
  listen: PipelineSnapshot;
  mic_active: boolean;
  headphones_advised: boolean;
  devices: DeviceSnapshot;
  usage: UsageLedger;
  notices: Notice[];
  /** 能力位报告（芯算的唯一一份）。界面按它开门/关门，不按档位分支。 */
  capabilities: Capabilities;
  /** 控制面现在的样子（观察值）。设置页那一屏按它显示状态。 */
  control: ControlStatus;
}

export type VoxEvent =
  | { kind: "settings_changed"; settings: Settings }
  | { kind: "pipeline_state"; pipeline: PipelineName; state: PipelineState }
  | { kind: "gate_status"; pipeline: PipelineName; status: GateStatus }
  | {
      kind: "subtitle_delta";
      track: Track;
      text: string;
      done: boolean;
      /** 服务端整句重写了。`true` 时 `text` 是完整当前句，要整行替换而不是追加。 */
      replace: boolean;
      /** 不再会变的那段完整句（已确认前缀；done 时是整句终稿），供整行替换推进用。 */
      confirmed: string | null;
    }
  | { kind: "subtitle_cleared"; track: Track }
  | { kind: "source_detected"; track: Track; language: string }
  | { kind: "usage_changed"; usage: UsageLedger }
  | { kind: "mic_active"; active: boolean }
  | { kind: "devices_changed" }
  | { kind: "latency_changed"; pipeline: PipelineName; latency: LatencySnapshot }
  | { kind: "notice"; notice: Notice };

/** update_settings 收的补丁：深度可选，Option 字段允许显式 null 用来清空。 */
export type SettingsPatch = DeepPartialNullable<Settings>;

type DeepPartialNullable<T> = {
  [K in keyof T]?: T[K] extends readonly unknown[]
    ? T[K]
    : T[K] extends Record<string, string>
      ? T[K]
      : T[K] extends object | null
        ? DeepPartialNullable<NonNullable<T[K]>> | (null extends T[K] ? null : never)
        : T[K];
};
