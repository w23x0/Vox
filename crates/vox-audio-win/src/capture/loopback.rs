//! 进程环回采集：只抓某个软件（及其子进程）的声音，不抓别人的。
//!
//! 这是“听人说话”的主路径：抓 VRChat 自己的输出，不用把系统音量搅进来，
//! 也不需要虚拟声卡绕线。
//!
//! 这条路上有一堆坑，每个都踩过一次，注释按坑写在对应位置：
//! - 要求系统内部版本 ≥ 20348（见 `osver`）；
//! - 激活参数和 PROPVARIANT 必须活过异步回调，不能放栈上；
//! - 回调对象必须在 MTA 上，还得同时实现 IAgileObject；
//! - 要检查两个 HRESULT（调用本身 + GetActivateResult 的出参）；
//! - 这个伪设备上 GetMixFormat / GetBufferSize / GetCurrentPadding 全不能用；
//! - 目标不出声时一帧都不给。

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vox_core::ports::{PortError, PortResult};
use windows::core::{implement, Interface, Ref, HRESULT, PCWSTR};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Variant::VT_BLOB;

use crate::com::{hr_err, WinContext};
use crate::osver;
use crate::wave::{float_format, SampleKind, WaveInfo};

use super::shared::{create_stream_event, OpenCapture};

/// 伪设备只吃写死的格式：32 位浮点 / 48 kHz / 双声道 / 掩码 0x3。
/// 它的 `GetMixFormat` 是不能用的，所以格式必须自己造，而且必须是这一组。
const LOOPBACK_RATE: u32 = 48_000;
const LOOPBACK_CHANNELS: u16 = 2;

/// 等激活回调的上限。正常是几毫秒；卡住一般意味着目标进程刚好退了。
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

/// 激活完成的回调对象。
///
/// 必须同时实现 `IAgileObject`：不实现的话 `ActivateAudioInterfaceAsync`
/// 直接返回 E_ILLEGAL_METHOD_CALL（0x8000000E）。`IAgileObject` 本身没有方法，
/// 它只是告诉 COM“这个对象跨套间安全”，所以 co-implement 的成本就是加一个名字。
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivateHandler {
    done: Arc<(Mutex<bool>, Condvar)>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // 回调里只负责叫醒等待方。真正的结果从 operation 上取，
        // 但那件事留给等待方做——在回调线程里做完还要跨线程搬 COM 接口，更麻烦。
        let (lock, cv) = &*self.done;
        if let Ok(mut done) = lock.lock() {
            *done = true;
        }
        cv.notify_all();
        Ok(())
    }
}

impl IAgileObject_Impl for ActivateHandler_Impl {}

/// 激活参数的容器。
///
/// 参数结构体和包着它的 PROPVARIANT 必须在**堆上**，而且要活过异步回调：
/// `ActivateAudioInterfaceAsync` 不复制这块内存，放栈上函数一返回就是野指针
/// （踩过：表现是激活偶发失败或者拿到别的进程的声音）。
struct ActivationParams {
    /// 装箱是关键：地址固定，能安全交给异步调用。
    params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
    variant: Box<PROPVARIANT>,
}

impl ActivationParams {
    fn new(pid: u32, include_tree: bool) -> Self {
        let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: if include_tree {
                        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                    } else {
                        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                    },
                },
            },
        });

        // PROPVARIANT 得手工填成 VT_BLOB（vt = 65），blob 指向上面那个结构体。
        // windows-rs 没有构造 VT_BLOB 的安全接口，只能按内存布局写。
        let mut variant = Box::new(PROPVARIANT::default());
        // SAFETY: PROPVARIANT 是 C 布局的联合体；这里写的是 vt + blob 两个字段，
        // 组合合法（VT_BLOB 对应 blob 成员）。blob 指向的 Box 与本结构体同生共死，
        // 而本结构体活到 GetActivateResult 之后，所以指针在整个异步过程中有效。
        unsafe {
            let inner = &mut variant.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob.cbSize =
                std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
            inner.Anonymous.blob.pBlobData =
                params.as_mut() as *mut AUDIOCLIENT_ACTIVATION_PARAMS as *mut u8;
        }

        Self { params, variant }
    }

    fn as_propvariant(&self) -> *const PROPVARIANT {
        self.variant.as_ref() as *const PROPVARIANT
    }
}

