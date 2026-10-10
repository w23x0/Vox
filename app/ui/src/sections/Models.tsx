/**
 * 模型页：左边三家服务商（状态点：已配置 / 未配置 / 出错），右边该服务商的
 * 密钥、地区 / 接入点（仅阿里云）、网络出口、可用模型（现场拉官方列表，
 * 拉不到退回内置列表并标注）。
 */

import { useEffect, useRef, useState } from "react";

import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import { fmtAgo } from "../lib/format";
import { fetchModels, useModelList } from "../lib/models";
import { useStore } from "../store";
import type {
  AliyunRegion,
  EndpointResult,
  ModelEntry,
  ModelList,
  ModelProvider,
  NetworkMode,
  NetworkReport,
  TakeoverReport,
} from "../types";
import { ConfirmButton } from "../ui/ConfirmButton";
import { Dropdown } from "../ui/controls";
import { IconChevron, IconExternal, IconRefresh, IconSave, IconSpeed } from "../ui/icons";
import { useToast } from "../ui/toast";

type ProviderStatus = "configured" | "none" | "error";

export function ModelsPage() {
  const { t, uiLang } = useLang();
  const [provider, setProvider] = useState<ModelProvider>(catalog.providerIds()[0] ?? "aliyun");

  return (
    <div className="models-layout">
      <nav className="provider-list" aria-label={t("models.providerListAria")}>
        {catalog.providerIds().map((id) => (
          <ProviderTab
            key={id}
            provider={id}
            name={catalog.providerLabel(id, uiLang)}
            selected={id === provider}
            onSelect={() => setProvider(id)}
          />
        ))}
      </nav>
      <ProviderDetail key={provider} provider={provider} />
    </div>
  );
}

function ProviderTab({
  provider,
  name,
  selected,
  onSelect,
}: {
  provider: ModelProvider;
  name: string;
  selected: boolean;
  onSelect: () => void;
}) {
  const { t } = useLang();
  const { snapshot } = useStore();
  const { list } = useModelList(provider);
  const configured = snapshot?.api_keys[provider] ?? false;
  const status: ProviderStatus = !configured ? "none" : list.error ? "error" : "configured";
  return (
    <button
      type="button"
      className={selected ? "provider-item selected" : "provider-item"}
      aria-pressed={selected}
      data-focus-item
      onClick={onSelect}
    >
      <span className={`provider-dot ${status}`} aria-hidden="true" />
      <span className="provider-name">{name}</span>
      <span className="provider-status">
        {snapshot === null
          ? t("models.loading")
          : status === "configured"
            ? t("models.statusConfigured")
            : status === "error"
              ? t("models.statusError")
              : t("models.statusNone")}
      </span>
    </button>
  );
}

function ProviderDetail({ provider }: { provider: ModelProvider }) {
  return (
    <div className="models-detail">
      <KeyCard provider={provider} />
      {provider === "aliyun" ? <EndpointCard /> : null}
      <NetworkCard provider={provider} />
      <ModelsCard provider={provider} />
    </div>
  );
}

/* ---- 密钥 -------------------------------------------------------- */

