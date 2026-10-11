//! 无屏档的平台实现：档位、事实、音频三件套（自成一档：**ALSA 直开**）。
//!
//! **跟桌面 Linux 侧（`app/src-tauri/src/platform/linux/mod.rs`）的差别有两类**：
//!
//! 1. **音频后端不同**：无屏档是 `vox-audio-alsa`（ALSA 直开），桌面档是 PipeWire 那一份
//!    （`app/src-tauri/src/platform/linux/audio.rs`）。依据 `DIRECTIONS.md` §8 第 17 行（2026-09-26 裁决）与
//!    `docs/plans/S4-C-ALSA.md` §6.2：**按档位定，不做运行时探测切换**——这一档的
//!    `host_ceiling` 里没有 `program_tap` / `virtual_mic`，PipeWire 给不了任何增量。
//! 2. **启动了哪些东西不同**：热键（evdev）、托盘（StatusNotifier）、悬浮字幕窗
//!    （GTK + XWayland）、虚拟麦（PipeWire sink）这四件整块不启动——不是"没写"，是这四位
//!    都在 `host_ceiling(LinuxHeadless)` 之外（`docs/platform/EMBEDDED.md` §2 / §3.7）。

use std::collections::BTreeMap;
use std::sync::Arc;

use vox_core::capability::{Capability, HostFacts, UnavailableReason};
use vox_core::composition::HostKind;
use vox_core::ports::{DeviceRegistry, PortResult};

use super::Platform;

/// 哪一档宿主。无屏档：这个构建就是这一档，常量（`platform::host_kind()` 是唯一声明处）。
pub fn host_kind() -> HostKind {
    HostKind::LinuxHeadless
}

/// 音频几件套。**连接本身不在这儿探**：`ALSA 在不在`是运行期事实（起流那一刻才知道采不采得到声，
/// 探一次是 `startup_notes` 的事），这里只把几个工厂装好——跟桌面 Linux 侧同口径
/// （那边也只在 `startup_notes` 里记一条提示，不把应用拦住）。
///
/// **重采样档在这儿定下来**：读一次 `VOX_RESAMPLE_QUALITY`（缺省便宜档），
/// 播放侧内部那一份与上行那一份用同一档；非法值在这里就报错退出，不静默回落。
pub fn platform() -> PortResult<Platform> {
    let quality = super::resample_quality_from_env()?;
    Ok(Platform {
        // 采集：麦克风（无屏档没有"抓某个程序的声音"这一格，`program_tap` 在上限之外；
        // ALSA 也给不了——`CaptureTarget::ProcessLoopback` 在那边直接报错，见 §5.5）。
        capture: Box::new(|| Box::new(vox_audio_alsa::AlsaCapture::new())),
        // 播放：重采样工厂交给 ALSA 侧（S4-C-ALSA §7.1 的 R2）——设备直接给得起 24 kHz
        // 就直通，给不起它自己按 §5.2 的阶梯第 3 档装同一档重采样器。
        playback: Box::new(move || {
            let resample = super::resample_factory_for(quality);
            Box::new(vox_audio_alsa::AlsaPlayback::new(resample))
        }),
        // 上行重采样：与上面播放侧同一档（同一个决定，不读第二遍环境变量）。
        resample: super::resample_factory_for(quality),
        quality,
        // 设备目录：ALSA 的 pcm hint，报出来的 id 一律 `hw:CARD=<卡名>,DEV=<n>`（§5.4）。
        // 无状态，全进程共用一个。
        registry: Arc::new(vox_audio_alsa::AlsaDeviceRegistry::new()),
    })
}

// AEC 支持集：**唯一真源是芯里的 `vox_core::pipeline::AEC_RATES`**（链上建 `Op::Aec`
// 用的就是它），这里不另抄一份字面量——抄一份就是第二处真源。
//
// `docs/plans/S4-C-AEC.md` §4.3 的 B3 字面写的是 `{16000, 32000, 48000}`，但真实现只收
// 16 k / 48 k（32 k 驱不动上游的块级 API，见该稿 §10 R12），所以判据按 `AEC_RATES` 走。

