//! 端点投影：`Settings` + `HostFacts` → `Composition`，以及它的**反方向**（清单 → `Settings`）。
//!
//! 这个文件是 `describe_endpoint` / `compose_endpoint` / `list_endpoints` 的全部实现，三件事
//! 各只有一处：
//!
//! - **投影**：`Composition::of(&ledger.session_config(endpoint), &ledger.host_facts())`——
//!   S0 的 D1 **唯一签名**。事实只从账本那份 `HostFacts` 取，本 crate 不另立第二份；
//!   `SessionConfig` 只从芯的 `Runtime::session_config` 取，本 crate 不另映射一份。
//! - **`editable` 表**（§2.1.4 的键）：告诉调用方"这份清单里哪几格能改"。`ops[gate]` 那两行
//!   按 **D6** 排除——闸门配置仍经 `SessionConfig.gate` 走，`Settings` 里没有 `tail_ms` /
//!   `preroll_ms` 的存储，把它们列进 `editable` 就是让调用方做空动作。
//! - **反方向表**（同一节，一格一条 [`Cell`]）：把清单里的差异翻译回 `Settings` 的写入。
//! - **清单文档**（[`document`]）：`capabilities` + 两条腿 + `errors` 那四个键——两个
//!   `--print-composition` 入口（无屏档 / 桌面档）打的就是它，组装也只有这一处。
//!
//! `compose_endpoint` 的四步**顺序本身就是设计**（§2.1.3-③）：
//!
//! ```text
//! validate() → missing_on() → diff → dry-run / apply
//! ```
//!
//! `missing_on` **必须**排在 diff 前面：反过来的话，"拿错档位的清单"（`host: "android"` 发到
//! Windows）会先撞 `unsupported_field`（`host` 这一格不在 `editable` 里），`HostMismatch`
//! 永远不可达。同理 `in: []` 在**第一道闸** `validate()` 就被拒（`missing_input`，D4），
//! 不许拖到 `Plan::from` 才炸。
//!
//! 最后一格是**机械核对**：反写出来的设置重新派生一遍，必须与提交的清单逐格相同（[`residual`]）。
//! `voice = null` 却留着 `out[playback]`、给 listen 塞 `denoise`、把输入换成 `host_feed`……
//! 都在这儿被逮住——**不用手写每一条规则**，也不靠"看起来对"。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use vox_core::capability::{Capability, CapabilityReport, HostFacts, UnavailableReason};
use vox_core::cloud::CloneFrequency;
use vox_core::composition::{Composition, CompositionError, Input, Op, Output, PlaybackRole};
use vox_core::event::Pipeline;
use vox_core::ports::AudioApp;
use vox_core::runtime::Runtime;
use vox_core::settings::Settings;

use crate::actions::{
    action_by_id, ActionId, DomainError, DomainErrorCode, EndpointId, Permission,
};
use crate::handlers::CallFailure;
use crate::ledger::{Grants, Ledger};
use crate::session::Tokens;

/// 一个端点在控制面上的静态身份（§2.1.3-①②）。**数据**：标题、摘要、可改的格都在这张表里。
pub struct Endpoint {
    pub id: EndpointId,
    /// 这个端点就是哪条流水线（`Settings` / `Runtime` 那侧的身份）。
    pub pipeline: Pipeline,
    pub summary: &'static str,
    /// `editable` 的键：**逐字**照 §2.1.4（标了"本轮不在 `editable`"的两行不在这里）。
    pub cells: &'static [Cell],
}

/// 一条映射行：清单里**能改**的一格 ↔ `Settings` 里的一格（§2.1.4 那张表的实现）。
///
/// `key` 是给调用方看的（`editable` 里的原文），`path` 是清单里的规范化路径（数组按 `kind`
/// 取键，见 [`flatten`]）——两者不是一套写法：`editable` 照设计稿逐字，`changed[].path` 用
/// 规范化路径（`session.params.target_language` 这种能直接指到格子的写法）。
pub struct Cell {
    pub key: &'static str,
    pub path: &'static str,
    /// 反写：把**提交的清单**里这一格写进 `Settings`。
    ///
    /// 签名统一成"吃整份清单"而不是"吃这一格的值"：清单里有几格是**联动**的
    /// （`session` 有没有 ⇔ 直通 ⇔ 有没有 resample / captions；`voice` 有没有 ⇔ 播放汇有没有），
    /// 单独看一格判不出来。
    write: fn(&mut Settings, &Composition, &[AudioApp]) -> Result<(), Unsupported>,
}

/// 一条"这一格不能改"的判定结果（`unsupported_field` 的 detail 来源）。
struct Unsupported {
    path: String,
    why: String,
}

impl Unsupported {
    fn new(path: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            why: why.into(),
        }
    }

    /// `structuredContent.error` 的 `detail`。
    fn detail(&self, expected: Option<Value>) -> Value {
        let mut detail = json!({ "path": self.path, "why": self.why });
        if let Some(expected) = expected {
            detail["expected"] = expected;
        }
        detail
    }
}

/// 两个端点。**顺序即 `list_endpoints` 的顺序**（与 `ACTIONS` 的顺序无关：那是工具的序）。
pub static ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        id: EndpointId::Speak,
        pipeline: Pipeline::Speak,
        summary: "麦克风 → 译音进虚拟麦 + 字幕",
        cells: &SPEAK_CELLS,
    },
    Endpoint {
        id: EndpointId::Listen,
        pipeline: Pipeline::Listen,
        summary: "抓某个程序的声音 → 中文语音 + 字幕",
        cells: &LISTEN_CELLS,
    },
];

