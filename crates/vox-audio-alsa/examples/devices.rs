//! 手动验一遍 ALSA 后端的设备目录。跑法：
//!
//! ```text
//! cargo run -p vox-audio-alsa --example devices
//! ```
//!
//! 检查点（后两条是这个目录自己的纪律，前两条是环境前提）：
//!
//! 1. 每个方向至少列出一条设备；全空说明这台机器没有开着的 ALSA 设备
//!    （`/dev/snd` 不可达或没有驱动），先修环境再看别的。
//! 2. 列表里只该有 `hw:` 开头的硬件条目与 `default`；出现 `pulse` / `pipewire` /
//!    `front:` / `plughw:` 就说明过滤表（`registry.rs::keep_hint`）破了。
//! 3. **每个方向至多一个 `*`**，不是"有且只有一个"——见下面「为什么可能一个 `*` 都没有」。
//! 4. 列出来的每个名字都真的开得了 PCM：它就是能直接喂给 `loopback` 的名字（§5.1）。
//!    开不了只报不失败：设备正被别的程序占着（EBUSY）是环境状态，不是目录的 bug。
//!
//! 退出码只在目录**自己**能保证的那两条上给非零（列表为空、星号多于一个）；
//! 打不开设备只打印，不改退出码。

use alsa::pcm::PCM;
use alsa::Direction;
use vox_audio_alsa::{alsa_available, pipewire_alsa_bridge_present, AlsaDeviceRegistry};
use vox_core::ports::DeviceRegistry;

/// 打印一段"为什么现在没有星号"的说明。**一个星号都没有是合法结果**。
///
/// `registry.rs` 的口径是"能打开 `default`、从 `snd_pcm_info` 读回硬件地址、
/// 与枚举结果逐条比对"三者都对上才标星。`default` 经服务层（PipeWire / Pulse）走时
/// `snd_pcm_info` 认不出底层那张卡，报的 card 是 `-1`——本机实测就是这个常态。
/// 那时全表无星号才是诚实的答案：宁可没有星号，也不要指错设备（指错的后果是
/// 译文播到麦上，形成回声串扰）。见 `registry.rs` 模块头第 3 条与 §5.4 第 4 步。
const NO_STAR_WHY: &str = "\
`default` 经服务层（PipeWire / Pulse）路由时，snd_pcm_info 认不出底层的硬件卡（card = -1），
  此时按 §5.4 第 4 步「谁都别标」处理。要确认缺省到底指哪儿，直接看这条：aplay -L 里的 default 定义。";

fn main() {
    let mut problems: Vec<String> = Vec::new();

    println!(
        "ALSA 可用（能打开一条 PCM）：{}",
        if alsa_available() { "是" } else { "否" }
    );
    println!(
        "检测到 PipeWire 的 ALSA 桥：{}（只作提示，不作能力判据）",
        if pipewire_alsa_bridge_present() {
            "有"
        } else {
            "没有"
        }
    );

    if !alsa_available() {
        eprintln!(
            "结果：FAIL —— 打不开任何一条 ALSA PCM。无屏档缺省后端就是 ALSA，\
             先查 `/dev/snd` 的可达性（`ls -l /dev/snd`）与当前用户是否在 audio 组"
        );
        std::process::exit(1);
    }

    let registry = AlsaDeviceRegistry::new();
    for (title, side, devices) in [
        (
            "输入设备（麦克风）",
            Direction::Capture,
            registry.input_devices(),
        ),
        (
            "输出设备（扬声器）",
            Direction::Playback,
            registry.output_devices(),
        ),
    ] {
        let devices = match devices {
            Ok(devices) => devices,
            Err(e) => {
                eprintln!("枚举{title}失败：{e}");
                std::process::exit(1);
            }
        };

        println!("== {title} ==");
        if devices.is_empty() {
            println!("  （一条都没有）");
            problems.push(format!("{title}：一条都没有 —— 没有可用的 ALSA 设备"));
            continue;
        }
        for device in &devices {
            println!(
                "  {}{}",
                if device.is_default { "* " } else { "  " },
                device.name
            );
            if !filter_allows(&device.name) {
                problems.push(format!("{title}：不该出现在列表里的名字 {}", device.name));
            }
            match PCM::new(&device.name, side, true) {
                Ok(_) => println!("      ↳ 能打开"),
                Err(e) => println!("      ↳ 打不开：{e}（设备可能被别的程序占着）"),
            }
        }

        // §5.4 的纪律是"宁可没有星号，也不要指错设备"：能对上就标一个，对不上就一个都不标。
        // 所以这里是「至多一个」，而 PipeWire 那份 devices example 的「有且只有一个」在这里是错的。
        let starred = devices.iter().filter(|d| d.is_default).count();
        if starred > 1 {
            problems.push(format!(
                "{title}：标出了 {starred} 个缺省设备，至多只能有一个"
            ));
        }
        if starred == 0 {
            println!("  （没有 *)");
            println!("      原因：{NO_STAR_WHY}");
        }
    }

    if problems.is_empty() {
        println!("结果：PASS（过滤表与「至多一个缺省」两条都成立）");
        return;
    }
    eprintln!("结果：FAIL");
    for problem in &problems {
        eprintln!("  - {problem}");
    }
    std::process::exit(1);
}

/// 列表里该出现的名字只有两类：`hw:` 开头的硬件条目与 `default`（§5.4 白名单）。
///
/// 这里写成 example 侧的一份**独立**副本而不是去调 `registry.rs` 的私有函数：
/// example 站在库的外面，正好可以当成"过滤表有没有被改坏"的旁证——
/// 库里的实现改了而这里没改，验收时就会红。`plughw:` 特意列进来：它**被列表丢掉**，
/// 但协商阶梯的第 2 档**照样用**它（名字是拿 `hw:` 的 id 自己拼出来的），
/// 在列表里看到它说明白名单漏了。
fn filter_allows(name: &str) -> bool {
    name == "default" || (name.starts_with("hw:") && !name.starts_with("hwtraw"))
}
