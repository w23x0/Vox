//! Vox 装配层。
//!
//! 这个 crate 自己不做任何业务判断：它把 Windows 的实现塞进 `vox-core` 的每个
//! 端口，把线程按 docs/architecture/ARCHITECTURE.md §6 的拓扑摆好，再把内核事件转成前端认的那一个
//! 事件通道。所有"要不要做、什么时候做"的决定都在内核里。
//!
//! 线程拓扑（§6）：
//! - 主线程：只跑 Tauri 事件循环，不干别的活；
//! - 悬浮窗线程：`vox-overlay-win` 自己起，带自己的 Win32 消息泵；
//! - 热键线程：25 ms 轮询 `GetAsyncKeyState`；
//! - 字幕帧线程：定时 prune + render，把内核的字幕状态推给悬浮窗；
//! - 设备轮询线程：低频枚举设备；
//! - tokio：复用 Tauri 自己那个 runtime，**不另起第二个**；
//! - 每条流水线一个工作线程，由 `PipelineEngine` 自己管。

// `windows_subsystem` 只在 Windows 上有效；别的平台上写了会被忽略，
// 但显式门控一下更清楚（发布构建没有控制台这件事只有 Windows 有）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use tauri::Manager;
use vox_host::core::{Finish, Notes, PersistMode};
use vox_host::secrets::SecretBackend;
use vox_host::{Core, HostPorts};

// ── 平台无关 ────────────────────────────────────────────────────────────────
mod catalog_updater;
mod commands;
mod composition;
mod devices;
mod dto;
mod events;
mod net;
mod overlay;
mod platform;
mod state;
mod sys;
mod tray;

// ── 只有 Windows 有实现（Linux 的对应物在 `platform/linux/` 与 `-linux` crate） ──
#[cfg(windows)]
mod input;
#[cfg(all(windows, feature = "steamvr-overlay"))]
mod vr_overlay;
#[cfg(windows)]
mod winminmax;

use state::AppState;

/// 桌面档的密钥文件名，落在配置目录下。**不进 `vox_host::Paths`**：Windows 侧是 DPAPI
/// 密文、无屏档是 0600 明文 `secret.json`，两者内容格式不一样，同名会互相读不懂。
/// 所以共享层只给 `SecretBackend` 一个"完整路径"，文件名由各档自己写死
/// （S4-A §9 M5.1 / M5.4）。
const DESKTOP_SECRET_FILE: &str = "secret.bin";

/// 这一档挑哪个密钥后端。**"选哪一个"的决策在这一档**（Windows DPAPI 落盘 / 桌面 Linux 走
/// Secret Service），机制在 `vox_host::secrets`。`HostPorts` 要的是后端**选择**而不是建好的
/// 存储（芯在第 3 步自己挂，顺手好报"盘上真有明文密钥"那条提示），所以这里给枚举而不是
/// `Arc<dyn SecretStore>`。
#[cfg(windows)]
fn secret_backend(path: PathBuf) -> SecretBackend {
    SecretBackend::Dpapi { path }
}

/// 桌面 Linux：Linux 不用文件存密钥，走 Secret Service（gnome-keyring / KWallet），
/// `path` 用不上——无屏档那个 0600 明文 `secret.json` 属于那一档。
#[cfg(not(windows))]
fn secret_backend(_path: PathBuf) -> SecretBackend {
    SecretBackend::SecretService
}

