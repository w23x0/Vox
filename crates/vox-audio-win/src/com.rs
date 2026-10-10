//! COM 初始化守卫和 HRESULT 翻译。
//!
//! 规矩：谁开线程谁自己初始化 COM，不假设调用方做过。所以每个碰 COM 的线程
//! 开头都建一个 `ComGuard`，线程退出时自动 `CoUninitialize`。
//!
//! 另一条规矩：COM 错误一律翻译成带 HRESULT 的中文 `PortError`，永远不 panic。
//! 排查音频问题几乎全靠 HRESULT（比如 0x88890004 = 设备正在被独占），
//! 所以数字必须原样带到日志里，不能只留一句“打开设备失败”。

use std::marker::PhantomData;
use std::path::Path;

use vox_core::ports::{PortError, PortResult};
use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT, COINIT_MULTITHREADED};

/// 线程级 COM 守卫。`Drop` 时按需 `CoUninitialize`。
///
/// 故意不实现 `Send`：COM 的初始化是线程私有的，句柄跨线程搬会把
/// 反初始化调到错误的线程上。光靠字段挡不住这件事（里面只有个 `bool`，
/// 编译器会自动给 `Send`），所以塞一个裸指针把自动实现关掉。
pub(crate) struct ComGuard {
    /// 只有真的是我们初始化成功的才需要反初始化；别人先初始化过就别乱动。
    needs_uninit: bool,
    /// 只为取消 `Send`/`Sync` 的自动实现，不存东西。
    _not_send: PhantomData<*const ()>,
}

impl ComGuard {
    /// 按 MTA 初始化当前线程。
    ///
    /// 进程环回的激活回调必须落在 MTA 上，所以采集线程一律用这个。
    pub(crate) fn mta() -> PortResult<Self> {
        Self::init(COINIT_MULTITHREADED)
    }

