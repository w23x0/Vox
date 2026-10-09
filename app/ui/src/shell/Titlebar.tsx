/**
 * 标题栏（38px）。照 GlassUI 规范：**只放固定的应用名**（Vox），
 * 不放当前页面名 —— 页面名是内容区顶部的大标题（.page-head h1），
 * 两处都放页面名才是重复。界面语言与主题在「设置」页。
 *
 * 窗口按钮按平台：macOS 左上红─黄─绿（三个色值是平台约定，不走 token）；
 * Windows / Linux 右上 最小化 / 最大化 / 关闭（线性图标）。样式见 shell.css。
 */

/** 应用名（和 tauri.conf.json 的 productName 一致）。固定不变，不随页面走。 */
const APP_NAME = "Vox";

import { useMemo } from "react";

import { useT } from "../i18n/context";
import { detectPlatform } from "../lib/platform";
import { IconWinClose, IconWinMaximize, IconWinMinimize } from "../ui/icons";

/** 不在 Tauri 里（浏览器跑 mock）时静默忽略，别让按钮抛异常。 */
const inTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** 动态 import，和 api.ts 保持一致 —— 静态 import 会把 @tauri-apps/api 拉进主 chunk，
 * 顺带让 api.ts 那边的动态 import 失去分包意义（构建会 warn INEFFECTIVE_DYNAMIC_IMPORT）。 */
async function win() {
  if (!inTauri()) return null;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

/**
 * 红绿灯关闭按钮 = 完全退出，不是收进托盘。
 * 走后端 quit_app 命令（内层 app.exit(0)），绕开 window.close() 会被
 * CloseRequested 拦截成 hide() 进托盘的那条路径 —— 那会「后台静默」。
 */
async function quitApp() {
  if (!inTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("quit_app");
}

export function Titlebar() {
  const t = useT();
  const platform = useMemo(detectPlatform, []);
  const close = () => void quitApp();
  const minimize = () => void win().then((w) => w?.minimize());
  const maximize = () => void win().then((w) => w?.toggleMaximize());
  return (
    <>
      {/* 空白处可拖窗。data-tauri-drag-region 要权限 core:window:allow-start-dragging */}
      <div className="app-titlebar" data-tauri-drag-region>
        <span className="app-titlebar-text">{APP_NAME}</span>
      </div>

      {platform === "mac" ? (
        <div className="window-traffic">
          <button
            type="button"
            aria-label={t("titlebar.close")}
            style={{ background: "#ff5f57" }}
            onClick={close}
          />
          <button
            type="button"
            aria-label={t("titlebar.minimize")}
            style={{ background: "#febc2e" }}
            onClick={minimize}
          />
          <button
            type="button"
            aria-label={t("titlebar.maximize")}
            style={{ background: "#28c840" }}
            onClick={maximize}
          />
        </div>
      ) : (
        <div className="window-traffic window-controls">
          <button type="button" aria-label={t("titlebar.minimize")} onClick={minimize}>
            <IconWinMinimize size={16} />
          </button>
          <button type="button" aria-label={t("titlebar.maximize")} onClick={maximize}>
            <IconWinMaximize size={14} />
          </button>
          <button
            type="button"
            className="window-close"
            aria-label={t("titlebar.close")}
            onClick={close}
          >
            <IconWinClose size={16} />
          </button>
        </div>
      )}
    </>
  );
}
