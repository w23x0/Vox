/**
 * 后端消息渲染：`{ key, params, detail? }` → 当前界面语言的一句话。
 *
 * 文案住在仓库根 `catalog/notices.json`（zh / en / ja），Rust 的无屏档读的是同一个文件
 * （`vox_core::notice`），所以两边的 English 不会漂移。
 *
 * - `key`：稳定的点分 id（`notice.*` 通知、`error.*` 命令错误）；
 * - `params`：只放可插值的值，模板里写 `{name}`；
 * - `detail`：服务端 / 系统给的**原文**，不翻译，原样接在译文后面。
 *
 * 缺 key 的回退顺序：当前语言 → English → key 本身。**不回退中文**——
 * 非中文用户看到中文比看到英文或 key 更糟。
 */

import catalog from "../../../../catalog/notices.json";
import type { UiLang } from "./types";

/** 后端发来的消息（通知与命令错误同一形状）。 */
export interface Msg {
  key: string;
  params?: Record<string, string>;
  detail?: string;
}

type Lang3 = "zh" | "en" | "ja";
type Tables = Record<UiLang, Record<string, string>>;
type Separators = Record<UiLang, string>;

const SHORT: Record<UiLang, Lang3> = { "zh-CN": "zh", en: "en", "ja-JP": "ja" };

interface CatalogShape {
  detail_separator: Record<Lang3, string>;
  messages: Record<string, Record<Lang3, string>>;
}

function tablesOf(shape: CatalogShape): { tables: Tables; separators: Separators } {
  const tables: Tables = { "zh-CN": {}, en: {}, "ja-JP": {} };
  const separators = {} as Separators;
  for (const lang of Object.keys(SHORT) as UiLang[]) {
    separators[lang] = shape.detail_separator[SHORT[lang]];
    for (const [key, texts] of Object.entries(shape.messages)) {
      const text = texts[SHORT[lang]];
      if (typeof text === "string" && text.length > 0) tables[lang][key] = text;
    }
  }
  return { tables, separators };
}

const BUILT_IN = tablesOf(catalog as unknown as CatalogShape);

/** 消息值长这样吗（命令的 `Err` 与通知都是）。 */
export function isMsg(value: unknown): value is Msg {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { key?: unknown }).key === "string"
  );
}

/**
 * 按给定的表渲染。纯函数，`check-i18n` 用合成的表验证回退顺序。
 * 查不到 key：当前语言 → English → key 本身。
 */
export function renderMsgFrom(
  tables: Tables,
  separators: Separators,
  lang: UiLang,
  msg: Msg,
): string {
  const template = tables[lang][msg.key] ?? tables.en[msg.key] ?? msg.key;
  const params = msg.params ?? {};
  let text = template.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in params ? String(params[name]) : match,
  );
  if (msg.detail) text += separators[lang] + msg.detail;
  return text;
}

/** 用内置文案渲染。 */
export function renderMsg(lang: UiLang, msg: Msg): string {
  return renderMsgFrom(BUILT_IN.tables, BUILT_IN.separators, lang, msg);
}

/** 渲染任何捕获到的错误：后端消息按语言渲染，其余（JS 错误、字符串）照原样转字符串。 */
export function renderError(lang: UiLang, error: unknown): string {
  return isMsg(error) ? renderMsg(lang, error) : String(error);
}
