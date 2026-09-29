//! 快照的对外形状。
//!
//! 内核 `runtime::Snapshot` 和前端 `types.snapshot.ts` 的字段名有少量偏差，
//! 在这里做一层映射。
//!
//! 规则：
//! - 前端要的每一个字段必须在、名字一字不差——缺字段前端会拿到 `undefined`。
//! - 多余字段无害（TS 不读罢了），所以 `virtual_cable_installed` 保留着不去掉。
//! - `Option` 字段**不加** `skip_serializing_if`——前端要的是显式 `null`。

use serde::Serialize;
use std::collections::BTreeMap;

use vox_core::capability::CapabilityReport;
use vox_core::event::{Notice, Pipeline, PipelineState};
use vox_core::gate::GateStatus;
use vox_core::latency::LatencySnapshot;
use vox_core::ports::{AudioApp, DeviceInfo};
use vox_core::runtime::Snapshot as CoreSnapshot;
use vox_core::settings::{
    ControlSettings, ListenSettings, Settings, SpeakSettings, SubtitleSettings,
};
use vox_core::usage::UsageLedger;

use crate::state::AppState;

// ─── 设置 ────────────────────────────────────────────────────────────────────

/// 前端 `Settings` 的镜像。v2 起模型跟着两条流水线分别保存，嵌套结构体可以透传；
/// 内核顶层只保留一个不再序列化的 v1 迁移字段，所以这里不暴露它。
#[derive(Debug, Clone, Serialize)]
pub struct SettingsDto {
    pub version: u32,
    pub speak: SpeakSettings,
    pub listen: ListenSettings,
    pub subtitle: SubtitleSettings,
    pub autostart: bool,
    pub start_minimized: bool,
    pub ui_language: String,
    /// Agent 控制面的开关与授权位。设置页那一屏（`sections/AgentControl.tsx`）读写它，
    /// **起没起**是另一回事——那在 `SnapshotDto.control` 里（观察值，不是设置）。
    pub control: ControlSettings,
}

impl From<Settings> for SettingsDto {
    fn from(s: Settings) -> Self {
        Self {
            version: s.version,
            speak: s.speak,
            listen: s.listen,
            subtitle: s.subtitle,
            autostart: s.autostart,
            start_minimized: s.start_minimized,
            ui_language: s.ui_language,
            control: s.control,
        }
    }
}

/// 兼容旧 UI 偶尔发来的顶层 `model`：复制到两条流水线的 `model_name`。
/// 新 UI 直接发 `speak.model_name` / `listen.model_name`，本函数不会改它们。
pub fn patch_to_kernel(mut patch: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = patch.as_object_mut() {
        if let Some(model) = obj.remove("model") {
            for pipeline in ["speak", "listen"] {
                let entry = obj
                    .entry(pipeline.to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(fields) = entry.as_object_mut() {
                    fields
                        .entry("model_name".to_string())
                        .or_insert_with(|| model.clone());
                }
            }
        }
    }
    patch
}

// ─── 顶层 DTO ───────────────────────────────────────────────────────────────

/// 前端 `Snapshot` 的完整镜像。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDto {
    pub settings: SettingsDto,
    /// 前端字段名 `has_api_key`；内核给的是 `api_key_configured`。
    pub has_api_key: bool,
    pub api_keys: ProviderKeyStatusDto,
    pub speak: PipelineSnapshotDto,
    pub listen: PipelineSnapshotDto,
    pub mic_active: bool,
    pub headphones_advised: bool,
    pub devices: DeviceSnapshotDto,
    pub usage: UsageLedger,
    pub notices: Vec<Notice>,
    /// **能力位报告**：芯算好的那一份（`{ tier, host, speak, listen }`），原样透传。
    ///
    /// 界面按 `(位, reason)` 渲染 / 降级（S0 §2.6 R1–R9），**不另立**第二份位表——
    /// 所以这里不做任何加工、也不改名。`tier` 只是显示用的档位名，界面**不许**按它分支。
    pub capabilities: CapabilityReport,
    /// **控制面现在的样子**：设置里要的那一档（`enabled` / `port`）与"真起了没有"。
    ///
    /// 界面**按这一格渲染**（起没起、起不来是为什么），不许拿 `settings.control` 自己推
    /// "应该"在跑——要什么是用户的事，起没起是事实（`mcp::Status` 的注释里有同一条）。
    /// 形状就是 `mcp::Status`，与 `capabilities` 一样原样透传：**没有第二份状态**。
    pub control: crate::mcp::Status,
}

