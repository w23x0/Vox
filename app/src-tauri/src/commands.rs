//! Tauri 命令。命令名和参数名照 `app/ui/src/api.ts`，改一个字就对不上。
//!
//! 设计约束：
//! - 所有业务判断都在 `Runtime` 里，这里只是薄薄一层胶水。
//! - `tauri::State` 不是 `Send`，不能跨 `.await` 持有——先把 `Arc` 克隆出来。
//! - 库代码里不许 `unwrap()` / `expect()`。

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::Manager;
use vox_core::catalog;
use vox_core::event::Pipeline;
use vox_core::settings::{ModelProvider, Settings};

use crate::dto::{SettingsDto, SnapshotDto};
use crate::state::AppState;

type State<'a> = tauri::State<'a, Arc<AppState>>;

// ─── 快照 ──────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn snapshot(state: State<'_>) -> SnapshotDto {
    crate::dto::snapshot(&state)
}

// ─── 设置 ──────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn update_settings(state: State<'_>, patch: serde_json::Value) -> Result<SettingsDto, String> {
    // 0. 兼容旧 UI 的顶层 model；新 UI 的两条流水线字段已经和内核同名。
    let patch = crate::dto::patch_to_kernel(patch);

    // 1. 当前设置序列化成 JSON Value
    let current = serde_json::to_value(state.runtime.settings())
        .map_err(|e| format!("序列化当前设置失败：{e}"))?;

    // 2. 深合并
    let mut merged = current;
    deep_merge(&mut merged, patch);

    // 3. 反序列化回强类型——不合法的 patch 在这里就会被拦住
    let new_settings: Settings =
        serde_json::from_value(merged).map_err(|e| format!("设置格式不合法：{e}"))?;

    // 4. 交给内核，内核会 normalize + 发事件 + 热更新在跑的会话
    let result = state.runtime.update_settings(|s| *s = new_settings);
    Ok(result.into())
}

// ─── 密钥 ──────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn set_api_key(state: State<'_>, provider: String, key: String) -> Result<(), String> {
    // 内核 `set_api_key` 自己处理 trim 和空字符串语义（空 = 清空），直接转发。
    // 内核内部不返回错误（失败走 Notice），签名保持 Result 是为了前端一致性。
    state
        .runtime
        .set_api_key_for(parse_provider(&provider)?, &key);
    Ok(())
}

// ─── 流水线开关 ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn start_pipeline(state: State<'_>, pipeline: String) -> Result<(), String> {
    let p = parse_pipeline(&pipeline)?;
    state.runtime.start(p);
    Ok(())
}

#[tauri::command]
pub fn stop_pipeline(state: State<'_>, pipeline: String) -> Result<(), String> {
    let p = parse_pipeline(&pipeline)?;
    state.runtime.stop(p);
    Ok(())
}

#[tauri::command]
pub fn toggle_pipeline(state: State<'_>, pipeline: String) -> Result<(), String> {
    let p = parse_pipeline(&pipeline)?;
    state.runtime.toggle(p);
    Ok(())
}

// ─── VRChat OSC（写回聊天框/头像参数） ────────────────────────────────────────────

#[tauri::command]
pub fn osc_start(
    state: State<'_>,
    port: u16,
    chatbox_enabled: bool,
    avatar_param: String,
    avatar_enabled: bool,
) -> Result<(), String> {
    let mut slot = state.osc.lock();
    if let Some(client) = slot.as_mut() {
        // 已起时只更新配置，不复建套接字（端口建时定死）。
        client.set_chat_enabled(chatbox_enabled);
        client.set_avatar_param(avatar_param);
        client.set_avatar_enabled(avatar_enabled);
        return Ok(());
    }
    let mut client = vox_osc::OscClient::new(port)?;
    client.set_chat_enabled(chatbox_enabled);
    client.set_avatar_param(avatar_param);
    client.set_avatar_enabled(avatar_enabled);
    *slot = Some(client);
    Ok(())
}

