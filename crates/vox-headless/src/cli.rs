//! 无屏入口的命令行。
//!
//! 五个开关，全是**数据**：往哪读配置、跑哪个模式、起来之后开哪条腿、跑多久。
//! **没有子命令**——子命令是 `voxctl`（控制面 CLI，另一个进程）的事；启动参数只描述
//! "这个进程怎么起"，不描述"能做什么"（后者是 `vox-mcp` 的动作清单）。
//!
//! 退出码跟 `voxctl` 同一套：`0` 成功 ｜ `2` 运行期失败 ｜ `3` 用法错误。
//! 参数写错时**宁可报错也不猜**（比如 `--config` 重复给、`--start` 配一次性模式）——
//! 无屏设备上的启动参数是 systemd unit 写的，猜错会变成一个安静跑错配置的服务。

use std::path::PathBuf;
use std::time::Duration;

/// 跑哪个模式。四者互斥（写两个就是用法错误）。
///
/// 后三个是**只读模式**（[`Mode::is_read_only`]）：装配到"设置 + 事实"就够，不碰 PipeWire
/// （不枚举设备、不探可用性）、不建配置目录、不写文件、不监听端口、不起流水线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 一直跑：装配 → 建引擎 → 起控制面（看开关）→ 开 `--start` 那几条腿 → 等退出。
    Run,
    /// 只装配：逐条验清单、报能力位，然后退出（只读，见上）。
    DryRun,
    /// 装配后打一份 `CapabilityReport` JSON 就退出（只读，见上）。
    PrintCapabilities,
    /// 装配后打一份"两份清单 + 有效能力位"的 JSON 就退出（S0 §4.3-A 的验收出口；只读，见上）。
    PrintComposition,
}

impl Mode {
    /// **只读模式**：`--print-capabilities` / `--print-composition` / `--dry-run`。
    ///
    /// 这三条路装配到"设置 + 事实"就够——清单是 `Composition::of(设置, 事实)`、能力报告是
    /// "档位上限 − 关掉的位"，设备目录与"PipeWire 在不在"一条都不进任何一格。所以它们不碰
    /// PipeWire（`Probe::Nothing`）、不建配置目录（`Paths::ensure_dir` 只给常驻模式调）、
    /// 不写文件、不监听端口、不起流水线。
    pub fn is_read_only(self) -> bool {
        !matches!(self, Mode::Run)
    }
}

/// 起来之后开哪条腿。无屏设备没有界面可按，所以这件事由启动参数（= systemd unit）说。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// 什么都不开：只当控制面的宿主（等 `session_open` 来开）。
    None,
    Speak,
    Listen,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// `--config <path>`：settings.json 的**文件**路径；`None` = 由配置目录回落推出来。
    pub config: Option<PathBuf>,
    pub mode: Mode,
    pub start: Start,
    /// `--run-for <秒>`：跑够这么多秒就自己收摊（冒烟/自检用）。`None` = 一直跑。
    pub run_for: Option<Duration>,
}

/// 解析结果。`--help` 不是错误，所以它得能被表达出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Run(Args),
    Help,
}

pub fn usage() -> String {
    "\
vox-headless —— 无屏档（小主板 ARM64 Linux）入口：芯 + PipeWire + 配置进 / 状态出 + 控制面

用法：
  vox-headless [--config <settings.json>]
                     [--print-capabilities | --print-composition | --dry-run]
                     [--start <speak|listen|all>] [--run-for <秒>]

开关：
  --config <path>         settings.json 的路径（要的是文件，不是目录；目录请用环境变量
                          VOX_CONFIG_DIR）。缺省按三级回落取：
                          $VOX_CONFIG_DIR → $XDG_CONFIG_HOME/vox → $HOME/.config/vox
  --print-capabilities    装配后打一份 CapabilityReport JSON（stdout），退出（只读：不碰 PipeWire、
                          不建配置目录、不写文件、不监听端口）
  --print-composition     装配后打一份 Composition 清单 JSON（两条腿 + 有效能力位，stdout），退出（只读，同上）
  --dry-run               只装配：逐条验清单、打能力报告，不监听端口、不碰 PipeWire（不枚举设备、
                          不探可用性）、不建配置目录、不开流；退出
  --start <endpoint>      起来之后开哪条腿（speak / listen / all）；缺省什么都不开
  --run-for <秒>          跑够这么多秒就收摊退出（0 = 一直跑）；缺省一直跑
  -h, --help              打这一页

环境变量：
  VOX_LOG           日志级别（tracing 的 EnvFilter 语法；缺省只打本 crate 的 info）
  VOX_CONFIG_DIR    配置目录（第一优先）
  VOX_API_KEY_<服务商>  API 密钥（例 VOX_API_KEY_ALIYUN）；读的时候优先于密钥文件

退出码：0 成功 ｜ 2 运行期失败 ｜ 3 用法错误
"
    .to_string()
}