pub type ProviderKeyStatusDto = BTreeMap<String, bool>;

// ─── 流水线 ──────────────────────────────────────────────────────────────────

/// 前端 `PipelineSnapshot`：多了 `running` 和完整 `gate`。
#[derive(Debug, Clone, Serialize)]
pub struct PipelineSnapshotDto {
    pub state: PipelineState,
    pub state_label: &'static str,
    /// 前端需要的布尔，内核没直接给——从 `PipelineState::is_running()` 派生。
    pub running: bool,
    /// 完整的五字段门状态。不跑的时候为 `null`（跟 mock 一致）。
    pub gate: Option<GateStatus>,
    /// 前端没用到，但内核给了、留着无害。
    pub last_error: Option<String>,
    /// 逐轮延迟统计（内核 `LatencySnapshot` 的同形映射）。
    pub latency: LatencySnapshot,
}

// ─── 设备 ────────────────────────────────────────────────────────────────────

/// 前端 `DeviceSnapshot`：`audio_apps` 要重命名为 `apps`。
#[derive(Debug, Clone, Serialize)]
pub struct DeviceSnapshotDto {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
    /// 前端字段名 `apps`；内核给的是 `audio_apps`。
    pub apps: Vec<AudioApp>,
    /// 前端没有这个字段，但保留着不会出错（多余字段被 TS 忽略）。
    pub virtual_cable_installed: bool,
    /// `installed` / `install_pending_reboot` / `uninstall_incomplete` / `not_installed`。
    pub virtual_cable_status: &'static str,
    /// `visible` / `hidden` / `absent`，用于管理新版附带的 16 声道端点。
    pub virtual_cable_16ch_status: &'static str,
}

// ─── 转换函数 ────────────────────────────────────────────────────────────────

/// 从 `AppState` 取内核快照，再加上装配层缓存的门状态，拼成前端要的 DTO。
pub fn snapshot(state: &AppState) -> SnapshotDto {
    let core: CoreSnapshot = state.runtime.snapshot();

    let speak_running = core.speak.state.is_running();
    let listen_running = core.listen.state.is_running();

    SnapshotDto {
        settings: core.settings.into(),
        has_api_key: core.api_key_configured,
        api_keys: core
            .api_keys_configured
            .iter()
            .map(|(provider, configured)| (provider.as_id().to_string(), *configured))
            .collect(),
        speak: pipeline_dto(&core.speak, state.gate_of(Pipeline::Speak, speak_running)),
        listen: pipeline_dto(
            &core.listen,
            state.gate_of(Pipeline::Listen, listen_running),
        ),
        mic_active: core.mic_active,
        headphones_advised: core.headphones_advised,
        devices: devices_dto(core.devices),
        usage: core.usage,
        notices: core.notices,
        capabilities: core.capabilities,
        control: state.control.status(),
    }
}

/// 内核 `PipelineSnapshot` → 前端形状。
///
/// `gate` 来自装配层缓存，不跑时已经被 `AppState::gate_of` 过滤成 `None`。
fn pipeline_dto(
    core: &vox_core::runtime::PipelineSnapshot,
    gate: Option<GateStatus>,
) -> PipelineSnapshotDto {
    PipelineSnapshotDto {
        state: core.state,
        state_label: core.state_label,
        running: core.state.is_running(),
        gate,
        last_error: core.last_error.clone(),
        latency: core.latency.clone(),
    }
}