function KeyCard({ provider }: { provider: ModelProvider }) {
  const { api, snapshot, reload } = useStore();
  const toast = useToast();
  const { uiLang, t } = useLang();
  const [hasDraft, setHasDraft] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [busy, setBusy] = useState<"save" | "clear" | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const busyRef = useRef(false);
  const name = catalog.providerLabel(provider, uiLang);

  const loading = snapshot === null;
  const hasKey = snapshot?.api_keys[provider] ?? false;
  // 末四位由后端快照给；保存 / 清除后 reload() 会带回新值。
  const tail = snapshot?.api_key_tails[provider] ?? "";
  const editing = !hasKey || replacing;

  // 点「更换」后输入框才出现，出现时直接聚焦。
  useEffect(() => {
    if (replacing) input.current?.focus();
  }, [replacing]);

  const saveKey = async () => {
    if (busyRef.current) return;
    const key = input.current?.value.trim() ?? "";
    if (!key) return;
    busyRef.current = true;
    setBusy("save");
    if (input.current) input.current.value = "";
    setHasDraft(false);
    try {
      await api.setApiKey(provider, key);
      setReplacing(false);
      reload();
      void fetchModels(api, provider, true);
      toast("success", t("models.savedToast", { name }));
    } catch (error: unknown) {
      toast(
        "danger",
        t("models.saveFailed", {
          error: String(error).replaceAll(key, t("models.keyHidden")),
        }),
      );
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  const clearKey = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy("clear");
    try {
      await api.setApiKey(provider, "");
      reload();
      void fetchModels(api, provider, true);
      toast("success", t("models.clearedToast", { name }));
    } catch (error: unknown) {
      toast("danger", t("models.clearFailed", { error: String(error) }));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  return (
    <section className="card" aria-labelledby={`key-title-${provider}`}>
      <div className="card-head">
        <h2 className="card-title" id={`key-title-${provider}`}>
          {t("models.keyTitle")}
        </h2>
        <span className={hasKey ? "badge badge-running" : "badge badge-idle"}>
          <span className="status-dot" />
          {loading
            ? t("models.loading")
            : hasKey
              ? t("models.apiKeySet")
              : t("models.apiKeyNotSet")}
        </span>
      </div>

      {hasKey && !replacing ? (
        <div className="row-wrap">
          <span className="mono key-mask" aria-label={t("models.keyMaskAria")}>
            ••••{tail}
          </span>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={busy !== null}
            data-focus-item
            onClick={() => setReplacing(true)}
          >
            {t("models.replaceKey")}
          </button>
          <ConfirmButton
            confirmText={t("models.confirmClear")}
            disabled={busy !== null}
            onConfirm={() => void clearKey()}
          >
            {t("models.clearKey")}
          </ConfirmButton>
        </div>
      ) : null}

      {/* 输入框只在没配密钥或点了「更换」时出现；保存时先清空输入框，密钥不留在页面上 */}
      {editing ? (
        <>
          <div className="row" style={{ alignItems: "stretch" }}>
            <input
              id="f-api-key"
              ref={input}
              className="form-input mono"
              type="password"
              placeholder={catalog.providerApiKeyPlaceholder(provider)}
              autoComplete="off"
              spellCheck={false}
              disabled={busy !== null}
              aria-label={t("models.keyInputAria", { name })}
              onChange={(event) => setHasDraft(event.currentTarget.value.trim().length > 0)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void saveKey();
              }}
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={!hasDraft || busy !== null}
              data-focus-item
              onClick={() => void saveKey()}
            >
              <IconSave size={15} />
              {busy === "save" ? t("models.savingKey") : t("models.saveKey")}
            </button>
            {replacing ? (
              <button
                type="button"
                className="btn btn-secondary"
                disabled={busy !== null}
                data-focus-item
                onClick={() => {
                  setReplacing(false);
                  setHasDraft(false);
                }}
              >
                {t("common.cancel")}
              </button>
            ) : null}
          </div>
          {!hasDraft ? (
            <div className="hint" style={{ marginTop: 8 }}>
              {hasKey ? t("models.overwriteHint") : t("models.pasteHint")}
            </div>
          ) : null}
          <div className="row-wrap" style={{ marginTop: 12 }}>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              data-focus-item
              onClick={() => void api.openProviderConsole(provider)}
            >
              <IconExternal size={14} />
              {t("models.getKey")}
            </button>
          </div>
        </>
      ) : null}
    </section>
  );
}

/* ---- 地区 / 接入点（仅阿里云）------------------------------------- */

// 与后端 `usable_base` 同一条规则：`wss://` 任意主机；`ws://` 只许本机回环
// （密钥会随握手头明文发出，远端必须走 TLS）。
const WSS_URL = /^wss:\/\/\S+$/i;
const WS_LOOPBACK_URL = /^ws:\/\/(localhost|127(\.\d{1,3}){3}|\[::1\])(:\d+)?(\/\S*)?$/i;
const isValidEndpoint = (value: string) => WSS_URL.test(value) || WS_LOOPBACK_URL.test(value);

function EndpointCard() {
  const { settings, patch } = useStore();
  const { t } = useLang();
  const saved = settings.aliyun_endpoint ?? "";
  const [draft, setDraft] = useState(saved);
  const [invalid, setInvalid] = useState(false);

  // 后端归一化后的值覆盖回来时，输入框跟着走。
  useEffect(() => setDraft(saved), [saved]);

  const commit = () => {
    const value = draft.trim();
    if (value !== "" && !isValidEndpoint(value)) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    if (value !== saved) patch({ aliyun_endpoint: value });
  };

  return (
    <section className="card" aria-labelledby="region-title">
      <h2 className="card-title" id="region-title">
        {t("models.regionTitle")}
      </h2>
      <div className="settings-item" style={{ paddingTop: 12 }}>
        <div className="si-text">
          <div className="si-title">
            <label htmlFor="dd-aliyun-region">{t("models.region")}</label>
          </div>
          <div className="si-desc">{t("models.regionDesc")}</div>
        </div>
        <div className="si-control si-control-w">
          <Dropdown
            id="dd-aliyun-region"
            label={t("models.regionAria")}
            value={settings.aliyun_region}
            options={[
              { value: "cn", label: t("models.regionCn") },
              { value: "intl", label: t("models.regionIntl") },
            ]}
            onChange={(value) => patch({ aliyun_region: value as AliyunRegion })}
          />
        </div>
      </div>

      <details className="adv-details">
        <summary data-focus-item>{t("models.advanced")}</summary>
        <label className="stat-label" htmlFor="f-aliyun-endpoint">
          {t("models.endpoint")}
        </label>
        <input
          id="f-aliyun-endpoint"
          className="form-input mono"
          type="text"
          value={draft}
          placeholder="wss://…"
          autoComplete="off"
          spellCheck={false}
          aria-invalid={invalid}
          data-focus-item
          onChange={(event) => {
            setDraft(event.currentTarget.value);
            setInvalid(false);
          }}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") commit();
          }}
        />
        <div className={invalid ? "hint hint-danger" : "hint"} style={{ marginTop: 6 }}>
          {invalid ? t("models.endpointInvalid") : t("models.endpointHint")}
        </div>
        <div className="hint" style={{ marginTop: 4 }}>
          {t("models.endpointRestart")}
        </div>
      </details>
    </section>
  );
}

