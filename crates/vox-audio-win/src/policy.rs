//! 系统「默认音频设备」的恢复。
//!
//! 安装 VB-CABLE 时，官方安装器和 Windows 音频服务会把默认播放/录音设备切到新出现的
//! CABLE 端点，用户的音乐就这样被抢走。本模块在安装**前**记下当时的默认设备，
//! 安装**后**用 `IPolicyConfig`（未公开但长期稳定的 COM 接口）把它写回去。
//!
//! # 提权
//! `IPolicyConfig::SetDefaultEndpoint` 需要管理员权限。主程序通常非提权运行，
//! 所以恢复动作分两步：
//! 1. 先在本进程直接试（用户以管理员运行 app 的少数情况）；
//! 2. 失败则用 `ShellExecuteExW` + `runas` 以 `--vox-restore-defaults` 参数
//!    重新拉起自身 exe，由隐藏 CLI 模式完成设置后退出。
//!
//! 隐藏模式必须在 `tauri_plugin_single_instance` 注册**之前**拦截 argv 并抢先
//! `exit`，否则第二个实例会被单实例插件当作「重复启动」吞掉，永远轮不到做恢复。

use std::ffi::c_void;

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Media::Audio::{eCapture, eRender, ERole};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows_core::PCWSTR;

use crate::com::{hr_err, run_elevated, to_wide, ComGuard, ElevateMessages, ElevatedRun};
use crate::devices;

/// 未公开的 PolicyConfig 类。IID/CLSID 长期稳定，被 SoundSwitch 等第三方长期使用。
const CLSID_POLICY_CONFIG: windows_core::GUID =
    windows_core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// 隐藏 CLI 模式标记。DataFlow 的默认设备写回必须提权，只能交给以管理员重拉的自身。
const RESTORE_ARG: &str = "--vox-restore-defaults";
/// 隐藏 CLI 模式里“没有要恢复的设备”的占位参数。
const RESTORE_NONE: &str = "-";

/// `IPolicyConfig` 的 vtable。前 10 个方法不会调用，但必须占位保证偏移正确：
/// IUnknown(3 槽) + 这 10 槽后，第 14 个槽（索引 13）才是 `SetDefaultEndpoint`。
///
/// 必须 `pub`：`define_interface!` 生成的接口类型要引用它。
#[repr(C)]
pub struct IPolicyConfigVtbl {
    base__: windows_core::IUnknown_Vtbl,
    get_mix_format:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *mut *mut c_void) -> windows_core::HRESULT,
    get_device_format: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        bool,
        *mut *mut c_void,
    ) -> windows_core::HRESULT,
    reset_device_format: unsafe extern "system" fn(*mut c_void, PCWSTR) -> windows_core::HRESULT,
    set_device_format: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *const c_void,
        *const c_void,
    ) -> windows_core::HRESULT,
    get_processing_period: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        bool,
        *mut *const c_void,
        *mut *const c_void,
    ) -> windows_core::HRESULT,
    set_processing_period:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *const c_void) -> windows_core::HRESULT,
    get_share_mode:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *mut *const c_void) -> windows_core::HRESULT,
    set_share_mode:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *const c_void) -> windows_core::HRESULT,
    get_property_value: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *const c_void,
        *mut *const c_void,
    ) -> windows_core::HRESULT,
    set_property_value: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *const c_void,
        *const c_void,
    ) -> windows_core::HRESULT,
    set_default_endpoint:
        unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> windows_core::HRESULT,
    set_endpoint_visibility:
        unsafe extern "system" fn(*mut c_void, PCWSTR, bool) -> windows_core::HRESULT,
}

windows_core::define_interface!(
    IPolicyConfig,
    IPolicyConfigVtbl,
    0xf8679f50_850a_41cf_9282_3bd47f5a1d8c
);

/// 记录当前默认播放、默认录音端点的 ID。
///
/// 返回 `(默认播放, 默认录音)`，没有默认设备（比如根本没声卡）时对应位为 `None`。
/// 必须放在 VB-CABLE 出现**之前**调用，否则记下的就是 CABLE 自己。
pub fn capture_default_endpoints() -> (Option<String>, Option<String>) {
    let Ok(_com) = ComGuard::mta() else {
        return (None, None);
    };
    let Ok(enumerator) = devices::enumerator() else {
        return (None, None);
    };
    (
        devices::default_device_id(&enumerator, eRender),
        devices::default_device_id(&enumerator, eCapture),
    )
}

/// 恢复默认设备。先在本进程尝试（app 以管理员运行时能成），否则提权重拉自身执行。
pub fn elevate_and_restore_defaults(
    render: Option<String>,
    capture: Option<String>,
) -> Result<(), String> {
    // 没记录到任何默认设备（例如新装机无输出）时没什么可恢复的，直接算成功。
    if render.is_none() && capture.is_none() {
        return Ok(());
    }
    // 多数情况下主进程没有管理员权限，SetDefaultEndpoint 会返回 0x80070005。
    // 把这个错误翻译出来，判断要不要走提权重拉。
    match set_default_endpoints(&render, &capture) {
        Ok(()) => return Ok(()),
        Err(message) if is_access_denied(&message) => {
            tracing::info!("本进程未提权（{message}），改用提权重拉自身恢复默认设备")
        }
        Err(message) => return Err(message),
    }

    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find own executable path: {e}"))?;
    let args: Vec<String> = [RESTORE_ARG.to_string(), encode(&render), encode(&capture)]
        .into_iter()
        .collect();
    let code = run_self_elevated(&exe, &args)?;
    match code {
        0 => Ok(()),
        val if val == ERROR_CANCELLED.0 => Err(
            "Administrator approval was cancelled; the default sound devices were not restored. You can change them back in Windows sound settings.".into(),
        ),
        other => Err(format!(
            "Restoring the default sound devices failed (exit code {other}). You can change them back in Windows sound settings."
        )),
    }
}

