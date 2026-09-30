//! Windows 侧的平台实现：全部是对现有 `-win` crate 的薄包装。
//!
//! 这里**不做任何判断**，只是把 `lib.rs` 认的那组函数名对着 Win32 实现接上。
//! 真正的活儿在 `crates/vox-audio-win`、`vox-input-win`、`vox-overlay-win` 里。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use vox_core::capability::{Capability, CapabilityStatus, HostFacts, UnavailableReason};
use vox_core::composition::HostKind;
use vox_core::pipeline::{CaptureFactory, PlaybackFactory};
use vox_core::ports::{Clock, DeviceRegistry, HotkeyHost, PortResult};
use vox_core::runtime::Runtime;

use super::{GeometryCallback, OverlayFailure, VirtualDeviceStatus};
use crate::state::OverlayHandle;

/// 悬浮窗的具体句柄留一份：`SubtitleView` trait 上没有关闭方法（`shutdown` 是
/// `Overlay` 的固有方法），而 `AppState` 里存的是 trait object，退出时得靠这个关窗。
static OVERLAY: OnceLock<Arc<vox_overlay_win::Overlay>> = OnceLock::new();

/// 启动前的一次性短路：`--vox-restore-defaults` 只做默认设备写回就退出。
/// 这是 VB-CABLE 卸载流程的收尾手段，只在 Windows 上有意义。
pub fn pre_main() -> bool {
    vox_audio_win::restore_via_args_if_requested()
}

pub fn clock() -> Arc<dyn Clock> {
    Arc::new(crate::sys::clock::SystemClock::new())
}

pub fn alert(title: &str, body: &str) {
    crate::sys::fatal::alert(title, body);
}

pub fn capture_factory() -> CaptureFactory {
    // 采集用**严格版**：系统不支持进程环回就报错，不偷偷退化成整机环回。
    Box::new(|| Box::new(vox_audio_win::WinCapture::new()))
}

pub fn playback_factory() -> PlaybackFactory {
    Box::new(|| {
        let rf = vox_dsp::ports::resample_factory();
        Box::new(vox_audio_win::WinPlayback::new(rf))
    })
}

pub fn registry() -> Arc<dyn DeviceRegistry> {
    Arc::new(vox_audio_win::WinDeviceRegistry::new())
}

/// 起热键线程。事件回调由 `vox-input-win` 直接推给 `Runtime::on_hotkey`。
pub fn start_hotkeys(runtime: Runtime) -> PortResult<Arc<dyn HotkeyHost>> {
    let listener = crate::input::start(runtime)?;
    Ok(listener)
}

pub fn stop_hotkeys() {
    crate::input::stop();
}

pub fn spawn_overlay(
    settings: &vox_core::settings::SubtitleSettings,
    on_geometry: GeometryCallback,
) -> Result<OverlayHandle, OverlayFailure> {
    let overlay = vox_overlay_win::Overlay::spawn_with_geometry(settings, Some(on_geometry))
        .map_err(|error| OverlayFailure {
            // 连分层窗都建不出来 = 这台机器给不了我们窗口（§2.5.4 的 `captions`）。
            reason: UnavailableReason::Unsupported,
            error,
        })?;
    let _ = OVERLAY.set(Arc::clone(&overlay));
    Ok(overlay)
}

/// 悬浮窗还活着吗。帧线程靠它发现"窗口自己没了"（比如 Windows 上用户把窗关了）。
pub fn overlay_running() -> bool {
    OVERLAY.get().is_some_and(|overlay| overlay.is_running())
}

pub fn shutdown_overlay() {
    if let Some(overlay) = OVERLAY.get() {
        overlay.shutdown();
    }
}

/// 透明无边框窗口下 `tauri.conf.json` 的 `minHeight` 压不住（实测会被压到 ~30px），
/// `set_min_size` 走 tao 的 subclass 链同样压不到，所以在 `WM_GETMINMAXINFO`
/// 最底层强制最小尺寸。
pub fn enforce_min_size(window: &tauri::WebviewWindow) {
    if let Ok(hwnd) = window.hwnd() {
        crate::winminmax::enforce_min_size(hwnd.0, 640, 38);
    }
}

/// 哪一档宿主。Windows 桌面：这个构建就是这一档，常量。
pub fn host_kind() -> HostKind {
    HostKind::Windows
}

