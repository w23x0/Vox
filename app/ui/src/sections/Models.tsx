/**
 * 模型页：左边三家服务商（状态点：已配置 / 未配置 / 出错），右边该服务商的
 * 密钥、地区 / 接入点（仅阿里云）、可用模型（现场拉官方列表，拉不到退回内置列表并标注）。
 */

import { useEffect, useRef, useState } from "react";

import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import { fmtAgo } from "../lib/format";
import { fetchModels, useModelList } from "../lib/models";
import { useStore } from "../store";
import type { AliyunRegion, ModelEntry, ModelList, ModelProvider } from "../types";
import { ConfirmButton } from "../ui/ConfirmButton";
import { Dropdown } from "../ui/controls";
import { IconChevron, IconExternal, IconRefresh, IconSave } from "../ui/icons";
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
