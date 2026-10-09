/**
 * 应用版本号与「有新版」状态：侧栏底部（版本 + 徽章）和关于页共用一份。
 *
 * 版本号以 Tauri 为准（`getVersion`）；浏览器里跑 mock 时没有 Tauri，退回 package.json 的版本。
 * 更新检查走 updater 插件，非 Tauri 环境会抛错——调用方自己决定是否静默。
 */

import { useSyncExternalStore } from "react";

import { version as bundledVersion } from "../../package.json";

interface AppInfo {
  version: string;
  /** 有新版时是新版本号，否则 null。 */
  update: string | null;
}

let info: AppInfo = { version: bundledVersion, update: null };
const listeners = new Set<() => void>();

function set(next: AppInfo): void {
  info = next;
  for (const fn of listeners) fn();
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function useAppInfo(): AppInfo {
  return useSyncExternalStore(subscribe, () => info);
}

/** 向 Tauri 要真实版本号；失败（浏览器里）保持 package.json 的版本。 */
export async function loadAppVersion(): Promise<void> {
  try {
    const { getVersion } = await import("@tauri-apps/api/app");
    set({ ...info, version: await getVersion() });
  } catch {
    /* 非 Tauri 环境：用打包进来的版本号 */
  }
}

/**
 * 问 updater 有没有新版，并把结果同步给所有订阅者。
 * 返回新版本号；已是最新返回 null；检查失败抛出（由调用方决定提不提示）。
 */
export async function checkAppUpdate(): Promise<string | null> {
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  const latest = update ? update.version : null;
  if (update) void update.close().catch(() => undefined);
  set({ ...info, update: latest });
  return latest;
}

/** 装完新版后清掉徽章（要重启才生效，但「有更新」这件事已经办完）。 */
export function clearAppUpdate(): void {
  set({ ...info, update: null });
}
