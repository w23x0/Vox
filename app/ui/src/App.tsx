/**
 * 外壳：38px 标题栏（只有居中应用名）+ 浮空侧栏（图标 + 文字，窄窗收成图标）+ app-layout 主内容。
 *
 * 八个页面全在 DOM 里，靠 .page.active 切显，不做入场动画。
 *
 * 应用级键盘也挂在这里：方向键焦点网格（focus.ts）+ 快捷键（shortcuts.ts）。
 * 挂 document 而非内容区 —— 焦点可能停侧栏图标，但 PgUp/PgDn 滚的必须是内容区。
 */

import { useCallback, useEffect, useRef, useState } from "react";

import * as catalog from "./catalog";
import { useT } from "./i18n/context";
import {
  cycleTrafficLights,
  enterContent,
  focusItem,
  focusSidebar,
  isDropdownOpenAt,
  isEditingText,
  isInSidebar,
  isLostFocus,
  isModalOpen,
  isTextField,
  setEditingText,
  stepColumnLeft,
  stepColumnRight,
  stepSidebarBy,
  stepUpDown,
} from "./lib/focus";
import { handleShortcut } from "./lib/shortcuts";
import { NavigateContext } from "./lib/navigate";
import { ABOUT_NAV, DEFAULT_PAGE, PAGE_NAV } from "./nav";
import { AboutPage } from "./sections/About";
import { AgentControlPage } from "./sections/AgentControl";
import { ModelsPage } from "./sections/Models";
import { HomePage } from "./sections/Home";
import { SettingsPage } from "./sections/Settings";
import { SubtitlePage } from "./sections/Subtitle";
import { UsagePage } from "./sections/Usage";
import { VrchatPage } from "./sections/Vrchat";
import { Sidebar } from "./shell/Sidebar";
import { Titlebar } from "./shell/Titlebar";
import { useStore } from "./store";
import { restoreTheme } from "./ui/theme";
import { useToast } from "./ui/toast";

const PAGES: Record<string, () => React.ReactElement> = {
  home: HomePage,
  models: ModelsPage,
  subtitle: SubtitlePage,
  vrchat: VrchatPage,
  agent: AgentControlPage,
  settings: SettingsPage,
  usage: UsagePage,
  about: AboutPage,
};

/** 所有可切页（↑↓/Ctrl+Tab/Ctrl+n 都按这个顺序走） */
const PAGE_IDS: string[] = PAGE_NAV.map((n) => n.id);

/** 换页后把焦点送到对应侧栏图标（等一帧，.active 刷上后才行）。 */
function focusNavItem(page: string) {
  requestAnimationFrame(() => {
    focusItem(
      document.querySelector<HTMLElement>(`[data-focus-zone="sidebar"] [data-page="${page}"]`),
    );
  });
}