/// `speak` 的反方向表（§2.1.4）。**不含 `ops[gate]` 那两行**（D6，见模块头）。
static SPEAK_CELLS: [Cell; 9] = [
    Cell {
        key: "in[0].device",
        path: "in[mic].device",
        write: |settings, submitted, _| {
            settings.speak.input_device = mic_device(submitted);
            Ok(())
        },
    },
    Cell {
        key: "ops[denoise] 有无",
        path: "ops[denoise]",
        write: |settings, submitted, _| {
            settings.speak.denoise = has_op(submitted, "denoise");
            Ok(())
        },
    },
    Cell {
        key: "out[playback(primary)].device",
        path: "out[playback(primary)].device",
        write: |settings, submitted, _| {
            settings.speak.output_device = primary_device(submitted);
            Ok(())
        },
    },
    Cell {
        key: "out[playback(monitor)] 有无",
        path: "out[playback(monitor)]",
        write: |settings, submitted, _| {
            settings.speak.monitor_translation = has_playback(submitted, PlaybackRole::Monitor);
            Ok(())
        },
    },
    Cell {
        key: "session.provider",
        path: "session.provider",
        write: |settings, submitted, _| {
            if let Some(session) = &submitted.session {
                settings.speak.provider = session.provider;
            }
            Ok(())
        },
    },
    Cell {
        key: "session.params.target_language",
        path: "session.params.target_language",
        write: |settings, submitted, _| {
            if let Some(session) = &submitted.session {
                settings.speak.target_language = session.params.target_language.clone();
            }
            Ok(())
        },
    },
    Cell {
        key: "session.params.voice",
        path: "session.params.voice",
        write: |settings, submitted, _| {
            let voice = session_voice(submitted);
            settings.speak.speak_translation = voice.is_some();
            if let Some(voice) = voice {
                settings.speak.voice = voice;
            }
            Ok(())
        },
    },
    Cell {
        key: "session.params.clone_frequency",
        path: "session.params.clone_frequency",
        write: |settings, submitted, _| {
            if let Some(session) = &submitted.session {
                settings.speak.voice_clone_frequency = session.params.clone_frequency.map(count_of);
            }
            Ok(())
        },
    },
    Cell {
        key: "session 有无（= 直通）",
        path: "session",
        write: |settings, submitted, _| {
            // 有没有 `session` 就是"走不走云端"：没有 = 原声直通（`Settings.speak.translate`）。
            // 整条会话一起写：从"直通"改回"翻译"时 provider / 语言 / 音色也得跟着回来，
            // 否则重新派生出来的清单与提交的那份对不上（[`residual`] 会逮住）。
            match &submitted.session {
                None => {
                    settings.speak.translate = false;
                    settings.speak.speak_translation = false;
                }
                Some(session) => {
                    settings.speak.translate = true;
                    settings.speak.provider = session.provider;
                    settings.speak.target_language = session.params.target_language.clone();
                    settings.speak.voice_clone_frequency =
                        session.params.clone_frequency.map(count_of);
                    let voice = session.params.voice.clone();
                    settings.speak.speak_translation = voice.is_some();
                    if let Some(voice) = voice {
                        settings.speak.voice = voice;
                    }
                }
            }
            Ok(())
        },
    },
];

/// `listen` 的反方向表（§2.1.4）：把 `input_device` / `clone_frequency` 换成
/// `in[0].executable` / `in[0].include_tree` / `session.params.source_language`。
static LISTEN_CELLS: [Cell; 6] = [
    Cell {
        key: "in[0].executable",
        path: "in[process_loopback].executable",
        write: |settings, submitted, apps| {
            let Some((executable, include_tree)) = loopback(submitted) else {
                return Ok(());
            };
            if settings
                .listen
                .target
                .as_ref()
                .map(|target| target.executable.as_str())
                == Some(executable)
            {
                return Ok(());
            }
            // `display_name` **不在清单里**：按 executable 去设备表里查（§2.1.4）。查不到就让
            // 用户先在界面里选一次——编一个名字出来只会让设置里出现一个抓不到的程序。
            let app = apps
                .iter()
                .find(|app| app.executable == executable)
                .ok_or_else(|| {
                    Unsupported::new(
                        "in[0].executable",
                        "这个程序不在本机的可抓列表里（`DeviceRegistry::audio_apps`）：先在界面里选一次",
                    )
                })?;
            settings.listen.target = Some(vox_core::settings::ListenTarget {
                executable: app.executable.clone(),
                display_name: app.display_name.clone(),
                include_process_tree: include_tree,
            });
            Ok(())
        },
    },
    Cell {
        key: "in[0].include_tree",
        path: "in[process_loopback].include_tree",
        write: |settings, submitted, _| {
            let Some((_, include_tree)) = loopback(submitted) else {
                return Ok(());
            };
            if let Some(target) = &mut settings.listen.target {
                target.include_process_tree = include_tree;
            }
            Ok(())
        },
    },
    Cell {
        key: "out[playback(primary)].device",
        path: "out[playback(primary)].device",
        write: |settings, submitted, _| {
            settings.listen.output_device = primary_device(submitted);
            Ok(())
        },
    },
    Cell {
        key: "session.provider",
        path: "session.provider",
        write: |settings, submitted, _| {
            if let Some(session) = &submitted.session {
                settings.listen.provider = session.provider;
            }
            Ok(())
        },
    },
    Cell {
        key: "session.params.voice",
        path: "session.params.voice",
        write: |settings, submitted, _| {
            let voice = session_voice(submitted);
            settings.listen.speak_translation = voice.is_some();
            if let Some(voice) = voice {
                settings.listen.voice = voice;
            }
            Ok(())
        },
    },
    Cell {
        key: "session.params.source_language",
        path: "session.params.source_language",
        write: |settings, submitted, _| {
            if let Some(session) = &submitted.session {
                settings.listen.source_language = session.params.source_language.clone();
            }
            Ok(())
        },
    },
];

