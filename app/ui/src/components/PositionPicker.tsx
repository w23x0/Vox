/**
 * 字幕窗位置示意图：一块按比例缩小的显示器，里面一个能拖动、能拖边缘缩放的矩形代表字幕窗口。
 * 拖动时按 ~50ms 节流写 `subtitle.geometry`（走普通的设置补丁），真字幕窗随之实时移动。
 *
 * 键盘：矩形是个 Tab / 方向键网格里的控件，按 Enter 进入「调整」状态后方向键才移动它
 * （平时方向键还要拿去在设置项之间导航，不能直接吞掉）：方向键 ±10px，Shift ±1px，
 * Alt+方向键改大小；Enter 或 Esc 退出调整状态。
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  FALLBACK_SCREEN,
  type DragMode,
  type PositionEdge,
  type ScreenRect,
  applyDrag,
  clampToScreen,
  defaultGeometry,
  nudge,
  presetGeometry,
} from "../lib/position";
import { useT } from "../i18n/context";
import type { Geometry } from "../types";

/** 写回设置的最小间隔。 */
const THROTTLE_MS = 50;
const EDGES: PositionEdge[] = ["n", "s", "e", "w", "ne", "nw", "se", "sw"];
const PRESETS: { row: 0 | 1 | 2; col: 0 | 1 | 2; key: string }[] = [
  { row: 0, col: 0, key: "subtitle.posPreset00" },
  { row: 0, col: 1, key: "subtitle.posPreset01" },
  { row: 0, col: 2, key: "subtitle.posPreset02" },
  { row: 1, col: 0, key: "subtitle.posPreset10" },
  { row: 1, col: 1, key: "subtitle.posPreset11" },
  { row: 1, col: 2, key: "subtitle.posPreset12" },
  { row: 2, col: 0, key: "subtitle.posPreset20" },
  { row: 2, col: 1, key: "subtitle.posPreset21" },
  { row: 2, col: 2, key: "subtitle.posPreset22" },
];

interface DragState {
  mode: DragMode;
  startX: number;
  startY: number;
  /** 示意图 1px = 桌面多少 px。 */
  ratio: number;
  origin: Geometry;
}

