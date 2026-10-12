/**
 * 假后端。没有 Rust 侧的时候整套界面照样能点通：
 * 电平条会动、字幕会流进来、token 会涨、通知会冒出来。
 *
 * 行为刻意抄 runtime.rs 的规则：没密钥不许启动、听人说话必须先选程序、
 * 运行中改了只有重启才生效的格就替用户停掉重开、切激活方式会清掉 mic_active。
 */

import { DEFAULT_SETTINGS, cloneSettings } from "../defaults";
import type { ModelProvider, PipelineName, PipelineState, Settings, Track } from "../types";
import * as catalog from "../catalog";
import { HOST_CAPABILITIES, UNAVAILABLE_REASONS } from "../capabilities";
import type {
  AudioApp,
  Capabilities,
  ControlStatus,
  GateStatus,
  HostCapability,
  LatencyMetric,
  LatencySnapshot,
  Notice,
  PipelineSnapshot,
  Snapshot,
  SettingsPatch,
  UnavailableReason,
  UsageLedger,
  VoxEvent,
} from "../types.snapshot";
import type { VoxApi } from "../api";
import type { Msg } from "../i18n/notice";
import { STATE_LABEL, isRunning } from "../pipeline";
import {
  LISTEN_SCRIPT,
  MOCK_APPS,
  MOCK_INPUTS,
  MOCK_OUTPUTS,
  SPEAK_SCRIPT,
  mockCapabilities,
  mockUsage,
  type MockHost,
} from "./data";
import { mergePatch, normalizeSettings } from "./merge";
import { mockModelList, parseMockErrors } from "./models";
import { mockNetworkFlags, mockNetworkReport, mockTakeoverReport } from "./network";
import { FakeGate, FakeVoice } from "./signal";
import { FakeTyper } from "./typer";

const TICK_MS = 60;

interface Lane {
  state: PipelineState;
  gate: GateStatus | null;
  voice: FakeVoice;
  gateImpl: FakeGate;
  typer: FakeTyper | null;
}

/**
 * 改了设置之后这条腿要不要**停掉重开**（真芯：`Runtime::restart_required`——把新旧设置
 * 各折成 `SessionConfig`，能热更新的那几格换成旧值再比）。这里按同一份"启动格"清单比：
 * 能热更新的是语言（只限认热更新的方言） / 音色 / 播放设备 / 回听 / 阀门，其余都算启动格。
 */
function needsRestart(before: Settings, after: Settings, pipeline: PipelineName): boolean {
  const a = before[pipeline];
  const b = after[pipeline];
  if (a.provider !== b.provider || a.model_name !== b.model_name) return true;
  if (pipeline === "speak") {
    if (
      before.speak.input_device !== after.speak.input_device ||
      before.speak.translate !== after.speak.translate ||
      before.speak.denoise !== after.speak.denoise ||
      before.speak.aec !== after.speak.aec ||
      before.speak.voice_clone_frequency !== after.speak.voice_clone_frequency ||
      (!catalog.supportsHotUpdateLanguage(after.speak.provider) &&
        before.speak.target_language !== after.speak.target_language)
    ) {
      return true;
    }
  } else if (
    before.listen.target_language !== after.listen.target_language ||
    before.listen.source_language !== after.listen.source_language ||
    JSON.stringify(before.listen.target) !== JSON.stringify(after.listen.target)
  ) {
    return true;
  }
  // 接入点 / 地区 / 网络出口只在建连时读（NET D8）：改了也得重开。
  return (
    (before.aliyun_endpoint ?? "") !== (after.aliyun_endpoint ?? "") ||
    before.aliyun_region !== after.aliyun_region ||
    JSON.stringify(before.network) !== JSON.stringify(after.network)
  );
}

