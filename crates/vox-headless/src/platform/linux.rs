//! 无屏档的平台实现：档位、事实、音频三件套（全部复用桌面 Linux 那一份）。
//!
//! **跟桌面 Linux 侧（`app/src-tauri/src/platform/linux/mod.rs`）的差别只有"启动了哪些东西"**：
//! 热键（evdev）、托盘（StatusNotifier）、悬浮字幕窗（GTK + XWayland）、虚拟麦（PipeWire sink）
//! 这四件在无屏档整块不启动——不是"没写"，是这四位都在 `host_ceiling(LinuxHeadless)`
//! 之外（`docs/platform/EMBEDDED.md` §2 / §3.7）。
//!
//! 音频三件套（采集 / 播放 / 设备目录）走的就是同一个 crate，见 [`platform`]。

use std::collections::BTreeMap;
use std::sync::Arc;

use vox_core::capability::{Capability, HostFacts, UnavailableReason};
use vox_core::composition::HostKind;
use vox_core::ports::PortResult;

use super::Platform;

/// 哪一档宿主。无屏档：这个构建就是这一档，常量（`platform::host_kind()` 是唯一声明处）。
pub fn host_kind() -> HostKind {
    HostKind::LinuxHeadless
}

/// 音频三件套。**连接本身不在这儿探**：`PipeWire 在不在`是运行期事实（[`pipewire_available`]），
/// 起流那一刻才知道采不采得到声；这里只把三个工厂装好——跟桌面 Linux 侧同口径
/// （那边也只在 `startup_notes` 里记一条提示，不把应用拦住）。
pub fn platform() -> PortResult<Platform> {
    Ok(Platform {
        // 采集：麦克风（无屏档没有"抓某个程序的声音"这一格，`program_tap` 在上限之外）。
        capture: Box::new(|| Box::new(vox_audio_linux::LinuxCapture::new())),
        // 播放：请求设备率交给 PipeWire 在图里转，重采样器用芯那一份；
        // 与桌面 Linux 侧 `platform/linux/audio.rs` 逐行同形。
        playback: Box::new(|| {
            let resample = crate::dsp::resample_factory();
            Box::new(vox_audio_linux::LinuxPlayback::new(resample))
        }),
        // 设备目录：PipeWire 图里数节点。无状态，全进程共用一个。
        registry: Arc::new(vox_audio_linux::LinuxDeviceRegistry::new()),
    })
}

/// PipeWire 在不在。只用来发一条启动提示（见 [`startup_notes`]），**不当能力位用**
/// ——理由见 [`host_facts`]。
pub fn pipewire_available() -> bool {
    vox_audio_linux::pipewire_available()
}

/// **这台机器现在的事实**（S0 §2.5.0 第 2 步）：只报"关掉的位"，其余按档位上限算开。
///
/// 上限那张表在芯里（`host_ceiling(LinuxHeadless)` = `mic` + `background_service`），
/// 这里**只填 off**，绝不另立一张。逐位说清定义者：
///
/// | 位 | 这一档怎么定 | 定义者 |
/// | --- | --- | --- |
/// | `mic` | **报开**（不进 `off`） | 采集流真的能打开（起流时才知道）。装配期没有任何否定证据：一条采集流都还没开，"麦克风被独占"要真去开流才发现；PipeWire 连不上时启动期只记一条提示，不把这一位翻假——跟桌面 Linux 侧 `host_facts()` 的注释同一条口径（§2.5.1 的 Linux 列），失败留给起流的 `PortError` → `Notice` 兜底 |
/// | `background_service` | `false(not_wired)` | 要的是 systemd unit 在不在（EMBEDDED §3.3）。**unit 与打包已经落地**（`crates/vox-headless/systemd/` 两份 + `tools/package-headless.sh`），但这一位还**没有检测者**：问 systemd 要状态得走 D-Bus（`org.freedesktop.systemd1`），而"无屏盒子上有没有 systemd 会话总线"本身还是个未知数——在位能真的问出答案之前照实报 `not_wired`（"这段路还没接上"），不拿"unit 文件存在"当凭据 |
///
/// 其余宿主位（`captions` / `tray` / `global_hotkey` / `virtual_mic` / `program_tap` /
/// `vr_captions` / `net_in` / `net_out` / `file_config`）**不进 `off`**：它们在
/// `host_ceiling(LinuxHeadless)` 之外，芯直接算 `false(unsupported)`。塞进 `off` 反而会被芯
/// 当成外壳 bug（`HostFacts::excess_off_bits`）——那是"谎报能力"的结构性封堵，见 §2.5.0 第 4 步。
pub fn host_facts() -> HostFacts {
    HostFacts {
        host: host_kind(),
        off: BTreeMap::from([(Capability::BackgroundService, UnavailableReason::NotWired)]),
        // 无屏档不建虚拟麦节点（这一位在上限之外），所以没有"译音往哪送才算虚拟麦"这回事。
        virtual_mic_device: None,
    }
}

