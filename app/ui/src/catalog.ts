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
import registryData from "../../../catalog/languages.json";
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
  /** 方言表：匹配前缀 + 语言能力（源/目标语言集合、语音或仅文字、线上码映射）。 */
  dialects?: Record<string, DialectRaw>;
  api: { api_key_placeholder?: string };
  capabilities: ProviderCapabilities;
  voices?: { id: string; name: CatalogLabel; description: CatalogLabel }[];
  defaults?: { voice: string };
  schema_version?: number;
  legacy_removed_voice_ids?: string[];
}

/** 语言集合：显式列表，或 `"all_registered"`（登记表全集）。 */
type CodeSetRaw = string[] | "all_registered";

/** 一条方言的语言能力，形状同 `catalog/*.json` 的 `dialects.*.languages`。 */
interface DialectLanguagesRaw {
  source: CodeSetRaw;
  target_audio: CodeSetRaw;
  target_text_only: CodeSetRaw;
  /** 规范码 → 该服务商线上码（只列不同名的）。线上码只在 Rust 发报文时用，界面不读。 */
  wire: Record<string, string>;
  default_speak_target: string;
  default_listen_target: string;
}

interface DialectRaw {
  match_prefixes?: string[];
  languages: DialectLanguagesRaw;
}

/** 展开后的方言语言表（`all_registered` 已按登记表展开）。 */
export interface DialectLanguages {
  source: string[];
  target_audio: string[];
  target_text_only: string[];
  default_speak_target: string;
  default_listen_target: string;
}

/** 目标语言的输出形态：语音+文字，或仅文字。 */
export type TargetMode = "audio" | "text_only";

/** 统一语言登记表（`catalog/languages.json`）：规范码 + 英文兜底名。 */
const REGISTRY = registryData as unknown as { languages: { code: string; en: string }[] };
const REGISTERED_CODES: string[] = REGISTRY.languages.map((l) => l.code);

function resolveSet(set: CodeSetRaw): string[] {
  return set === "all_registered" ? [...REGISTERED_CODES] : [...set];
}

/** 这家的推荐模型。目录必须给出恰好一条 `recommended`，否则这一层退回首条。 */
function recommended_model(raw: ProviderRaw): CatalogModelRaw {
  const models = raw.models ?? [];
  return models.find((model) => model.recommended) ?? models[0] ?? { id: "", dialect: "", recommended: true, label: { zh: "", en: "", ja: "" } };
}

/** 内置 JSON（编译期打进二进制）解析成 registry 结构。 */
function parse_data(raw: ProviderRaw): RegistryEntry {
  const dialects: Record<string, ResolvedDialect> = {};
  for (const [id, dialect] of Object.entries(raw.dialects ?? {})) {
    dialects[id] = {
      id,
      match_prefixes: dialect.match_prefixes ?? [],
      languages: {
        source: resolveSet(dialect.languages.source),
        target_audio: resolveSet(dialect.languages.target_audio),
        target_text_only: resolveSet(dialect.languages.target_text_only),
        default_speak_target: dialect.languages.default_speak_target,
        default_listen_target: dialect.languages.default_listen_target,
      },
    };
  }
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
    dialects,
    voices: raw.voices ?? [],
    defaults: raw.defaults ?? { voice: "" },
    legacy_removed_voice_ids: raw.legacy_removed_voice_ids ?? [],
  };
}

interface ResolvedDialect {
  id: string;
  match_prefixes: string[];
  languages: DialectLanguages;
}

