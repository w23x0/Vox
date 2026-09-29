//! 无屏档（小主板 ARM64 Linux）入口的 `main`。
//!
//! 薄得像一张纸：解析参数 → 装日志 → 交给 [`vox_headless::run`]。业务一行都没有——
//! 装配在 `headless.rs`，平台在 `platform/`，控制面在 `mcp.rs`。

use std::process::ExitCode;

use vox_headless::cli::{self, Invocation};

/// 退出码：`0` 成功 ｜ `2` 运行期失败（装配不起来、控制面起不来之类）｜ `3` 用法错误。
fn main() -> ExitCode {
    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(Invocation::Help) => {
            print!("{}", cli::usage());
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Run(args)) => args,
        Err(error) => {
            eprintln!("{error}\n\n{}", cli::usage());
            return ExitCode::from(3);
        }
    };

    // 日志**在装配之前**装好：`Paths::ensure_dir` 里的目录警告也要能被看见。
    vox_headless::sys::log::init();

    match vox_headless::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 无屏设备上没人守着终端：systemd 收走这一行，退出状态也说清了"没起来"。
            tracing::error!("{error}");
            ExitCode::from(2)
        }
    }
}