export function App() {
  const [page, setPage] = useState(DEFAULT_PAGE);
  const t = useT();
  useEffect(() => restoreTheme(), []);
  useNoticeToasts();

  // 启动时把 Rust 落盘的覆盖版目录灌进来。加载完成会触发重渲染，列表/下拉随即反映。
  const [catalogTick, setCatalogTick] = useState(0);
  useEffect(() => {
    void catalog.ensureCatalogLoaded().then(() => setCatalogTick((n) => n + 1));
    return catalog.subscribeCatalog(() => setCatalogTick((n) => n + 1));
  }, []);
  void catalogTick;

  // ref 反映最新 page，让 document keydown 闭包始终读到当前页，不重挂监听。
  const pageRef = useRef(page);
  pageRef.current = page;

  const selectPage = useCallback((next: string) => {
    setPage(next);
    focusNavItem(next);
  }, []);

  useEffect(() => {
    function scrollContent(dir: number) {
      const c = document.querySelector('.app-content');
      if (!c) return;
      c.scrollBy({ top: dir * c.clientHeight * 0.9, behavior: 'smooth' });
    }

    function onKeyDown(e: KeyboardEvent) {
      // 快捷键（带修饰键）在输入框里也优先（Ctrl+, 从哪都能开设置）。
      if (
        handleShortcut(e, {
          cyclePage: (d) => {
            const list = PAGE_IDS;
            const i = list.indexOf(pageRef.current);
            const next = list[(i + d + list.length) % list.length];
            selectPage(next);
          },
          gotoPage: (n) => {
            const target = PAGE_IDS[n - 1];
            if (target) selectPage(target);
          },
          openSettings: () => selectPage('settings'),
          onEscape: () => {
            if (isModalOpen()) return false;
            const el = document.activeElement as HTMLElement | null;
            if (isTextField(el)) {
              if (isEditingText(el)) {
                setEditingText(el, false);
                return true;
              }
              if (!isInSidebar()) {
                focusSidebar();
                return true;
              }
              return false;
            }
            if (!isInSidebar()) {
              focusSidebar();
              return true;
            }
            return false;
          },
        })
      ) {
        e.preventDefault();
        return;
      }

      // Tab：只循环窗口红绿灯。模态框打开时让位。
      if (e.key === 'Tab' && !isModalOpen()) {
        cycleTrafficLights(e.shiftKey ? -1 : 1);
        e.preventDefault();
        return;
      }

      // Ctrl+Enter：文本域换行（编辑里插新行，不退出编辑）
      if (e.ctrlKey && !e.altKey && !e.metaKey && e.key === 'Enter') {
        const el = document.activeElement as HTMLElement | null;
        if (el && el.tagName === 'TEXTAREA') {
          el.focus();
          document.execCommand('insertText', false, '\n');
          e.preventDefault();
        }
        return;
      }

      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (isModalOpen()) return;

      const el = document.activeElement as HTMLElement | null;

      // 展开的下拉：方向键交还给下拉自己（移选项 / 收起）。
      if (isDropdownOpenAt(el)) return;

      // 原生 range 滑块：←/→ 交给它自己做值微调，↑/↓ 仍走网格行间。
      const isRange = el instanceof HTMLInputElement && el.type === 'range';
      if (isRange) {
        if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') return;
      }

      // 文本输入框：编辑态方向键归光标；未编辑只放方向键走导航，其它键原样放行。
      if (isTextField(el)) {
        if (e.key === 'Enter') {
          setEditingText(el, false);
          e.preventDefault();
          return;
        }
        if (isEditingText(el)) return;
        if (!['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.key)) return;
      }

      // 失焦：第一下方向键直接从「当前激活页图标」出发导航，不浪费一拍。
      if (isLostFocus() && ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.key)) {
        e.preventDefault();
        if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
          stepSidebarBy(pageRef.current, selectPage, e.key === 'ArrowDown' ? 1 : -1);
        } else if (e.key === 'ArrowRight') {
          enterContent();
        }
        return;
      }

      switch (e.key) {
        case 'ArrowUp':
        case 'ArrowDown':
        case 'ArrowLeft':
        case 'ArrowRight': {
          e.preventDefault();
          if (isInSidebar()) {
            // 侧栏 ↑↓ 整条走各页，→ 进内容区首控件
            if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
              stepSidebarBy(pageRef.current, selectPage, e.key === 'ArrowDown' ? 1 : -1);
            } else if (e.key === 'ArrowRight') {
              enterContent();
            }
            break;
          }
          // 内容区：↑↓ 相邻行（列号夹紧）；→ 右移列；← 第 0 列回侧栏。
          if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            stepUpDown(e.key === 'ArrowDown' ? 1 : -1);
          } else if (e.key === 'ArrowRight') {
            stepColumnRight();
          } else if (!stepColumnLeft()) {
            focusSidebar();
          }
          break;
        }
        case 'PageUp':
          scrollContent(-1);
          e.preventDefault();
          break;
        case 'PageDown':
          scrollContent(1);
          e.preventDefault();
          break;
        case 'Home': {
          const c = document.querySelector('.app-content');
          c?.scrollTo({ top: 0, behavior: 'smooth' });
          e.preventDefault();
          break;
        }
        case 'End': {
          const c = document.querySelector('.app-content');
          c?.scrollTo({ top: c.scrollHeight, behavior: 'smooth' });
          e.preventDefault();
          break;
        }
        default:
          return;
      }
    }

    // 编辑触发：点进去 / 输入了内容 = 打开；焦点离开 = 关闭。
    function onPointerDown(ev: PointerEvent) {
      if (isTextField(ev.target as Element)) setEditingText(ev.target as Element, true);
    }
    function onInput(ev: Event) {
      if (isTextField(ev.target as Element)) setEditingText(ev.target as Element, true);
    }
    function onBlur(ev: FocusEvent) {
      if (isTextField(ev.target as Element)) setEditingText(ev.target as Element, false);
    }

    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('input', onInput, true);
    document.addEventListener('blur', onBlur, true);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('input', onInput, true);
      document.removeEventListener('blur', onBlur, true);
    };
  }, [selectPage]);

  return (
    <NavigateContext.Provider value={selectPage}>
      <Titlebar />
      <Sidebar page={page} onNavigate={selectPage} />
      <div className="app-layout">
        <div className="app-content">
          {PAGE_NAV.map((n) => {
            const Page = PAGES[n.id];
            if (!Page) return null;
            return (
              <section
                key={n.id}
                id={`page-${n.id}`}
                className={page === n.id ? "page active" : "page"}
                aria-labelledby={`title-${n.id}`}
              >
                <div className={n === ABOUT_NAV ? "page-scroll narrow" : "page-scroll"}>
                  <div className="page-head">
                    <h1 id={`title-${n.id}`}>{n.label(t)}</h1>
                  </div>
                  <Page />
                </div>
              </section>
            );
          })}
        </div>
      </div>
    </NavigateContext.Provider>
  );
}

/**
 * 把后端的 notices 和通信错误转成 toast。
 */
function useNoticeToasts() {
  const store = useStore();
  const toast = useToast();
  const t = useT();
  const notices = store.snapshot?.notices;
  const done = useRef<unknown>(null);
  const lastError = useRef<string | null>(null);
  const dismiss = useRef(store.dismissNotice);
  dismiss.current = store.dismissNotice;

  useEffect(() => {
    if (!notices || notices.length === 0) return;
    if (done.current === notices) return;
    done.current = notices;
    for (const n of notices) {
      if (n.severity !== "info" || n.pipeline) {
        const prefix = n.pipeline ? `${t(`pipeline.${n.pipeline}`)}：` : "";
        toast(n.severity === "error" ? "danger" : "warning", `${prefix}${n.text}`);
      }
      dismiss.current(0);
    }
  }, [notices, toast]);

  useEffect(() => {
    const err = store.error;
    if (err && err !== lastError.current) toast("danger", t("common.connectFailed", { error: err }));
    lastError.current = err;
  }, [store.error, toast]);
}
