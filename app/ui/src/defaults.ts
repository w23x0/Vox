/** 默认值与取值范围。镜像 crates/vox-core/src/settings.rs。 */

import type { Settings } from "./types";
import {
  DEFAULT_MODEL_NAME,
  DEFAULT_TARGET_LANGUAGE,
  DEFAULT_VOICE,
} from "./catalog";

export const SETTINGS_VERSION = 2;

export const GATE_THRESHOLD_RANGE = { min: 0, max: 0.2 } as const;
export const FONT_SIZE_RANGE = { min: 12, max: 96 } as const;
export const CHAR_TTL_RANGE = { min: 500, max: 20000 } as const;
export const CHAR_FADE_RANGE = { min: 0, max: 5000 } as const;
export const LINES_PER_TRACK_RANGE = { min: 1, max: 4 } as const;
export const BACKGROUND_ALPHA_RANGE = { min: 0, max: 255 } as const;
export const DIM_ALPHA_RANGE = { min: 0.05, max: 1 } as const;
/** Rust: `CONTROL_PORT_MIN`——小于它的端口一律归 0（内核保留段，`Settings::normalize`）。 */
export const CONTROL_PORT_MIN = 1024;
/** Rust: `TRANSCRIPT_NOTIFY_MS_RANGE`。 */
export const TRANSCRIPT_NOTIFY_MS_RANGE = { min: 50, max: 5000 } as const;

export const DEFAULT_SETTINGS: Settings = {
  version: SETTINGS_VERSION,
  speak: {
    enabled: false,
    provider: "aliyun",
    model_name: DEFAULT_MODEL_NAME,
    target_language: DEFAULT_TARGET_LANGUAGE,
    voice: DEFAULT_VOICE,
    voice_by_language: {},
    voice_clone_frequency: null,
    input_device: null,
    output_device: null,
    show_translation: true,
    speak_translation: true,
    translate: true,
    monitor_translation: false,
    activation_mode: "toggle",
    hotkey: { ctrl: false, alt: false, shift: false, key: "V" },
    gate_threshold: 0.012,
    denoise: true,
  },
  listen: {
    enabled: false,
    provider: "aliyun",
    model_name: DEFAULT_MODEL_NAME,
    target: null,
    target_language: "zh",
    output_device: null,
    show_translation: true,
    speak_translation: true,
    voice: DEFAULT_VOICE,
    hotkey: null,
    source_language: null,
  },
  subtitle: {
    visible: true,
    vr_overlay_enabled: false,
    font_family: "Microsoft YaHei UI",
    font_size: 30,
    speak_color: "#fff4de",
    listen_color: "#eef6ff",
    background_alpha: 165,
    char_ttl_ms: 2600,
    char_fade_ms: 900,
    lines_per_track: 2,
    dim_zeros: false,
    dim_alpha: 0.3,
    geometry: null,
  },
  autostart: false,
  start_minimized: false,
  ui_language: "zh-CN",
  control: {
    enabled: false,
    port: 0,
    allow_microphone: false,
    allow_system_audio: false,
    allow_audible_output: false,
    allow_config_write: false,
    transcript_notify_ms: 250,
  },
  aliyun_region: "cn",
  aliyun_endpoint: "",
  // 缺省与 Rust `NetworkSettings::default()` 一致：阿里云直连（国内站不该绕代理），
  // OpenAI / Gemini 跟随系统代理（国内多数人要靠代理才连得上）。
  network: {
    aliyun: { mode: "direct", proxy: "", proxy_auth: false },
    gpt: { mode: "system", proxy: "", proxy_auth: false },
    gemini: { mode: "system", proxy: "", proxy_auth: false },
  },
};

export function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) return min;
  return Math.min(max, Math.max(min, value));
}

/** 深拷贝一份默认设置，避免共享引用被就地改掉。 */
export function cloneSettings(settings: Settings): Settings {
  return structuredClone(settings);
}