/// **这台机器现在的事实**（§2.5.0 第 2 步）：只报"关掉的位"，其余按档位上限算开。
///
/// 每一个关掉的位都有定义者（§2.5.4），且定义者就是"真的把这条路打开的那段代码"：
///
/// | 位 | 定义者 |
/// | --- | --- |
/// | `program_tap` | `vox_audio_win::process_loopback_available()`（build ≥ 20348） |
/// | `virtual_mic` | `vox_audio_win::cable::detect()`（VB-CABLE 装没装） |
/// | `captions` | `overlay::start` 的返回值 × `overlay_running()`（[`super::captions_status`]） |
/// | `global_hotkey` | `assemble()` 里 `start_hotkeys` 的 `Ok`/`Err` |
/// | `tray` | `tray::can_hide_to_tray()`（图标装上 × 有宿主） |
/// | `background_service` | `events::sync_autostart` 的返回值（[`super::background_service_status`]） |
/// | `vr_captions` | 这个构建编没编 feature × `vr_overlay::status()`（[`vr_captions_status`]） |
///
/// `mic` 这一位**不在这里关门**——今天**没有定义者**，于是按档位上限报开。理由：
/// 装配期没有任何否定证据。此刻一条采集流都没开，"麦克风被别的程序独占"只有真去开流
/// 才知道；为了探测而先开一条流本身就是抢用户设备的动作（系统录音指示灯会亮）。
/// **代价**：界面不会提前把麦克风那一块灰掉，被独占时的失败推迟到用户按"对外说话"
/// 那一刻，由起流的 `PortError` → 界面 `Notice` 兜底（`off[mic] = busy` 要等有可靠的
/// 占用信号再接，§2.5.4）。
///
/// 关掉的位**必须**在档位上限里，否则芯会当成外壳 bug 报一条 `Notice`（上限是硬的）。
pub fn host_facts() -> HostFacts {
    let mut off = BTreeMap::new();

    if !vox_audio_win::process_loopback_available() {
        off.insert(Capability::ProgramTap, UnavailableReason::Unsupported);
    }
    if let Some(reason) = virtual_mic_ensure().reason {
        off.insert(Capability::VirtualMic, reason);
    }
    // 悬浮字幕窗：装配期起没起来（`overlay::start`）× 此刻窗口还在不在（`OVERLAY` 句柄）。
    if let Some(reason) = super::captions_status().reason {
        off.insert(Capability::Captions, reason);
    }
    // 头显字幕：没编 feature 是 `not_built`；编了就得看 `vr_overlay::start` 那条路
    // 真的把 Overlay 连上没有（与 `lib.rs` 里起 overlay 的那处是同一个 `cfg`）。
    if let Some(reason) = vr_captions_status().reason {
        off.insert(Capability::VrCaptions, reason);
    }
    if !super::hotkeys_up() {
        // 装配层起热键失败（`start_hotkeys` 返回 `Err`）。
        off.insert(Capability::GlobalHotkey, UnavailableReason::Permission);
    }
    if !crate::tray::can_hide_to_tray() {
        // 图标没装上（通知区域永远在，所以"有宿主"这一半恒真）。
        off.insert(Capability::Tray, UnavailableReason::Unsupported);
    }
    // 开机自启：注册这条路通不通（查不到状态 → `unsupported`，写不进 → `permission`）。
    if let Some(reason) = super::background_service_status().reason {
        off.insert(Capability::BackgroundService, reason);
    }

    HostFacts {
        host: host_kind(),
        off,
        // Windows 的虚拟麦是 VB-CABLE 驱动提供的端点，用户在设置里自己选
        // （`settings.output_device`），所以这里**恒 `None`**，不做缺省解析。
        virtual_mic_device: None,
    }
}

/// `vr_captions` 现在该不该开着（§2.5.4）。
///
/// 两位输入：**这个构建编没编进去** × **`vr_overlay::start` 那条路真的把 Overlay 连上没连上**。
/// 只看 `cfg` 是不够的——那正是 ★1 的老毛病：没有 SteamVR / 没有头显的机器上，"编了"就被
/// 报成"能用"，用户点开一个永远不会亮的开关。
///
/// 判据本体在 [`super::vr_captions_status_for`]（纯函数，任何平台都能单测）；
/// 这里只负责把 feature 这一位与那三个探针喂进去。
pub fn vr_captions_status() -> CapabilityStatus {
    #[cfg(feature = "steamvr-overlay")]
    {
        // 定义者 = `vr_overlay::start` 之后那条路的状态：线程起来了 × `Backend::connect()`
        // 真的建出 Overlay 了（`CONNECTED`）。没连上时按原因分：运行期/HMD 不在 →
        // `unsupported`，都在但还没连上 → `busy`。
        crate::vr_overlay::status()
    }
    #[cfg(not(feature = "steamvr-overlay"))]
    {
        // 这个构建没编进 `vr_overlay`（那个模块整个不存在，三个探针取不到），
        // 判据第一条就短路成 `not_built`；后面三个 `false` 是占位，不是事实。
        super::vr_captions_status_for(false, false, false, false)
    }
}

