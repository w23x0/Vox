//! 建一个虚拟麦克风并挂着（默认 15 秒），期间可以去看系统里有没有它。
//!
//! ```text
//! cargo run -p vox-audio-linux --example virtual_mic          # 挂 15 秒
//! cargo run -p vox-audio-linux --example virtual_mic -- 60    # 挂 60 秒
//! ```
//!
//! 检查点：`wpctl status` 的 Sinks 里出现「Vox Virtual Mic」；`pw-link -l` 里能看到
//! 它的 `monitor_FL/FR` 端口；退出后设备从系统里消失（不留幽灵设备）。

use std::time::Duration;

use vox_audio_linux::{virtual_mic_description, VirtualSink};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(15);

    if VirtualSink::exists()? {
        eprintln!("系统里已经有同名虚拟麦了，先把它关掉再跑");
        return Ok(());
    }

    let sink = VirtualSink::create()?;
    println!(
        "虚拟麦已建立：{}（{}）",
        virtual_mic_description(),
        sink.node_name()
    );
    println!("在别的程序（VRChat / Discord / OBS）的录音设备里选它即可。");
    println!("挂 {seconds} 秒…");
    std::thread::sleep(Duration::from_secs(seconds));

    sink.destroy();
    println!("已删除，系统里不该再有它了。");
    Ok(())
}