/// 内核 `DeviceSnapshot` → 前端形状（`audio_apps` 重命名为 `apps`）。
fn devices_dto(core: vox_core::runtime::DeviceSnapshot) -> DeviceSnapshotDto {
    // 虚拟麦克风的状态是**平台事实**：Windows 上是"VB-CABLE 装没装"，
    // Linux 上是"PipeWire 在不在"（不需要安装任何东西）。
    let virtual_device = crate::platform::virtual_device_status();
    DeviceSnapshotDto {
        inputs: core.inputs,
        outputs: core.outputs,
        apps: core.audio_apps,
        virtual_cable_installed: core.virtual_cable_installed,
        virtual_cable_status: virtual_device.status,
        virtual_cable_16ch_status: virtual_device.multichannel_status,
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use vox_core::event::PipelineState;
    use vox_core::gate::{GateKind, GateState};

    /// 造一个最小的 `SnapshotDto`，用来验证序列化后的 JSON 形状。
    fn make_dto(running: bool, gate: Option<GateStatus>) -> SnapshotDto {
        let state = if running {
            PipelineState::Ready
        } else {
            PipelineState::Idle
        };
        let label = state.label();
        let pipe = PipelineSnapshotDto {
            state,
            state_label: label,
            running,
            gate,
            last_error: None,
            latency: LatencySnapshot::default(),
        };
        SnapshotDto {
            settings: Settings::default().into(),
            has_api_key: true,
            api_keys: BTreeMap::from_iter(vox_core::settings::ModelProvider::ALL.into_iter().map(
                |provider| {
                    (
                        provider.as_id().to_string(),
                        provider == vox_core::settings::ModelProvider::Aliyun,
                    )
                },
            )),
            speak: pipe.clone(),
            listen: pipe,
            mic_active: false,
            headphones_advised: false,
            devices: DeviceSnapshotDto {
                inputs: vec![],
                outputs: vec![],
                apps: vec![],
                virtual_cable_installed: false,
                virtual_cable_status: "not_installed",
                virtual_cable_16ch_status: "absent",
            },
            usage: UsageLedger::default(),
            notices: vec![],
            // 芯的缺省事实（`HostFacts::uninjected`）：档位是 Linux 桌面，四个"要外壳接上
            // 才有"的位报 `not_wired`。产品路径上事实由外壳注入，这里只是让 DTO 有值可测。
            capabilities: CapabilityReport::of(
                &vox_core::capability::HostFacts::uninjected(),
                vox_core::settings::ModelProvider::Aliyun,
                vox_core::settings::ModelProvider::Aliyun,
            ),
            // 控制面的观察值：默认档 = 开关关着、没监听、没失败过。
            control: crate::mcp::Status {
                enabled: false,
                port: 0,
                running: false,
                bound_port: None,
                error: None,
                state_file: "/tmp/vox-test/control.json".to_string(),
            },
        }
    }

    fn sample_gate() -> GateStatus {
        GateStatus {
            kind: GateKind::Level,
            state: GateState::Speech,
            rms: 0.42,
            active: true,
            ended: false,
        }
    }

    /// 前端要的所有顶层字段都在。
    #[test]
    fn snapshot_has_all_frontend_keys() {
        let dto = make_dto(true, Some(sample_gate()));
        let json = serde_json::to_value(&dto).unwrap();

        let expected_keys = [
            "settings",
            "has_api_key",
            "api_keys",
            "speak",
            "listen",
            "mic_active",
            "headphones_advised",
            "devices",
            "usage",
            "notices",
            "capabilities",
            "control",
        ];
        for key in expected_keys {
            assert!(json.get(key).is_some(), "顶层缺字段: {key}");
        }
    }

    /// 前端拿到的 API key 状态要包含 gpt，不能只来自 v1 的两个 provider。
    #[test]
    fn api_keys_map_includes_gpt() {
        let dto = make_dto(true, None);
        let json = serde_json::to_value(&dto).unwrap();
        let keys = json.get("api_keys").unwrap().as_object().unwrap();
        assert!(keys.contains_key("aliyun"));
        assert!(keys.contains_key("gemini"));
        assert!(keys.contains_key("gpt"));
    }

    /// 不含只用于 v1 迁移的顶层模型字段——防止旧结构重新漏给前端。
    #[test]
    fn snapshot_no_stale_core_names() {
        let dto = make_dto(true, Some(sample_gate()));
        let json = serde_json::to_value(&dto).unwrap();
        let settings = json.get("settings").unwrap();

        assert!(settings.get("model_name").is_none(), "顶层共用模型已废弃");
        assert!(settings.get("model").is_none(), "旧 UI 的顶层 model 已废弃");
        assert!(settings["speak"].get("model_name").is_some());
        assert!(settings["listen"].get("model_name").is_some());
    }

    /// 两条流水线各自带服务商和模型。
    #[test]
    fn snapshot_settings_has_per_pipeline_provider_and_model() {
        let dto = make_dto(true, None);
        let json = serde_json::to_value(&dto).unwrap();
        let settings = json.get("settings").unwrap();
        for pipeline in ["speak", "listen"] {
            assert_eq!(settings[pipeline]["provider"], "aliyun");
            assert_eq!(
                settings[pipeline]["model_name"],
                Value::String(vox_core::catalog::DEFAULT_MODEL_NAME.to_string())
            );
        }
    }

    #[test]
    fn snapshot_settings_has_translation_switches() {
        let dto = make_dto(true, None);
        let settings = serde_json::to_value(&dto).unwrap()["settings"].clone();

        assert_eq!(settings["speak"]["show_translation"], true);
        assert_eq!(settings["speak"]["speak_translation"], true);
        assert_eq!(settings["listen"]["show_translation"], true);
        assert_eq!(settings["listen"]["speak_translation"], true);
    }

    /// 旧 UI 的顶层 model 会同时迁到两条流水线，新 UI 不受影响。
    #[test]
    fn patch_to_kernel_expands_legacy_model() {
        let patch = serde_json::json!({
            "model": "retired-model",
            "speak": { "enabled": true }
        });
        let out = patch_to_kernel(patch);
        assert_eq!(
            out["speak"]["model_name"],
            Value::String("retired-model".to_string())
        );
        assert_eq!(out["listen"]["model_name"], out["speak"]["model_name"]);
        assert!(out.get("model").is_none());
        assert_eq!(out["speak"]["enabled"], Value::Bool(true));
    }

    /// patch 里没带 `model` 键时不该凑一个出来。
    #[test]
    fn patch_to_kernel_no_op_without_model() {
        let patch = serde_json::json!({ "speak": { "enabled": true } });
        let out = patch_to_kernel(patch.clone());
        assert_eq!(out, patch);
    }

    /// 流水线子对象包含前端需要的四个字段。
    #[test]
    fn pipeline_dto_shape() {
        let dto = make_dto(true, Some(sample_gate()));
        let json = serde_json::to_value(&dto).unwrap();

        let speak = json.get("speak").unwrap();
        for key in ["state", "state_label", "running", "gate", "latency"] {
            assert!(speak.get(key).is_some(), "speak 缺字段: {key}");
        }
        assert_eq!(speak.get("running").unwrap(), &Value::Bool(true));
    }

    /// 不跑的时候 `gate` 序列化成 JSON `null`——不是字段消失。
    #[test]
    fn gate_is_null_when_not_running() {
        let dto = make_dto(false, None);
        let json = serde_json::to_value(&dto).unwrap();

        let speak = json.get("speak").unwrap();
        // 字段必须存在
        assert!(speak.get("gate").is_some(), "gate 字段不该消失");
        // 值必须是 null
        assert!(speak.get("gate").unwrap().is_null(), "gate 应为 null");
        assert_eq!(speak.get("running").unwrap(), &Value::Bool(false));
    }

    /// 跑起来的时候 `gate` 是五字段对象。
    #[test]
    fn gate_has_five_fields_when_running() {
        let dto = make_dto(true, Some(sample_gate()));
        let json = serde_json::to_value(&dto).unwrap();

        let gate = json.get("speak").unwrap().get("gate").unwrap();
        assert!(gate.is_object(), "gate 应为对象");
        for key in ["kind", "state", "rms", "active", "ended"] {
            assert!(gate.get(key).is_some(), "gate 缺字段: {key}");
        }
    }

    /// 能力位报告透传到前端：四个键（`tier` / `host` / `speak` / `listen`）一字不差，
    /// 每位都是 `{enabled, reason}`，关着的必带 reason（芯的 `is_consistent` 不变量）。
    #[test]
    fn capabilities_report_is_passed_through() {
        let dto = make_dto(true, None);
        let json = serde_json::to_value(&dto).unwrap();
        let caps = json.get("capabilities").expect("顶层缺 capabilities 字段");

        for key in ["tier", "host", "speak", "listen"] {
            assert!(caps.get(key).is_some(), "capabilities 缺字段: {key}");
        }
        // 档位是**名字**，不是宿主机位表（`host` 那一格才是位表）。
        assert_eq!(caps["tier"], Value::String("linux_desktop".to_string()));

        let host = caps["host"].as_object().expect("host 应是位表对象");
        assert_eq!(host.len(), vox_core::capability::Capability::HOST.len());
        for (bit, status) in host {
            let enabled = status["enabled"].as_bool().expect("位要有 enabled");
            assert_eq!(
                enabled,
                status["reason"].is_null(),
                "{bit}：开着就不许有 reason，关着必须给 reason"
            );
        }
        // 缺省事实（没注入）下这四位如实报假，且 reason 是 `not_wired`——界面靠它说
        // "这个平台还没接上"，不是"这台设备做不到"。
        assert_eq!(
            host["virtual_mic"],
            serde_json::json!({ "enabled": false, "reason": "not_wired" })
        );
        // 档位上限里的采集位照旧为真：位表不是"全都假"。
        assert_eq!(
            host["mic"],
            serde_json::json!({ "enabled": true, "reason": null })
        );

        for section in ["speak", "listen"] {
            let bits = caps[section].as_object().expect("provider 位表应是对象");
            assert_eq!(bits.len(), vox_core::capability::Capability::PROVIDER.len());
        }
    }

    /// devices 子对象里叫 `apps` 不叫 `audio_apps`。
    #[test]
    fn devices_uses_apps_not_audio_apps() {
        let dto = make_dto(false, None);
        let json = serde_json::to_value(&dto).unwrap();

        let devices = json.get("devices").unwrap();
        assert!(devices.get("apps").is_some(), "devices 缺 apps 字段");
        assert!(
            devices.get("audio_apps").is_none(),
            "devices 不该有 audio_apps"
        );
        assert!(devices.get("inputs").is_some());
        assert!(devices.get("outputs").is_some());
    }

    /// 控制面那两格：`settings.control`（要什么）与 `control`（起没起）。
    ///
    /// 界面靠后者说"没在跑"（不许拿设置自己推），所以字段名与取值都要钉住：
    /// `running` / `bound_port` 是事实，`enabled` / `port` 是后端**按下去的那一档**。
    #[test]
    fn control_settings_and_status_are_both_exposed() {
        let mut dto = make_dto(false, None);
        dto.settings.control.enabled = true;
        dto.settings.control.port = 47123;
        dto.control = crate::mcp::Status {
            enabled: true,
            port: 47123,
            running: true,
            bound_port: Some(47123),
            error: None,
            state_file: "/cfg/control.json".to_string(),
        };
        let json = serde_json::to_value(&dto).unwrap();

        // 设置那一半：开关、端口、四个授权位、去抖间隔——设置页那一屏要读写它们。
        let control = &json["settings"]["control"];
        for key in [
            "enabled",
            "port",
            "allow_microphone",
            "allow_system_audio",
            "allow_audible_output",
            "allow_config_write",
            "transcript_notify_ms",
        ] {
            assert!(control.get(key).is_some(), "settings.control 缺字段: {key}");
        }
        assert_eq!(control["enabled"], Value::Bool(true));
        assert_eq!(
            control["transcript_notify_ms"],
            Value::from(vox_core::settings::DEFAULT_TRANSCRIPT_NOTIFY_MS)
        );

        // 观察那一半：六个字段一字不差。
        let status = &json["control"];
        for key in [
            "enabled",
            "port",
            "running",
            "bound_port",
            "error",
            "state_file",
        ] {
            assert!(status.get(key).is_some(), "control 缺字段: {key}");
        }
        assert_eq!(status["running"], Value::Bool(true));
        assert_eq!(status["bound_port"], Value::from(47123));

        // 没在跑时 `bound_port` / `error` 是**显式 null**，不是字段消失。
        let mut idle = make_dto(false, None);
        idle.control.bound_port = None;
        let json = serde_json::to_value(&idle).unwrap();
        assert!(
            json["control"]["bound_port"].is_null(),
            "没在跑时 bound_port 要显式 null"
        );
        assert!(json["control"]["error"].is_null());
        assert_eq!(json["control"]["running"], Value::Bool(false));
    }
}
