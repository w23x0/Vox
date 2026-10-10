//! 账本端口：控制面**唯一**读账本、**唯一**写账本的地方。
//!
//! 这一层薄得几乎不像一层，它存在的理由只有两条：
//!
//! 1. **本 crate 要能在没有界面、没有设备、没有 Tauri 的进程里跑**（`voxctl` 的离线自检、
//!    单元测试、将来的无屏档）。所以控制面不直接吃 [`Runtime`]，而是吃这个端口——真实现只有
//!    一个，就是 [`Runtime`]（本文件底部那段 `impl`，逐格转发，**没有第二份状态**）。
//! 2. **清单只有一条派生路径**（§2.1.3-③ / S0 的 D1）：`session_config` 这一格给的就是芯
//!    `Runtime::session_config` 的那份 `SessionConfig`——控制面**不许**自己从 [`Settings`]
//!    再映射一份（那会变成第二份真源）。
//!
//! 平台 API、界面文案一个都不出现：位与档位是数据（`HostFacts`），授权位是布尔。

use vox_core::capability::{CapabilityReport, HostFacts};
use vox_core::event::PipelineState;
use vox_core::ports::AudioApp;
use vox_core::runtime::{Listener, Runtime, SessionConfig};
use vox_core::settings::Settings;

use crate::actions::{EndpointId, Permission};
use crate::endpoints;

/// 控制面要问账本的全部东西。宿主注入实现；[`Runtime`] 那份是本 crate 自带的默认实现。
///
/// 全是 `&self`：芯的账本是 `Arc` 共享的，改设置走 [`Ledger::update_settings`]（芯的唯一写入口，
/// 事件 / 落盘 / 界面刷新都跟着它走），所以这个端口**不需要** `&mut self`。
pub trait Ledger: Send {
    /// 当前设置（只读快照）。
    fn settings(&self) -> Settings;

    /// 这台机器报上来的**同一份**事实（芯的 `Runtime::host_facts`）——投影与校验都只用它。
    fn host_facts(&self) -> HostFacts;

    /// 有效能力位报告（芯的 `Runtime::capabilities`）：`missing_on` 与 `describe` 吃同一份。
    fn capabilities(&self) -> CapabilityReport;

    /// 这条腿现在会派出去的会话配置（芯的 `Runtime::session_config`，**唯一**那条派生路径）。
    fn session_config(&self, endpoint: EndpointId) -> SessionConfig;

    /// 正在放声音的程序（`in[0].executable` → `display_name` 的查表来源）。
    fn audio_apps(&self) -> Vec<AudioApp>;

    /// 写设置。**唯一写入口**：`edit` 在芯的写锁内跑，改完自动 `normalize`、发事件、该热更新的
    /// 热更新、该提示重启的提示重启。
    fn update_settings(&self, edit: &mut dyn FnMut(&mut Settings));

    /// 这条流水线现在跑到哪一步了。
    fn pipeline_state(&self, endpoint: EndpointId) -> PipelineState;

    /// 上一次失败的原因（`start_failed` 的 `detail.reason` 从这里来）。
    fn pipeline_error(&self, endpoint: EndpointId) -> Option<String>;

    /// 开这条流水线（已在跑就什么也不做）。
    fn start(&self, endpoint: EndpointId);

    /// 停这条流水线（走握手，实现方等它真收摊）。
    fn stop(&self, endpoint: EndpointId);

    /// 这条腿现在**可见**的字幕文本（芯 `SubtitleTrack::text` 的语义：已经滤掉完全透明的字）。
    ///
    /// 资源面（`vox://session/<handle>/transcript` 的快照）要的就是它——**不在这里重写
    /// "滤 alpha=0"那段**：投影走芯的 `Runtime::subtitle_frame()`，本 crate 只把 alpha>0 的字
    /// 拼起来（那正是 `text` 的定义）。
    fn subtitle_text(&self, endpoint: EndpointId) -> String;

    /// 订一条芯的事件监听器（`Event::SubtitleDelta` 一类）。
    ///
    /// 资源面的**事件口**只有这一条路：`confirmed` / `last_delta_done` 只在事件里，账本里读不到。
    /// 回调可能在芯持有状态锁时被调用，所以监听器里**只许碰自己的数据**（`crate::transcript`）。
    fn add_listener(&self, listener: Listener);

    /// 单调毫秒时钟（compose token 的 TTL 用它，和芯打事件用的是同一个钟）。
    fn now_ms(&self) -> u64;
}

