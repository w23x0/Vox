//! 整机环回：抓某个输出设备上的全部声音。
//!
//! 这是进程环回的退路：系统内部版本 < 20348 的机器上没有按进程抓的接口，
//! 只能整机抓。代价是别的软件出声也会被抓进来（QQ 消息提示音会被当成人在说话），
//! 所以只在没得选的时候用。

use vox_core::ports::PortResult;
use windows::Win32::Media::Audio::{
    eRender, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};

use super::shared::{open_capture_client, OpenCapture};

/// 打开输出设备的环回采集。`None` 用系统默认输出设备。
pub(crate) fn open_endpoint_loopback(device_name: Option<&str>) -> PortResult<OpenCapture> {
    open_capture_client(
        eRender,
        device_name,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        "output device",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ComGuard;

    #[test]
    #[ignore = "需要真声卡，手动跑：cargo test -p vox-audio-win -- --ignored"]
    fn default_endpoint_loopback_opens() {
        let _com = ComGuard::mta().unwrap();
        let open = open_endpoint_loopback(None).unwrap();
        let f = open.format();
        assert!(f.sample_rate >= 8_000);
        assert!(f.channels >= 1);
    }

    #[test]
    fn unknown_endpoint_name_fails_with_chinese_message() {
        let _com = ComGuard::mta().unwrap();
        let Err(err) = open_endpoint_loopback(Some("不存在的输出 zzz")) else {
            panic!("不存在的设备名不该打开成功");
        };
        assert!(err.message.contains("找不到"), "{}", err.message);
    }
}
