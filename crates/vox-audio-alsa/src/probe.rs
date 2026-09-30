//! ALSA 探测与参数协商。**这里只放纯逻辑**：所有不碰硬件的决策都在这儿，
//! 所有碰硬件的调用（`PCM::new` / `hw_params` / `readi`）都在 capture/playback/registry 里。
//!
//! 为什么不把协商直接写进线程里：PipeWire 那侧是向图请求格式、由图转，
//! ALSA 直开设备**没有那张图**，阶梯必须自己走一遍。把阶梯与"io 结果该
//! 怎么判读"抽成纯函数，是这一稿唯一能在无声卡的 CI 上验收的东西
//! （`docs/plans/S4-C-ALSA.md` §3.2 / §9.1）。

use alsa::pcm::{Format, PCM};
use alsa::Direction;
use vox_core::ports::PortError;

/// 我们向采集侧要的率：降噪的原生率（`docs/platform/EMBEDDED.md` §4-8 的三处写死之一）。
pub(crate) const CAPTURE_WANT_RATE: u32 = 48_000;
/// 采集侧要的声道数：麦克风基本都给单声道或双声道，多声道麦克风阵列不在这一稿。
pub(crate) const CAPTURE_WANT_CHANNELS: u32 = 1;
/// 播放侧要的声道数：单声道铺成双声道给声卡（与 `vox-audio-linux` 侧同思路）。
pub(crate) const PLAYBACK_WANT_CHANNELS: u32 = 2;

/// io 之后该怎么走。**热路径上唯一的分支**，单独拆出来就是为了能脱开硬件测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// 数据到手 / 出去了，继续。
    Continue { frames: usize },
    /// 缓冲欠载（写）/ 本轮没有数据（读）：补静音或跳过，**不算错**。
    Underrun,
    /// `EPIPE`（欠载致设备挂起）或 `ESTRPIPE`（设备内部断了，如 USB 拔出重插）：
    /// 恢复后继续。不恢复的话任何一次爆音都会让整条流水线永久停摆。
    Recover,
    /// 别的错：这条流到此为止。
    Fatal { func: &'static str, errno: i32 },
}

/// `readi` / `writei` 的结果该怎么判读。
///
/// 只看"这一次有没有真的搬动帧"，**不跟请求量比**：短读/短写搬动的那 `n` 帧是真数据，
/// 调用方必须按 `n` 处理（采集侧只换算前 `n` 帧，播放侧只把游标推进 `n` 帧、剩下的下一轮再写）。
/// 早先把"不满一个周期"也算成欠载，会让采集侧丢掉已读到的帧、播放侧把已写出的帧再写一遍。
///
/// 参数用 `&Result<..>` 而不是 `Result<..>` 是为了不把 `alsa::Error` 的所有权搬进这个纯函数；
/// `usize` 就是 `alsa::pcm::IO::readi` / `::writei` 的返回类型（`alsa-0.12.1/src/pcm.rs::IO::readi`）。
pub(crate) fn step_after(res: &Result<usize, alsa::Error>) -> Step {
    match res {
        // 一帧都没搬 = 欠载（写）/ 本轮没数据（读）。它必须被当成欠载而不是"成功"，
        // 否则播放侧会在没数据时空转烧 CPU、`rendered_samples` 也会被虚增。
        Ok(0) => Step::Underrun,
        Ok(frames) => Step::Continue { frames: *frames },
        // `alsa` 的 `acheck!` 把 errno 取了正号（`alsa-0.12.1/src/error.rs:28`），
        // 所以这里跟正的 `libc::EPIPE` 比。`EAGAIN` 特意不在这一支：
        // 非阻塞 PCM 上撞到它只说明循环里忘了先 poll，是真错不是可恢复（§4.3）。
        Err(e) => match e.errno() {
            libc::EPIPE | libc::ESTRPIPE => Step::Recover,
            errno => Step::Fatal {
                func: e.func(),
                errno,
            },
        },
    }
}

/// 采样格式阶梯：先要最好的（S32_LE），实在不行退 S16_LE。
///
/// 32 位不是为了音质（我们是 f32 进 f32 出），是为了**采集侧的余量**：
/// 廉价 USB 声卡常把麦克风增益拉得很高，16 位很容易削顶，而削顶发生在
/// 驱动里，我们拿回来就只剩 0 了。
pub(crate) fn format_ladder() -> Vec<Format> {
    let mut out = vec![Format::s32()];
    if Format::s16() != Format::s32() {
        out.push(Format::s16());
    }
    out
}

