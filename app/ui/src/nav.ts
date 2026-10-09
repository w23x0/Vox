/** 导航表。侧栏图标 + 文字，label 同时用在窄屏 tooltip 和页面标题上。 */

import type { ReactElement } from "react";
import type { TParams } from "./i18n/types";

import {
  IconAgent,
  IconChart,
  IconHome,
  IconInfo,
  IconModel,
  IconSettings,
  IconSubtitles,
  IconVrc,
} from "./ui/icons";

export type LabelFn = (t: (key: string, params?: TParams) => string) => string;

export interface NavItem {
  id: string;
  /** 侧栏文字 + 页面大标题。渲染处用 `useT()` 求值，避免模块无 hook 环境。 */
  label: LabelFn;
  /** 侧栏图标。尺寸由 shell.css 的 .nav-item svg 统一定成 20px，这里不传。 */
  icon: (p: { size?: number }) => ReactElement;
}

export interface NavGroup {
  id: string;
  /** 组标题；首页单独一组，没有标题。 */
  title: LabelFn | null;
  items: NavItem[];
}

const HOME: NavItem = { id: "home", label: (t) => t("nav.home"), icon: IconHome };
const MODELS: NavItem = { id: "models", label: (t) => t("nav.models"), icon: IconModel };
const SUBTITLE: NavItem = { id: "subtitle", label: (t) => t("nav.subtitle"), icon: IconSubtitles };
const VRCHAT: NavItem = { id: "vrchat", label: (t) => t("nav.vrchat"), icon: IconVrc };
const AGENT: NavItem = { id: "agent", label: (t) => t("nav.agent"), icon: IconAgent };
const SETTINGS: NavItem = { id: "settings", label: (t) => t("nav.settings"), icon: IconSettings };
const USAGE: NavItem = { id: "usage", label: (t) => t("nav.usage"), icon: IconChart };
export const ABOUT_NAV: NavItem = { id: "about", label: (t) => t("nav.about"), icon: IconInfo };

/** 侧栏分组（组间在纯图标模式下用分隔线）。 */
export const NAV_GROUPS: NavGroup[] = [
  { id: "main", title: null, items: [HOME] },
  { id: "translate", title: (t) => t("nav.groupTranslate"), items: [MODELS, SUBTITLE] },
  { id: "connect", title: (t) => t("nav.groupConnect"), items: [VRCHAT, AGENT] },
  { id: "system", title: (t) => t("nav.groupSystem"), items: [SETTINGS, USAGE, ABOUT_NAV] },
];

/** 所有需要挂载到内容区的页面（也是 ↑↓ / Ctrl+Tab / Ctrl+n 的顺序）。 */
export const PAGE_NAV: NavItem[] = NAV_GROUPS.flatMap((g) => g.items);

export const DEFAULT_PAGE = "home";
