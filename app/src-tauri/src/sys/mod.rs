//! 跟操作系统直接打交道的小件。
//!
//! 只有 `log` 是跨平台的（写 stderr / 日志文件）；另外两个是 Windows 专属，
//! Linux 的对应物在 `platform/linux/`（时钟）与 `platform::alert`（致命提示）。
//!
//! **密钥库不在这里**：DPAPI / Secret Service / 0600 文件三个后端与"按平台挑一个"的
//! 决策都在 `vox_host::secrets`（S4-A W3），本 crate 只在 `platform::{win,linux}` 里
//! 各留一句"这一档选哪个"。

pub mod log;

#[cfg(windows)]
pub mod clock;
#[cfg(windows)]
pub mod fatal;
