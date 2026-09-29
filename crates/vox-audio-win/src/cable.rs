//! VB-CABLE 检测与静默安装。
//!
//! 授权义务（不要在后续重构里丢掉这一条）：
//! VB-CABLE 是 VB-Audio 的捐赠软件（donationware）。允许打包/自动安装的前提是
//! **用户能看见并认出这是 VB-CABLE，并且有机会去官网捐赠**。所以：
//! - 界面在触发安装之前，必须显示 `PRODUCT_NAME` 以及 `PRODUCT_URL` / `DONATION_URL`；
//! - 代码层面用 `ProductDisclosure` 把这件事钉死：`download` 和 `install` 都要这个凭证，
//!   而凭证只能由“我确实已经把名字和链接展示给用户了”的调用方构造出来。
//!
//! 谁删了这段注释或者绕过 `ProductDisclosure`，就是在违反 VB-Audio 的授权条件。
//!
//! 还有一条硬规矩：本模块的测试绝不联网、绝不执行安装程序。下载和安装只有用户
//! 明确点了之后才会真的跑起来——那是他自己的机器，得他自己决定。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::com::{hr_err, run_elevated, to_wide, ComGuard, ElevateMessages, ElevatedRun};
use crate::devices;

/// 产品名。界面上必须原样出现这个名字，别改成“虚拟声卡”之类的泛称。
pub const PRODUCT_NAME: &str = "VB-CABLE Virtual Audio Device";

/// 产品主页。安装前给用户的链接。
pub const PRODUCT_URL: &str = "https://vb-audio.com/Cable/";

/// 捐赠页。授权义务的另一半：用户得能找到地方付钱。
pub const DONATION_URL: &str = "https://vb-audio.com/Services/licensing.htm";

/// 官方驱动包的主机名。WinHttpConnect 要单独的主机，不吃完整 URL。
const DOWNLOAD_HOST: &str = "download.vb-audio.com";

/// 包在主机上的路径。WinHttpOpenRequest 也要单独的路径。
const DOWNLOAD_PATH: &str = "/Download_CABLE/VBCABLE_Driver_Pack45.zip";

/// 官方驱动包直链（钉死版本，避免哪天官网改结构就抓到别的东西）。
///
/// 真正发请求用的是上面拆开的 `DOWNLOAD_HOST` + `DOWNLOAD_PATH`；这个常量
/// 给界面显示和报错文案用。下面有测试盯着三者一致，改一个另两个会红。
pub const DOWNLOAD_URL: &str =
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

/// 上面那个包的预期字节数。对不上就不解压、不安装，直接让用户去官网。
pub const DOWNLOAD_EXPECTED_BYTES: u64 = 1_318_877;

/// SetupAPI 设备信息集守卫。Drop 时销毁列表。
struct DeviceSetGuard(windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO);

impl Drop for DeviceSetGuard {
    fn drop(&mut self) {
        // SAFETY: 句柄由本守卫独占。
        unsafe {
            let _ =
                windows::Win32::Devices::DeviceAndDriverInstallation::SetupDiDestroyDeviceInfoList(
                    self.0,
                );
        }
    }
}

/// 下载后落盘的文件名。
pub const ARCHIVE_FILE_NAME: &str = "VBCABLE_Driver_Pack45.zip";

/// 压缩包里的 64 位安装器。
pub const INSTALLER_EXE: &str = "VBCABLE_Setup_x64.exe";

/// 静默安装参数：`-i` 装，`-h` 不弹界面。
pub const INSTALL_ARGS: &str = "-i -h";

/// 静默卸载参数：`-u` 卸，`-h` 不弹界面。
pub const UNINSTALL_ARGS: &str = "-u -h";

/// 装好以后出现的输出端点名（我们把译文往这儿播）。
pub const RENDER_ENDPOINT_NAME: &str = "CABLE Input (VB-Audio Virtual Cable)";

/// 装好以后出现的输入端点名（VRChat 把这个当麦克风）。
pub const CAPTURE_ENDPOINT_NAME: &str = "CABLE Output (VB-Audio Virtual Cable)";

/// 安装状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CableStatus {
    /// 端点已经在了，可以直接用。
    Installed,
    /// 服务已注册但端点还没出现——装完没重启。
    InstalledPendingReboot,
    /// 根设备已经移除，但音频服务或旧端点还在，重启后才会彻底消失。
    UninstallIncomplete,
    /// 没装。
    NotInstalled,
}

/// 新版 VB-CABLE 附带的 16 声道播放端点状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultichannelEndpointStatus {
    NotPresent,
    Enabled,
    Disabled,
}

/// 启用/禁用 16 声道端点的结果。操作只影响这个 AudioEndpoint，不动整条驱动。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointToggleOutcome {
    Changed,
    AlreadySet,
    NotFound,
    NeedsReboot,
    UserDeclinedElevation,
    Failed(String),
}

/// 下载结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// 下好了，字节数也对得上。
    Saved { path: PathBuf, bytes: u64 },
    /// 下下来了但大小和预期不一致（官网换包 / 被中间设备改写）。
    /// 这时候别装，让用户自己去官网。
    SizeMismatch {
        expected: u64,
        actual: u64,
        hint: String,
    },
    /// 链接失效（404 之类）。
    Unavailable { status: u32, hint: String },
    /// 网络或落盘失败。
    Failed(String),
}

/// 安装结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// 装完端点立刻就出现了，不用重启。
    Succeeded,
    /// 装完了，但端点要重启才出现。
    NeedsReboot,
    /// 用户在 UAC 弹窗上点了“否”。
    UserDeclinedElevation,
    /// 其他失败，附中文原因。
    Failed(String),
}

/// “我已经把 VB-CABLE 的名字和官网链接展示给用户了”的凭证。
///
/// 字段是私有的，外部只能通过 `ProductDisclosure::shown()` 拿到——那个方法的文档
/// 就是授权义务本身。这样以后有人想在后台偷偷装，就得先动这行代码，而不是无声无息
/// 地绕过去。
#[derive(Debug, Clone, Copy)]
pub struct ProductDisclosure {
    _private: (),
}

