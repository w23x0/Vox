/**
 * i18n 核心类型：语言枚举、字典形状、翻译接口。
 *
 * 架构一句话：语言偏好持久化在 Rust `settings.ui_language`（zh-CN / ja-JP / en），
 * 前端 `LanguageProvider` 监听它，向整棵组件树提供 `useT()`。
 * 翻译函数 `t(key)` 支持 `{var}` 占位符替换。
 */

/** 当前支持的界面语言。值是 settings `ui_language` 的取值。 */
export type UiLang = "zh-CN" | "ja-JP" | "en";

/** 语言下拉里每项用**该语言自己的名字**，不随当前界面语言变。 */
export const UI_LANG_NAMES: Record<UiLang, string> = {
  "zh-CN": "简体中文",
  en: "English",
  "ja-JP": "日本語",
};

/** 下拉里的排列顺序。 */
export const UI_LANG_ORDER: readonly UiLang[] = ["zh-CN", "en", "ja-JP"];

/** `<html lang>` 取值（BCP 47）：`:lang()` 据此选 CJK 字体，日文汉字走日文字形。 */
export const UI_LANG_HTML: Record<UiLang, string> = {
  "zh-CN": "zh-CN",
  "ja-JP": "ja",
  en: "en",
};

/** 按系统语言（`navigator.language`）选界面语言；不支持的一律 English。 */
export function detectUiLang(systemLang: string | undefined): UiLang {
  const l = (systemLang ?? "").toLowerCase();
  if (l.startsWith("zh")) return "zh-CN";
  if (l.startsWith("ja")) return "ja-JP";
  return "en";
}

/** 字典叶子：翻译后的字符串。占位符形如 `{name}`。 */
export type MessageValue = string | { [key: string]: MessageValue };

/** 字典：一个语言包的完整结构。key 集合两岸必须一致（en 用 `satisfies` 对齐）。 */
export type Dict = { [key: string]: MessageValue };

/** `t` 的替换参数：key → 变量名 → 值。 */
export type TParams = Record<string, unknown>;