/// 虚拟麦这一位：Windows 上设备由 VB-CABLE 驱动提供，**只读探测，不建任何节点**。
pub fn virtual_mic_ensure() -> CapabilityStatus {
    match vox_audio_win::cable::detect() {
        vox_audio_win::CableStatus::Installed => CapabilityStatus::ON,
        vox_audio_win::CableStatus::NotInstalled => {
            CapabilityStatus::off(UnavailableReason::NotInstalled)
        }
        vox_audio_win::CableStatus::InstalledPendingReboot
        | vox_audio_win::CableStatus::UninstallIncomplete => {
            CapabilityStatus::off(UnavailableReason::PendingReboot)
        }
    }
}

/// Windows 上没有"退出时删节点"这回事：设备是驱动提供的，不是我们建的。
pub fn virtual_mic_shutdown() {}

pub fn virtual_device_status() -> VirtualDeviceStatus {
    let status = match vox_audio_win::cable::detect() {
        vox_audio_win::CableStatus::Installed => "installed",
        vox_audio_win::CableStatus::InstalledPendingReboot => "install_pending_reboot",
        vox_audio_win::CableStatus::UninstallIncomplete => "uninstall_incomplete",
        vox_audio_win::CableStatus::NotInstalled => "not_installed",
    };
    let multichannel = match vox_audio_win::multichannel_endpoint_status() {
        vox_audio_win::MultichannelEndpointStatus::Enabled => "visible",
        vox_audio_win::MultichannelEndpointStatus::Disabled => "hidden",
        vox_audio_win::MultichannelEndpointStatus::NotPresent => "absent",
    };
    VirtualDeviceStatus {
        status,
        multichannel_status: multichannel,
    }
}

/// 启动时要提醒用户的事。Windows 上没有额外前置检查（VB-CABLE 缺失由界面引导）。
pub fn startup_notes() -> Vec<String> {
    Vec::new()
}

/// 有没有东西在显示托盘图标。
///
/// Windows 的通知区域永远在，所以恒为 `true`：托盘真的建不起来只可能是我们自己的
/// 错，那种情况由 `tray::install()` 的 `Err` 体现，不用在这里判。
pub fn tray_host_available() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `vr_captions` 与定义者一致（间接形式）：本用例跑在 Windows 上，真起 OpenVR 需要
    /// 装了 SteamVR 的机器，所以只钉两件能钉的——**没编 feature 必须 `not_built`**，
    /// **编了但 OpenVR 探针说没有必须 `unsupported` 且如实报关**。后者正是 ★1 要修的
    /// "产品路径上恒报开"。
    #[test]
    fn vr_captions_bit_follows_the_build_and_the_openvr_probe() {
        #[cfg(not(feature = "steamvr-overlay"))]
        {
            assert_eq!(
                vr_captions_status(),
                CapabilityStatus::off(UnavailableReason::NotBuilt)
            );
            assert_eq!(
                host_facts().off.get(&Capability::VrCaptions),
                Some(&UnavailableReason::NotBuilt),
                "没编 feature 就必须报关，界面才不会渲染那个开关"
            );
        }

        #[cfg(feature = "steamvr-overlay")]
        {
            // 没连上 OpenVR 的机器（CI 上就是这种）走这一支。
            if !crate::vr_overlay::hmd_ready() {
                assert_eq!(
                    vr_captions_status(),
                    CapabilityStatus::off(UnavailableReason::Unsupported),
                    "没有 SteamVR 运行期 / 没有头显就不许报开"
                );
                assert_eq!(
                    host_facts().off.get(&Capability::VrCaptions),
                    Some(&UnavailableReason::Unsupported),
                    "位为假必须报关"
                );
            }
        }
    }
}