/// 应用入口。`main.rs` 只调这一个函数。
pub fn run() {
    sys::log::init();

    // 隐藏 CLI 模式（只有 Windows 有）：带 `--vox-restore-defaults` 时只做默认设备
    // 写回就退出，不构建 Tauri（避免被单实例插件当成「重复启动」吞掉）。
    if platform::pre_main() {
        return;
    }

    // 隐藏 CLI 模式（两个平台都有）：`--print-composition` 打两份清单 + 有效能力位就退
    // （S0 §4.3-A）。同样排在 Tauri 之前，理由见 `composition.rs` 头注释。
    if composition::requested() {
        composition::print_and_exit();
    }

    let app = tauri::Builder::default()
        // 单实例插件的文档要求：必须是第一个注册的插件。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::focus_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::update_settings,
            commands::set_api_key,
            commands::start_pipeline,
            commands::stop_pipeline,
            commands::toggle_pipeline,
            commands::osc_start,
            commands::osc_update,
            commands::osc_stop,
            commands::osc_send_chatbox,
            commands::osc_set_avatar,
            commands::reset_usage,
            commands::reset_usage_model,
            commands::refresh_devices,
            commands::install_virtual_cable,
            commands::uninstall_virtual_cable,
            commands::virtual_cable_blockers,
            commands::set_virtual_cable_multichannel_visible,
            commands::open_dashscope_console,
            commands::open_provider_console,
            commands::read_catalog_override,
            commands::check_catalog_update,
            commands::apply_catalog_update,
            commands::open_virtual_cable_website,
            commands::open_virtual_cable_donation,
            commands::quit_app,
        ])
        .setup(|app| {
            let state = assemble(app.handle())
                // Tauri 的 `setup` 要 `Box<dyn Error>`，而共享层那几步的错误类型是
                // `Box<dyn Error + Send + Sync>`。两者之间**没有** `From`（裸的
                // `dyn Error` 本身不是 `Send`），所以显式收成一个 `io::Error` 递回去——
                // 它只负责把那句话原样带出去：`build()` 的 `Err(e)` 分支拿 `e` 弹框给用户看，
                // 显示的仍是原来那句。
                .map_err(std::io::Error::other)?;
            app.manage(state);
            Ok(())
        })
        .build(tauri::generate_context!());

    // 构建失败（含 setup 里 assemble 报错）不能默默 panic：发布构建没有控制台，
    // 用户看到的会是"双击图标什么都没发生"。弹个框把原因摆出来再退。
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!("Tauri 应用构建失败：{e}");
            platform::alert(
                "Vox 启动失败",
                &format!("初始化时出错，应用无法启动。\n\n{e}"),
            );
            return;
        }
    };

    app.run(|app, event| match event {
        // 关设置窗默认只是收进托盘，进程继续跑——热键和悬浮窗还得用。
        // 但**托盘没显示出来的时候不能收**：窗口没了、托盘也没有，用户除了任务管理器
        // 就没有别的入口能叫回界面（`tray.rs` 头注释）。那种情况下改成最小化，
        // 窗口留在任务栏/dock 里还能点回来。
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" => {
            api.prevent_close();
            if let Some(w) = app.get_webview_window("main") {
                if tray::can_hide_to_tray() {
                    let _ = w.hide();
                } else {
                    let _ = w.minimize();
                }
            }
        }
        tauri::RunEvent::Exit => shutdown(app),
        _ => {}
    });
}

