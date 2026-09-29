/**
 * 虚拟麦克风（VB-CABLE）管理区：安装状态、二次确认卸载、重新安装、
 * 16 声道端点的隐藏与恢复。
 *
 * **能不能用读能力位 `virtual_mic`**（`hostBit`），**不是**读
 * `devices.virtual_cable_status`：那个字段只说明安装器 / 平台形态（装没装、要不要重启、
 * 这一档有没有"装驱动"这一步），只决定这一页摆哪些管理动作。位为假时不再渲染
 * "去目标程序里选 Vox Virtual Mic"那句引导，改渲染 `CapabilityNote` 的 reason 文案
 * （S0 §2.6 R1/R9：不静默、说清"这台设备做不到"）。
 *
 * 从 Settings.tsx 抽出来的一整块 Cable 相关 UI：状态徽标、安装/卸载按钮、
 * 驱动下载入口、多声道隐藏/恢复，以及卸载前的二次确认弹窗。徽标文案与
 * 样式类原来是嵌套三元，现在改成模块级 Record 表查表（badges 是静态 class，
 * labels 是 i18n key，组件内用 t(...) 取翻译）。
 *
 * 焦点网格（src/lib/focus.ts）依赖 data-focus-item，每个交互控件都保留；
 * 弹窗的 role/aria-* 原样搬过来，不改属性。
 */

import { useState } from "react";
import { hostBit } from "../capabilities";
import { CapabilityNote } from "../components/Capability";
import { useT } from "../i18n/context";
import { useStore } from "../store";
import type { AudioApp, Snapshot } from "../types.snapshot";
import { SettingsItem } from "../ui/controls";
import { IconDownload, IconExternal, IconTrash } from "../ui/icons";
import { useToast } from "../ui/toast";

type VirtualCableStatus = Snapshot["devices"]["virtual_cable_status"];
type ChannelStatus = Snapshot["devices"]["virtual_cable_16ch_status"];

interface UninstallDialogState {
  blockers: AudioApp[];
  loading: boolean;
}

/** Cable 状态徽标：后端五态到 badge class 的静态映射。 */
const CABLE_BADGE: Record<VirtualCableStatus, string> = {
  installed: "badge badge-running",
  install_pending_reboot: "badge badge-warn",
  uninstall_incomplete: "badge badge-warn",
  not_installed: "badge badge-idle",
  // Linux：不是"没装"，是"不需要装"。
  not_applicable: "badge badge-neutral",
};

/** 16 声道端点徽标：三态到 badge class 的静态映射。 */
const CHANNEL_BADGE: Record<ChannelStatus, string> = {
  hidden: "badge badge-running",
  visible: "badge badge-warn",
  absent: "badge badge-neutral",
};

/** Cable 状态文案的 i18n key 表；组件内用 t() 取翻译，loading 时短路成「检测中」。 */
const CABLE_STATUS_LABEL: Record<VirtualCableStatus, string> = {
  installed: "settings.cableStatus.installed",
  install_pending_reboot: "settings.cableStatus.installPendingReboot",
  uninstall_incomplete: "settings.cableStatus.uninstallIncomplete",
  not_installed: "settings.cableStatus.notInstalled",
  not_applicable: "settings.cableStatus.notApplicable",
};