impl ProductDisclosure {
    /// 调用前提：界面上**已经**向用户显示了 `PRODUCT_NAME`，并给出了
    /// `PRODUCT_URL` / `DONATION_URL` 可点击的入口，用户在知情的情况下同意安装。
    ///
    /// 只有满足上面这条才允许调用本方法。不满足就不要装。
    pub fn shown() -> Self {
        Self { _private: () }
    }
}

/// 查当前状态。
///
/// 先看端点在不在（这是唯一“真能用”的判据），端点没有再看服务键，
/// 用来把“装了没重启”和“压根没装”分开。
pub fn detect() -> CableStatus {
    let endpoints = endpoints_present();
    let root_device = driver_root_device_present();
    if endpoints && root_device {
        return CableStatus::Installed;
    }
    if root_device {
        return CableStatus::InstalledPendingReboot;
    }
    if endpoints || cable_endpoint_records_remain() {
        return CableStatus::UninstallIncomplete;
    }
    CableStatus::NotInstalled
}

/// 枚举一个设备安装类里的设备，逐个读 `property` 这个 UTF-16 属性并交给 `visit` 判断。
///
/// `all_classes` 为假只看现役设备（`DIGCF_PRESENT`），为真连被 SetupAPI 保留、
/// 已不在现役的幽灵节点一起看（`DIGCF_ALLCLASSES`）。`visit` 返回 `Some(v)` 就
/// 立刻结束枚举。`N` 是属性缓冲的字节数，调用方按属性长度给。
///
/// 类枚举失败、单项读属性失败都按“不是要找的”跳过，绝不 panic。
fn device_property_any<T, const N: usize>(
    class: &windows::core::GUID,
    all_classes: bool,
    property: windows::Win32::Devices::DeviceAndDriverInstallation::SETUP_DI_REGISTRY_PROPERTY,
    mut visit: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
        DIGCF_ALLCLASSES, DIGCF_PRESENT, SP_DEVINFO_DATA,
    };

    let flags = if all_classes {
        DIGCF_ALLCLASSES
    } else {
        DIGCF_PRESENT
    };
    // SAFETY: class 由调用方给，是合法类 GUID；空 enumerator 表示枚举该类全部实例。
    let Ok(set) = (unsafe { SetupDiGetClassDevsW(Some(class), PCWSTR::null(), None, flags) })
    else {
        return None;
    };
    let _guard = DeviceSetGuard(set);

    let mut index = 0;
    loop {
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: set 有效；info 带正确 cbSize。失败即枚举结束。
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut info) }.is_err() {
            return None;
        }
        index += 1;
        let mut bytes = [0u8; N];
        let mut required = 0;
        // SAFETY: set/info 来自本次枚举；bytes 是有效可写缓冲。
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                &info,
                property,
                None,
                Some(&mut bytes),
                Some(&mut required),
            )
        }
        .is_err()
        {
            continue;
        }
        let len = (required as usize).min(bytes.len()) / 2;
        // 不截断结尾的 NUL：HARDWAREID 是 REG_MULTI_SZ，多个 ID 之间也用 NUL 隔开，
        // 只看第一个字符串会漏掉后面的 ID。
        let words: Vec<u16> = bytes[..len * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        if let Some(found) = visit(&String::from_utf16_lossy(&words)) {
            return Some(found);
        }
    }
}

/// 残留端点也算记录：除 present 设备外，还要看到被 SetupAPI 保留、但已不在
/// 现役的幽灵节点。根设备已删、卸载却被 PnP veto 拦下时，Core Audio 不再
/// 枚举这些端点，但记录还挂在 AudioEndpoint 类里，DIGCF_PRESENT 会漏掉它们。
fn cable_endpoint_records_remain() -> bool {
    use windows::core::GUID;
    use windows::Win32::Devices::DeviceAndDriverInstallation::SPDRP_FRIENDLYNAME;

    // AudioEndpoint 设备安装类：系统声音设置和 Get-PnpDevice -Class AudioEndpoint 用的同一类。
    const AUDIO_ENDPOINT_CLASS: GUID = GUID::from_u128(0xc166523c_fe0c_4a94_a586_f1a80cfbbf3e);
    // DIGCF_ALLCLASSES 会连非 present 的幽灵节点一起枚举，DIGCF_PRESENT 只挑
    // present 的，这正是残留检测把"有残留"误判成"没装"的根因。
    device_property_any::<bool, 1024>(&AUDIO_ENDPOINT_CLASS, true, SPDRP_FRIENDLYNAME, |name| {
        (is_cable_render(name) || is_cable_capture(name)).then_some(true)
    })
    .unwrap_or(false)
}

/// VB-CABLE 的 ROOT\MEDIA 根设备是否仍存在。
///
/// 只看服务键分不清“刚安装待重启”和“刚卸载待重启”：两种状态下服务键都可能还在。
/// 根设备已经被卸载器移除时，SetupAPI 的 present 设备里不会再出现它。
fn driver_root_device_present() -> bool {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        GUID_DEVCLASS_MEDIA, SPDRP_HARDWAREID,
    };

    device_property_any::<bool, 2048>(
        &GUID_DEVCLASS_MEDIA,
        false,
        SPDRP_HARDWAREID,
        |hardware_ids| {
            hardware_ids
                .to_ascii_lowercase()
                .contains("vbaudiovacwdm")
                .then_some(true)
        },
    )
    .unwrap_or(false)
}

/// 端点是否已经出现。COM 由本函数自己初始化，不依赖调用方。
fn endpoints_present() -> bool {
    // COM 初始化失败就当没装，不 panic。
    let Ok(_com) = ComGuard::mta() else {
        return false;
    };
    let render = devices::list_devices(devices::RENDER).unwrap_or_default();
    let capture = devices::list_devices(devices::CAPTURE).unwrap_or_default();
    let has_render = render.iter().any(|d| is_cable_render(&d.name));
    let has_capture = capture.iter().any(|d| is_cable_capture(&d.name));
    // 两头都要有。只有一头说明装坏了，当没装处理更安全。
    has_render && has_capture
}

