/**
 * 全局状态：一份快照 + 事件归约 + 设置补丁的乐观更新。
 *
 * 规则：
 * - 快照是唯一真相，事件只做增量修补；
 * - 改设置时先乐观改本地（输入框不卡手），再把补丁发给后端，
 *   后端返回的归一化结果覆盖回来（会夹取值域，所以本地可能被修正）；
 * - 字幕、电平、通知是易失状态，不进 settings。
 */

import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { getApi } from "./api";
import { isMsg, type Msg } from "./i18n/notice";
import type { VoxApi } from "./api";
import { DEFAULT_SETTINGS, cloneSettings } from "./defaults";
import { mergePatch } from "./mock/merge";
import type { PipelineName, PipelineState, Settings, Track } from "./types";
import type { GateStatus, Snapshot, SettingsPatch } from "./types.snapshot";
import { STATE_LABEL, isRunning } from "./pipeline";

export interface Live {
  /** 两条轨的当前字幕文本（已按 done 断句累积）。 */
  subtitles: Record<Track, string>;
  gates: Partial<Record<PipelineName, GateStatus>>;
  /** 服务端回报的「识别成 X 语言」（自动识别下才有），按轨记，显示成小字。 */
  sourceLanguage: Partial<Record<Track, string>>;
}

export interface Store {
  api: VoxApi;
  snapshot: Snapshot | null;
  settings: Settings;
  live: Live;
  /** 正在发的补丁数，>0 时头部显示「保存中」。 */
  saving: number;
  /** 通信 / 命令失败：后端消息（按语言渲染）或其它错误的字符串。 */
  error: Msg | string | null;
  patch(patch: SettingsPatch): void;
  dismissNotice(index: number): void;
  reload(): void;
  /** 后端命令回报的 16 声道端点状态，立刻落本地，不等 devices_changed 事件。 */
  applyCableChannelStatus(status: "visible" | "hidden"): void;
}

const EMPTY_LIVE: Live = {
  subtitles: { speak: "", listen: "" },
  gates: {},
  sourceLanguage: {},
};

const StoreContext = createContext<Store | null>(null);

