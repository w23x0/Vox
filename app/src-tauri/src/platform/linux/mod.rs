//! Linux 侧的平台实现。
//!
//! 现状（P0 收尾）：时钟、密钥库、设备目录已经是真实现；音频采集/播放、全局热键、
//! 悬浮字幕窗还没写（P1/P2/P3）。**没写的部分一律返回明确错误，不静默降级**：
//! 用户按下"对外说话"就该看到"这个平台还没做好"，绝不能让他以为自己已经开麦了。

mod audio;
mod virtual_mic;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use vox_core::capability::{Capability, HostFacts, UnavailableReason};
use vox_core::composition::HostKind;
use vox_core::pipeline::{CaptureFactory, PlaybackFactory};
use vox_core::ports::{Clock, DeviceRegistry, HotkeyHost, PortResult};
use vox_core::runtime::Runtime;
use vox_core::settings::SubtitleSettings;

use super::{GeometryCallback, OverlayFailure, VirtualDeviceStatus};
use crate::state::OverlayHandle;

/// 悬浮窗的具体句柄留一份：装配层 `AppState` 里存的是 trait object，
/// 而关窗/查存活是 `Overlay` 的固有方法。
static OVERLAY: OnceLock<Arc<vox_overlay_linux::Overlay>> = OnceLock::new();

/// 热键监听句柄留一份，退出时显式停（理由同 Windows 侧：不能指望 Drop）。
static HOTKEYS: OnceLock<Arc<vox_input_linux::HotkeyListener>> = OnceLock::new();

