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
    Composition, EdgeSource, HostKind, Input, Op, Output, PlaybackRole, RateRef, SessionSpec,
    COMPOSITION_SCHEMA_VERSION,
};
use crate::gate::HalfDuplex;
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

    // —— 半双工的判据要看 `out` 里有没有"会出声的播放出口"，所以 `ops` 这一段挪到这儿。
    //    这是**纯位移**：`ops` 只看 `passthrough` / `config.denoise` / `config.aec` /
    //    `config.gate`（外加 `soft_aec` 那一位），
    //    与 `out` 互不依赖；`OP_ORDER` 只管 `ops` 数组的顺序，与 `out` 在清单里的
    //    相对位置无关。判据**直接读 `out`**，不另写一份推导（另写一份就是第二份真源）。
    //
    //    `&& !passthrough` **不许省**：直通模式下 `out` 里照样有一个 `Output::Playback`
    //    （role 是 Speaker/Monitor，`source` 是 `EdgeSource::Chain`），`audible` 会为真；
    //    但那时推的是**麦克风原声**，不是译音，把上行关掉就是自噬。`composition` 里的
    //    `session` 字段正是 `(!passthrough).then(...)`，所以 `!passthrough` ⟺ 有会话。
    let audible = !passthrough
        && out.iter().any(|output| {
            matches!(
                output,
                Output::Playback {
                    role: PlaybackRole::Speaker | PlaybackRole::Monitor,
                    ..
                }
            )
        });

    let mut gate = config.gate;
    // 半双工门（保底档）：只在"译音真的会从房间里放出来"**且**"无屏档"两条同时成立时开。
    // （"采集是麦克风"这一条**不用写成判据**：这里是 `speak::composition`，采集天生是
    //  `Input::Mic`；抓进程环回的那条腿在 `listen::composition`。）
    // 桌面档为什么不开：桌面 Linux 用户若没装虚拟麦，角色会退成 `Speaker`，此时开了
    // 半双工就是**在用户没要求的情况下切断他说话**。无屏盒子是本轮目标，桌面保持逐字不变。
    if audible && facts.host == HostKind::LinuxHeadless {
        gate.half_duplex = Some(HalfDuplex::DEFAULT);
    }

    let mut ops = vec![Op::Mono];
    // 麦克风收的是真实空气声，空调、键盘、风扇都在里面，得降。
    if config.denoise {
        ops.push(Op::Denoise);
    }
    // 回声消除：**降噪之后、门之前**（`Composition::OP_ORDER` 第 3 格，S4-C 稿 §3.1）。
    // 门之后装等于没装：门看能量判开合，残留的回声会自己把门推开，于是"没人说话时
    // 门被回声打开" → 上行发一整轮回声 → 云端把我们的译音又译回来。门之前的失效
    // 是可恢复的（开头几十毫秒落在门的 preroll 里，开门时补回来）。
    //
    // 两道闸缺一不可：能力位说"这台机器装得上"（判据见 `capability.rs::host_ceiling`
    // 与外壳的 `host_facts()`），`config.aec` 说"用户要不要"。
    if config.aec && bits.contains(Capability::SoftAec) {
        ops.push(Op::Aec);
    }
    ops.push(Op::Gate { config: gate });
    if !passthrough {
        ops.push(Op::Resample {
            from: RateRef::Capture,
            to: RateRef::Session,
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
    use crate::capability::UnavailableReason;
    use crate::gate::GateConfig;
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

    /// `Op::Aec` 的两道闸各自钉住：**能力位**（`soft_aec`）与**设置**（`speak.aec`），
    /// 缺一不进清单；进的时候位置正好在 `denoise` 之后、`gate` 之前（`OP_ORDER` 第 3 格）。
    ///
    /// 三段断言各自钉住一条变异：删掉整段 `push` → 第一段红；只留位、丢掉设置
    /// （`if bits.contains(SoftAec)`）→ 第三段红；只留设置、丢掉位（`if config.aec`）→
    /// 第二段红。
    #[test]
    fn the_aec_stage_needs_both_the_bit_and_the_setting() {
        fn ops_of(config: &SessionConfig, facts: &HostFacts) -> Vec<&'static str> {
            composition(config, facts)
                .ops
                .iter()
                .map(Op::kind)
                .collect()
        }

        // 位为假的那一档：档位上限里有 `soft_aec`，但这台机器报"装不上"。
        let mut without_bit = HostFacts::all_wired(HostKind::Windows);
        without_bit
            .off
            .insert(Capability::SoftAec, UnavailableReason::NotWired);

        let mut config = speak_config();
        assert!(config.aec, "缺省是开的（`SpeakSettings::default().aec`）");
        assert_eq!(
            ops_of(&config, &HostFacts::all_wired(HostKind::Windows)),
            vec!["mono", "denoise", "aec", "gate", "resample"],
            "位开 + 设置开 → 装，且落在 denoise 与 gate 之间"
        );
        assert_eq!(
            ops_of(&config, &without_bit),
            vec!["mono", "denoise", "gate", "resample"],
            "位为假时设置说什么都不进清单"
        );

        config.aec = false;
        assert_eq!(
            ops_of(&config, &HostFacts::all_wired(HostKind::Windows)),
            vec!["mono", "denoise", "gate", "resample"],
            "用户关了这一格就不装"
        );
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

    /// 半双工的判据全部落在清单里已有的两格上（有出声的播放出口 + 无屏档）。
    ///
    /// 四条各自钉一件事，缺一条那两条机制就会被后人"顺手删掉"。
    #[test]
    fn speak_opens_a_half_duplex_gate_only_on_the_headless_tier_with_a_speaker() {
        let mut config = speak_config();
        config.output_device = Some("hw:0,0".to_string());

        // ① 桌面（Windows）+ 虚拟麦：译音在线缆里，麦克风听不到，不开。
        let headless_gate = gate_of(&composition(
            &config,
            &HostFacts::all_wired(HostKind::Windows),
        ));
        assert!(
            headless_gate.half_duplex.is_none(),
            "译音进虚拟麦不是外放，不该开半双工"
        );

        // ② 桌面（Windows）+ 普通出声口：**桌面档一律不开**（没要求就别切用户的话）。
        let mut desktop_speaker = HostFacts::all_wired(HostKind::Windows);
        desktop_speaker
            .off
            .insert(Capability::VirtualMic, UnavailableReason::NotWired);
        let gate = gate_of(&composition(&config, &desktop_speaker));
        assert!(
            gate.half_duplex.is_none(),
            "桌面档即使退成普通出声也不许自动切话"
        );

        // ③ 无屏档 + 普通出声口：这才是这一档存在的场景，自动开。
        let gate = gate_of(&composition(
            &config,
            &HostFacts::all_wired(HostKind::LinuxHeadless),
        ));
        assert_eq!(
            gate.half_duplex,
            Some(HalfDuplex::DEFAULT),
            "无屏盒子外放译音 → 麦克风会录到回声，必须开半双工"
        );

        // ④ 直通（关翻译）：`out` 里照样有 `Output::Playback`，但推的是**麦克风原声**
        //    ——把上行关掉就是自噬。这条钉 `&& !passthrough`。
        config.translate = false;
        let gate = gate_of(&composition(
            &config,
            &HostFacts::all_wired(HostKind::LinuxHeadless),
        ));
        assert!(
            gate.half_duplex.is_none(),
            "直通推的是原声，关掉上行等于自噬"
        );
    }

    /// 清单的线上形状：`half_duplex` 为假时**不写这个键**（`skip_serializing_if`），
    /// 为真时写出来。键名与取值照 `composition.rs::tests::the_manifests_match_the_design_documents_instances`
    /// 里那两份期望值的写法（`gate.config` 是一个扁平对象）。
    #[test]
    fn the_headless_manifest_keeps_its_json_shape_when_the_gate_is_unset() {
        let facts = HostFacts::all_wired(HostKind::LinuxHeadless);
        let mut config = speak_config();
        config.output_device = Some("hw:0,0".to_string());

        // ① 有出声出口 → 这一格写出来，且只多这一格。
        let gate = serde_json::to_value(gate_of(&composition(&config, &facts))).unwrap();
        assert_eq!(gate["tail_ms"], GateConfig::MANUAL.tail_ms);
        assert_eq!(gate["preroll_ms"], GateConfig::MANUAL.preroll_ms);
        assert_eq!(
            gate["half_duplex"],
            serde_json::json!({ "tail_ms": HalfDuplex::DEFAULT_TAIL_MS }),
            "装上就该写出回声尾巴"
        );
        assert_eq!(
            sorted_keys(&gate),
            ["half_duplex", "kind", "preroll_ms", "tail_ms", "threshold"]
        );

        // ② 没有出声出口（没音色 → 不开播放汇）→ 判据为假 → **不写**这个键。
        config.voice = None;
        let gate = serde_json::to_value(gate_of(&composition(&config, &facts))).unwrap();
        assert!(
            gate.get("half_duplex").is_none(),
            "为假时不许写这个键，否则设计稿那两份实例当场红：{gate}"
        );
        assert_eq!(
            sorted_keys(&gate),
            ["kind", "preroll_ms", "tail_ms", "threshold"]
        );
    }

    /// 清单里门这一格的配置。Speak 的清单恒有门（`speak::composition` 无条件 push）。
    fn gate_of(composition: &Composition) -> GateConfig {
        composition
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Gate { config } => Some(*config),
                _ => None,
            })
            .expect("Speak 的清单里恒有门这一节")
    }

    fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("门配置在线上是个对象")
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    }
}
