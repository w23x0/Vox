/**
 * 服务商目录的前端适配层。
 *
 * 模型、语言、音色和 API 元数据只维护在仓库根目录 `catalog/*.json`
 * （多语 label 存为 {zh,en,ja}）。Rust 构建脚本读取同一数据生成常量；
 * 这里直接导入，避免双份清单漂移。label 依赖当前界面语言，所以本模块只暴露
 * `l10n(label, uiLang)` 与按 uiLang 取 label 的函数；组件在体内调用。
 *
 * ## 运行时覆盖（在线更新模型目录）
 *
 * 默认用编译期打进来的内置副本。安装目录不可写，用户点「检查更新/应用更新」后
 * Rust 把新目录写到 `app_config_dir/catalog/*.json`。本模块启动时问 Rust 要覆盖版，
 * 有就替换对应服务商的目录。所有读目录的函数都从「当前 registry」取数，所以覆盖
 * 后只需要触发一次 React 重渲染，下拉/列表就会用到新数据。
 *
 * 注意：默认目标语言/默认音色/默认模型名这些「首次启动的种子值」仍然指向内置副本
 * 不变——已经是写进设置文件的用户选择不该被目录更新悄悄改掉；目录更新动的应该是
 * 「可选清单」（语言、音色、模型、服务商），而不是用户已存的设置。
 */

import aliyun from "../../../catalog/aliyun.json";
import gemini from "../../../catalog/gemini.json";
import gpt from "../../../catalog/gpt.json";
import type { ModelEntry, ModelProvider } from "./types";
import type { VoxApi } from "./api";
import type { UiLang } from "./i18n/types";

/** catalog JSON 里 label/name/description 的多语对象。键是 zh / en / ja。 */
export interface CatalogLabel {
  zh: string;
  en: string;
  ja: string;
}

/** 按当前界面语言取一个 label；未知语言回退 zh。 */
export function l10n(label: CatalogLabel, uiLang: UiLang): string {
  return uiLang === "en" ? label.en : uiLang === "ja-JP" ? label.ja : label.zh;
}

export interface LabeledOption {
  value: string;
  label: string;
}

export interface ModelInfo {
  name: string;
  label: CatalogLabel;
}

export interface ProviderCapabilities {
  languages?: string;
  voice_selection: boolean;
  voice_clone: boolean;
  source_language: boolean;
  hot_update_language: boolean;
}

export interface ProviderCatalog {
  id: ModelProvider;
  label: CatalogLabel;
  console_url: string;
  api_key_placeholder: string;
  model_name: string;
  model_label: CatalogLabel;
  capabilities: ProviderCapabilities;
}

/** `catalog/*.json` 的 `models[]` 里的一条（表里只放官方接口给不了的东西）。 */
export interface CatalogModelRaw {
  id: string;
  dialect: string;
  recommended: boolean;
  label: CatalogLabel;
}

/** 目录 JSON 的顶层形状。只声明前端要读的字段，协议常量等 extra 字段宽松忽略。 */
interface ProviderRaw {
  provider: { id: string; label: CatalogLabel; console_url: string };
  /** 多模型维护表；`recommended: true` 的那一条是"当前推荐模型"。 */
  models?: CatalogModelRaw[];
  /** 方言表（协议形状 + 匹配前缀）。前端这轮只用它的存在与否，不读内容。 */
  dialects?: Record<string, unknown>;
  api: { api_key_placeholder?: string };
  capabilities: ProviderCapabilities;
  languages?: { code: string; label: CatalogLabel; audio_output?: boolean }[];
  voices?: { id: string; name: CatalogLabel; description: CatalogLabel }[];
  defaults?: { target_language: string; listen_target_language: string; voice: string };
  schema_version?: number;
  legacy_removed_voice_ids?: string[];
}

/** 这家的推荐模型。目录必须给出恰好一条 `recommended`，否则这一层退回首条。 */
function recommended_model(raw: ProviderRaw): CatalogModelRaw {
  const models = raw.models ?? [];
  return models.find((model) => model.recommended) ?? models[0] ?? { id: "", dialect: "", recommended: true, label: { zh: "", en: "", ja: "" } };
}

/** 内置 JSON（编译期打进二进制）解析成 registry 结构。 */
function parse_data(raw: ProviderRaw): RegistryEntry {
  const languages = (raw.languages ?? []).map((l) => ({
    code: l.code,
    label: l.label,
    audio_output: l.audio_output ?? false,
  }));
  const recommended = recommended_model(raw);
  return {
    provider: {
      id: raw.provider.id as ModelProvider,
      label: raw.provider.label,
      console_url: raw.provider.console_url,
      api_key_placeholder: raw.api.api_key_placeholder ?? "",
      model_name: recommended.id,
      model_label: recommended.label,
      capabilities: {
        languages: raw.capabilities.languages,
        voice_selection: raw.capabilities.voice_selection ?? false,
        voice_clone: raw.capabilities.voice_clone ?? false,
        source_language: raw.capabilities.source_language ?? false,
        hot_update_language: raw.capabilities.hot_update_language ?? false,
      },
    },
    models: raw.models ?? [],
    languages,
    voices: raw.voices ?? [],
    defaults: raw.defaults ?? {
      target_language: "",
      listen_target_language: "",
      voice: "",
    },
    legacy_removed_voice_ids: raw.legacy_removed_voice_ids ?? [],
  };
}