#[tauri::command]
pub fn osc_update(
    state: State<'_>,
    chatbox_enabled: bool,
    avatar_param: String,
    avatar_enabled: bool,
) -> Result<(), String> {
    let mut slot = state.osc.lock();
    match slot.as_mut() {
        Some(client) => {
            client.set_chat_enabled(chatbox_enabled);
            client.set_avatar_param(avatar_param);
            client.set_avatar_enabled(avatar_enabled);
            Ok(())
        }
        None => Err("OSC 未启动".into()),
    }
}

#[tauri::command]
pub fn osc_stop(state: State<'_>) -> Result<(), String> {
    let client = state.osc.lock().take();
    drop(client);
    Ok(())
}

#[tauri::command]
pub fn osc_send_chatbox(state: State<'_>, text: String) -> Result<(), String> {
    let slot = state.osc.lock();
    match slot.as_ref() {
        // 测试发送 = 真发一条 VRChat 聊天消息（immediate=true，不弹输入框），
        // 让用户确认 OSC 链路通、VRChat 收到了。
        Some(client) => client.chatbox(&text, true),
        None => Err("OSC 未启动".into()),
    }
}

#[tauri::command]
pub fn osc_set_avatar(state: State<'_>, on: bool) -> Result<(), String> {
    let slot = state.osc.lock();
    match slot.as_ref() {
        Some(client) => {
            let param = client.avatar_param();
            if param.is_empty() {
                return Err("尚未配置 OSC 头像参数".into());
            }
            client.set_avatar_bool(param, on)
        }
        None => Err("OSC 未启动".into()),
    }
}

// ─── 用量 ──────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn reset_usage(state: State<'_>) {
    state.runtime.reset_usage();
}

#[tauri::command]
pub fn reset_usage_model(state: State<'_>, model: String) {
    // 内核没有单模型重置方法，用"取出账本 → 改 → 塞回去"的方式实现。
    // `load_usage` 会照常发 `UsageChanged` 事件并触发落盘。
    //
    // 已知竞态（装配层修不掉）：这三步之间锁是断开的。如果正好有一次翻译在计费，
    // 工作线程在窗口内调 `record_usage`，那笔记录会被 `load_usage` 的整体替换
    // 静默盖掉——用量少一笔。窗口只有几微秒，且要求用户恰好在计费瞬间点"重置"。
    // 想彻底修得在内核加一个锁内完成的 `Runtime::reset_usage_model(&str)`
    // （对照现成的 `reset_usage`），那是 vox-core 的改动，不在本层职责内。
    let mut ledger = state.runtime.usage();
    ledger.reset_model(&model);
    state.runtime.load_usage(ledger);
}

// ─── 设备刷新 ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn refresh_devices(state: State<'_>) -> Result<(), String> {
    // `tauri::State` 不是 Send，不能跨 await 持有。先把需要的东西 clone 出来。
    let registry = Arc::clone(&state.registry);
    let runtime = state.runtime.clone();

    // 设备枚举要走 COM，阻塞几十毫秒，必须在阻塞线程池上跑，不能卡 UI 线程。
    let snapshot =
        tauri::async_runtime::spawn_blocking(move || crate::devices::scan(registry.as_ref()))
            .await
            .map_err(|e| format!("设备枚举线程异常：{e}"))?;

    runtime.set_devices(snapshot);
    Ok(())
}

// ─── 虚拟麦克风 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CableActionDto {
    pub needs_reboot: bool,
    pub multichannel_hidden: bool,
}

/// VB-CABLE 的安装 / 卸载 / 多声道端点管理**整块都是 Windows 专属**：
/// 下载安装包、静默安装、UAC 提权、按进程关掉占用方，全是 Win32 的活。
///
/// 模块里的四个函数是**普通函数**（不是 `#[tauri::command]`）：tauri 宏会生成
/// `__cmd__*` 支持项，那些东西没法通过 `pub use` 转发。对外的四个命令在模块
/// 外面，各自 `#[cfg]` 一段——Windows 调这里，Linux 直接回"这个平台不需要装
/// 虚拟声卡"。前端的调用点因此不用分平台。
#[cfg(windows)]
mod cable_admin {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum CableAction {
        Install,
        Uninstall,
    }

