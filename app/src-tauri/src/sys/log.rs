//! tracing 初始化。
//!
//! 只往 stderr 写。发布构建是 `windows_subsystem = "windows"`，没有控制台，
//! 这些输出会被丢掉——不落文件是刻意的：日志里难免带上识别出来的原话，
//! 那是用户说的话，不该在磁盘上留一份。要排查问题就跑调试构建。
//!
//! `with_writer(std::io::stderr)` 是**必须写的**：`fmt()` 的缺省落点是 **stdout**，
//! 而 `--print-composition`（`composition.rs`）把 stdout 当**数据出口**（一份 JSON）。
//! 日志混进 stdout 会让 `vox --print-composition | jq …` 直接解析不了——
//! 那条命令是 S0 §4.3-A 的验收读法。写完这一行之后：stdout 只有数据、stderr 只有日志，
//! 跟无屏档（`vox-headless/src/sys/log.rs`）逐字同一条口径。

use tracing_subscriber::EnvFilter;

pub fn init() {
    let filter = EnvFilter::try_from_env("VOX_LOG")
        .unwrap_or_else(|_| EnvFilter::new("vox_lib=debug,vox_=debug,warn"));
    // 重复初始化不算错（测试里可能已经装过一个）。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}