/// VB-Audio 家的别的产品。这些不是 VB-CABLE，不能拿来当它用。
///
/// Voicemeeter 的端点叫「VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)」，
/// 名字里同时有 `vb-audio` 和 `input`。不排掉它，只装了 Voicemeeter 的机器会被
/// 判成装了 VB-CABLE，然后播放端把翻译语音送进 Voicemeeter 的虚拟输入。
fn is_other_vb_product(lowercased: &str) -> bool {
    lowercased.contains("voicemeeter") || lowercased.contains("matrix")
}

/// 输出端点名匹配（大小写无关，容忍官方偶尔改后缀）。
///
/// VB-CABLE 老驱动把播放端点叫 `CABLE Input`，新版驱动改叫 `CABLE In 16 Ch`
/// （带通道数），偶尔还注册第二根播放 pin，名字里只有 `VB-Audio Virtual Cable`。
/// 三种都要认，否则 `detect()` 会把装好的驱动误判成"没装"，界面会一直让人
/// 去重新下载安装。
///
/// 判方向靠排除：CABLE 系端点里，录音端（capture）名字都带 `output`/`out`；
/// 只要不含方向词、又确实属于 CABLE 系，就当是播放端。
pub fn is_cable_render(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if is_other_vb_product(&n) {
        return false;
    }
    if !is_cable_family(&n) {
        return false;
    }
    if n.contains("output") || has_standalone_word(&n, "out") {
        return false;
    }
    n.contains("input")
        || has_standalone_word(&n, "in")
        || n.contains("vb-audio")
        || n.contains("virtual cable")
}

/// 输入端点名匹配。老版新版都叫 `CABLE Output`，没变过。
pub(crate) fn is_cable_capture(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if is_other_vb_product(&n) {
        return false;
    }
    n.contains("cable output") || (n.contains("vb-audio") && n.contains("output"))
}

/// 新版额外暴露的多声道播放端点。Vox 只传人声，不需要它。
pub fn is_cable_multichannel_render(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    is_cable_render(name)
        && (n.contains("16 ch") || n.contains("16ch") || n.contains("16 channels"))
}

/// 查询 16 声道端点是否还对系统可见。
pub fn multichannel_endpoint_status() -> MultichannelEndpointStatus {
    use windows::Win32::Media::Audio::{DEVICE_STATE, DEVICE_STATEMASK_ALL, DEVICE_STATE_ACTIVE};

    let Ok(_com) = ComGuard::mta() else {
        return MultichannelEndpointStatus::NotPresent;
    };
    let Ok(enumerator) = devices::enumerator() else {
        return MultichannelEndpointStatus::NotPresent;
    };
    // SAFETY: enumerator 有效；集合和接口由 windows-rs 管引用计数。
    let Ok(collection) = (unsafe {
        enumerator.EnumAudioEndpoints(devices::RENDER, DEVICE_STATE(DEVICE_STATEMASK_ALL))
    }) else {
        return MultichannelEndpointStatus::NotPresent;
    };
    // SAFETY: collection 有效。
    let Ok(count) = (unsafe { collection.GetCount() }) else {
        return MultichannelEndpointStatus::NotPresent;
    };
    for index in 0..count {
        // SAFETY: index 小于刚取到的 count。
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        let Ok(name) = devices::friendly_name(&device) else {
            continue;
        };
        if !is_cable_multichannel_render(&name) {
            continue;
        }
        let pnp_disabled = devices::device_id(&device)
            .ok()
            .and_then(|id| pnp_endpoint_is_disabled(&format!("SWD\\MMDEVAPI\\{id}")));
        if pnp_disabled == Some(true) {
            return MultichannelEndpointStatus::Disabled;
        }
        // SAFETY: device 是有效 IMMDevice。
        return match unsafe { device.GetState() } {
            Ok(state) if state == DEVICE_STATE_ACTIVE => MultichannelEndpointStatus::Enabled,
            Ok(_) => MultichannelEndpointStatus::Disabled,
            Err(_) => MultichannelEndpointStatus::NotPresent,
        };
    }
    MultichannelEndpointStatus::NotPresent
}

/// Core Audio 在禁用 PnP 端点后仍可能把旧 MMDevice 标成 ACTIVE；Discord 等应用
/// 已经看不到它，但单看 IMMDevice 会误报“仍可见”。所以最终状态以 ConfigMgr 为准。
fn pnp_endpoint_is_disabled(instance_id: &str) -> Option<bool> {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Locate_DevNodeW, CM_DEVNODE_STATUS_FLAGS,
        CM_LOCATE_DEVNODE_NORMAL, CM_PROB, CM_PROB_DISABLED, CR_SUCCESS,
    };

    let id = to_wide(instance_id);
    let mut devinst = 0u32;
    // SAFETY: id 是 NUL 结尾宽字符串，devinst 是有效出参。
    if unsafe { CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return None;
    }
    let mut flags = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    // SAFETY: 两个状态变量都是有效出参，devinst 来自上一步。
    if unsafe { CM_Get_DevNode_Status(&mut flags, &mut problem, devinst, 0) } != CR_SUCCESS {
        return None;
    }
    Some(problem == CM_PROB_DISABLED)
}