interface RegistryEntry {
  provider: ProviderCatalog;
  /** 这家登记在册的模型（维护表里的，不是官方列表接口拉回来的）。 */
  models: CatalogModelRaw[];
  languages: { code: string; label: CatalogLabel; audio_output: boolean }[];
  voices: { id: string; name: CatalogLabel; description: CatalogLabel }[];
  defaults: { target_language: string; listen_target_language: string; voice: string };
  legacy_removed_voice_ids: string[];
}

/** 内置三个目录。作为覆盖版的回落底。 */
const BUILTIN: Record<string, RegistryEntry> = {
  aliyun: parse_data(aliyun as unknown as ProviderRaw),
  gemini: parse_data(gemini as unknown as ProviderRaw),
  gpt: parse_data(gpt as unknown as ProviderRaw),
};

/** 当前 registry。启动时 `loadCatalogOverrides` 可能替换个别服务商。 */
let registry: Record<string, RegistryEntry> = { ...BUILTIN };

/** 目录被替换后置位，组件订阅它来触发重渲染。 */
let version = 0;
const listeners = new Set<() => void>();

/** 订阅目录变化；覆盖后触发回调（通常是重渲染通知）。 */
export function subscribeCatalog(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

function notifyChanged() {
  version++;
  for (const fn of listeners) fn();
}

let initPromise: Promise<void> | null = null;

async function loadApi(): Promise<VoxApi> {
  const { getApi } = await import("./api");
  return getApi();
}

/** 启动时把 Rust 落盘的覆盖版目录灌进来。只会调用一次。 */
export function ensureCatalogLoaded(): Promise<void> {
  if (!initPromise) {
    initPromise = loadOverrides().catch(() => undefined);
  }
  return initPromise;
}

async function loadOverrides(): Promise<void> {
  let api: VoxApi;
  try {
    api = await loadApi();
  } catch {
    return;
  }
  const ids = ["aliyun", "gemini", "gpt"] as const;
  for (const id of ids) {
    let text: string | null = null;
    try {
      text = await api.readCatalogOverride(id);
    } catch {
      text = null;
    }
    if (!text) continue;
    try {
      const parsed = JSON.parse(text) as unknown;
      const entry = parse_data(parsed as ProviderRaw);
      // 只认 schema_version >= 2（多语 label）的目录，否则保持内置。
      if (parsed && typeof parsed === "object" && "schema_version" in parsed) {
        const sv = (parsed as { schema_version?: number }).schema_version;
        if (typeof sv !== "number" || sv < 2) continue;
      }
      registry = { ...registry, [id]: entry };
      notifyChanged();
    } catch {
      // 覆盖版坏了就回落内置，不阻塞界面。
    }
  }
}

function get(provider: ModelProvider): RegistryEntry | undefined {
  return registry[provider];
}

/**
 * 重新读取 Rust 落盘的覆盖版目录并替换 registry。
 * 与 `ensureCatalogLoaded` 不同，不保证「只跑一次」——About 页在「应用更新」后
 * 调它把刚落盘的目录也灌进内存。任何一处失败都保持原状态。
 */
export async function reloadCatalog(): Promise<void> {
  let api: VoxApi;
  try {
    api = await loadApi();
  } catch {
    return;
  }
  const ids = ["aliyun", "gemini", "gpt"] as const;
  for (const id of ids) {
    try {
      const text = await api.readCatalogOverride(id);
      if (!text) continue;
      const parsed = JSON.parse(text) as unknown;
      if (!(parsed && typeof parsed === "object" && "schema_version" in parsed)) continue;
      const sv = (parsed as { schema_version?: number }).schema_version;
      if (typeof sv !== "number" || sv < 2) continue;
      const entry = parse_data(parsed as ProviderRaw);
      registry = { ...registry, [id]: entry };
      notifyChanged();
    } catch {
      // 覆盖版读不进就保持原有（内置或上一次覆盖），不打断界面。
    }
  }
}

const PROVIDER_IDS: ModelProvider[] = ["aliyun", "gemini", "gpt"];

// ─── 服务商 ────────────────────────────────────────────────────────────────────

export function providerIds(): ModelProvider[] {
  return [...PROVIDER_IDS];
}

/** 服务商下拉选项（label 已按界面语言解析）。 */
export function providerOptions(uiLang: UiLang): LabeledOption[] {
  return PROVIDER_IDS.map((id) => ({
    value: id,
    label: l10n(get(id)!.provider.label, uiLang),
  }));
}

export function defaultModelForProvider(provider: ModelProvider): string {
  return get(provider)?.provider.model_name ?? DEFAULT_MODEL_NAME;
}

export function providerApiKeyPlaceholder(provider: ModelProvider): string {
  return get(provider)?.provider.api_key_placeholder ?? "";
}

export function providerConsoleUrl(provider: ModelProvider): string {
  return get(provider)?.provider.console_url ?? "";
}

export function supportsVoiceSelection(provider: ModelProvider): boolean {
  return get(provider)?.provider.capabilities.voice_selection ?? false;
}
export function supportsVoiceClone(provider: ModelProvider): boolean {
  return get(provider)?.provider.capabilities.voice_clone ?? false;
}
export function supportsSourceLanguage(provider: ModelProvider): boolean {
  return get(provider)?.provider.capabilities.source_language ?? false;
}
export function supportsHotUpdateLanguage(provider: ModelProvider): boolean {
  return get(provider)?.provider.capabilities.hot_update_language ?? false;
}

export function providerLabel(provider: ModelProvider, uiLang: UiLang): string {
  return get(provider) ? l10n(get(provider)!.provider.label, uiLang) : provider;
}

// ─── 语言 ──────────────────────────────────────────────────────────────────────

export function languageOptions(uiLang: UiLang): LabeledOption[] {
  return (get("aliyun")?.languages ?? []).map((language) => ({
    value: language.code,
    label: l10n(language.label, uiLang),
  }));
}

export function sourceLanguageOptions(uiLang: UiLang, autoDetect: string): LabeledOption[] {
  return [{ value: "", label: autoDetect }, ...languageOptions(uiLang)];
}

export function languageLabel(code: string, uiLang: UiLang): string {
  const language = get("aliyun")?.languages.find((candidate) => candidate.code === code);
  return language ? l10n(language.label, uiLang) : code;
}

export function supportsAudioOutput(language: string, provider: ModelProvider = "aliyun"): boolean {
  const row = get(provider);
  if (row?.provider.capabilities.voice_selection) {
    const lang = row.languages.find((candidate) => candidate.code === language);
    return lang ? lang.audio_output : false;
  }
  return (get("aliyun")?.languages ?? []).some((candidate) => candidate.code === language);
}

// ─── 音色 ──────────────────────────────────────────────────────────────────────

export interface CatalogVoice {
  id: string;
  name: CatalogLabel;
  description: CatalogLabel;
}

export function voiceCatalog(): CatalogVoice[] {
  return get("aliyun")?.voices ?? [];
}

export function legacyRemovedVoiceIds(): Set<string> {
  return new Set(get("aliyun")?.legacy_removed_voice_ids ?? []);
}

export function defaultVoiceForProvider(_provider: ModelProvider = "aliyun"): string {
  return get("aliyun")?.defaults.voice ?? DEFAULT_VOICE;
}

// ─── 模型 ──────────────────────────────────────────────────────────────────────

export function findModel(name: string): ModelInfo | undefined {
  for (const id of PROVIDER_IDS) {
    const model = get(id)?.models.find((candidate) => candidate.id === name);
    if (model) {
      return { name, label: model.label };
    }
  }
  return undefined;
}

/** 这家登记在册的模型（内置维护表，不是官方列表）。 */
export function catalogModels(provider: ModelProvider): CatalogModelRaw[] {
  return get(provider)?.models ?? [];
}

export function modelLabel(name: string, uiLang: UiLang): string {
  const model = findModel(name);
  return model ? l10n(model.label, uiLang) : name;
}

/**
 * 内置的那一条模型（目录里每家只登记一个当前推荐模型），形状同 `list_models` 的条目。
 * 拉不到官方列表时界面用它兜底；label 按界面语言解析。
 */
export function builtinModelEntry(provider: ModelProvider, uiLang: UiLang): ModelEntry {
  const row = get(provider);
  const caps = row?.provider.capabilities;
  const withLanguages = (row?.languages.length ?? 0) > 0 ? row : undefined;
  return {
    id: defaultModelForProvider(provider),
    label: row ? l10n(row.provider.model_label, uiLang) : defaultModelForProvider(provider),
    adapted: true,
    recommended: true,
    snapshot_of: null,
    offline_at: null,
    capabilities: {
      voice_selection: caps?.voice_selection ?? false,
      voice_clone: caps?.voice_clone ?? false,
      source_language: caps?.source_language ?? false,
      hot_update_language: caps?.hot_update_language ?? false,
    },
    languages: withLanguages
      ? {
          audio: withLanguages.languages.filter((l) => l.audio_output).map((l) => l.code),
          text_only: withLanguages.languages.filter((l) => !l.audio_output).map((l) => l.code),
        }
      : null,
  };
}

/** 服务商能力里写的语言概述（如 "70+"）；目录没有语言明细的服务商靠它说话。 */
export function providerLanguageSummary(provider: ModelProvider): string | null {
  return get(provider)?.provider.capabilities.languages ?? null;
}

// ─── 首次启动种子（固定用内置，不被覆盖改） ────────────────────────────────

/** 阿里云默认目标语言。界面上听的固定翻成中文。 */
export const DEFAULT_TARGET_LANGUAGE = aliyun.defaults.target_language;
/** 首次兜底的模型名（阿里云那张表的推荐模型）。 */
export const DEFAULT_MODEL_NAME = recommended_model(aliyun as unknown as ProviderRaw).id;
/** 首次兜底的音色 id。 */
export const DEFAULT_VOICE = aliyun.defaults.voice;