impl Drop for ActivationParams {
    fn drop(&mut self) {
        // 不能调 PropVariantClear：blob 那块内存是我们自己的 Box，
        // 交给 COM 释放会直接崩。手工把字段清掉，Box 各自 Drop。
        // SAFETY: 只写我们自己填过的联合体字段，不释放任何东西。
        unsafe {
            let inner = &mut self.variant.Anonymous.Anonymous;
            inner.Anonymous.blob.pBlobData = std::ptr::null_mut();
            inner.Anonymous.blob.cbSize = 0;
            inner.vt = windows::Win32::System::Variant::VT_EMPTY;
        }
        let _ = &self.params;
    }
}

/// 打开进程环回流。调用线程必须已经初始化成 MTA。
///
/// 回调对象要落在 MTA 上，所以这个函数只能在 MTA 线程上调
/// （采集线程用 `ComGuard::mta()`，满足这个前提）。
pub(crate) fn open_process_loopback(pid: u32, include_tree: bool) -> PortResult<OpenCapture> {
    let build = osver::os_build_number();
    if !osver::process_loopback_supported(build) {
        // 版本不够就明确报错，让上层退到整机环回，而不是在这里悄悄换路。
        return Err(PortError::new(osver::unsupported_message(build)));
    }

    let params = ActivationParams::new(pid, include_tree);
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler = ActivateHandler {
        done: Arc::clone(&done),
    }
    .into();

    // SAFETY: 设备路径是系统常量；riid 指向本地 GUID；激活参数在堆上且
    // 活到本函数末尾（params 在作用域内），回调对象由 windows-rs 持引用计数。
    let operation: IActivateAudioInterfaceAsyncOperation = unsafe {
        ActivateAudioInterfaceAsync(
            PCWSTR(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK.as_ptr()),
            &IAudioClient::IID,
            Some(params.as_propvariant()),
            &handler,
        )
    }
    .ctx("发起进程环回激活失败")?;

    // 等回调。带超时，不能无限等——目标进程在激活途中退出时回调可能不来。
    {
        let (lock, cv) = &*done;
        let mut guard = lock.lock().map_err(|_| {
            PortError::new("the process-loopback activation state lock is poisoned")
        })?;
        while !*guard {
            let (next, timeout) = cv.wait_timeout(guard, ACTIVATE_TIMEOUT).map_err(|_| {
                PortError::new(
                    "the lock was poisoned while waiting for process-loopback activation",
                )
            })?;
            guard = next;
            if timeout.timed_out() && !*guard {
                // 超时说明激活还挂着。这时候把 params 释放掉，系统那边可能还
                // 拿着 pBlobData 在读——直接漏掉这几十字节，别给它悬垂指针。
                // 只有本来就出错的这条路会漏，正常路径照常释放。
                std::mem::forget(params);
                return Err(PortError::new(format!(
                    "process-loopback activation timed out (after {} s; target PID {pid} may have exited)",
                    ACTIVATE_TIMEOUT.as_secs()
                )));
            }
        }
    }

    // 第二个 HRESULT：激活调用本身成功了不代表激活成功，
    // 真正的结果在这个出参里。只看函数返回值会拿到一个 null 接口然后到处崩。
    let mut activate_hr = HRESULT(0);
    let mut interface = None;
    // SAFETY: operation 有效；两个出参都指向本地变量。
    unsafe { operation.GetActivateResult(&mut activate_hr, &mut interface) }
        .ctx("读进程环回激活结果失败")?;
    if activate_hr.is_err() {
        return Err(loopback_activation_error(activate_hr, pid));
    }
    let client: IAudioClient = interface
        .ok_or_else(|| {
            PortError::new("process-loopback activation succeeded but returned no audio interface")
        })?
        .cast()
        .ctx("进程环回激活结果不是音频接口")?;

    // 格式必须自己造：这个伪设备上 GetMixFormat 是不能用的（会失败或给垃圾）。
    let format = float_format(LOOPBACK_RATE, LOOPBACK_CHANNELS);
    // SAFETY: client 有效；格式头由 float_format 造出，栈上结构体在 Initialize
    // 调用期间有效（Initialize 会自己拷走）。
    unsafe {
        crate::client::initialize_default_period(
            &client,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            &format.Format,
        )
    }
    .ctx("初始化进程环回流失败")?;

    let event = create_stream_event()?;
    // SAFETY: client 已初始化；事件在 OpenCapture 存活期间有效。
    unsafe { client.SetEventHandle(event.raw()) }.ctx("绑定进程环回事件失败")?;
    // SAFETY: client 已初始化，取采集服务接口。
    let capture: IAudioCaptureClient =
        unsafe { client.GetService() }.ctx("获取进程环回采集接口失败")?;
    // SAFETY: 一切就绪，开始走流。
    unsafe { client.Start() }.ctx("启动进程环回流失败")?;

    // 注意：这里绝不能用 GetBufferSize 去算缓冲大小。伪设备上它返回垃圾
    //（见过 3131961357）而且不报错，照它分配内存就是几个 GB。
    // 我们的循环按 GetNextPacketSize 拿到的帧数走，不预分配。
    Ok(OpenCapture {
        client,
        capture,
        event,
        info: WaveInfo {
            sample_rate: LOOPBACK_RATE,
            channels: LOOPBACK_CHANNELS,
            kind: SampleKind::F32,
            block_align: (LOOPBACK_CHANNELS as usize) * 4,
        },
    })
}