/// 解析 argv（不含程序名）。
pub fn parse<I>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = String>,
{
    let mut config: Option<PathBuf> = None;
    let mut mode: Option<Mode> = None;
    let mut start: Option<Start> = None;
    let mut run_for: Option<Duration> = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "--config" => {
                let value = value_of(&mut args, "--config")?;
                if config.replace(PathBuf::from(&value)).is_some() {
                    return Err("--config 给了两次".to_string());
                }
            }
            "--print-capabilities" => set_mode(&mut mode, Mode::PrintCapabilities)?,
            "--print-composition" => set_mode(&mut mode, Mode::PrintComposition)?,
            "--dry-run" => set_mode(&mut mode, Mode::DryRun)?,
            "--start" => {
                let value = value_of(&mut args, "--start")?;
                let parsed = match value.as_str() {
                    "speak" => Start::Speak,
                    "listen" => Start::Listen,
                    "all" => Start::All,
                    other => {
                        return Err(format!("--start 不认识 {other}：只认 speak / listen / all"))
                    }
                };
                if start.replace(parsed).is_some() {
                    return Err("--start 给了两次".to_string());
                }
            }
            "--run-for" => {
                let value = value_of(&mut args, "--run-for")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--run-for 要的是秒数（非负整数），给的是 {value}"))?;
                // 0 = 一直跑（跟不给这个开关等价），免得"0 秒"这种写法被理解成"立刻退出"。
                run_for = (seconds > 0).then(|| Duration::from_secs(seconds));
            }
            other => return Err(format!("不认识的参数：{other}")),
        }
    }

    let mode = mode.unwrap_or(Mode::Run);
    let args = Args {
        config,
        mode,
        start: start.unwrap_or(Start::None),
        run_for,
    };

    // 一次性模式（打印/试装）不跑进程，"开哪条腿""跑多久"在那两条路上没有意义：
    // 与其静默忽略，不如说清楚——启动参数是 unit 写的，安静忽略会变成"配了没生效"。
    if mode != Mode::Run {
        if args.start != Start::None {
            return Err(
                "--start 只在一直跑的模式下有意义（跟 --print-capabilities / --print-composition \
                 / --dry-run 冲突）"
                    .to_string(),
            );
        }
        if args.run_for.is_some() {
            return Err(
                "--run-for 只在一直跑的模式下有意义（跟 --print-capabilities / --print-composition \
                 / --dry-run 冲突）"
                    .to_string(),
            );
        }
    }

    Ok(Invocation::Run(args))
}

fn set_mode(slot: &mut Option<Mode>, mode: Mode) -> Result<(), String> {
    match slot {
        Some(existing) if *existing != mode => {
            Err("--print-capabilities 与 --print-composition 与 --dry-run 只能给一个".to_string())
        }
        _ => {
            *slot = Some(mode);
            Ok(())
        }
    }
}

fn value_of(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} 后面要给一个值"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Invocation, String> {
        parse(argv.iter().map(|arg| arg.to_string()))
    }

    fn args_of(argv: &[&str]) -> Args {
        match parse_of(argv) {
            Ok(Invocation::Run(args)) => args,
            other => panic!("应该解析成 Args：{other:?}"),
        }
    }

    #[test]
    fn defaults_to_a_long_running_daemon() {
        let args = args_of(&[]);
        assert_eq!(args.mode, Mode::Run);
        assert_eq!(args.start, Start::None, "无屏缺省不擅自开腿");
        assert_eq!(args.run_for, None, "缺省一直跑（systemd 的常驻形态）");
        assert_eq!(args.config, None);
    }

    #[test]
    fn config_takes_a_file_path() {
        let args = args_of(&["--config", "/etc/vox/settings.json"]);
        assert_eq!(
            args.config.as_deref(),
            Some(std::path::Path::new("/etc/vox/settings.json"))
        );
    }

    #[test]
    fn run_for_zero_means_forever() {
        assert_eq!(args_of(&["--run-for", "0"]).run_for, None);
        assert_eq!(
            args_of(&["--run-for", "3"]).run_for,
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn start_names_are_closed() {
        assert_eq!(args_of(&["--start", "all"]).start, Start::All);
        assert_eq!(args_of(&["--start", "listen"]).start, Start::Listen);
        let error = parse_of(&["--start", "both"]).unwrap_err();
        assert!(error.contains("speak / listen / all"), "{error}");
    }

    #[test]
    fn one_shot_modes_reject_daemon_only_flags() {
        assert!(parse_of(&["--print-capabilities", "--dry-run"]).is_err());
        assert!(parse_of(&["--print-composition", "--dry-run"]).is_err());
        assert!(parse_of(&["--print-capabilities", "--print-composition"]).is_err());
        assert!(parse_of(&["--dry-run", "--start", "speak"]).is_err());
        assert!(parse_of(&["--print-capabilities", "--run-for", "1"]).is_err());
        assert!(parse_of(&["--print-composition", "--run-for", "1"]).is_err());
        // 但一次性模式本身没问题。
        assert_eq!(
            args_of(&["--print-capabilities"]).mode,
            Mode::PrintCapabilities
        );
        assert_eq!(
            args_of(&["--print-composition"]).mode,
            Mode::PrintComposition
        );
        assert_eq!(args_of(&["--dry-run"]).mode, Mode::DryRun);
        // 同一个一次性模式给两次不算冲突（幂等），三个不同的才算。
        assert_eq!(
            args_of(&["--print-composition", "--print-composition"]).mode,
            Mode::PrintComposition
        );
    }

    #[test]
    fn malformed_flags_are_errors_not_guesses() {
        assert!(parse_of(&["--config"]).is_err(), "缺值必须报错");
        assert!(parse_of(&["--config", "a", "--config", "b"]).is_err());
        assert!(parse_of(&["--start", "speak", "--start", "all"]).is_err());
        assert!(parse_of(&["--run-for", "abc"]).is_err());
        assert!(parse_of(&["--bogus"]).is_err());
        assert!(parse_of(&["extra"]).is_err());
        assert_eq!(parse_of(&["--help"]), Ok(Invocation::Help));
    }
}