interface RegistryEntry {
  provider: ProviderCatalog;
  /** 这家登记在册的模型（维护表里的，不是官方列表接口拉回来的）。 */
  models: CatalogModelRaw[];
  dialects: Record<string, ResolvedDialect>;
  voices: { id: string; name: CatalogLabel; description: CatalogLabel }[];
  defaults: { voice: string };
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

/** 覆盖版目录的最低 schema：4 起语言按方言声明（旧覆盖版没有这张表，读进来界面就没有语言可选）。 */
const MIN_OVERRIDE_SCHEMA = 4;

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
      // 只认 schema_version >= MIN_OVERRIDE_SCHEMA 的目录，否则保持内置。
      if (parsed && typeof parsed === "object" && "schema_version" in parsed) {
        const sv = (parsed as { schema_version?: number }).schema_version;
        if (typeof sv !== "number" || sv < MIN_OVERRIDE_SCHEMA) continue;
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
      if (typeof sv !== "number" || sv < MIN_OVERRIDE_SCHEMA) continue;
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

/** 界面语言 → `Intl` 用的 BCP-47 标签（三个界面语言本身就是合法标签）。 */
const nameFormats = new Map<UiLang, Intl.DisplayNames | null>();
const collators = new Map<UiLang, Intl.Collator>();

function displayNames(uiLang: UiLang): Intl.DisplayNames | null {
  if (!nameFormats.has(uiLang)) {
    try {
      nameFormats.set(uiLang, new Intl.DisplayNames([uiLang], { type: "language" }));
    } catch {
      nameFormats.set(uiLang, null);
    }
  }
  return nameFormats.get(uiLang) ?? null;
}

function collator(uiLang: UiLang): Intl.Collator {
  let c = collators.get(uiLang);
  if (!c) {
    c = new Intl.Collator(uiLang);
    collators.set(uiLang, c);
  }
  return c;
}

/**
 * 语言名：交给 `Intl.DisplayNames` 按界面语言现算，不手写翻译。
 * `Intl` 给不出（原样返回码、抛错）时用登记表的英文兜底名，登记表也没有就返回码本身。
 */
export function languageName(code: string, uiLang: UiLang): string {
  try {
    const name = displayNames(uiLang)?.of(code);
    if (name && name.toLowerCase() !== code.toLowerCase()) return name;
  } catch {
    // 码形不合法时 `of` 会抛 RangeError：落到兜底名。
  }
  return REGISTRY.languages.find((l) => l.code === code)?.en ?? code;
}

/** 登记表是否登记过这个规范码。 */
export function isRegisteredLanguage(code: string): boolean {
  return REGISTERED_CODES.includes(code);
}

/** 模型 → 方言：先按 `models[]` 的显式 id，再按方言的 `match_prefixes`；认不出回这家推荐模型的方言。 */
function dialectOf(provider: ModelProvider, model: string): ResolvedDialect | undefined {
  const row = get(provider);
  if (!row) return undefined;
  const explicit = row.models.find((m) => m.id === model)?.dialect;
  const byPrefix = Object.values(row.dialects).find((d) =>
    d.match_prefixes.some((prefix) => model.startsWith(prefix)),
  );
  const own = (explicit ? row.dialects[explicit] : undefined) ?? byPrefix;
  if (own) return own;
  const fallback = row.models.find((m) => m.recommended)?.dialect;
  return fallback ? row.dialects[fallback] : undefined;
}

/** 这个模型的方言语言表（目标/源语言集合与缺省）。 */
export function dialectLanguages(provider: ModelProvider, model: string): DialectLanguages | null {
  return dialectOf(provider, model)?.languages ?? null;
}

function sortedOptions(codes: readonly string[], uiLang: UiLang): LabeledOption[] {
  const compare = collator(uiLang);
  return codes
    .map((code) => ({ value: code, label: languageName(code, uiLang) }))
    .sort((a, b) => compare.compare(a.label, b.label));
}

/** 一组规范码的显示名，按界面语言排序（模型页展开行列语言用）。 */
export function languageNames(codes: readonly string[], uiLang: UiLang): string[] {
  return sortedOptions(codes, uiLang).map((option) => option.label);
}

/** 目标语言下拉：这个模型的方言支持的目标语言（语音+文字 ∪ 仅文字），按界面语言排序。 */
export function targetLanguageOptions(
  provider: ModelProvider,
  model: string,
  uiLang: UiLang,
): LabeledOption[] {
  const langs = dialectLanguages(provider, model);
  return sortedOptions(langs ? [...langs.target_audio, ...langs.target_text_only] : [], uiLang);
}

/** 源语言下拉：首项是"自动识别"，其余是方言支持的源语言。 */
export function sourceLanguageOptions(
  provider: ModelProvider,
  model: string,
  uiLang: UiLang,
  autoDetect: string,
): LabeledOption[] {
  const langs = dialectLanguages(provider, model);
  return [{ value: "", label: autoDetect }, ...sortedOptions(langs?.source ?? [], uiLang)];
}

/** 这个目标语言在该模型的方言里是语音+文字还是仅文字；方言不支持返回 `null`。 */
export function targetMode(provider: ModelProvider, model: string, code: string): TargetMode | null {
  const langs = dialectLanguages(provider, model);
  if (!langs) return null;
  if (langs.target_audio.includes(code)) return "audio";
  if (langs.target_text_only.includes(code)) return "text_only";
  return null;
}

/** 该模型能不能为这个目标语言出译音。 */
export function supportsAudioOutput(code: string, provider: ModelProvider, model: string): boolean {
  return targetMode(provider, model, code) === "audio";
}

/**
 * 目标语言归一：不被该模型的方言支持就回方言缺省（`speak` / `listen` 两条流水线缺省不同）。
 * `changed` 告诉调用方"是不是被换掉了"，好发通知。
 */
export function resolveTarget(
  provider: ModelProvider,
  model: string,
  code: string,
  pipeline: "speak" | "listen",
): { code: string; changed: boolean } {
  const langs = dialectLanguages(provider, model);
  if (!langs || targetMode(provider, model, code) !== null) return { code, changed: false };
  const fallback = pipeline === "speak" ? langs.default_speak_target : langs.default_listen_target;
  return { code: fallback, changed: true };
}

/** 源语言归一：方言不支持就回自动识别（`null`）。 */
export function resolveSource(
  provider: ModelProvider,
  model: string,
  code: string | null,
): { code: string | null; changed: boolean } {
  if (code === null || code === "") return { code: null, changed: false };
  const langs = dialectLanguages(provider, model);
  if (langs?.source.includes(code)) return { code, changed: false };
  return { code: null, changed: true };
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
  const langs = dialectLanguages(provider, defaultModelForProvider(provider));
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
    languages: langs ? { audio: langs.target_audio, text_only: langs.target_text_only } : null,
  };
}

/** 服务商能力里写的语言概述（如 "70+"）；目录没有语言明细的服务商靠它说话。 */
export function providerLanguageSummary(provider: ModelProvider): string | null {
  return get(provider)?.provider.capabilities.languages ?? null;
}

// ─── 首次启动种子（固定用内置，不被覆盖改） ────────────────────────────────

/** 阿里云推荐模型所属方言的缺省目标语言（首次启动种子）。 */
export const DEFAULT_TARGET_LANGUAGE =
  BUILTIN.aliyun!.dialects[recommended_model(aliyun as unknown as ProviderRaw).dialect]!.languages
    .default_speak_target;
/** 首次兜底的模型名（阿里云那张表的推荐模型）。 */
export const DEFAULT_MODEL_NAME = recommended_model(aliyun as unknown as ProviderRaw).id;
/** 首次兜底的音色 id。 */
export const DEFAULT_VOICE = aliyun.defaults.voice;