/// 协商阶梯里的一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// 设备直接给这个率：**不重采样**（S4 §2.3"能不重采样就不重采样"的字面兑现）。
    Exact,
    /// 设备只能给别的率，但 `plughw:` 的 plug 插件能替我们换率：
    /// 我们仍然按 `want` 推给设备，Vox 侧**不装重采样器**。
    PlugConvert,
    /// 设备只能给别的率且不走 plug：在 `hw:` 上按 `probe_rate` 试探，
    /// 装 Vox 侧重采样器（`ResampleFactory` 注入的那份）。
    VoxResample { probe_rate: u32 },
}

/// 播放侧的率协商阶梯，按 `wanted` 生成。**全灭时调用方必须报错，不许悄悄换一个率**
/// （与 `vox-audio-win::rates::choose_output_rate` 的"退回设备默认率"相反：
/// 那份退下去等于替用户做了一个没要求的重采样，这里报出来让用户知道）。
pub(crate) fn rate_ladder(wanted: u32) -> Vec<Attempt> {
    vec![
        Attempt::Exact,
        Attempt::PlugConvert,
        Attempt::VoxResample { probe_rate: wanted },
    ]
}

/// f32 → 设备整数样本用的满量程。
///
/// 施工稿写的是 `2_147_483_647.0`（`i32::MAX`），**但 f32 存不下这个数**：24 位尾数在
/// `2^30..2^31` 区间的间距是 128，`2147483647` 会舍入到 `2147483648`。写不写都是
/// `2^31`，所以这里直接写 `2^31`，让字面量等于代码实际在做的事。
///
/// 后果（`out_of_range_and_nan_clamp_to_silence` 与 `f32_round_trips_through_the_device_sample`
/// 各钉一条）：`-1.0` 落到 `i32::MIN`、`+1.0` 经 Rust 的饱和转换落到 `i32::MAX`——
/// 这就是 PCM 的标准满量程约定（16 位也是 `-1.0 → -32768`）。两边**往返都精确**，
/// `0.0` 也精确映射到 0，因为 `1.0 / I32_SCALE == 2^-31` 正好是 f32 的幂。
pub(crate) const I32_SCALE: f32 = 2_147_483_648.0;

/// f32 → 设备整数样本。**显式夹紧**，不靠 Rust 的 float→int 饱和转换。
pub(crate) fn f32_to_i32(x: f32) -> i32 {
    // NaN 落在这里：`clamp` 对 NaN 返回 NaN，转整数是 0。NaN 进设备等于噪声，
    // 显式归零比让下游猜要好。
    let scaled = (x * I32_SCALE).clamp(-I32_SCALE, I32_SCALE);
    if scaled.is_nan() {
        0
    } else {
        scaled as i32
    }
}

/// 设备整数样本 → f32。**这是采集侧唯一的换算**。
pub(crate) fn i32_to_f32(x: i32) -> f32 {
    x as f32 * (1.0 / I32_SCALE)
}

/// 一块该喂给 `Blocker` 的音频有多少个交织样本。
///
/// `block_ms` 的下限 10 ms 与 `vox_dsp::chunk::Blocker::new` 逐字同口径：不夹的话
/// `block_ms = 0` 会算出 1 帧，而 `Blocker` 那边仍按 10 ms 攒块，两边对不上，
/// 采集侧就会拿错长度的缓冲去喂。
pub(crate) fn block_frames(rate: u32, block_ms: u32, channels: u16) -> usize {
    let block_ms = block_ms.max(10);
    let frames = ((rate as u64 * block_ms as u64) / 1000).max(1) as usize;
    frames * channels.max(1) as usize
}

/// `alsa::Error` → 中文 `PortError`。**带函数名和 errno**：无屏档没有界面，
/// 运维只看得到 journal，报错必须自解释。
pub(crate) fn map_err(what: &str, e: alsa::Error) -> PortError {
    PortError::new(format!(
        "ALSA {what} 失败：{}（{}，errno {}）",
        e,
        e.func(),
        e.errno()
    ))
}

/// ALSA 在不在 = 能不能打开一条 PCM。开完立刻丢，失败返回 `false`。
///
/// 选"打开一条 PCM"而不是"读 `/proc/asound`"或"查 `/dev/snd`"：前者是**端到端的
/// 真事实**（权限、驱动、`/dev/snd` 可达性全都在里面），后两者只是必要条件。
///
/// 第三次参数取 `true`（非阻塞打开）：这里不做任何读写，阻塞与否不影响结论，
/// 但与 `capture.rs` / `playback.rs` 的打开方式保持一致，免得两边口径分叉。
pub(crate) fn alsa_available() -> bool {
    PCM::new("default", Direction::Capture, true).is_ok()
}

/// PipeWire 铺的 ALSA 桥的定义：名字段是 `pcm.pipewire.*`。
const PIPEWIRE_PCM_DEFINITION: &str = "pcm.pipewire";

