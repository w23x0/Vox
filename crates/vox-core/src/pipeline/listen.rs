//! 听人说话：对方说外语 → 我看/听到中文。
//!
//! 抓的不是麦克风而是**某个程序的环回**（Discord、浏览器……），所以：
//! - 信号是数字源，本来就干净，**不降噪**（白降一遍还费 CPU，还可能吃掉人声）。
//! - **不设电平门**（`GateConfig::level(0.0)` 无条件放行）：环回源有假底噪，
//!   电平门会被底噪骗"正在说话"而误开，白白烧 token；省 token 靠服务端 VAD。
//! - 中文语音走系统默认输出（耳机），不能推 VB-CABLE，不然对方会听到自己的译文。
//! - 不认 `HotUpdate`：听的方向永远译成中文。

use crate::capability::{effective, Capability, HostFacts};
use crate::cloud::SessionParams;
use crate::composition::{
    Composition, EdgeSource, Input, Op, Output, PlaybackRole, RateRef, SessionSpec,
    COMPOSITION_SCHEMA_VERSION,
};
use crate::pipeline::INPUT_BLOCK_MS;
use crate::ports::PortError;
use crate::runtime::SessionConfig;
use crate::subtitle::Track;
// 生产路径把采集目标写在清单里（`Input::…`）；`CaptureTarget` 只剩测试在直接比。
#[cfg(test)]
use crate::ports::CaptureTarget;

#[cfg(test)]
use super::Plan;

/// Listen 的清单实例（§2.3.1）：本机派生，位为假的格子不进清单。
///
/// 它**不吃虚拟麦缺省**：这条腿的角色恒 [`PlaybackRole::Speaker`]，理由见模块头第三条。
pub(crate) fn composition(
    config: &SessionConfig,
    facts: &HostFacts,
) -> Result<Composition, PortError> {
    // 没选程序就没法抓环回。账本在 `start()` 里已经挡了一道，这里是兜底。
    let target = config
        .loopback_target
        .as_ref()
        .ok_or_else(|| PortError::new("还没选择监听程序。"))?;

    let bits = effective(facts);
    let mut inputs = Vec::new();
    // 这台设备抓不了指定程序（Android 通话类结构性拿不到、老 Windows 没有进程环回）→
    // 这一格进不了清单。腿仍在架构里：它在这档宿主上的形态另外定（§2.3.2）。
    if bits.contains(Capability::ProgramTap) {
        inputs.push(Input::ProcessLoopback {
            executable: target.executable.clone(),
            // 浏览器那种多进程的，声音常在子进程里，得连带抓。
            include_tree: target.include_process_tree,
            block_ms: INPUT_BLOCK_MS,
        });
    }

    let mut out = Vec::new();
    // 中文语音走系统默认输出（耳机）：**不能**推虚拟麦，不然对方会听到自己的译文。
    // 所以这一格恒 `Speaker`，既不看 `virtual_mic` 位，也不吃虚拟麦缺省设备。
    if config.voice.is_some() {
        out.push(Output::Playback {
            role: PlaybackRole::Speaker,
            device: config.output_device.clone(),
            source: EdgeSource::Session,
        });
    }
    // 字幕出口只在有屏幕的档位存在；"要不要显示"是视图开关，不进清单。
    if bits.contains(Capability::Captions) {
        out.push(Output::Captions {
            track: Track::Listen,
            source: EdgeSource::Session,
        });
    }

    let shell = facts.host.shell();
    Ok(Composition {
        schema_version: COMPOSITION_SCHEMA_VERSION,
        host: facts.host,
        r#in: inputs,
        // 不装降噪：环回的数字源本来就干净（`config.denoise` 对这条腿不生效，与现状一致）。
        ops: vec![
            Op::Mono,
            Op::Gate {
                config: config.gate,
            },
            Op::Resample {
                from: RateRef::Capture,
                to: RateRef::Session,
            },
        ],
        out,
        // 同 Speak：这三格跟着 `facts.host` 走（`HostKind::shell`）。
        life: shell.life,
        ui: shell.ui,
        control: shell.control.to_vec(),
        session: Some(SessionSpec {
            provider: config.provider,
            // 听的方向永远译成中文，不许热改。
            hot_update: false,
            uplink_rate: RateRef::Session,
            downlink_rate: RateRef::Playback,
            params: SessionParams {
                model_name: config.model_name.clone(),
                target_language: config.target_language.clone(),
                voice: config.voice.clone(),
                // 听别人说话没有"复刻我的音色"这回事。
                clone_frequency: None,
                // 源语言；None = 服务端自动识别。
                source_language: config.source_language.clone(),
            },
        }),
    })
}

/// 单测用的作业单：**位全开**时的派生物（模拟一台已经装配好、事实也注入过的桌面机）。
///
/// 产品路径是 `Plan::build(&config, &runtime.host_facts())`，而缺省事实是 fail-closed 的
/// （`HostFacts::uninjected`）；这里只是给"手上只有一份 `SessionConfig`"的单元测试一个入口。
#[cfg(test)]
pub(crate) fn plan(config: &SessionConfig) -> Result<Plan, PortError> {
    use crate::composition::HostKind;
    Plan::from(&composition(
        config,
        &HostFacts::all_wired(HostKind::Windows),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::listen_config;

    #[test]
    fn listen_grabs_the_process_loopback_and_skips_denoise() {
        let plan = plan(&listen_config()).expect("选了程序就该能派活");
        match &plan.target {
            CaptureTarget::ProcessLoopback {
                executable,
                include_tree,
            } => {
                assert_eq!(executable, "Discord.exe");
                assert!(include_tree);
            }
            other => panic!("听人说话该抓环回，结果是 {other:?}"),
        }
        assert!(
            !plan.chain_ops.iter().any(|op| matches!(op, Op::Denoise)),
            "数字源本来就干净，清单里不该有降噪这一节"
        );
        assert!(!plan.hot_update, "听的方向永远译成中文，不许热改");
    }

    #[test]
    fn listen_without_a_target_is_a_clear_error() {
        let mut config = listen_config();
        config.loopback_target = None;
        let err = match plan(&config) {
            Err(err) => err,
            Ok(_) => panic!("没选程序就该报错"),
        };
        assert!(
            err.message.contains("程序"),
            "报错要说人话：{}",
            err.message
        );
    }

    #[test]
    fn listen_can_run_text_only() {
        let mut config = listen_config();
        // 关掉"念出译文"就只剩字幕，省一半 token。
        config.voice = None;
        let plan = plan(&config).expect("纯文字也该能跑");
        assert!(plan.playback_device.is_none());
        assert!(plan.params.voice.is_none());
    }
}