    pub async fn install(state: State<'_>) -> Result<CableActionDto, String> {
        manage_virtual_cable(state, CableAction::Install, false).await
    }

    pub async fn uninstall(
        state: State<'_>,
        close_blockers: bool,
    ) -> Result<CableActionDto, String> {
        // 播放线程可能正占着 CABLE Input。先停「对外说话」，让设备句柄尽快释放。
        state.runtime.stop(Pipeline::Speak);
        manage_virtual_cable(state, CableAction::Uninstall, close_blockers).await
    }

    pub async fn blockers() -> Result<Vec<vox_core::ports::AudioApp>, String> {
        tauri::async_runtime::spawn_blocking(vox_audio_win::virtual_cable_blocking_apps)
            .await
            .map_err(|e| format!("占用检测线程异常：{e}"))?
            .map_err(|e| e.message)
    }

    async fn manage_virtual_cable(
        state: State<'_>,
        action: CableAction,
        close_blockers: bool,
    ) -> Result<CableActionDto, String> {
        // State 不能跨 await：只留下可安全送进阻塞线程的克隆。
        let runtime = state.runtime.clone();
        let registry = Arc::clone(&state.registry);

        let (outcome, restore_result, hide_outcome, devices) =
            tauri::async_runtime::spawn_blocking(move || {
                use vox_audio_win::{
                    CableStatus, DownloadOutcome, InstallOutcome, ProductDisclosure,
                };

                if matches!(action, CableAction::Uninstall) {
                    let blockers = vox_audio_win::virtual_cable_blocking_apps().unwrap_or_default();
                    if !blockers.is_empty() && !close_blockers {
                        let names = blockers
                            .iter()
                            .map(|app| app.display_name.as_str())
                            .collect::<Vec<_>>()
                            .join("、");
                        return (
                            InstallOutcome::Failed(format!("以下应用仍在占用虚拟麦克风：{names}")),
                            None,
                            None,
                            crate::devices::scan(registry.as_ref()),
                        );
                    }
                    if close_blockers {
                        if let Err(message) = close_cable_blockers(&blockers) {
                            return (
                                InstallOutcome::Failed(message),
                                None,
                                None,
                                crate::devices::scan(registry.as_ref()),
                            );
                        }
                        std::thread::sleep(std::time::Duration::from_millis(800));
                    }
                }

                let current = vox_audio_win::cable::detect();
                let settled = match (action, current) {
                    (CableAction::Install, CableStatus::Installed)
                    | (CableAction::Uninstall, CableStatus::NotInstalled) => {
                        Some(InstallOutcome::Succeeded)
                    }
                    (CableAction::Install, CableStatus::InstalledPendingReboot) => {
                        Some(InstallOutcome::NeedsReboot)
                    }
                    (CableAction::Install, CableStatus::UninstallIncomplete) => {
                        Some(InstallOutcome::Failed(
                            "驱动刚卸载，必须先重启 Windows 才能重新安装。".into(),
                        ))
                    }
                    (CableAction::Uninstall, CableStatus::InstalledPendingReboot) => Some(
                        InstallOutcome::Failed("驱动刚安装，必须先重启 Windows 才能卸载。".into()),
                    ),
                    _ => None,
                };

                let outcome = if let Some(settled) = settled {
                    (settled, None)
                } else {
                    // 真要执行安装。装上之后 Windows/官方安装器会把默认播放/录音设备切到
                    // CABLE 端点，先把当前默认设备记下来，装完再写回去，用户听歌才不被打断。
                    let preserved = if matches!(action, CableAction::Install) {
                        vox_audio_win::capture_default_endpoints()
                    } else {
                        (None, None)
                    };
                    // UI 已展示 VB-CABLE 全名、官网和 Donationware/授权入口。
                    let disclosure = ProductDisclosure::shown();
                    let download = vox_audio_win::cable::download(
                        disclosure,
                        &vox_audio_win::cable::default_download_dir(),
                        |_downloaded, _total| {},
                    );
                    let result = match download {
                        DownloadOutcome::Saved { path, .. } => match action {
                            CableAction::Install => {
                                vox_audio_win::cable::install(disclosure, &path)
                            }
                            CableAction::Uninstall
                                if matches!(current, CableStatus::UninstallIncomplete) =>
                            {
                                vox_audio_win::uninstall_with_audio_reset(disclosure, &path)
                            }
                            CableAction::Uninstall => {
                                vox_audio_win::cable::uninstall(disclosure, &path)
                            }
                        },
                        DownloadOutcome::SizeMismatch { hint, .. }
                        | DownloadOutcome::Unavailable { hint, .. }
                        | DownloadOutcome::Failed(hint) => InstallOutcome::Failed(hint),
                    };
                    // 安装成功才恢复；无人记下任何默认设备时更是空转。
                    let restore = if matches!(action, CableAction::Install)
                        && matches!(
                            &result,
                            InstallOutcome::Succeeded | InstallOutcome::NeedsReboot
                        )
                        && (preserved.0.is_some() || preserved.1.is_some())
                    {
                        Some(vox_audio_win::elevate_and_restore_defaults(
                            preserved.0,
                            preserved.1,
                        ))
                    } else {
                        None
                    };
                    (result, restore)
                };
                let (outcome, restore_result) = outcome;
                // 安装后的默认动作：把不需要的 16 声道端点从系统设备列表中隐藏。
                // 这是独立 PnP 端点，操作不会影响普通播放端和 CABLE Output。
                let hide_outcome = if matches!(action, CableAction::Install)
                    && matches!(
                        &outcome,
                        InstallOutcome::Succeeded | InstallOutcome::NeedsReboot
                    ) {
                    Some(vox_audio_win::set_multichannel_endpoint_enabled(false))
                } else {
                    None
                };
                let devices = crate::devices::scan(registry.as_ref());
                (outcome, restore_result, hide_outcome, devices)
            })
            .await
            .map_err(|e| format!("驱动管理线程异常：{e}"))?;

        // 不等四秒轮询，操作结束立刻把最新设备状态推给前端。
        select_regular_cable_output(&runtime, &devices);
        runtime.set_devices(devices);

        let multichannel_hidden = is_multichannel_hidden();
        if let Some(vox_audio_win::EndpointToggleOutcome::Failed(message)) = hide_outcome {
            runtime.notify(vox_core::event::Notice::warning(format!(
                "虚拟麦克风已安装，但 16 声道端点未能自动隐藏：{message}"
            )));
        }
        if let Some(Err(message)) = restore_result {
            runtime.notify(vox_core::event::Notice::warning(format!(
                "虚拟麦克风已安装，但系统默认声音设备没有恢复：{message}"
            )));
        }

        match outcome {
            vox_audio_win::InstallOutcome::Succeeded => Ok(CableActionDto {
                needs_reboot: false,
                multichannel_hidden,
            }),
            vox_audio_win::InstallOutcome::NeedsReboot => Ok(CableActionDto {
                needs_reboot: true,
                multichannel_hidden,
            }),
            vox_audio_win::InstallOutcome::UserDeclinedElevation => {
                Err("已取消管理员授权，驱动没有发生变化。".into())
            }
            vox_audio_win::InstallOutcome::Failed(message) => {
                Err(if matches!(action, CableAction::Uninstall) {
                    format!(
                        "{message} 请先关闭 Discord、VRChat 和其它正在使用虚拟麦克风的软件后重试。"
                    )
                } else {
                    message
                })
            }
        }
    }

