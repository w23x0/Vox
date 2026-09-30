//! 平台分派：装配层只跟这一层打交道。
//!
//! `lib.rs` 的装配流程本身是平台无关的——建 Runtime、注端口、起线程、挂事件桥。
//! 平台差异全部收在这里：
//!
//! | 能力 | Windows | Linux |
//! | --- | --- | --- |
//! | 时钟 | `GetLocalTime` | `chrono::Local`（`vox_host::clock::LocalClock`，与无屏档共用） |
//! | 密钥库 | DPAPI 加密落盘 | Secret Service（`keyring`） |
//!
//! **"选哪一个"的决策留在这一层，机制在 `vox_host::secrets`**（S4-A W3/W5）：`lib.rs` 的
//! `secret_backend` 底下是 `SecretBackend::{Dpapi, SecretService}`，加解密与权限位都在共享层，
//! 本 crate 只留"这一档挑哪个"。
//! | 启动期致命提示 | `MessageBoxW` | stderr + 日志 |
//! | 音频三件套 | WASAPI（`vox-audio-win`） | PipeWire（`vox-audio-linux`） |
//! | 全局热键 | `GetAsyncKeyState` 轮询 | evdev |
//! | 悬浮字幕窗 | Win32 分层窗 | GTK + XWayland |
//! | 虚拟麦克风 | VB-CABLE（要装） | PipeWire sink（原生，不需要装） |
//! | 托盘宿主 | 通知区域永远在 → `true` | 问 D-Bus 有没有 StatusNotifier 宿主 |
//!
//! 两边都实现同一组函数，`lib.rs` 不写 `#[cfg]`。
//!
//! 其中两个函数是 S0 的**宿主档位 / 宿主事实**入口（`docs/plans/S0-COMPOSITION-MANIFEST.md`
//! §2.5.0 第 1–2 步）：
//!
//! - [`host_kind()`]：**声明自己是哪一档**（同一份构建里是常量）。档位决定查 `host_ceiling`
//!   的哪一行，也决定一份清单该不该装在这台机器上。
//! - [`host_facts()`]：**报这台机器现在的事实**——只报"关掉的位"+ 虚拟麦设备名。位由芯算
//!   （档位上限 − 关掉的），外壳不许自带一张上限表（§2.5.2）。
//!
//! 还有虚拟麦三件（`virtual_mic_ensure` / `virtual_mic_shutdown` 由装配流程调，
//! 设备名跟着 `HostFacts.virtual_mic_device` 走）：Windows 侧只读探测 VB-CABLE，
//! **不建任何节点**（设备由驱动提供）；Linux 侧真的把 PipeWire 节点建出来，
//! **位为真的唯一凭据就是这个句柄**（§3.2 ①）。
//!
//! 托盘那一行是后加的：`TrayIconBuilder::build()` 在"没人显示托盘"的环境下照样返回
//! Ok（GNOME 默认就是这样），所以"建成功"不足以决定关窗要不要 `hide()`。

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

use vox_core::capability::{CapabilityStatus, UnavailableReason};
use vox_core::ports::PortError;

// ── 装配期观测到的结果（`host_facts()` 要读，但只有装配流程自己知道） ──────────
//
// 位为 `true` 必须由"真的把这条路打开的那段代码"负责（S0 §2.5.4 的 R6）。有三条路的
// 开关落在 `assemble()` 的某一步里，平台实现看不见，所以那几步把结果记在这里，
// `host_facts()` 读它：
//
// - 热键：`start_hotkeys` 的 `Ok`/`Err`（[`record_hotkeys`]）；
// - 悬浮字幕窗：`overlay::start` 的返回值（[`record_captions`]）；
// - 开机自启：`events::sync_autostart` 的返回值（[`record_background_service`]）。
//
// 缺省一律是"还没起"（fail-closed）：**还没起就是没起**，宁可报假，不许"位说能用、
// 按下去没反应"。装配流程里每一次 `record_*` 都排在注入事实之前。
//
// 槽是进程级的，而且 `host_facts()` 每 4 秒被 `devices.rs` 的轮询线程读一次
// （§2.6 R7），所以定义者再跑一次、`record_*` 再写一次，位最迟 4 秒内跟上。

/// 热键监听起没起来。`assemble()` 的 `start_hotkeys` 之后写。
static HOTKEYS_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `assemble()` 记下热键那一步的结果。
pub(crate) fn record_hotkeys(started: bool) {
    HOTKEYS_UP.store(started, std::sync::atomic::Ordering::Relaxed);
}

