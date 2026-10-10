/** 字幕外观：悬浮窗的显示开关、字体、颜色、整句淡出，加一块按真实参数渲染的预览。 */

import { useCallback } from "react";
import { hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { PositionPicker } from "../components/PositionPicker";
import {
  BACKGROUND_ALPHA_RANGE,
  CHAR_FADE_RANGE,
  CHAR_TTL_RANGE,
  LINES_PER_TRACK_RANGE,
  DIM_ALPHA_RANGE,
  FONT_SIZE_RANGE,
  clamp,
} from "../defaults";
import { useT } from "../i18n/context";
import { useStore } from "../store";
import type { SubtitleSettings } from "../types";
import { Dropdown, SettingsItem, Slider, Toggle } from "../ui/controls";

const FONTS = [
  { value: "Microsoft YaHei UI", label: "Microsoft YaHei UI" },
  { value: "Maple Mono Normal CN", label: "Maple Mono CN" },
  { value: "SimHei", label: "SimHei" },
  { value: "KaiTi", label: "KaiTi" },
  { value: "DengXian", label: "DengXian" },
];

/** 底衬颜色 = 纯黑 + 设置里的 alpha，和 vox-overlay-win 的 PLATE_COLOR 一致。 */
function plateStyle(sub: SubtitleSettings) {
  return {
    background: `rgba(0, 0, 0, ${(sub.background_alpha / 255).toFixed(3)})`,
    borderRadius: "var(--radius)",
  };
}

/** 预览里固定摆两行：对外（暖）在上、听人（冷）在下，颜色跟设置走。 */
function SubtitlePreview({ sub }: { sub: SubtitleSettings }) {
  const t = useT();
  const line = (color: string, text: string) => (
    <div
      className="subtitle-preview__row"
      style={{
        ...plateStyle(sub),
        color,
        fontSize: Math.min(sub.font_size, 30),
        fontFamily: sub.font_family,
      }}
    >
      {text.split("").map((ch, i) => (
        <span key={i} className="subtitle-preview__ch">
          {ch}
        </span>
      ))}
    </div>
  );
  return (
    <div className="subtitle-preview" aria-hidden="true">
      <span className="subtitle-preview__tag">{t("subtitle.previewTag")}</span>
      {line(sub.speak_color, t("subtitle.previewSpeakLine"))}
      {line(sub.listen_color, t("subtitle.previewListenLine"))}
    </div>
  );
}

export function SubtitlePage() {
  const { api, snapshot, settings, patch } = useStore();
  const loading = snapshot === null;
  const loadScreen = useCallback(() => api.screenRect(), [api]);
  const sub = settings.subtitle;
  const t = useT();
  /**
   * 字幕这一位（R1/R8）：位为假 = 这一档宿主没有屏幕（无屏档）——整页外观设置与预览
   * 都不渲染，只留一句 reason 说明"这台设备做不到"。位为真时这一页与从前一模一样。
   */
  const captions = hostBit(snapshot, "captions");
  if (!loading && !captions.enabled) {
    return (
      <div className="settings-group">
        <CapabilityNote bit="captions" />
      </div>
    );
  }

  return (
    <div className="sub-compact">
      <div className="settings-group">
        <div className="group-title">{t("subtitle.groupAppearance")}</div>
        <SettingsItem
          title={t("subtitle.visible")}
          desc={t("subtitle.visibleDesc")}
          control={
            <Toggle
              checked={sub.visible}
              disabled={loading}
              label={t("subtitle.visible")}
              onChange={(visible) => patch({ subtitle: { visible } })}
            />
          }
        />
        <div className="subtitle-preview-wrap">
          <SubtitlePreview sub={sub} />
        </div>
        <SettingsItem
          wide
          htmlFor="dd-subtitle-font"
          title={t("subtitle.font")}
          control={
            <Dropdown
              id="dd-subtitle-font"
              label={t("subtitle.font")}
              value={sub.font_family}
              options={
                FONTS.some((f) => f.value === sub.font_family)
                  ? FONTS
                  : [...FONTS, { value: sub.font_family, label: sub.font_family }]
              }
              onChange={(font_family) => patch({ subtitle: { font_family } })}
            />
          }
        />
        <SettingsItem
          wide
          title={t("subtitle.fontSize")}
          control={
            <Slider
              value={sub.font_size}
              min={FONT_SIZE_RANGE.min}
              max={FONT_SIZE_RANGE.max}
              step={1}
              disabled={loading}
              label={t("subtitle.fontSize")}
              format={(v) => t("subtitle.pxSuffix", { v })}
              onChange={(font_size) => patch({ subtitle: { font_size } })}
            />
          }
        />
        <SettingsItem
          title={t("subtitle.speakColor")}
          desc={t("subtitle.speakColorDesc")}
          control={
            <input
              type="color"
              className="color-input"
              value={sub.speak_color}
              disabled={loading}
              aria-label={t("subtitle.speakColor")}
              onChange={(e) => patch({ subtitle: { speak_color: e.target.value } })}
            />
          }
        />
        <SettingsItem
          title={t("subtitle.listenColor")}
          desc={t("subtitle.listenColorDesc")}
          control={
            <input
              type="color"
              className="color-input"
              value={sub.listen_color}
              disabled={loading}
              aria-label={t("subtitle.listenColor")}
              onChange={(e) => patch({ subtitle: { listen_color: e.target.value } })}
            />
          }
        />
        <SettingsItem
          wide
          title={t("subtitle.background")}
          desc={t("subtitle.backgroundDesc")}
          control={
            <Slider
              value={sub.background_alpha}
              min={BACKGROUND_ALPHA_RANGE.min}
              max={BACKGROUND_ALPHA_RANGE.max}
              step={1}
              disabled={loading}
              label={t("subtitle.background")}
              format={(v) => t("subtitle.bgAlphaLabel", { v })}
              onChange={(background_alpha) => patch({ subtitle: { background_alpha } })}
            />
          }
        />
      </div>

      <div className="settings-group">
        <div className="group-title">{t("subtitle.groupBehavior")}</div>
        <SettingsItem
          wide
          title={t("subtitle.linesPerTrack")}
          desc={t("subtitle.linesPerTrackDesc")}
          control={
            <Slider
              value={sub.lines_per_track}
              min={LINES_PER_TRACK_RANGE.min}
              max={LINES_PER_TRACK_RANGE.max}
              step={1}
              disabled={loading}
              label={t("subtitle.linesPerTrack")}
              format={(v) => t("subtitle.linesSuffix", { v })}
              onChange={(lines_per_track) => patch({ subtitle: { lines_per_track } })}
            />
          }
        />
        <SettingsItem
          wide
          title={t("subtitle.charTtl")}
          desc={t("subtitle.charTtlDesc")}
          control={
            <Slider
              value={sub.char_ttl_ms}
              min={CHAR_TTL_RANGE.min}
              max={CHAR_TTL_RANGE.max}
              step={100}
              disabled={loading}
              label={t("subtitle.charTtl")}
              format={(v) => t("subtitle.secSuffix", { v: (v / 1000).toFixed(1) })}
              onChange={(char_ttl_ms) =>
                patch({
                  subtitle: {
                    char_ttl_ms,
                    // 后端归一化会把淡出夹到不超过停留，这里提前跟上，
                    // 免得拖完两个滑块读数看着像没反应。
                    char_fade_ms: Math.min(sub.char_fade_ms, char_ttl_ms),
                  },
                })
              }
            />
          }
        />
        <SettingsItem
          wide
          title={t("subtitle.charFade")}
          desc={t("subtitle.charFadeDesc")}
          control={
            <Slider
              value={sub.char_fade_ms}
              min={CHAR_FADE_RANGE.min}
              max={clamp(CHAR_FADE_RANGE.max, CHAR_FADE_RANGE.min, sub.char_ttl_ms)}
              step={100}
              disabled={loading}
              label={t("subtitle.charFade")}
              format={(v) =>
                v === 0
                  ? t("subtitle.fadeInstant")
                  : t("subtitle.secSuffix", { v: (v / 1000).toFixed(1) })
              }
              onChange={(char_fade_ms) => patch({ subtitle: { char_fade_ms } })}
            />
          }
        />
      </div>

      <div className="settings-group">
        <div className="group-title">{t("subtitle.groupPosition")}</div>
        <SettingsItem
          title={t("subtitle.positionSize")}
          desc={t("subtitle.positionSizeDesc")}
          control={
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              data-focus-item
              onClick={() => patch({ subtitle: { geometry: null } })}
            >
              {t("subtitle.resetDefault")}
            </button>
          }
        />
        <PositionPicker
          geometry={sub.geometry}
          disabled={loading}
          loadScreen={loadScreen}
          onChange={(geometry) => patch({ subtitle: { geometry } })}
        />
      </div>

      <details className="settings-group sub-advanced">
        <summary data-focus-item className="group-title">
          {t("subtitle.groupAdvanced")}
        </summary>
        <SettingsItem
          title={t("subtitle.dimZeros")}
          desc={t("subtitle.dimZerosDesc")}
          control={
            <Toggle
              checked={sub.dim_zeros}
              disabled={loading}
              label={t("subtitle.dimZeros")}
              onChange={(dim_zeros) => patch({ subtitle: { dim_zeros } })}
            />
          }
        />
        <SettingsItem
          wide
          title={t("subtitle.dimAlpha")}
          desc={t("subtitle.dimAlphaDesc")}
          control={
            <Slider
              value={sub.dim_alpha}
              min={DIM_ALPHA_RANGE.min}
              max={DIM_ALPHA_RANGE.max}
              step={0.05}
              disabled={loading || !sub.dim_zeros}
              label={t("subtitle.dimAlpha")}
              format={(v) => `${Math.round(v * 100)}%`}
              onChange={(dim_alpha) => patch({ subtitle: { dim_alpha } })}
            />
          }
        />
      </details>
    </div>
  );
}