    fn close_cable_blockers(blockers: &[vox_core::ports::AudioApp]) -> Result<(), String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE,
        };

        let me = std::process::id();
        for app in blockers {
            if app.pid == 0 || app.pid == me {
                continue;
            }
            // SAFETY: 只申请终止和等待权限，PID 来自当前音频会话枚举。
            let process =
                unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, app.pid) }
                    .map_err(|e| format!("无法关闭 {}：{e}", app.display_name))?;
            // SAFETY: process 是刚打开的有效进程句柄。
            let terminated = unsafe { TerminateProcess(process, 0) };
            if let Err(error) = terminated {
                // SAFETY: 句柄由本函数持有，只关闭一次。
                unsafe {
                    let _ = CloseHandle(process);
                }
                return Err(format!("无法关闭 {}：{error}", app.display_name));
            }
            // 最多等两秒让音频会话释放；超时也继续，安装器会给最终结果。
            unsafe {
                let _ = WaitForSingleObject(process, 2_000);
                let _ = CloseHandle(process);
            }
        }
        Ok(())
    }

    pub async fn set_multichannel(
        state: State<'_>,
        visible: bool,
    ) -> Result<CableActionDto, String> {
        let runtime = state.runtime.clone();
        let registry = Arc::clone(&state.registry);
        let (outcome, devices) = tauri::async_runtime::spawn_blocking(move || {
            let outcome = vox_audio_win::set_multichannel_endpoint_enabled(visible);
            let devices = crate::devices::scan(registry.as_ref());
            (outcome, devices)
        })
        .await
        .map_err(|e| format!("音频端点管理线程异常：{e}"))?;

        select_regular_cable_output(&runtime, &devices);
        runtime.set_devices(devices);
        // 16 声道端点的启停不一定改 outputs 列表（禁用后 Core Audio 仍可能旧报
        // ACTIVE），set_devices 的去重可能吞掉这次变化；这里强制补发一次，
        // 让前端尽快拿到最新快照，徽标不会卡死在旧状态。
        runtime.touch_devices();

        let hidden = is_multichannel_hidden();
        match outcome {
            vox_audio_win::EndpointToggleOutcome::Changed
            | vox_audio_win::EndpointToggleOutcome::AlreadySet
            | vox_audio_win::EndpointToggleOutcome::NotFound => Ok(CableActionDto {
                needs_reboot: false,
                multichannel_hidden: hidden,
            }),
            vox_audio_win::EndpointToggleOutcome::NeedsReboot => Ok(CableActionDto {
                needs_reboot: true,
                multichannel_hidden: hidden,
            }),
            vox_audio_win::EndpointToggleOutcome::UserDeclinedElevation => {
                Err("已取消管理员授权，16 声道端点没有发生变化。".into())
            }
            vox_audio_win::EndpointToggleOutcome::Failed(message) => Err(message),
        }
    }

    fn select_regular_cable_output(
        runtime: &vox_core::Runtime,
        devices: &vox_core::runtime::DeviceSnapshot,
    ) {
        let Some(device) = devices.outputs.iter().find(|device| {
            vox_audio_win::cable::is_cable_render(&device.name)
                && !vox_audio_win::cable::is_cable_multichannel_render(&device.name)
        }) else {
            return;
        };
        let name = device.name.clone();
        runtime.update_settings(|settings| settings.speak.output_device = Some(name));
    }

    /// 16 声道端点当前该不该在界面上显示成"已隐藏"。
    /// 禁用和"系统里根本没有这个端点"对用户是同一回事，安装和手动切换两处共用。
    fn is_multichannel_hidden() -> bool {
        matches!(
            vox_audio_win::multichannel_endpoint_status(),
            vox_audio_win::MultichannelEndpointStatus::Disabled
                | vox_audio_win::MultichannelEndpointStatus::NotPresent
        )
    }
}

