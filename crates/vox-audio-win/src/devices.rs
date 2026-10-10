//! WASAPI 端点枚举：列设备、按名字找设备、拿默认设备。
//!
//! 只列 `DEVICE_STATE_ACTIVE` 的端点。禁用和拔掉的设备也能枚举出来，
//! 但列给用户选只会让人困惑（选了打不开）。

use vox_core::notice::{key, Msg};
use vox_core::ports::{DeviceInfo, PortError, PortResult};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, STGM_READ};

use crate::com::{wide_to_string, WinContext};

/// 建一个端点枚举器。调用线程必须已经初始化过 COM。
pub(crate) fn enumerator() -> PortResult<IMMDeviceEnumerator> {
    // SAFETY: CLSID 是系统内置的多媒体设备枚举器，调用线程已由 ComGuard 初始化。
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .ctx("failed to create the audio device enumerator")
}

/// 读端点的友好名，例如 `扬声器 (Realtek(R) Audio)`。
pub(crate) fn friendly_name(device: &IMMDevice) -> PortResult<String> {
    // SAFETY: device 是有效接口；属性库以只读方式打开，PROPVARIANT 用完立刻清理。
    unsafe {
        let store = device
            .OpenPropertyStore(STGM_READ)
            .ctx("failed to open the device properties")?;
        let mut value = store
            .GetValue(&PKEY_Device_FriendlyName)
            .ctx("failed to read the device name")?;
        let name = wide_to_string(value.Anonymous.Anonymous.Anonymous.pwszVal.0);
        // PROPVARIANT 里的字符串是 COM 分配的，必须交回去，否则每次枚举都漏一点。
        let _ = PropVariantClear(&mut value);
        Ok(name)
    }
}

/// 读端点 ID（那串 `{0.0.0.00000000}.{guid}`），用来判断是不是默认设备。
pub(crate) fn device_id(device: &IMMDevice) -> PortResult<String> {
    // SAFETY: device 有效；GetId 返回 COM 分配的宽字符串，读完立刻释放。
    unsafe {
        let raw = device.GetId().ctx("failed to read the device ID")?;
        let id = wide_to_string(raw.0);
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _));
        Ok(id)
    }
}

/// 默认端点的 ID。没有默认设备（比如根本没声卡）时返回 `None`。
pub(crate) fn default_device_id(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Option<String> {
    // SAFETY: enumerator 有效；拿不到默认设备是正常情况（返回 Err），不当错误处理。
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }.ok()?;
    device_id(&device).ok()
}

/// 列出某个方向上的所有活动端点。
pub(crate) fn list_devices(flow: EDataFlow) -> PortResult<Vec<DeviceInfo>> {
    let enumerator = enumerator()?;
    let default_id = default_device_id(&enumerator, flow);
    // SAFETY: enumerator 有效，集合和其中的设备接口都由 windows-rs 管引用计数。
    unsafe {
        let collection = enumerator
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .ctx("failed to enumerate audio devices")?;
        let count = collection
            .GetCount()
            .ctx("failed to read the device count")?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                // 单个设备取不出来就跳过，不能让一个坏端点毁掉整张列表。
                continue;
            };
            let Ok(name) = friendly_name(&device) else {
                continue;
            };
            let is_default = match (&default_id, device_id(&device)) {
                (Some(d), Ok(id)) => *d == id,
                _ => false,
            };
            out.push(DeviceInfo { name, is_default });
        }
        Ok(out)
    }
}

/// 按名字找端点。`None` 表示默认端点。
///
/// 匹配顺序：完全相等（忽略大小写）→ 包含。包含匹配是给 UI 留的余地：
/// 有些地方存的是截断过的名字，或者用户自己敲了一半。
pub(crate) fn find_device(flow: EDataFlow, name: Option<&str>) -> PortResult<IMMDevice> {
    let enumerator = enumerator()?;
    let Some(wanted) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        // SAFETY: enumerator 有效；eConsole 表示“普通用途”的默认设备，
        // 跟 Windows 声音设置里那个默认设备一致。
        return unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
            .ctx("failed to get the default audio device");
    };

    // SAFETY: enumerator 有效，循环里的接口都由 windows-rs 托管。
    unsafe {
        let collection = enumerator
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .ctx("failed to enumerate audio devices")?;
        let count = collection
            .GetCount()
            .ctx("failed to read the device count")?;
        let mut fallback = None;
        let wanted_lower = wanted.to_lowercase();
        for i in 0..count {
            let Ok(device) = collection.Item(i) else {
                continue;
            };
            let Ok(current) = friendly_name(&device) else {
                continue;
            };
            let current_lower = current.to_lowercase();
            if current_lower == wanted_lower {
                return Ok(device);
            }
            if fallback.is_none()
                && (current_lower.contains(&wanted_lower) || wanted_lower.contains(&current_lower))
            {
                fallback = Some(device);
            }
        }
        fallback.ok_or_else(|| {
            let (user_key, kind) = if flow == eRender {
                (key::OUTPUT_DEVICE_NOT_FOUND, "output")
            } else {
                (key::INPUT_DEVICE_NOT_FOUND, "input")
            };
            PortError::user(
                Msg::new(user_key).param("name", &wanted),
                format!("no {kind} device named '{wanted}' (it may be disabled or unplugged)"),
            )
        })
    }
}

/// 输入端点方向。
pub(crate) const CAPTURE: EDataFlow = eCapture;
/// 输出端点方向。
pub(crate) const RENDER: EDataFlow = eRender;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ComGuard;

    #[test]
    fn enumerating_real_devices_does_not_error() {
        let _com = ComGuard::mta().unwrap();
        // 没声卡的机器会返回空列表，但不该报错。
        let outputs = list_devices(RENDER).unwrap();
        let inputs = list_devices(CAPTURE).unwrap();
        for d in outputs.iter().chain(inputs.iter()) {
            assert!(!d.name.is_empty());
        }
        // 有设备的话，默认设备最多只有一个。
        assert!(outputs.iter().filter(|d| d.is_default).count() <= 1);
        assert!(inputs.iter().filter(|d| d.is_default).count() <= 1);
    }

    #[test]
    fn missing_device_name_gives_chinese_error() {
        let _com = ComGuard::mta().unwrap();
        let err = find_device(RENDER, Some("绝对不存在的设备名 zzz")).unwrap_err();
        assert!(err.message.contains("找不到"), "{}", err.message);
    }
}