/// 主配置与 conf.d 目录。`alsa.conf` 里的 `confdir` 指向哪个因发行版而异，
/// 三处都扫一遍——这里宁可多扫（多扫的代价是几条 `read_to_string`）也不要漏报。
const ALSA_CONF_FILES: [&str; 1] = ["/usr/share/alsa/alsa.conf"];
const ALSA_CONF_DIRS: [&str; 2] = ["/usr/share/alsa/alsa.conf.d", "/etc/alsa/conf.d"];

/// 一份 alsa 配置文本里有没有 PipeWire 铺的 pcm 定义。**纯函数**，过滤在这里。
///
/// 只认 `pcm.pipewire` 这个**定义名**，不是"文本里出现过 pipewire"：PulseAudio 机器上
/// 也会有一堆提到 PipeWire 的注释与 `.conf` 文件名，按关键词认会到处误报。
/// 名字后面跟的是 `.`（子定义如 `pcm.pipewire.pulse`）或空白/行尾，所以还得看一眼
/// 紧跟的那个字符——`pcm.pipewire-extra.pcm` 是另一个设备，不是这座桥。
pub(crate) fn pipewire_pcm_declared(conf: &str) -> bool {
    conf.match_indices(PIPEWIRE_PCM_DEFINITION).any(|(at, _)| {
        match conf[at + PIPEWIRE_PCM_DEFINITION.len()..].chars().next() {
            // 定义名到此为止（文件被截断的情况）也算命中。
            None => true,
            Some(c) => !c.is_ascii_alphanumeric() && c != '-' && c != '_',
        }
    })
}

