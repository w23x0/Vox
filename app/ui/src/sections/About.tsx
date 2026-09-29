import { useEffect, useRef, useState } from "react";
import * as catalog from "../catalog";
import { CapabilityPanel } from "../components/Capability";
import { useLang, useT } from "../i18n/context";
import { useStore } from "../store";
import type { ModelProvider } from "../types";
import { useToast } from "../ui/toast";

const PROVIDERS: ModelProvider[] = ["aliyun", "gemini", "gpt"];

type RowState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "checked"; current: string; latest: string }
  | { kind: "applying" }
  | { kind: "applied"; last: string }
  | { kind: "failed" };

type AppState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; version: string }
  | { kind: "downloading" }
  | { kind: "failed" };

export function AboutPage() {
  const t = useT();
  const { uiLang } = useLang();
  const { api } = useStore();
  const toast = useToast();
  const [version, setVersion] = useState("0.1.0");
  const [rows, setRows] = useState<Record<string, RowState>>({});
  const [appState, setAppState] = useState<AppState>({ kind: "idle" });
  // 进页只自动检查一次；手动点按钮的行为照旧。
  const didAutoCheck = useRef(false);

  useEffect(() => {
    void import("@tauri-apps/api/app")
      .then(({ getVersion }) => getVersion())
      .then(setVersion)
      .catch(() => undefined);
  }, []);

  // 目录被覆盖后本组件订阅重渲染，让下拉/列表那行的展示名跟上去。
  useEffect(() => {
    return catalog.subscribeCatalog(() => setRows((r) => ({ ...r })));
  }, []);

  // 进入「关于」页自动检查一次。失败不弹窗（页面只做静默提示），让行内状态说话。
  useEffect(() => {
    if (didAutoCheck.current) return;
    didAutoCheck.current = true;
    for (const provider of PROVIDERS) {
      void check(provider, true);
    }
    void checkApp(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  const providerName = (provider: ModelProvider) =>
    catalog.providerLabel(provider, uiLang);

  const check = async (provider: ModelProvider, silent = false) => {
    setRows((r) => ({ ...r, [provider]: { kind: "checking" } }));
    try {
      const res = await api.checkCatalogUpdate(provider);
      setRows((r) => ({
        ...r,
        [provider]: { kind: "checked", current: res.current, latest: res.latest },
      }));
    } catch (error: unknown) {
      if (!silent) {
        toast("danger", t("about.catalogUpdateFailed", { error: String(error) }));
      }
      setRows((r) => ({ ...r, [provider]: { kind: "failed" } }));
    }
  };

  const apply = async (provider: ModelProvider) => {
    setRows((r) => ({ ...r, [provider]: { kind: "applying" } }));
    try {
      const res = await api.applyCatalogUpdate(provider);
      // 把刚落盘的目录灌进内存（不用等重启），并让相关组件重渲染。
      await catalog.reloadCatalog();
      toast("success", t("about.catalogApplied", { at: res.verified }));
      setRows((r) => ({ ...r, [provider]: { kind: "applied", last: res.verified } }));
    } catch (error: unknown) {
      toast("danger", t("about.catalogUpdateFailed", { error: String(error) }));
      setRows((r) => ({ ...r, [provider]: { kind: "idle" } }));
    }
  };

  // 检查程序本体有没有新版。纯前端走 updater 插件，不经过 Rust 命令，
  // 所以这里直接动态 import —— 非 Tauri 环境（mock/浏览器）会进 catch，落成 failed。
  const checkApp = async (silent = false) => {
    setAppState({ kind: "checking" });
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (!update) {
        setAppState({ kind: "upToDate" });
        return;
      }
      setAppState({ kind: "available", version: update.version });
      void update.close().catch(() => undefined);
    } catch (error: unknown) {
      if (!silent) {
        toast("danger", t("about.appInstallFailed", { error: String(error) }));
      }
      setAppState({ kind: "failed" });
    }
  };

  // 下载并安装新版。updater 装完需要重启才生效；这里不强制重启，
  // 跟目录更新那句「重启后生效」保持一致的口吻，交给用户自己重启。
  const installApp = async () => {
    setAppState({ kind: "downloading" });
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (!update) {
        setAppState({ kind: "upToDate" });
        return;
      }
      await update.downloadAndInstall();
      void update.close().catch(() => undefined);
      toast("success", t("about.appInstallDone"));
      setAppState({ kind: "upToDate" });
    } catch (error: unknown) {
      toast("danger", t("about.appInstallFailed", { error: String(error) }));
      setAppState({ kind: "failed" });
    }
  };

  return (
    <div className="panel">
      <div className="panel-top">
        <div className="panel-title">Vox</div>
        <span className="badge badge-running">v{version}</span>
      </div>
      <div className="panel-body">
        <div className="sub-row">
          <span>{t("about.product")}</span>
          <span className="num num-muted">{t("about.productValue")}</span>
        </div>

        <div className="sub-card" style={{ marginTop: 14 }}>
          <div className="sub-card-head">{t("about.catalogSection")}</div>
          {PROVIDERS.map((provider) => {
            const row = rows[provider] ?? { kind: "idle" as const };
            return (
              <div key={provider} className="row" style={{ gap: 10, padding: "6px 0" }}>
                <span style={{ minWidth: 120 }}>{providerName(provider)}</span>
                <span className="hint mono" style={{ flex: 1, minWidth: 0 }}>
                  {row.kind === "checked"
                    ? row.current === row.latest
                      ? t("about.catalogUpToDate", { at: row.current })
                      : t("about.catalogCurrent", { at: row.current })
                    : row.kind === "applied"
                      ? t("about.catalogApplied", { at: row.last })
                      : row.kind === "checking"
                        ? t("about.catalogChecking")
                        : row.kind === "applying"
                          ? t("about.catalogApplying")
                          : row.kind === "failed"
                            ? t("about.checkUpdateFailed")
                            : t("about.catalogNoOverride", { at: "builtin" })}
                </span>
                {row.kind === "checked" && row.latest !== row.current ? (
                  <>
                    <span className="badge badge-warn">
                      {t("about.catalogHasUpdate", { at: row.latest })}
                    </span>
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      data-focus-item
                      onClick={() => void apply(provider)}
                    >
                      {t("about.catalogApply")}
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm"
                    disabled={row.kind === "checking" || row.kind === "applying"}
                    data-focus-item
                    onClick={() => void check(provider)}
                  >
                    {t("about.checkUpdate")}
                  </button>
                )}
              </div>
            );
          })}
        </div>

        <div className="sub-card" style={{ marginTop: 14 }}>
          <div className="sub-card-head">{t("about.appSection")}</div>
          <div className="row" style={{ gap: 10, padding: "6px 0" }}>
            <span style={{ minWidth: 120 }}>{t("about.appChannels")}</span>
            <span className="hint mono" style={{ flex: 1, minWidth: 0 }}>
              {appState.kind === "upToDate"
                ? t("about.appUpToDate")
                : appState.kind === "available"
                  ? t("about.appAvailable", { ver: appState.version })
                  : appState.kind === "checking"
                    ? t("about.appChecking")
                    : appState.kind === "downloading"
                      ? t("about.appDownloading")
                      : appState.kind === "failed"
                        ? t("about.checkUpdateFailed")
                        : t("about.appUpToDate")}
            </span>
            {appState.kind === "available" ? (
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                data-focus-item
                onClick={() => void installApp()}
              >
                {t("about.appInstall")}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                disabled={appState.kind === "checking" || appState.kind === "downloading"}
                data-focus-item
                onClick={() => void checkApp()}
              >
                {t("about.checkUpdate")}
              </button>
            )}
          </div>
        </div>

        {/* 这台设备 / 这份构建能做什么：位 + reason 全摆出来（位是事实，不是期望）。 */}
        <CapabilityPanel />
      </div>
    </div>
  );
}