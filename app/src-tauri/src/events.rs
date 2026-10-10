//! 内核事件 → 前端单通道 + 落盘 + 开机自启同步。
//!
//! 一个监听器挂到 `runtime.add_listener`，对每条事件做分派：
//! - **所有事件**都转发到前端通道 `vox://event`；
//! - 重活（落盘、悬浮窗换样式、注册表）按事件类型选择性做；
//! - 高频事件（`GateStatus`、`SubtitleDelta`）保证快路径：只转发，不做 IO。

use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::Emitter;
use tauri_plugin_autostart::ManagerExt;
use vox_core::capability::{CapabilityStatus, UnavailableReason};
use vox_core::event::Event;
use vox_core::runtime::Listener;
use vox_core::settings::SubtitleSettings;
use vox_host::EventSink;

use crate::state::AppState;

/// 前端订阅的唯一事件通道，和 `app/ui/src/api.ts` 里的 `EVENT_CHANNEL` 一致。
pub(crate) const EVENT_CHANNEL: &str = "vox://event";

/// 事件往**前端通道**出的那一个出口：`vox_host::events::EventSink` 的桌面实现。
///
/// 共享层只出 trait，不碰 Tauri——所以这一条通道的落地写在这里，**编译期就保证
/// `vox-host` 里没有一个 Tauri 类型**。同一个监听器里的其余分派（落盘、托盘、OSC…）
/// 仍留在 [`wire`]，这里只负责"把这一条事件送出去"那一句。
struct FrontendSink(tauri::AppHandle);

impl vox_host::EventSink for FrontendSink {
    fn emit(&self, event: &Event) {
        // v2 起前后端设置字段名一致，所有事件都能直接走克隆快路径——**除了一条**：
        // `SettingsChanged` 带的是设置账本原值，代理地址里的账号密码只许住在账本与
        // 落盘那一侧，所以出前端之前换成 `without_secrets()` 的脱敏副本（快照那条路
        // 走 `dto::snapshot`，同样脱敏）。
        if let Err(e) = self.0.emit(EVENT_CHANNEL, for_frontend(event)) {
            tracing::warn!("转发事件到前端失败：{e}");
        }
    }
}

/// 转发给前端之前的事件整形（见 [`FrontendSink::emit`]）。
///
/// 抽成自由函数只为一条：不建 `AppHandle` 也能单测"密码不出芯"。
fn for_frontend(event: &Event) -> Event {
    match event {
        Event::SettingsChanged { settings } => Event::SettingsChanged {
            settings: Box::new(settings.without_secrets()),
        },
        other => other.clone(),
    }
}