/// 启动期要告诉用户的话（进日志 / journal，不是界面弹窗）。
///
/// 只报**事实**，不改任何位：PipeWire 连不上时"采不到声"是起流那一刻的事
/// （见 [`host_facts`] 那张表第一行）。
pub fn startup_notes() -> Vec<String> {
    let mut notes = Vec::new();
    if !pipewire_available() {
        notes.push(
            "没连上 PipeWire：Linux 音频后端以 PipeWire 为前提（主流发行版默认就有；\
             无屏盒子上要装 daemon + 会话管理器）。"
                .to_string(),
        );
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::capability::{host_ceiling, CapabilityReport};
    use vox_core::settings::ModelProvider;

    fn report() -> CapabilityReport {
        CapabilityReport::of(&host_facts(), ModelProvider::Aliyun, ModelProvider::Aliyun)
    }

    /// 外壳报的 off 不许越出档位上限——越出去就是"谎报能力"，芯会把它记成外壳 bug。
    /// 这是本文件唯一一条能被单测钉死的结构性不变量。
    #[test]
    fn facts_never_claim_bits_outside_the_tier_ceiling() {
        let facts = host_facts();
        let excess: Vec<&str> = facts.excess_off_bits().iter().map(|bit| bit.id()).collect();
        assert!(excess.is_empty(), "off 里有上限之外的位：{excess:?}");
        assert_eq!(facts.host, HostKind::LinuxHeadless);
    }

    /// 无屏档的能力位：**上限里那两位**照 §2.5.1 的表报，别的宿主位一律 `unsupported`。
    #[test]
    fn the_report_matches_the_headless_tier() {
        let report = report();
        assert_eq!(report.tier, HostKind::LinuxHeadless);

        // `mic` 按上限报开（装配期没有否定证据，见 `host_facts` 的注释）。
        assert!(
            report.host_enabled(Capability::Mic),
            "无屏档的 mic 按上限报开"
        );
        // 后台常驻是这一档相对手机档的优势，但 unit 还没落地 → not_wired（不是 unsupported）。
        let background = report.host[&Capability::BackgroundService];
        assert!(!background.enabled);
        assert_eq!(background.reason, Some(UnavailableReason::NotWired));

        // 上限之外的那些：无屏设备没有屏幕/键盘/托盘宿主，也不给别的程序当麦克风。
        for bit in [
            Capability::Captions,
            Capability::Tray,
            Capability::GlobalHotkey,
            Capability::VirtualMic,
            Capability::ProgramTap,
            Capability::VrCaptions,
        ] {
            let status = report.host[&bit];
            assert!(!status.enabled, "{} 在无屏档不该开着", bit.id());
            assert_eq!(
                status.reason,
                Some(UnavailableReason::Unsupported),
                "{}",
                bit.id()
            );
            assert!(
                !host_ceiling(HostKind::LinuxHeadless).contains(bit),
                "{} 确实在档位上限之外",
                bit.id()
            );
        }
        // 还没实现的那三位（网络进出、配置当控制面）：位必须是事实 → 恒假。
        for bit in [
            Capability::NetIn,
            Capability::NetOut,
            Capability::FileConfig,
        ] {
            assert!(!report.host_enabled(bit), "{} 还没实现，不许广告", bit.id());
        }
        // 宿主位表每位都有条目（界面按"有几位、关了几位"排版）。
        assert_eq!(report.host.len(), Capability::HOST.len());
    }

    /// 报告里不许出现"开了却带 reason"这种自相矛盾的格子（芯的构造入口已经保证，
    /// 这里再按报告的实际输出核一遍——外壳是唯一可能注入脏 off 的地方）。
    #[test]
    fn every_reported_bit_is_consistent() {
        for (bit, status) in report().host {
            assert!(
                status.is_consistent(),
                "{} 的位自相矛盾：{status:?}",
                bit.id()
            );
        }
    }

    #[test]
    fn pipewire_probe_does_not_panic() {
        // 有 PipeWire（本机）→ true，没有（CI 容器）→ false；两种都算对，
        // 只要"探得出来、不 panic"。
        let _ = pipewire_available();
        let _ = startup_notes();
    }
}
