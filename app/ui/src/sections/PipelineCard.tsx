/**
 * 首页单卡：一张常驻功能（对外说话 / 听人说话）的全部配置与启停。
 *
 * 结构（上到下）：头部（图标、名称、状态、启停按钮）→ 服务商 · 模型（可更换）→
 * 语言对（源 → 目标）→ 设备 → 折叠的「更多」。服务商能力决定字段显隐：
 * 不支持的字段不渲染，而不是灰掉。
 *
 * `listen.target` 是整块替换语义；更换程序或切换子进程选项时，
 * executable / display_name / include_process_tree 必须一起提交。
 */

import { useCallback, useRef, useState } from "react";

import * as catalog from "../catalog";
import { capabilityNote, hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { useLang } from "../i18n/context";
import { modelDisplayName, useModelList } from "../lib/models";
import { recentFirst, useRecentValues } from "../lib/recent";
import { useStore } from "../store";
import type { ModelProvider } from "../types";
import { Dropdown, Toggle, type Option } from "../ui/controls";
import { useToast } from "../ui/toast";
import {
  IconChevron,
  IconHeadphones,
  IconMic,
  IconPlay,
  IconRefresh,
  IconStop,
} from "../ui/icons";
import { defaultVoiceForLanguage, voiceOptions } from "../voices";
import {
  SYSTEM_DEFAULT,
  buildAppOptions,
  deviceOptions,
  pickApp as pickListenApp,
  type HomeCard,
  type HomePipeline,
} from "./home_logic";
import { ModelPicker } from "./ModelPicker";

/** 两张卡的元数据（图标 / 语义色）。HomePage 用它 .map 渲染。 */
export const CARDS: HomeCard[] = [
  { id: "speak", tone: "blue", icon: IconMic },
  { id: "listen", tone: "green", icon: IconHeadphones },
];

/** 「更多」折叠状态记在本机，下次打开保持。 */
function useMoreOpen(pipeline: HomePipeline): [boolean, (open: boolean) => void] {
  const key = `vox.home.more.${pipeline}`;
  const [open, setOpenState] = useState<boolean>(() => {
    try {
      return localStorage.getItem(key) === "1";
    } catch {
      return false;
    }
  });
  const setOpen = useCallback(
    (next: boolean) => {
      setOpenState(next);
      try {
        localStorage.setItem(key, next ? "1" : "0");
      } catch {
        // 存不下就只在本次运行里记住
      }
    },
    [key],
  );
  return [open, setOpen];
}

export function PipelineCard({ pipeline }: { pipeline: HomePipeline }) {
  const { uiLang, t } = useLang();
  const pipeLabel = (id: HomePipeline) =>
    id === "speak" ? t("pipeline.speak") : t("pipeline.listen");
  const { api, snapshot, settings, patch } = useStore();
  const toast = useToast();
  const speak = settings.speak;
  const listen = settings.listen;
  const target = settings.listen.target;
  const inputs = snapshot?.devices.inputs ?? [];
  const apps = snapshot?.devices.apps ?? [];
  const missingInput =
    pipeline === "speak" &&
    !!speak.input_device &&
    !inputs.some((device) => device.name === speak.input_device);

  const [pickerOpen, setPickerOpen] = useState(false);
  const changeButton = useRef<HTMLButtonElement | null>(null);
  const [moreOpen, setMoreOpen] = useMoreOpen(pipeline);

  const [recentSpeakLanguages, rememberSpeakLanguage] = useRecentValues(
    "vox.recent.speak-languages",
    speak.target_language,
  );
  const [recentListenLanguages, rememberListenLanguage] = useRecentValues(
    "vox.recent.listen-languages",
    listen.source_language,
  );
  const [recentSpeakVoices, rememberSpeakVoice] = useRecentValues(
    "vox.recent.speak-voices",
    speak.voice,
  );
  const [recentListenVoices, rememberListenVoice] = useRecentValues(
    "vox.recent.listen-voices",
    listen.voice,
  );

  const activeProvider: ModelProvider = pipeline === "speak" ? speak.provider : listen.provider;
  const activeModel = pipeline === "speak" ? speak.model_name : listen.model_name;
  const { list: modelList } = useModelList(activeProvider);

  const languageChoices = catalog.targetLanguageOptions(activeProvider, activeModel, uiLang);
  const sourceChoices = catalog.sourceLanguageOptions(
    listen.provider,
    listen.model_name,
    uiLang,
    t("catalog.autoDetect"),
  );
  const speakLanguageOptions = recentFirst(languageChoices, recentSpeakLanguages);
  const listenLanguageOptions = [
    sourceChoices[0],
    ...recentFirst(sourceChoices.slice(1), recentListenLanguages),
  ].filter((option): option is catalog.LabeledOption => option !== undefined);
  const speakVoiceOptions = voiceOptions(
    uiLang,
    speak.voice,
    recentSpeakVoices,
    t("catalog.customVoiceSuffix"),
    t("catalog.defaultVoiceSuffix"),
  );
  const listenVoiceOptions = voiceOptions(
    uiLang,
    listen.voice,
    recentListenVoices,
    t("catalog.customVoiceSuffix"),
    t("catalog.defaultVoiceSuffix"),
  );

  const appOptions = buildAppOptions(apps, target, t);

  /**
   * 抓程序这一位为假时那句说明（位为真、或快照还没到 = `null`）。
   *
   * `program_tap` 为假 ⇒ 这条腿在这台设备上开不了：选择器**整个不渲染**（R1：不静默撤掉
   * 入口，要给一句说明），启动键也跟着禁用（否则点下去只会在装配时撞 `MissingInput`）。
   */
  const tapNote =
    pipeline === "listen" && snapshot ? capabilityNote(t, "program_tap", hostBit(snapshot, "program_tap")) : null;

  const card = CARDS.find((c) => c.id === pipeline);
  if (!card) return null;
  const Icon = card.icon;

  const state = snapshot?.[pipeline] ?? null;
  const running = state?.running ?? false;
  const failed = state?.state === "failed";

  /** 开不了的原因。null 表示能开。 */
  const blocked = ((): string | null => {
    if (!snapshot) return t("pipeline.stateLoading");
    // 抓程序这一位为假：说清"这台设备做不到"（R9），这句就是下面那一行说明。
    if (tapNote) return tapNote;
    // 对外说话关掉「翻译」= 直通原声，不走云端，也就不需要 API 密钥。
    const needsKey = pipeline === "listen" || speak.translate;
    if (needsKey && !snapshot.api_keys[activeProvider]) {
      return t("pipeline.blockedNoApiKey", {
        provider: catalog.providerLabel(activeProvider, uiLang),
      });
    }
    if (pipeline === "listen" && !target) return t("pipeline.blockedSelectApp");
    return null;
  })();

  const onPickApp = (exe: string) => pickListenApp(apps, target, exe, patch);

  /** 换模型后当前语言若不被新方言支持：回该方言缺省，并发一条通知说明换成了什么。 */
  const onPickModel = (provider: ModelProvider, model: string) => {
    setPickerOpen(false);
    const notify = (kind: "target" | "source", from: string | null, to: string | null) =>
      toast(
        "warning",
        t(kind === "target" ? "pipeline.targetFellBack" : "pipeline.sourceFellBack", {
          from: from ? catalog.languageName(from, uiLang) : "",
          to: to ? catalog.languageName(to, uiLang) : t("catalog.autoDetect"),
        }),
      );
    if (pipeline === "speak") {
      const target = catalog.resolveTarget(provider, model, speak.target_language, "speak");
      if (!target.changed) {
        patch({ speak: { provider, model_name: model } });
        return;
      }
      const nextVoice = speak.voice_by_language[target.code] ?? defaultVoiceForLanguage(target.code);
      patch({
        speak: {
          provider,
          model_name: model,
          target_language: target.code,
          voice: nextVoice,
          voice_by_language: { ...speak.voice_by_language, [target.code]: nextVoice },
        },
      });
      notify("target", speak.target_language, target.code);
    } else {
      const target = catalog.resolveTarget(provider, model, listen.target_language, "listen");
      const wanted = catalog.supportsSourceLanguage(provider) ? listen.source_language : null;
      const source = catalog.resolveSource(provider, model, wanted);
      patch({
        listen: {
          provider,
          model_name: model,
          target_language: target.code,
          source_language: source.code,
        },
      });
      if (target.changed) notify("target", listen.target_language, target.code);
      if (source.changed) notify("source", listen.source_language, source.code);
    }
  };

  const voiceSelectable = catalog.supportsVoiceSelection(activeProvider);
  const sourceSelectable = pipeline === "listen" && catalog.supportsSourceLanguage(listen.provider);
  /** 当前目标语言在这个模型的方言里是不是"仅文字"：是就没有译音可播。 */
  const currentTarget = pipeline === "speak" ? speak.target_language : listen.target_language;
  const targetTextOnly = catalog.targetMode(activeProvider, activeModel, currentTarget) === "text_only";
  /** 译音（合成语音）是否开着：关了（或目标语言仅文字）就没有音色 / 输出设备可言。 */
  const speaksTranslation =
    (pipeline === "speak" ? speak.translate : listen.speak_translation) && !targetTextOnly;
  const voiceId = pipeline === "speak" ? "dd-home-speak-voice" : "dd-home-listen-voice";

  /** 「更多」里实际渲染的字段名，折叠时当摘要显示。 */
  const moreLabels: string[] = [];
  if (pipeline === "listen") moreLabels.push(t("pipeline.playTranslation"));
  if (voiceSelectable && speaksTranslation) moreLabels.push(t("pipeline.voice"));
  if (pipeline === "speak" || speaksTranslation) moreLabels.push(t("pipeline.outputDevice"));
  if (pipeline === "speak" && speak.translate) {
    moreLabels.push(t("pipeline.showTranslation"), t("pipeline.monitorTranslation"));
  }

  const voiceDropdownOptions: Option[] = pipeline === "speak" ? speakVoiceOptions : listenVoiceOptions;

  return (
    <div className="stat-card pipeline-card">
      <div className="pipeline-head">
        <div className={`stat-icon ${card.tone}`}>
          <Icon size={24} />
        </div>
        <div className="pipeline-title">
          <div className="pipeline-name">{pipeLabel(pipeline)}</div>
          <span
            className={
              failed ? "badge badge-danger" : running ? "badge badge-running" : "badge badge-idle"
            }
          >
            <span className={running && !failed ? "status-dot running" : "status-dot"} />
            {state ? t(`pipeline.state.${state.state}`) : t("pipeline.stateLoading")}
          </span>
        </div>
        <button
          type="button"
          className={running ? "btn btn-secondary" : "btn btn-dark"}
          disabled={!snapshot || (!running && blocked !== null)}
          data-focus-item
          onClick={() => void api.togglePipeline(pipeline)}
        >
          {running ? <IconStop size={14} /> : <IconPlay size={14} />}
          {running ? t("pipeline.stop") : t("pipeline.start")}
        </button>
      </div>

      {!running && blocked && !tapNote ? <div className="hint hint-warn">{blocked}</div> : null}

      <div className="model-row">
        <div className="model-name">
          {catalog.providerLabel(activeProvider, uiLang)}
          <span aria-hidden="true">{" · "}</span>
          <span className="model-id">{modelDisplayName(modelList, activeModel, uiLang)}</span>
        </div>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          ref={changeButton}
          aria-expanded={pickerOpen}
          aria-label={t("pipeline.changeModelAria", { pipe: pipeLabel(pipeline) })}
          data-focus-item
          onClick={(e) => {
            // 不让这一下冒到 document 上，否则刚开就被弹层的「点外面收起」关掉
            e.stopPropagation();
            setPickerOpen((open) => !open);
          }}
        >
          {t("pipeline.changeModel")}
        </button>
        {pickerOpen ? (
          <ModelPicker
            provider={activeProvider}
            modelName={activeModel}
            onPick={onPickModel}
            onClose={(byKeyboard) => {
              setPickerOpen(false);
              if (byKeyboard) changeButton.current?.focus();
            }}
          />
        ) : null}
      </div>

      <div className="lang-pair">
        <div className="lang-cell">
          {pipeline === "speak" ? (
            <>
              <div className="stat-label" id="lbl-home-speak-source">
                {t("pipeline.sourceLabelSpeak")}
              </div>
              <div className="lang-fixed" id="lang-home-speak-source" aria-labelledby="lbl-home-speak-source">
                {t("catalog.autoDetect")}
              </div>
            </>
          ) : (
            <>
              <label className="stat-label" htmlFor="dd-home-listen-source">
                {t("pipeline.sourceLabelListen")}
              </label>
              {sourceSelectable ? (
                <Dropdown
                  id="dd-home-listen-source"
                  label={t("pipeline.sourceLanguageAria")}
                  value={listen.source_language ?? ""}
                  options={listenLanguageOptions}
                  onChange={(language) => {
                    rememberListenLanguage(language);
                    patch({ listen: { source_language: language === "" ? null : language } });
                  }}
                />
              ) : (
                <div
                  className="lang-fixed"
                  id="lang-home-listen-source"
                  aria-label={t("pipeline.sourceLanguageAria")}
                >
                  {t("catalog.autoDetect")}
                </div>
              )}
            </>
          )}
        </div>
        <span className="lang-arrow" aria-hidden="true">
          →
        </span>
        <div className="lang-cell">
          <label
            className="stat-label"
            htmlFor={pipeline === "speak" ? "dd-home-target-language" : "dd-home-listen-target-language"}
          >
            {t("pipeline.targetLabel")}
          </label>
          {pipeline === "speak" ? (
            <Dropdown
              id="dd-home-target-language"
              label={t("pipeline.targetLanguageAria")}
              value={speak.target_language}
              options={speakLanguageOptions}
              disabled={!speak.translate}
              onChange={(targetLanguage) => {
                rememberSpeakLanguage(targetLanguage);
                const nextVoice =
                  speak.voice_by_language[targetLanguage] ?? defaultVoiceForLanguage(targetLanguage);
                patch({
                  speak: {
                    target_language: targetLanguage,
                    voice: nextVoice,
                    voice_by_language: {
                      ...speak.voice_by_language,
                      [targetLanguage]: nextVoice,
                    },
                  },
                });
              }}
            />
          ) : (
            <Dropdown
              id="dd-home-listen-target-language"
              label={t("pipeline.listenTargetLanguageAria")}
              value={listen.target_language}
              options={languageChoices}
              onChange={(targetLanguage) => patch({ listen: { target_language: targetLanguage } })}
            />
          )}
        </div>
      </div>
      {pipeline === "speak" ? (
        <div className="hint">{t("pipeline.speakSourceHint")}</div>
      ) : !sourceSelectable ? (
        <div className="hint">{t("pipeline.sourceFixedHint")}</div>
      ) : null}
      {targetTextOnly ? (
        <div className="hint hint-warn">
          {t("home.textOnlyHint", { lang: catalog.languageName(currentTarget, uiLang) })}
        </div>
      ) : null}

      <div className="pipeline-device">
        {pipeline === "speak" ? (
          <>
            <label className="stat-label" htmlFor="dd-home-input-device">
              {t("pipeline.inputDevice")}
            </label>
            <div className="row" style={{ alignItems: "stretch" }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <Dropdown
                  id="dd-home-input-device"
                  label={t("pipeline.inputDeviceAria")}
                  value={speak.input_device ?? SYSTEM_DEFAULT}
                  options={deviceOptions(inputs, speak.input_device, t)}
                  disabled={!snapshot}
                  onChange={(inputDevice) =>
                    patch({
                      speak: { input_device: inputDevice === SYSTEM_DEFAULT ? null : inputDevice },
                    })
                  }
                />
              </div>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                aria-label={t("pipeline.rescanMic")}
                data-focus-item
                onClick={() => void api.refreshDevices()}
              >
                <IconRefresh size={15} />
              </button>
            </div>
            {missingInput || inputs.length === 0 ? (
              <div className="hint hint-warn">
                {missingInput ? t("pipeline.micNotConnected") : t("pipeline.micNotFound")}
              </div>
            ) : null}
            <div className="toggle-line">
              <span>{t("pipeline.translate")}</span>
              <Toggle
                checked={speak.translate}
                label={t("pipeline.translate")}
                onChange={(checked) => patch({ speak: { translate: checked } })}
              />
            </div>
            {!speak.translate ? <div className="hint">{t("pipeline.translateOffHint")}</div> : null}
          </>
        ) : tapNote ? (
          /* 位为假：选择器 / 子进程开关 / 重扫按钮整块不渲染，只留这一句说明（R1/R9）。 */
          <CapabilityNote bit="program_tap" />
        ) : (
          <>
            <label className="stat-label" htmlFor="dd-home-listen-target">
              {t("pipeline.listenTarget")}
            </label>
            <div className="row" style={{ alignItems: "stretch" }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <Dropdown
                  id="dd-home-listen-target"
                  label={t("pipeline.listenTargetAria")}
                  value={target?.executable ?? ""}
                  options={appOptions}
                  placeholder={t("pipeline.listenTargetPlaceholder")}
                  disabled={!snapshot || appOptions.length === 0}
                  onChange={onPickApp}
                />
              </div>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                aria-label={t("pipeline.rescanApps")}
                data-focus-item
                onClick={() => void api.refreshDevices()}
              >
                <IconRefresh size={15} />
              </button>
            </div>
            {appOptions.length === 0 || !target ? (
              <div className="hint hint-warn">
                {appOptions.length === 0 ? t("pipeline.appNoAudioFound") : t("pipeline.appSelectApp")}
              </div>
            ) : null}
            <div className="toggle-line">
              <span>{t("pipeline.includeSubprocess")}</span>
              <Toggle
                checked={target?.include_process_tree ?? true}
                disabled={!target}
                label={t("pipeline.includeSubprocess")}
                onChange={(include) => {
                  if (target) {
                    patch({ listen: { target: { ...target, include_process_tree: include } } });
                  }
                }}
              />
            </div>
          </>
        )}
      </div>

      <div className="more-section">
        <button
          type="button"
          className="more-toggle"
          aria-expanded={moreOpen}
          aria-controls={`more-${pipeline}`}
          data-focus-item
          onClick={() => setMoreOpen(!moreOpen)}
        >
          <IconChevron size={14} className={moreOpen ? "more-chevron open" : "more-chevron"} />
          <span className="more-title">{t("pipeline.more")}</span>
          {moreOpen ? null : <span className="more-summary">{moreLabels.join(" / ")}</span>}
        </button>
        {moreOpen ? (
          <div className="more-body" id={`more-${pipeline}`}>
            {pipeline === "listen" ? (
              <div className="toggle-line">
                <span>{t("pipeline.playTranslation")}</span>
                <Toggle
                  checked={speaksTranslation}
                  disabled={targetTextOnly}
                  label={t("pipeline.playTranslation")}
                  onChange={(checked) => patch({ listen: { speak_translation: checked } })}
                />
              </div>
            ) : null}
            {pipeline === "listen" && targetTextOnly ? (
              <div className="hint">
                {t("pipeline.playTranslationTextOnly", {
                  lang: catalog.languageName(listen.target_language, uiLang),
                })}
              </div>
            ) : null}

            {voiceSelectable && speaksTranslation ? (
              <div>
                <label className="stat-label" htmlFor={voiceId}>
                  {t("pipeline.voice")}
                </label>
                <Dropdown
                  id={voiceId}
                  label={t("pipeline.voiceAria", { pipe: pipeLabel(pipeline) })}
                  value={pipeline === "speak" ? speak.voice : listen.voice}
                  options={voiceDropdownOptions}
                  onChange={(voice) => {
                    if (pipeline === "speak") {
                      rememberSpeakVoice(voice);
                      patch({
                        speak: {
                          voice,
                          voice_by_language: { ...speak.voice_by_language, [speak.target_language]: voice },
                        },
                      });
                    } else {
                      rememberListenVoice(voice);
                      patch({ listen: { voice } });
                    }
                  }}
                />
              </div>
            ) : null}

            {pipeline === "speak" || speaksTranslation ? (
              <div>
                <label className="stat-label" htmlFor={`dd-home-${pipeline}-output`}>
                  {t("pipeline.outputDevice")}
                </label>
                <Dropdown
                  id={`dd-home-${pipeline}-output`}
                  label={t("pipeline.outputDeviceAria", { pipe: pipeLabel(pipeline) })}
                  value={
                    (pipeline === "speak" ? speak.output_device : listen.output_device) ?? SYSTEM_DEFAULT
                  }
                  options={deviceOptions(
                    snapshot?.devices.outputs ?? [],
                    pipeline === "speak" ? speak.output_device : listen.output_device,
                    t,
                  )}
                  disabled={!snapshot}
                  onChange={(outputDevice) => {
                    const device = outputDevice === SYSTEM_DEFAULT ? null : outputDevice;
                    if (pipeline === "speak") patch({ speak: { output_device: device } });
                    else patch({ listen: { output_device: device } });
                  }}
                />
              </div>
            ) : null}

            {pipeline === "speak" && speak.translate ? (
              <>
                <div className="toggle-line">
                  <span>{t("pipeline.showTranslation")}</span>
                  <Toggle
                    checked={speak.show_translation}
                    label={t("pipeline.showTranslation")}
                    onChange={(checked) => patch({ speak: { show_translation: checked } })}
                  />
                </div>
                <div className="toggle-line">
                  <span>{t("pipeline.monitorTranslation")}</span>
                  <Toggle
                    checked={speak.monitor_translation}
                    disabled={!snapshot}
                    label={t("pipeline.monitorTranslation")}
                    onChange={(enabled) => patch({ speak: { monitor_translation: enabled } })}
                  />
                </div>
              </>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}