/// 装配入口。由 `lib.rs` 的 `assemble` 调一次。
///
/// 做两件事：
/// 1. 初始同步——对齐一次开机自启注册表。
/// 2. 挂内核事件监听器，后续所有变动由回调驱动。
pub fn wire(state: &Arc<AppState>, app: tauri::AppHandle) {
    // --- 初始同步 ---
    // 结果记进观测槽：它是 `background_service` 这一位的定义者（§2.5.4），
    // `host_facts()` 读的就是这份观测。
    crate::platform::record_background_service(sync_autostart(
        &app,
        state.runtime.settings().autostart,
    ));

    // --- 挂监听器 ---
    // 闭包只能捕 `Weak<AppState>`，**不能**捕 `Arc<AppState>`。
    //
    // 捕 Arc 会造出一个确定的引用环：
    //   Inner.listeners → 这个闭包 → Arc<AppState> → AppState.runtime
    //     → Arc<Inner> → 回到 Inner.listeners
    // 环上的引用计数永远降不到 0，于是 `AppState`、`Overlay`、`HotkeyListener`、
    // `PipelineEngine` 的 `Drop` 全都不执行——热键线程会一直轮询到进程被内核
    // 回收，期间还能改账本（那时 `persist.flush()` 已经跑完，改动静默丢失）。
    //
    // 换成 Weak 之后环断了，Drop 链恢复正常。闭包里先 upgrade，拿不到就说明
    // `AppState` 已经没了（进程在退出），直接 return。
    let weak = Arc::downgrade(state);
    let handle = app.clone();
    // 转发给前端那一半抽成了 `EventSink` 的实现（`FrontendSink`）；它跟下面那个复合监听器
    // 仍是**同一个闭包里的第一句**——"先发前端、再落盘/托盘/OSC"这个顺序一个字都没动。
    let sink = FrontendSink(app.clone());

    // 记住上一次的字幕样式设置，只在真的变了时才调 restyle。
    // 用 Mutex 包一份 SubtitleSettings 的克隆。
    let prev_subtitle: Arc<Mutex<SubtitleSettings>> =
        Arc::new(Mutex::new(state.runtime.settings().subtitle.clone()));

    // 上一次推进给 VRChat ChatBox 的已确认前缀。非 done 的逐字推进靠它去重，
    // 别在同一段前缀下重复发；新一轮说话时清掉，否则上一轮的尾字会卡住这一轮。
    let osc_last_sent: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let listener: Listener = Arc::new(move |event: &Event| {
        // ── 转发给前端 ──────────────────────────────────────────────────
        sink.emit(event);

        // ── 拿住 AppState ───────────────────────────────────────────────
        // upgrade 失败 = AppState 已析构（进程退出中），后面的活都没意义了。
        // 注意：转发给前端放在 upgrade 之前，那一步不需要 AppState。
        let Some(st) = weak.upgrade() else { return };

        // ── 按事件类型分派 ──────────────────────────────────────────────
        match event {
            Event::SettingsChanged { settings } => {
                // 落盘（persist 内部有去抖，直接调即可）。
                st.persist.save_settings(settings);

                // 同步开机自启注册表（只在不一致时才动），结果记进 `background_service`
                // 这一位的观测槽——用户勾了却写不进去时，位最迟 4 秒内翻假（§2.5.4）。
                crate::platform::record_background_service(sync_autostart(
                    &handle,
                    settings.autostart,
                ));

                // 字幕样式或 geometry 变化时同步原生悬浮窗；visible 也会随设置
                // 一并投递，具体显示切换仍由帧线程负责。
                let need_restyle = {
                    let mut prev = prev_subtitle.lock();
                    let changed = subtitle_style_changed(&prev, &settings.subtitle);
                    if changed {
                        *prev = settings.subtitle.clone();
                    }
                    changed
                };
                if need_restyle {
                    if let Some(overlay) = st.overlay.get() {
                        overlay.restyle(&settings.subtitle);
                    }
                }

                // 托盘的"显示字幕"勾选跟着走。设置也可能是前端或热键改的，
                // 不是只有托盘自己那条路径。
                crate::tray::sync(&st);
            }

            Event::UsageChanged { usage } => {
                st.persist.save_usage(usage);
            }

            Event::GateStatus { pipeline, status } => {
                // 极高频（每 200ms × 3 条），只做最小工作：缓存门状态供快照用。
                // 不落盘、不做快照。
                st.remember_gate(*pipeline, *status);
            }

            Event::PipelineState {
                pipeline,
                state: pipe_state,
            } => {
                // 流水线停了→清掉门缓存，否则 UI 残留最后一格电平。
                if !pipe_state.is_running() {
                    st.forget_gate(*pipeline);
                }
                // 托盘勾选跟着流水线实际状态走——热键、前端、崩溃重连都会走这里。
                crate::tray::sync(&st);
                // 对外说话的运行状态 → VRChat 头像指示灯（「正在翻译」亮灯）。
                // 只在 OSC 头像开关打开且配置了参数名时发，发失败静默。
                if *pipeline == vox_core::event::Pipeline::Speak {
                    let slot = st.osc.lock();
                    if let Some(client) = slot.as_ref() {
                        if client.avatar_enabled() {
                            let param = client.avatar_param();
                            if !param.is_empty() {
                                let _ = client.set_avatar_bool(param, pipe_state.is_running());
                            }
                        }
                    }
                    // 一轮说完了（Speak 离开运行态）：清掉上一轮的推进前驱，免得
                    // 下一轮的同一个已确认前缀因为和上轮尾巴相同而发不出去。
                    if !pipe_state.is_running() {
                        *osc_last_sent.lock() = None;
                    }
                }
            }

            Event::Notice { .. } => {}

            // 以下事件只需转发（上面已经 emit 过了），不做额外工作。
            // SubtitleDelta 也很频繁，快路径到此结束。
            // 高频事件（`GateStatus`、`SubtitleDelta`）保证快路径：只转发，不做 IO。
            Event::SubtitleDelta {
                track,
                text,
                done,
                confirmed,
                ..
            } => {
                // 与字幕轨并列的另一条 fast path：把译文实时写进 VRChat 聊天框。
                // 高帧率下不落盘、不做快照，发送失败静默忽略，绝不因为 OSC
                // 发不出去打断字幕流。
                if *track != vox_core::subtitle::Track::Speak {
                    return;
                }
                let slot = st.osc.lock();
                let Some(client) = slot.as_ref() else {
                    return;
                };
                if !client.chat_enabled() {
                    return;
                }
                let mut last = osc_last_sent.lock();
                match chatbox_step(*done, text, confirmed.as_deref(), last.as_deref()) {
                    ChatboxStep::Idle => {}
                    ChatboxStep::Send(line) => {
                        // immediate=true：VRChat 不弹输入框、直接把这一行发出去。
                        let _ = client.chatbox(&line, true);
                        *last = Some(line);
                    }
                    ChatboxStep::Finish(line) => {
                        // 用权威终稿收口，覆盖逐字推进可能残留的中间态。
                        let _ = client.chatbox(&line, true);
                        *last = None;
                    }
                    ChatboxStep::FinishQuiet => *last = None,
                }
            }
            Event::MicActive { .. } => {}
            Event::SubtitleCleared { .. } => {
                // 轨被清了（关掉「显示译文」/ 换回话），ChatBox 的长度基线得跟着作废。
                // 不清的话，这之后第一句短话会被上一句留下的基线按 +STEP 压住——
                // 跟 `chatbox_step` 里「上一条不是前缀就按新一轮发」防的是同一件事，
                // 只是一个从事件入口防、一个从判定入口防。
                *osc_last_sent.lock() = None;
            }
            Event::SourceDetected { .. } => {}
            Event::LatencyChanged { .. } => {}
            Event::DevicesChanged => {}
        }
    });

    state.runtime.add_listener(listener);
}