export function createMockApi(): VoxApi {
  let settings = normalizeSettings(cloneSettings(DEFAULT_SETTINGS));
  const apiKeys = Object.fromEntries(
    catalog.providerIds().map((provider) => [provider, false]),
  ) as Record<string, boolean>;
  /** 假密钥末四位（真后端由快照给）。 */
  const apiKeyTails: Record<string, string | null> = Object.fromEntries(
    catalog.providerIds().map((provider) => [provider, null]),
  );
  let virtualCableInstalled = true;
  let virtualCableStatus:
    | "installed"
    | "install_pending_reboot"
    | "uninstall_incomplete"
    | "not_installed"
    | "not_applicable" = "installed";
  let virtualCable16ChStatus: "visible" | "hidden" | "absent" = "hidden";
  /**
   * 假后端的宿主档位与"关掉的位"（S0 §2.5.0 第 1–2 步的模拟）。
   * URL 开关（`?host=` / `?off=` / `?virtual_mic=` / `?on=`）见 `seed()`。
   */
  let host: MockHost = "windows";
  let offOverride: Partial<Record<HostCapability, UnavailableReason>> = {};
  let onBits: readonly HostCapability[] = [];
  let cableBlockers = [MOCK_APPS[1]].filter((app): app is AudioApp => app !== undefined);
  let micActive = false;
  let usage: UsageLedger = mockUsage();
  let notices: Notice[] = [];
  const handlers = new Set<(e: VoxEvent) => void>();
  let timer: ReturnType<typeof setInterval> | null = null;
  let lastTick = performance.now();
  /** 假的「按住说话」相位：hold 模式下自动一按一放，好让人看到门控切换。 */
  let holdUntil = 0;
  let holdNextAt = 0;
  let oscRunning = false;
  /** 每家「官方列表」最近一次拉取的时间（毫秒）。首次给个几分钟前，好让「3 分钟前」点得通。 */
  const modelFetchedAt: Partial<Record<ModelProvider, number>> = {};
  const modelFailures = parseMockErrors(window.location.search);
  /** 网络体检的演示档位（`?mockTun=1` / `?mockNetFail=1`，见 `mock/network.ts`）。 */
  const netFlags = mockNetworkFlags(window.location.search);
  /**
   * 控制面起不来的那一档（`?control=busy`）：真后端里这是 `serve()` 的 `EADDRINUSE`，
   * 假后端造一句同样意思的话，好让"起不来时界面怎么显示"这条路点得通。
   */
  let controlFail = false;
  /** 端口是系统分配（`control.port === 0`）时，假后端"绑上"的那个号。 */
  const MOCK_BOUND_PORT = 47123;
  /** 握手文件路径：真后端是 `<app_config_dir>/control.json`，假后端给个同形状的。 */
  const MOCK_STATE_FILE = "~/.config/Vox/control.json";

  const lanes: Record<PipelineName, Lane> = {
    speak: mkLane(SPEAK_SCRIPT, "speak"),
    listen: mkLane(LISTEN_SCRIPT, "listen"),
  };

  function mkLane(script: string[], track: Track): Lane {
    return {
      state: "idle",
      gate: null,
      voice: new FakeVoice(),
      gateImpl: new FakeGate("level"),
      typer: new FakeTyper(track, script),
    };
  }

  function emit(event: VoxEvent): void {
    for (const h of [...handlers]) h(event);
  }

  function notify(
    severity: Notice["severity"],
    key: string,
    pipeline: PipelineName | null = null,
    params: Record<string, string> = {},
    detail?: string,
  ): void {
    const notice: Notice = { severity, key, params, pipeline, ...(detail ? { detail } : {}) };
    notices = [...notices, notice].slice(-50);
    emit({ kind: "notice", notice });
  }

  function setState(pipeline: PipelineName, state: PipelineState): void {
    const lane = lanes[pipeline];
    if (lane.state === state) return;
    lane.state = state;
    if (!isRunning(state)) lane.gate = null;
    emit({ kind: "pipeline_state", pipeline, state });
  }

  function snap(pipeline: PipelineName): PipelineSnapshot {
    const lane = lanes[pipeline];
    return {
      state: lane.state,
      state_label: STATE_LABEL[lane.state],
      running: isRunning(lane.state),
      gate: lane.gate,
      latency: latency(pipeline),
    };
  }

  function latency(pipeline: PipelineName): LatencySnapshot {
    const lane = lanes[pipeline];
    const running = isRunning(lane.state);
    // 演示数据：跟随时钟轻微抖动，让数字看起来是活的。真实值来自 Rust 侧的
    // LatencySnapshot，这里只是让假后端把界面喂饱。
    const jitter = (base: number, span: number) => running ? Math.max(0, base + Math.sin(performance.now() / 400 + (pipeline === "speak" ? 0 : 2)) * span) : 0;
    const metric = (base: number): LatencyMetric => {
      const last = jitter(base, base * 0.2);
      return {
        last_ms: running ? Math.round(last) : null,
        p50_ms: running ? Math.round(base + base * 0.06) : null,
        p95_ms: running ? Math.round(base * 1.35) : null,
        samples: running ? 37 : 0,
      };
    };
    const queued = Math.max(0, Math.round(jitter(8, 4)));
    return {
      connect_ms: 312,
      session_ready_ms: 425,
      input_queue: metric(12),
      upload_send: metric(18),
      server_vad: metric(64),
      first_text: metric(380),
      first_audio: metric(520),
      first_playback: metric(545),
      turn_complete: metric(880),
      completed_turns: 4,
      input_queue_depth: queued,
      input_queue_oldest_ms: Math.round(queued * 20),
      playback_queue_ms: Math.max(0, Math.round(jitter(60, 20))),
      processed_chunks: 1186,
      dropped_chunks: 0,
    };
  }

  function bumpUsage(model: string, chars: number): void {
    const inTok = Math.max(1, Math.round(chars * 2.4));
    const outTok = Math.max(1, Math.round(chars * 1.6));
    const prev = usage[model];
    const base = prev ?? {
      input_tokens: 0, output_tokens: 0, total_tokens: 0, turns: 0,
      daily: { input_tokens: 0, output_tokens: 0, total_tokens: 0, turns: 0 },
      daily_date: new Date().toISOString().slice(0, 10),
      monthly: { input_tokens: 0, output_tokens: 0, total_tokens: 0, turns: 0 },
      monthly_month: new Date().toISOString().slice(0, 7),
      updated_at: 0,
    };
    const add = (t: typeof base.daily) => ({
      input_tokens: t.input_tokens + inTok,
      output_tokens: t.output_tokens + outTok,
      total_tokens: t.total_tokens + inTok + outTok,
      turns: t.turns,
    });
    usage = {
      ...usage,
      [model]: {
        ...add(base),
        turns: base.turns,
        daily: add(base.daily),
        monthly: add(base.monthly),
        daily_date: base.daily_date,
        monthly_month: base.monthly_month,
        updated_at: Math.floor(Date.now() / 1000),
      },
    };
    emit({ kind: "usage_changed", usage });
  }

  function tick(): void {
    const now = performance.now();
    const dt = now - lastTick;
    lastTick = now;

    // --- 对外说话：按激活方式驱动门控 ---
    const speak = lanes.speak;
    if (isRunning(speak.state)) {
      if (settings.speak.activation_mode === "hold") {
        if (now > holdNextAt) {
          holdUntil = now + 1800 + Math.random() * 1500;
          holdNextAt = holdUntil + 1200 + Math.random() * 1600;
        }
        const held = now < holdUntil;
        speak.gate = speak.gateImpl.manual(now, held);
        setMic(held);
      } else {
        const rms = speak.voice.step(now, dt);
        speak.gate = speak.gateImpl.level(now, rms, settings.speak.gate_threshold);
        setMic(speak.gate.active);
      }
      emit({ kind: "gate_status", pipeline: "speak", status: speak.gate });
      // 关掉「翻译」= 直通原声：不走云端，只有说话状态，没有字幕、不烧 token。
      if (settings.speak.translate && speak.gate.active && speak.typer) {
        const n = speak.typer.step(now, emit);
        if (n > 0 && Math.random() < 0.35) bumpUsage(settings.speak.model_name, n);
      }
    } else if (micActive) {
      setMic(false);
    }

    // --- 听人说话：门是常开的（真实后端阈值 0，无条件放行），电平条照样动 ---
    const listen = lanes.listen;
    if (isRunning(listen.state)) {
      const rms = listen.voice.step(now, dt);
      listen.gate = listen.gateImpl.level(now, rms, 0);
      emit({ kind: "gate_status", pipeline: "listen", status: listen.gate });
      if (listen.gate.active && listen.typer) {
        const n = listen.typer.step(now, emit);
        if (n > 0 && Math.random() < 0.35) bumpUsage(settings.listen.model_name, n);
      }
    }

    // 延迟统计沿每拍推一次（真实后端约 500 ms 节流；这里只是喂活面板）。
    if (isRunning(speak.state)) {
      emit({ kind: "latency_changed", pipeline: "speak", latency: latency("speak") });
    }
    if (isRunning(listen.state)) {
      emit({ kind: "latency_changed", pipeline: "listen", latency: latency("listen") });
    }
  }

  function setMic(active: boolean): void {
    if (micActive === active) return;
    micActive = active;
    emit({ kind: "mic_active", active });
  }

  function ensureTimer(): void {
    const anyLive =
      (["speak", "listen"] as const).some((p) => isRunning(lanes[p].state));
    if (anyLive && timer === null) {
      lastTick = performance.now();
      timer = setInterval(tick, TICK_MS);
    } else if (!anyLive && timer !== null) {
      clearInterval(timer);
      timer = null;
      setMic(false);
    }
  }

  async function start(pipeline: PipelineName): Promise<void> {
    const provider = settings[pipeline].provider;
    if (!apiKeys[provider]) {
      notify("warning", "notice.apiKeyMissing", pipeline);
      return;
    }
    // 位为假 ⇒ 这条腿在这台机器上根本装不起来（真后端是装配期的 `MissingInput`）：
    // 假后端也照实拒绝，不然界面会显示"运行中"，而位说这台设备做不到。
    if (pipeline === "listen" && !report().host.program_tap.enabled) {
      notify("warning", "notice.noCaptureInput", pipeline);
      return;
    }
    if (pipeline === "listen" && !settings.listen.target) {
      notify("warning", "notice.listenTargetMissing", pipeline);
      return;
    }
    if (isRunning(lanes[pipeline].state)) return;
    setState(pipeline, "starting");
    ensureTimer();
    await wait(420);
    setState(pipeline, "ready");
    await wait(260);
    setState(pipeline, "active");
  }

  async function stop(pipeline: PipelineName): Promise<void> {
    if (!isRunning(lanes[pipeline].state)) return;
    lanes[pipeline].typer?.reset(emit);
    setState(pipeline, "idle");
    ensureTimer();
  }

  /**
   * 这台机器上报"关掉的位"（`HostFacts.off` 的模拟）。
   *
   * - `vr_captions`：这份构建默认没开 `steamvr-overlay`（`?on=vr_captions` 可以按开）；
   * - Windows 的 `virtual_mic` 只认安装器探测（`win.rs::virtual_mic_ensure` 同款）——
   *   点一次"卸载"它就该翻假，点"安装"又翻真；
   * - `?off=` 覆盖在上面（`?on=` 再抹掉）——测试要能造出任意的 `(位, reason)`。
   */
  function offBits(): Partial<Record<HostCapability, UnavailableReason>> {
    const off: Partial<Record<HostCapability, UnavailableReason>> = { vr_captions: "not_built" };
    if (host === "windows") {
      if (virtualCableStatus === "installed") delete off.virtual_mic;
      else if (virtualCableStatus === "not_installed") off.virtual_mic = "not_installed";
      else off.virtual_mic = "pending_reboot";
    }
    Object.assign(off, offOverride);
    for (const bit of onBits) delete off[bit];
    return off;
  }

  /** 这份快照里那份能力位报告（档位 + off + 两条腿的 provider）。 */
  function report(): Capabilities {
    return mockCapabilities({
      host,
      off: offBits(),
      speak: settings.speak.provider,
      listen: settings.listen.provider,
    });
  }

  /**
   * 控制面现在的样子（Rust `mcp::Status` 的假版本）。
   *
   * **按设置推 + 一个"起不来"的开关**：假后端没有真监听，起没起只能这么演。
   * 真后端那一边是"句柄在不在"（`ControlPlane` 手里的 `Option<ServerHandle>`），
   * 界面两条路读到的都是同一个形状。
   */
  function controlStatus(): ControlStatus {
    const wanted = settings.control;
    const running = wanted.enabled && !controlFail;
    return {
      enabled: wanted.enabled,
      port: wanted.port,
      running,
      bound_port: running ? (wanted.port !== 0 ? wanted.port : MOCK_BOUND_PORT) : null,
      error:
        wanted.enabled && !running
          ? "监听 127.0.0.1 失败：地址已被占用（Address already in use）"
          : null,
      state_file: MOCK_STATE_FILE,
    };
  }

  function currentSnapshot(): Snapshot {
    return {
      settings: cloneSettings(settings),
      api_keys: { ...apiKeys },
      api_key_tails: { ...apiKeyTails },
      speak: snap("speak"),
      listen: snap("listen"),
      mic_active: micActive,
      headphones_advised: isRunning(lanes.speak.state) && isRunning(lanes.listen.state),
      devices: {
        inputs: MOCK_INPUTS,
        outputs: MOCK_OUTPUTS,
        apps: MOCK_APPS,
        virtual_cable_installed: virtualCableInstalled,
        virtual_cable_status: virtualCableStatus,
        virtual_cable_16ch_status: virtualCable16ChStatus,
      },
      usage,
      notices: [...notices],
      capabilities: report(),
      control: controlStatus(),
    };
  }

  /**
   * 假后端默认起在「已经在用」的状态：有密钥、选好了监听程序、两条常驻管线在跑。
   * 不然一进来什么都不动，看不出电平条和字幕长什么样。
   *
   * URL 开关（都只在 mock 里）：
   * - `?cold=1` 空状态（未填密钥、没选程序）；
   * - `?host=windows|linux|android|embedded` 换宿主档位（`?platform=linux` 是等价旧写法，
   *   README 里用过）——四档的位表不同，界面该按位降级；
   * - `?off=<位>:<reason>,...` 把这台机器上某几位按下来（任意 `(位, reason)` 组合）；
   * - `?on=<位>,...` 反过来把某几位按开（例如 `?on=vr_captions` = 这份构建编了头显字幕）；
   * - `?virtual_mic=not_wired` 是 `off=virtual_mic:not_wired` 的简写（接线前的中间态最常用）；
   * - `?control=busy` 让控制面"起不来"（端口被占的那一档）：总开关开着也报没在跑 + 原因。
   */
  function seed(): void {
    const params = new URLSearchParams(window.location.search);
    const rawHost = params.get("host") ?? (params.get("platform") === "linux" ? "linux" : "windows");
    host = rawHost === "linux" || rawHost === "android" || rawHost === "embedded" ? rawHost : "windows";

    const off: Partial<Record<HostCapability, UnavailableReason>> = {};
    for (const entry of params.get("off")?.split(",") ?? []) {
      const [bit, reason] = entry.split(":");
      if (
        HOST_CAPABILITIES.includes(bit as HostCapability) &&
        UNAVAILABLE_REASONS.includes(reason as UnavailableReason)
      ) {
        off[bit as HostCapability] = reason as UnavailableReason;
      }
    }
    const virtualMic = params.get("virtual_mic") as UnavailableReason | null;
    if (virtualMic && UNAVAILABLE_REASONS.includes(virtualMic)) off.virtual_mic = virtualMic;
    offOverride = off;
    // `?mockNotice=<key>[&mockParams=a:1,b:2][&mockDetail=<原文>]`：开局稍后发一条该 key 的通知
    // （等首次启动按系统语言切完界面语言——真后端的语言是持久化的，不存在这个先后），
    // `check:i18n` 用它验证 toast 的渲染、detail 原样保留和缺 key 的回退。
    const noticeKey = params.get("mockNotice");
    if (noticeKey) {
      const noticeParams: Record<string, string> = {};
      for (const pair of params.get("mockParams")?.split(",") ?? []) {
        const [name, ...rest] = pair.split(":");
        if (name) noticeParams[name] = rest.join(":");
      }
      const mockDetail = params.get("mockDetail") ?? undefined;
      setTimeout(() => notify("warning", noticeKey, "speak", noticeParams, mockDetail), 1500);
    }
    onBits = (params.get("on")?.split(",") ?? []).filter((bit): bit is HostCapability =>
      HOST_CAPABILITIES.includes(bit as HostCapability),
    );

    if (host !== "windows") {
      // 只有 Windows 那份构建有"装驱动"这一步：别的档位没有装/卸/多声道那一套。
      virtualCableStatus = "not_applicable";
      virtualCableInstalled = false;
      virtualCable16ChStatus = "absent";
    }
    // 控制面起不来那一档：给设置页一条"起不来"的路走（`?control=busy`）。
    controlFail = params.get("control") === "busy";
    // `?mockFresh=1`：模拟首次启动——没有任何密钥、引导没走过（其余档位一律视为已走过）。
    const fresh = params.get("mockFresh") === "1";
    settings = { ...settings, onboarding_done: !fresh };
    if (params.get("cold") === "1" || fresh) return;
    for (const provider of catalog.providerIds()) {
      apiKeys[provider] = true;
      apiKeyTails[provider] = "a1b2";
    }
    const app = MOCK_APPS[0];
    if (app) {
      settings = normalizeSettings({
        ...cloneSettings(settings),
        listen: {
          ...settings.listen,
          target: { executable: app.executable, display_name: app.display_name, include_process_tree: true },
        },
      });
    }
    setState("speak", "active");
    // 抓程序这一位为假（手机 / 无屏档）就不起听人说话：位说做不到，界面也不该显示"运行中"。
    if (report().host.program_tap.enabled) setState("listen", "active");
    ensureTimer();
  }

  seed();

  return {
    mock: true,
    async snapshot() {
      await wait(60);
      return currentSnapshot();
    },
    async updateSettings(patch: SettingsPatch) {
      const before = settings;
      const next = normalizeSettings(mergePatch(cloneSettings(settings), patch));
      if (JSON.stringify(before) === JSON.stringify(next)) return cloneSettings(settings);
      settings = next;
      // 只有重启才生效的格改了、这条腿又在跑：发一条通知，等下把这条腿停掉重开
      // （真芯是 `Runtime::restart_required` + 放锁后 `stop`/`start`）。
      const restarted: PipelineName[] = [];
      if (isRunning(lanes.speak.state) && needsRestart(before, next, "speak")) {
        notify("info", "notice.restartSpeak", "speak");
        restarted.push("speak");
      }
      if (isRunning(lanes.listen.state) && needsRestart(before, next, "listen")) {
        notify("info", "notice.restartListen", "listen");
        restarted.push("listen");
      }
      if (before.speak.activation_mode !== next.speak.activation_mode) {
        holdUntil = 0;
        holdNextAt = 0;
        setMic(false);
      }
      emit({ kind: "settings_changed", settings: cloneSettings(settings) });
      for (const pipeline of restarted) {
        await stop(pipeline);
        await start(pipeline);
      }
      return cloneSettings(settings);
    },
    async setApiKey(provider, key) {
      apiKeys[provider] = key.trim().length > 0;
      apiKeyTails[provider] = key.trim().length >= 8 ? key.trim().slice(-4) : null;
    },
    async togglePipeline(pipeline) {
      if (isRunning(lanes[pipeline].state)) await stop(pipeline);
      else await start(pipeline);
    },
    // VRChat(OSC) 假后端：状态本地存、命令成功占位。
    async oscStart() {
      if (!oscRunning) oscRunning = true;
      await wait(280);
    },
    async oscUpdate() {
      await wait(180);
    },
    async oscStop() {
      oscRunning = false;
      await wait(180);
    },
    async oscSendChatbox() {
      await wait(220);
    },
    async oscSetAvatar() {
      await wait(180);
    },
    async screenRect() {
      await wait(40);
      return { x: 0, y: 0, width: 1920, height: 1080 };
    },
    async resetUsage() {
      usage = {};
      emit({ kind: "usage_changed", usage });
    },
    async resetUsageModel(model: string) {
      const next = { ...usage };
      delete next[model];
      usage = next;
      emit({ kind: "usage_changed", usage });
    },
    async refreshDevices() {
      await wait(320);
      emit({ kind: "devices_changed" });
    },
    async installVirtualCable() {
      await wait(900);
      virtualCableInstalled = true;
      virtualCableStatus = "installed";
      virtualCable16ChStatus = "hidden";
      emit({ kind: "devices_changed" });
      return { needs_reboot: false, multichannel_hidden: true };
    },
    async virtualCableBlockers() {
      await wait(250);
      return [...cableBlockers];
    },
    async uninstallVirtualCable(closeBlockers: boolean) {
      if (cableBlockers.length > 0 && !closeBlockers) {
        // 真后端的命令 `Err` 就是这个形状 `{key, params, detail?}`，不是 Error。
        const failure: Msg = {
          key: "error.cableBusy",
          params: { names: cableBlockers.map((app) => app.display_name).join(", ") },
        };
        throw failure;
      }
      await wait(700);
      cableBlockers = [];
      virtualCableInstalled = false;
      virtualCableStatus = "not_installed";
      virtualCable16ChStatus = "absent";
      emit({ kind: "devices_changed" });
      return { needs_reboot: false, multichannel_hidden: true };
    },
    async setVirtualCableMultichannelVisible(visible: boolean) {
      await wait(500);
      virtualCable16ChStatus = visible ? "visible" : "hidden";
      emit({ kind: "devices_changed" });
      return { needs_reboot: false, multichannel_hidden: !visible };
    },
    async openVirtualCableWebsite() {
      window.open("https://vb-audio.com/Cable/", "_blank", "noopener");
    },
    async openVirtualCableDonation() {
      window.open("https://vb-audio.com/Services/licensing.htm", "_blank", "noopener");
    },
    async openProviderConsole(provider) {
      window.open(catalog.providerConsoleUrl(provider), "_blank", "noopener");
    },
    async listModels(provider, opts) {
      await wait(400);
      if (modelFailures.missing.has(provider)) throw new Error("command list_models not found");
      const now = Date.now();
      if (opts?.refresh || modelFetchedAt[provider] === undefined) {
        modelFetchedAt[provider] = opts?.refresh ? now : now - 3 * 60_000;
      }
      return mockModelList(provider, {
        hasKey: apiKeys[provider] === true,
        fetchedAt: modelFetchedAt[provider] ?? now,
        errors: modelFailures.errors,
      });
    },
    async testNetwork(provider) {
      await wait(600);
      return mockNetworkReport(provider, settings.network[provider], netFlags);
    },
    async detectTakeover(provider) {
      await wait(250);
      return mockTakeoverReport(provider, netFlags);
    },
    // 假后端没有真实网络/磁盘：覆盖版永远为空，检查/应用给一段占位结果。
    async readCatalogOverride() {
      return null;
    },
    async checkCatalogUpdate() {
      return { current: "builtin", latest: "builtin" };
    },
    async applyCatalogUpdate() {
      return { file: "aliyun.json", verified: "builtin" };
    },
    subscribe(handler) {
      handlers.add(handler);
      return () => {
        handlers.delete(handler);
      };
    },
  };
}

function wait(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
