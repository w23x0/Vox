//! 系统托盘图标及其右键菜单。
//!
//! 主窗口被关掉时收进托盘（`hide()`），进程继续跑——热键和悬浮窗还得用。
//! 托盘图标是用户重新打开窗口和退出进程的主入口，不是装饰。
//!
//! **但"托盘没显示出来"是 Linux 上的常态**（GNOME 默认不带 StatusNotifier 宿主，
//! 装 AppIndicator 扩展才有），而 `TrayIconBuilder::build()` 在那种环境下**照样返回
//! Ok**——图标对象建出来了，只是没有任何东西会画它。所以这里除了建图标，还要问一次
//! "真有人显示它吗"（`platform::tray_host_available`），并把答案喂给关窗逻辑：
//! 没宿主就**最小化**而不是 `hide()`，否则窗口没了、托盘也没有，用户除了任务管理器
//! 就没有别的入口叫回界面。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use tauri::menu::{CheckMenuItem, MenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use vox_core::event::Pipeline;

use crate::state::AppState;

// ── 菜单项 ID ──────────────────────────────────────────────────────────────

const ID_OPEN_SETTINGS: &str = "open_settings";
const ID_TOGGLE_SPEAK: &str = "toggle_speak";
const ID_TOGGLE_LISTEN: &str = "toggle_listen";
const ID_TOGGLE_SUBTITLE: &str = "toggle_subtitle";
const ID_QUIT: &str = "quit";

// ── 存活菜单项句柄，供外部 sync 刷新 ──────────────────────────────────────

/// 需要从外部刷新勾选状态的菜单项句柄。
struct MenuItems {
    speak: CheckMenuItem<tauri::Wry>,
    listen: CheckMenuItem<tauri::Wry>,
    subtitle: CheckMenuItem<tauri::Wry>,
    /// `sync()` 要靠它把 `set_checked` 投递到主线程，见 `sync()` 的注释。
    app: tauri::AppHandle,
}

/// 模块级单例：install() 写一次，sync() 读。
/// OnceLock 保证并发安全且无锁开销（只写一次后都是只读）。
static ITEMS: OnceLock<MenuItems> = OnceLock::new();

/// 托盘图标**用户真的看得见**吗。
///
/// Windows：通知区域永远在，`install()` 成功就等于看得见。
/// Linux：`install()` 成功**不等于**看得见，理由见模块头注释。
static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// 进程正在退出。竖起来之后 `sync()` 直接 return。
///
/// 退出期主线程会卡在 `engine.shutdown()` 的 join 里，此时事件循环已经不抽消息，
/// 但 tao 的 `thread_msg_target` 窗口还没销毁——投递会"成功"然后永远不被执行。
/// 我们的 `sync()` 不等返回值所以不会死锁，但投过去的闭包也没意义，索性不投。
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

// ── 公开 API ──────────────────────────────────────────────────────────────

/// 构建托盘图标和菜单。由 lib.rs 在 setup 末尾调用，失败不致命。
pub fn install(app: &tauri::AppHandle, state: &Arc<AppState>) -> tauri::Result<()> {
    // 图标：复用主窗口图标，最简单也最不会出错。
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    // ── 菜单项 ──────────────────────────────────────────────────────────

    let speak_checked = state.runtime.pipeline_state(Pipeline::Speak).is_running();
    let listen_checked = state.runtime.pipeline_state(Pipeline::Listen).is_running();
    let subtitle_checked = state.runtime.settings().subtitle.visible;
    let item_speak = CheckMenuItem::with_id(
        app,
        ID_TOGGLE_SPEAK,
        "对外说话",
        true,
        speak_checked,
        None::<&str>,
    )?;
    let item_listen = CheckMenuItem::with_id(
        app,
        ID_TOGGLE_LISTEN,
        "听人说话",
        true,
        listen_checked,
        None::<&str>,
    )?;
    let item_subtitle = CheckMenuItem::with_id(
        app,
        ID_TOGGLE_SUBTITLE,
        "显示字幕",
        true,
        subtitle_checked,
        None::<&str>,
    )?;

    let menu = MenuBuilder::new(app)
        .text(ID_OPEN_SETTINGS, "打开设置")
        .separator()
        .item(&item_speak)
        .item(&item_listen)
        .item(&item_subtitle)
        .separator()
        .text(ID_QUIT, "退出")
        .build()?;

    // 保存句柄，供 sync() 刷新勾选状态。
    let _ = ITEMS.set(MenuItems {
        speak: item_speak,
        listen: item_listen,
        subtitle: item_subtitle,
        app: app.clone(),
    });

    // ── 构建托盘图标 ────────────────────────────────────────────────────

    let state_for_menu = Arc::clone(state);

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("Vox")
        .menu(&menu)
        // 左键单击显示主窗口，不弹菜单。右键才弹。
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            handle_menu_event(app, &state_for_menu, event.id());
        })
        .on_tray_icon_event(move |tray, event| {
            // 左键松开 → 显示/聚焦主窗口（Windows 用户肌肉记忆）。
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                focus_main(tray.app_handle());
            }
        })
        .build(app)?;

    // 见 `AVAILABLE` 的注释：Linux 上"建成功"和"看得见"是两件事。
    let visible = crate::platform::tray_host_available();
    AVAILABLE.store(visible, Ordering::Relaxed);
    tracing::debug!("托盘图标已建；有没有宿主在显示它 = {visible}");

    Ok(())
}