export function StoreProvider({ children }: { children: ReactNode }) {
  const api = useMemo(() => getApi(), []);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [settings, setSettings] = useState<Settings>(() => cloneSettings(DEFAULT_SETTINGS));
  const [live, setLive] = useState<Live>(EMPTY_LIVE);
  const [saving, setSaving] = useState(0);
  const [error, setError] = useState<Msg | string | null>(null);
  const [reloadTick, setReloadTick] = useState(0);
  /** 本地最新设置的镜像，供补丁合并用，避免闭包拿到旧值。 */
  const settingsRef = useRef(settings);
  settingsRef.current = settings;

  useEffect(() => {
    let alive = true;
    api
      .snapshot()
      .then((s) => {
        if (!alive) return;
        setSnapshot(s);
        setSettings(s.settings);
        setError(null);
      })
      .catch((e: unknown) => alive && setError(isMsg(e) ? e : String(e)));
    return () => {
      alive = false;
    };
  }, [api, reloadTick]);

  useEffect(() => {
    return api.subscribe((event) => {
      switch (event.kind) {
        case "settings_changed":
          setSettings(event.settings);
          setSnapshot((s) => (s ? { ...s, settings: event.settings } : s));
          break;
        case "pipeline_state":
          setSnapshot((s) =>
            s
              ? {
                  ...s,
                  [event.pipeline]: {
                    ...s[event.pipeline],
                    state: event.state,
                    state_label: STATE_LABEL[event.state],
                    running: isRunning(event.state),
                  },
                  headphones_advised: advised(s, event),
                }
              : s,
          );
          break;
        case "gate_status":
          setLive((l) => ({ ...l, gates: { ...l.gates, [event.pipeline]: event.status } }));
          setSnapshot((s) =>
            s ? { ...s, [event.pipeline]: { ...s[event.pipeline], gate: event.status } } : s,
          );
          break;
        case "latency_changed":
          setSnapshot((s) =>
            s ? { ...s, [event.pipeline]: { ...s[event.pipeline], latency: event.latency } } : s,
          );
          break;
        case "subtitle_delta":
          setLive((l) => ({
            ...l,
            // 订正时后端发整句 + replace，必须整行替换；否则追加会叠字。
            subtitles: {
              ...l.subtitles,
              [event.track]: event.replace ? event.text : l.subtitles[event.track] + event.text,
            },
          }));
          break;
        case "subtitle_cleared":
          setLive((l) => ({
            ...l,
            subtitles: { ...l.subtitles, [event.track]: "" },
            sourceLanguage: { ...l.sourceLanguage, [event.track]: undefined },
          }));
          break;
        case "source_detected":
          setLive((l) => ({
            ...l,
            sourceLanguage: { ...l.sourceLanguage, [event.track]: event.language },
          }));
          break;
        case "usage_changed":
          setSnapshot((s) => (s ? { ...s, usage: event.usage } : s));
          break;
        case "mic_active":
          setSnapshot((s) => (s ? { ...s, mic_active: event.active } : s));
          break;
        case "devices_changed":
          // 设备与宿主事实是同一个轮询 tick 报的（`devices.rs`）：能力位跟着一起刷新。
          // 只取 `devices` 的话，虚拟麦节点掉了 / 托盘宿主装上这类**位变了但设备列表没变**
          // 的情况界面永远看不到，就会拿着旧位降级（S0 §2.6 R7）。
          void api.snapshot().then((s) =>
            setSnapshot((prev) =>
              prev ? { ...prev, devices: s.devices, capabilities: s.capabilities } : s,
            ),
          );
          break;
        case "notice":
          setSnapshot((s) => (s ? { ...s, notices: [...s.notices, event.notice].slice(-50) } : s));
          break;
      }
    });
  }, [api]);

  /**
   * 只刷控制面那一格（快照的其余部分不动，免得把别处刚到的增量盖回去）。
   *
   * 起/停是后端按新开关真做的事，本地推不出来：设置页据此说"在跑 / 没在跑、为什么"。
   */
  function refreshControl(): void {
    void api
      .snapshot()
      .then((s) => setSnapshot((prev) => (prev ? { ...prev, control: s.control } : s)));
  }

  const store: Store = {
    api,
    snapshot,
    settings,
    live,
    saving,
    error,
    patch(p) {
      setSettings((cur) => mergePatch(cloneSettings(cur), p));
      setSaving((n) => n + 1);
      api
        .updateSettings(p)
        .then((next) => {
          setSettings(next);
          setError(null);
          /*
           * 控制面那一格要重新拉：起/停是后端按新开关**真做**的事（`SettingsChanged` 一到
           * 就在 `update_settings` 里同步起/停完），本地推不出来。`settings_changed` 事件
           * 比这次 invoke 先到，那时服务还没起停完，拿不到结果——所以拉在 promise 之后。
           */
          if (p.control !== undefined) refreshControl();
        })
        .catch((e: unknown) => {
          setError(isMsg(e) ? e : String(e));
          setSettings(cloneSettings(settingsRef.current));
        })
        .finally(() => setSaving((n) => n - 1));
    },
    dismissNotice(index) {
      setSnapshot((s) => (s ? { ...s, notices: s.notices.filter((_, i) => i !== index) } : s));
    },
    reload() {
      setReloadTick((n) => n + 1);
    },
    applyCableChannelStatus(status) {
      setSnapshot((s) =>
        s
          ? {
              ...s,
              devices: { ...s.devices, virtual_cable_16ch_status: status },
            }
          : s,
      );
    },
  };

  return <StoreContext.Provider value={store}>{children}</StoreContext.Provider>;
}

function advised(s: Snapshot, event: { pipeline: PipelineName; state: PipelineState }): boolean {
  const running = (p: PipelineName) =>
    p === event.pipeline ? isRunning(event.state) : s[p].running;
  return running("speak") && running("listen");
}

export function useStore(): Store {
  const ctx = useContext(StoreContext);
  if (!ctx) throw new Error("useStore 必须在 StoreProvider 里用");
  return ctx;
}
