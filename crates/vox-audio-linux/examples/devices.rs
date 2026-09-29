//! 手动验一遍 Linux 音频后端到底通不通。跑法：
//!
//! ```text
//! cargo run -p vox-audio-linux --example devices            # 列设备 + 正在出声的程序
//! ```
//!
//! 检查点：默认设备有且只有一个被标出来；正在放声音的程序排在前面且 `active=true`；
//! 自己（vox 这个进程）不出现在列表里。

use vox_audio_linux::{pipewire_available, LinuxDeviceRegistry};
use vox_core::ports::DeviceRegistry;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !pipewire_available() {
        eprintln!("连不上 PipeWire：Linux 音频后端需要 PipeWire（主流发行版默认就有）");
        return Ok(());
    }

    let registry = LinuxDeviceRegistry::new();

    println!("== 输入设备 ==");
    for device in registry.input_devices()? {
        println!(
            "  {}{}",
            if device.is_default { "* " } else { "  " },
            device.name
        );
    }

    println!("== 输出设备 ==");
    for device in registry.output_devices()? {
        println!(
            "  {}{}",
            if device.is_default { "* " } else { "  " },
            device.name
        );
    }

    println!("== 正在出声的程序 ==");
    let apps = registry.audio_apps()?;
    if apps.is_empty() {
        println!("  （没有。先随便放个音频/视频再跑一次）");
    }
    for app in apps {
        println!(
            "  {}{}  executable={} pid={}",
            if app.active { "* " } else { "  " },
            app.display_name,
            app.executable,
            app.pid
        );
    }

    Ok(())
}