/// 热键这一位现在该不该开着。
pub(crate) fn hotkeys_up() -> bool {
    HOTKEYS_UP.load(std::sync::atomic::Ordering::Relaxed)
}

/// 一位的装配期观测：`None` = 定义者还没跑。
///
/// 只给"平台实现看不见、只有装配流程自己知道"的位用。没观测到时报
/// [`UnavailableReason::NotWired`]：实现是有的，只是装配层还没把这段路接上——
/// 这正是 `not_wired` 的定义（§2.5.4）。
struct ObservedBit(parking_lot::Mutex<Option<CapabilityStatus>>);

impl ObservedBit {
    const fn new() -> Self {
        Self(parking_lot::Mutex::new(None))
    }

    /// 定义者跑完，把结果记下。可以反复调：后来的覆盖先前的。
    fn record(&self, status: CapabilityStatus) {
        *self.0.lock() = Some(status);
    }

    /// 现在读到的值。`CapabilityStatus` 是 `Copy`，这里读一份快照走。
    fn status(&self) -> CapabilityStatus {
        self.0
            .lock()
            .unwrap_or(CapabilityStatus::off(UnavailableReason::NotWired))
    }
}

/// `captions` 的装配期观测：`overlay::start` 的结果。
static CAPTIONS: ObservedBit = ObservedBit::new();

/// `background_service` 的装配期观测：`events::sync_autostart` 的结果。
static BACKGROUND_SERVICE: ObservedBit = ObservedBit::new();

/// 单测用的串行锁。
///
/// 观测槽是**进程级**的：写槽的用例（定义者那几条）与"前后两次 `host_facts()` 该一致"
/// 的用例（`devices::tests`）并行跑就会互相打架——那是测试并行造的假象，不是产品的
/// 竞态（产品里只有定义者会写槽，写完整份事实会被 4 秒轮询重新注入）。
/// 所有碰槽的用例都先拿这把锁。
#[cfg(test)]
pub(crate) static OBSERVED_BIT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// `assemble()` 记下 `overlay::start` 的结果（悬浮窗 + 字幕帧线程都起来了才是 ON）。
pub(crate) fn record_captions(status: CapabilityStatus) {
    CAPTIONS.record(status);
}

/// `captions` 这一位的判据，**纯函数**（§2.5.4）：装配期结论 × 活体探针 → 位。
///
/// 抽成纯函数的理由有两个：
///
/// 1. **能被单测钉住**——"定义者结论 → 位"这一跳（哪一种输入报哪一个 reason）必须由
///    单测直接喂四种组合（起/没起 × 活/没了）钉死，而不是靠"起个真窗看看"；
/// 2. **判据里不许有线程相关的东西**——主线程与 `devices.rs` 的 4 秒轮询线程读同一位
///    必须得到同一个结论。第七轮 D2：Linux 上的活体探针曾经读的是**建窗线程的
///    `thread_local`**，于是同一位在两条线程上给出两个答案（轮询线程恒 `off(busy)`，
///    位启动约 4 秒后翻假，而 reason 说"设备被别的程序占着"，跟真实原因根本不搭）。
///
/// 两段证据缺一不可：`started` 说"装配期真的把窗和帧线程都拉起来了"，`running` 说
/// "此刻窗还在"。`started` 为假时**reason 原样带走**——活体探针只回答"窗还在不在"，
/// 回答不了"当初为什么没起来"，不许把它改写成 `busy`。
pub(crate) fn captions_status_for(started: CapabilityStatus, running: bool) -> CapabilityStatus {
    if !started.enabled {
        return started;
    }
    if running {
        CapabilityStatus::ON
    } else {
        CapabilityStatus::off(UnavailableReason::Busy)
    }
}

/// `captions` 现在该不该开着（§2.5.4）。
///
/// 定义者是"悬浮字幕窗真的起来了"，两段证据缺一不可：
///
/// 1. 装配期 `overlay::start` 的结果（[`record_captions`] 记的那份）；
/// 2. 此刻窗口还在不在（[`overlay_running()`]——帧线程发现"窗没了"用的是同一个探针）。
///
/// 合成规则在 [`captions_status_for`] 里（纯函数，可单测）。
/// 窗口没了就没人画字幕，位必须跟着翻假：报 `busy`（这条路还在，只是现在走不通），
/// 不报 `unsupported`（这台机器明明能画）。
pub(crate) fn captions_status() -> CapabilityStatus {
    captions_status_for(CAPTIONS.status(), overlay_running())
}

