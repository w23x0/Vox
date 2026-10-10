/**
 * 字幕窗位置的纯计算：默认摆位、九宫格预设、示意图里的拖动 / 缩放 / 键盘微调。
 * 不碰 DOM，方便单独推敲；单位都是桌面像素（跟 `subtitle.geometry` 同一套坐标）。
 */

import type { Geometry } from "../types";

/** 显示器在桌面坐标里的矩形。 */
export interface ScreenRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 窗口尺寸下限，和后端 `normalize` 一致（160×60）。 */
export const MIN_WIDTH = 160;
export const MIN_HEIGHT = 60;
/** 没存过几何时的默认大小，和 Linux / Windows 悬浮窗的默认一致。 */
export const DEFAULT_WIDTH = 880;
export const DEFAULT_HEIGHT = 170;
/** 默认摆位离屏幕底边的距离（两端悬浮窗同一个值）。 */
export const BOTTOM_MARGIN = 80;
/** 九宫格预设离屏幕上、左、右边的距离。 */
export const EDGE_MARGIN = 40;

/** 取不到显示器时示意图用的屏幕。 */
export const FALLBACK_SCREEN: ScreenRect = { x: 0, y: 0, width: 1920, height: 1080 };

export type PositionEdge = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";
export type DragMode = "move" | PositionEdge;

const clampInt = (v: number, lo: number, hi: number) =>
  Math.round(Math.min(Math.max(v, lo), Math.max(lo, hi)));

/** 把矩形收进屏幕：先夹大小，再夹位置。 */
export function clampToScreen(screen: ScreenRect, g: Geometry): Geometry {
  const width = clampInt(g.width, MIN_WIDTH, screen.width);
  const height = clampInt(g.height, MIN_HEIGHT, screen.height);
  return {
    width,
    height,
    x: clampInt(g.x, screen.x, screen.x + screen.width - width),
    y: clampInt(g.y, screen.y, screen.y + screen.height - height),
  };
}

/** 没摆过时的默认位置：屏幕底部居中、离底边 80px（跟后端 `default_placement` 同一套）。 */
export function defaultGeometry(screen: ScreenRect): Geometry {
  const width = Math.min(DEFAULT_WIDTH, screen.width);
  return {
    x: screen.x + Math.round((screen.width - width) / 2),
    y: screen.y + screen.height - DEFAULT_HEIGHT - BOTTOM_MARGIN,
    width,
    height: DEFAULT_HEIGHT,
  };
}

/** 九宫格预设：行 0/1/2 = 上/中/下，列 0/1/2 = 左/中/右；大小沿用当前的。下排用默认的底边距。 */
export function presetGeometry(
  screen: ScreenRect,
  current: Geometry,
  row: 0 | 1 | 2,
  col: 0 | 1 | 2,
): Geometry {
  const g = clampToScreen(screen, current);
  const x = [
    screen.x + EDGE_MARGIN,
    screen.x + Math.round((screen.width - g.width) / 2),
    screen.x + screen.width - g.width - EDGE_MARGIN,
  ][col];
  const y = [
    screen.y + EDGE_MARGIN,
    screen.y + Math.round((screen.height - g.height) / 2),
    screen.y + screen.height - g.height - BOTTOM_MARGIN,
  ][row];
  return clampToScreen(screen, { ...g, x, y });
}

/**
 * 按拖动位移算新矩形。`dx`/`dy` 已换算成桌面像素；
 * 移动保持大小并夹在屏幕内；缩放只动被拖的边，不会小于下限，也不会越出屏幕。
 */
export function applyDrag(
  screen: ScreenRect,
  start: Geometry,
  mode: DragMode,
  dx: number,
  dy: number,
): Geometry {
  if (mode === "move") {
    return clampToScreen(screen, { ...start, x: start.x + dx, y: start.y + dy });
  }
  let left = start.x;
  let top = start.y;
  let right = start.x + start.width;
  let bottom = start.y + start.height;
  if (mode.includes("w")) left = clampInt(left + dx, screen.x, right - MIN_WIDTH);
  if (mode.includes("e")) right = clampInt(right + dx, left + MIN_WIDTH, screen.x + screen.width);
  if (mode.includes("n")) top = clampInt(top + dy, screen.y, bottom - MIN_HEIGHT);
  if (mode.includes("s")) bottom = clampInt(bottom + dy, top + MIN_HEIGHT, screen.y + screen.height);
  return { x: left, y: top, width: right - left, height: bottom - top };
}

/** 方向键微调：普通 10px，按住 Shift 1px；按住 Alt 改大小（←→ 宽，↑↓ 高）。 */
export function nudge(
  screen: ScreenRect,
  g: Geometry,
  key: string,
  opts: { shift: boolean; alt: boolean },
): Geometry | null {
  const step = opts.shift ? 1 : 10;
  const dir: Record<string, [number, number]> = {
    ArrowLeft: [-1, 0],
    ArrowRight: [1, 0],
    ArrowUp: [0, -1],
    ArrowDown: [0, 1],
  };
  const d = dir[key];
  if (!d) return null;
  if (opts.alt) {
    return applyDrag(screen, g, "se", d[0] * step, d[1] * step);
  }
  return applyDrag(screen, g, "move", d[0] * step, d[1] * step);
}