// VRChat ChatBox 逐字推进的参数。
/// 已确认前缀涨满多少个字符才再发一次。逐 token/逐帧发会闪、会吵，VRChat 的
/// 气泡也压不住；攒够一小段再发既够"实时"又不糊。
const OSC_CHAT_STEP_CHARS: usize = 6;
/// VRChat 聊天框 /chatbox/input 一次能塞的字符上限附近（估 144），超出截断加省略号。
const OSC_CHAT_MAX_CHARS: usize = 144;

/// 一条 `SubtitleDelta` 对 VRChat ChatBox 意味着什么（**纯函数**，可单测）。
///
/// 抽成纯函数是为了一条：这里四路分支（本轮首条 / 攒够步长 / 换了句 / 终稿）以前散在
/// 事件闭包里，**没有任何一条单测看得到它们**——"短句一个字都发不出去"能活到今天，
/// 根因就是它散在闭包里。喂四个参数就能把四路全钉住，用得着 `AppHandle` 吗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatboxStep {
    /// 不发，基线不动。
    Idle,
    /// 发这一行，基线推进到它。
    Send(String),
    /// 发这一行收口，然后清基线。
    Finish(String),
    /// 不发，但要清基线（终稿与刚发过的那条一模一样，不刷重复气泡）。
    FinishQuiet,
}