#[tauri::command]
pub async fn install_virtual_cable(state: State<'_>) -> Result<CableActionDto, String> {
    #[cfg(windows)]
    {
        cable_admin::install(state).await
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        Err(virtual_device_not_needed())
    }
}

#[tauri::command]
pub async fn uninstall_virtual_cable(
    state: State<'_>,
    close_blockers: bool,
) -> Result<CableActionDto, String> {
    #[cfg(windows)]
    {
        cable_admin::uninstall(state, close_blockers).await
    }
    #[cfg(not(windows))]
    {
        let _ = (state, close_blockers);
        Err(virtual_device_not_needed())
    }
}

#[tauri::command]
pub async fn virtual_cable_blockers() -> Result<Vec<vox_core::ports::AudioApp>, String> {
    #[cfg(windows)]
    {
        cable_admin::blockers().await
    }
    #[cfg(not(windows))]
    {
        Err(virtual_device_not_needed())
    }
}

#[tauri::command]
pub async fn set_virtual_cable_multichannel_visible(
    state: State<'_>,
    visible: bool,
) -> Result<CableActionDto, String> {
    #[cfg(windows)]
    {
        cable_admin::set_multichannel(state, visible).await
    }
    #[cfg(not(windows))]
    {
        let _ = (state, visible);
        Err(virtual_device_not_needed())
    }
}