/// 这个端点属于哪条流水线。
pub fn pipeline(endpoint: EndpointId) -> Pipeline {
    get(endpoint).pipeline
}

/// 端点目录项。两个 id 都有条目（`EndpointId::ALL` 与 `ENDPOINTS` 由用例钉住）。
pub fn get(endpoint: EndpointId) -> &'static Endpoint {
    ENDPOINTS
        .iter()
        .find(|entry| entry.id == endpoint)
        .expect("两个端点都在目录里")
}

/// `editable`：这份清单里哪几格可以改（§2.1.4 的键，逐字）。
pub fn editable(endpoint: EndpointId) -> Vec<&'static str> {
    get(endpoint).cells.iter().map(|cell| cell.key).collect()
}

// --- 投影 ------------------------------------------------------------------

/// 这条腿**当前**的清单。派生路径只有这一条（S0 的 D1）：
/// `Settings` ──`Runtime::session_config`──► `SessionConfig` ──`Composition::of`──► 清单。
///
/// 失败只有一种可能：这条腿现在派不出清单（听人说话还没选要抓的程序）。那是**领域失败**
/// （`endpoint_unavailable`），不是协议错误——清单本身没毛病，是账本里缺一个选择。
pub fn manifest(ledger: &dyn Ledger, endpoint: EndpointId) -> Result<Composition, DomainError> {
    let config = ledger.session_config(endpoint);
    let facts = ledger.host_facts();
    Composition::of(&config, &facts).map_err(|error| {
        DomainError::with_detail(
            DomainErrorCode::EndpointUnavailable,
            error.to_string(),
            json!({
                "errors": [],
                "hint": "先在设置里选一个要抓的程序（`listen.target`），控制面才能派生这份清单",
            }),
        )
    })
}

/// 清单的**线上形态**：先 `to_string` 再读回来。
///
/// 中间那一趟不是多余：`to_value` 会把 `f32` 摊成 `f64`（`0.012` →
/// `0.012000000104308128`），而线上走的是 `0.012` 那种文本——比的和报的都得是客户端看到的
/// 那一种（与芯的 `wire()` 同口径）。
pub fn wire(composition: &Composition) -> Value {
    let text = serde_json::to_string(composition).expect("清单是数据，要能序列化");
    serde_json::from_str(&text).expect("刚序列化出来的文本必须读得回")
}

/// 清单**文档**（S0 §4.3-A）：`capabilities` + 两条腿的清单 + 派生不出来的理由。
///
/// 打这份文档的两个入口——无屏档的 `--print-composition`（`vox-headless`）与桌面档的
/// 同一个开关（`app`）——都调这里，**组装只有这一处**：两边打出来的**形状**（键名、嵌套、
/// 键顺序）逐字相同，**取值随档位与宿主事实本就不同**（能力位上限、`host`、设备名都该不一样
/// ——同形说的是骨架，不是内容；把取值也读成"逐字相同"就会把两档该有的差别当成 bug）。
/// 这份文档的形状（缩进 JSON，`jq -c` 一行照样能用；键名与 S0 §4.3-A 逐字）：
///
/// ```text
/// capabilities  <CapabilityReport>       // 当前**有效**位（档位上限 − 关掉的）
/// speak         <Composition> | null     // 这条腿现在派出来的清单（[`wire`] 那一份）
/// listen        <Composition> | null     //   派不出来就是 null，理由进 errors
/// errors        [ { endpoint, code, message, detail? } ]
/// ```
///
/// 上面那一块列的是"有哪几个键"，**不是文本顺序**：`serde_json` 缺省不开 `preserve_order`，
/// 所以打出来的顶层顺序是 `capabilities` → `errors` → `listen` → `speak`（读它按名字取，
/// 别按顺序写脚本）。
///
/// **两条腿都打**：派不出来的那条腿是 `null` + `errors` 里一条 `endpoint_unavailable`——
/// 打一份看起来像清单的假清单比打 `null` 糟得多。派生走 [`manifest`]（**与 S1 的
/// `describe_endpoint` 同一个函数**），清单的线上形态走 [`wire`]，所以这份打印与 Agent 面
/// 看到的逐字同形。
///
/// `on_failure` 让入口把"哪条腿派不出来"写进自己的日志（桌面档进日志文件、无屏档进 journal）：
/// 本 crate 的依赖表里没有 `tracing`（`Cargo.toml` 里那张"零新依赖"的规矩），所以**日志往哪走
/// 由入口定**；逐条报出去的还是这里那一份数据（`EndpointId` + 芯的 `DomainError`），
/// `errors` 数组里也是同一份——不是第二处派生。
pub fn document(
    ledger: &dyn Ledger,
    on_failure: &mut dyn FnMut(EndpointId, &DomainError),
) -> Result<String, serde_json::Error> {
    let mut document = serde_json::Map::new();
    document.insert(
        "capabilities".to_string(),
        serde_json::to_value(ledger.capabilities())?,
    );

    let mut errors = Vec::new();
    for endpoint in EndpointId::ALL {
        let endpoint = *endpoint;
        let key = endpoint.as_str().to_string();
        match manifest(ledger, endpoint) {
            Ok(composition) => {
                document.insert(key, wire(&composition));
            }
            Err(error) => {
                on_failure(endpoint, &error);
                let mut entry = error.to_json();
                entry["endpoint"] = Value::String(key.clone());
                document.insert(key, Value::Null);
                errors.push(entry);
            }
        }
    }
    document.insert("errors".to_string(), Value::Array(errors));

    serde_json::to_string_pretty(&Value::Object(document))
}

/// `CompositionError` 的线上形态（`{"kind": …}` 判别键，形状由芯定）。
fn errors_json(errors: &[CompositionError]) -> Value {
    serde_json::to_value(errors).expect("CompositionError 的 serde 由芯定，且不带非字符串键")
}

