//! 对外说话：我说中文 → 对方听到外语。
//!
//! 麦克风 48 kHz → 单声道 → 降噪 → 阀门 → 16 kHz → 上传；
//! 服务端回的外语语音推给播放汇（外壳会把它指到 VB-CABLE，这样对方在
//! 语音软件里听到的是译文），译文文字进"暖白"那条字幕轨。
//!
//! 两条流水线里只有它认 `HotUpdate`（换目标语言、换音色不重连）。

use crate::capability::{effective, Capability, HostFacts};
use crate::cloud::{protocol::CloneFrequency, SessionParams};
use crate::composition::{
    Composition, EdgeSource, Input, Op, Output, PlaybackRole, RateRef, SessionSpec,
    COMPOSITION_SCHEMA_VERSION,
};
use crate::pipeline::INPUT_BLOCK_MS;
use crate::runtime::SessionConfig;
use crate::subtitle::Track;
// 生产路径把采集目标写在清单里（`Input::…`）；`CaptureTarget` 只剩测试在直接比。
#[cfg(test)]
use crate::ports::CaptureTarget;

#[cfg(test)]
use super::Plan;

/// Speak 的清单实例（§2.3.1）：本机派生，位为假的格子不进清单。
pub(crate) fn composition(config: &SessionConfig, facts: &HostFacts) -> Composition {
    let bits = effective(facts);
    // 关掉「翻译」= 直通原声：不起云端会话，只把麦克风原声经闸门/降噪推给输出设备。
    // 此时没有译文/译音/字幕，`params` 里的语言音色都用不上。
    let passthrough = !config.translate;

    let mut inputs = Vec::new();
    // 采不到声的机器（麦克风被占 / 没授权）→ 这一格不进清单；界面按 `(mic, busy | permission)`
    // 说清"这台设备现在采不到声"，而不是让用户点一个必然失败的开始按钮。
    if bits.contains(Capability::Mic) {
        inputs.push(Input::Mic {
            // `None` = 系统默认麦克风。
            device: config.input_device.clone(),
            block_ms: INPUT_BLOCK_MS,
        });
    }

    let mut ops = vec![Op::Mono];
    // 麦克风收的是真实空气声，空调、键盘、风扇都在里面，得降。
    if config.denoise {
        ops.push(Op::Denoise);
    }
    ops.push(Op::Gate {
        config: config.gate,
    });
    if !passthrough {
        ops.push(Op::Resample {
            from: RateRef::Capture,
            to: RateRef::Session,
        });
    }

    let mut out = Vec::new();
    // 译文语音（直通时是原声）往哪去：虚拟麦位开着就是 `virtual_mic`，关着退成普通出声。
    // 退档**不是删功能**：位与 reason 进快照，界面照实说"这台设备做不到"（§2.3.2 / §2.6 R9）。
    let role = if bits.contains(Capability::VirtualMic) {
        PlaybackRole::VirtualMic
    } else {
        PlaybackRole::Speaker
    };
    // 缺省设备解析：这台机器报了"译音往哪送才算虚拟麦"、用户又没自己选，就用它。
    // （Windows 那份恒 `None`：设备由 VB-CABLE 驱动提供，用户在设置里选。）
    let device = match (role, &config.output_device) {
        (PlaybackRole::VirtualMic, None) => facts.virtual_mic_device.clone(),
        _ => config.output_device.clone(),
    };
    let source = if passthrough {
        EdgeSource::Chain
    } else {
        EdgeSource::Session
    };
    // 直通时送的是原声，与有没有音色无关；带翻译时没音色就不开播放汇（省一个设备）。
    if passthrough || config.voice.is_some() {
        out.push(Output::Playback {
            role,
            device,
            source,
        });
    }
    // 回听：只有带翻译、要语音、用户开了回听才有；直通没有译文，回听没意义。
    if !passthrough && config.monitor_translation && config.voice.is_some() {
        out.push(Output::Playback {
            role: PlaybackRole::Monitor,
            device: None,
            source: EdgeSource::Session,
        });
    }
    // 字幕出口只在有会话时存在（没接云端就没有文字）；"要不要显示"是视图开关，不进清单。
    if !passthrough && bits.contains(Capability::Captions) {
        out.push(Output::Captions {
            track: Track::Speak,
            source: EdgeSource::Session,
        });
    }

    let shell = facts.host.shell();
    Composition {
        schema_version: COMPOSITION_SCHEMA_VERSION,
        host: facts.host,
        r#in: inputs,
        ops,
        out,
        // 谁管它的命 / 有没有屏 / 谁在操控它：**档位属性**，不是芯的常量（`HostKind::shell`）。
        life: shell.life,
        ui: shell.ui,
        control: shell.control.to_vec(),
        session: (!passthrough).then(|| SessionSpec {
            provider: config.provider,
            // 两条流水线里只有它认热更新（换目标语言、换音色不重连）。
            hot_update: true,
            uplink_rate: RateRef::Session,
            downlink_rate: RateRef::Playback,
            params: SessionParams {
                model_name: config.model_name.clone(),
                target_language: config.target_language.clone(),
                voice: config.voice.clone(),
                clone_frequency: config
                    .voice_clone_frequency
                    .and_then(CloneFrequency::from_count),
                // 对外说话只说，不做源文识别。
                source_language: None,
            },
        }),
    }
}