/// 找到 16 声道端点的 PnP 实例 ID。禁用后 IMMDevice 仍能以 ALL 状态枚举到，
/// 因此同一个函数也能用于恢复。
fn multichannel_pnp_instance_id() -> Option<String> {
    use windows::Win32::Media::Audio::{DEVICE_STATE, DEVICE_STATEMASK_ALL};

    let _com = ComGuard::mta().ok()?;
    let enumerator = devices::enumerator().ok()?;
    // SAFETY: enumerator 有效。
    let collection = unsafe {
        enumerator.EnumAudioEndpoints(devices::RENDER, DEVICE_STATE(DEVICE_STATEMASK_ALL))
    }
    .ok()?;
    // SAFETY: collection 有效。
    let count = unsafe { collection.GetCount() }.ok()?;
    for index in 0..count {
        // SAFETY: index 小于 count。
        let device = unsafe { collection.Item(index) }.ok()?;
        let name = devices::friendly_name(&device).ok()?;
        if is_cable_multichannel_render(&name) {
            let endpoint_id = devices::device_id(&device).ok()?;
            return Some(format!("SWD\\MMDEVAPI\\{endpoint_id}"));
        }
    }
    None
}

/// 在系统层隐藏或恢复 16 声道端点。PnPUtil 是 Windows 自带工具，禁用设备必须提权。
pub fn set_multichannel_endpoint_enabled(enabled: bool) -> EndpointToggleOutcome {
    let current = multichannel_endpoint_status();
    if matches!(
        (enabled, current),
        (true, MultichannelEndpointStatus::Enabled) | (false, MultichannelEndpointStatus::Disabled)
    ) {
        return EndpointToggleOutcome::AlreadySet;
    }
    let Some(instance_id) = multichannel_pnp_instance_id() else {
        return EndpointToggleOutcome::NotFound;
    };

    let tool = system32_tool("pnputil.exe");
    let operation = if enabled {
        "/enable-device"
    } else {
        "/disable-device"
    };
    let args = format!("{operation} \"{instance_id}\"");
    let cwd = tool.parent().unwrap_or_else(|| Path::new("."));
    let code = match run_elevated(
        &tool,
        &args,
        cwd,
        60_000,
        &ElevateMessages {
            start: "启动系统设备管理工具失败",
            no_handle: "系统设备管理工具没有返回进程句柄",
            timeout: "等待系统隐藏音频端点超时",
            exit_code: "读取设备管理工具结果失败",
        },
    ) {
        Ok(ElevatedRun::Exited(code)) => code,
        Ok(ElevatedRun::Declined) => return EndpointToggleOutcome::UserDeclinedElevation,
        Err(message) => return EndpointToggleOutcome::Failed(message),
    };
    if matches!(code, 3010 | 1641) {
        return EndpointToggleOutcome::NeedsReboot;
    }
    if code != 0 {
        return EndpointToggleOutcome::Failed(format!("系统设备管理工具返回错误码 {code}"));
    }

    std::thread::sleep(Duration::from_millis(700));
    match (enabled, multichannel_endpoint_status()) {
        (true, MultichannelEndpointStatus::Enabled)
        | (false, MultichannelEndpointStatus::Disabled) => EndpointToggleOutcome::Changed,
        _ => EndpointToggleOutcome::NeedsReboot,
    }
}

/// 是不是 CABLE 这条线（而不是 Voicemeeter 之类别的 VB 产品）。
///
/// 只看"名字里带 cable / virtual cable / vb-audio"这些标记，具体是输入还是
/// 输出端由调用方再判。
fn is_cable_family(n: &str) -> bool {
    n.contains("cable") || n.contains("vb-cable") || n.contains("virtual cable")
}

/// `word` 在名字里是不是作为隔开的独立单词出现。
///
/// 新版端点名 `cable in 16 ch` 里的 `in` 是单独的单词，和 `cable input` 的
/// `input` 不是一回事。用词边界匹配，避免把 `line in`、`spanish in` 这类
/// 名字里恰好带 `in` 子串的真设备误认成 CABLE。
fn has_standalone_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == word)
}

/// 建议的下载落盘目录：`%LOCALAPPDATA%\Vox\vbcable`，取不到就用临时目录。
pub fn default_download_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Vox").join("vbcable")
}

/// WinHTTP 句柄的所有者。任何一步失败都要把已经开的句柄按序关掉，
/// 用 Drop 兜住比手写 goto 靠谱。
struct HttpHandle(*mut std::ffi::c_void);

impl HttpHandle {
    fn new(raw: *mut std::ffi::c_void) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self(raw))
        }
    }

    fn raw(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由本结构体独占，只在这里关一次。
        unsafe {
            let _ = windows::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0);
        }
    }
}

/// 取最近一次 Win32 错误，包成中文说明。
fn last_error(context: &str) -> String {
    // SAFETY: GetLastError 只读当前线程的错误码，没有任何前置条件。
    let code = unsafe { windows::Win32::Foundation::GetLastError() };
    hr_err(context, windows::core::HRESULT::from_win32(code.0)).message
}