/// 把一个 `CompositionError` 渲染成**数据**（不写句子、不自己拼文案）：
/// `unavailable_reason` 用它，形状就是芯定的那个 `{"kind": …}`。
fn render_error(error: &CompositionError) -> String {
    serde_json::to_string(error).expect("CompositionError 的 serde 由芯定")
}

/// 一个位的关闭原因（芯的 `UnavailableReason` 的 snake_case 名）。上限之外一律 `unsupported`。
fn reason_id(facts: &HostFacts, bit: Capability) -> &'static str {
    match facts.off.get(&bit) {
        None => "unsupported",
        Some(UnavailableReason::Unsupported) => "unsupported",
        Some(UnavailableReason::NotInstalled) => "not_installed",
        Some(UnavailableReason::Permission) => "permission",
        Some(UnavailableReason::NotBuilt) => "not_built",
        Some(UnavailableReason::NotWired) => "not_wired",
        Some(UnavailableReason::PendingReboot) => "pending_reboot",
        Some(UnavailableReason::Busy) => "busy",
    }
}

/// OS 侧的授权事实。**唯一出处**是事实表：`off[bit] == permission` 就是"系统没让"
/// （S0 §5.1 指定的那一条）。其余原因（没装 / 没接线 / 被占）都**不是** OS 拒绝。
fn os_granted(facts: &HostFacts, permission: Permission) -> bool {
    let bit = match permission {
        Permission::Microphone => Capability::Mic,
        Permission::SystemAudio => Capability::ProgramTap,
        // 出声没有对应的位：任何档位都能出声，谈不上"系统没让"。
        Permission::AudibleOutput => return true,
    };
    facts.off.get(&bit) != Some(&UnavailableReason::Permission)
}

/// 这个端点要的用户授权位（取 `session_open` 那份：describe 自己不碰设备，但要如实报
/// "开这条腿需要用户先开哪几位"——§2.1.3-② 的 `permissions` 就是这个意思）。
fn permissions_of(endpoint: EndpointId) -> &'static [Permission] {
    action_by_id(ActionId::SessionOpen)
        .permissions
        .for_endpoint(Some(endpoint))
}

/// `permissions` 那一格：逐位给"用户让不让"与"系统让不让"。
fn permissions_json(ledger: &dyn Ledger, grants: &dyn Grants, endpoint: EndpointId) -> Value {
    let facts = ledger.host_facts();
    Value::Array(
        permissions_of(endpoint)
            .iter()
            .map(|permission| {
                json!({
                    "permission": permission.as_str(),
                    "user_granted": grants.user_granted(*permission),
                    "os_granted": os_granted(&facts, *permission),
                })
            })
            .collect(),
    )
}

/// 本机能不能开这个端点：清单过得了**两道闸**（`validate()` + `missing_on()`）才算能开。
/// `None` = 能开；`Some(reason)` = 不能开，附一条**数据**原因（`list_endpoints` 用它）。
fn availability(
    entry: &Endpoint,
    manifest: &Composition,
    caps: &CapabilityReport,
    facts: &HostFacts,
) -> Option<String> {
    let mut reasons = Vec::new();
    // 入口那一位为假 → 这一格根本进不了清单。`validate()` 随后会报 `missing_input`，但
    // "为什么没有输入口"的答案在**事实**里，不在清单里——两个都给，调用方才拼得出全貌。
    if manifest.r#in.is_empty() {
        let bit = match entry.pipeline {
            Pipeline::Speak => Capability::Mic,
            Pipeline::Listen => Capability::ProgramTap,
        };
        reasons.push(format!("{}: {}", bit.id(), reason_id(facts, bit)));
    }
    if let Err(errors) = manifest.validate() {
        reasons.extend(errors.iter().map(render_error));
    }
    reasons.extend(manifest.missing_on(caps).iter().map(render_error));
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

/// 事实让清单降级时的一条**数据**说明（不是文案）：虚拟麦位假 → 播放汇退成普通出声。
fn downgrade_note(
    endpoint: EndpointId,
    manifest: &Composition,
    facts: &HostFacts,
) -> Option<String> {
    if endpoint != EndpointId::Speak {
        return None;
    }
    let playback = primary_playback(manifest)?;
    let Output::Playback { role, .. } = playback else {
        return None;
    };
    if *role != PlaybackRole::Speaker {
        return None;
    }
    Some(format!(
        "virtual_mic=off({}) → out[playback(primary)].role=speaker",
        reason_id(facts, Capability::VirtualMic)
    ))
}

// --- 三个动作 --------------------------------------------------------------

/// `list_endpoints`：这台设备能开的端点 + 本机控制面通道。
pub fn list(ledger: &dyn Ledger) -> Result<Value, CallFailure> {
    let caps = ledger.capabilities();
    let facts = ledger.host_facts();
    let mut endpoints = Vec::with_capacity(ENDPOINTS.len());
    for entry in ENDPOINTS {
        let mut item = json!({
            "id": entry.id.as_str(),
            "title": entry.pipeline.label(),
            "running": ledger.pipeline_state(entry.id).is_running(),
            "summary": entry.summary,
        });
        match manifest(ledger, entry.id) {
            Ok(current) => match availability(entry, &current, &caps, &facts) {
                None => item["available"] = json!(true),
                Some(reason) => {
                    item["available"] = json!(false);
                    item["unavailable_reason"] = json!(reason);
                }
            },
            // 连清单都派不出来（还没选要抓的程序）：照实说，别编一个 available。
            Err(error) => {
                item["available"] = json!(false);
                item["unavailable_reason"] = json!(error.message);
            }
        }
        endpoints.push(item);
    }

    // 控制面通道取**清单自己**的那一格（`control`），不另立一份"这台设备支持什么通道"的表。
    let control = match manifest(ledger, EndpointId::Speak) {
        Ok(current) => serde_json::to_value(&current.control).expect("Control 是标量枚举"),
        Err(_) => json!([]),
    };

    Ok(json!({
        "device": { "tier": caps.tier, "control": control },
        "endpoints": endpoints,
    }))
}

