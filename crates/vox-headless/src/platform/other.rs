//! 非 Linux 宿主上的平台实现：**明说做不到**。
//!
//! 无屏档的产品形态是 Linux 小盒子（ARM64），这个二进制到了别的宿主上没有任何意义：
//! 音频后端 `vox-audio-alsa` 在非 Linux 上是空 lib（`#![cfg(target_os = "linux")]`），
//! 没有采集、没有播放、没有设备目录。给它编一套假实现 = 静默降级，正是仓库规矩不许做的事
//! （`.omp/RULES.md`：未实现的能力不许广告）。
//!
//! 这一份存在的唯一理由是让 `cargo test/clippy --workspace` 在别的宿主上也能编过
//! （跟 `vox-audio-alsa` / `vox-overlay-win` 的空 lib 同一个套路）。

use std::collections::BTreeMap;

use vox_core::capability::{Capability, HostFacts, UnavailableReason};
use vox_core::composition::HostKind;
use vox_core::ports::{PortError, PortResult};

use super::Platform;

/// 档位仍然报无屏档：这个二进制**是**无屏档那一份构建，跟它跑在哪个宿主上无关
/// （档位是"哪一份构建"，不是"哪台机器"）。
pub fn host_kind() -> HostKind {
    HostKind::LinuxHeadless
}

/// 装配音频：直接失败。装配层会把它翻成一条错误退出（退出码 2），
/// 而不是留一个"起来了但什么都干不了"的进程。
pub fn platform() -> PortResult<Platform> {
    Err(PortError::new(
        "the headless build only works on Linux: this binary needs ALSA (vox-audio-alsa)",
    ))
}

/// 事实：这台机器上无屏档能做的事一件都没有（采集后端不存在），
/// 所以上限里那三位都关——`mic` 这里的 reason 是 `unsupported` 而不是 `not_wired`：
/// 不是"还没接线"，是这个构建在这台机器上根本采不到声。
/// `soft_aec` 同理：判据的 B2（采集与播放同声卡）连采集这一半都不存在，
/// 那是"这台机器做不到"，不是"这个版本还没接通"。
pub fn host_facts() -> HostFacts {
    HostFacts {
        host: host_kind(),
        off: BTreeMap::from([
            (Capability::Mic, UnavailableReason::Unsupported),
            (
                Capability::BackgroundService,
                UnavailableReason::Unsupported,
            ),
            (Capability::SoftAec, UnavailableReason::Unsupported),
        ]),
        virtual_mic_device: None,
    }
}

pub fn startup_notes() -> Vec<vox_core::Msg> {
    vec![vox_core::Msg::new(
        vox_core::notice::key::HEADLESS_NEEDS_LINUX,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_stay_inside_the_tier_ceiling() {
        assert!(host_facts().excess_off_bits().is_empty());
        assert_eq!(host_facts().host, HostKind::LinuxHeadless);
    }

    #[test]
    fn audio_assembly_refuses_instead_of_faking_it() {
        // `PortResult<Platform>` 上不能 `unwrap_err()`：`Platform` 不是 `Debug`
        // （它装着几个 `Box<dyn Fn…>`），所以用 `match` 取错误。
        let error = match platform() {
            Ok(_) => panic!("非 Linux 上装配必须失败，不许给一份假实现"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Linux"), "{error}");
    }
}