/// 检查自身 argv。若带隐藏恢复标记，执行恢复后立刻 `exit`，不再进入 Tauri 构建。
pub fn restore_via_args_if_requested() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(RESTORE_ARG) {
        return false;
    }
    let render = decode(args.next().as_deref());
    let capture = decode(args.next().as_deref());
    tracing::info!("进入默认设备恢复模式（render={render:?} capture={capture:?}）");
    let code = match set_default_endpoints(&render, &capture) {
        Ok(()) => 0,
        Err(message) => {
            tracing::error!("恢复默认设备失败：{message}");
            1
        }
    };
    std::process::exit(code)
}

/// 在本进程内把两个端点写回 `eConsole` 默认。
///
/// 改写的只是「系统默认设备」标签，不碰默认通信设备；app 自己的译文输出仍按
/// 设备名走 `speak.output_device`，与这里无关。
fn set_default_endpoints(render: &Option<String>, capture: &Option<String>) -> Result<(), String> {
    let Ok(_com) = ComGuard::mta() else {
        return Err("failed to initialize COM".into());
    };
    if let Some(device_id) = render.as_deref() {
        set_default_endpoint(device_id)
            .map_err(|e| format!("failed to restore the default playback device: {e}"))?;
    }
    if let Some(device_id) = capture.as_deref() {
        set_default_endpoint(device_id)
            .map_err(|e| format!("failed to restore the default recording device: {e}"))?;
    }
    Ok(())
}

/// 把单个端点写回 `eConsole` 默认设备。
fn set_default_endpoint(device_id: &str) -> Result<(), String> {
    // SAFETY: CLSID/接口 IID 是常量；CoCreateInstance 拿实例，接口由 windows_core 管引用计数。
    // CLSCTX_ALL 允许 COM 为所需线程模型自动代理。
    let policy: IPolicyConfig = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
        .map_err(|e| hr_err("failed to create the audio policy object", e.code()).message)?;

    let wide = to_wide(device_id);
    // SAFETY: 调用期间 wide 保持有效；SetDefaultEndpoint(此指针→ID, eConsole)。
    let hr = unsafe {
        (windows_core::Interface::vtable(&policy).set_default_endpoint)(
            windows_core::Interface::as_raw(&policy),
            PCWSTR(wide.as_ptr()),
            ERole(0),
        )
    };
    if !hr.is_ok() {
        return Err(hr_err("failed to set the default sound device", hr).message);
    }
    Ok(())
}

/// 用 `runas` 以隐藏窗口拉起 `exe` 并等待退出，返回退出码。UAC 拒绝时返回提示。
fn run_self_elevated(exe: &std::path::Path, args: &[String]) -> Result<u32, String> {
    let cwd = exe.parent().unwrap_or_else(|| std::path::Path::new("."));
    match run_elevated(
        exe,
        &args.join(" "),
        cwd,
        60_000,
        &ElevateMessages {
            start: "failed to launch the restore process as administrator",
            no_handle: "the restore process returned no handle",
            timeout: "timed out waiting for the restore process",
            exit_code: "failed to read the restore process exit code",
        },
    ) {
        Ok(ElevatedRun::Exited(code)) => Ok(code),
        Ok(ElevatedRun::Declined) => Err("Administrator approval was cancelled".into()),
        Err(message) => Err(message),
    }
}

/// 把可能为 `None` 的设备 ID 编码成命令行参数形式。
fn encode(id: &Option<String>) -> String {
    id.as_deref().unwrap_or(RESTORE_NONE).to_string()
}

/// 命令行参数反过来解码回 `Option`。
fn decode(raw: Option<&str>) -> Option<String> {
    match raw {
        Some(value) if !value.is_empty() && value != RESTORE_NONE => Some(value.to_string()),
        _ => None,
    }
}

/// 本进程未提权时 SetDefaultEndpoint 返回的 HRESULT 是 0x80070005（访问被拒绝）。
fn is_access_denied(message: &str) -> bool {
    message.contains("0x80070005")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iids_are_nonzero() {
        assert_ne!(CLSID_POLICY_CONFIG, windows_core::GUID::zeroed());
        assert_ne!(
            windows_core::GUID::from_u128(0xf8679f50_850a_41cf_9282_3bd47f5a1d8c),
            windows_core::GUID::zeroed()
        );
    }

    #[test]
    fn vtable_keeps_set_default_endpoint_at_the_right_slot() {
        use std::mem::{offset_of, size_of};
        // 3 基槽 + 12 个方法槽，全是函数指针。
        assert_eq!(size_of::<IPolicyConfigVtbl>(), 15 * size_of::<usize>());
        // SetDefaultEndpoint 是第 14 个槽（base 3 + 10 个前置方法）。
        let expected = size_of::<windows_core::IUnknown_Vtbl>() + 10 * size_of::<usize>();
        assert_eq!(
            offset_of!(IPolicyConfigVtbl, set_default_endpoint),
            expected
        );
    }

    #[test]
    fn encode_decode_roundtrip() {
        let some = Some("设备 ID".to_string());
        assert_eq!(decode(Some(&encode(&some))), some);
        let none: Option<String> = None;
        assert_eq!(decode(Some(&encode(&none))), None);
        assert_eq!(decode(None), None);
        assert_eq!(decode(Some("")), None);
    }

    #[test]
    fn access_denied_is_recognized() {
        assert!(is_access_denied("restore default failed：0x80070005"));
        assert!(!is_access_denied("这是普通错误"));
    }
}
