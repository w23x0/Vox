//! tracing 初始化。**只往 stderr 写**：systemd 直接把它收进 journal（`docs/platform/EMBEDDED.md`
//! §3.5-②的第 1 档，零改动就能用）。
//!
//! 两条跟桌面档的差别（都是无屏档的性质决定的）：
//!
//! - **默认更安静**：桌面档缺省 `vox_=debug`（有界面在跑，日志多也无所谓）；无屏设备常年
//!   无人值守，缺省只打本 crate 的 `info`，其余一律 `warn`。要看细节就 `VOX_LOG=...`。
//! - **不落文件**：跟桌面档同一条理由——日志里难免带上识别出来的原话，那是用户说的话，
//!   不该在磁盘上再留一份（journal 是 systemd 的，看得到、也能配限额）。

use tracing_subscriber::EnvFilter;

/// 缺省过滤器。只开本 crate 的 info：芯的 debug 太吵（每个音频块都可能打一行），
/// 而"流水线阶段变了 / 连不上云端"这些真正要看的东西在本 crate 的 `status::wire` 里。
const DEFAULT_FILTER: &str = "vox_headless=info,warn";

pub fn init() {
    let filter =
        EnvFilter::try_from_env("VOX_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    // 重复初始化不算错（测试里可能已经装过一个）。
    //
    // `with_writer(std::io::stderr)` 是**必须写的**：`fmt()` 的缺省落点是 **stdout**，
    // 而这个进程的 stdout 是**数据出口**（`--print-capabilities` / `--dry-run` 打 JSON）。
    // 日志混进 stdout 会把那条出口弄成"日志 + JSON 混排"，谁也没法解析它。
    // 写到 stderr 之后：stdout 只有数据，stderr 只有日志，systemd 两路都能收。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        // 装两次不该 panic（第二个 subscriber 装不上就丢掉）。
        init();
        init();
    }
}