/// PipeWire 的 ALSA 桥在不在。**只读文件系统**：缺文件 / 读不动一律当"没有"。
///
/// 不作任何能力判据（`alsa_available` 才是判据），只用来在 `startup_notes` 里说一句
/// "检测到 PipeWire 的 ALSA 桥；当前按无屏档缺省使用 ALSA 直开设备"。
pub(crate) fn pipewire_alsa_bridge_present() -> bool {
    let in_files = ALSA_CONF_FILES
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|conf| pipewire_pcm_declared(&conf));
    if in_files {
        return true;
    }
    ALSA_CONF_DIRS.iter().any(|dir| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                && std::fs::read_to_string(entry.path())
                    .map(|conf| pipewire_pcm_declared(&conf))
                    .unwrap_or(false)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_dsp::chunk::Blocker;

    /// 约等：f32 → i32 → f32 必然掉一位有效位，用相对误差判。
    fn close_enough(got: f32, want: f32) -> bool {
        (got - want).abs() <= want.abs() * 1e-6 + 1e-9
    }

    fn alsa_err(func: &'static str, errno: i32) -> alsa::Error {
        alsa::Error::new(func, errno)
    }

    #[test]
    fn format_ladder_puts_s32_first_and_never_repeats() {
        let ladder = format_ladder();
        assert_eq!(
            ladder[0],
            Format::s32(),
            "32 位必须排第一：廉价 USB 麦在 16 位下会削顶"
        );
        assert_eq!(ladder, vec![Format::s32(), Format::s16()]);
        assert_eq!(
            ladder.iter().filter(|f| **f == Format::s32()).count(),
            1,
            "阶梯里不许出现重复项，否则会白跑一次 hw_params"
        );
    }

    #[test]
    fn rate_ladder_is_exact_then_plugin_then_vox_resample() {
        assert_eq!(
            rate_ladder(vox_core::cloud::protocol::OUTPUT_SAMPLE_RATE),
            vec![
                Attempt::Exact,
                Attempt::PlugConvert,
                Attempt::VoxResample {
                    probe_rate: vox_core::cloud::protocol::OUTPUT_SAMPLE_RATE
                },
            ]
        );
        let ladder = rate_ladder(44_100);
        assert_eq!(
            ladder[2],
            Attempt::VoxResample { probe_rate: 44_100 },
            "第 3 档要拿 wanted 去试设备，探针率写死成别的值就换错了率"
        );
    }

    #[test]
    fn a_short_read_still_counts_the_frames_it_moved() {
        // 短读的 30 帧是真数据：当成欠载丢掉 = 采集断一截、播放重复一截。
        assert_eq!(step_after(&Ok(30)), Step::Continue { frames: 30 });
    }

    #[test]
    fn a_zero_frame_result_is_underrun_too() {
        assert_eq!(step_after(&Ok(0)), Step::Underrun);
    }

    #[test]
    fn epipe_and_estrpipe_recover_while_eagain_is_fatal() {
        assert_eq!(
            step_after(&Err(alsa_err("snd_pcm_writei", libc::EPIPE))),
            Step::Recover,
            "一次爆音就把整条流永久停摆是不许的"
        );
        assert_eq!(
            step_after(&Err(alsa_err("snd_pcm_readi", libc::ESTRPIPE))),
            Step::Recover,
            "USB 拔出重插是能 recover 的，不能当致命错"
        );
        assert_eq!(
            step_after(&Err(alsa_err("snd_pcm_writei", libc::EAGAIN))),
            Step::Fatal {
                func: "snd_pcm_writei",
                errno: libc::EAGAIN
            },
            "非阻塞 PCM 上撞到 EAGAIN 只说明循环里忘了先 poll，当可恢复会把'忘了 poll'变成静默死循环"
        );
    }

    #[test]
    fn a_full_result_is_progress() {
        assert_eq!(step_after(&Ok(480)), Step::Continue { frames: 480 });
        // 比请求多不可能发生，但真发生了也不能当成欠载把数据丢掉。
        assert_eq!(step_after(&Ok(512)), Step::Continue { frames: 512 });
    }

    #[test]
    fn f32_round_trips_through_the_device_sample() {
        for x in [0.0f32, 1.0, -1.0, 0.5, -0.5, 1e-4, -1e-4] {
            let back = i32_to_f32(f32_to_i32(x));
            assert!(
                close_enough(back, x),
                "{x} 往返后成了 {back}（满量程用 2^31 还是 2^31-1 会在这里露馅）"
            );
        }
        assert_eq!(i32_to_f32(0), 0.0, "0.0 必须有一个精确的映射");
        // 满量程两端也必须精确往返，否则爆音那一头会削掉一格。
        assert_eq!(i32_to_f32(i32::MAX), 1.0);
        assert_eq!(i32_to_f32(i32::MIN), -1.0);
    }

    #[test]
    fn out_of_range_and_nan_clamp_to_silence() {
        // 满量程是 PCM 的标准约定：正侧饱和到 i32::MAX、负侧到 i32::MIN。
        // 不夹紧的话 2.0 会溢出成 UB/垃圾值；NaN 若不显式归零就变成设备里的噪声。
        assert_eq!(f32_to_i32(2.0), i32::MAX);
        assert_eq!(f32_to_i32(-2.0), i32::MIN);
        assert_eq!(f32_to_i32(f32::INFINITY), i32::MAX);
        assert_eq!(f32_to_i32(f32::NEG_INFINITY), i32::MIN);
        assert_eq!(f32_to_i32(f32::NAN), 0, "NaN 进设备等于噪声，显式归零");
        assert_eq!(f32_to_i32(1.0), i32::MAX, "+1.0 恰好落在正侧满量程");
        assert_eq!(f32_to_i32(-1.0), i32::MIN, "-1.0 恰好落在负侧满量程");
    }

    #[test]
    fn block_frames_agrees_with_the_shared_blocker() {
        // 最后两组特意取 block_ms < 10：采集侧一旦把协商到的率 / 块长算错，
        // 块长就会和 Blocker 内部那套对不上，AudioChunk 报错的率让芯重采样变调。
        for (rate, block_ms, channels) in [
            (48_000u32, 20u32, 1u16),
            (48_000, 10, 2),
            (16_000, 20, 1),
            (44_100, 10, 2),
            (24_000, 20, 2),
            (8_000, 0, 1),
            (48_000, 9, 2),
        ] {
            let ours = block_frames(rate, block_ms, channels);
            let shared =
                Blocker::new(rate, channels, block_ms).frames_per_block() * channels as usize;
            assert_eq!(
                ours, shared,
                "({rate}, {block_ms}, {channels}) 两边算出来的块长不一致"
            );
        }
        // 帧数 → 交织样本数：48000/20 ms = 960 帧，单声道 960 样本、双声道 1920。
        assert_eq!(block_frames(48_000, 20, 1), 960);
        assert_eq!(block_frames(48_000, 20, 2), 1920);
    }

    #[test]
    fn only_a_pipewire_pcm_definition_counts_as_the_bridge() {
        assert!(pipewire_pcm_declared(
            "pcm.pipewire.pcm {\n type pipewire\n}\n"
        ));
        // 只是提到 PipeWire（注释、pulse 的转发、PipeWire 自己的 socket 名）
        // 不算桥——按关键词认会在纯 Pulse 机器上误报。
        assert!(!pipewire_pcm_declared("# see pipewire(7) for details\n"));
        assert!(!pipewire_pcm_declared("pcm.pulse {\n type pulse\n}\n"));
        assert!(!pipewire_pcm_declared(
            "pcm.pipewire-extra.pcm {\n type null\n}\n"
        ));
    }

    #[test]
    fn probing_never_panics_without_a_sound_card() {
        // CI 上没有 /dev/snd、配置目录也可能是空的：这两个原语必须安静地给出
        // false，而不是 unwrap 炸掉整个测试进程。
        let _ = alsa_available();
        let _ = pipewire_alsa_bridge_present();
    }
}