/// 把所有实现拼起来。返回的 `AppState` 交给 Tauri 托管。
///
/// 这里的每一步都尽量"失败不致命"：设备枚举、悬浮窗、热键任何一样起不来，都只是
/// 记一条 `Notice` 让界面告诉用户，不把整个应用拖死——用户至少得能进设置窗改配置。
///
/// 形状：**入口参数（本机有哪些端口 + 本机能力位）→ 共享的前 4 步（`vox_host::Core::assemble`）
/// → Tauri 特有的那几步**。第 5–7 步（事实 / 启动提示 / 落盘监听）也在下面自己做，理由见
/// 第 12、13 步的注释与 `vox_host::core::Finish`。
fn assemble(
    app: &tauri::AppHandle,
) -> Result<Arc<AppState>, Box<dyn std::error::Error + Send + Sync>> {
    // 配置目录只有这一处真源（Tauri 给的 `app_config_dir`）；目录里的文件名由
    // `vox_host::paths` 定。密钥文件名是这一档自己的口味（下面 `secret_backend`），不进 `Paths`。
    let paths = vox_host::Paths::from_dir(app.path().app_config_dir()?);
    // 目录自己留一份：控制面的握手文件（`control.json`）也落在同一个目录里（第 14 步）。
    let config_dir = paths.dir.clone();

    // 本机有什么。**一次列清**：`Core::assemble` 读它建账本，`ports.engine()` 吃掉它装流水线。
    let ports = HostPorts {
        kind: platform::host_kind,
        facts: platform::host_facts,
        clock: platform::clock(),
        secret: secret_backend(paths.dir.join(DESKTOP_SECRET_FILE)),
        capture: platform::capture_factory(),
        playback: platform::playback_factory(),
        registry: platform::registry(),
        startup_notes: platform::startup_notes,
    };
    // 后面两步（启动提示、事实）要等宿主特有步骤跑完才问，但 `ports.engine()` 会把这一份
    // 吃掉，所以先把那两个"本机有什么"的问法存成函数指针（`fn` 是 `Copy`）。
    let host_facts = ports.facts;
    let startup_notes = ports.startup_notes;
    // 设备目录全进程只构造一个：账本（`state.registry`）与 4 秒轮询线程（`devices::start`）共用它。
    let registry = Arc::clone(&ports.registry);

    // ── 共用的第 1–4 步 ────────────────────────────────────────────────────
    // 落盘（建目录 + 起去抖线程）→ 设置 + 时钟 + `Runtime` → 密钥库
    // （`set_secret_store` 会顺手把存着的密钥读进来）→ 用量账本。
    // 逐条与无屏档同源，顺序理由随注释搬进了 `vox_host::core::Core::assemble`。
    // `Finish::AfterHostSteps`：共享层那 7 步里的第 5–7 步（事实 / 启动提示 / 落盘监听）
    // **推迟**到下面那几步之后，因为这五位宿主事实的定义者（悬浮窗 / 头显 / 事件桥 / 热键 /
    // 托盘）还没起来——早注入就是漏报（§2.5.4）。
    //
    // 下面第 4–14 步的编号是**这一档自己的**，与改动前逐条相同（后面几条注释里引用的
    // "第 13 步""第 14 步"指的就是它们）。
    let core = Core::assemble(
        paths,
        &ports,
        PersistMode::Writing,
        Notes::Never,
        Finish::AfterHostSteps,
    )?;
    let runtime = core.runtime;
    let persist = core.persist;

    // 4. 窗口先亮出来。后面任何一步失败，用户至少看得见界面。
    if let Some(w) = app.get_webview_window("main") {
        // Windows：透明无边框窗口下 `tauri.conf.json` 的 minHeight 压不住
        // （实测会被压到 ~30px），set_min_size 也压不到，只能原生子类硬来。
        // Linux：GTK 自己认配置里的最小尺寸，这里是空实现。
        platform::enforce_min_size(&w);
        if runtime.settings().start_minimized {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    // 5. 流水线引擎。tokio 复用 Tauri 那一个 runtime（句柄由入口备好，闭包捕获进去；
    //    runtime 的保活是 Tauri 自己的事）。降噪 / 重采样两节由 `vox-host` 从
    //    `vox_dsp::ports` 取，入口不发言。`engine()` 同时把引擎注入账本。
    let engine = ports.engine(
        &runtime,
        net::transport_factory(tauri::async_runtime::handle().inner().clone()),
    );

    // 控制面（Agent 面）的 reconciler。**这里只是构造**：不监听、不起服务、也不挂监听器——
    // 开门在第 14 步（事实齐了才开，见 `vox_host::control` 的头注释）。
    let control = Arc::new(vox_host::ControlPlane::new(
        runtime.clone(),
        config_dir.clone(),
    ));
    let state = Arc::new(AppState::new(
        runtime.clone(),
        Arc::clone(&engine),
        Arc::clone(&registry),
        Arc::clone(&persist),
        Arc::clone(&control),
    ));

    // 6. 纯显示悬浮窗 + 字幕帧线程。悬浮窗永久穿透，不处理按钮或设置命令。
    //    返回值是 `captions` 这一位的定义者（§2.5.4）：窗口和帧线程都起来了才是 ON，
    //    记进观测槽给 `host_facts()` 读（第 13 步）。
    platform::record_captions(overlay::start(&state));
    // 7. SteamVR 头显字幕。不可用时只在后台等待，不影响桌面字幕和 VRChat OSC。
    //    `vr_captions` 这一位由 `vr_overlay::status()` 报（`RUNNING` × `CONNECTED`），
    //    `host_facts()` 直接读它，不用再记一份。
    #[cfg(all(windows, feature = "steamvr-overlay"))]
    vr_overlay::start(runtime.clone());

    // 8. 设备枚举（首次同步一把，之后低频轮询）。
    devices::start(&state);

    // 9. 事件桥：落盘、推给前端、同步开机自启。
    events::wire(&state, app.clone());

    // 10. 热键。注入 host 会触发一次 rebind，把当前绑定推下去。
    //
    // 放在 events::wire 之后：热键线程一起来就可能立刻回调 `on_hotkey` →
    // `update_settings`，要是那会儿 listener 还没挂上，这次改动就不会被标脏，
    // 也就永远不落盘。中间夹着建 Win32 窗口，窗口不止几微秒。
    match platform::start_hotkeys(runtime.clone()) {
        Ok(host) => {
            platform::record_hotkeys(true);
            runtime.set_hotkey_host(host);
        }
        Err(e) => {
            // 位 + Notice：位是机器可读的那一面（界面/出口按它降级），Notice 是给人看的那句。
            platform::record_hotkeys(false);
            runtime.notify(vox_core::event::Notice::error(format!(
                "全局热键起不来，只能用界面上的开关：{e}"
            )));
        }
    }

    // 11. 托盘。起不来不致命——设置窗和悬浮窗都还在；但托盘不可用时关窗逻辑会退化成
    //     "最小化"而不是"收进托盘"，理由见 `tray.rs` 头注释。
    if let Err(e) = tray::install(app, &state) {
        runtime.notify(vox_core::event::Notice::warning(format!(
            "托盘图标没建起来：{e}"
        )));
    }

    // 12. 平台前置条件的提醒（Linux：PipeWire 在不在、托盘有没有宿主）。放进 Notice
    //     而不是启动失败，因为设置窗、密钥、目录更新这些功能不依赖它们。
    //     **必须排在托盘之后**：托盘那条要读 `install()` 刚写下的"看得见吗"。
    //     （`Core::assemble` 传的是 `Notes::Never` + `Finish::AfterHostSteps`，
    //     就是为了把这一步原样留在这里。）
    for note in startup_notes() {
        runtime.notify(vox_core::event::Notice::warning(note));
    }

    // 13. 宿主事实：虚拟麦接线 + 报事实，各一次。
    //     **排在这里**：定义者都在前面的步骤里跑——悬浮窗（第 6 步）、头显（第 7 步）、
    //     自启（第 9 步）、热键（第 10 步）、托盘（第 11 步）；早注入会漏掉它们
    //     （§2.5.4）。位由芯算（档位上限 − 关掉的），外壳只报事实。
    //     Linux 上这一步会真的在 PipeWire 图里建出虚拟麦节点——**位为真的唯一凭据
    //     就是那个句柄**（§3.2 ①）；Windows 上只读探测 VB-CABLE，不建节点。
    //     返回的那一位不用接：它在下面跟其余事实一起报（`virtual_mic_ensure()` 只负责"把路打开"）。
    platform::virtual_mic_ensure();
    let facts = host_facts();
    tracing::debug!(
        tier = ?facts.host,
        off = ?facts.off.keys().map(|bit| bit.id()).collect::<Vec<_>>(),
        virtual_mic_device = ?facts.virtual_mic_device,
        "宿主事实已注入（位由芯算）"
    );
    runtime.set_host_facts(facts);

    // 14. 控制面（Agent 面）。**排在最后**：`list_endpoints` / `describe_endpoint` 报的是
    //     能力位与清单，而事实刚在第 13 步注入——早开门的话，先连上来的客户端会拿到一份
    //     建立在默认事实上的清单（"广告了做不到的事"）。开关关着就什么都不做：不监听、
    //     不写握手文件。
    //
    //     两步都要：`install()` 挂上监听器（此后设置页拨开关就是热切换，见 `vox_host::control`），
    //     `reconcile()` 按**现在**这一档把服务起起来。`install()` 排在 `set_host_facts()`
    //     之后不是巧合——监听器一挂上，任何一次设置变更都会走到起停。
    state.control.install();
    state
        .control
        .reconcile(vox_host::Switch::from_settings(&runtime.settings()));

    Ok(state)
}

/// 退出前收摊。
///
/// **刻意保持顺序代码**，不抽成 `Shutdown` 之类的步骤链（S4-A §9 M1）：桌面退出 11 步里夹着
/// 托盘/热键/悬浮/头显/虚拟麦这些宿主特有的步骤，闭包链不比顺序代码清楚，而这一层唯一要
/// 保护的东西就是那个顺序——见末尾那条不变量。
/// 托盘是这 11 步里的第 1 步，`state.control` 第 2 步，`state.engine` 第 7 步，
/// `state.persist.flush()` 第 11 步。
///
/// 顺序是有讲究的，**竖旗不等于停下**——所有"还能改账本"的线程必须先真的
/// join 掉，才能 flush，否则它们会在 flush 之后把 `dirty` 重新标脏，那份改动
/// 永远不落盘（静默丢数据）。
///
/// 1. `tray::begin_shutdown()` 头一个：主线程马上要卡在下面的 join 里，事件
///    循环停转，这时候再往主线程投递 `set_checked` 没有任何意义。
/// 2. 控制面紧跟着停：它是**外部进程能碰账本的那条路**，而 `ServerHandle::shutdown`
///    会等当前那次调用跑完（不打断半截的配置写入）——不等它，那次写就落在 flush 之后。
/// 3. 热键线程其次——它是唯一能在退出中途 `toggle()` 拉起新工作线程去开麦的。
/// 4. 设备线程、字幕线程，都是会 emit 事件的。
/// 5. 工作线程（`engine.shutdown()`），再关悬浮窗窗口。
/// 6. Linux 的虚拟麦节点排在**工作线程之后**：播放流还挂在节点上时先删节点，
///    会留下一条指向不存在节点的悬挂 stream。
/// 7. 最后 flush。此时没有别的线程能碰账本了。
///
/// **唯一要守住的不变量：`state.control.shutdown()` < `state.engine.shutdown()` <
/// `state.persist.flush()`。** 无屏档那一份在 `vox_headless::headless::Daemon::shutdown`，
/// 同一句话、同一条不变量（S4-A §9 M1）。
fn shutdown(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let state = state.inner();

    tray::begin_shutdown();
    // 先停控制面：`shutdown()` 可能等上一次 `tools/call` 跑完（不打断半截的配置写入）。
    state.control.shutdown();
    platform::stop_hotkeys();
    devices::stop();
    overlay::stop();
    #[cfg(all(windows, feature = "steamvr-overlay"))]
    vr_overlay::stop();
    state.engine.shutdown();
    platform::virtual_mic_shutdown();
    platform::shutdown_overlay();
    if let Some(client) = state.osc.lock().take() {
        drop(client);
    }
    state.persist.flush();
}