/// **这台机器现在的事实**（S0 §2.5.0 第 2 步）：只报"关掉的位"，其余按档位上限算开。
///
/// 上限那张表在芯里（`host_ceiling(LinuxHeadless)` = `mic` + `background_service` + `soft_aec`），
/// 这里**只填 off**，绝不另立一张。逐位说清定义者：
///
/// | 位 | 这一档怎么定 | 定义者 |
/// | --- | --- | --- |
/// | `mic` | **报开**（不进 `off`） | 采集流真的能打开（起流时才知道）。装配期没有任何否定证据：一条采集流都还没开，"麦克风被独占"要真去开流才发现；ALSA 开不了时启动期只记一条提示（[`startup_notes`]），不把这一位翻假——跟桌面 Linux 侧 `host_facts()` 的注释同一条口径（§2.5.1 的 Linux 列），失败留给起流的 `PortError` → `Notice` 兜底 |
/// | `background_service` | `false(not_wired)` | 要的是 systemd unit 在不在（EMBEDDED §3.3）。**unit 与打包已经落地**（`crates/vox-headless/systemd/` 两份 + `tools/package-headless.sh`），但这一位还**没有检测者**：问 systemd 要状态得走 D-Bus（`org.freedesktop.systemd1`），而"无屏盒子上有没有 systemd 会话总线"本身还是个未知数——在位能真的问出答案之前照实报 `not_wired`（"这段路还没接上"），不拿"unit 文件存在"当凭据 |
/// | `soft_aec` | 按 B1–B4 判（见 [`soft_aec_unavailable_reason`]） | 这个构建编没编进去 × 采集与播放同不同卡 × 采集率在不在支持集内 × AEC 工厂起不起得来（`docs/plans/S4-C-AEC.md` §4.3）。**裸板子上才可能是开的**：B2 要"缺省的采集口与播放口同属一张卡"，而 `default` 经 PipeWire 桥时卡名认不出来（`card=-1`）⇒ 照实报 `unsupported`；只有 `default` 真的解析到一块声卡（无屏盒子的常态）这一位才可能开 |
///
/// 其余宿主位（`captions` / `tray` / `global_hotkey` / `virtual_mic` / `program_tap` /
/// `vr_captions` / `net_in` / `net_out` / `file_config`）**不进 `off`**：它们在
/// `host_ceiling(LinuxHeadless)` 之外，芯直接算 `false(unsupported)`。塞进 `off` 反而会被芯
/// 当成外壳 bug（`HostFacts::excess_off_bits`）——那是"谎报能力"的结构性封堵，见 §2.5.0 第 4 步。
pub fn host_facts() -> HostFacts {
    let mut off = BTreeMap::from([(Capability::BackgroundService, UnavailableReason::NotWired)]);
    if let Some(reason) = soft_aec_unavailable_reason() {
        off.insert(Capability::SoftAec, reason);
    }
    HostFacts {
        host: host_kind(),
        off,
        // 无屏档不建虚拟麦节点（这一位在上限之外），所以没有"译音往哪送才算虚拟麦"这回事。
        virtual_mic_device: None,
    }
}

/// `soft_aec` 为什么不能开（`docs/plans/S4-C-AEC.md` §4.3 的 B1–B4）。全过 = `None` = 位开。
///
/// **B1：这个构建编进去没有。** `vox-dsp` 里**没有** `soft-aec` 这个 cargo feature，
/// AEC 是**无条件**的依赖（W1 的活）——所以"编进去了"在构建里恒成立，B1 恒成立，
/// 这里如实写清楚而不是现造一个 feature 出来。
///
/// **B4 在 B2/B3 之前**，两条理由：一是拿不到工厂时"能不能做"根本无从谈起，报出来的
/// 理由该是 `not_wired`（"这个版本还没接通"）而不是 `unsupported`（"这台设备结构上做不到"
/// ——那是假话）；二是 B2 要**枚举设备目录**，而报告三模式（`Probe::Nothing`）承诺不碰
/// 音频设备，工厂拿不到时就没理由去碰。
///
/// **B2 / B3 只在 B4 过了之后才采**：`aec_factory_available()` 一返 `false`
/// 就到此为止（报 `not_wired`），不再去枚举设备目录——报告三模式（`Probe::Nothing`）
/// 承诺不碰音频设备，工厂拿不到时没理由去碰。
///
/// 判据本身在 [`soft_aec_reason_from`] 里（纯函数），这里只负责**采事实**。
fn soft_aec_unavailable_reason() -> Option<UnavailableReason> {
    // B1 恒成立（见上），所以这里没有分支。

    // B4：AEC 工厂起不起来。
    if !aec_factory_available() {
        return soft_aec_reason_from(false, false, false);
    }

    let rate_ok = vox_core::pipeline::AEC_RATES.contains(&vox_audio_alsa::CAPTURE_WANT_RATE);
    soft_aec_reason_from(true, default_devices_share_a_card(), rate_ok)
}