/// 关窗能不能收进托盘。
///
/// 比 `available()` 多问一次宿主：用户可能在我们启动之后才装上（或卸掉）
/// AppIndicator 扩展。没宿主还 `hide()` 的话，窗口没了、托盘也没有——见模块头注释。
pub fn can_hide_to_tray() -> bool {
    AVAILABLE.load(Ordering::Relaxed) && crate::platform::tray_host_available()
}

/// 刷新托盘菜单的勾选状态，使其与实际流水线 / 设置保持一致。
///
/// 从任意线程调都安全，**永不阻塞**。
///
/// 为什么不能直接调 `set_checked`：它内部是
/// `run_on_main_thread(task).and_then(|_| rx.recv())`（tauri 的
/// `run_item_main_thread!` 宏），而 `send_user_message` 只在**主线程上**就地执行，
/// 别的线程只是 `PostMessageW` 入队。也就是说非主线程调 `set_checked` 会一直
/// 阻塞到主线程回到消息泵。而主线程经常正卡在 `PipelineEngine` 的
/// `join(工作线程)` 里（托盘点击、同步 command、退出都会走 join），此时工作线程
/// 若因为阀门跳变发出 `PipelineState` 事件 → listener → 这里 → 阻塞等主线程，
/// 就是"主线程等工作线程退出、工作线程等主线程抽消息"的硬死锁，整个应用冻死。
///
/// 所以这里只投递、不等返回：闭包体在主线程上执行，那时 `set_checked` 走
/// "已在主线程"分支就地完成，零阻塞。代价是勾选状态延迟一个消息循环（无感）。
///
/// 没被调到之前，菜单显示的是 install() 时的快照——不会出现"显示了错误状态"
/// 的情况，只是可能"没跟上最新的状态变化"。
pub fn sync(state: &AppState) {
    if SHUTTING_DOWN.load(Ordering::Relaxed) {
        return;
    }
    let Some(items) = ITEMS.get() else { return };

    // 状态在当前线程读完，闭包里只带三个 bool——不把 AppState 拖进主线程闭包，
    // 也就不会在主线程上再去抢内核的锁。
    let speak_running = state.runtime.pipeline_state(Pipeline::Speak).is_running();
    let listen_running = state.runtime.pipeline_state(Pipeline::Listen).is_running();
    let subtitle_visible = state.runtime.settings().subtitle.visible;

    // 投递失败只能是事件循环已经没了（进程正在退出），忽略即可。
    let _ = items.app.run_on_main_thread(move || {
        let Some(items) = ITEMS.get() else { return };
        // 到这儿已经在主线程，set_checked 就地执行不阻塞。
        let _ = items.speak.set_checked(speak_running);
        let _ = items.listen.set_checked(listen_running);
        let _ = items.subtitle.set_checked(subtitle_visible);
    });
}

/// 进入退出流程时调一次，之后 `sync()` 变成 no-op。
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

// ── 内部实现 ──────────────────────────────────────────────────────────────

/// 处理菜单项点击。
fn handle_menu_event(app: &tauri::AppHandle, state: &Arc<AppState>, id: &tauri::menu::MenuId) {
    if id == ID_OPEN_SETTINGS {
        focus_main(app);
    } else if id == ID_TOGGLE_SPEAK {
        state.runtime.toggle(Pipeline::Speak);
        // toggle 后立刻刷新勾选状态，不等事件回来——响应更快。
        sync(state);
    } else if id == ID_TOGGLE_LISTEN {
        state.runtime.toggle(Pipeline::Listen);
        sync(state);
    } else if id == ID_TOGGLE_SUBTITLE {
        state.runtime.update_settings(|s| {
            s.subtitle.visible = !s.subtitle.visible;
        });
        sync(state);
    } else if id == ID_QUIT {
        // 走 app.exit(0)，会触发 RunEvent::Exit → shutdown() 做收尾。
        app.exit(0);
    }
}

/// 显示 + 取消最小化 + 聚焦主窗口。托盘菜单「打开设置」、托盘左键和
/// 第二个实例被拦下时（`lib.rs` 的单实例回调）共用同一条路径。
pub fn focus_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}
