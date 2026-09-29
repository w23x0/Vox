import { useState } from "react";

import { hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { useT } from "../i18n/context";
import { useStore } from "../store";
import { SettingsItem, Toggle } from "../ui/controls";
import { IconPlay, IconSend, IconStop } from "../ui/icons";
import { useToast } from "../ui/toast";

const DEFAULT_PARAM = "VoxSpeaking";
const DEFAULT_PORT = 9000;

export function VrchatPage() {
  const { api, settings, patch, snapshot } = useStore();
  const toast = useToast();
  const t = useT();
  /** 头显字幕这一位：位为假时不渲染 SteamVR 开关（R5）。 */
  const vrCaptions = hostBit(snapshot, "vr_captions");

  // 总开关 + 收发忙态
  const [running, setRunning] = useState(false);
  const [busy, setBusy] = useState(false);
  // 各项配置（页内本地态，不落 snapshot schema）
  const [chatboxEnabled, setChatboxEnabled] = useState(false);
  const [avatarEnabled, setAvatarEnabled] = useState(false);
  const [param, setParam] = useState(DEFAULT_PARAM);
  const [port, setPort] = useState(DEFAULT_PORT);
  // 测试发送的忙态
  const [testing, setTesting] = useState(false);

  async function toggle(): Promise<void> {
    if (busy) return;
    setBusy(true);
    try {
      if (running) {
        await api.oscStop();
        setRunning(false);
        toast("success", t("vrchat.stoppedToast"));
      } else {
        await api.oscStart(chatboxEnabled, avatarEnabled, param, port);
        setRunning(true);
        toast("success", t("vrchat.started"));
      }
    } catch (e: unknown) {
      toast("danger", running ? t("vrchat.stopFailed", { error: String(e) }) : t("vrchat.startFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  }

  /** 运行中热更新配置（端口建时定死，不在此下发）。 */
  async function hotUpdate(
    nextChatbox: boolean,
    nextAvatar: boolean,
    nextParam: string,
  ): Promise<void> {
    if (!running) return;
    try {
      await api.oscUpdate(nextChatbox, nextAvatar, nextParam);
      toast("success", t("vrchat.updated"));
    } catch (e: unknown) {
      toast("danger", t("vrchat.updateFailed", { error: String(e) }));
    }
  }

  function onChatbox(next: boolean): void {
    setChatboxEnabled(next);
    void hotUpdate(next, avatarEnabled, param);
  }

  function onChangePort(next: number): void {
    setPort(next);
  }

  function onAvatar(next: boolean): void {
    setAvatarEnabled(next);
    void hotUpdate(chatboxEnabled, next, param);
  }

  function onParam(next: string): void {
    setParam(next);
    void hotUpdate(chatboxEnabled, avatarEnabled, next);
  }

  async function testSend(): Promise<void> {
    if (testing) return;
    setTesting(true);
    try {
      await api.oscSendChatbox("Vox test message");
      toast("success", t("vrchat.sent"));
    } catch (e: unknown) {
      toast("danger", t("vrchat.sendFailed", { error: String(e) }));
    } finally {
      setTesting(false);
    }
  }

  return (
    <>
      <div className="panel">
        <div className="panel-top">
          <div className="panel-title">{t("vrchat.sync")}</div>
          <span className={running ? "badge badge-running" : "badge badge-idle"}>
            <span className={running ? "status-dot running" : "status-dot"} />
            {running ? t("vrchat.running") : t("vrchat.stopped")}
          </span>
        </div>
        <div className="panel-body">
          <SettingsItem
            title={t("vrchat.sync")}
            desc={t("vrchat.syncDesc")}
            control={
              <button
                type="button"
                className={running ? "btn btn-secondary btn-sm" : "btn btn-primary btn-sm"}
                data-focus-item
                disabled={busy}
                onClick={() => void toggle()}
              >
                {running ? <IconStop size={15} /> : <IconPlay size={15} />}
                {running ? t("vrchat.stop") : t("vrchat.start")}
              </button>
            }
          />
          {vrCaptions.enabled ? (
            <SettingsItem
              title={t("vrchat.steamVrOverlay")}
              desc={t("vrchat.steamVrOverlayDesc")}
              control={
                <Toggle
                  checked={settings.subtitle.vr_overlay_enabled}
                  label={t("vrchat.steamVrOverlay")}
                  onChange={(enabled) => patch({ subtitle: { vr_overlay_enabled: enabled } })}
                />
              }
            />
          ) : (
            /* 位为假（构建里没编进去 / 这一档没有头显字幕）：开关整个不渲染，只留 reason。
               从前这个开关是无条件渲染的——没有该 feature 的构建上，用户会点到一个
               不存在的功能（S0 §2.6 R5 的现状缺口）。 */
            <CapabilityNote bit="vr_captions" />
          )}
        </div>
      </div>

      <div className="settings-group">
        <SettingsItem
          title={t("vrchat.chatbox")}
          desc={t("vrchat.chatboxDesc")}
          control={
            <Toggle
              checked={chatboxEnabled}
              disabled={busy}
              label={t("vrchat.chatbox")}
              onChange={onChatbox}
            />
          }
        />
        <SettingsItem
          title={t("vrchat.testSend")}
          control={
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              data-focus-item
              disabled={testing}
              onClick={() => void testSend()}
            >
              <IconSend size={15} />
              {testing ? t("vrchat.testing") : t("vrchat.testSend")}
            </button>
          }
        />
      </div>

      <div className="settings-group">
        <SettingsItem
          title={t("vrchat.avatarEnable")}
          desc={t("vrchat.avatarDesc")}
          control={
            <Toggle
              checked={avatarEnabled}
              disabled={busy}
              label={t("vrchat.avatarEnable")}
              onChange={onAvatar}
            />
          }
        />
        <SettingsItem
          wide
          title={t("vrchat.param")}
          desc={t("vrchat.paramDesc")}
          control={
            <input
              className="form-input mono"
              type="text"
              value={param}
              aria-label={t("vrchat.param")}
              spellCheck={false}
              onChange={(e) => onParam(e.currentTarget.value)}
            />
          }
        />
        <SettingsItem
          wide
          title={t("vrchat.port")}
          desc={t("vrchat.portDesc")}
          control={
            <input
              className="form-input mono"
              type="number"
              min={1}
              max={65535}
              value={port}
              aria-label={t("vrchat.port")}
              onChange={(e) => onChangePort(Number(e.currentTarget.value))}
            />
          }
        />
      </div>
    </>
  );
}