/// 启动前短路：只有 Windows 那条 VB-CABLE 默认设备写回走这条路。
///
/// Linux 这边顺手做一件必须**在 GTK 初始化之前**做的事：GNOME 的 Wayland 会话下
/// GTK 客户端不能自定坐标、不能置顶（协议层就没有），而 XWayland 下两样都成立
/// （实测见 `docs/platform/LINUX.md` §2.3）。所以 Wayland 会话里把整个应用切到
/// X11 后端；已经有 `DISPLAY` 才切，没有就保持原样（那样悬浮窗会由合成器摆位）。
pub fn pre_main() -> bool {
    let wayland = std::env::var("XDG_SESSION_TYPE")
        .map(|value| value.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    let has_x11 = std::env::var_os("DISPLAY").is_some();
    if wayland && has_x11 && std::env::var_os("GDK_BACKEND").is_none() {
        // SAFETY：单线程启动早期设置环境变量，GTK 还没初始化，也没有别的线程读它。
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
        tracing::info!("Wayland 会话：切到 X11 后端（XWayland），悬浮窗才能定位置顶");
    }
    false
}

pub fn clock() -> Arc<dyn Clock> {
    // `LocalClock` 与无屏档共用同一份（`vox_host::clock`，S4-A W3）：`Instant` 单调 +
    // `chrono::Local` 日期，两件事分开取。
    vox_host::clock::local_clock()
}

pub fn alert(title: &str, body: &str) {
    // 没有 MessageBoxW：发布构建在 Linux 上是从终端或 .desktop 起的，stderr 看得见。
    eprintln!("{title}\n{body}");
    tracing::error!("{title}：{body}");
}

/// 哪一档宿主。Linux 桌面：这个构建就是这一档，常量。
pub fn host_kind() -> HostKind {
    HostKind::LinuxDesktop
}

/// **这台机器现在的事实**（§2.5.0 第 2 步）：只报"关掉的位"+ 虚拟麦设备名。
///
/// 每一个关掉的位都有定义者（§2.5.4），且定义者就是"真的把这条路打开的那段代码"：
///
/// | 位 | 定义者 |
/// | --- | --- |
/// | `program_tap` | `vox_audio_linux::pipewire_available()`（PipeWire 在不在） |
/// | `virtual_mic` | [`virtual_mic::ensure()`] 真的建出来的那个句柄 |
/// | `captions` | `overlay::start` 的返回值 × `overlay_running()`（[`super::captions_status`]） |
/// | `global_hotkey` | `assemble()` 里 `start_hotkeys` 的 `Ok`/`Err` |
/// | `tray` | `tray::can_hide_to_tray()`（图标装上 × D-Bus 上有 StatusNotifier 宿主） |
/// | `background_service` | `events::sync_autostart` 的返回值（[`super::background_service_status`]） |
/// | `soft_aec` | **还没有定义者** → [`UnavailableReason::NotWired`]（见下面那行代码的注释） |
///
/// `mic` 这一位**不在这里关门**——今天**没有定义者**，于是按档位上限报开。理由：
/// 装配期没有任何否定证据（此刻一条采集流都没开，"麦克风被别的程序独占"只有真去开流
/// 才知道），而且这一位**不因为 PipeWire 连不上就翻假**（§2.5.1 的 Linux 列）：连不上
/// 会话总线时启动期只发一条 `Notice::warning`，真正抓不到声音是起流时的事。
/// **代价**：界面不会提前把麦克风那一块灰掉，被独占时的失败推迟到用户按"对外说话"
/// 那一刻，由起流的 `PortError` → 界面 `Notice` 兜底（`off[mic] = busy` 要等有可靠的
/// 占用信号再接，§2.5.4）。
/// 桌面档向采集侧要的那个率（`AEC_RATES` 里我们真正想要的一档）。
///
/// PipeWire 按设备协商出真实采集率，那一步在起流之后；B3 只问"这个率在不在支持集里"，
/// 真实协商结果的兜底在链上（`AecSkip::Rate`）。别拿 `cloud::OUTPUT_SAMPLE_RATE`
/// （播放率 24 kHz）来问 AEC——AEC 跑在**采集率**上，24 kHz 不在 `AEC_RATES` 里，
/// 拿它问会把 B4 误报成 `not_wired`（2026-10-12 踩过）。
const DESKTOP_CAPTURE_RATE: u32 = 48_000;

/// `soft_aec` 为什么开不了（`docs/plans/S4-C-AEC.md` §4.3 的 B1–B4）。全过 ⇒ `None` = 位开。
///
/// **B1 恒成立**：AEC 是无条件依赖，没有 `soft-aec` 这个 cargo feature。
/// **B4**：拿 [`DESKTOP_CAPTURE_RATE`] 把工厂真的调起来一次（`AecFactory` 是
/// `Fn(u32) -> PortResult<..>`，所以是"先取工厂、再用率调它"）。起不来 ⇒ `not_wired`。
/// **B2**：缺省采集口与缺省播放口的**卡名段**相同；认不出 ⇒ 不成立。
/// **B3**：[`DESKTOP_CAPTURE_RATE`] 在 `AEC_RATES` 里。
///
/// 这条是**纯函数**：两条节点名进、reason 出。`host_facts()` 只负责探测，于是单测与
/// "本机接了什么声卡"无关。
fn soft_aec_reason(
    source_node: Option<&str>,
    sink_node: Option<&str>,
) -> Option<UnavailableReason> {
    // B4。PipeWire 不在时不等于是"工厂坏了"——那是另一件事（装配层会另报），
    // 这里只回答"这个构建能不能造出 AEC"。
    if vox_audio_linux::pipewire_available()
        && vox_dsp::ports::aec_factory()(DESKTOP_CAPTURE_RATE).is_err()
    {
        return Some(UnavailableReason::NotWired);
    }
    let same_card = source_node
        .and_then(vox_audio_linux::card_key_of_node)
        .zip(sink_node.and_then(vox_audio_linux::card_key_of_node))
        .is_some_and(|(capture, playback)| capture == playback);
    if !same_card {
        return Some(UnavailableReason::Unsupported);
    }
    if !vox_core::pipeline::AEC_RATES.contains(&DESKTOP_CAPTURE_RATE) {
        return Some(UnavailableReason::Unsupported);
    }
    None
}

pub fn host_facts() -> HostFacts {
    let mut off = BTreeMap::new();

    if !vox_audio_linux::pipewire_available() {
        off.insert(Capability::ProgramTap, UnavailableReason::Unsupported);
    }
    // 设备轮询每个 tick 都会走到这里：顺手复核虚拟麦节点还在不在（掉了就如实翻假）。
    let virtual_mic = virtual_mic::recheck();
    if let Some(reason) = virtual_mic.reason {
        off.insert(Capability::VirtualMic, reason);
    }
    // 悬浮字幕窗：装配期起没起来（`overlay::start`）× 此刻窗口还在不在（`OVERLAY` 句柄）。
    if let Some(reason) = super::captions_status().reason {
        off.insert(Capability::Captions, reason);
    }
    if !super::hotkeys_up() {
        // 少了 `input` 组（`start_hotkeys` 的错误文案里有 `usermod -aG input`）。
        off.insert(Capability::GlobalHotkey, UnavailableReason::Permission);
    }
    if !crate::tray::can_hide_to_tray() {
        off.insert(Capability::Tray, UnavailableReason::Unsupported);
    }
    // 开机自启（freedesktop 的 `~/.config/autostart/*.desktop`）：查不到状态 → `unsupported`，
    // 写不进 → `permission`。
    if let Some(reason) = super::background_service_status().reason {
        off.insert(Capability::BackgroundService, reason);
    }
    // `vr_captions` 不在这里报：它在 Linux 档位的上限之外（`host_ceiling`），塞进 `off`
    // 会被芯当成外壳 bug。这一位的判定在 `platform/win.rs::vr_captions_status`（它只在
    // Windows 上编），只有 Windows 的 `host_facts()` 读它。
    //
    // 软件 AEC（`docs/plans/S4-C-AEC.md` §4.3 的 B1–B4）：判据本身是纯函数
    // （`soft_aec_reason`），这里只把**探测到的缺省设备对**喂进去。
    let (source, sink) = vox_audio_linux::default_source_and_sink_nodes().unwrap_or_default();
    if let Some(reason) = soft_aec_reason(source.as_deref(), sink.as_deref()) {
        off.insert(Capability::SoftAec, reason);
    }

    HostFacts {
        host: host_kind(),
        off,
        // 译音往哪送才算虚拟麦：**节点名**，不是界面里那个 description。
        // `PlaybackSink::open` 把它塞进 `target.object`，而 `target.object` 认的是
        // `node.name`——给 description 时请求不解析、流会退到默认输出设备（§4.4 态 B 实测）。
        virtual_mic_device: if virtual_mic.enabled {
            Some(vox_audio_linux::VIRTUAL_MIC_NODE_NAME.to_string())
        } else {
            None
        },
    }
}

/// 虚拟麦接线：把 PipeWire 节点建出来（S0 §3.2 ①）。装配时调一次（主线程）。
pub fn virtual_mic_ensure() -> vox_core::capability::CapabilityStatus {
    virtual_mic::ensure()
}

/// 退出时删掉节点。**排在 `engine.shutdown()` 之后**（见 `lib.rs` 的 `shutdown`）。
pub fn virtual_mic_shutdown() {
    virtual_mic::shutdown();
}

pub fn capture_factory() -> CaptureFactory {
    audio::capture_factory()
}

pub fn playback_factory() -> PlaybackFactory {
    audio::playback_factory()
}

pub fn registry() -> Arc<dyn DeviceRegistry> {
    audio::registry()
}

/// 起热键监听（evdev 直读 `/dev/input`）。
///
/// 权限不足时返回的错误里带着**能照做的命令**（`usermod -aG input`），装配层会把它
/// 翻成界面上的 `Notice`——用户看到的是"怎么修"，而不是按了热键没反应。
pub fn start_hotkeys(runtime: Runtime) -> PortResult<Arc<dyn HotkeyHost>> {
    let listener = vox_input_linux::HotkeyListener::start(
        vox_core::ports::HotkeyBindings::default(),
        Box::new(move |event| runtime.on_hotkey(event)),
    )?;
    let _ = HOTKEYS.set(Arc::clone(&listener));
    Ok(listener)
}

pub fn stop_hotkeys() {
    if let Some(listener) = HOTKEYS.get() {
        listener.stop();
    }
}

/// 起悬浮字幕窗。GTK 只能在主线程建窗，而装配层的 `assemble()` 就在主线程，
/// 所以这里直接建；不是主线程会拿到明确错误（见 `window.rs`）。
pub fn spawn_overlay(
    settings: &SubtitleSettings,
    on_geometry: GeometryCallback,
) -> Result<OverlayHandle, OverlayFailure> {
    // 永久鼠标穿透（`docs/architecture/DECISIONS.md` A5）；位置由设置页改 `subtitle.geometry`，
    // 经 `restyle` 实时搬窗。回调保留（目前无人触发）。
    let overlay =
        vox_overlay_linux::spawn_with_geometry(settings, on_geometry).map_err(|error| {
            OverlayFailure {
                // GTK 建窗只有一条失败路：不在 GTK 主线程上（`vox-overlay-linux/src/window.rs`）。
                // 那是装配层没接对，不是这台机器做不到——`not_wired` 正是为这种中间态准备的。
                reason: UnavailableReason::NotWired,
                error,
            }
        })?;
    let _ = OVERLAY.set(Arc::clone(&overlay));
    Ok(overlay)
}

/// 窗口还活着吗。帧线程靠它发现"窗被关了"。
pub fn overlay_running() -> bool {
    OVERLAY.get().is_some_and(|overlay| overlay.is_running())
}

pub fn shutdown_overlay() {
    if let Some(overlay) = OVERLAY.get() {
        overlay.shutdown();
    }
}

/// GTK 自己就认 `tauri.conf.json` 的 `minWidth` / `minHeight`，不需要 Windows 那套
/// `WM_GETMINMAXINFO` 子类化。P2 真机验收时要确认这一点。
pub fn enforce_min_size(_window: &tauri::WebviewWindow) {}

pub fn virtual_device_status() -> VirtualDeviceStatus {
    // PipeWire 原生就能建虚拟 sink，没有"安装"这一步：前端看到
    // `not_applicable` 就整块隐藏 VB-CABLE 管理页，只留一句"去目标程序里选虚拟麦"。
    // 那句话现在**成立**：节点由 `virtual_mic::ensure()` 在装配时建出来
    // （接线前它是空头支票——系统里根本没有那个设备，S0 §1.4）。
    VirtualDeviceStatus {
        status: "not_applicable",
        multichannel_status: "absent",
    }
}

pub fn startup_notes() -> Vec<vox_core::Msg> {
    let mut notes = Vec::new();
    if !vox_audio_linux::pipewire_available() {
        notes.push(vox_core::Msg::new(vox_core::notice::key::PIPEWIRE_MISSING));
    }
    if !tray_host_available() {
        notes.push(vox_core::Msg::new(vox_core::notice::key::TRAY_MISSING));
    }
    notes
}

/// 有没有东西在显示托盘图标。
///
/// 只问 D-Bus 上那两个标准名字在不在（GNOME 的 AppIndicator 扩展占 `org.kde.…`，
/// KDE 自己那套平时不跑、注册图标时靠 D-Bus 激活拉起来，所以要连"可激活"一起算）。
/// **不能**只看 `TrayIconBuilder::build()` 成没成功：没有宿主时它照样返回 Ok，
/// 图标对象建出来了但没有任何东西会画它——用户关窗之后就再也叫不回界面。
pub fn tray_host_available() -> bool {
    /// StatusNotifier 规范里的宿主名字。KDE 与 Ayatana 两套实现各占一个。
    const WATCHERS: [&str; 2] = [
        "org.kde.StatusNotifierWatcher",
        "org.ayatana.StatusNotifierWatcher",
    ];

    // 拿不到会话总线（比如从 ssh/无桌面环境里跑）就是没有宿主。
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return false;
    };
    let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&connection) else {
        return false;
    };
    if WATCHERS.iter().any(|name| has_owner(&bus, name)) {
        return true;
    }
    bus.list_activatable_names()
        .map(|names| {
            names
                .iter()
                .any(|name| WATCHERS.iter().any(|watcher| name.as_str() == *watcher))
        })
        .unwrap_or(false)
}