/// 决定这一条字幕事件要不要写成 VRChat 消息、写哪一行。
///
/// `last` = 上一条**真发出去**的整行（`None` = 本轮还没发过）。
///
/// 两条纪律：
///
/// 1. **中间推进只吃已确认前缀**（`.text` 形状下是 `confirmed`）。`stash` 尾巴还会被
///    服务端改写，而 `immediate=true` 的消息发出去收不回来——宁可少推进，不能报错字。
/// 2. **终稿以 `confirmed` 为准，但空的 `Some("")` 不许盖掉 `text`**。
///    `Some("")` 是"一个字都还没确认"，不是"没有正文"。
pub(crate) fn chatbox_step(
    done: bool,
    text: &str,
    confirmed: Option<&str>,
    last: Option<&str>,
) -> ChatboxStep {
    if done {
        let line = truncate_for_chat(confirmed.filter(|s| !s.is_empty()).unwrap_or(text));
        if line.is_empty() {
            return ChatboxStep::FinishQuiet;
        }
        // 中间推进刚发过同一行（整句一次确认完的短句就是这样），别发第二遍：
        // VRChat 那边就是两条一模一样的气泡。
        if last == Some(line.as_str()) {
            return ChatboxStep::FinishQuiet;
        }
        return ChatboxStep::Finish(line);
    }
    let Some(prefix) = confirmed.filter(|p| !p.is_empty()) else {
        return ChatboxStep::Idle;
    };
    let proposed = truncate_for_chat(prefix);
    if proposed.is_empty() {
        return ChatboxStep::Idle;
    }
    let grew_enough = match last {
        // 本轮首条：一定发。不发的话，短句永远等不到——它的确认前缀可能整句都空。
        None => true,
        // 上一条不是这一条的前缀 = **换句了**。服务端偶尔只发 `response.done` 不发
        // `.text.done`，那时基线没被清掉；不按新一轮重来的话，这一整句短话会被
        // +STEP 的闸门永久压住，一条都发不出去。
        Some(sent) if !proposed.starts_with(sent) => true,
        Some(sent) => proposed.chars().count() >= sent.chars().count() + OSC_CHAT_STEP_CHARS,
    };
    if grew_enough {
        ChatboxStep::Send(proposed)
    } else {
        ChatboxStep::Idle
    }
}