/// `vr_captions` 这一位的判据，**纯函数**（§2.5.4）：四件事实 → 位。
///
/// - `built`：这个构建**编没编** `steamvr-overlay` feature。没编 → `not_built`
///   （界面据此不渲染那个开关；后三个参数那时只是占位，判据第一条就短路了）；
/// - `thread_up`：`vr_overlay::start` 那条线程起没起来。没起来 = 装配层没把这段路接上
///   → `not_wired`；
/// - `connected`：OpenVR Overlay 真的连上了没（`Backend::connect()` 成功才置位）→ `ON`；
/// - `hmd_ready`：没连上时按"这台机器有没有 SteamVR 运行期与头显"分：都在但还没连上
///   → `busy`，缺一个 → `unsupported`（不是"重试一下就好"，是这台机器做不了）。
///
/// 判据本身**平台中立**（四个 `bool` → 一个位），所以放在这个共享分派文件里，让它在
/// **任何平台**上都能被单测钉住；Windows 专有的只是"那三个探针怎么读"
/// （`vr_overlay::{RUNNING, CONNECTED, hmd_ready}`，那个模块整个
/// `#[cfg(all(windows, feature = "steamvr-overlay"))]`，Linux 上连编都编不到）。
///
/// `cfg`：Windows 上是产品代码（两种 feature 配置下都有调用方）；别的平台上只为单测
/// 而编译（那边没有调用方，不 `cfg` 掉会响 `dead_code`）——同一份源码文本就是 Windows
/// 构建里跑的那一份。
#[cfg(any(windows, test))]
pub(crate) fn vr_captions_status_for(
    built: bool,
    thread_up: bool,
    connected: bool,
    hmd_ready: bool,
) -> CapabilityStatus {
    if !built {
        return CapabilityStatus::off(UnavailableReason::NotBuilt);
    }
    if !thread_up {
        return CapabilityStatus::off(UnavailableReason::NotWired);
    }
    if connected {
        return CapabilityStatus::ON;
    }
    if hmd_ready {
        CapabilityStatus::off(UnavailableReason::Busy)
    } else {
        CapabilityStatus::off(UnavailableReason::Unsupported)
    }
}

/// `events::sync_autostart` 记下自启那一步的结果（§2.5.4 的 `background_service`）。
pub(crate) fn record_background_service(status: CapabilityStatus) {
    BACKGROUND_SERVICE.record(status);
}

/// `background_service` 现在该不该开着。
///
/// 定义者 = 自启注册这条路通不通（`sync_autostart` 的返回值）：注册状态查得到、
/// 要写也写得进去才是 ON。**用户把自启开关关掉不会让这一位翻假**——位是"能不能"，
/// 开关是"要不要"（§2.6 R5）。
pub(crate) fn background_service_status() -> CapabilityStatus {
    BACKGROUND_SERVICE.status()
}

/// 虚拟麦克风在界面上要显示的状态。
///
/// 三个平台事实不同，字段就按"界面要显示什么"定，而不是按谁的系统 API 长什么样：
/// - Windows 上 VB-CABLE 是需要用户**安装**的第三方驱动，所以有"装了没/要不要重启"；
/// - Linux 上 PipeWire 原生就能建虚拟 sink，没有"安装"这一步，状态恒为 `not_applicable`。
pub struct VirtualDeviceStatus {
    /// 前端认的值：`installed` / `install_pending_reboot` / `uninstall_incomplete` /
    /// `not_installed` / `not_applicable`（Linux）。`not_applicable` 时界面应当
    /// 整块隐藏 VB-CABLE 管理页。
    pub status: &'static str,
    pub multichannel_status: &'static str,
}

/// 用户在悬浮窗上拖完/缩放完，把新几何回写设置时用。
pub type GeometryCallback =
    std::sync::Arc<dyn Fn(vox_core::settings::OverlayGeometry) + Send + Sync + 'static>;

