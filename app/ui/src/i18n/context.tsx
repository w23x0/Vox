/**
 * i18n 运行时：LanguageProvider + useT()。
 *
 * 语言偏好从 store 的 `settings.ui_language` 来（Rust 持久化），
 * 切换即 patch({ ui_language })。本 Provider 在 StoreProvider 之内，
 * 读 settings 作为驱动源，向子树提供当前语言与翻译函数 `t`。
 */

import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";

import en from "./en";
import ja from "./ja";
import zh from "./zh";
import { UI_LANG_HTML, type Dict, type TParams, type UiLang } from "./types";

/** 把嵌套字典展平成点分 key → 文案，运行时简单查找。 */
const cache = new Map<Dict, Record<string, string>>();
function flatten(dict: Dict, prefix = ""): Record<string, string> {
  const cached = cache.get(dict);
  if (cached) return cached;
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(dict)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "string") {
      out[path] = value;
    } else {
      Object.assign(out, flatten(value, path));
    }
  }
  cache.set(dict, out);
  return out;
}

const FLAT: Record<UiLang, Record<string, string>> = {
  "zh-CN": flatten(zh),
  "ja-JP": flatten(ja),
  en: flatten(en),
};

/** 校验并替换 `{var}` 占位符。
 *
 * 查不到时的两级兜底：
 * 1. **缺的键回落到 English**（不回落中文——非中文用户看到中文比看到英文更糟）；
 * 2. 连 English 都没有 → 原样返回 key（便于排查漏翻译）。
 */
export function translate(
  lang: UiLang,
  key: string,
  params?: TParams,
): string {
  let text = FLAT[lang][key] ?? FLAT.en[key];
  if (text === undefined) text = key;
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in params ? String(params[name]) : match,
  );
}

/** 语言上下文：当前语言 + 翻译函数 + 切换语言（同步写 settings）。 */
export interface I18nState {
  uiLang: UiLang;
  t: (key: string, params?: TParams) => string;
}

const I18nContext = createContext<I18nState | null>(null);

/** 从 settings.ui_language 校验并取一个合法 UiLang（白名单外回退 English）。 */
export function toUiLang(value: string | undefined): UiLang {
  return value === "zh-CN" || value === "ja-JP" ? value : "en";
}

/**
 * 语言 Provider。props 传当前 UI 语言的**值**（而非直接挂 store），
 * 这样既能被 Settings 之外任何持有 `settings.ui_language` 的地方复用，
 * 也保持纯展示层自我更新、不依赖特定状态容器。
 */
export function LanguageProvider({
  uiLang,
  children,
}: {
  uiLang: string;
  children: ReactNode;
}) {
  const lang = toUiLang(uiLang);

  // <html lang> 跟界面语言走：:lang() 才能给 CJK 选对字体、日文汉字才用日文字形。
  useEffect(() => {
    document.documentElement.lang = UI_LANG_HTML[lang];
  }, [lang]);

  const value = useMemo<I18nState>(
    () => ({ uiLang: lang, t: (key, params) => translate(lang, key, params) }),
    [lang],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

/** 取语言上下文。必须在 <LanguageProvider> 内。 */
export function useLang(): I18nState {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useLang 必须在 <LanguageProvider> 内使用");
  return ctx;
}

/** 便捷短名：`const { t } = useT();`。 */
export function useT(): I18nState["t"] {
  return useLang().t;
}