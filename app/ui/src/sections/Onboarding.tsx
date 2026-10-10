/**
 * 首次启动引导：界面语言 → 服务商与密钥 → 语言对。三步，随时可跳过。
 *
 * 弹出条件：设置里 `onboarding_done` 为假、且还没有任何服务商配过密钥。
 * 已有密钥的老用户（旧设置文件没有这一格）直接补写成"已走过"，不打扰。
 * 走完或跳过都写 `onboarding_done = true`；设置页的"重新运行引导"只在本地再开一次。
 */

import { useEffect, useRef, useState } from "react";

import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import { UI_LANG_NAMES, UI_LANG_ORDER, type UiLang } from "../i18n/types";
import { storeModelList } from "../lib/models";
import { useStore } from "../store";
import type { AliyunRegion, EndpointResult, ModelProvider, NetworkReport } from "../types";
import { Dropdown } from "../ui/controls";
import { IconExternal, IconSave, IconSpeed } from "../ui/icons";
import { defaultVoiceForLanguage } from "../voices";

/** 设置页"重新运行引导"发这个事件。 */
export const ONBOARDING_OPEN_EVENT = "vox:onboarding-open";

const STEPS = 3;

/** 界面语言 → 翻译用的规范语言码。 */
const UI_TO_CODE: Record<UiLang, string> = { "zh-CN": "zh", "ja-JP": "ja", en: "en" };

type KeyCheck =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "ok"; count: number }
  | { state: "error"; text: string };

export function Onboarding() {
  const { snapshot, settings, patch } = useStore();
  const [active, setActive] = useState(false);
  const [step, setStep] = useState(0);
  const [provider, setProvider] = useState<ModelProvider>(settings.speak.provider);

  const loaded = snapshot !== null;
  const anyKey = snapshot ? Object.values(snapshot.api_keys).some(Boolean) : false;
  const done = settings.onboarding_done;

  useEffect(() => {
    if (!loaded || active || done) return;
    if (anyKey) patch({ onboarding_done: true });
    else setActive(true);
  }, [loaded, active, done, anyKey, patch]);

  useEffect(() => {
    const open = () => {
      setStep(0);
      setProvider(settings.speak.provider);
      setActive(true);
    };
    window.addEventListener(ONBOARDING_OPEN_EVENT, open);
    return () => window.removeEventListener(ONBOARDING_OPEN_EVENT, open);
  }, [settings.speak.provider]);

  if (!active) return null;

  const close = () => {
    setActive(false);
    if (!done) patch({ onboarding_done: true });
  };

  return (
    <OnboardingDialog
      step={step}
      setStep={setStep}
      provider={provider}
      setProvider={setProvider}
      onClose={close}
    />
  );
}

