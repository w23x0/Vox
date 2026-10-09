/** 模型服务商：按服务商管理密钥与能力目录。 */

import { useEffect, useRef, useState } from "react";

import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import { useStore } from "../store";
import type { AliyunRegion, ModelProvider } from "../types";
import { Dropdown } from "../ui/controls";
import { IconExternal, IconSave, IconTrash } from "../ui/icons";
import { useToast } from "../ui/toast";
import { ConfirmButton } from "../ui/ConfirmButton";
import { orderedVoices } from "../voices";

export function ProvidersPage() {
  const { api, snapshot, reload, settings, patch } = useStore();
  const toast = useToast();
  const { uiLang, t } = useLang();
  const [provider, setProvider] = useState<ModelProvider>(
    catalog.providerIds()[0] ?? "aliyun",
  );
  const [hasDraft, setHasDraft] = useState(false);
  const [busy, setBusy] = useState<"save" | "clear" | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const busyRef = useRef(false);

  const loading = snapshot === null;
  const hasKey = snapshot?.api_keys[provider] ?? false;
  const supportsVoiceSelection = catalog.supportsVoiceSelection(provider);

  useEffect(() => {
    if (input.current) input.current.value = "";
    setHasDraft(false);
  }, []);

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
      reload();
      toast("success", t("providersPage.savedToast", { name: catalog.providerLabel(provider, uiLang) }));
    } catch (error: unknown) {
      toast(
        "danger",
        t("providersPage.saveFailed", {
          error: String(error).replaceAll(key, t("providersPage.keyHidden")),
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
      toast("success", t("providersPage.clearedToast", { name: catalog.providerLabel(provider, uiLang) }));
    } catch (error: unknown) {
      toast("danger", t("providersPage.clearFailed", { error: String(error) }));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  const voices = orderedVoices(uiLang, null, [], t("catalog.customVoiceSuffix"));
  const allLanguages = catalog.languageOptions(uiLang);
  const audioLanguages = allLanguages.filter((language) =>
    catalog.supportsAudioOutput(language.value, provider),
  );
  const textOnlyLanguages = allLanguages.filter(
    (language) => !catalog.supportsAudioOutput(language.value, provider),
  );

  return (
    <>
      <div className="card">
        <div className="row" style={{ justifyContent: "space-between", marginBottom: 14 }}>
          <div style={{ width: 260, maxWidth: "70%" }}>
            <Dropdown
              id="dd-provider-config"
              label={t("providersPage.selectProvider")}
              value={provider}
              options={catalog.providerOptions(uiLang)}
              disabled={busy !== null}
              onChange={(value) => setProvider(value as ModelProvider)}
            />
          </div>
          <span
            className={hasKey ? "badge badge-running" : "badge badge-idle"}
            style={{ cursor: "default" }}
          >
            <span className="status-dot" />
            {loading
              ? t("providersPage.loading")
              : hasKey
                ? t("providersPage.apiKeySet")
                : t("providersPage.apiKeyNotSet")}
          </span>
        </div>

        {provider === "aliyun" ? (
          <div className="row" style={{ justifyContent: "space-between", marginBottom: 14 }}>
            <label className="stat-label" htmlFor="dd-aliyun-region">
              {t("providersPage.region")}
            </label>
            <div style={{ width: 260, maxWidth: "70%" }}>
              <Dropdown
                id="dd-aliyun-region"
                label={t("providersPage.regionAria")}
                value={settings.aliyun_region}
                options={[
                  { value: "cn", label: t("providersPage.regionCn") },
                  { value: "intl", label: t("providersPage.regionIntl") },
                ]}
                onChange={(value) => patch({ aliyun_region: value as AliyunRegion })}
              />
            </div>
          </div>
        ) : null}

        <div className="row" style={{ alignItems: "stretch" }}>
          {!hasDraft ? (
            <span className="hint" style={{ alignSelf: "center" }}>
              {hasKey
                ? t("providersPage.overwriteHint")
                : t("providersPage.pasteHint")}
            </span>
          ) : null}
          <input
            id="f-api-key"
            ref={input}
            className="form-input mono"
            type="password"
            placeholder={
              hasKey
                ? t("providersPage.overwritePlaceholder")
                : catalog.providerApiKeyPlaceholder(provider)
            }
            autoComplete="off"
            spellCheck={false}
            disabled={busy !== null}
            aria-label={`${catalog.providerLabel(provider, uiLang)} ${t("providersPage.apiKey")}`}
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
            {busy === "save"
              ? t("providersPage.savingKey")
              : t("providersPage.saveKey")}
          </button>
        </div>

        <div className="row-wrap" style={{ marginTop: 12 }}>
          <button
            type="button"
            className="btn btn-secondary"
            data-focus-item
            onClick={() => void api.openProviderConsole(provider)}
          >
            <IconExternal size={15} />
            {t("providersPage.openConsole")}
          </button>
          {hasKey ? (
            <ConfirmButton
              confirmText={t("providersPage.confirmClear")}
              disabled={busy !== null}
              onConfirm={() => void clearKey()}
            >
              <IconTrash size={15} />
              {t("providersPage.clearKey")}
            </ConfirmButton>
          ) : null}
        </div>
      </div>

      <div className="panel">
        <div className="panel-top">
          <div className="panel-title">{t("providersPage.capabilities")}</div>
        </div>
        <div className="panel-body">
          <div className="sub-card" style={{ marginBottom: 14 }}>
            <div className="sub-card-head">{t("providersPage.realtimeModels")}</div>
            <div className="row-wrap">
              <span className="mono">
                {catalog.modelLabel(catalog.defaultModelForProvider(provider), uiLang)}
              </span>
              <span className="hint mono">
                {catalog.defaultModelForProvider(provider)}
              </span>
            </div>
          </div>

          {!supportsVoiceSelection ? (
            <div className="input-grid-3">
              <div className="sub-card">
                <div className="sub-card-head">{t("providersPage.language")}</div>
                <div className="si-title">{t("providersPage.autoLanguageCount")}</div>
                <div className="hint">{t("providersPage.realtimeInterp")}</div>
              </div>
              <div className="sub-card">
                <div className="sub-card-head">{t("providersPage.inputAudio")}</div>
                <div className="mono">PCM16LE · 16 kHz</div>
                <div className="hint">{t("providersPage.mono")}</div>
              </div>
              <div className="sub-card">
                <div className="sub-card-head">{t("providersPage.outputAudio")}</div>
                <div className="mono">PCM16LE · 24 kHz</div>
                <div className="hint">{t("providersPage.autoVoiceMono")}</div>
              </div>
            </div>
          ) : (
            <div className="input-grid-2">
              <div className="sub-card">
                <div className="sub-card-head">
                  {t("providersPage.voiceCount", { n: String(audioLanguages.length) })}
                </div>
                <div className="row-wrap">
                  {audioLanguages.slice(0, 8).map((language) => (
                    <span className="chip static" key={language.value}>{language.label}</span>
                  ))}
                </div>
                <details className="catalog-details">
                  <summary>{t("providersPage.viewAll")}</summary>
                  <div className="row-wrap">
                    {audioLanguages.map((language) => (
                      <span className="chip static" key={language.value}>{language.label}</span>
                    ))}
                  </div>
                </details>
              </div>

              <div className="sub-card">
                <div className="sub-card-head">
                  {t("providersPage.textOnlyCount", { n: String(textOnlyLanguages.length) })}
                </div>
                <div className="row-wrap">
                  {textOnlyLanguages.slice(0, 8).map((language) => (
                    <span className="chip static" key={language.value}>{language.label}</span>
                  ))}
                </div>
              </div>

              <div className="sub-card" style={{ gridColumn: "1 / -1" }}>
                <div className="sub-card-head">
                  {t("providersPage.officialVoices", { n: String(voices.length) })}
                </div>
                <div className="row-wrap">
                  {catalog.voiceCatalog().slice(0, 8).map((voice) => (
                    <span className="chip static" key={voice.id}>{catalog.l10n(voice.name, uiLang)}</span>
                  ))}
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