/// B1–B4 的**判据映射**（纯函数，与机器无关）：三个事实进，`None` = 位开。
///
/// 事实由 [`soft_aec_unavailable_reason`] 采（那要调工厂、要枚举设备，机器之间不一样）；
/// 判据放这里，单测因此不依赖本机有没有声卡。
fn soft_aec_reason_from(
    factory_ok: bool,
    same_card: bool,
    rate_ok: bool,
) -> Option<UnavailableReason> {
    if !factory_ok {
        // 拿不到工厂时报 `not_wired`（"这个版本还没接通"）而不是 `unsupported`
        // （"这台设备结构上做不到"——那是假话）。
        return Some(UnavailableReason::NotWired);
    }
    if !same_card || !rate_ok {
        return Some(UnavailableReason::Unsupported);
    }
    None
}

/// B2：缺省的那一对采集口与播放口是不是同一张卡。
///
/// 判据要两条**设备 id**，而这里的 `host_facts()` 是个 `fn()`（`vox_host::HostPorts::facts`
/// 的类型），拿不到设置里选的设备名，所以取**缺省的那一对**（设备目录里被标了 `is_default`
/// 的那两条）；任一取不到就按不成立报（"认不出是同一张卡" ≠ "是同一张卡"）。
///
/// **欠账**：设置里显式选的设备对不参与判定。要改得让事实在设置读完之后再注入一次
/// （或把 `HostPorts::facts` 换成能捕获的闭包）——那要动共享宿主层，不在本轮。
fn default_devices_share_a_card() -> bool {
    let registry = vox_audio_alsa::AlsaDeviceRegistry::new();
    let Ok(inputs) = registry.input_devices() else {
        return false;
    };
    let Ok(outputs) = registry.output_devices() else {
        return false;
    };
    let Some(capture) = inputs.into_iter().find(|device| device.is_default) else {
        return false;
    };
    let Some(playback) = outputs.into_iter().find(|device| device.is_default) else {
        return false;
    };
    vox_audio_alsa::same_card(&capture.name, &playback.name)
}

/// B4：AEC 工厂起得来吗。
///
/// 判据 = 拿采集侧要的那个率把工厂真的调起来一次：工厂的签名是
/// `Box<dyn Fn(u32) -> PortResult<Box<dyn Aec>>>`（`vox_core::pipeline::AecFactory`），
/// 所以是**先取工厂、再用率调它**，不是 `aec_factory().is_ok()`。
/// 返回 `Err` 说明这一档建不起来（配置非法 / 分配失败）⇒ `not_wired`（"这段路还没接上"），
/// 而不是 `unsupported`（"这台设备结构上做不到"）。
fn aec_factory_available() -> bool {
    aec_factory_available_at(vox_audio_alsa::CAPTURE_WANT_RATE)
}

/// 上面那道闸门按任意采集率问一次（单测用它做反证：换一个上游建不起来的率必须翻假）。
fn aec_factory_available_at(rate: u32) -> bool {
    vox_dsp::ports::aec_factory()(rate).is_ok()
}

/// 启动期要告诉用户的话（进日志 / journal，不是界面弹窗）。
///
/// 只报**事实**，不改任何位：ALSA 开不了设备时"采不到声"是起流那一刻的事
/// （见 [`host_facts`] 那张表第一行）。两句文案照 `docs/plans/S4-C-ALSA.md` §6.3 末段：
/// 一句说"打不开 ALSA 设备"，一句说"检测到 PipeWire 的 ALSA 桥（我们仍然直开）"。
pub fn startup_notes() -> Vec<vox_core::Msg> {
    startup_note_keys(
        vox_audio_alsa::alsa_available(),
        vox_audio_alsa::pipewire_alsa_bridge_present(),
    )
    .into_iter()
    .map(vox_core::Msg::new)
    .collect()
}