export function PositionPicker({
  geometry,
  disabled,
  loadScreen,
  onChange,
}: {
  /** 设置里的几何；`null` = 默认位置。 */
  geometry: Geometry | null;
  disabled?: boolean;
  loadScreen: () => Promise<ScreenRect | null>;
  onChange: (g: Geometry) => void;
}) {
  const t = useT();
  const [screen, setScreen] = useState<ScreenRect>(FALLBACK_SCREEN);
  useEffect(() => {
    let alive = true;
    loadScreen()
      .then((r) => {
        if (alive && r && r.width > 0 && r.height > 0) setScreen(r);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [loadScreen]);

  const effective = useMemo(
    () => clampToScreen(screen, geometry ?? defaultGeometry(screen)),
    [screen, geometry],
  );
  // 拖动期间示意图自己说了算（设置回包有延迟，直接读设置会抖）；松手后回到设置。
  const [draft, setDraft] = useState<Geometry | null>(null);
  const shown = draft ?? effective;
  const [adjusting, setAdjusting] = useState(false);

  const drag = useRef<DragState | null>(null);
  const mapRef = useRef<HTMLDivElement>(null);
  const timer = useRef<number | null>(null);
  const pending = useRef<Geometry | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  function flush() {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    if (pending.current) {
      const g = pending.current;
      pending.current = null;
      onChangeRef.current(g);
    }
  }
  /** 节流写回：窗口内最多一次，最后一个值一定会发出去。 */
  function commit(g: Geometry) {
    pending.current = g;
    if (timer.current === null) {
      timer.current = window.setTimeout(() => {
        timer.current = null;
        flush();
      }, THROTTLE_MS);
    }
  }
  useEffect(() => () => flush(), []);

  function begin(mode: DragMode, e: React.PointerEvent<HTMLElement>) {
    if (disabled || e.button !== 0) return;
    const box = mapRef.current?.getBoundingClientRect();
    if (!box || box.width <= 0) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = {
      mode,
      startX: e.clientX,
      startY: e.clientY,
      ratio: screen.width / box.width,
      origin: shown,
    };
  }
  function move(e: React.PointerEvent<HTMLElement>) {
    const d = drag.current;
    if (!d) return;
    const next = applyDrag(
      screen,
      d.origin,
      d.mode,
      (e.clientX - d.startX) * d.ratio,
      (e.clientY - d.startY) * d.ratio,
    );
    setDraft(next);
    commit(next);
  }
  function end() {
    if (!drag.current) return;
    drag.current = null;
    flush();
    setDraft(null);
  }

  function setGeometry(g: Geometry) {
    const next = clampToScreen(screen, g);
    commit(next);
    flush();
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (disabled) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      setAdjusting((v) => !v);
      return;
    }
    if (!adjusting) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setAdjusting(false);
      return;
    }
    const next = nudge(screen, shown, e.key, { shift: e.shiftKey, alt: e.altKey });
    if (!next) return;
    // 方向键在调整状态下归矩形，别让全局的网格导航抢走。
    e.preventDefault();
    e.stopPropagation();
    commit(next);
  }

  const pct = (v: number, total: number) => `${(v / total) * 100}%`;
  const rectStyle = {
    left: pct(shown.x - screen.x, screen.width),
    top: pct(shown.y - screen.y, screen.height),
    width: pct(shown.width, screen.width),
    height: pct(shown.height, screen.height),
  };

  const fields: { key: "x" | "y" | "width" | "height"; label: string }[] = [
    { key: "x", label: t("subtitle.posX") },
    { key: "y", label: t("subtitle.posY") },
    { key: "width", label: t("subtitle.posW") },
    { key: "height", label: t("subtitle.posH") },
  ];

  return (
    <div className="pos-picker">
      <div
        ref={mapRef}
        className="pos-map"
        style={{ aspectRatio: `${screen.width} / ${screen.height}` }}
      >
        <div
          className={`pos-rect${adjusting ? " pos-rect--adjusting" : ""}`}
          style={rectStyle}
          role="button"
          tabIndex={disabled ? -1 : 0}
          data-focus-item
          aria-pressed={adjusting}
          aria-disabled={disabled}
          aria-label={t("subtitle.posMapLabel")}
          aria-description={t(adjusting ? "subtitle.posMapAdjusting" : "subtitle.posMapHint")}
          onKeyDown={onKeyDown}
          onBlur={() => setAdjusting(false)}
          onPointerDown={(e) => begin("move", e)}
          onPointerMove={move}
          onPointerUp={end}
          onPointerCancel={end}
        >
          <span className="pos-rect__tag" aria-hidden="true">
            {t("subtitle.posRectTag")}
          </span>
          {EDGES.map((edge) => (
            <span
              key={edge}
              className={`pos-handle pos-handle--${edge}`}
              aria-hidden="true"
              onPointerDown={(e) => begin(edge, e)}
            />
          ))}
        </div>
      </div>
      <p className="pos-hint">
        {adjusting ? t("subtitle.posMapAdjusting") : t("subtitle.posMapHint")}
      </p>

      <div className="pos-controls">
        <div className="pos-presets" role="group" aria-label={t("subtitle.posPresets")}>
          {PRESETS.map((p) => (
            <button
              key={p.key}
              type="button"
              className="pos-preset"
              data-focus-item
              disabled={disabled}
              aria-label={t(p.key)}
              title={t(p.key)}
              onClick={() => setGeometry(presetGeometry(screen, shown, p.row, p.col))}
            >
              <span aria-hidden="true" />
            </button>
          ))}
        </div>
        <div className="pos-fields">
          {fields.map((f) => (
            <PosNumber
              key={f.key}
              label={f.label}
              value={shown[f.key]}
              disabled={disabled}
              onCommit={(v) => setGeometry({ ...shown, [f.key]: v })}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

/** 数字框：打字期间自己持有草稿，回车 / 失焦才提交（提交值由上层夹进屏幕）。 */
function PosNumber({
  label,
  value,
  disabled,
  onCommit,
}: {
  label: string;
  value: number;
  disabled?: boolean;
  onCommit: (v: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  function commit() {
    if (draft === null) return;
    const n = Math.round(Number(draft));
    setDraft(null);
    if (draft.trim() !== "" && Number.isFinite(n) && n !== value) onCommit(n);
  }
  return (
    <label className="pos-field">
      <span>{label}</span>
      <input
        type="number"
        className="form-input mono"
        value={draft ?? String(value)}
        disabled={disabled}
        data-focus-item
        onChange={(e) => setDraft(e.currentTarget.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
        }}
      />
    </label>
  );
}
