/**
 * 模型清单的前端缓存：首页的「更换模型」和模型页共用同一份，各家只问一次。
 *
 * - 正常情况下 `api.listModels` 总是 resolve 一份 `ModelList`（拉取失败时 `error` 非空，
 *   `models` 是后端的内置列表）；
 * - 命令本身调不通时，退回前端 `catalog.ts` 的内置信息，
 *   标 `source: "builtin"`、没有 `error`——界面上就是「内置列表」；
 * - 不缓存展示名以外的东西：内置兜底在渲染时按当前界面语言生成。
 */

import { useCallback, useEffect, useSyncExternalStore } from "react";

import type { VoxApi } from "../api";
import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import type { UiLang } from "../i18n/types";
import { useStore } from "../store";
import type { ModelEntry, ModelList, ModelProvider } from "../types";

/** `null` = 命令调不通（用前端内置兜底）。 */
const cache = new Map<ModelProvider, ModelList | null>();
const inflight = new Set<ModelProvider>();
const listeners = new Set<() => void>();
let version = 0;

function emit(): void {
  version += 1;
  for (const fn of listeners) fn();
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 向后端要某家的清单并落缓存。同一家同时只发一个请求。 */
export async function fetchModels(
  api: VoxApi,
  provider: ModelProvider,
  refresh: boolean,
): Promise<void> {
  if (inflight.has(provider)) return;
  inflight.add(provider);
  emit();
  try {
    cache.set(provider, await api.listModels(provider, { refresh }));
  } catch {
    cache.set(provider, null);
  } finally {
    inflight.delete(provider);
    emit();
  }
}

/** 前端兜底：只有目录里登记的那一条，不假装知道官方列表。 */
function builtinModelList(provider: ModelProvider, uiLang: UiLang): ModelList {
  return {
    provider,
    source: "builtin",
    fetched_at: null,
    error: null,
    models: [catalog.builtinModelEntry(provider, uiLang)],
  };
}

export interface ModelsView {
  list: ModelList;
  /** 正在向后端要（首次或刷新）。 */
  loading: boolean;
  refresh: () => void;
}

export function useModelList(provider: ModelProvider): ModelsView {
  const { api } = useStore();
  const { uiLang } = useLang();
  useSyncExternalStore(subscribe, () => version);

  useEffect(() => {
    if (!cache.has(provider)) void fetchModels(api, provider, false);
  }, [api, provider]);

  const refresh = useCallback(() => void fetchModels(api, provider, true), [api, provider]);
  const cached = cache.get(provider);
  return {
    list: cached ?? builtinModelList(provider, uiLang),
    loading: inflight.has(provider),
    refresh,
  };
}

/** 某个模型 ID 的展示名：清单里有就用清单的，否则目录里的，再不行就原样 ID。 */
export function modelDisplayName(list: ModelList, id: string, uiLang: UiLang): string {
  const entry: ModelEntry | undefined = list.models.find((m) => m.id === id);
  return entry ? entry.label : catalog.modelLabel(id, uiLang);
}