/// 悬浮窗起不来：给人看的错误 + 给能力位用的原因（§2.5.4 的 `captions`）。
///
/// 两个平台失败的含义不同，只有平台实现自己分得清，所以由它填：
/// - Windows：连分层窗都建不出来 = 这台机器给不了我们窗口 → `unsupported`；
/// - Linux：GTK 建窗只有"不在 GTK 主线程"这一条失败路 = 装配层没接对 → `not_wired`。
pub struct OverlayFailure {
    pub reason: UnavailableReason,
    pub error: PortError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::capability::{host_ceiling, Capability, CapabilityStatus, UnavailableReason};
    use vox_core::runtime::Runtime;
    use vox_core::Settings;

    /// 事实 → 位 → 快照：外壳报什么，芯算出来的位与快照就必须是什么。
    ///
    /// 这条同时钉住 §2.5.0 的两条不变量：
    ///
    /// - **上限是硬的**：外壳不许报档位上限之外的位（报了就是外壳 bug，芯会发 `Notice`）；
    /// - **位只由事实决定**：关掉的位在报告与快照里都得关着、`reason` 一字不差；
    ///   没关的位在报告里必须是开的。
    ///
    /// 不钉具体哪几位是开是关——那些是**这台机器**的事实（有没有 PipeWire、装没装
    /// VB-CABLE、托盘有没有宿主），换个环境就该换值。
    #[test]
    fn host_facts_decide_the_bits_and_the_snapshot() {
        let facts = host_facts();
        assert_eq!(
            facts.host,
            host_kind(),
            "档位由外壳声明，报告里的 tier 就是它"
        );
        assert!(
            facts.excess_off_bits().is_empty(),
            "外壳报了档位上限之外的位：{:?}",
            facts.excess_off_bits()
        );

        let runtime = Runtime::new(Settings::default(), clock());
        runtime.set_host_facts(facts.clone());
        let report = runtime.capabilities();
        assert_eq!(report.tier, host_kind());

        for bit in host_ceiling(host_kind()).iter() {
            let status = *report.host.get(&bit).expect("每个宿主位都要有条目");
            assert!(status.is_consistent(), "{bit:?} 的状态自相矛盾：{status:?}");
            match facts.off.get(&bit) {
                Some(reason) => {
                    assert!(!status.enabled, "{bit:?} 事实里报了关，位就必须是关的");
                    assert_eq!(status.reason, Some(*reason), "{bit:?} 的 reason 走样了");
                }
                None => assert!(status.enabled, "{bit:?} 事实里没报关，位就该是开的"),
            }
        }

        // 快照里那一份就是同一个值（§2.5.0 第 5 步的唯一出口）。
        assert_eq!(runtime.snapshot().capabilities, report);

        // 设备名跟着位走：位为假时不许给出设备名——"位说 ON、设备不存在"正是 §1.4 的老毛病。
        assert_eq!(
            facts.virtual_mic_device.is_some(),
            report.host_enabled(Capability::VirtualMic)
        );
    }

    // ── 定义者（§2.5.4）：位为真必须有"真的把这条路打开的那段代码"负责 ──────────
    //
    // 真造出否定情形需要真起悬浮窗 / 真注册自启 / 真插头显，单测里造不出来。所以这四条
    // 走**定义者的判据**（`overlay::captions_outcome` / `events::autostart_status` /
    // `vr_captions_status_for` 这些纯函数）→ 观测槽 → `host_facts()` 这条全链：
    // 判据改成恒 `ON`、或把位改成不看槽，都会在这里翻红（第七轮 D3 要的就是这个：
    // 那时"判定"散在 `start` / `sync_autostart` 的函数体里，单测看不到它）。