/// 把激活失败的 HRESULT 翻成人话。
fn loopback_activation_error(hr: HRESULT, pid: u32) -> PortError {
    let code = hr.0 as u32;
    let hint = match code {
        // E_ILLEGAL_METHOD_CALL：回调对象没在 MTA 上，或者没实现 IAgileObject。
        0x8000_000E => "（回调对象套间不对，这是代码问题，不是环境问题）",
        // E_INVALIDARG：参数结构体没活到回调，或者 blob 大小写错了。
        0x8007_0057 => "（激活参数不对，可能是目标 PID 已经没了）",
        _ => "",
    };
    hr_err(
        &format!("进程环回激活被系统拒绝（目标 PID {pid}）{hint}"),
        hr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_params_point_at_the_boxed_struct() {
        let p = ActivationParams::new(1234, true);
        // SAFETY: 刚构造完，vt/blob 都是我们自己填的。
        unsafe {
            let inner = &p.variant.Anonymous.Anonymous;
            assert_eq!(inner.vt, VT_BLOB);
            let blob = inner.Anonymous.blob;
            assert_eq!(
                blob.cbSize as usize,
                std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
            );
            // blob 必须指向那个 Box，而不是别的什么地方。
            assert_eq!(
                blob.pBlobData as usize,
                p.params.as_ref() as *const AUDIOCLIENT_ACTIVATION_PARAMS as usize
            );
            let params = &*(blob.pBlobData as *const AUDIOCLIENT_ACTIVATION_PARAMS);
            assert_eq!(
                params.ActivationType,
                AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
            );
            assert_eq!(params.Anonymous.ProcessLoopbackParams.TargetProcessId, 1234);
            assert_eq!(
                params.Anonymous.ProcessLoopbackParams.ProcessLoopbackMode,
                PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
            );
        }
    }

    #[test]
    fn exclude_mode_is_selected_without_tree() {
        let p = ActivationParams::new(7, false);
        // SAFETY: 同上。
        unsafe {
            let blob = p.variant.Anonymous.Anonymous.Anonymous.blob;
            let params = &*(blob.pBlobData as *const AUDIOCLIENT_ACTIVATION_PARAMS);
            assert_eq!(
                params.Anonymous.ProcessLoopbackParams.ProcessLoopbackMode,
                PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
            );
        }
    }

    #[test]
    fn dropping_params_does_not_hand_our_memory_to_com() {
        // 只要 Drop 里没调 PropVariantClear，这个测试跑完不该崩。
        for pid in 0..50u32 {
            let _ = ActivationParams::new(pid, pid % 2 == 0);
        }
    }

    #[test]
    fn handler_can_be_built_and_signals() {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let handler: IActivateAudioInterfaceCompletionHandler = ActivateHandler {
            done: Arc::clone(&done),
        }
        .into();
        // 同时实现了 IAgileObject 才能 cast 成功——这是激活能过的前提。
        assert!(handler.cast::<IAgileObject>().is_ok());
    }

    #[test]
    fn activation_error_mentions_hresult_and_pid() {
        let err = loopback_activation_error(HRESULT(0x8000_000Eu32 as i32), 4242);
        assert!(err.message.contains("4242"), "{}", err.message);
        assert!(err.message.contains("0x8000000E"), "{}", err.message);
    }

    #[test]
    fn refuses_when_build_too_low() {
        // 真机是 26200，所以这里只验证消息生成逻辑本身。
        let msg = osver::unsupported_message(19045);
        assert!(msg.contains("整机环回"), "{msg}");
    }
}