/// 把一长句截到 ChatBox 一次能发的长度，末尾补 `…`。按字符边界切，不劈开 UTF-8。
fn truncate_for_chat(text: &str) -> String {
    let boundary = text.char_indices().nth(OSC_CHAT_MAX_CHARS).map(|(i, _)| i);
    match boundary {
        Some(idx) => {
            let mut s = text[..idx].to_string();
            s.push('…');
            s
        }
        None => text.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 开机自启同步
// ---------------------------------------------------------------------------

/// 我们上一次确认过的 autostart 状态。`-1` = 还不知道。
///
/// 存在的意义是省掉注册表读：`SettingsChanged` 可能连续到达，而这个 listener 跑在
/// 发事件的线程上（可能是音频工作线程，输入队列只有几格，攒着就要丢音频）。
/// 让每条 SettingsChanged 都去读一次注册表是不能接受的。
static AUTOSTART_KNOWN: AtomicI8 = AtomicI8::new(-1);

/// `background_service` 这一位的判据，**纯函数**（§2.5.4）：注册这条路通不通。
///
/// 抽成纯函数（两件事实 → 位）是为了**能被单测钉住**：这一段就是"判定"本身，
/// 单测直接喂四种组合即可；`sync_autostart` 只负责产出这两件事实（读注册状态、需要时写）。
/// 第七轮复核的变异（把定义者改成恒 `ON`）当时全绿，根因就是判定散在 `sync_autostart`
/// 的函数体里、没有任何一条单测看得到它。
///
/// - `is_enabled`: `app.autolaunch().is_enabled()` 的结果。`Err` = **查不到**注册状态
///   （插件不支持 / 读失败）→ `unsupported`；
/// - `write`: 需要写时那次写的结果；`None` = 读到的状态已与期望一致，不需要写。
///   写不进（企业组策略锁了启动项）→ `permission`。
///
/// **`desired`（要不要）不进判据**：位是"能不能"，用户开关是"要不要"（§2.6 R5）——
/// 用户把自启关掉（`desired == false`）不会让这一位翻假。
pub(crate) fn autostart_status(
    is_enabled: Result<bool, ()>,
    write: Option<Result<(), ()>>,
) -> CapabilityStatus {
    if is_enabled.is_err() {
        return CapabilityStatus::off(UnavailableReason::Unsupported);
    }
    match write {
        Some(Err(())) => CapabilityStatus::off(UnavailableReason::Permission),
        Some(Ok(())) | None => CapabilityStatus::ON,
    }
}

/// 把设置里的 `autostart` 和注册表实际状态对齐。只在不一致时才写注册表。
/// 失败只 warn：有些企业环境用组策略锁了注册表启动项。
///
/// 返回值就是 `background_service` 这一位的定义者（§2.5.4）：**注册这条路通不通**。
/// 判定本身在 [`autostart_status`] 里（纯函数，可单测）；两条调用点都把它交给
/// [`crate::platform::record_background_service`]，`host_facts()` 每 4 秒读一次那份观测
/// （§2.6 R7）。
///
/// 注意判的是"能不能"，不是"要不要"（§2.6 R5）：用户把自启开关关掉（`desired == false`）
/// 不会让这一位翻假，只有**查不到状态 / 写不进去**才会。
fn sync_autostart(app: &tauri::AppHandle, desired: bool) -> CapabilityStatus {
    // 快路径：跟我们已知的状态一致就什么都不做，一次注册表 IO 都不发生。
    // 用户从别处改了注册表我们会漏掉，但那是他自己动的，下次启动会重新对齐。
    if AUTOSTART_KNOWN.load(Ordering::Relaxed) == i8::from(desired) {
        return autostart_status(Ok(desired), None);
    }

    let manager = app.autolaunch();
    let current = match manager.is_enabled() {
        Ok(v) => v,
        Err(e) => {
            // 问不到注册状态 = 这台机器上自启这条路走不通（插件不支持 / 读失败）。
            tracing::warn!("查询开机自启状态失败：{e}");
            return autostart_status(Err(()), None);
        }
    };
    if current == desired {
        AUTOSTART_KNOWN.store(i8::from(desired), Ordering::Relaxed);
        return autostart_status(Ok(current), None);
    }
    let result = if desired {
        manager.enable()
    } else {
        manager.disable()
    };
    match &result {
        Ok(()) => AUTOSTART_KNOWN.store(i8::from(desired), Ordering::Relaxed),
        Err(e) => {
            // 写失败就不缓存——下次还得再试，否则用户勾了开机自启却永远不生效。
            // 位跟着翻假：企业环境用组策略锁了启动项时，用户看到的是"需要先授权"。
            tracing::warn!("同步开机自启失败（期望={desired}, 当前={current}）：{e}");
        }
    }
    autostart_status(Ok(current), Some(result.map_err(|_| ())))
}

// ---------------------------------------------------------------------------
// 字幕样式比较
// ---------------------------------------------------------------------------

/// 判断字幕的"样式"部分是否变了。
///
/// `visible` 由 overlay.rs 的帧线程负责；geometry 需要送进原生窗口，
/// 这样设置页的“恢复默认”和拖动回写都能立即生效。
/// 这里关心窗口渲染器直接使用的字段：字体、字号、配色、底衬透明度和几何。
/// 字符生命周期由内核字幕模型立即更新，下一帧自然会带来新的 alpha。
fn subtitle_style_changed(prev: &SubtitleSettings, curr: &SubtitleSettings) -> bool {
    prev.font_family != curr.font_family
        || prev.font_size != curr.font_size
        || prev.speak_color != curr.speak_color
        || prev.listen_color != curr.listen_color
        || prev.vr_overlay_enabled != curr.vr_overlay_enabled
        || prev.background_alpha != curr.background_alpha
        || prev.geometry != curr.geometry
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::event::{Event, Notice, Pipeline, PipelineState};
    use vox_core::gate::GateStatus;
    use vox_core::subtitle::Track;
    use vox_core::usage::UsageLedger;

    // -- 字幕样式比较 --

    #[test]
    fn chat_truncate_keeps_the_head_and_appends_ellipsis() {
        let short = "短句".to_string();
        assert_eq!(truncate_for_chat(&short), "短句", "没超长原样返回");

        let long = "あ".repeat(OSC_CHAT_MAX_CHARS + 2);
        let cut = truncate_for_chat(&long);
        assert!(cut.ends_with('…'), "超长要补省略号：{cut}");
        assert_eq!(
            cut.chars().count(),
            OSC_CHAT_MAX_CHARS + 1,
            "截断后 = 上限字符 + 省略号"
        );
    }

    #[test]
    fn same_style_returns_false() {
        let a = SubtitleSettings::default();
        let b = a.clone();
        assert!(!subtitle_style_changed(&a, &b));
    }

    #[test]
    fn font_size_change_detected() {
        let a = SubtitleSettings::default();
        let mut b = a.clone();
        b.font_size = 42;
        assert!(subtitle_style_changed(&a, &b));
    }

    #[test]
    fn visible_change_ignored() {
        let a = SubtitleSettings::default();
        let mut b = a.clone();
        b.visible = !a.visible;
        assert!(!subtitle_style_changed(&a, &b), "visible 不算样式");
    }

    #[test]
    fn geometry_change_detected() {
        let a = SubtitleSettings::default();
        let mut b = a.clone();
        b.geometry = Some(vox_core::settings::OverlayGeometry {
            x: 99,
            y: 99,
            width: 800,
            height: 120,
        });
        assert!(subtitle_style_changed(&a, &b), "geometry 变化需要同步窗口");
    }

    #[test]
    fn timing_change_is_handled_by_subtitle_frames() {
        let a = SubtitleSettings::default();
        let mut b = a.clone();
        b.char_ttl_ms += 100;
        b.char_fade_ms += 50;
        assert!(
            !subtitle_style_changed(&a, &b),
            "时序变化不需要重建窗口渲染器"
        );
    }

    // -- 开机自启这一位的判据（§2.5.4 的 `background_service`） --

    /// 判据（纯函数）：**查得到注册状态 × 需要写的时候写得进去** → 位。
    ///
    /// 第七轮 D3：这一跳原来藏在 `sync_autostart` 的函数体里，把它改成恒 `ON` 单测全绿。
    /// **自证**：把 [`autostart_status`] 改成恒 `ON`，这条变红，
    /// `platform::tests::background_service_bit_follows_the_autostart_result` 也跟着红。
    #[test]
    fn autostart_status_maps_the_read_and_the_write_to_the_bit() {
        use vox_core::capability::UnavailableReason;

        // 读得到、不需要写（当前状态已与期望一致，或快路径命中）→ 这条路通。
        assert_eq!(autostart_status(Ok(true), None), CapabilityStatus::ON);
        assert_eq!(autostart_status(Ok(false), None), CapabilityStatus::ON);
        // 需要写且写成功 → 通。
        assert_eq!(
            autostart_status(Ok(false), Some(Ok(()))),
            CapabilityStatus::ON
        );
        assert_eq!(
            autostart_status(Ok(true), Some(Ok(()))),
            CapabilityStatus::ON
        );
        // 需要写但写不进（企业组策略锁了启动项）→ 要授权。
        assert_eq!(
            autostart_status(Ok(true), Some(Err(()))),
            CapabilityStatus::off(UnavailableReason::Permission)
        );
        // 查不到注册状态（插件不支持 / 读失败）→ 这台机器上这条路走不通；
        // 读都读不到就谈不上去写，所以写的结果不改变结论。
        assert_eq!(
            autostart_status(Err(()), None),
            CapabilityStatus::off(UnavailableReason::Unsupported)
        );
        assert_eq!(
            autostart_status(Err(()), Some(Err(()))),
            CapabilityStatus::off(UnavailableReason::Unsupported)
        );
    }

    // -- 事件序列化契约：确认 JSON 的 kind 标签和字段名与前端 VoxEvent 一致 --

    /// 辅助：序列化一个事件并返回 JSON Value。
    fn to_json(event: &Event) -> serde_json::Value {
        serde_json::to_value(event).expect("Event 序列化不应失败")
    }

    #[test]
    fn pipeline_state_event_shape() {
        let json = to_json(&Event::PipelineState {
            pipeline: Pipeline::Listen,
            state: PipelineState::Reconnecting,
        });
        assert_eq!(json["kind"], "pipeline_state");
        assert_eq!(json["pipeline"], "listen");
        assert_eq!(json["state"], "reconnecting");
    }

    #[test]
    fn gate_status_event_shape() {
        let json = to_json(&Event::GateStatus {
            pipeline: Pipeline::Speak,
            status: GateStatus {
                kind: vox_core::gate::GateKind::Level,
                state: vox_core::gate::GateState::Speech,
                rms: 0.42,
                active: true,
                ended: false,
            },
        });
        assert_eq!(json["kind"], "gate_status");
        assert_eq!(json["pipeline"], "speak");
        // status 子对象
        let status = &json["status"];
        assert!(status["rms"].is_number());
        assert_eq!(status["active"], true);
        assert_eq!(status["ended"], false);
    }

    #[test]
    fn subtitle_delta_event_shape() {
        let json = to_json(&Event::SubtitleDelta {
            track: Track::Listen,
            text: "你好".into(),
            done: true,
            replace: true,
            confirmed: None,
        });
        assert_eq!(json["kind"], "subtitle_delta");
        assert_eq!(json["track"], "listen");
        assert_eq!(json["text"], "你好");
        assert_eq!(json["done"], true);
        assert_eq!(json["replace"], true);
    }

    #[test]
    fn source_detected_event_shape() {
        let json = to_json(&Event::SourceDetected {
            track: Track::Listen,
            language: "ja".into(),
        });
        assert_eq!(json["kind"], "source_detected");
        assert_eq!(json["track"], "listen");
        assert_eq!(json["language"], "ja");
    }

    #[test]
    fn usage_changed_event_shape() {
        let json = to_json(&Event::UsageChanged {
            usage: Box::new(UsageLedger::default()),
        });
        assert_eq!(json["kind"], "usage_changed");
        // usage 是 #[serde(transparent)]，所以 json["usage"] 是个对象（空 map）。
        assert!(json["usage"].is_object());
    }

    #[test]
    fn notice_event_shape() {
        let json = to_json(&Event::Notice {
            notice: Notice::error(
                vox_core::Msg::new(vox_core::notice::key::CONNECT_FAILED).detail("boom"),
            )
            .on(Pipeline::Speak),
        });
        assert_eq!(json["kind"], "notice");
        let notice = &json["notice"];
        assert_eq!(notice["severity"], "error");
        assert_eq!(notice["key"], "notice.connectFailed");
        assert_eq!(notice["detail"], "boom");
        assert!(notice["params"].is_object());
        assert_eq!(notice["pipeline"], "speak");
    }

    #[test]
    fn mic_active_event_shape() {
        let json = to_json(&Event::MicActive { active: true });
        assert_eq!(json["kind"], "mic_active");
        assert_eq!(json["active"], true);
    }

    #[test]
    fn devices_changed_event_shape() {
        let json = to_json(&Event::DevicesChanged);
        assert_eq!(json["kind"], "devices_changed");
    }

    #[test]
    fn subtitle_cleared_event_shape() {
        let json = to_json(&Event::SubtitleCleared {
            track: Track::Speak,
        });
        assert_eq!(json["kind"], "subtitle_cleared");
        assert_eq!(json["track"], "speak");
    }

    // --- VRChat ChatBox 推进判定（`chatbox_step`）-----------------------------
    //
    // 这一组钉的是 2026-10-10 报上来的那条："只说**一句短话**时 VRChat 里没有字幕；
    // 快速说很多话反而正常"。自证：把 `chatbox_step` 里 `None => true` 改回"攒够才发"，
    // `a_non_prefix_means_a_new_sentence_and_sends_immediately` 就得变红。

    /// 本轮首条一定发。短句的确认前缀可能整句都空，首发不无条件放行就永远等不到。
    #[test]
    fn the_first_chunk_of_a_turn_always_goes_out() {
        assert_eq!(
            chatbox_step(false, "", Some("你好吗"), None),
            ChatboxStep::Send("你好吗".into())
        );
    }

    /// 同一句里没攒够步长就先憋着，别逐 token 刷一长串碎气泡。
    ///
    /// 用重复字把字数写死（上一条 3 字，步长 6 ⇒ 阈值 9 字），
    /// 免得汉字数得不清不楚、测试自己先错。
    #[test]
    fn within_a_sentence_the_step_gate_holds_back_short_growth() {
        let sent = "好".repeat(3);
        assert_eq!(
            chatbox_step(false, "", Some(&"好".repeat(4)), Some(&sent)),
            ChatboxStep::Idle,
            "只涨 1 个字（4 < 3 + 6）不该发"
        );
        assert_eq!(
            chatbox_step(false, "", Some(&"好".repeat(8)), Some(&sent)),
            ChatboxStep::Idle,
            "还差一个字（8 < 9）"
        );
        assert_eq!(
            chatbox_step(false, "", Some(&"好".repeat(9)), Some(&sent)),
            ChatboxStep::Send("好".repeat(9)),
            "刚好涨到步长（9 ≥ 3 + 6）就该推进一步"
        );
    }

    /// 上一条不是这一条的前缀 = **换句了**，按新一轮重来。
    ///
    /// 服务端偶尔只发 `response.done` 不发 `.text.done`，基线就没被清掉；
    /// 不走这一路的话，下一句短话（字数远少于上一句）会被 +6 的闸门**永久压住**，
    /// 一个字都发不出去——这就是用户说的"只有一句话时没有字幕"。
    #[test]
    fn a_non_prefix_means_a_new_sentence_and_sends_immediately() {
        assert_eq!(
            chatbox_step(false, "", Some("好"), Some("你好吗我是小明")),
            ChatboxStep::Send("好".into()),
        );
    }

    /// 终稿发一次，然后把基线清掉，下一轮重新首发。
    #[test]
    fn the_final_line_is_sent_once_and_resets_the_baseline() {
        assert_eq!(
            chatbox_step(true, "", Some("你好吗我是小明"), Some("你好吗")),
            ChatboxStep::Finish("你好吗我是小明".into())
        );
    }

    /// 终稿跟刚发过的那条一模一样：不许再发一遍。
    /// 整句一次就确认完的短句就是这条路径，以前会在 VRChat 里出两条一样的气泡。
    #[test]
    fn the_final_line_is_not_duplicated_when_it_was_just_sent() {
        assert_eq!(
            chatbox_step(true, "", Some("你好吗"), Some("你好吗")),
            ChatboxStep::FinishQuiet
        );
    }

    /// 空的 `Some("")` = "一个字都还没确认"，不是"没有正文"：不许盖掉事件带的 `text`。
    /// 盖掉的话终稿就变成空串，一个字都发不出去。
    #[test]
    fn an_empty_confirmed_does_not_shadow_the_text() {
        assert_eq!(
            chatbox_step(true, "你好吗", Some(""), None),
            ChatboxStep::Finish("你好吗".into())
        );
    }

    /// 整句还在服务端 stash 里（`confirmed` 空）的短句：
    /// 说话过程中**不发**是设计如此（stash 会被改写，而 VRChat 消息发出去收不回来）；
    /// 但终稿必须落地，不能一个字都不出。
    #[test]
    fn a_stash_only_sentence_lands_on_the_final_line() {
        assert_eq!(
            chatbox_step(false, "你好吗", Some(""), None),
            ChatboxStep::Idle,
            "confirm 为空时中间推进不发（stash 尾巴会被改写）"
        );
        assert_eq!(
            chatbox_step(true, "", Some("你好吗"), None),
            ChatboxStep::Finish("你好吗".into()),
            "终稿必须把整句发出去"
        );
    }

    /// 撞了 144 字上限之后：下一条还是那 144 字，不能因为"长度又涨了"而重发同一条。
    #[test]
    fn the_character_cap_does_not_refire_on_the_same_line() {
        let long = "好".repeat(200);
        let capped = truncate_for_chat(&long);
        assert!(capped.ends_with('…'));
        assert_eq!(
            chatbox_step(false, "", Some(&long), Some(&capped)),
            ChatboxStep::Idle
        );
    }

    /// 代理密码只许住在设置账本与落盘那一侧：转发给前端的 `SettingsChanged`
    /// 必须换成脱敏副本（快照那条路在 `dto.rs` 的用例里盯）。
    #[test]
    fn settings_changed_forwarded_to_the_frontend_is_redacted() {
        let mut settings = vox_core::settings::Settings::default();
        settings.network.gpt = vox_core::settings::NetworkSetting {
            mode: vox_core::settings::NetworkMode::Custom,
            proxy: "socks5h://alice:hunter2@127.0.0.1:1080".into(),
        };
        let forwarded = for_frontend(&Event::SettingsChanged {
            settings: Box::new(settings),
        });
        let json = serde_json::to_string(&forwarded).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!json.contains("alice"), "{json}");
        assert!(json.contains("socks5h://127.0.0.1:1080"), "{json}");
    }
}