/// 单测用的作业单：**位全开**时的派生物（模拟一台已经装配好、事实也注入过的桌面机）。
///
/// 产品路径是 `Plan::build(&config, &runtime.host_facts())`，而缺省事实是 fail-closed 的
/// （`HostFacts::uninjected`）；这里只是给"手上只有一份 `SessionConfig`"的单元测试一个入口。
#[cfg(test)]
pub(crate) fn plan(config: &SessionConfig) -> Plan {
    use crate::composition::HostKind;
    Plan::from(&composition(
        config,
        &HostFacts::all_wired(HostKind::Windows),
    ))
    .expect("位全开的清单必然可装")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::speak_config;

    #[test]
    fn speak_captures_the_microphone_and_denoises() {
        let config = speak_config();
        let plan = plan(&config);
        assert!(matches!(plan.target, CaptureTarget::Microphone(None)));
        assert!(
            plan.chain_ops.iter().any(|op| matches!(op, Op::Denoise)),
            "麦克风收的是空气声，清单里必须有降噪这一节"
        );
        assert!(plan.hot_update, "只有对外说话认热更新");
    }

    #[test]
    fn speak_opens_playback_only_when_a_voice_is_picked() {
        let mut config = speak_config();
        assert!(plan(&config).playback_device.is_some());
        // 不要语音就别开播放汇，白占一个设备。
        config.voice = None;
        assert!(plan(&config).playback_device.is_none());
    }

    #[test]
    fn headphone_monitor_only_opens_for_synthesized_voice() {
        let mut config = speak_config();
        config.monitor_translation = true;
        assert!(plan(&config).monitor_translation);
        config.voice = None;
        assert!(!plan(&config).monitor_translation, "没有译音就不该开空回听");
    }

    #[test]
    fn speak_passes_the_clone_frequency_through() {
        let mut config = speak_config();
        config.voice_clone_frequency = Some(1);
        assert_eq!(
            plan(&config).params.clone_frequency,
            Some(CloneFrequency::Once)
        );
        config.voice_clone_frequency = Some(5);
        assert_eq!(
            plan(&config).params.clone_frequency,
            Some(CloneFrequency::Always)
        );
        // 0 次 = 不复刻。
        config.voice_clone_frequency = Some(0);
        assert_eq!(plan(&config).params.clone_frequency, None);
    }

    #[test]
    fn closing_translate_turns_speak_into_original_voice_passthrough() {
        let mut config = speak_config();
        config.translate = false;
        let plan = plan(&config);
        assert!(plan.passthrough, "关掉翻译就该是原声直通，不接云端");
        assert!(!plan.monitor_translation, "直通没有译文，不该开回听");
        assert!(
            plan.playback_device == Some(config.output_device.clone()),
            "直通要把原声送到设置的输出设备（VB-CABLE），而不是跟着音色走空"
        );
    }

    #[test]
    fn opened_translate_always_keeps_translate_mode() {
        let config = speak_config();
        assert!(!plan(&config).passthrough, "开着翻译就是正常对外说话");
    }
}