/// "哪条提示在什么情况下发"。**纯函数**：两个探测原语的结果进、key 列表出，
/// 这样两条提示的判据能脱开真设备验（本机有 ALSA，"打不开"那一支跑不到）。
///
/// 两个探测都只描述**事实**：
/// - `alsa_ok` = 能开一条 PCM（[`vox_audio_alsa::alsa_available`]）。它只让用户知道
///   "现在采不到声"这件事**在装配期就能看出来**了，不动任何能力位（`mic` 照旧按上限报开）。
/// - `pipewire_bridge` = `alsa.conf` 里铺着 PipeWire 的桥。**不是故障**：`hw:` 直开不走它，
///   只有 `default` 会被导过去，所以这条提示是"我们知道这机器上有桥，但我们仍然直开"。
fn startup_note_keys(alsa_ok: bool, pipewire_bridge: bool) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if !alsa_ok {
        keys.push(vox_core::notice::key::ALSA_MISSING_HEADLESS);
    }
    if pipewire_bridge {
        keys.push(vox_core::notice::key::PIPEWIRE_ALSA_BRIDGE_HEADLESS);
    }
    keys
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

    /// 无屏档的能力位：**上限里那三位**照 §2.5.1 的表报，别的宿主位一律 `unsupported`。
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
        // 软件 AEC 的上限里有这一位（所以不是 unsupported 的"结构上做不到"），但它今天
        // 开不开**取决于这台机器**：B4 过了之后还要 B2（缺省的采集口与播放口同卡）——
        // 开发机上 `default` 走 PipeWire 桥、卡名认不出来就报 unsupported，裸板子上才可能开。
        // 所以这里只钉**机器无关**的两件：上限里有它、且报告里那一格就是那道探针的答案。
        let soft_aec = report.host[&Capability::SoftAec];
        let expected = soft_aec_unavailable_reason();
        assert!(host_ceiling(HostKind::LinuxHeadless).contains(Capability::SoftAec));
        assert_eq!(
            soft_aec.reason, expected,
            "报告里这一格必须就是那道探针的答案"
        );
        assert_eq!(soft_aec.enabled, expected.is_none());

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

    /// B1–B4 的判据映射（纯函数）：三个事实怎么折成一个 reason。与机器无关。
    #[test]
    fn the_soft_aec_reasons_map_from_the_three_facts() {
        assert_eq!(
            soft_aec_reason_from(false, true, true),
            Some(UnavailableReason::NotWired),
            "工厂拿不到时必须是 not_wired，不是 unsupported 那句假话"
        );
        assert_eq!(
            soft_aec_reason_from(true, false, true),
            Some(UnavailableReason::Unsupported),
            "采集与播放不同卡 ⇒ unsupported"
        );
        assert_eq!(
            soft_aec_reason_from(true, true, false),
            Some(UnavailableReason::Unsupported),
            "采集率不在支持集 ⇒ unsupported"
        );
        assert_eq!(
            soft_aec_reason_from(true, true, true),
            None,
            "三条全过 = 位开"
        );
    }

    /// B4 那道闸门**真的在调工厂**（不是恒 `false` 的钩子）：采集侧要的那个率起得来，
    /// 而 AEC 支持集之外的率同一句探针会翻假。
    #[test]
    fn the_aec_factory_gate_really_calls_the_factory() {
        assert!(
            aec_factory_available(),
            "{} Hz 上工厂该起得来",
            vox_audio_alsa::CAPTURE_WANT_RATE
        );
        assert!(
            !aec_factory_available_at(44_100),
            "44100 不在 AEC_RATES 里，工厂必须报 Err——这才说明闸门是活的"
        );
    }

    /// B3 的边界：采集侧要的率（`CAPTURE_WANT_RATE`）必须落在芯给的 `AEC_RATES` 里
    /// （唯一真源，不在这里抄一份字面量）。
    #[test]
    fn the_requested_capture_rate_is_inside_the_aec_support_set() {
        let wanted = vox_audio_alsa::CAPTURE_WANT_RATE;
        assert!(
            vox_core::pipeline::AEC_RATES.contains(&wanted),
            "采集侧要的 {wanted} Hz 不在 AEC 支持集 {:?} 里",
            vox_core::pipeline::AEC_RATES,
        );
    }

    /// 两条启动提示的判据（`docs/plans/S4-C-ALSA.md` §6.3 末段 / §7.1 的 R3）：
    /// 打不开 ALSA 发第一条，检测到 PipeWire 的 ALSA 桥发第二条，两条互不排斥。
    /// 纯函数，所以本机（有 ALSA、可能没有桥）也能把四种组合都过一遍。
    #[test]
    fn the_two_startup_notes_follow_the_two_probes() {
        use vox_core::notice::key;
        assert_eq!(startup_note_keys(true, false), Vec::<&str>::new());
        assert_eq!(
            startup_note_keys(false, false),
            vec![key::ALSA_MISSING_HEADLESS]
        );
        assert_eq!(
            startup_note_keys(true, true),
            vec![key::PIPEWIRE_ALSA_BRIDGE_HEADLESS]
        );
        assert_eq!(
            startup_note_keys(false, true),
            vec![
                key::ALSA_MISSING_HEADLESS,
                key::PIPEWIRE_ALSA_BRIDGE_HEADLESS
            ]
        );
        // 真探针的结果也走同一条路（本机取值不定，只要求不 panic、key 都在声明表里）。
        for note in startup_notes() {
            assert!(vox_core::notice::ALL_KEYS.contains(&note.key), "{note}");
        }
    }

    /// 探针不 panic：有 ALSA（本机）→ 各自 true/false，没有（CI 容器）→ 全是 false；
    /// 两种都算对，只要"探得出来、不 panic"，且启动提示装得出来。
    #[test]
    fn audio_probes_do_not_panic() {
        let _ = vox_audio_alsa::alsa_available();
        let _ = vox_audio_alsa::pipewire_alsa_bridge_present();
        let _ = startup_notes();
    }
}
