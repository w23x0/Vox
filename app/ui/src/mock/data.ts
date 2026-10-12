/** 假数据素材：设备、进程、字幕脚本、初始用量。只在 dev / 浏览器预览里用。 */

import type {
  AudioApp,
  Capabilities,
  CapabilityStatus,
  DeviceInfo,
  HostCapability,
  HostTier,
  ProviderCapability,
  UnavailableReason,
  UsageLedger,
} from "../types.snapshot";
import type { ModelProvider } from "../types";
import { HOST_CAPABILITIES } from "../capabilities";
import * as catalog from "../catalog";
import { DEFAULT_MODEL_NAME } from "../catalog";
import { dateKey, monthKey } from "../lib/format";

export const MOCK_INPUTS: DeviceInfo[] = [
  { name: "麦克风 (Realtek(R) Audio)", is_default: true },
  { name: "耳机麦克风 (Arctis Nova 7)", is_default: false },
  { name: "CABLE Output (VB-Audio Virtual Cable)", is_default: false },
  { name: "线路输入 (Focusrite Scarlett Solo)", is_default: false },
];

export const MOCK_OUTPUTS: DeviceInfo[] = [
  { name: "扬声器 (Realtek(R) Audio)", is_default: true },
  { name: "耳机 (Arctis Nova 7)", is_default: false },
  { name: "CABLE Input (VB-Audio Virtual Cable)", is_default: false },
  { name: "LG HDR 4K (NVIDIA High Definition Audio)", is_default: false },
];

export const MOCK_APPS: AudioApp[] = [
  { executable: "VRChat.exe", display_name: "VRChat", pid: 18420, active: true },
  { executable: "Discord.exe", display_name: "Discord", pid: 9312, active: true },
  { executable: "chrome.exe", display_name: "Google Chrome", pid: 4488, active: true },
  { executable: "steam.exe", display_name: "Steam", pid: 7704, active: false },
  { executable: "Spotify.exe", display_name: "Spotify", pid: 15992, active: false },
];

/** 对外说话：中文进、外语出，字幕轨显示的是译文。 */
export const SPEAK_SCRIPT: string[] = [
  "はじめまして、よろしくお願いします。",
  "この部屋のライトはどうやって変えるんですか？",
  "ちょっと待ってください、マイクを直します。",
  "さっきの話、もう一度言ってもらえますか？",
  "今日は付き合ってくれてありがとう、また明日ね。",
];

/** 听人说话：抓别人的声音，翻成中文。 */
export const LISTEN_SCRIPT: string[] = [
  "欢迎来玩，随便找个位置坐吧。",
  "我这边的麦好像有点小，你能听清吗？",
  "那个镜子后面有个隐藏房间，要不要一起去看看。",
  "等一下，我去换个头像，两分钟就回来。",
  "今天人有点多，语音有点卡，抱歉啊。",
];

const today = new Date();
export const MOCK_TODAY = dateKey(today);
export const MOCK_MONTH = monthKey(today);

export function mockUsage(): UsageLedger {
  return {
    [DEFAULT_MODEL_NAME]: {
      input_tokens: 412_866,
      output_tokens: 118_204,
      total_tokens: 531_070,
      turns: 1_284,
      daily: { input_tokens: 24_118, output_tokens: 7_402, total_tokens: 31_520, turns: 86 },
      daily_date: MOCK_TODAY,
      monthly: { input_tokens: 186_530, output_tokens: 54_998, total_tokens: 241_528, turns: 604 },
      monthly_month: MOCK_MONTH,
      updated_at: Math.floor(Date.now() / 1000) - 90,
    },
  };
}
// ─── 能力位（假后端自己算一份报告） ──────────────────────────────────────────
//
// 这里**故意**抄一遍 `crates/vox-core/src/capability.rs` 的两张表：假后端要能造出真后端
// 会造出的那份 `CapabilityReport`，界面才有得降级（`?host=` 四档 + `?off=` 任意 reason）。
//
// 界面**不许**自带上限表——真实路径上界面只读快照里的 `capabilities`，这个文件只被 mock 引用。

/** 假后端的四档宿主（`?host=` 的取值，`embedded` = 内核的无屏档）。 */
export type MockHost = "windows" | "linux" | "android" | "embedded";

/** 档位 → 内核档位名（`CapabilityReport.tier`）。 */
export const MOCK_TIER: Record<MockHost, HostTier> = {
  windows: "windows",
  linux: "linux_desktop",
  android: "android",
  embedded: "linux_headless",
};

/** 档位上限（= 芯的 `host_ceiling` 四行，逐位对齐）。 */
const MOCK_CEILING: Record<MockHost, readonly HostCapability[]> = {
  windows: [
    "mic",
    "program_tap",
    "virtual_mic",
    "captions",
    "global_hotkey",
    "tray",
    "background_service",
    "vr_captions",
    "soft_aec",
  ],
  linux: [
    "mic",
    "program_tap",
    "virtual_mic",
    "captions",
    "global_hotkey",
    "tray",
    "background_service",
    "soft_aec",
  ],
  android: ["mic", "captions", "background_service"],
  // 无屏档（`linux_headless`）也在 `soft_aec` 的三档里；`hw_aec` 四档都不在
  // （没有任何手段探测会议麦的 AEC 芯片 ⇒ 报告恒 `off(unsupported)`）。
  embedded: ["mic", "background_service", "soft_aec"],
};

export interface MockFacts {
  host: MockHost;
  /** 这台机器上报"关掉的位"（`?off=` / `?virtual_mic=`）。上限之外的位会被忽略（上限是硬的）。 */
  off: Partial<Record<HostCapability, UnavailableReason>>;
  speak: ModelProvider;
  listen: ModelProvider;
}

const on = (enabled: boolean): CapabilityStatus =>
  enabled ? { enabled: true, reason: null } : { enabled: false, reason: "unsupported" };

/** 一位的状态：**先看上限、再看 off**（与芯的 `status_of` 同序：上限是硬的）。 */
function bitStatus(
  ceiling: readonly HostCapability[],
  off: Partial<Record<HostCapability, UnavailableReason>>,
  bit: HostCapability,
): CapabilityStatus {
  if (!ceiling.includes(bit)) return { enabled: false, reason: "unsupported" };
  const reason = off[bit];
  return reason ? { enabled: false, reason } : { enabled: true, reason: null };
}

/** provider 位：已实现那 4 位读目录 JSON（与界面同一份），另 4 位占名恒假（S0 §2.5.1）。 */
function providerBits(provider: ModelProvider): Record<ProviderCapability, CapabilityStatus> {
  return {
    voice_selection: on(catalog.supportsVoiceSelection(provider)),
    voice_clone: on(catalog.supportsVoiceClone(provider)),
    source_language: on(catalog.supportsSourceLanguage(provider)),
    hot_update_language: on(catalog.supportsHotUpdateLanguage(provider)),
    usage_reporting: on(false),
    speech_activity: on(false),
    turn_end: on(false),
    source_transcript: on(false),
  };
}

export function mockCapabilities(facts: MockFacts): Capabilities {
  const ceiling = MOCK_CEILING[facts.host];
  const host = Object.fromEntries(
    HOST_CAPABILITIES.map((bit) => [bit, bitStatus(ceiling, facts.off, bit)]),
  ) as Record<HostCapability, CapabilityStatus>;
  return {
    tier: MOCK_TIER[facts.host],
    host,
    speak: providerBits(facts.speak),
    listen: providerBits(facts.listen),
  };
}