    /// `captions` 跟着定义者的结论走，而不是按上限恒报开、也不跟着建窗之外的东西走。
    #[test]
    fn captions_bit_follows_the_overlay_start_result() {
        let _guard = OBSERVED_BIT_LOCK.lock();
        assert!(
            !overlay_running(),
            "这个用例假定进程里没有真悬浮窗（有的话下面的 busy 断言就不成立）"
        );

        // 定义者还没跑过（缺省）→ fail-closed 报 `not_wired`，不是"按上限报开"。
        assert_eq!(
            CAPTIONS.status(),
            CapabilityStatus::off(UnavailableReason::NotWired)
        );

        // 否定情形：装配期窗口没起来 → 位必须关，且 reason 一字不差
        // （`unsupported` 是"这台机器给不了我们窗口"，从定义者的结论一路走到报告）。
        record_captions(crate::overlay::captions_outcome(
            Err(UnavailableReason::Unsupported),
            false,
        ));
        assert_eq!(
            host_facts().off.get(&Capability::Captions),
            Some(&UnavailableReason::Unsupported),
            "悬浮窗起不来时位必须关"
        );

        // 定义者说"窗起来了、帧线程也起来了"→ 还差**第二段证据**（此刻窗还在不在，本进程
        // 里没有真窗）→ `busy`：位说 ON 而窗口不在，正是 §1.4 的老毛病。
        record_captions(crate::overlay::captions_outcome(Ok(()), true));
        assert_eq!(
            captions_status(),
            CapabilityStatus::off(UnavailableReason::Busy)
        );
        assert_eq!(
            host_facts().off.get(&Capability::Captions),
            Some(&UnavailableReason::Busy)
        );

        // 还原：同进程里别的用例还要读这份槽（含 `host_facts_decide_the_bits_and_the_snapshot`）。
        record_captions(CapabilityStatus::off(UnavailableReason::NotWired));
    }

    /// `captions` 的判据（纯函数）：两段证据 → 位，四种组合各报什么 —— 逐条钉死。
    ///
    /// **自证**：把 [`captions_status_for`] 改成恒 `ON`（或恒 `busy`、或让活体探针把
    /// `started` 的 reason 吞掉）这条就红。
    #[test]
    fn captions_status_for_maps_the_two_evidence_to_the_bit() {
        // 装配期就没起来：reason 原样带走——活体探针只回答"窗还在不在"，
        // 回答不了"当初为什么没起来"，不许把它改写成 busy。
        for running in [false, true] {
            assert_eq!(
                captions_status_for(
                    CapabilityStatus::off(UnavailableReason::Unsupported),
                    running
                ),
                CapabilityStatus::off(UnavailableReason::Unsupported)
            );
            assert_eq!(
                captions_status_for(CapabilityStatus::off(UnavailableReason::NotWired), running),
                CapabilityStatus::off(UnavailableReason::NotWired)
            );
        }

        // 起来了且窗还在 → 真能画。
        assert_eq!(
            captions_status_for(CapabilityStatus::ON, true),
            CapabilityStatus::ON
        );
        // 起来了但窗没了 → 现在画不出来：`busy`（这条路还在），不是"这台机器做不到"。
        assert_eq!(
            captions_status_for(CapabilityStatus::ON, false),
            CapabilityStatus::off(UnavailableReason::Busy)
        );
    }

    /// 同一位在**所有线程**上必须给出同一个结论（第七轮 D2）。
    ///
    /// `host_facts()` 有两个调用方：装配期的主线程与 `devices.rs` 的 4 秒轮询线程。
    /// 第七轮 Linux 上 `captions` 的活体探针读的是**建窗线程的 `thread_local`**，于是
    /// 同一位在两条线程上得到两个答案（轮询线程恒 `off(busy)`）——产品路径上这一位
    /// 启动约 4 秒后翻假，reason 还写着"设备被别的程序占着"，跟真实原因（字幕帧线程
    /// 读到 `false` 秒退）完全不搭。
    ///
    /// **自证**：判据里一旦出现线程相关的东西（按调用线程分支、读 `thread_local`），
    /// 这条变红。真正的探针线程无关性由 `vox-overlay-linux` 的
    /// `window::tests::running_is_a_process_wide_fact` 从源头钉住。
    #[test]
    fn captions_bit_is_the_same_answer_on_the_poll_thread() {
        let _guard = OBSERVED_BIT_LOCK.lock();
        for started in [
            CapabilityStatus::ON,
            CapabilityStatus::off(UnavailableReason::NotWired),
            CapabilityStatus::off(UnavailableReason::Unsupported),
        ] {
            record_captions(started);
            let main = captions_status();
            let poller = std::thread::spawn(captions_status)
                .join()
                .expect("轮询线程");
            assert_eq!(
                poller, main,
                "轮询线程读到的 captions 必须与主线程一致（定义者结论={started:?}）"
            );
            // 两边的结论都必须等于判据本身，而不是"恰好都不说话"。
            assert_eq!(poller, captions_status_for(started, overlay_running()));
        }
        record_captions(CapabilityStatus::off(UnavailableReason::NotWired));
    }