/// `describe_endpoint`：清单投影 + 能力位 + 权限状态 + 可改的格。
pub fn describe(
    ledger: &dyn Ledger,
    grants: &dyn Grants,
    endpoint: EndpointId,
) -> Result<Value, CallFailure> {
    let entry = get(endpoint);
    let current = manifest(ledger, endpoint)?;
    let caps = ledger.capabilities();
    let facts = ledger.host_facts();

    let mut described = json!({
        "endpoint": entry.id.as_str(),
        "title": entry.pipeline.label(),
        "available": availability(entry, &current, &caps, &facts).is_none(),
        "manifest": wire(&current),
        "capabilities": serde_json::to_value(&caps).expect("能力位报告是数据"),
        "permissions": permissions_json(ledger, grants, endpoint),
        "editable": editable(endpoint),
        "running": ledger.pipeline_state(endpoint).is_running(),
    });
    if let Some(note) = downgrade_note(endpoint, &current, &facts) {
        described["notes"] = json!([note]);
    }
    Ok(described)
}

/// `compose_endpoint`：读—改—写，四步顺序见模块头。
///
/// `apply == false` 只算差异并签发 token；`apply == true` 校验 token（匹配 + 未过期 + 未用过）
/// 后落进账本。**用户同意落在 token 上**：dry-run 是默认值，第二次调用必须带同一个 token，
/// 中间那一步宿主/人可以先看 diff（协议无状态，token 就是服务端签发的普通参数）。
pub fn compose(
    ledger: &dyn Ledger,
    grants: &dyn Grants,
    tokens: &mut Tokens,
    endpoint: EndpointId,
    composition: Value,
    apply: bool,
    token: Option<String>,
) -> Result<Value, CallFailure> {
    // 第 0 步：`composition` 必须真的是一份清单。serde 都过不去的值连 `CompositionError`
    // 都产生不了（它连结构都不成立），所以这一条用 `malformed` + serde 的原话。
    let submitted: Composition =
        serde_json::from_value(composition).map_err(|error| CallFailure::InvalidParams {
            message: "composition 不是一份清单（S0 的 serde 形态）".to_string(),
            errors: json!([{ "kind": "malformed", "reason": error.to_string() }]),
        })?;

    // 第 1 步：结构性校验。一次给**全部**问题（不是遇错就返回）。
    // `in: []` 在这里就被拒（`missing_input`，D4），走协议错误通道（-32602 + data.errors）。
    if let Err(errors) = submitted.validate() {
        return Err(CallFailure::InvalidParams {
            message: "清单不满足 Composition::validate()".to_string(),
            errors: errors_json(&errors),
        });
    }

    // 第 2 步：这份清单装得到这台机器上吗。`HostMismatch` / `MissingCapability` 在这一步
    // 回答（**必须在 diff 之前**，理由见模块头）。
    let caps = ledger.capabilities();
    let missing = submitted.missing_on(&caps);
    if !missing.is_empty() {
        return Err(DomainError::with_detail(
            DomainErrorCode::EndpointUnavailable,
            "这份清单装不到这台机器上",
            json!({ "errors": errors_json(&missing) }),
        )
        .into());
    }

    // 第 3 步：与**当前清单**算差异。当前清单与本机派生走的是同一条路径、同一份事实。
    let current = manifest(ledger, endpoint)?;
    let changed = diff(&flatten(&current), &flatten(&submitted));

    // 改 `editable` 之外的格 → `unsupported_field`（附 path / why / expected）。
    for change in &changed {
        if is_editable(endpoint, &change.path) || removable_playback(change, &submitted) {
            continue;
        }
        return Err(unsupported(endpoint, &change.path, &submitted, &current, None).into());
    }

    // 反写 + **机械核对**：照反方向表算出草稿设置，重新派生一遍，必须与提交的清单逐格相同。
    // 到这一步为止都还没碰账本。
    let draft = draft_settings(ledger, endpoint, &submitted, &changed, &current)?;

    // 第 4 步：dry-run / apply。
    let manifest_text = serde_json::to_string(&submitted).expect("清单是数据，要能序列化");
    if !apply {
        let (token, expires_in_ms) = tokens.sign(endpoint, manifest_text, ledger.now_ms());
        return Ok(json!({
            "endpoint": endpoint.as_str(),
            "applied": false,
            "changed": changes_json(&changed),
            "manifest": wire(&submitted),
            "token": token,
            "expires_in_ms": expires_in_ms,
        }));
    }

    // `apply` 才要用户授权位：dry-run 不写任何东西，凭什么拦（"用户同意"落在 token 上）。
    // 门**由动作表那一格说了算**（`Action::writes_config`，在同名的 `actions.rs`），不是这里
    // 硬编码的"compose 才要授权"：`actions.rs` 模块头那句"授权只看 permissions 与
    // writes_config"要是真的，就不能让这张表只在 `destructiveHint` 上生效。表里这一格改成
    // `false`，这道门就没有了——那条由 `tests/endpoints.rs::the_default_grants_deny_everything`
    // 钉着。
    if action_by_id(ActionId::ComposeEndpoint).writes_config && !grants.config_write_allowed() {
        return Err(DomainError::with_detail(
            DomainErrorCode::ConfigWriteDenied,
            "用户没有允许控制面改配置",
            json!({ "hint": "在设置 →「Agent 控制面」里打开「允许改配置」（`control.allow_config_write`）" }),
        )
        .into());
    }

    // token：匹配 + 未过期 + 未用过（一次性）。过期/重放 → `compose_token_stale`，
    // 附**重新算好的** diff 与剩余有效期。
    if let Err(expires_in_ms) =
        tokens.redeem(endpoint, token.as_deref(), &manifest_text, ledger.now_ms())
    {
        return Err(DomainError::with_detail(
            DomainErrorCode::ComposeTokenStale,
            "token 过期、不匹配或已经用过",
            json!({ "changed": changes_json(&changed), "expires_in_ms": expires_in_ms }),
        )
        .into());
    }

    // 落进账本：唯一写入口（事件 / 落盘 / 界面刷新都跟着它走）。
    if !changed.is_empty() {
        ledger.update_settings(&mut |settings| settings.clone_from(&draft));
    }
    // 返回**账本重新派生**的那份（不是"提交的那份"）：客户端看到的是真生效的东西。
    let applied = manifest(ledger, endpoint)?;
    Ok(json!({
        "endpoint": endpoint.as_str(),
        "applied": true,
        "changed": changes_json(&changed),
        "manifest": wire(&applied),
    }))
}

