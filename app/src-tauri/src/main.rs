// 不带控制台窗口发布；调试构建保留控制台，方便看 tracing 输出。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    vox_lib::run()
}