    /// `vr_captions` 的判据（纯函数）：四件事实 → 位，逐条钉死。
    ///
    /// 判据本身平台中立，所以这条在**任何平台**上都跑；Windows 专有的只是那三个探针
    /// 怎么读（`vr_overlay::{RUNNING, CONNECTED, hmd_ready}`）。另有 Windows 专有的
    /// `platform::win::tests::vr_captions_bit_follows_the_build_and_the_openvr_probe`
    /// 钉"这条判据真的接在 `host_facts()` 上"。
    ///
    /// **自证**：把 [`vr_captions_status_for`] 改成恒 `ON` 这条就红（第九轮 M4 实测）。
    #[test]
    fn vr_captions_status_for_maps_the_three_probes_to_the_bit() {
        // 没编 feature：这一位跟头显、线程都无关，界面据此不渲染开关。
        assert_eq!(
            vr_captions_status_for(false, false, false, false),
            CapabilityStatus::off(UnavailableReason::NotBuilt)
        );
        assert_eq!(
            vr_captions_status_for(false, true, true, true),
            CapabilityStatus::off(UnavailableReason::NotBuilt)
        );
        // 编了、线程没起来 = 装配层没把这段路接上，跟"这台机器有没有头显"无关。
        assert_eq!(
            vr_captions_status_for(true, false, false, false),
            CapabilityStatus::off(UnavailableReason::NotWired)
        );
        assert_eq!(
            vr_captions_status_for(true, false, false, true),
            CapabilityStatus::off(UnavailableReason::NotWired)
        );
        // 真连上了 → 位为真（不看用户开关：位是"能不能"，§2.6 R5）。
        assert_eq!(
            vr_captions_status_for(true, true, true, false),
            CapabilityStatus::ON
        );
        assert_eq!(
            vr_captions_status_for(true, true, true, true),
            CapabilityStatus::ON
        );
        // 没连上：有运行期+头显只是还没连上 → busy；缺一个 → 这台机器做不了。
        assert_eq!(
            vr_captions_status_for(true, true, false, true),
            CapabilityStatus::off(UnavailableReason::Busy)
        );
        assert_eq!(
            vr_captions_status_for(true, true, false, false),
            CapabilityStatus::off(UnavailableReason::Unsupported)
        );
    }

    /// `background_service` 跟着"自启注册那条路通不通"走。
    #[test]
    fn background_service_bit_follows_the_autostart_result() {
        let _guard = OBSERVED_BIT_LOCK.lock();
        assert_eq!(
            BACKGROUND_SERVICE.status(),
            CapabilityStatus::off(UnavailableReason::NotWired),
            "定义者没跑过就不许报开"
        );

        // 否定情形：组策略锁了启动项（写不进）→ 位关、reason = permission。
        record_background_service(crate::events::autostart_status(Ok(true), Some(Err(()))));
        assert_eq!(
            host_facts().off.get(&Capability::BackgroundService),
            Some(&UnavailableReason::Permission),
            "注册失败时位必须关"
        );

        // 否定情形：根本查不到注册状态（插件不支持）→ unsupported。
        record_background_service(crate::events::autostart_status(Err(()), None));
        assert_eq!(
            host_facts().off.get(&Capability::BackgroundService),
            Some(&UnavailableReason::Unsupported)
        );

        // 注册成功 → 不报关（用户把开关关掉不影响这一位，位是"能不能"）。
        record_background_service(crate::events::autostart_status(Ok(false), Some(Ok(()))));
        assert!(
            !host_facts()
                .off
                .contains_key(&Capability::BackgroundService),
            "注册成功就不该再报关"
        );

        record_background_service(CapabilityStatus::off(UnavailableReason::NotWired));
    }

    /// `mic` **有意不接定义者**（见 `host_facts()` 的注释）：装配期拿不到"麦克风被占"的
    /// 否定证据，只能按档位上限报开。这条钉住"外壳没替芯关门"，免得将来某次改动把它
    /// 悄悄变成恒假——真出问题的证据由起流的 `PortError` → 界面 `Notice` 说话。
    #[test]
    fn mic_is_reported_on_the_ceiling_until_there_is_a_definer() {
        let facts = host_facts();
        assert!(
            !facts.off.contains_key(&Capability::Mic),
            "mic 今天没有定义者，外壳不该替它关门"
        );

        let runtime = Runtime::new(Settings::default(), clock());
        runtime.set_host_facts(facts);
        assert!(
            runtime.capabilities().host_enabled(Capability::Mic),
            "报告里这位得是开的（按上限报开）"
        );
    }
}