fn has_owner(bus: &zbus::blocking::fdo::DBusProxy, name: &str) -> bool {
    let Ok(name) = zbus::names::BusName::try_from(name) else {
        return false;
    };
    bus.name_has_owner(name).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_host_probe_never_panics() {
        // 有宿主（本机 GNOME + AppIndicator 扩展）→ true；没有会话总线（CI 容器）
        // → false。两种都算对，这里只钉住"不许 panic、不许卡住"。
        let _ = tray_host_available();
    }

    #[test]
    fn startup_notes_use_declared_keys() {
        for note in startup_notes() {
            assert!(vox_core::notice::ALL_KEYS.contains(&note.key), "{note:?}");
        }
    }

    /// 真机快照（★1 收口）：四位在这台机器上到底报什么、凭的是什么。
    ///
    /// ```text
    /// cargo test -p vox -- --ignored the_four_star_bits --nocapture
    /// ```
    ///
    /// 三件事一起验：
    ///
    /// - `captions`：**真的去调 Linux 侧的悬浮窗定义者**（[`spawn_overlay`]）。测试进程
    ///   不在 GTK 主线程上，所以它必须返回 `Err(OverlayFailure { not_wired })`——那是
    ///   "装配层没接对"，不是"这台机器做不到"（`unsupported`）。位必须如实报假。
    /// - `virtual_mic`：定义者也真的跑一遍（位为真的唯一凭据是那个句柄），顺带确认
    ///   退出后不留幽灵设备。
    /// - `mic` / `background_service` / `vr_captions`：定义者分别"不存在""跑在
    ///   `assemble()` 里（要 `AppHandle`）""只有 Windows 有"，测试进程里拿不到，于是
    ///   报的正是 fail-closed 的那一档（`mic` 按上限报开、后两位 `not_wired` / 不在
    ///   Linux 上限里）。
    #[test]
    #[ignore = "真机快照：要看这台机器上四位实际报什么"]
    fn the_four_star_bits_on_this_machine() {
        let failure = match spawn_overlay(&SubtitleSettings::default(), Arc::new(|_| {})) {
            Ok(_) => panic!("测试进程不在 GTK 主线程上，建窗本该失败"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason,
            UnavailableReason::NotWired,
            "GTK 主线程之外建窗是装配层的问题，不是这台机器做不到"
        );

        let wired = virtual_mic_ensure();
        let facts = host_facts();
        let runtime = Runtime::new(vox_core::Settings::default(), clock());
        runtime.set_host_facts(facts.clone());
        let report = runtime.capabilities();

        println!("── 四位在真机上的实际取值（本进程：定义者只在 `assemble()` 里跑过才是 ON）──");
        let ceiling = vox_core::capability::host_ceiling(host_kind());
        for (bit, definer) in [
            (
                Capability::Mic,
                "没有定义者（装配期拿不到\"被占\"的否定证据）→ 按档位上限报开",
            ),
            (
                Capability::Captions,
                "`overlay::start` 的返回值 × `overlay_running()`",
            ),
            (
                Capability::BackgroundService,
                "`events::sync_autostart` 的返回值",
            ),
            (
                Capability::VrCaptions,
                "`cfg(feature=\"steamvr-overlay\")` × `vr_overlay::status()`（只有 Windows 有这一位）",
            ),
        ] {
            let status = report.host.get(&bit).copied().expect("宿主位都有条目");
            if ceiling.contains(bit) {
                println!(
                    "{:<20} enabled={:<5} reason={:?}",
                    bit.id(),
                    status.enabled,
                    status.reason
                );
            } else {
                // 上限之外的位外壳根本不报关，报告里那份是芯按上限给的 `unsupported`。
                println!(
                    "{:<20} 不在本档位（linux_desktop）上限里；报告里那份是芯给的 {:?}",
                    bit.id(),
                    status.reason
                );
            }
            println!("{:<20} 定义者 = {}", "", definer);
        }
        println!("── 顺带：虚拟麦 ──");
        println!(
            "virtual_mic          enabled={} device={:?}",
            wired.enabled, facts.virtual_mic_device
        );
        println!(
            "── 快照（`Snapshot.capabilities` 那一格）──\n{}",
            serde_json::to_string_pretty(&report).expect("报告可序列化")
        );

        virtual_mic_shutdown();
        assert!(
            !vox_audio_linux::VirtualSink::exists().expect("查不到 PipeWire 图"),
            "退出后不许留幽灵设备"
        );
    }

    /// 真机验收（S0 §4.4 态 B）：装配层真的把虚拟麦接上了。
    ///
    /// 一路走到黑：`ensure()` 真的建出节点 → 事实里设备名非空、这一位不报关 →
    /// 注入账本后快照里那位开着 → 退出后系统里不留幽灵设备。
    ///
    /// ```text
    /// cargo test -p vox -- --ignored virtual_mic_is_wired --nocapture
    /// ```
    #[test]
    #[ignore = "需要真机上跑着 PipeWire"]
    fn virtual_mic_is_wired_on_this_machine() {
        let status = virtual_mic_ensure();
        assert!(
            status.enabled,
            "虚拟麦这一位必须为 ON：{status:?}（建不起来时先看 PipeWire 在不在）"
        );
        assert!(
            vox_audio_linux::VirtualSink::exists().expect("查不到 PipeWire 图"),
            "位说 ON，系统里就必须真的有这个设备"
        );

        let facts = host_facts();
        assert_eq!(
            facts.virtual_mic_device.as_deref(),
            Some(vox_audio_linux::VIRTUAL_MIC_NODE_NAME),
            "译音往哪送：事实里报的必须是节点名"
        );
        assert!(!facts.off.contains_key(&Capability::VirtualMic));

        let runtime = Runtime::new(vox_core::Settings::default(), clock());
        runtime.set_host_facts(facts);
        let report = runtime.capabilities();
        assert!(
            report.host_enabled(Capability::VirtualMic),
            "快照里的位必须跟着事实走：{report:?}"
        );
        assert_eq!(report.tier, HostKind::LinuxDesktop);

        println!(
            "{}",
            serde_json::to_string_pretty(&runtime.snapshot().capabilities).expect("报告可序列化")
        );

        virtual_mic_shutdown();
        assert!(
            !vox_audio_linux::VirtualSink::exists().expect("查不到 PipeWire 图"),
            "退出后不许留幽灵设备"
        );
    }

    /// `soft_aec` 的判据（B1–B4）是**纯函数**：两条 PipeWire 节点名进、reason 出。
    /// 所以这条与"本机接了什么声卡"无关，换台机器也是同一个答案。
    #[test]
    fn soft_aec_needs_both_devices_on_one_card() {
        let builtin_in = Some("alsa_input.pci-0000_00_1f.3.analog-stereo");
        let builtin_out = Some("alsa_output.pci-0000_00_1f.3.analog-stereo");
        let hdmi_out = Some("alsa_output.pci-0000_01_00.1.hdmi-stereo");
        let usb_in = Some("alsa_input.usb-Generic_USB_Audio-00.pro-input-0");

        // ① 同卡 ⇒ 位开（笔记本内置 codec：麦和喇叭本来就在一张卡上）。
        assert_eq!(soft_aec_reason(builtin_in, builtin_out), None);
        // ② 不同卡 ⇒ 认得出但不是同一张 ⇒ `unsupported`（"这台设备做不到"）。
        assert_eq!(
            soft_aec_reason(builtin_in, hdmi_out),
            Some(UnavailableReason::Unsupported)
        );
        assert_eq!(
            soft_aec_reason(usb_in, builtin_out),
            Some(UnavailableReason::Unsupported)
        );
        // ③ 认不出 ⇒ 同样 `unsupported`，**不许**因为"看不出问题"就放行。
        //    （反例：虚拟麦那个监视节点没有方向段，`card_key_of_node` 给 `None`。）
        assert_eq!(
            soft_aec_reason(Some("vox_virtual_mic.monitor"), builtin_out),
            Some(UnavailableReason::Unsupported)
        );
        assert_eq!(
            soft_aec_reason(None, None),
            Some(UnavailableReason::Unsupported)
        );
    }

    /// 桌面档的位**不许永远是 `not_wired`**（`not_wired` = 这个版本还没接通；报它是因为
    /// 判据不在，不是因为判据判不出来）。这一条钉住"判据真的接了"：B4 那条路现在是活的，
    /// 所以职责只能是 B2/B3 那几种 `unsupported`，或者干脆位开。
    #[test]
    fn soft_aec_is_judged_not_just_declared_unwired() {
        for (source, sink) in [
            (
                Some("alsa_input.pci-0000_00_1f.3.analog-stereo"),
                Some("alsa_output.pci-0000_00_1f.3.analog-stereo"),
            ),
            (
                Some("alsa_input.usb-A.pro-input-0"),
                Some("alsa_output.pci-0000_01_00.1.hdmi-stereo"),
            ),
            (None, None),
            (
                Some("vox_virtual_mic.monitor"),
                Some("alsa_output.pci-0000_00_1f.3.analog-stereo"),
            ),
        ] {
            assert_ne!(
                soft_aec_reason(source, sink),
                Some(UnavailableReason::NotWired),
                "桌面档今天没有'还没接通'这回事了——B4 是活的，剩下的只能是判据判出来的"
            );
        }
    }
}
