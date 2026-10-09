/** 设置：通用（界面语言、主题、启动行为）、虚拟麦克风、激活模式与快捷键。虚拟麦克风管理区见 CableManager。 */

import { useState } from "react";

import { hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { toUiLang, useLang } from "../i18n/context";
import { UI_LANG_NAMES, UI_LANG_ORDER } from "../i18n/types";
import { useStore } from "../store";
import type { ActivationMode, Hotkey } from "../types";
import { Dropdown, SettingsItem, Toggle } from "../ui/controls";
import { HotkeyEditor } from "../ui/hotkey";
import { getThemeMode, setThemeMode, type ThemeMode } from "../ui/theme";
import { CableManager } from "./CableManager";

const DEFAULT_LISTEN_HOTKEY: Hotkey = { ctrl: true, alt: false, shift: false, key: "L" };

export function SettingsPage() {
  const { snapshot, settings, patch } = useStore();
  const { t, uiLang } = useLang();
  const [themeMode, setThemeModeState] = useState<ThemeMode>(getThemeMode);
  const speak = settings.speak;
  const listenHotkey = settings.listen.hotkey;
  const loading = snapshot === null;
  /** 全局热键这一位：位为假时整块热键设置不渲染（按不到的热键不许摆出来，R5）。 */
  const hotkeys = hostBit(snapshot, "global_hotkey");

  const activationOptions = [
    { value: "toggle", label: t("settings.activationToggle") },
    { value: "hold", label: t("settings.activationHold") },
  ];

  return (
    <>
      <div className="settings-group">
        <div className="group-title">{t("settings.general")}</div>
        <SettingsItem
          wide
          htmlFor="dd-ui-language"
          title={t("settings.uiLanguage")}
          control={
            <Dropdown
              id="dd-ui-language"
              label={t("settings.uiLanguageAria")}
              value={uiLang}
              options={UI_LANG_ORDER.map((lang) => ({ value: lang, label: UI_LANG_NAMES[lang] }))}
              onChange={(lang) => patch({ ui_language: toUiLang(lang) })}
            />
          }
        />
        <SettingsItem
          wide
          htmlFor="dd-theme"
          title={t("settings.theme")}
          control={
            <Dropdown
              id="dd-theme"
              label={t("settings.themeAria")}
              value={themeMode}
              options={[
                { value: "system", label: t("settings.themeSystem") },
                { value: "light", label: t("settings.themeLight") },
                { value: "dark", label: t("settings.themeDark") },
              ]}
              onChange={(mode) => {
                setThemeMode(mode as ThemeMode);
                setThemeModeState(mode as ThemeMode);
              }}
            />
          }
        />
        <SettingsItem
          title={t("settings.autostart")}
          control={
            <Toggle
              checked={settings.autostart}
              disabled={loading}
              label={t("settings.autostart")}
              onChange={(autostart) => patch({ autostart })}
            />
          }
        />
        <SettingsItem
          title={t("settings.startMinimized")}
          control={
            <Toggle
              checked={settings.start_minimized}
              disabled={loading}
              label={t("settings.startMinimized")}
              onChange={(startMinimized) => patch({ start_minimized: startMinimized })}
            />
          }
        />
      </div>

      <CableManager />

      <div className="settings-group">
        <SettingsItem
          wide
          htmlFor="dd-activation"
          title={t("settings.speakActivation")}
          control={
            <Dropdown
              id="dd-activation"
              label={t("settings.speakActivationAria")}
              value={speak.activation_mode}
              options={activationOptions}
              onChange={(activationMode) =>
                patch({ speak: { activation_mode: activationMode as ActivationMode } })
              }
            />
          }
        />

        {loading || hotkeys.enabled ? (
          <>
            <SettingsItem
              title={t("settings.speakHotkey")}
              control={
                <HotkeyEditor
                  id="dd-speak-key"
                  label={t("settings.speakHotkeyAria")}
                  hotkey={speak.hotkey}
                  onChange={(hotkey) => patch({ speak: { hotkey } })}
                />
              }
            />

            <SettingsItem
              title={t("settings.listenHotkey")}
              control={
                <Toggle
                  checked={listenHotkey !== null}
                  label={t("settings.enableListenHotkey")}
                  onChange={(enabled) =>
                    patch({ listen: { hotkey: enabled ? DEFAULT_LISTEN_HOTKEY : null } })
                  }
                />
              }
            />

            {listenHotkey ? (
              <SettingsItem
                title={t("settings.listenHotkeyCombo")}
                control={
                  <HotkeyEditor
                    id="dd-listen-key"
                    label={t("settings.listenHotkeyAria")}
                    hotkey={listenHotkey}
                    onChange={(hotkey) => patch({ listen: { hotkey } })}
                  />
                }
              />
            ) : null}
          </>
        ) : (
          /* 位为假：热键编辑器整个不渲染，只留这一句 reason（R1/R9）。 */
          <CapabilityNote bit="global_hotkey" />
        )}
      </div>
    </>
  );
}
