/**
 * 主题：跟随系统 / 浅色 / 深色。偏好存 localStorage（"app-theme"：dark | light，
 * 没存 = 跟随系统）；落到 <html data-theme> 上的永远是解析后的 dark / light。
 *
 * 两个要点：
 *  1. 切换前必须锁住所有 transition —— 否则几十个元素同时补间会闪。
 *     用注入 <style> 的方式而不是加 class，因为 class 要等下一帧才生效。
 *  2. 用 View Transition 从左下角画圆扩散。不支持或用户要求减少动效时降级。
 *
 * 首帧那次套用在 index.html 的内联脚本里，不在这儿 —— 等模块加载完就晚了，会闪一下。
 */

export type Theme = "light" | "dark";
export type ThemeMode = "system" | Theme;

const KEY = "app-theme";

const systemDark = (): MediaQueryList => matchMedia("(prefers-color-scheme: dark)");

/** 存下来的偏好；没存过（或存储被禁）就是跟随系统。 */
export function getThemeMode(): ThemeMode {
  try {
    const v = localStorage.getItem(KEY);
    return v === "dark" || v === "light" ? v : "system";
  } catch {
    // WebView 里 localStorage 偶尔被禁（隐私模式 / 沙箱），读不到就当没存过
    return "system";
  }
}

function resolve(mode: ThemeMode): Theme {
  return mode === "system" ? (systemDark().matches ? "dark" : "light") : mode;
}

/** 当前生效的主题。tokens.css 的暗色挂在 [data-theme=dark] 上。 */
function currentTheme(): Theme {
  return document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light";
}

function paint(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "dark") root.setAttribute("data-theme", "dark");
  else root.removeAttribute("data-theme");
}

/** 启动时恢复，并让「跟随系统」随系统明暗实时变化。index.html 已经同步做过首帧那次。 */
export function restoreTheme(): () => void {
  paint(resolve(getThemeMode()));
  const mq = systemDark();
  const onChange = () => {
    if (getThemeMode() === "system") paint(resolve("system"));
  };
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}

/** 切换偏好并（带过渡地）落到界面上。 */
export function setThemeMode(mode: ThemeMode): void {
  try {
    if (mode === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, mode);
  } catch {
    // 存不下就只在本次会话里生效，不影响切换本身
  }
  const next = resolve(mode);
  if (next === currentTheme()) return;
  const root = document.documentElement;

  // 1. 注入样式锁死过渡
  const lock = document.createElement("style");
  lock.textContent = "*, *::before, *::after { transition-duration: 0s !important; }";
  document.head.appendChild(lock);

  const apply = () => paint(next);
  const unlock = () => setTimeout(() => lock.remove(), 100);

  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  // Linux 桌面端的 WebKitGTK 在透明窗口里做 view transition + clip-path 动画会让渲染进程崩溃
  // （用户实测：切浅色主题直接闪退），那里一律直接切换、不做过渡动画。
  const webkitGtk = /Linux/.test(navigator.userAgent) && !/Chrome|Chromium/.test(navigator.userAgent);
  if (!document.startViewTransition || reduce || webkitGtk) {
    apply();
    unlock();
    return;
  }

  const vt = document.startViewTransition(apply);
  void vt.finished.then(unlock);
  void vt.ready.then(() => {
    // 左下角起点，半径取到最远角
    const x = 0;
    const y = innerHeight;
    const r = Math.hypot(Math.max(x, innerWidth - x), Math.max(y, innerHeight - y));
    root.animate(
      { clipPath: [`circle(0% at ${x}px ${y}px)`, `circle(${r}px at ${x}px ${y}px)`] },
      {
        duration: 500,
        easing: "ease-in-out",
        pseudoElement: "::view-transition-new(root)",
      },
    );
  });
}