/// 反写 + 核对。返回可以落进账本的设置（**还没写**）。
fn draft_settings(
    ledger: &dyn Ledger,
    endpoint: EndpointId,
    submitted: &Composition,
    changed: &[Change],
    current: &Composition,
) -> Result<Settings, CallFailure> {
    let mut draft = ledger.settings();
    // 设备表只有 listen 的 `executable` 那一格要用（`display_name` 不在清单里，得按可执行名查）。
    // 别的改动不为它去克隆一次设备快照。
    let apps = if changed
        .iter()
        .any(|change| change.path == "in[process_loopback].executable")
    {
        ledger.audio_apps()
    } else {
        Vec::new()
    };
    for change in changed {
        // 例外格（跟着 `voice = null` 一起消失的播放汇）没有对应的 `Cell`：它的效果由
        // `session.params.voice` 那一格写出来。
        let Some(cell) = get(endpoint)
            .cells
            .iter()
            .find(|cell| cell.path == change.path)
        else {
            continue;
        };
        if let Err(problem) = (cell.write)(&mut draft, submitted, &apps) {
            let why = problem.why;
            let path = problem.path;
            return Err(unsupported(endpoint, &path, submitted, current, Some(why)).into());
        }
    }
    // 芯的 `update_settings` 会 `normalize()`；草稿先过一遍，核对的就是真会落盘的那一份。
    draft.normalize();
    residual(ledger, endpoint, &draft, submitted, current)?;
    Ok(draft)
}

/// **机械核对**：照草稿设置重新派生一遍，与提交的清单逐格比。
///
/// 这一步是反方向表的证明：单看某一格永远判不出"这组改动自洽不自洽"（`voice = null` 与
/// 播放汇有没有、`session` 有没有与 resample / captions、listen 不该有 `denoise`……），
/// 而重新派生一次就全都对上了——**不写第二条规则表**。
fn residual(
    ledger: &dyn Ledger,
    endpoint: EndpointId,
    draft: &Settings,
    submitted: &Composition,
    current: &Composition,
) -> Result<(), CallFailure> {
    let facts = ledger.host_facts();
    let config = Runtime::session_config_for(draft, pipeline(endpoint));
    let derived = Composition::of(&config, &facts).map_err(|error| {
        unsupported(
            endpoint,
            "in[0]",
            submitted,
            current,
            Some(error.to_string()),
        )
    })?;
    let differences = diff(&flatten(&derived), &flatten(submitted));
    match differences.first() {
        None => Ok(()),
        Some(change) => Err(unsupported(
            endpoint,
            &change.path,
            submitted,
            current,
            Some(format!(
                "{}；照反方向表写完之后重新派生的清单与提交的不一致",
                why(endpoint, &change.path, submitted)
            )),
        )
        .into()),
    }
}

/// `unsupported_field` 的错误对象。`expected` 取**当前**清单里那一格的值（有就给）。
fn unsupported(
    endpoint: EndpointId,
    path: &str,
    submitted: &Composition,
    current: &Composition,
    why_override: Option<String>,
) -> DomainError {
    let why = why_override.unwrap_or_else(|| why(endpoint, path, submitted));
    let expected = flatten(current)
        .get(path)
        .cloned()
        .filter(|value| !value.is_null());
    DomainError::with_detail(
        DomainErrorCode::UnsupportedField,
        "清单里改了 `editable` 之外的格",
        Unsupported::new(path, why).detail(expected),
    )
}

/// 这一格能不能改：`editable` 表里有没有它。
fn is_editable(endpoint: EndpointId, path: &str) -> bool {
    get(endpoint).cells.iter().any(|cell| cell.path == path)
}

/// 唯一一条"不在 `editable` 里但跟着别人一起改"的例外：**主播放汇整条消失**。
///
/// §2.1.4 的 `session.params.voice` 那一行明写"`voice = null` ⇒ 那条播放汇要**一起删掉**"——
/// 调用方改的是 `voice`（在 `editable` 里），那条播放汇是被它带走的，不是单独改的。所以
/// "voice 归零 **且** 这一条没了"算一次合法改动；反过来（这一条没了而 `voice` 还在）还是
/// `unsupported_field`。至于这条腿到底做不做得到"只要文字不要语音"，由 [`residual`] 说了算。
fn removable_playback(change: &Change, submitted: &Composition) -> bool {
    change.path == "out[playback(primary)]"
        && change.to.is_null()
        && submitted.session.is_some()
        && session_voice(submitted).is_none()
}