/// Linux：虚拟麦克风由 PipeWire 原生提供，没有"装"这一步。
#[cfg(not(windows))]
fn virtual_device_not_needed() -> String {
    "Linux 不需要安装虚拟声卡：虚拟麦克风由 PipeWire 原生提供，在目标程序的录音设置里选「Vox 虚拟麦」即可。"
        .to_string()
}

// ─── 模型目录在线更新 ───────────────────────────────────────────────────────────
/// 读本地覆盖版目录（app_config_dir/catalog/{provider}.json）。没有就返回 null，
/// 前端据此回落内置副本。返回的是原始 JSON 文本，由前端按内置同构解析。
#[tauri::command]
pub fn read_catalog_override(
    app: tauri::AppHandle,
    provider: String,
) -> Result<Option<String>, String> {
    if crate::catalog_updater::catalog_file(&provider).is_none() {
        return Err(format!("未知模型服务商：{provider}"));
    }
    let config_dir = app_config_dir(&app)?;
    Ok(crate::catalog_updater::read_override(
        &config_dir,
        &provider,
    ))
}

/// 检查某服务商的模型目录有没有线上更新。只查不写。
#[tauri::command]
pub async fn check_catalog_update(
    app: tauri::AppHandle,
    provider: String,
) -> Result<CatalogUpdateCheckDto, String> {
    parse_provider(&provider)?;
    let config_dir = app_config_dir(&app)?;
    let latest = crate::catalog_updater::check_update(&provider).await?;
    Ok(CatalogUpdateCheckDto {
        current: crate::catalog_updater::local_verified_at(&config_dir, &provider),
        latest,
    })
}

