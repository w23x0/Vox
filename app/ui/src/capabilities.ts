/**
 * 能力位在界面侧的**唯一**读数点。
 *
 * 分工（S0 §2.5.2 / §2.6）：
 * - 芯只给 `(位, reason)`，**句子不在芯里**（R3）；位名 → 文案、reason → 文案这两张表
 *   只此一份，就住在这个文件里，句子在 `i18n/{zh,en}.ts`（ja 已冻结，见 `i18n/context.tsx`）。
 * - 界面**不按档位分支**：`tier` 只用来显示"这是哪一档"，门开在哪一律看 `enabled`。
 *   `if (tier === "android")` 这种写法等于把位表抄了第二份。
 * - 位是"**能不能**"，用户开关是"**要不要**"（R5）：位为假时入口整个不渲染（不是灰掉），
 *   并在原位给一句说明（R1/R9）。
 *
 * 只管**宿主 / 设备位**：provider 位（`capabilities.speak` / `.listen`）的消费者是
 * `catalog.ts` 的 `supportsXxx()`——同一份值走编译期目录 JSON，不在这里再抄一遍。
 *
 * 新增一位或一个 reason 时，改这里 + `i18n/zh.ts` + `i18n/en.ts`，别在组件里现写三元。
 */

import type { TParams } from "./i18n/types";
import type {
  CapabilityStatus,
  HostCapability,
  Snapshot,
  UnavailableReason,
} from "./types.snapshot";

/** 翻译函数（`useT()` 的返回值）。只依赖这一个签名，好让纯函数也能查文案。 */
type Translate = (key: string, params?: TParams) => string;

/**
 * 宿主位的排版顺序：采集 → 出口 → 界面 → 常驻 → 别的。
 * 与内核 `Capability::HOST` 同一集合（内核那一份是协议顺序，这一份只管怎么摆）。
 */
export const HOST_CAPABILITIES: readonly HostCapability[] = [
  "mic",
  "program_tap",
  "virtual_mic",
  "captions",
  "global_hotkey",
  "tray",
  "background_service",
  "vr_captions",
  "net_in",
  "net_out",
  "file_config",
];

/** 位 → 文案 key（人是看不懂 `program_tap` 的）。 */
export const CAPABILITY_KEY: Record<HostCapability, string> = {
  mic: "capabilities.bit.mic",
  program_tap: "capabilities.bit.programTap",
  virtual_mic: "capabilities.bit.virtualMic",
  captions: "capabilities.bit.captions",
  global_hotkey: "capabilities.bit.globalHotkey",
  tray: "capabilities.bit.tray",
  background_service: "capabilities.bit.backgroundService",
  vr_captions: "capabilities.bit.vrCaptions",
  net_in: "capabilities.bit.netIn",
  net_out: "capabilities.bit.netOut",
  file_config: "capabilities.bit.fileConfig",
};

/** reason → 文案 key。**七种全覆盖**（缺一种就会在界面上漏出 key）。 */
const REASON_KEY: Record<UnavailableReason, string> = {
  unsupported: "capabilities.reason.unsupported",
  not_installed: "capabilities.reason.notInstalled",
  permission: "capabilities.reason.permission",
  not_built: "capabilities.reason.notBuilt",
  not_wired: "capabilities.reason.notWired",
  pending_reboot: "capabilities.reason.pendingReboot",
  busy: "capabilities.reason.busy",
};

/** reason 的全集（`REASON_KEY` 的键）。假后端的 `?off=` 校验、检查脚本的遍历都读它。 */
export const UNAVAILABLE_REASONS = Object.keys(REASON_KEY) as UnavailableReason[];

/**
 * `(位, reason)` 特例表：通用 reason 文案说不出"要做什么 / 做完去哪"时（R2）在这里覆盖。
 * 没覆盖的组合用 `REASON_KEY` 的通用文案——所以这张表**只许更具体，不许更含糊**。
 */
const SPECIFIC_KEY: Partial<Record<string, string>> = {
  "tray:unsupported": "capabilities.specific.trayUnsupported",
  "global_hotkey:permission": "capabilities.specific.hotkeyPermission",
  "program_tap:unsupported": "capabilities.specific.programTapUnsupported",
  "virtual_mic:not_installed": "capabilities.specific.virtualMicNotInstalled",
};

/**
 * 宿主位。`snapshot` 还没到（`null`）时按"关着 + `unsupported`"算——跟芯的
 * `CapabilityReport::host_enabled` 同口径（缺条目 = 保守）。
 *
 * 调用方要区分"**还不知道**"和"**做不到**"：快照没到时别渲染降级文案，
 * 否则用户先看到一句"这台设备做不到"，半秒后又消失。
 */
export function hostBit(snapshot: Snapshot | null, bit: HostCapability): CapabilityStatus {
  return snapshot?.capabilities.host[bit] ?? { enabled: false, reason: "unsupported" };
}

/**
 * 位为假时那句说明（位为真返回 `null`）：`{位名}：{reason 文案}`。
 *
 * 句子必须说清三件事（R2）：**现在为什么不行 + 要用户做什么 + 做完之后去哪**；
 * 架构内功能一律说"**这台设备做不到**"，不许写成"没有这个功能"（R9）。
 */
export function capabilityNote(
  t: Translate,
  bit: HostCapability,
  status: CapabilityStatus,
): string | null {
  if (status.enabled) return null;
  const reason = status.reason ?? "unsupported";
  return t("capabilities.note", {
    bit: t(CAPABILITY_KEY[bit]),
    reason: t(SPECIFIC_KEY[`${bit}:${reason}`] ?? REASON_KEY[reason]),
  });
}