/// 为什么这一格不可改（§2.1.4 的"规则 / 依据"列）。**一处**，不散在各分支里。
fn why(endpoint: EndpointId, path: &str, submitted: &Composition) -> String {
    let fixed = "设备 / 外壳属性（不是会话选项）：必须与当前值逐字相同";
    match path {
        "schema_version" | "host" | "life" | "ui" | "control" => fixed.to_string(),
        "session" => "听人说话恒有云端会话（去掉它 = 没有译文与字幕），不是可选项".to_string(),
        "session.hot_update" | "session.uplink_rate" | "session.downlink_rate" => {
            "由端点固定：只有对外说话认热更新，采样率由 provider 决定（S0 §2.3）".to_string()
        }
        "ops[mono]" => "`ops[mono]` 恒在（现状恒有）：删掉它等于改了算子链".to_string(),
        "ops[resample]" => "`ops[resample]` 与 `session` 联动：直通就没有重采样".to_string(),
        "out[captions].track" => {
            "`track` 是清单里唯一的腿身份泄漏，不许用它反推端点（S0 §2.7）".to_string()
        }
        "session.params.target_language" => {
            "听的方向恒译成中文（S0 §2.3）：这条腿没有目标语言这一格".to_string()
        }
        // `session.params.voice` **在** `editable` 里（换音色就走它），所以走到这一格的
        // 只有一种情况：`residual` 发现"照反方向表写完也表达不了"（`voice = null` 那一路）。
        "session.params.voice" => match endpoint {
            EndpointId::Speak => {
                "对外说话恒有译音（`speak_translation` 恒真）：这条腿没有「只要文字不要语音」，\
                 `voice` 不能是 null；换音色请填一个音色名"
                    .to_string()
            }
            EndpointId::Listen => {
                "`voice = null` 时这条腿没有播放汇：这一格要跟 `out[playback(primary)]` 一起改（§2.1.4）"
                    .to_string()
            }
        },
        "session.params.clone_frequency" => {
            "听别人说话没有『复刻我的音色』这回事（S0 §2.3）".to_string()
        }
        "session.params.source_language" => {
            "只有听人说话有源语言：对外说话的那格不在清单里（S0 §2.3）".to_string()
        }
        "session.params.model_name" => {
            "模型名由能力表固定（`Settings` 里那格只为兼容旧配置）".to_string()
        }
        _ if path.starts_with("ops[gate]") => {
            "闸门配置仍经 `SessionConfig.gate` 走（D6）：`Settings` 里没有 `tail_ms` / \
             `preroll_ms` 的存储，改它是空动作"
                .to_string()
        }
        _ if path.starts_with("out[captions]") => {
            "字幕出口恒存在（有会话时），与任何开关无关：要不要显示是视图开关，不进清单"
                .to_string()
        }
        _ if path.starts_with("out[playback(primary)]") => {
            if session_voice(submitted).is_none() && submitted.session.is_some() {
                // 对外说话的产品定义就是"把译音送进目标应用"：`Settings::normalize` 把
                // `speak_translation` 钉死为真，所以这条腿做不到"只要文字不要语音"。
                match endpoint {
                    EndpointId::Speak => "对外说话恒有译音（`speak_translation` 恒真）：这条腿没有\
                         「只要文字不要语音」，`voice` 不能是 null；换音色请填一个音色名"
                        .to_string(),
                    EndpointId::Listen => {
                        "`voice = null` 就没有播放汇：这一条要跟 `voice` 一起改（§2.1.4）"
                            .to_string()
                    }
                }
            } else {
                "播放汇有没有由 `voice`（以及直通）决定，不能单独增删；`role` 由 \
                 (leg, translate, monitor, 本机能力位) 一起决定，不由设备名决定"
                    .to_string()
            }
        }
        _ if path.starts_with("out[playback(monitor)]") => {
            "回听只有带翻译、要语音、用户开了回听才有；它的 `device` 恒为 `null`（§2.1.4）"
                .to_string()
        }
        _ if path.starts_with("out[") => {
            "本机没有这条出口（条目要的位为假），或它不在 `editable` 里".to_string()
        }
        _ if path.starts_with("in[") => {
            "输入口由端点固定，不能增删（`in: []` 会在第一道闸被拒）".to_string()
        }
        _ => "这一格不在 `editable` 表里（§2.1.4 只列了那几格）".to_string(),
    }
}

// --- 差异 ------------------------------------------------------------------

/// 一条变更（`changed[]` 的元素）。
struct Change {
    path: String,
    from: Value,
    to: Value,
}

fn changes_json(changes: &[Change]) -> Value {
    Value::Array(
        changes
            .iter()
            .map(|change| json!({ "path": change.path, "from": change.from, "to": change.to }))
            .collect(),
    )
}

/// 清单的两层规范化视图：**条目**（存在性）与**叶子格**（取值）。
///
/// 路径用 `kind` 而不是下标取键（`in[mic].device` / `ops[gate].config.threshold` /
/// `out[playback(primary)].device`）：算子链里插一节、删一节都不该让别的格"看起来变了"，
/// 而下标会。播放汇再按角色细分（`primary` / `monitor`）——它俩是同一组件的两种角色，
/// 谁在谁不在正是要看的差异。
#[derive(Default)]
struct Flat {
    entries: BTreeMap<String, Value>,
    leaves: BTreeMap<String, Value>,
}

impl Flat {
    /// 取某个路径的值（先找条目，再找叶子）——`unsupported_field` 的 `expected` 用。
    fn get(&self, path: &str) -> Option<&Value> {
        self.entries.get(path).or_else(|| self.leaves.get(path))
    }
}