export function CableManager() {
  const { api, snapshot, applyCableChannelStatus } = useStore();
  const toast = useToast();
  const t = useT();
  const [cableBusy, setCableBusy] = useState<
    "install" | "uninstall" | "hide16" | "show16" | null
  >(null);
  const [uninstallDialog, setUninstallDialog] = useState<UninstallDialogState | null>(null);
  const loading = snapshot === null;
  /**
   * **能不能用**只看能力位（`virtual_mic`）——位是事实，由"真的把这条路打开的那段代码"负责
   * （S0 §2.5.4）。
   *
   * `virtual_cable_status` 是**安装器 / 平台形态**：装没装、要不要重启、这一档有没有
   * "装驱动"这一步。它**不是**"能不能用"的判据（§3.2：这个字段降级成安装器状态），
   * 只决定这一页摆哪些管理动作。
   */
  const micBit = hostBit(snapshot, "virtual_mic");
  const usable = snapshot !== null && micBit.enabled;
  const detail = snapshot?.devices.virtual_cable_status ?? "not_installed";
  /** 这一档有没有"装 / 卸 / 多声道"这套动作（Linux 是 PipeWire 原生 sink，没有）。 */
  const manageable = detail !== "not_applicable";
  const cableStatusLabel = loading
    ? t("settings.cableStatus.checking")
    : t(CABLE_STATUS_LABEL[detail]);
  const cableBadgeClass = CABLE_BADGE[detail];
  const cableInstalled = detail === "installed";
  const cablePending = detail === "install_pending_reboot";
  const cableIncomplete = detail === "uninstall_incomplete";

  const channelStatus = snapshot?.devices.virtual_cable_16ch_status ?? "absent";
  const channelBadgeClass = CHANNEL_BADGE[channelStatus];

  /**
   * 这一档没有"装 / 卸"这一步：只报状态 + 告诉用户去哪选设备。
   *
   * **位为真才给"去目标程序里选 Vox Virtual Mic"那句引导**：位为假时（比如 Linux
   * 接线前的 `not_wired`）那句引导指向一个不存在的设备，正是 §1.4 的老毛病——改成
   * `CapabilityNote` 的 reason 文案（R1/R9）。
   */
  if (!manageable) {
    return (
      <div className="settings-group">
        <SettingsItem
          title={t("settings.virtualCable")}
          desc={usable ? t("settings.virtualCableNativeHint") : undefined}
          control={
            <span className="badge badge-neutral">
              <span className={usable ? "status-dot running" : "status-dot"} />
              {loading
                ? t("settings.cableStatus.checking")
                : usable
                  ? t("settings.cableStatus.notApplicable")
                  : t("capabilities.unavailable")}
            </span>
          }
        />
        <CapabilityNote bit="virtual_mic" style={{ marginTop: 8 }} />
      </div>
    );
  }

  const manageCable = async (action: "install" | "uninstall", closeBlockers = false) => {
    setCableBusy(action);
    try {
      const result =
        action === "install"
          ? await api.installVirtualCable()
          : await api.uninstallVirtualCable(closeBlockers);
      if (action === "uninstall") setUninstallDialog(null);
      if (result.needs_reboot) {
        toast(
          "warning",
          action === "install"
            ? t("settings.toast.installNeedReboot")
            : t("settings.toast.uninstallNeedReboot"),
        );
      } else if (action === "install" && !result.multichannel_hidden) {
        toast("warning", t("settings.toast.hideMultichannelFailed"));
      } else {
        toast(
          "success",
          action === "install" ? t("settings.toast.installDone") : t("settings.toast.uninstallDone"),
        );
      }
    } catch (error: unknown) {
      toast(
        "danger",
        action === "install"
          ? t("settings.toast.installFailed", { error: String(error) })
          : t("settings.toast.uninstallFailed", { error: String(error) }),
      );
    } finally {
      setCableBusy(null);
    }
  };

  const inspectUninstall = async () => {
    setUninstallDialog({ blockers: [], loading: true });
    try {
      const blockers = await api.virtualCableBlockers();
      setUninstallDialog({ blockers, loading: false });
    } catch (error: unknown) {
      setUninstallDialog(null);
      toast("danger", t("settings.toast.checkBlockersFailed", { error: String(error) }));
    }
  };

  const setChannelVisible = async (visible: boolean) => {
    setCableBusy(visible ? "show16" : "hide16");
    try {
      const result = await api.setVirtualCableMultichannelVisible(visible);
      // 后端回报的禁用状态是权威值，不等异步 devices_changed 事件就翻徽标。
      // （devices_changed 只在 outputs/inputs/apps 真的变时才发；16 声道端点被
      // 禁用后 Core Audio 仍可能报 ACTIVE，列表不变，事件不会来，徽标就卡死。）
      applyCableChannelStatus(result.multichannel_hidden ? "hidden" : "visible");
      if (result.needs_reboot) {
        toast("warning", t("settings.toast.channelNeedReboot"));
      } else {
        toast("success", visible ? t("settings.toast.showChannel") : t("settings.toast.hideChannel"));
      }
    } catch (error: unknown) {
      toast("danger", t("settings.toast.setChannelFailed", { error: String(error) }));
    } finally {
      setCableBusy(null);
    }
  };

  return (
    <>
      <div className="settings-group">
        <SettingsItem
          title={t("settings.virtualCable")}
          control={
            <div className="row row-wrap">
              <span className={cableBadgeClass}>
                <span className={cableInstalled ? "status-dot running" : "status-dot"} />
                {cableStatusLabel}
              </span>
              {cablePending ? null : cableInstalled || cableIncomplete ? (
                <button
                  type="button"
                  className={cableIncomplete ? "btn btn-danger btn-sm" : "btn btn-secondary btn-sm"}
                  disabled={loading || cableBusy !== null}
                  data-focus-item
                  onClick={() => void inspectUninstall()}
                >
                  <IconTrash size={14} />
                  {cableIncomplete ? t("settings.microSwitch.continueUninstall") : t("settings.microSwitch.uninstall")}
                </button>
              ) : (
                <button
                  type="button"
                  className="btn btn-primary btn-sm"
                  disabled={loading || cableBusy !== null}
                  data-focus-item
                  onClick={() => void manageCable("install")}
                >
                  <IconDownload size={14} />
                  {cableBusy === "install" ? t("settings.microSwitch.installing") : t("settings.microSwitch.install")}
                </button>
              )}
            </div>
          }
        />
        <CapabilityNote bit="virtual_mic" style={{ marginTop: 8 }} />
        <SettingsItem
          title={t("settings.driveSource")}
          control={
            <div className="row">
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                data-focus-item
                onClick={() => void api.openVirtualCableWebsite()}
              >
                <IconExternal size={14} />
                {t("settings.driveSite")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                data-focus-item
                onClick={() => void api.openVirtualCableDonation()}
              >
                {t("settings.licenseDonate")}
              </button>
            </div>
          }
        />
        <SettingsItem
          title={t("settings.multichannel")}
          control={
            <div className="row">
              <span className={channelBadgeClass}>
                {channelStatus === "hidden"
                  ? t("settings.channelStatus.hidden")
                  : channelStatus === "visible"
                    ? t("settings.channelStatus.visible")
                    : t("settings.channelStatus.none")}
              </span>
              {cableInstalled && channelStatus !== "absent" ? (
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  disabled={cableBusy !== null}
                  data-focus-item
                  onClick={() => void setChannelVisible(channelStatus === "hidden")}
                >
                  {channelStatus === "hidden"
                    ? cableBusy === "show16"
                      ? t("settings.microSwitch.showing")
                      : t("settings.microSwitch.show")
                    : cableBusy === "hide16"
                      ? t("settings.microSwitch.hiding")
                      : t("settings.microSwitch.hide")}
                </button>
              ) : null}
            </div>
          }
        />
      </div>

      {uninstallDialog ? (
        <div className="dialog-backdrop" role="presentation">
          <div
            className="dialog-card"
            role="dialog"
            aria-modal="true"
            aria-labelledby="cable-uninstall-title"
          >
            <h2 id="cable-uninstall-title">{t("settings.cableDialog.title")}</h2>
            {uninstallDialog.loading ? (
              <p>{t("settings.cableDialog.loading")}</p>
            ) : uninstallDialog.blockers.length > 0 ? (
              <>
                <p className="hint-danger">
                  {t("settings.cableDialog.closeApps")}
                </p>
                <div className="dialog-app-list">
                  {uninstallDialog.blockers.map((app) => (
                    <div className="dialog-app" key={app.pid}>
                      <span>{app.display_name}</span>
                      <span className="mono">PID {app.pid}</span>
                    </div>
                  ))}
                </div>
              </>
            ) : (
              <p>
                {cableIncomplete
                  ? t("settings.cableDialog.restartAudio")
                  : t("settings.cableDialog.noBlockers")}
              </p>
            )}
            <div className="dialog-actions">
              <button
                type="button"
                className="btn btn-secondary"
                disabled={cableBusy !== null}
                data-focus-item
                onClick={() => setUninstallDialog(null)}
              >
                {t("settings.cableDialog.cancel")}
              </button>
              {!uninstallDialog.loading ? (
                <button
                  type="button"
                  className="btn btn-danger"
                  disabled={cableBusy !== null}
                  data-focus-item
                  onClick={() =>
                    void manageCable("uninstall", uninstallDialog.blockers.length > 0)
                  }
                >
                  {cableBusy === "uninstall"
                    ? t("settings.cableDialog.uninstalling")
                    : uninstallDialog.blockers.length > 0
                      ? t("settings.cableDialog.uninstallWithClose")
                      : cableIncomplete
                        ? t("settings.cableDialog.resetAndUninstall")
                        : t("settings.cableDialog.uninstall")}
                </button>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}