/// 应用线上模型目录：下载 → 校验 → 覆盖到 app_config_dir。返回落盘后的 verified_at。
#[tauri::command]
pub async fn apply_catalog_update(
    app: tauri::AppHandle,
    provider: String,
) -> Result<CatalogUpdateAppliedDto, String> {
    parse_provider(&provider)?;
    let config_dir = app_config_dir(&app)?;
    let (file, verified) = crate::catalog_updater::apply_update(&config_dir, &provider).await?;
    Ok(CatalogUpdateAppliedDto { file, verified })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogUpdateCheckDto {
    /// 当前生效的 verified_at（可能是覆盖版或内置）。
    pub current: String,
    /// 线上仓库里最新的 verified_at。
    pub latest: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogUpdateAppliedDto {
    /// 覆盖到的文件名，如 "aliyun.json"。
    pub file: String,
    /// 落盘后的 verified_at。
    pub verified: String,
}

// ─── 打开控制台 ────────────────────────────────────────────────────────────────

/// 打开阿里云百炼控制台的 API-Key 页面。
/// 只准打开这一个写死的 URL——绝不接受前端传 URL 参数。
#[tauri::command]
pub fn open_dashscope_console(app: tauri::AppHandle) -> Result<(), String> {
    const DASHSCOPE_CONSOLE_URL: &str = "https://bailian.console.aliyun.com/?apiKey=1";
    tauri_plugin_opener::open_url(DASHSCOPE_CONSOLE_URL, None::<&str>)
        .map_err(|e| format!("打开浏览器失败：{e}"))?;
    // `app` 参数是 Tauri 宏要求的签名一部分，opener 插件并不需要它。
    let _ = app;
    Ok(())
}

#[tauri::command]
pub fn open_provider_console(provider: String) -> Result<(), String> {
    let provider = parse_provider(&provider)?;
    let url = catalog::provider_info(provider).console_url;
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| format!("打开浏览器失败：{e}"))
}

#[tauri::command]
pub fn open_virtual_cable_website() -> Result<(), String> {
    #[cfg(windows)]
    {
        tauri_plugin_opener::open_url(vox_audio_win::PRODUCT_URL, None::<&str>)
            .map_err(|e| format!("打开 VB-CABLE 官网失败：{e}"))
    }
    #[cfg(not(windows))]
    Err(virtual_device_not_needed())
}

#[tauri::command]
pub fn open_virtual_cable_donation() -> Result<(), String> {
    #[cfg(windows)]
    {
        tauri_plugin_opener::open_url(vox_audio_win::DONATION_URL, None::<&str>)
            .map_err(|e| format!("打开 VB-CABLE 授权页面失败：{e}"))
    }
    #[cfg(not(windows))]
    Err(virtual_device_not_needed())
}

/// 完全退出应用（不是收进托盘）。红绿灯的关闭按钮走这里，而不是走 `window.close()`
/// —— close() 会被 lib.rs 的 CloseRequested 拦截成 hide() 收进托盘。
/// `app.exit(0)` 和托盘菜单「退出」走同一条路径，触发 RunEvent::Exit → shutdown() 收尾。
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ─── 内部工具函数 ──────────────────────────────────────────────────────────────

/// 把前端传来的字符串解析成 `Pipeline` 枚举。三条命令共用。
fn parse_pipeline(name: &str) -> Result<Pipeline, String> {
    match name {
        "speak" => Ok(Pipeline::Speak),
        "listen" => Ok(Pipeline::Listen),
        other => Err(format!("未知的流水线：{other}")),
    }
}

fn parse_provider(name: &str) -> Result<ModelProvider, String> {
    ModelProvider::from_id(name).ok_or_else(|| format!("未知模型服务商：{name}"))
}

/// 取 `app_config_dir`。设置、用量、密钥和覆盖版模型目录都落在这个根下；
/// `read_catalog_override` / `check_catalog_update` / `apply_catalog_update`
/// 三条命令共用这一段取值 + 报错包装（错误串跟前端显示的一致）。
fn app_config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("取配置目录失败：{e}"))
}

/// 深合并 `patch` 到 `base` 上。
///
/// 规则：
/// - 两边都是 object → 递归合并（但"整体替换路径"例外，见下面）；
/// - patch 里是 `null` → 写入 null（让 `Option` 反序列化成 `None`，即"清空"）；
/// - 其它 → patch 整体覆盖 base。
///
/// 关于 map 类型的特殊处理：
/// `SpeakSettings.voice_by_language` 是 `BTreeMap<String, String>`，前端的
/// `DeepPartialNullable` 对 `Record<string, string>` 是整体替换的（不是递归）。
/// 如果也递归合并，那就永远删不掉某个语言的音色。所以维护一条"整体替换"的路径
/// 白名单：命中时直接用 patch 值覆盖，不递归。
fn deep_merge(base: &mut Value, patch: Value) {
    // 对象 + 对象 → 逐 key 递归
    if let (Some(base_map), Value::Object(patch_map)) = (base.as_object_mut(), &patch) {
        for (key, patch_value) in patch_map {
            // 检查当前 key 是否属于"整体替换"白名单
            if is_replace_whole_key(key) {
                // 不递归，直接整体替换（包括 null → 删掉）
                base_map.insert(key.clone(), patch_value.clone());
            } else if patch_value.is_object() {
                // 两边都是 object → 递归
                let entry = base_map
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                deep_merge(entry, patch_value.clone());
            } else {
                // 标量 / 数组 / null → 整体覆盖
                base_map.insert(key.clone(), patch_value.clone());
            }
        }
    } else {
        // base 不是 object 或 patch 不是 object → 整体替换
        *base = patch;
    }
}