/// 下载官方驱动包到 `dir`。
///
/// - 需要 `ProductDisclosure`：界面必须先把 VB-CABLE 的名字和官网/捐赠链接摆出来。
/// - `on_progress(已下载字节, 总字节)`，总字节拿不到时给 0。
/// - 大小和 `DOWNLOAD_EXPECTED_BYTES` 不一致时**不删文件**但返回 `SizeMismatch`，
///   让上层引导用户去官网手动装（我们不猜官方换了什么包）。
pub fn download(
    _disclosure: ProductDisclosure,
    dir: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> DownloadOutcome {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinHttp::{
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
        WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    if let Err(e) = std::fs::create_dir_all(dir) {
        return DownloadOutcome::Failed(format!("创建下载目录失败：{e}"));
    }
    let dest = dir.join(ARCHIVE_FILE_NAME);

    let agent = to_wide("Vox");
    // SAFETY: agent 在调用期间有效；返回的句柄立刻交给 HttpHandle 托管。
    let session = match HttpHandle::new(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    }) {
        Some(h) => h,
        None => return DownloadOutcome::Failed(last_error("初始化 WinHTTP 失败")),
    };

    // 解析、连接、发送、接收各给 30 秒，别让界面上的进度条永远转。
    // SAFETY: session 有效。
    unsafe {
        let _ = WinHttpSetTimeouts(session.raw(), 30_000, 30_000, 30_000, 30_000);
    }

    // WinHttpConnect 要主机和路径分开传，所以这里没法直接用 DOWNLOAD_URL。
    // 两处必须一起改：DOWNLOAD_URL 只是给界面显示和报错用的。
    let host = to_wide(DOWNLOAD_HOST);
    // SAFETY: host 在调用期间有效。
    let connect = match HttpHandle::new(unsafe {
        WinHttpConnect(session.raw(), PCWSTR(host.as_ptr()), 443, 0)
    }) {
        Some(h) => h,
        None => return DownloadOutcome::Failed(last_error("连接下载服务器失败")),
    };

    let verb = to_wide("GET");
    let path = to_wide(DOWNLOAD_PATH);
    // SAFETY: verb / path 在调用期间有效；accept types 传空表示默认。
    let request = match HttpHandle::new(unsafe {
        WinHttpOpenRequest(
            connect.raw(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    }) {
        Some(h) => h,
        None => return DownloadOutcome::Failed(last_error("创建下载请求失败")),
    };

    // SAFETY: request 有效；无附加头、无请求体。
    if let Err(e) = unsafe { WinHttpSendRequest(request.raw(), None, None, 0, 0, 0) } {
        return DownloadOutcome::Failed(hr_err("发送下载请求失败", e.code()).message);
    }
    // SAFETY: request 有效。
    if let Err(e) = unsafe { WinHttpReceiveResponse(request.raw(), std::ptr::null_mut()) } {
        return DownloadOutcome::Failed(hr_err("等待下载响应失败", e.code()).message);
    }

    let mut status: u32 = 0;
    let mut len: u32 = std::mem::size_of::<u32>() as u32;
    let mut index: u32 = 0;
    // SAFETY: 用 FLAG_NUMBER 时缓冲必须正好是一个 u32，这里给的就是本地 u32 的地址。
    let got_status = unsafe {
        WinHttpQueryHeaders(
            request.raw(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut len,
            &mut index,
        )
    }
    .is_ok();
    if !got_status {
        return DownloadOutcome::Failed(last_error("读取下载响应状态失败"));
    }
    if status != 200 {
        return DownloadOutcome::Unavailable {
            status,
            hint: format!(
                "官方下载链接返回 HTTP {status}，可能是官网换了版本。请直接打开 {PRODUCT_URL} 手动下载安装 {PRODUCT_NAME}。"
            ),
        };
    }

    let mut total: u32 = 0;
    let mut tlen: u32 = std::mem::size_of::<u32>() as u32;
    let mut tindex: u32 = 0;
    // SAFETY: 同上，FLAG_NUMBER 配一个 u32 缓冲。拿不到 Content-Length 不算错。
    let total_bytes = if unsafe {
        WinHttpQueryHeaders(
            request.raw(),
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut total as *mut u32).cast()),
            &mut tlen,
            &mut tindex,
        )
    }
    .is_ok()
    {
        u64::from(total)
    } else {
        0
    };

    let mut body: Vec<u8> = Vec::with_capacity(DOWNLOAD_EXPECTED_BYTES as usize);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut read: u32 = 0;
        // SAFETY: chunk 是本地缓冲，长度就是传进去的那个数；read 是本地变量。
        if let Err(e) = unsafe {
            WinHttpReadData(
                request.raw(),
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
        } {
            return DownloadOutcome::Failed(hr_err("下载中断", e.code()).message);
        }
        if read == 0 {
            break;
        }
        // 夹一下：切片越界会 panic，而这个长度是 WinHTTP 写回来的出参。
        // 正常它不会超过我们给的缓冲，但不值得拿一个 panic 去赌。
        let read = (read as usize).min(chunk.len());
        body.extend_from_slice(&chunk[..read]);
        on_progress(body.len() as u64, total_bytes);

        // 防炸：真包才 1.3 MB，超过 32 MB 说明抓到的不是我们要的东西。
        if body.len() > 32 * 1024 * 1024 {
            return DownloadOutcome::SizeMismatch {
                expected: DOWNLOAD_EXPECTED_BYTES,
                actual: body.len() as u64,
                hint: format!(
                    "下载内容异常偏大，已中止。请直接打开 {PRODUCT_URL} 手动下载安装 {PRODUCT_NAME}。"
                ),
            };
        }
    }

    let actual = body.len() as u64;
    if actual != DOWNLOAD_EXPECTED_BYTES {
        return DownloadOutcome::SizeMismatch {
            expected: DOWNLOAD_EXPECTED_BYTES,
            actual,
            hint: size_mismatch_hint(DOWNLOAD_EXPECTED_BYTES, actual),
        };
    }

    if let Err(e) = std::fs::write(&dest, &body) {
        return DownloadOutcome::Failed(format!("保存驱动包失败：{e}"));
    }
    DownloadOutcome::Saved {
        path: dest,
        bytes: actual,
    }
}

/// 大小不符时给用户的提示。抽出来是为了能在不联网的情况下测文案。
pub(crate) fn size_mismatch_hint(expected: u64, actual: u64) -> String {
    format!(
        "下载到 {actual} 字节，和预期的 {expected} 字节不一致，为安全起见没有安装。\
         请直接打开 {PRODUCT_URL} 手动下载安装 {PRODUCT_NAME}（也欢迎在 {DONATION_URL} 支持作者）。"
    )
}

/// 不弹黑框跑子进程。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 安装器退出码 → 结果。纯函数，方便离线测。
///
/// 3010 / 1641 是 MSI 约定的“要重启”；1223 是 UAC 被拒；
/// VB-CABLE 的静默安装器成功时给 0，但驱动端点一般也要重启才出现，
/// 所以 0 的最终判断交给调用方再探一次设备。
pub(crate) fn map_install_exit(code: u32) -> InstallOutcome {
    match code {
        0 => InstallOutcome::Succeeded,
        3010 | 1641 => InstallOutcome::NeedsReboot,
        1223 => InstallOutcome::UserDeclinedElevation,
        other => InstallOutcome::Failed(format!(
            "{PRODUCT_NAME} 安装器返回错误码 {other}。可以打开 {PRODUCT_URL} 手动安装。"
        )),
    }
}

/// 拼出 System32 下某个系统工具的绝对路径。
///
/// 不能只写 `tar.exe` 让它去 PATH 里找：PATH 是进程环境里的东西，用户或者
/// 别的软件往前面塞一个同名 exe，我们就会去跑那个。取不到 `%SystemRoot%`
/// 才退回裸名字。
fn system32_tool(relative: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let full = Path::new(&root).join("System32").join(relative);
    if full.is_file() {
        return full;
    }
    // 实在找不到就交给系统去解析，总比直接失败好。
    PathBuf::from(relative)
}

/// 解压驱动包。
///
/// 先用系统自带的 `tar.exe`（Win10 1803+ 都有，比 PowerShell 快很多），
/// 不行再退回 `Expand-Archive`。
fn extract_archive(archive: &Path, into: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::create_dir_all(into) {
        return Err(format!("创建解压目录失败：{e}"));
    }

    let tar = Command::new(system32_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    if matches!(&tar, Ok(s) if s.success()) {
        return Ok(());
    }

    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        archive.display(),
        into.display()
    );
    let ps = Command::new(system32_tool("WindowsPowerShell\\v1.0\\powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    match ps {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "解压驱动包失败（tar 和 Expand-Archive 都不行，PowerShell 退出码 {:?}）",
            s.code()
        )),
        Err(e) => Err(format!("解压驱动包失败：{e}")),
    }
}

/// 在解压目录里找 64 位安装器（官方包有时多套一层目录）。
pub(crate) fn find_installer(root: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                subdirs.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(INSTALLER_EXE))
            {
                return Some(path);
            }
        }
        subdirs.into_iter().find_map(|d| walk(&d, depth + 1))
    }
    walk(root, 0)
}

