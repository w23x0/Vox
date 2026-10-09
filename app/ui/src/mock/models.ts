/**
 * 假后端的 `list_models`：三家各一份正常列表。
 *
 * - 阿里云含一个 `adapted=false` 的条目和一个带 `offline_at` 的条目；
 * - `?mockErr=gpt` 让 OpenAI 拉取失败（网络错误），`?mockErr=gpt:auth` 是密钥无效，
 *   `?mockErr=gpt,gemini:other` 可以逗号叠加——失败时按契约退回内置列表并带上 `error`；
 * - `?mockErr=gpt:missing` 模拟「命令本身调不通」（真后端还没有 `list_models`）：直接抛错，
 *   界面要退回前端内置信息并标「内置列表」。
 */

import * as catalog from "../catalog";
import type { ModelEntry, ModelList, ModelListError, ModelProvider } from "../types";

const HOSTS: Record<ModelProvider, string> = {
  aliyun: "dashscope.aliyuncs.com",
  gpt: "api.openai.com",
  gemini: "generativelanguage.googleapis.com",
};

export interface MockModelFailures {
  /** 命令能调通，但官方列表拉取失败：返回内置列表 + `error`。 */
  errors: Partial<Record<ModelProvider, ModelListError>>;
  /** 命令本身调不通：`listModels` 直接抛错。 */
  missing: Set<ModelProvider>;
}

/** 解析 `?mockErr=`：provider[:kind]，逗号分隔。 */
export function parseMockErrors(search: string): MockModelFailures {
  const errors: Partial<Record<ModelProvider, ModelListError>> = {};
  const missing = new Set<ModelProvider>();
  const raw = new URLSearchParams(search).get("mockErr");
  for (const entry of raw?.split(",") ?? []) {
    const [provider, kind = "network"] = entry.split(":");
    if (provider !== "aliyun" && provider !== "gpt" && provider !== "gemini") continue;
    if (kind === "missing") {
      missing.add(provider);
      continue;
    }
    if (kind !== "auth" && kind !== "network" && kind !== "other") continue;
    errors[provider] = {
      kind,
      host: HOSTS[provider],
      message:
        kind === "auth" ? "401 Unauthorized" : kind === "network" ? "connection timed out" : "502 Bad Gateway",
    };
  }
  return { errors, missing };
}

/** 在内置那一条的基础上改字段，造出官方列表里的其它条目。 */
function variant(
  base: ModelEntry,
  over: Partial<ModelEntry> & { id: string; label: string },
): ModelEntry {
  return { ...base, recommended: false, snapshot_of: null, offline_at: null, ...over };
}

function officialModels(provider: ModelProvider): ModelEntry[] {
  // 展示名用英文：官方列表接口给的就是模型自己的名字，不随界面语言变。
  const base = catalog.builtinModelEntry(provider, "en");
  if (provider === "aliyun") {
    return [
      base,
      variant(base, {
        id: `${base.id}-2026-09-01`,
        label: "Qwen3.8 LiveTranslate Flash (2026-09-01)",
        snapshot_of: base.id,
      }),
      variant(base, {
        id: "qwen3-livetranslate-flash-realtime",
        label: "Qwen3 LiveTranslate Flash",
        offline_at: "2026-12-01",
        capabilities: { ...base.capabilities, voice_clone: false },
      }),
      variant(base, {
        id: "qwen3-omni-flash-realtime",
        label: "Qwen3 Omni Flash Realtime",
        adapted: false,
        languages: null,
        capabilities: {
          voice_selection: false,
          voice_clone: false,
          source_language: false,
          hot_update_language: false,
        },
      }),
    ];
  }
  if (provider === "gpt") {
    return [
      base,
      variant(base, {
        id: "gpt-realtime-translate-2026-08-15",
        label: "GPT Realtime Translate (2026-08-15)",
        snapshot_of: base.id,
      }),
      variant(base, {
        id: "gpt-realtime-2",
        label: "GPT Realtime 2",
        adapted: false,
        capabilities: { ...base.capabilities, hot_update_language: false },
      }),
    ];
  }
  return [
    base,
    variant(base, {
      id: "gemini-3.5-flash-live-preview",
      label: "Gemini 3.5 Flash Live",
      adapted: false,
    }),
  ];
}

/**
 * 一次 `list_models` 的假结果。没配密钥时拿不到官方列表：给内置列表、不报错
 * （和真后端「没密钥就不去问官方」的口径一致）。
 */
export function mockModelList(
  provider: ModelProvider,
  opts: {
    hasKey: boolean;
    fetchedAt: number;
    errors: Partial<Record<ModelProvider, ModelListError>>;
  },
): ModelList {
  const err = opts.errors[provider];
  const builtin = (error: ModelListError | null): ModelList => ({
    provider,
    source: "builtin",
    fetched_at: null,
    error,
    models: [catalog.builtinModelEntry(provider, "en")],
  });
  if (!opts.hasKey) return builtin(null);
  if (err) return builtin(err);
  return {
    provider,
    source: "official",
    fetched_at: opts.fetchedAt,
    error: null,
    models: officialModels(provider),
  };
}

/** 这个模型 ID 在该服务商的（假）官方列表里、并且已适配？merge.ts 归一化 `model_name` 时用。 */
export function isAdaptedModel(provider: ModelProvider, id: string): boolean {
  return officialModels(provider).some((m) => m.id === id && m.adapted);
}