/// "整体替换"的字段白名单。
///
/// 这些字段在 Rust 侧是 `BTreeMap<String, String>` 或类似的扁平 map，前端
/// `DeepPartialNullable` 对 `Record<string, string>` 一支是整体替换语义。
/// 如果递归合并，就永远只能加 key 不能删 key，跟前端语义对不上。
fn is_replace_whole_key(key: &str) -> bool {
    matches!(key, "voice_by_language")
}

// ─── 测试 ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- deep_merge 测试 ---

    #[test]
    fn merge_nested_field_only_touches_that_field() {
        let mut base = json!({
            "subtitle": {
                "font_size": 30,
                "speak_color": "#fff4de",
                "visible": true
            }
        });
        let patch = json!({
            "subtitle": { "font_size": 42 }
        });
        deep_merge(&mut base, patch);
        assert_eq!(base["subtitle"]["font_size"], 42);
        assert_eq!(
            base["subtitle"]["speak_color"], "#fff4de",
            "未改的字段不受影响"
        );
        assert_eq!(base["subtitle"]["visible"], true);
    }

    #[test]
    fn merge_explicit_null_sets_null() {
        // 用真的 Settings 走完整往返，验证 Option 字段能被 null 清空。
        let mut settings = Settings::default();
        settings.speak.input_device = Some("我的麦克风".to_string());
        settings.normalize();

        let mut base = serde_json::to_value(&settings).unwrap();
        let patch = json!({ "speak": { "input_device": null } });
        deep_merge(&mut base, patch);

        let restored: Settings = serde_json::from_value(base).unwrap();
        assert_eq!(
            restored.speak.input_device, None,
            "显式 null 应清空 Option 字段"
        );
    }

    #[test]
    fn merge_voice_by_language_is_replaced_wholly() {
        let mut base = json!({
            "speak": {
                "voice_by_language": { "en": "a", "ja": "b" }
            }
        });
        let patch = json!({
            "speak": {
                "voice_by_language": { "en": "c" }
            }
        });
        deep_merge(&mut base, patch);

        let map = base["speak"]["voice_by_language"].as_object().unwrap();
        assert_eq!(map.get("en").and_then(Value::as_str), Some("c"));
        assert!(map.get("ja").is_none(), "整体替换：ja 应该被删掉，不能残留");
    }

    #[test]
    fn merge_empty_patch_changes_nothing() {
        let original = json!({
            "model_name": "x",
            "speak": { "enabled": true }
        });
        let mut base = original.clone();
        deep_merge(&mut base, json!({}));
        assert_eq!(base, original);
    }

    #[test]
    fn merge_garbage_patch_causes_deser_error() {
        // 给 font_size 喂一个字符串，深合并本身不会报错，但反序列化时会。
        let mut base = serde_json::to_value(Settings::default()).unwrap();
        let patch = json!({ "subtitle": { "font_size": "不是数字" } });
        deep_merge(&mut base, patch);

        let result = serde_json::from_value::<Settings>(base);
        assert!(result.is_err(), "垃圾值应该让反序列化失败");
    }

    // --- parse_pipeline 测试 ---

    #[test]
    fn parse_pipeline_legal_values() {
        assert_eq!(parse_pipeline("speak").unwrap(), Pipeline::Speak);
        assert_eq!(parse_pipeline("listen").unwrap(), Pipeline::Listen);
    }

    #[test]
    fn parse_pipeline_illegal_value() {
        let err = parse_pipeline("invalid").unwrap_err();
        assert!(err.contains("未知的流水线"), "错误信息：{err}");
        assert!(err.contains("invalid"));
    }

    #[test]
    fn parse_provider_accepts_catalog_ids() {
        for provider in vox_core::settings::ModelProvider::ALL {
            assert_eq!(parse_provider(provider.as_id()).unwrap(), provider);
        }
        assert!(parse_provider("other").is_err());
    }
}