/* ---- 网络出口 ------------------------------------------------------ */

/**
 * 与后端 `vox_proxy::ProxySpec::parse` 同一条校验：只认 `http://` / `socks5://` /
 * `socks5h://`（也可不带 scheme，当 http），可带 `user:pass@`；端口缺省 8080 / 1080。
 * 只做"格式像不像"，真的连不上由测速那一栏报出来。
 */
const PROXY_SCHEME = /^(?:https?|socks5h?):\/\//i;
const PROXY_AUTHORITY =
  /^(?:[^@/\s]+@)?(?:\[[0-9a-fA-F:.]+\]|[^:\s/?#]+)(?::\d{1,5})?(?:[/?#]\S*)?$/;
const isValidProxy = (value: string) =>
  PROXY_AUTHORITY.test(PROXY_SCHEME.test(value) ? value.replace(/^[a-z0-9+.-]+:\/\//i, "") : value);

/** 模式三选一的下拉项（顺序即界面顺序）。 */
const NET_MODES: { value: NetworkMode; labelKey: string }[] = [
  { value: "direct", labelKey: "models.netModeDirect" },
  { value: "system", labelKey: "models.netModeSystem" },
  { value: "custom", labelKey: "models.netModeCustom" },
];

function NetworkCard({ provider }: { provider: ModelProvider }) {
  const { settings, patch, api } = useStore();
  const { t } = useLang();
  const toast = useToast();
  const saved = settings.network?.[provider];
  const mode: NetworkMode = saved?.mode ?? "direct";
  const [draft, setDraft] = useState(saved?.proxy ?? "");
  const [invalid, setInvalid] = useState(false);
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<NetworkReport | null>(null);
  const [takeover, setTakeover] = useState<TakeoverReport | null>(null);

  // 保存后的值（后端脱敏过的）覆盖回来时，输入框跟着走。
  useEffect(() => setDraft(saved?.proxy ?? ""), [saved?.proxy]);

  // 接管检测进卡片就查一次：它只解析服务商域名 + 扫网卡名，不发任何数据。
  useEffect(() => {
    let alive = true;
    void api
      .detectTakeover(provider)
      .then((r) => alive && setTakeover(r))
      .catch(() => alive && setTakeover(null));
    return () => {
      alive = false;
    };
  }, [api, provider]);

  const commitProxy = () => {
    const value = draft.trim();
    if (value !== "" && !isValidProxy(value)) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    if (value !== (saved?.proxy ?? "")) {
      // 只补丁这一格：带着账号的密码存在后端账本里，界面从来不碰它。
      patch({ network: { [provider]: { proxy: value } } });
    }
  };

  const runTest = async () => {
    setTesting(true);
    try {
      setReport(await api.testNetwork(provider));
    } catch (error: unknown) {
      toast("danger", t("models.netTestFailed", { error: String(error) }));
    } finally {
      setTesting(false);
    }
  };

  /**
   * TUN 引导：只对阿里云 + 直连模式（N4）。另两家本来就要走代理，
   * 被代理软件接管不是问题；阿里云直连时被绕到海外才会延迟升高。
   */
  const showTakeover = provider === "aliyun" && mode === "direct" && takeover?.detected === true;

  return (
    <section className="card" aria-labelledby={`net-title-${provider}`}>
      <div className="card-head">
        <h2 className="card-title" id={`net-title-${provider}`}>
          {t("models.netTitle")}
        </h2>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          style={{ marginLeft: "auto" }}
          disabled={testing}
          data-focus-item
          onClick={() => void runTest()}
        >
          <IconSpeed size={14} />
          {testing ? t("models.netTesting") : t("models.netTest")}
        </button>
      </div>

      <div className="settings-item" style={{ paddingTop: 12 }}>
        <div className="si-text">
          <div className="si-title">
            <label htmlFor={`dd-net-mode-${provider}`}>{t("models.netMode")}</label>
          </div>
          <div className="si-desc">{t(`models.netMode_${mode}`)}</div>
        </div>
        <div className="si-control si-control-w">
          <Dropdown
            id={`dd-net-mode-${provider}`}
            label={t("models.netModeAria")}
            value={mode}
            options={NET_MODES.map((m) => ({ value: m.value, label: t(m.labelKey) }))}
            onChange={(value) => patch({ network: { [provider]: { mode: value as NetworkMode } } })}
          />
        </div>
      </div>

      {mode === "custom" ? (
        <div className="settings-item">
          <div className="si-text">
            <div className="si-title">
              <label htmlFor={`f-net-proxy-${provider}`}>{t("models.netProxy")}</label>
            </div>
            <div className="si-desc">
              {saved?.proxy_auth ? t("models.netProxyAuthSet") : t("models.netProxyHint")}
            </div>
          </div>
          <div className="si-control si-control-w">
            <input
              id={`f-net-proxy-${provider}`}
              className="form-input mono"
              type="text"
              value={draft}
              placeholder="socks5h://127.0.0.1:1080"
              autoComplete="off"
              spellCheck={false}
              aria-invalid={invalid}
              data-focus-item
              onChange={(event) => {
                setDraft(event.currentTarget.value);
                setInvalid(false);
              }}
              onBlur={commitProxy}
              onKeyDown={(event) => {
                if (event.key === "Enter") commitProxy();
              }}
            />
            {invalid ? (
              <div className="hint hint-danger" style={{ marginTop: 6 }}>
                {t("models.netProxyInvalid")}
              </div>
            ) : null}
          </div>
        </div>
      ) : null}

      <div className="hint" style={{ margin: "0 0 4px" }}>
        {t("models.netRestart")}
      </div>

      {showTakeover ? (
        <div className="hint hint-warn" style={{ marginTop: 8 }}>
          {t("models.netTakeover")}
          <br />
          <span className="mono">DOMAIN-SUFFIX,aliyuncs.com,DIRECT</span>
        </div>
      ) : null}

      {report ? <NetworkReportView report={report} /> : null}
    </section>
  );
}

/* ---- 测速结果 ----------------------------------------------------- */

function NetworkReportView({ report }: { report: NetworkReport }) {
  const { t } = useLang();
  return (
    <div style={{ marginTop: 12 }}>
      <div className="sub-row">
        <span>{t("models.netVia")}</span>
        <span className="mono">{report.via ?? t("models.netViaDirect")}</span>
      </div>
      <div className="model-list">
        {report.results.map((result) => (
          <EndpointRow key={`${result.id}-${result.host}`} result={result} />
        ))}
      </div>
      {report.suggestion ? (
        <div className="hint hint-warn" style={{ marginTop: 10 }}>
          {t("models.netSuggest", { host: report.suggestion.host })}
        </div>
      ) : null}
      {report.results.some((result) => result.error) ? (
        <div className="hint" style={{ marginTop: 10 }}>
          {t("models.netFailHint")}
        </div>
      ) : null}
    </div>
  );
}

function EndpointRow({ result }: { result: EndpointResult }) {
  const { t } = useLang();
  const stages = result.error
    ? null
    : [
        t("models.netStageDns", { ms: result.dns_ms ?? 0 }),
        t("models.netStageConnect", { ms: result.connect_ms ?? 0 }),
        ...(result.tunnel_ms === null ? [] : [t("models.netStageTunnel", { ms: result.tunnel_ms })]),
        t("models.netStageTls", { ms: result.tls_ms ?? 0 }),
        t("models.netStageHttp", { ms: result.http_ms ?? 0 }),
      ];
  return (
    <div className="model-item">
      <div className="model-item-head" style={{ cursor: "default" }}>
        <span className="mi-main">
          <span className="mi-label">{result.host}</span>
          <span className="mono mi-id">
            {result.current ? t("models.netCurrent") : null}
            {result.current ? " · " : null}
            {result.error ? t(`models.netFail_${result.error.kind}`) : t("models.netOk")}
          </span>
        </span>
        <span className="mi-badges">
          {result.error ? (
            <span className="badge badge-danger">{result.error.message}</span>
          ) : (
            <>
              <span className="badge badge-neutral">{stages?.join(" · ")}</span>
              <span className="badge badge-running">{result.total_ms} ms</span>
            </>
          )}
        </span>
      </div>
    </div>
  );
}

/* ---- 可用模型 ----------------------------------------------------- */

function ModelsCard({ provider }: { provider: ModelProvider }) {
  const { snapshot } = useStore();
  const { t } = useLang();
  const { list, loading, refresh } = useModelList(provider);
  const configured = snapshot?.api_keys[provider] ?? false;

  return (
    <section className="card" aria-labelledby={`models-title-${provider}`}>
      <div className="card-head">
        <h2 className="card-title" id={`models-title-${provider}`}>
          {t("models.title")}
        </h2>
        <span className="source-tag">{sourceText(list, t)}</span>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          style={{ marginLeft: "auto" }}
          disabled={loading}
          data-focus-item
          onClick={refresh}
        >
          <IconRefresh size={14} />
          {loading ? t("models.refreshing") : t("models.refresh")}
        </button>
      </div>

      {list.error ? <div className="hint hint-danger model-error">{errorText(list, t)}</div> : null}
      {!configured && !list.error ? (
        <div className="hint model-error">{t("models.needKey")}</div>
      ) : null}

      <div className="model-list">
        {list.models.map((model) => (
          <ModelItem
              key={model.id}
              model={model}
              provider={provider}
              builtin={list.source === "builtin"}
            />
        ))}
      </div>
    </section>
  );
}

type Translate = ReturnType<typeof useLang>["t"];

function sourceText(list: ModelList, t: Translate): string {
  if (list.source === "official" && list.fetched_at !== null) {
    return t("models.sourceOfficial", { ago: fmtAgo(list.fetched_at / 1000, t) });
  }
  return t("models.sourceBuiltin");
}

function errorText(list: ModelList, t: Translate): string {
  const err = list.error;
  if (!err) return "";
  if (err.kind === "auth") return t("models.errAuth");
  if (err.kind === "network") return t("models.errNetwork", { host: err.host });
  return t("models.errOther", { message: err.message });
}

function ModelItem({
  model,
  provider,
  builtin,
}: {
  model: ModelEntry;
  provider: ModelProvider;
  builtin: boolean;
}) {
  const { t, uiLang } = useLang();
  // 内置条目的名字后端只有一种语言，按 id 查目录取界面语言；官方条目用官方 name。
  const label = builtin ? catalog.modelLabel(model.id, uiLang) : model.label;
  const [open, setOpen] = useState(false);
  const bodyId = `model-detail-${provider}-${model.id}`;
  const voiceCount = provider === "aliyun" ? catalog.voiceCatalog().length : 0;
  const summary = catalog.providerLanguageSummary(provider);

  return (
    <div className={model.adapted ? "model-item" : "model-item dim"}>
      <button
        type="button"
        className="model-item-head"
        aria-expanded={open}
        aria-controls={bodyId}
        data-focus-item
        onClick={() => setOpen(!open)}
      >
        <IconChevron size={14} className={open ? "more-chevron open" : "more-chevron"} />
        <span className="mi-main">
          <span className="mi-label">{label}</span>
          <span className="mono mi-id">{model.id}</span>
        </span>
        <span className="mi-badges">
          {model.recommended ? (
            <span className="badge badge-running">{t("models.recommended")}</span>
          ) : null}
          {model.adapted ? (
            <span className="badge badge-neutral">{t("models.adapted")}</span>
          ) : (
            <span className="badge badge-idle">{t("models.notAdapted")}</span>
          )}
          {model.snapshot_of ? (
            <span className="badge badge-neutral">{t("models.snapshot")}</span>
          ) : null}
          {model.offline_at ? (
            <span className="badge badge-warn">{t("models.offlineAt", { date: model.offline_at })}</span>
          ) : null}
        </span>
      </button>

      {open ? (
        <div className="mi-detail" id={bodyId}>
          <div className="sub-row">
            <span>{t("models.capLanguages")}</span>
            <span>
              {model.languages
                ? t("models.langCounts", {
                    audio: model.languages.audio.length,
                    text: model.languages.text_only.length,
                  })
                : (summary ?? "—")}
            </span>
          </div>
          {model.languages && model.languages.audio.length > 0 ? (
            <div className="sub-row">
              <span>{t("models.langAudio")}</span>
              <span>{catalog.languageNames(model.languages.audio, uiLang).join(t("models.listSep"))}</span>
            </div>
          ) : null}
          {model.languages && model.languages.text_only.length > 0 ? (
            <div className="sub-row">
              <span>{t("models.langTextOnly")}</span>
              <span>{catalog.languageNames(model.languages.text_only, uiLang).join(t("models.listSep"))}</span>
            </div>
          ) : null}
          <div className="sub-row">
            <span>{t("models.capVoices")}</span>
            <span>
              {model.capabilities.voice_selection
                ? voiceCount > 0
                  ? t("models.voiceCount", { n: voiceCount })
                  : t("models.yes")
                : t("models.autoVoice")}
            </span>
          </div>
          <div className="sub-row">
            <span>{t("models.capClone")}</span>
            <span>{model.capabilities.voice_clone ? t("models.yes") : t("models.no")}</span>
          </div>
          <div className="sub-row">
            <span>{t("models.capSource")}</span>
            <span>{model.capabilities.source_language ? t("models.yes") : t("models.no")}</span>
          </div>
          {model.snapshot_of ? (
            <div className="sub-row">
              <span>{t("models.capSnapshotOf")}</span>
              <span className="mono">{model.snapshot_of}</span>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