/// 安装前的入场检查：包在不在、大小对不对。
///
/// 单独拆出来是为了让测试只验这一段，不用真进 `install`。`install` 里那些
/// 解压和提权都在这个检查之后，但"测试安全"不该靠这个先后顺序撑着——
/// 有人哪天把解压挪到前面，测试就会开始跑安装程序了。
///
/// `Ok(())` 表示可以往下走；`Err(outcome)` 就是要直接返回给调用方的失败。
fn preflight(archive: &Path) -> Result<(), InstallOutcome> {
    if !archive.is_file() {
        return Err(InstallOutcome::Failed(format!(
            "找不到驱动包：{}",
            archive.display()
        )));
    }
    // 再校一遍大小：download 和 install 之间可能隔了很久，或者文件是别处塞进来的。
    match std::fs::metadata(archive) {
        Ok(m) if m.len() == DOWNLOAD_EXPECTED_BYTES => Ok(()),
        Ok(m) => Err(InstallOutcome::Failed(size_mismatch_hint(
            DOWNLOAD_EXPECTED_BYTES,
            m.len(),
        ))),
        Err(e) => Err(InstallOutcome::Failed(format!("读取驱动包信息失败：{e}"))),
    }
}

/// 用管理员权限静默安装。
///
/// - 需要 `ProductDisclosure`（授权义务，见模块头注释）。
/// - `archive` 是 `download` 返回的那个 zip。
/// - 会弹一次 UAC：驱动安装绕不过去。用户点“否”返回 `UserDeclinedElevation`，
///   上层可以原地重试。
/// - 装完先探一次设备：端点已经出现就 `Succeeded`，还没出现就 `NeedsReboot`。
pub fn install(_disclosure: ProductDisclosure, archive: &Path) -> InstallOutcome {
    run_setup(archive, SetupAction::Install)
}

/// 用管理员权限静默卸载。
///
/// VB-CABLE 的安装和卸载共用同一个官方安装器。卸载完成后端点可能要等重启才消失，
/// 这种情况返回 `NeedsReboot`，不能把仍在使用中的端点误报成卸载失败。
pub fn uninstall(_disclosure: ProductDisclosure, archive: &Path) -> InstallOutcome {
    run_setup(archive, SetupAction::Uninstall)
}

