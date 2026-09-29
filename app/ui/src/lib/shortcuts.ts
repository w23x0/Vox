/* ==========================================================================
   应用级快捷键（Windows 惯例）。移植 GlassUI 模板的 shortcuts.ts，裁剪掉
   命令面板（Ctrl+K / openCommandPalette）—— Vox 没有命令面板。

   ── Windows 系统级，应用收不到 keydown，也不需要做 ──
     Alt+F4 / Win+↑↓←→ / Alt+Space：窗口管理器处理，不经过 WebView。

   ── 我们实现的应用层 ──
     Ctrl+Tab / Ctrl+Shift+Tab   切页（循环）
     Ctrl+1~6                    跳到第 N 页
     Ctrl+,                       打开设置页
     F11                          全屏（走 Tauri API）
     Esc                          关弹窗 / 退文本编辑 / 回侧栏
     ← → ↑ ↓                      二维焦点网格（见 focus.ts）
     PgUp/PgDn/Home/End           滚动内容区
   ========================================================================== */

import { getCurrentWindow } from '@tauri-apps/api/window';

/** 修饰键是否按下（Mac 认 metaKey，其他认 ctrlKey）。 */
export function hasCmd(e: KeyboardEvent): boolean {
  const isMac = /mac/i.test(navigator.platform ?? '');
  return isMac ? e.metaKey : e.ctrlKey;
}

/** 切换窗口全屏。Tauri 不在时（浏览器里跑 dev）静默失败。 */
export async function toggleFullscreen(): Promise<void> {
  try {
    const w = getCurrentWindow();
    const on = await w.isFullscreen();
    await w.setFullscreen(!on);
  } catch {
    /* 非 Tauri 环境：忽略，不 fallback 到浏览器全屏 --
       那会造成「窗口没变但内容全屏」的错位状态 */
  }
}

export interface ShortcutHandlers {
  /** 切页，delta +1/-1（Ctrl+Tab）。必须以当前激活页。 */
  cyclePage: (delta: number) => void;
  /** 跳到第 n 页（1 起，Ctrl+1~6）。 */
  gotoPage: (n: number) => void;
  /** 打开设置页（Ctrl+,）。 */
  openSettings: () => void;
  /** Esc：先让调用方决定（关弹窗？退区域？），返回 true 表示已处理。 */
  onEscape: () => boolean;
}

/** 识别并处理一个 keydown。返回 true = 已消费（调用方应 preventDefault）。
 *  顺序：先判修饰键组合，再判裸键，避免 Ctrl+K 被 K 误吞。 */
export function handleShortcut(e: KeyboardEvent, h: ShortcutHandlers): boolean {
  if (hasCmd(e) && !e.altKey) {
    if (e.key === 'Tab') {
      h.cyclePage(e.shiftKey ? -1 : 1);
      return true;
    }
    const m = /^Digit([1-9])$/.exec(e.code);
    if (m && !e.shiftKey) {
      h.gotoPage(Number(m[1]));
      return true;
    }
    if (e.key === ',' && !e.shiftKey) {
      h.openSettings();
      return true;
    }
    return false; // 其它 Ctrl 组合放过（Ctrl+C/V 等留给系统）
  }

  if (e.altKey || e.ctrlKey || e.metaKey) return false;

  if (e.key === 'F11') {
    void toggleFullscreen();
    return true;
  }
  if (e.key === 'Escape') {
    return h.onEscape();
  }
  return false;
}