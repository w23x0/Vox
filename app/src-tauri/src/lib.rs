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

use std::sync::Arc;

use tauri::Manager;
use vox_core::{PipelineEngine, Runtime};

// ── 平台无关 ────────────────────────────────────────────────────────────────
mod catalog_updater;
mod commands;
mod composition;
mod devices;
mod dto;
mod events;
pub mod mcp;
mod net;
mod overlay;
mod persist;
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
            let state = assemble(app.handle())?;
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
fn assemble(app: &tauri::AppHandle) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    let config_dir = app.path().app_config_dir()?;
    // start() 而不是 new()：去抖线程要持一份 Arc 才能保证对象活着（详见 persist.rs）。
    // 目录自己留一份：控制面的握手文件（`control.json`）也落在同一个目录里（第 14 步）。
    let persist = persist::Persist::start(config_dir.clone());

    // 1. 设置 + 时钟 + Runtime。
    let settings = persist.load_settings();
    let clock = platform::clock();
    let runtime = Runtime::new(settings, Arc::clone(&clock));

    // 2. 密钥库（Windows：DPAPI 落盘；Linux：Secret Service）。
    //    set_secret_store 会顺手把存着的密钥读进来。
    runtime.set_secret_store(platform::secret_store(persist.secret_path()));

    // 3. 用量账本。要在挂落盘监听之前灌进去，免得刚读出来就又写一遍。
    runtime.load_usage(persist.load_usage());

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

    // 5. 流水线引擎。tokio 复用 Tauri 那一个 runtime。
    let tokio_handle = tauri::async_runtime::handle().inner().clone();
    let engine = PipelineEngine::new(
        runtime.clone(),
        vox_core::pipeline::Deps {
            transport: net::transport_factory(tokio_handle),
            capture: platform::capture_factory(),
            playback: platform::playback_factory(),
            denoise: vox_dsp::ports::denoise_factory(),
            resample: vox_dsp::ports::resample_factory(),
        },
    );
    runtime.set_control(Arc::clone(&engine) as Arc<_>);

    let registry = platform::registry();
    // 控制面（Agent 面）的 reconciler。**这里只是构造**：不监听、不起服务、也不挂监听器——
    // 开门在第 14 步（事实齐了才开，见 `mcp.rs` 的头注释）。
    let control = Arc::new(mcp::ControlPlane::new(runtime.clone(), config_dir.clone()));
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
    for note in platform::startup_notes() {
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
    let facts = platform::host_facts();
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
    //     两步都要：`install()` 挂上监听器（此后设置页拨开关就是热切换，见 `mcp.rs`），
    //     `reconcile()` 按**现在**这一档把服务起起来。`install()` 排在 `set_host_facts()`
    //     之后不是巧合——监听器一挂上，任何一次设置变更都会走到起停。
    state.control.install();
    state
        .control
        .reconcile(mcp::Switch::from_settings(&runtime.settings()));

    Ok(state)
}

/// 退出前收摊。
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
