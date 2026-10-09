/**
 * 窗口按钮的平台判断：macOS 左上红黄绿，Windows / Linux 右上线性按钮。
 *
 * 用 userAgent 判；浏览器里跑 mock 时可用 `?platform=mac|windows|linux` 覆盖
 * （真实 Tauri 窗口里不认这个参数）。
 */

export type Platform = "mac" | "windows" | "linux";

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function detectPlatform(): Platform {
  if (!inTauri()) {
    const forced = new URLSearchParams(window.location.search).get("platform");
    if (forced === "mac" || forced === "windows" || forced === "linux") return forced;
  }
  const ua = navigator.userAgent;
  if (/Macintosh|Mac OS X/i.test(ua)) return "mac";
  if (/Windows/i.test(ua)) return "windows";
  return "linux";
}