/// "用户让不让"：授权位（`Settings.control.allow_*`）与 OS 侧的授权事实。
///
/// **和 [`Ledger`] 分开是有意的**：账本回答"本机能不能"（能力位、设备、会话），这里回答
/// "用户让不让"。两者的来源与失败语义都不一样——账本就一个真源（芯的 `Runtime`，1:1 转发），
/// 这一份也一样：真实现是下面那个 `impl Grants for Runtime`，逐项读 `Settings.control`；
/// OS 那一侧的事实（系统权限页）不在这里，它由能力位回答（`HostFacts` / `Capability::off`）。
///
/// 缺省实现是 [`Denied`]（一位都不开）。**不是占位**：`ControlSettings` 的默认值就是全关，
/// 老配置文件里没有 `control` 这一段时读出来也是全关（fail-closed 与"位必须是事实"同一条口径）。
pub trait Grants: Send {
    /// 用户授权位：这一位开了没（默认全关，必须用户主动打开）。
    fn user_granted(&self, permission: Permission) -> bool;

    /// `control.allow_config_write`：让不让控制面改用户配置。
    fn config_write_allowed(&self) -> bool;
}

/// 缺省授权：一位都不开、配置一个字节都不许改。
///
/// 与真实现（`impl Grants for Runtime`）并列存在，用途只有两个：用例的对照，以及"控制面整体
/// 关掉"的宿主——`control.enabled == false` 时起都不该起（装配层读那一格），起不来自然也没有
/// 授权可言。**别在外壳里另写一份**读 `Settings.control` 的实现（那就是第二份真源）。
pub struct Denied;

impl Grants for Denied {
    fn user_granted(&self, _permission: Permission) -> bool {
        false
    }

    fn config_write_allowed(&self) -> bool {
        false
    }
}

/// 芯的账本。**唯一的真实现**——逐格转发，不含任何判断。
impl Ledger for Runtime {
    fn settings(&self) -> Settings {
        Runtime::settings(self)
    }

    fn host_facts(&self) -> HostFacts {
        Runtime::host_facts(self)
    }

    fn capabilities(&self) -> CapabilityReport {
        Runtime::capabilities(self)
    }

    fn session_config(&self, endpoint: EndpointId) -> SessionConfig {
        Runtime::session_config(self, endpoints::pipeline(endpoint))
    }

    fn audio_apps(&self) -> Vec<AudioApp> {
        self.snapshot().devices.audio_apps
    }

    fn update_settings(&self, edit: &mut dyn FnMut(&mut Settings)) {
        Runtime::update_settings(self, |settings| edit(settings));
    }

    fn pipeline_state(&self, endpoint: EndpointId) -> PipelineState {
        Runtime::pipeline_state(self, endpoints::pipeline(endpoint))
    }

    fn pipeline_error(&self, endpoint: EndpointId) -> Option<String> {
        let snapshot = self.snapshot();
        // Agent 读的是英文（同 `catalog/notices.json` 的 en 文案，原文接在后面）。
        match endpoints::pipeline(endpoint) {
            vox_core::event::Pipeline::Speak => snapshot.speak.last_error,
            vox_core::event::Pipeline::Listen => snapshot.listen.last_error,
        }
        .map(|msg| msg.render_en())
    }

    fn start(&self, endpoint: EndpointId) {
        Runtime::start(self, endpoints::pipeline(endpoint));
    }

    fn stop(&self, endpoint: EndpointId) {
        Runtime::stop(self, endpoints::pipeline(endpoint));
    }

    fn subtitle_text(&self, endpoint: EndpointId) -> String {
        let track = endpoints::pipeline(endpoint).track();
        Runtime::subtitle_frame(self)
            .lines
            .into_iter()
            .find(|line| line.track == track)
            .map(|line| {
                line.chars
                    .into_iter()
                    .filter(|rendered| rendered.alpha > 0.0)
                    .map(|rendered| rendered.ch)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn add_listener(&self, listener: Listener) {
        Runtime::add_listener(self, listener);
    }

    fn now_ms(&self) -> u64 {
        Runtime::now_ms(self)
    }
}

/// `Grants` 的**真实现**：授权位就是芯设置里的 `Settings.control.allow_*`（S1 §2.5.2 的闸门①）。
///
/// 三个"让不让"只有一个真源，就是用户在设置里拨的那四格；本 crate 每次调用**现读一次快照**
/// （`Runtime::settings()`），所以界面上一开一关下一次 `tools/call` 立刻见效，不需要重启、也不需要
/// 谁把位搬过来。冷路径：一次 `tools/call` 最多读两三次（`session_open` 逐个权限位读）。
///
/// 两道 **fail-closed**：
///
/// - `control.enabled`（总开关）关着时一位都不开——服务万一还在跑（装配层只管"起不起"），
///   也已经没有授权可言；
/// - `ControlSettings` 默认全关，老配置文件里没有 `control` 这一段时读出来也是全关，
///   不存在的字段不等于允许。
impl Grants for Runtime {
    fn user_granted(&self, permission: Permission) -> bool {
        let control = self.settings().control;
        control.enabled
            && match permission {
                Permission::Microphone => control.allow_microphone,
                Permission::SystemAudio => control.allow_system_audio,
                Permission::AudibleOutput => control.allow_audible_output,
            }
    }

    fn config_write_allowed(&self) -> bool {
        let control = self.settings().control;
        control.enabled && control.allow_config_write
    }
}