/// 加强卸载：临时停止 Windows 音频服务，释放 AudioEndpointBuilder 对驱动的持有，
/// 再运行官方卸载器，最后恢复系统音频。只在普通卸载被 PnP veto 后由用户确认调用。
pub fn uninstall_with_audio_reset(
    _disclosure: ProductDisclosure,
    archive: &Path,
) -> InstallOutcome {
    if let Err(failed) = preflight(archive) {
        return failed;
    }
    let work = archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("extracted");
    if let Err(error) = extract_archive(archive, &work) {
        return InstallOutcome::Failed(error);
    }
    let Some(installer) = find_installer(&work) else {
        return InstallOutcome::Failed(format!("驱动包里没找到 {INSTALLER_EXE}"));
    };

    let escaped = installer.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Continue'; \
         $audio=(Get-Service Audiosrv -ErrorAction SilentlyContinue).Status -eq 'Running'; \
         $builder=(Get-Service AudioEndpointBuilder -ErrorAction SilentlyContinue).Status -eq 'Running'; \
         $code=1; try {{ \
           Stop-Service Audiosrv -Force -ErrorAction SilentlyContinue; \
           Stop-Service AudioEndpointBuilder -Force -ErrorAction SilentlyContinue; \
           & '{escaped}' -u -h; $code=$LASTEXITCODE; \
           & sc.exe stop VBAudioVACMME | Out-Null; \
           Start-Sleep -Milliseconds 800; \
         }} finally {{ \
           if($builder){{Start-Service AudioEndpointBuilder -ErrorAction SilentlyContinue}}; \
           if($audio){{Start-Service Audiosrv -ErrorAction SilentlyContinue}} \
         }}; exit $code"
    );

    let powershell = system32_tool("WindowsPowerShell\\v1.0\\powershell.exe");
    let args = format!(
        "-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"{}\"",
        script.replace('"', "\\\"")
    );
    let code = match run_elevated(
        &powershell,
        &args,
        &work,
        120_000,
        &ElevateMessages {
            start: "启动加强卸载流程失败",
            no_handle: "加强卸载流程没有返回进程句柄",
            timeout: "加强卸载等待超时",
            exit_code: "读取加强卸载结果失败",
        },
    ) {
        Ok(ElevatedRun::Exited(code)) => code,
        Ok(ElevatedRun::Declined) => return InstallOutcome::UserDeclinedElevation,
        Err(message) => return InstallOutcome::Failed(message),
    };
    if code != 0 {
        return map_install_exit(code);
    }
    std::thread::sleep(Duration::from_millis(1200));
    match detect() {
        CableStatus::NotInstalled => InstallOutcome::Succeeded,
        _ => InstallOutcome::Failed("Windows 音频服务重启后驱动仍在，卸载没有完成。".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupAction {
    Install,
    Uninstall,
}

fn run_setup(archive: &Path, action: SetupAction) -> InstallOutcome {
    if let Err(failed) = preflight(archive) {
        return failed;
    }

    let work = archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("extracted");
    if let Err(e) = extract_archive(archive, &work) {
        return InstallOutcome::Failed(e);
    }
    let Some(installer) = find_installer(&work) else {
        return InstallOutcome::Failed(format!(
            "驱动包里没找到 {INSTALLER_EXE}，可能官方换了包结构。请打开 {PRODUCT_URL} 手动安装。"
        ));
    };

    let args = match action {
        SetupAction::Install => INSTALL_ARGS,
        SetupAction::Uninstall => UNINSTALL_ARGS,
    };
    let code = match run_elevated(
        &installer,
        args,
        &work,
        5 * 60 * 1000,
        &ElevateMessages {
            start: "启动安装器失败",
            no_handle: "安装器没有返回进程句柄，无法确认安装结果",
            timeout: "等待安装器结束超时（5 分钟）。请检查是否有安装向导窗口在等你操作。",
            exit_code: "读取安装器退出码失败",
        },
    ) {
        Ok(ElevatedRun::Exited(code)) => code,
        Ok(ElevatedRun::Declined) => return InstallOutcome::UserDeclinedElevation,
        Err(message) => return InstallOutcome::Failed(message),
    };

    match map_install_exit(code) {
        // 退出码说成功，还要看设备实际状态；安装和卸载都可能要重启才完成。
        InstallOutcome::Succeeded => {
            std::thread::sleep(Duration::from_millis(1500));
            match (action, detect()) {
                (SetupAction::Install, CableStatus::Installed)
                | (SetupAction::Uninstall, CableStatus::NotInstalled) => InstallOutcome::Succeeded,
                _ => InstallOutcome::NeedsReboot,
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 本模块的测试一律离线，而且不碰系统：
    // 不调 download（唯一联网的函数），不调 install（唯一会解压和提权的函数）。
    // 要验入场检查就调 preflight，它自己不做任何副作用。

    #[test]
    fn detect_returns_a_status_without_panicking() {
        let status = detect();
        assert!(matches!(
            status,
            CableStatus::Installed
                | CableStatus::InstalledPendingReboot
                | CableStatus::UninstallIncomplete
                | CableStatus::NotInstalled
        ));
    }

    #[test]
    #[ignore = "会弹 UAC 并禁用当前机器的 CABLE In 16 Ch；只在人工验收时运行"]
    fn multichannel_endpoint_can_be_hidden_on_real_machine() {
        assert_ne!(
            multichannel_endpoint_status(),
            MultichannelEndpointStatus::NotPresent,
            "当前机器没有 16 声道端点"
        );
        let outcome = set_multichannel_endpoint_enabled(false);
        assert!(
            matches!(
                outcome,
                EndpointToggleOutcome::Changed | EndpointToggleOutcome::AlreadySet
            ),
            "隐藏结果：{outcome:?}"
        );
        assert_eq!(
            multichannel_endpoint_status(),
            MultichannelEndpointStatus::Disabled
        );
    }

    #[test]
    #[ignore = "会弹 UAC、短暂停止系统音频并真实卸载 VB-CABLE；只在人工验收时运行"]
    fn incomplete_uninstall_can_be_finished_on_real_machine() {
        if matches!(detect(), CableStatus::NotInstalled) {
            return;
        }
        let archive = default_download_dir().join(ARCHIVE_FILE_NAME);
        let outcome = uninstall_with_audio_reset(ProductDisclosure::shown(), &archive);
        assert_eq!(
            outcome,
            InstallOutcome::Succeeded,
            "加强卸载结果：{outcome:?}"
        );
        assert_eq!(detect(), CableStatus::NotInstalled);
    }

    #[test]
    fn endpoint_names_match_their_matchers() {
        assert!(is_cable_render(RENDER_ENDPOINT_NAME));
        assert!(is_cable_capture(CAPTURE_ENDPOINT_NAME));
        assert!(!is_cable_render(CAPTURE_ENDPOINT_NAME));
        assert!(!is_cable_capture(RENDER_ENDPOINT_NAME));
        assert!(!is_cable_render("Speakers (Realtek(R) Audio)"));
        assert!(!is_cable_capture("Microphone Array"));
    }

    #[test]
    fn voicemeeter_is_not_mistaken_for_vb_cable() {
        // VB-Audio 自己家的别的产品。名字里有 vb-audio + input/output，
        // 但它们不是 VB-CABLE，认错了会把语音送错设备。
        for name in [
            "VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)",
            "VoiceMeeter Aux Input (VB-Audio VoiceMeeter AUX VAIO)",
            "VoiceMeeter VAIO3 Input (VB-Audio VoiceMeeter VAIO3)",
        ] {
            assert!(!is_cable_render(name), "{name} 被当成了 CABLE Input");
        }
        for name in [
            "VoiceMeeter Output (VB-Audio VoiceMeeter VAIO)",
            "VoiceMeeter Aux Output (VB-Audio VoiceMeeter AUX VAIO)",
        ] {
            assert!(!is_cable_capture(name), "{name} 被当成了 CABLE Output");
        }
        // 真的 VB-CABLE 不能被这个排除规则误伤。
        assert!(is_cable_render(RENDER_ENDPOINT_NAME));
        assert!(is_cable_capture(CAPTURE_ENDPOINT_NAME));
    }

    #[test]
    fn matchers_are_case_insensitive() {
        assert!(is_cable_render("CABLE INPUT (VB-AUDIO VIRTUAL CABLE)"));
        assert!(is_cable_capture("cable output (vb-audio virtual cable)"));
    }

    #[test]
    fn matchers_recognize_new_driver_channel_names() {
        // VB-CABLE 新版驱动把播放端点改叫 `CABLE In 16 Ch`（带通道数），
        // 老匹配串 `CABLE Input` 会配不上，detect 就会误判成"没装"。
        // 这三组是真实机器上这套驱动注册出来的名字。
        assert!(is_cable_render("CABLE In 16 Ch (VB-Audio Virtual Cable)"));
        // 同一条线的第二根播放 pin 有时就叫"扬声器 (VB-Audio Virtual Cable)"。
        assert!(is_cable_render("扬声器 (VB-Audio Virtual Cable)"));
        assert!(is_cable_capture("CABLE Output (VB-Audio Virtual Cable)"));

        // 反向不串。（"CABLE In 16 Ch" 是 render，不该被当成 capture。）
        assert!(!is_cable_capture("CABLE In 16 Ch (VB-Audio Virtual Cable)"));
        assert!(!is_cable_render("CABLE Output (VB-Audio Virtual Cable)"));
    }

    #[test]
    fn render_matcher_does_not_swallow_arbitrary_in_words() {
        // 放宽到能认 "In 16 Ch" 之后，不能把名字里恰好带独立单词 `in` 的
        // 真设备也误认成 CABLE。这些都不属于 CABLE 系，必须保持不匹配。
        assert!(!is_cable_render("Line In (Realtek(R) Audio)"));
        assert!(!is_cable_render("线路输入 (Realtek(R) Audio)"));
        assert!(!is_cable_render("扬声器 (Realtek(R) Audio)"));
        assert!(!is_cable_render("USB Audio 1 in 1 out"));
    }

    #[test]
    fn install_exit_codes_map_to_outcomes() {
        assert_eq!(map_install_exit(0), InstallOutcome::Succeeded);
        assert_eq!(map_install_exit(3010), InstallOutcome::NeedsReboot);
        assert_eq!(map_install_exit(1641), InstallOutcome::NeedsReboot);
        assert_eq!(
            map_install_exit(1223),
            InstallOutcome::UserDeclinedElevation
        );
        let InstallOutcome::Failed(msg) = map_install_exit(1603) else {
            panic!("1603 应该是失败");
        };
        assert!(msg.contains("1603"), "{msg}");
        assert!(msg.contains(PRODUCT_URL), "{msg}");
    }

    #[test]
    fn size_mismatch_hint_names_product_and_links() {
        let hint = size_mismatch_hint(DOWNLOAD_EXPECTED_BYTES, 123);
        assert!(hint.contains(PRODUCT_NAME));
        assert!(hint.contains(PRODUCT_URL));
        assert!(hint.contains(DONATION_URL));
        assert!(hint.contains("123"));
    }

    #[test]
    fn preflight_rejects_missing_archive() {
        let Err(InstallOutcome::Failed(msg)) =
            preflight(Path::new("Z:\\不存在的目录\\不存在的包.zip"))
        else {
            panic!("不存在的包应该失败");
        };
        assert!(msg.contains("找不到驱动包"), "{msg}");
    }

    #[test]
    fn preflight_rejects_wrong_size_archive() {
        let dir = std::env::temp_dir().join("vox_cable_test_size");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake.zip");
        std::fs::write(&fake, b"not the real driver pack").unwrap();
        let Err(InstallOutcome::Failed(msg)) = preflight(&fake) else {
            panic!("大小不对应该失败");
        };
        assert!(msg.contains("不一致"), "{msg}");
        let _ = std::fs::remove_file(&fake);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn installer_lookup_finds_nested_exe() {
        let root = std::env::temp_dir().join("vox_cable_test_walk");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let exe = nested.join(INSTALLER_EXE);
        std::fs::write(&exe, b"stub").unwrap();
        assert_eq!(find_installer(&root).as_deref(), Some(exe.as_path()));
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installer_lookup_returns_none_when_absent() {
        let root = std::env::temp_dir().join("vox_cable_test_empty");
        std::fs::create_dir_all(&root).unwrap();
        assert!(find_installer(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn download_url_matches_the_pinned_file_name() {
        assert!(DOWNLOAD_URL.ends_with(ARCHIVE_FILE_NAME));
        assert!(DOWNLOAD_URL.starts_with("https://"));
    }

    #[test]
    fn download_url_agrees_with_the_host_and_path_actually_requested() {
        // DOWNLOAD_URL 是展示用的，真请求走 HOST + PATH。改一个忘了另一个，
        // 界面上显示的地址就会和实际下载的地址不是同一个东西。
        assert_eq!(
            DOWNLOAD_URL,
            format!("https://{DOWNLOAD_HOST}{DOWNLOAD_PATH}")
        );
        assert!(DOWNLOAD_PATH.starts_with('/'));
        assert!(DOWNLOAD_PATH.ends_with(ARCHIVE_FILE_NAME));
    }

    #[test]
    fn download_dir_is_under_a_writable_root() {
        let dir = default_download_dir();
        assert!(dir.ends_with("vbcable"));
        assert!(dir.is_absolute());
    }
}