function OnboardingDialog({
  step,
  setStep,
  provider,
  setProvider,
  onClose,
}: {
  step: number;
  setStep: (step: number) => void;
  provider: ModelProvider;
  setProvider: (provider: ModelProvider) => void;
  onClose: () => void;
}) {
  const { t } = useLang();
  return (
    <div className="dialog-backdrop" role="presentation">
      <div
        className="dialog-card ob-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="ob-title"
      >
        <div className="ob-head">
          <div>
            <h2 id="ob-title">{t("onboarding.title")}</h2>
            <div className="hint">{t("onboarding.stepOf", { n: step + 1, total: STEPS })}</div>
          </div>
          <button type="button" className="btn btn-secondary btn-sm" data-focus-item onClick={onClose}>
            {t("onboarding.skip")}
          </button>
        </div>

        <div className="ob-body">
          {step === 0 ? <StepLanguage /> : null}
          {step === 1 ? <StepProvider provider={provider} setProvider={setProvider} /> : null}
          {step === 2 ? <StepPair provider={provider} onBack={() => setStep(1)} onDone={onClose} /> : null}
        </div>

        {step < 2 ? (
          <div className="dialog-actions">
            {step > 0 ? (
              <button
                type="button"
                className="btn btn-secondary"
                data-focus-item
                onClick={() => setStep(step - 1)}
              >
                {t("onboarding.back")}
              </button>
            ) : null}
            <button
              type="button"
              className="btn btn-primary"
              data-focus-item
              onClick={() => setStep(step + 1)}
            >
              {t("onboarding.next")}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}

/* ---- 第 1 步：界面语言 ------------------------------------------------ */

function StepLanguage() {
  const { settings, patch } = useStore();
  const { t, uiLang } = useLang();
  return (
    <>
      <h3 className="ob-step-title">{t("onboarding.langTitle")}</h3>
      <p className="ob-desc">{t("onboarding.langDesc")}</p>
      <div className="ob-choices" role="group" aria-label={t("settings.uiLanguageAria")}>
        {UI_LANG_ORDER.map((lang) => (
          <button
            key={lang}
            type="button"
            lang={lang === "ja-JP" ? "ja" : lang}
            className={lang === uiLang ? "provider-item selected" : "provider-item"}
            aria-pressed={lang === uiLang}
            data-focus-item
            onClick={() => {
              if (lang !== settings.ui_language) patch({ ui_language: lang });
            }}
          >
            <span className="provider-dot configured" aria-hidden="true" />
            <span className="provider-name">{UI_LANG_NAMES[lang]}</span>
          </button>
        ))}
      </div>
    </>
  );
}

/* ---- 第 2 步：服务商与密钥 -------------------------------------------- */

function StepProvider({
  provider,
  setProvider,
}: {
  provider: ModelProvider;
  setProvider: (provider: ModelProvider) => void;
}) {
  const { api, settings, patch, reload } = useStore();
  const { t, msg, uiLang } = useLang();
  const input = useRef<HTMLInputElement | null>(null);
  const [hasDraft, setHasDraft] = useState(false);
  const [check, setCheck] = useState<KeyCheck>({ state: "idle" });
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<NetworkReport | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const name = catalog.providerLabel(provider, uiLang);

  const choose = (next: ModelProvider) => {
    setProvider(next);
    setCheck({ state: "idle" });
    setReport(null);
    setTestError(null);
    setHasDraft(false);
    if (input.current) input.current.value = "";
  };

  const runTest = async () => {
    setTesting(true);
    setTestError(null);
    try {
      setReport(await api.testNetwork("aliyun"));
    } catch (error: unknown) {
      setReport(null);
      setTestError(t("models.netTestFailed", { error: msg(error) }));
    } finally {
      setTesting(false);
    }
  };

  const saveKey = async () => {
    const key = input.current?.value.trim() ?? "";
    if (!key || check.state === "checking") return;
    if (input.current) input.current.value = "";
    setHasDraft(false);
    setCheck({ state: "checking" });
    try {
      await api.setApiKey(provider, key);
    } catch (error: unknown) {
      setCheck({
        state: "error",
        text: t("models.saveFailed", { error: msg(error).replaceAll(key, t("models.keyHidden")) }),
      });
      return;
    }
    reload();
    try {
      const list = await api.listModels(provider, { refresh: true });
      storeModelList(list);
      const err = list.error;
      if (!err) {
        setCheck({ state: "ok", count: list.models.filter((m) => m.adapted).length });
      } else if (err.kind === "auth") {
        setCheck({ state: "error", text: t("models.errAuth") });
      } else if (err.kind === "network") {
        setCheck({ state: "error", text: t("models.errNetwork", { host: err.host }) });
      } else {
        setCheck({ state: "error", text: t("models.errOther", { message: err.message }) });
      }
    } catch (error: unknown) {
      setCheck({ state: "error", text: t("models.errOther", { message: msg(error) }) });
    }
  };

  const regionName = (result: EndpointResult) =>
    result.id === "cn"
      ? t("models.regionCn")
      : result.id === "intl"
        ? t("models.regionIntl")
        : result.host;

  return (
    <>
      <h3 className="ob-step-title">{t("onboarding.providerTitle")}</h3>
      <p className="ob-desc">{t("onboarding.providerDesc")}</p>

      <div className="ob-choices" role="group" aria-label={t("onboarding.providerAria")}>
        {catalog.providerIds().map((id) => (
          <button
            key={id}
            type="button"
            className={id === provider ? "provider-item selected" : "provider-item"}
            aria-pressed={id === provider}
            data-focus-item
            onClick={() => choose(id)}
          >
            <span className="provider-dot configured" aria-hidden="true" />
            <span className="provider-name">{catalog.providerLabel(id, uiLang)}</span>
          </button>
        ))}
      </div>

      {provider === "aliyun" ? (
        <div className="ob-block">
          <div className="row-wrap">
            <label className="stat-label" htmlFor="dd-ob-region">
              {t("onboarding.region")}
            </label>
            <div className="ob-region-dd">
              <Dropdown
                id="dd-ob-region"
                label={t("models.regionAria")}
                value={settings.aliyun_region}
                options={[
                  { value: "cn", label: t("models.regionCn") },
                  { value: "intl", label: t("models.regionIntl") },
                ]}
                onChange={(value) => {
                  setReport(null);
                  patch({ aliyun_region: value as AliyunRegion });
                }}
              />
            </div>
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={testing}
              data-focus-item
              onClick={() => void runTest()}
            >
              <IconSpeed size={14} />
              {testing ? t("onboarding.speedTesting") : t("onboarding.speedTest")}
            </button>
          </div>
          <div className="hint">{t("onboarding.speedHint")}</div>
          {testError ? <div className="hint hint-danger">{testError}</div> : null}
          {report ? (
            <>
              <div className="ob-results">
                {report.results.map((result) => (
                  <div className="sub-row" key={`${result.id}-${result.host}`}>
                    <span>{regionName(result)}</span>
                    <span className={result.error ? "hint-danger" : "mono"}>
                      {result.error
                        ? t("onboarding.unreachable")
                        : t("onboarding.latency", { ms: result.total_ms ?? 0 })}
                    </span>
                  </div>
                ))}
              </div>
              {report.suggestion ? (
                <div className="hint hint-warn">
                  {t("models.netSuggest", { host: report.suggestion.host })}
                </div>
              ) : null}
            </>
          ) : null}
        </div>
      ) : null}

      <div className="ob-block">
        <label className="stat-label" htmlFor="f-ob-key">
          {t("onboarding.keyLabel")}
        </label>
        <div className="row" style={{ alignItems: "stretch" }}>
          <input
            id="f-ob-key"
            ref={input}
            className="form-input mono"
            type="password"
            placeholder={catalog.providerApiKeyPlaceholder(provider)}
            autoComplete="off"
            spellCheck={false}
            disabled={check.state === "checking"}
            aria-label={t("models.keyInputAria", { name })}
            onChange={(event) => setHasDraft(event.currentTarget.value.trim().length > 0)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void saveKey();
            }}
          />
          <button
            type="button"
            className="btn btn-primary"
            disabled={!hasDraft || check.state === "checking"}
            data-focus-item
            onClick={() => void saveKey()}
          >
            <IconSave size={15} />
            {t("models.saveKey")}
          </button>
        </div>
        <div className="row-wrap">
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
        {check.state === "idle" ? <div className="hint">{t("onboarding.keyHint")}</div> : null}
        {check.state === "checking" ? <div className="hint">{t("onboarding.checking")}</div> : null}
        {check.state === "ok" ? (
          <div className="hint ob-ok" role="status">
            {t("onboarding.connected", { n: check.count })}
          </div>
        ) : null}
        {check.state === "error" ? (
          <div className="hint hint-danger" role="alert">
            {check.text}
          </div>
        ) : null}
      </div>
    </>
  );
}

/* ---- 第 3 步：语言对 -------------------------------------------------- */

function StepPair({
  provider,
  onBack,
  onDone,
}: {
  provider: ModelProvider;
  onBack: () => void;
  onDone: () => void;
}) {
  const { settings, patch } = useStore();
  const { t, uiLang } = useLang();
  const model = catalog.defaultModelForProvider(provider);

  const [speakTarget, setSpeakTarget] = useState(
    () => catalog.resolveTarget(provider, model, settings.speak.target_language, "speak").code,
  );
  const [listenTarget, setListenTarget] = useState(
    () => catalog.resolveTarget(provider, model, UI_TO_CODE[uiLang], "listen").code,
  );
  const options = catalog.targetLanguageOptions(provider, model, uiLang);

  const finish = () => {
    const voice = settings.speak.voice_by_language[speakTarget] ?? defaultVoiceForLanguage(speakTarget);
    patch({
      speak: {
        provider,
        model_name: model,
        target_language: speakTarget,
        voice,
        voice_by_language: { ...settings.speak.voice_by_language, [speakTarget]: voice },
      },
      listen: { provider, model_name: model, target_language: listenTarget },
    });
    onDone();
  };

  return (
    <>
      <h3 className="ob-step-title">{t("onboarding.pairTitle")}</h3>
      <p className="ob-desc">{t("onboarding.pairDesc")}</p>
      <div className="ob-block">
        <label className="stat-label" htmlFor="dd-ob-speak">
          {t("onboarding.speakTarget")}
        </label>
        <Dropdown
          id="dd-ob-speak"
          label={t("onboarding.speakTargetAria")}
          value={speakTarget}
          options={options}
          onChange={setSpeakTarget}
        />
      </div>
      <div className="ob-block">
        <label className="stat-label" htmlFor="dd-ob-listen">
          {t("onboarding.listenTarget")}
        </label>
        <Dropdown
          id="dd-ob-listen"
          label={t("onboarding.listenTargetAria")}
          value={listenTarget}
          options={options}
          onChange={setListenTarget}
        />
      </div>
      <OnboardingNoKey />
      <div className="dialog-actions">
        <button type="button" className="btn btn-secondary" data-focus-item onClick={onBack}>
          {t("onboarding.back")}
        </button>
        <button type="button" className="btn btn-primary" data-focus-item onClick={finish}>
          {t("onboarding.finish")}
        </button>
      </div>
    </>
  );
}

/** 到最后一步还没有密钥时，提醒一句去哪里补。 */
function OnboardingNoKey() {
  const { snapshot } = useStore();
  const { t } = useLang();
  const anyKey = snapshot ? Object.values(snapshot.api_keys).some(Boolean) : true;
  return anyKey ? null : <div className="hint">{t("onboarding.noKeyNote")}</div>;
}
