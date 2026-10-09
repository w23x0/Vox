/**
 * 浮空侧栏：图标 + 文字，分三组（翻译 / 连接 / 系统），首页单列。
 * 窗口 < 860px 时收成纯图标（文字隐藏，组标题换成分隔线，悬停出 tooltip）。
 * 底部只放应用版本号；有新版时多一枚「有更新」徽章，点击跳关于页。
 *
 * 界面语言与主题在「设置」页，不在侧栏。
 */

import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { createPortal } from "react-dom";

import { useT } from "../i18n/context";
import { checkAppUpdate, loadAppVersion, useAppInfo } from "../lib/appinfo";
import { NAV_GROUPS } from "../nav";

export function Sidebar({
  page,
  onNavigate,
}: {
  page: string;
  onNavigate: (id: string) => void;
}) {
  const t = useT();
  const { version, update } = useAppInfo();
  const [tip, setTip] = useState<{ label: string; y: number } | null>(null);

  // 启动时静默拿一次版本号并问一次有没有新版；失败（离线 / 浏览器里）不打扰。
  useEffect(() => {
    void loadAppVersion();
    void checkAppUpdate().catch(() => undefined);
  }, []);

  const hoverTip = useCallback(
    (label: string) => ({
      onMouseEnter: (event: MouseEvent<HTMLButtonElement>) => {
        const rect = event.currentTarget.getBoundingClientRect();
        setTip({ label, y: rect.top + rect.height / 2 });
      },
      onMouseLeave: () => setTip(null),
    }),
    [],
  );

  return (
    <>
      <aside className="sidebar" data-tauri-drag-region data-focus-zone="sidebar">
        <nav className="sidebar-nav" aria-label={t("sidebar.navLabel")}>
          {NAV_GROUPS.map((group) => (
            <div className="nav-group" key={group.id}>
              {group.title ? <div className="nav-group-title">{group.title(t)}</div> : null}
              {group.items.map((item) => {
                const Icon = item.icon;
                const label = item.label(t);
                const active = page === item.id;
                return (
                  <button
                    key={item.id}
                    type="button"
                    className={active ? "nav-item active" : "nav-item"}
                    data-page={item.id}
                    data-focus-item
                    aria-current={active ? "page" : undefined}
                    aria-label={label}
                    onClick={() => onNavigate(item.id)}
                    {...hoverTip(label)}
                  >
                    <span className="nav-dot" aria-hidden="true" />
                    <Icon />
                    <span className="nav-label">{label}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </nav>

        <div className="sidebar-foot">
          {update ? (
            <button
              type="button"
              className="sidebar-update"
              aria-label={t("sidebar.updateAvailable", { ver: update })}
              onClick={() => onNavigate("about")}
              {...hoverTip(t("sidebar.updateAvailable", { ver: update }))}
            >
              <span className="sidebar-update-dot" aria-hidden="true" />
              <span className="sidebar-update-text">{t("sidebar.updateBadge")}</span>
            </button>
          ) : null}
          <span className="sidebar-version mono">v{version}</span>
        </div>
      </aside>

      {tip
        ? createPortal(
            <div className="nav-tooltip" role="tooltip" style={{ top: tip.y }}>
              {tip.label}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
