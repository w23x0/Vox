/**
 * 后端适配层。
 *
 * 生产走 Tauri `invoke` + 单通道 `vox://event`；
 * 浏览器里（`npm run dev` 直接开、或者渲染截图）自动切到 mock，
 * 造一份逼真的快照并定时推假事件，界面在没有 Rust 侧的时候也能整套点通。
 *
 * Rust 侧一旦接上，这里不用改任何界面代码。
 */

import { createMockApi } from "./mock/backend";
import type { ModelProvider, PipelineName, Settings } from "./types";
import type {
  AudioApp,
  Snapshot,
  SettingsPatch,
  VoxEvent,
} from "./types.snapshot";

export const EVENT_CHANNEL = "vox://event";

export interface CableActionResult {
  needs_reboot: boolean;
  multichannel_hidden: boolean;
}

export interface CatalogUpdateCheck {
  /** 当前生效的 verified_at（内置或覆盖版）。 */
  current: string;
  /** 线上仓库最新的 verified_at。 */
  latest: string;
}

export interface CatalogUpdateApplied {
  /** 覆盖到的文件名，如 "aliyun.json"。 */
  file: string;
  /** 落盘后的 verified_at。 */
  verified: string;
}

export interface VoxApi {
  /** 真后端还是假数据。界面靠这个决定要不要显示「演示数据」角标。 */
  readonly mock: boolean;
  snapshot(): Promise<Snapshot>;
  updateSettings(patch: SettingsPatch): Promise<Settings>;
  setApiKey(provider: ModelProvider, key: string): Promise<void>;
  togglePipeline(pipeline: PipelineName): Promise<void>;
  /** VRChat(OSC) 外部模块：启动同步（聊天框开关 + 头像开关/参数名 + 端口）。 */
  oscStart(
    chatboxEnabled: boolean,
    avatarEnabled: boolean,
    avatarParam: string,
    port: number,
  ): Promise<void>;
  /** 运行中改配置热更新（端口建时定死，不在热更新里）。 */
  oscUpdate(chatboxEnabled: boolean, avatarEnabled: boolean, avatarParam: string): Promise<void>;
  oscStop(): Promise<void>;
  /** 往 VRChat ChatBox 送一条文本。 */
  oscSendChatbox(text: string): Promise<void>;
  /** 手动设一个头像状态参数（旁路测试用；自动跟随交由后端）。 */
  oscSetAvatar(on: boolean): Promise<void>;
  resetUsage(): Promise<void>;
  resetUsageModel(model: string): Promise<void>;
  refreshDevices(): Promise<void>;
  installVirtualCable(): Promise<CableActionResult>;
  virtualCableBlockers(): Promise<AudioApp[]>;
  uninstallVirtualCable(closeBlockers: boolean): Promise<CableActionResult>;
  setVirtualCableMultichannelVisible(visible: boolean): Promise<CableActionResult>;
  openVirtualCableWebsite(): Promise<void>;
  openVirtualCableDonation(): Promise<void>;
  openProviderConsole(provider: ModelProvider): Promise<void>;
  /** 读某个服务商落盘的覆盖版目录；没覆盖时返回 null。 */
  readCatalogOverride(provider: ModelProvider): Promise<string | null>;
  /** 检查某服务商目录有没有线上更新（只查不写）。 */
  checkCatalogUpdate(provider: ModelProvider): Promise<CatalogUpdateCheck>;
  /** 应用某服务商的线上目录，落盘并返回结果。 */
  applyCatalogUpdate(provider: ModelProvider): Promise<CatalogUpdateApplied>;
  /** 订阅事件，返回取消订阅。 */
  subscribe(handler: (event: VoxEvent) => void): () => void;
}

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function forcedMock(): boolean {
  if (typeof window === "undefined") return true;
  const q = new URLSearchParams(window.location.search);
  if (q.get("mock") === "1") return true;
  if (q.get("mock") === "0") return false;
  return import.meta.env.VITE_VOX_MOCK === "1";
}

/** 真后端：命令名和参数名照抄 Rust 侧，改一个字就对不上了。 */
function createTauriApi(): VoxApi {
  const call = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  };
  return {
    mock: false,
    snapshot: () => call<Snapshot>("snapshot"),
    updateSettings: (patch) => call<Settings>("update_settings", { patch }),
    setApiKey: async (provider, key) =>
      void (await call<unknown>("set_api_key", { provider, key })),
    togglePipeline: async (pipeline) => void (await call<unknown>("toggle_pipeline", { pipeline })),
    oscStart: async (chatboxEnabled, avatarEnabled, avatarParam, port) =>
      void (await call<unknown>("osc_start", { port, chatboxEnabled, avatarParam, avatarEnabled })),
    oscUpdate: async (chatboxEnabled, avatarEnabled, avatarParam) =>
      void (await call<unknown>("osc_update", { chatboxEnabled, avatarParam, avatarEnabled })),
    oscStop: async () => void (await call<unknown>("osc_stop")),
    oscSendChatbox: async (text) => void (await call<unknown>("osc_send_chatbox", { text })),
    oscSetAvatar: async (on) => void (await call<unknown>("osc_set_avatar", { on })),
    resetUsage: async () => void (await call<unknown>("reset_usage")),
    resetUsageModel: async (model) => void (await call<unknown>("reset_usage_model", { model })),
    refreshDevices: async () => void (await call<unknown>("refresh_devices")),
    installVirtualCable: () => call<CableActionResult>("install_virtual_cable"),
    virtualCableBlockers: () => call<AudioApp[]>("virtual_cable_blockers"),
    uninstallVirtualCable: (closeBlockers) =>
      call<CableActionResult>("uninstall_virtual_cable", { closeBlockers }),
    setVirtualCableMultichannelVisible: (visible) =>
      call<CableActionResult>("set_virtual_cable_multichannel_visible", { visible }),
    openVirtualCableWebsite: async () => void (await call<unknown>("open_virtual_cable_website")),
    openVirtualCableDonation: async () => void (await call<unknown>("open_virtual_cable_donation")),
    openProviderConsole: async (provider) =>
      void (await call<unknown>("open_provider_console", { provider })),
    readCatalogOverride: (provider) =>
      call<string | null>("read_catalog_override", { provider }),
    checkCatalogUpdate: (provider) =>
      call<CatalogUpdateCheck>("check_catalog_update", { provider }),
    applyCatalogUpdate: (provider) =>
      call<CatalogUpdateApplied>("apply_catalog_update", { provider }),
    subscribe(handler) {
      let stop: (() => void) | null = null;
      let cancelled = false;
      void (async () => {
        const { listen } = await import("@tauri-apps/api/event");
        const un = await listen<VoxEvent>(EVENT_CHANNEL, (e) => handler(e.payload));
        if (cancelled) un();
        else stop = un;
      })();
      return () => {
        cancelled = true;
        stop?.();
      };
    },
  };
}

let cached: VoxApi | null = null;

/** 单例。整个界面只认这一个入口。 */
export function getApi(): VoxApi {
  if (!cached) {
    cached = inTauri() && !forcedMock() ? createTauriApi() : createMockApi();
  }
  return cached;
}
