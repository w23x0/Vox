/**
 * 首页单卡：一张常驻功能（对外说话 / 听人说话）的全部配置与启停。
 *
 * 从 Home.tsx 的 CARDS.map 回调体抽出来——那张回调体里 speak/listen 的差异
 * 原来全靠 `card.id === "speak"` 三元贯穿整个 JSX，现在组件内部按 pipeline 分支，
 * 每条路径都保持和原实现一致的 a11y 属性、data-focus-item 与 patch 语义。
 *
 * `listen.target` 是整块替换语义；更换程序或切换子进程选项时，
 * executable / display_name / include_process_tree 必须一起提交。
 */

import * as catalog from "../catalog";
import { capabilityNote, hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { useLang } from "../i18n/context";
import { recentFirst, useRecentValues } from "../lib/recent";
import { useStore } from "../store";
import type { ModelProvider } from "../types";
import { Dropdown, Toggle, type Option } from "../ui/controls";
import { IconHeadphones, IconMic, IconPlay, IconRefresh, IconStop } from "../ui/icons";
import { defaultVoiceForLanguage, voiceOptions } from "../voices";
import {
  SYSTEM_DEFAULT,
  buildAppOptions,
  deviceOptions,
  pickApp as pickListenApp,
  type HomeCard,
  type HomePipeline,
} from "./home_logic";

/** 两张卡的元数据（图标 / 语义色）。HomePage 用它 .map 渲染。 */
export const CARDS: HomeCard[] = [
  { id: "speak", tone: "blue", icon: IconMic },
  { id: "listen", tone: "green", icon: IconHeadphones },
];

export function PipelineCard({ pipeline }: { pipeline: HomePipeline }) {
  const { uiLang, t } = useLang();
  const pipeLabel = (id: HomePipeline) =>
    id === "speak" ? t("pipeline.speak") : t("pipeline.listen");
  const { api, snapshot, settings, patch } = useStore();
  const speak = settings.speak;
  const listen = settings.listen;
  const target = settings.listen.target;
  const inputs = snapshot?.devices.inputs ?? [];
  const apps = snapshot?.devices.apps ?? [];
  const missingInput =
    pipeline === "speak" &&
    !!speak.input_device &&
    !inputs.some((device) => device.name === speak.input_device);

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

  const languageChoices = catalog.languageOptions(uiLang);
  const sourceChoices = catalog.sourceLanguageOptions(uiLang, t("catalog.autoDetect"));
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
  const activeProvider = pipeline === "speak" ? speak.provider : listen.provider;

  /** 开不了的原因。null 表示能开。 */
  const blocked = ((): string | null => {
    if (!snapshot) return t("pipeline.openFailReason");
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

  const voiceDropdownOptions: Option[] = !catalog.supportsVoiceSelection(activeProvider)
    ? [
        {
          value: pipeline === "speak" ? speak.voice : listen.voice,
          label: t("catalog.autoVoice"),
        },
      ]
    : pipeline === "speak"
      ? speakVoiceOptions
      : listenVoiceOptions;

  return (
    <div
      className="stat-card pipeline-card"
      style={{ flexDirection: "column", alignItems: "stretch" }}
    >
      <div className="row pipeline-status" style={{ gap: 16 }}>
        <div className={`stat-icon ${card.tone}`}>
          <Icon size={26} />
        </div>
        <div style={{ minWidth: 0, flex: 1 }}>
          <div className="stat-label">{pipeLabel(pipeline)}</div>
          <div className="row" style={{ gap: 6, marginTop: 4 }}>
            <span
              className={
                failed
                  ? "badge badge-danger"
                  : running
                    ? "badge badge-running"
                    : "badge badge-idle"
              }
            >
              <span className={running && !failed ? "status-dot running" : "status-dot"} />
              {state ? t(`pipeline.state.${state.state}`) : "读取中"}
            </span>
          </div>
        </div>
      </div>

      {!running && blocked && !tapNote ? (
        <div className="hint hint-warn" style={{ marginTop: 10 }}>
          {blocked}
        </div>
      ) : null}

      <div className="pipeline-config pipeline-primary">
        <div className="pipeline-config-grid">
          <div style={{ minWidth: 0 }}>
            <label
              className="stat-label"
              htmlFor={`dd-home-${pipeline}-provider`}
            >
              {t("pipeline.provider")}
            </label>
            <Dropdown
              id={`dd-home-${pipeline}-provider`}
              label={t("pipeline.providerAria", { pipe: pipeLabel(pipeline) })}
              value={activeProvider}
              options={catalog.providerOptions(uiLang)}
              onChange={(provider) => {
                const nextProvider = provider as ModelProvider;
                if (pipeline === "speak") {
                  patch({
                    speak: {
                      provider: nextProvider,
                      model_name: catalog.defaultModelForProvider(nextProvider),
                    },
                  });
                } else {
                  patch({
                    listen: {
                      provider: nextProvider,
                      model_name: catalog.defaultModelForProvider(nextProvider),
                      source_language: catalog.supportsSourceLanguage(nextProvider)
                        ? listen.source_language
                        : null,
                    },
                  });
                }
              }}
            />
          </div>

          {pipeline === "speak" ? (
            <div style={{ minWidth: 0 }}>
              <div className="pipeline-inline-toggle"
              style={{ marginBottom: 8, width: "100%", justifyContent: "space-between" }}>
                <span>{t("pipeline.translate")}</span>
                <Toggle
                  checked={speak.translate}
                  label={t("pipeline.translate")}
                  onChange={(checked) => patch({ speak: { translate: checked } })}
                />
              </div>
              {!speak.translate ? (
                <div className="hint" style={{ marginBottom: 8 }}>
                  {t("pipeline.translateOffHint")}
                </div>
              ) : null}
              <label className="stat-label" htmlFor="dd-home-target-language">
                {t("pipeline.targetLanguage")}
              </label>
              <Dropdown
                id="dd-home-target-language"
                label={t("pipeline.targetLanguageAria")}
                value={speak.target_language}
                options={speakLanguageOptions}
                disabled={!speak.translate}
                onChange={(targetLanguage) => {
                  rememberSpeakLanguage(targetLanguage);
                  const nextVoice =
                    speak.voice_by_language[targetLanguage] ??
                    defaultVoiceForLanguage(targetLanguage);
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
              <div className="pipeline-inline-toggle"
              style={{ marginTop: 8, width: "100%", justifyContent: "space-between" }}>
                <span>{t("pipeline.showTranslation")}</span>
                <Toggle
                  checked={speak.show_translation}
                  disabled={!speak.translate}
                  label={t("pipeline.showTranslation")}
                  onChange={(checked) => patch({ speak: { show_translation: checked } })}
                />
              </div>
            </div>
          ) : (
            <div style={{ minWidth: 0 }}>
              <label className="stat-label" htmlFor="dd-home-listen-source">
                {t("pipeline.sourceLanguage")}
              </label>
              <Dropdown
                id="dd-home-listen-source"
                label={t("pipeline.sourceLanguageAria")}
                value={listen.source_language ?? ""}
                options={
                  !catalog.supportsSourceLanguage(listen.provider)
                    ? [sourceChoices[0]].filter(
                        (option): option is catalog.LabeledOption => option !== undefined,
                      )
                    : listenLanguageOptions
                }
                disabled={!catalog.supportsSourceLanguage(listen.provider)}
                onChange={(language) => {
                  rememberListenLanguage(language);
                  patch({
                    listen: { source_language: language === "" ? null : language },
                  });
                }}
              />
              <div className="pipeline-inline-toggle"
              style={{ marginTop: 8, width: "100%", justifyContent: "space-between" }}>
                <span>{t("pipeline.playTranslation")}</span>
                <Toggle
                  checked={listen.speak_translation}
                  label={t("pipeline.playTranslation")}
                  onChange={(checked) => patch({ listen: { speak_translation: checked } })}
                />
              </div>
            </div>
          )}

          <div className="pipeline-config-wide">
            <label
              className="stat-label"
              htmlFor={pipeline === "speak" ? "dd-home-speak-voice" : "dd-home-listen-voice"}
            >
              {t("pipeline.voice")}
            </label>
            <Dropdown
              id={pipeline === "speak" ? "dd-home-speak-voice" : "dd-home-listen-voice"}
              label={t("pipeline.voiceAria", { pipe: pipeLabel(pipeline) })}
              value={pipeline === "speak" ? speak.voice : listen.voice}
              options={voiceDropdownOptions}
              disabled={
                !catalog.supportsVoiceSelection(activeProvider) ||
                (pipeline === "listen" && !listen.speak_translation) ||
                (pipeline === "speak" && !speak.translate)
              }
              onChange={(voice) => {
                if (pipeline === "speak") {
                  rememberSpeakVoice(voice);
                  patch({
                    speak: {
                      voice,
                      voice_by_language: {
                        ...speak.voice_by_language,
                        [speak.target_language]: voice,
                      },
                    },
                  });
                } else {
                  rememberListenVoice(voice);
                  patch({ listen: { voice } });
                }
              }}
            />

            <div className="pipeline-output-device">
              <label className="stat-label" htmlFor={`dd-home-${pipeline}-output`}>
                {t("pipeline.outputDevice")}
              </label>
              <Dropdown
                id={`dd-home-${pipeline}-output`}
                label={t("pipeline.outputDeviceAria", { pipe: pipeLabel(pipeline) })}
                value={
                  (pipeline === "speak" ? speak.output_device : listen.output_device) ??
                  SYSTEM_DEFAULT
                }
                options={deviceOptions(
                  snapshot?.devices.outputs ?? [],
                  pipeline === "speak" ? speak.output_device : listen.output_device,
                  t,
                )}
                disabled={!snapshot}
                onChange={(outputDevice) =>
                  pipeline === "speak"
                    ? patch({
                        speak: {
                          output_device:
                            outputDevice === SYSTEM_DEFAULT ? null : outputDevice,
                        },
                      })
                    : patch({
                        listen: {
                          output_device:
                            outputDevice === SYSTEM_DEFAULT ? null : outputDevice,
                        },
                      })
                }
              />
            </div>
          </div>
        </div>

        {pipeline === "speak" && !catalog.supportsAudioOutput(speak.target_language, speak.provider) ? (
          <div className="hint hint-warn" style={{ marginTop: 8 }}>
            {t("home.textOnlyHint", { lang: catalog.languageLabel(speak.target_language, uiLang) })}
          </div>
        ) : null}
      </div>

      {pipeline === "speak" ? (
        <div className="pipeline-advanced" style={{ marginTop: 10 }}>
          <label className="stat-label" htmlFor="dd-home-input-device">
            {t("pipeline.inputDevice")}
          </label>
          <div className="row" style={{ alignItems: "stretch", marginTop: 6 }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <Dropdown
                id="dd-home-input-device"
                label={t("pipeline.inputDeviceAria")}
                value={speak.input_device ?? SYSTEM_DEFAULT}
                options={deviceOptions(inputs, speak.input_device, t)}
                disabled={!snapshot}
                onChange={(inputDevice) =>
                  patch({
                    speak: {
                      input_device:
                        inputDevice === SYSTEM_DEFAULT ? null : inputDevice,
                    },
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
            <div className="hint hint-warn" style={{ marginTop: 6 }}>
              {missingInput ? t("pipeline.micNotConnected") : t("pipeline.micNotFound")}
            </div>
          ) : null}

          <div
            className="row"
            style={{ justifyContent: "space-between", marginTop: 12 }}
          >
            <div className="stat-label">{t("pipeline.monitorTranslation")}</div>
            <Toggle
              checked={speak.monitor_translation}
              disabled={!snapshot || !speak.translate}
              label={t("pipeline.monitorTranslation")}
              onChange={(enabled) =>
                patch({ speak: { monitor_translation: enabled } })
              }
            />
          </div>
        </div>
      ) : null}

      {pipeline === "listen" ? (
        <div className="pipeline-advanced" style={{ marginTop: 10 }}>
          {tapNote ? (
            /* 位为假：选择器 / 子进程开关 / 重扫按钮整块不渲染，只留这一句说明（R1/R9）。 */
            <CapabilityNote bit="program_tap" />
          ) : (
            <>
              <label className="stat-label" htmlFor="dd-home-listen-target">
                {t("pipeline.listenTarget")}
              </label>
              <div className="row" style={{ alignItems: "stretch", marginTop: 6 }}>
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
                <div className="hint hint-warn" style={{ marginTop: 6 }}>
                  {appOptions.length === 0 ? t("pipeline.appNoAudioFound") : t("pipeline.appSelectApp")}
                </div>
              ) : null}
              <div className="row" style={{ justifyContent: "space-between", marginTop: 10 }}>
                <span className="stat-label">{t("pipeline.includeSubprocess")}</span>
                <Toggle
                  checked={target?.include_process_tree ?? true}
                  disabled={!target}
                  label={t("pipeline.includeSubprocess")}
                  onChange={(include) => {
                    if (target) {
                      patch({
                        listen: { target: { ...target, include_process_tree: include } },
                      });
                    }
                  }}
                />
              </div>
            </>
          )}
        </div>
      ) : null}

      <button
        type="button"
        className={running ? "btn btn-secondary btn-sm pipeline-action" : "btn btn-dark btn-sm pipeline-action"}
        style={{ marginTop: "auto", justifyContent: "center" }}
        disabled={!snapshot || (!running && blocked !== null)}
        data-focus-item
        onClick={() => void api.togglePipeline(pipeline)}
      >
        {running ? <IconStop size={14} /> : <IconPlay size={14} />}
        {running ? t("pipeline.stop") : t("pipeline.start")}
      </button>
    </div>
  );
}