fn flatten(composition: &Composition) -> Flat {
    let value = wire(composition);
    let mut flat = Flat::default();
    for (key, child) in value.as_object().expect("清单的 serde 形态是对象") {
        match key.as_str() {
            "in" | "ops" | "out" => flatten_entries(key, child, &mut flat),
            "session" => {
                if child.is_null() {
                    continue;
                }
                flat.entries.insert("session".to_string(), child.clone());
                flatten_object(child, "session", &mut flat, false);
            }
            _ => flatten_value(child, key, &mut flat),
        }
    }
    flat
}

/// `in` / `ops` / `out`：整条进 `entries`，其余格进 `leaves`。
fn flatten_entries(list: &str, value: &Value, flat: &mut Flat) {
    for entry in value.as_array().expect("in/ops/out 都是数组") {
        let kind = entry["kind"].as_str().expect("条目都带 kind");
        let path = match (list, kind) {
            ("out", "playback") => match entry["role"].as_str() {
                Some("monitor") => "out[playback(monitor)]".to_string(),
                _ => "out[playback(primary)]".to_string(),
            },
            _ => format!("{list}[{kind}]"),
        };
        flat.entries.insert(path.clone(), entry.clone());
        flatten_object(entry, &path, flat, true);
    }
}

/// 对象逐格展开；`tagged = true` 时跳过判别键 `kind`（它已经在路径里了）。
fn flatten_object(object: &Value, path: &str, flat: &mut Flat, tagged: bool) {
    for (key, child) in object.as_object().expect("对象") {
        if tagged && key == "kind" {
            continue;
        }
        flatten_value(child, &format!("{path}.{key}"), flat);
    }
}

/// 标量 / 数组直接进 `leaves`；嵌套对象继续往下（`session.params`、`ops[gate].config`）。
fn flatten_value(value: &Value, path: &str, flat: &mut Flat) {
    match value {
        Value::Object(_) => flatten_object(value, path, flat, false),
        _ => {
            flat.leaves.insert(path.to_string(), value.clone());
        }
    }
}

/// 两清单的差异。**条目存在性差异先报**（一条路径一条，不报它的子格——整条不在时子格的
/// 差异没有意义），存在性相同的条目再逐格比叶子。
fn diff(current: &Flat, submitted: &Flat) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut shadowed: Vec<&str> = Vec::new();

    let entries: BTreeSet<&String> = current
        .entries
        .keys()
        .chain(submitted.entries.keys())
        .collect();
    for path in entries {
        let from = current.entries.get(path);
        let to = submitted.entries.get(path);
        if from.is_some() == to.is_some() {
            continue;
        }
        changes.push(Change {
            path: path.clone(),
            from: from.cloned().unwrap_or(Value::Null),
            to: to.cloned().unwrap_or(Value::Null),
        });
        shadowed.push(path);
    }

    let leaves: BTreeSet<&String> = current
        .leaves
        .keys()
        .chain(submitted.leaves.keys())
        .collect();
    for path in leaves {
        // 整条没了/刚出现：它的子格由那条差异代表（`out[captions]` 不会散成三条）。
        if shadowed.iter().any(|entry| path.starts_with(*entry)) {
            continue;
        }
        let from = current.leaves.get(path).cloned().unwrap_or(Value::Null);
        let to = submitted.leaves.get(path).cloned().unwrap_or(Value::Null);
        if from != to {
            changes.push(Change {
                path: path.clone(),
                from,
                to,
            });
        }
    }

    changes.sort_by(|left, right| left.path.cmp(&right.path));
    changes
}

// --- 清单 → 设置 的小工具（反方向表用） ------------------------------------

/// `in` 里那个麦克风条目。
fn mic_input(composition: &Composition) -> Option<&Input> {
    composition
        .r#in
        .iter()
        .find(|input| matches!(input, Input::Mic { .. }))
}

fn mic_device(composition: &Composition) -> Option<String> {
    match mic_input(composition) {
        Some(Input::Mic { device, .. }) => device.clone(),
        _ => None,
    }
}

/// `in` 里那个进程环回条目（可执行名 + 连带抓子进程）。
fn loopback(composition: &Composition) -> Option<(&str, bool)> {
    composition.r#in.iter().find_map(|input| match input {
        Input::ProcessLoopback {
            executable,
            include_tree,
            ..
        } => Some((executable.as_str(), *include_tree)),
        _ => None,
    })
}

/// 算子链里有没有这一节。
fn has_op(composition: &Composition, wanted: &str) -> bool {
    composition.ops.iter().any(|op| match op {
        Op::Mono => wanted == "mono",
        Op::Denoise => wanted == "denoise",
        Op::Gate { .. } => wanted == "gate",
        Op::Resample { .. } => wanted == "resample",
    })
}

/// 主播放汇（不是回听的那条）。
fn primary_playback(composition: &Composition) -> Option<&Output> {
    composition.out.iter().find(|output| match output {
        Output::Playback { role, .. } => *role != PlaybackRole::Monitor,
        _ => false,
    })
}

fn primary_device(composition: &Composition) -> Option<String> {
    match primary_playback(composition) {
        Some(Output::Playback { device, .. }) => device.clone(),
        _ => None,
    }
}

fn has_playback(composition: &Composition, role: PlaybackRole) -> bool {
    composition.out.iter().any(|output| match output {
        Output::Playback { role: actual, .. } => *actual == role,
        _ => false,
    })
}

/// 清单里的音色：`None` = 只要文字不要语音（⇒ `speak_translation = false`）。
fn session_voice(composition: &Composition) -> Option<String> {
    composition
        .session
        .as_ref()
        .and_then(|session| session.params.voice.clone())
}

/// 复刻频次回 `Settings` 的**次数**（`Settings` 存的是次数：1 次 = 只用第一段）。
fn count_of(frequency: CloneFrequency) -> u32 {
    match frequency {
        CloneFrequency::Once => 1,
        CloneFrequency::Always => 2,
    }
}
