//! 麦克风采集：WASAPI 共享模式，事件驱动。
//!
//! 共享模式而不是独占：独占能少几毫秒延迟，但会把设备占死，别的软件（包括
//! VRChat 自己）就用不了麦了。语音翻译这点延迟无所谓，能共存更重要。
//!
//! 格式直接用 `GetMixFormat` 的结果，不去跟设备较劲。共享模式下混音格式是
//! 唯一保证能 `Initialize` 成功的格式，协商到什么就把什么报给上层，
//! 由流水线那边负责转到 16 kHz。

use vox_core::ports::PortResult;
use windows::Win32::Media::Audio::{eCapture, AUDCLNT_STREAMFLAGS_EVENTCALLBACK};

use super::shared::{open_capture_client, OpenCapture};

/// 按设备名打开麦克风。`None` 用系统默认输入设备。
pub(crate) fn open_microphone(device_name: Option<&str>) -> PortResult<OpenCapture> {
    open_capture_client(
        eCapture,
        device_name,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        "microphone",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ComGuard;

    #[test]
    #[ignore = "需要真麦克风，手动跑：cargo test -p vox-audio-win -- --ignored"]
    fn default_microphone_opens_and_reports_format() {
        let _com = ComGuard::mta().unwrap();
        let open = open_microphone(None).unwrap();
        let f = open.format();
        assert!(f.sample_rate >= 8_000 && f.sample_rate <= 192_000, "{f:?}");
        assert!(f.channels >= 1);
    }

    #[test]
    fn unknown_device_name_fails_with_chinese_message() {
        let _com = ComGuard::mta().unwrap();
        // OpenCapture 里全是 COM 接口，不方便 derive Debug，所以手动解构而不是 unwrap_err。
        let Err(err) = open_microphone(Some("不存在的麦 zzz")) else {
            panic!("不存在的设备名不该打开成功");
        };
        assert!(err.message.contains("找不到"), "{}", err.message);
    }
}