    fn init(mode: COINIT) -> PortResult<Self> {
        // SAFETY: CoInitializeEx 对任意线程都可调用，第一个参数按文档传 NULL。
        // 返回值全部走 HRESULT 判断，不会 panic。
        let hr = unsafe { CoInitializeEx(None, mode) };
        if hr.is_ok() {
            // S_OK 表示我们是初始化者，S_FALSE 表示同模式已初始化过——
            // 两种情况都要配对调用 CoUninitialize，这是 COM 的引用计数规则。
            Ok(Self {
                needs_uninit: true,
                _not_send: PhantomData,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            // 线程之前被初始化成另一种套间。此时不能反初始化别人的，
            // 也不该硬失败：接口调用照样能用，只是套间语义由先来者决定。
            Ok(Self {
                needs_uninit: false,
                _not_send: PhantomData,
            })
        } else {
            Err(hr_err("failed to initialize COM", hr))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.needs_uninit {
            // SAFETY: 与本线程上成功返回的 CoInitializeEx 一一配对。
            unsafe { CoUninitialize() };
        }
    }
}

/// 把 HRESULT 包成 English 诊断 `PortError`，附带系统给的描述和原始十六进制码。
pub(crate) fn hr_err(context: &str, hr: HRESULT) -> PortError {
    let detail = windows::core::Error::from_hresult(hr).message();
    if detail.is_empty() {
        PortError::new(format!("{context} (HRESULT 0x{:08X})", hr.0 as u32))
    } else {
        PortError::new(format!(
            "{context}: {detail} (HRESULT 0x{:08X})",
            hr.0 as u32
        ))
    }
}

/// `windows::core::Error` 版本，用在 `?` 收不住的地方。
pub(crate) fn win_err(context: &str, err: windows::core::Error) -> PortError {
    hr_err(context, err.code())
}

/// 给 `Result<_, windows::core::Error>` 加上 English 上下文。
pub(crate) trait WinContext<T> {
    fn ctx(self, context: &str) -> PortResult<T>;
}

impl<T> WinContext<T> for windows::core::Result<T> {
    fn ctx(self, context: &str) -> PortResult<T> {
        self.map_err(|e| win_err(context, e))
    }
}

/// 内核对象句柄的所有者，`Drop` 时关掉。
///
/// `windows` 的 `HANDLE` 不是 `Send`，但事件句柄本身跨线程用是安全的
/// （`SetEvent` / `WaitForSingleObject` 都是线程安全的内核调用），
/// 所以这里包一层显式声明，免得到处写 `unsafe impl`。
pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: 内核句柄是进程级资源，跨线程传递和并发调用 SetEvent/Wait 都由内核保证；
// 这里唯一的所有权在本结构体上，Drop 只会执行一次 CloseHandle。
unsafe impl Send for OwnedHandle {}
// SAFETY: 同上，只读地共享一个句柄值，内核侧调用本身线程安全。
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub(crate) fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: 句柄由本结构体独占，只在这里关一次。
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

/// 把宽字符指针读成 `String`，遇到空指针给空串。
///
/// # Safety
/// `ptr` 必须是 NUL 结尾的合法宽字符串，或者空指针。
pub(crate) unsafe fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: 调用方保证 NUL 结尾，逐个探测直到终止符。
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: 上面刚数出长度，区间内全是已初始化的 u16。
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

/// Rust 字符串转 NUL 结尾的宽字符缓冲，交给 Win32 用。
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 提权运行的结局。
pub(crate) enum ElevatedRun {
    /// 子进程已退出，附退出码。
    Exited(u32),
    /// 用户在上 UAC 时点了“否”。
    Declined,
}

/// `run_elevated` 的中文文案。四个字段依次用在：启动失败、拿不到进程句柄、
/// 等待超时、读取退出码失败。文案由调用方给，各条路的问题在日志里能一眼分清。
pub(crate) struct ElevateMessages<'a> {
    pub(crate) start: &'a str,
    pub(crate) no_handle: &'a str,
    pub(crate) timeout: &'a str,
    pub(crate) exit_code: &'a str,
}

/// 以管理员权限（`runas`）静默拉起 `file`，等它退出并取回退出码。
///
/// `args` 是已经拼好的参数串，`cwd` 是工作目录，`timeout_ms` 是等待上限。
/// 用户在 UAC 上点“否”返回 `Ok(ElevatedRun::Declined)`；其余失败包成中文说明。
/// 进程句柄由 `OwnedHandle` 兜住所有返回路径，不会漏关。
pub(crate) fn run_elevated(
    file: &Path,
    args: &str,
    cwd: &Path,
    timeout_ms: u32,
    messages: &ElevateMessages<'_>,
) -> Result<ElevatedRun, String> {
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb = to_wide("runas");
    let file_wide = to_wide(&file.to_string_lossy());
    let args_wide = to_wide(args);
    let cwd_wide = to_wide(&cwd.to_string_lossy());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file_wide.as_ptr()),
        lpParameters: PCWSTR(args_wide.as_ptr()),
        lpDirectory: PCWSTR(cwd_wide.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: 各宽字符串在调用期间有效；NOCLOSEPROCESS 表示 hProcess 归我们关，
    // 下面的 OwnedHandle 兜住所有返回路径。
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        // 用户在 UAC 上点“否”就是这个码，单独分出来好让上层提示“需要管理员权限”。
        if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
            return Ok(ElevatedRun::Declined);
        }
        return Err(hr_err(messages.start, error.code()).message);
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return Err(messages.no_handle.into());
    }
    let _guard = OwnedHandle::new(process);
    // SAFETY: 句柄有效，由上面的守卫持有。
    if unsafe { WaitForSingleObject(process, timeout_ms) } != WAIT_OBJECT_0 {
        return Err(messages.timeout.into());
    }
    let mut code: u32 = 0;
    // SAFETY: 进程已退出，句柄仍由守卫持有。
    if let Err(error) = unsafe { GetExitCodeProcess(process, &mut code) } {
        return Err(hr_err(messages.exit_code, error.code()).message);
    }
    Ok(ElevatedRun::Exited(code))
}
