//! 出站代理：一份实现，供 WebSocket 传输（`vox-net`）与 HTTP 列表 / 测速（`vox-host`）共用。
//!
//! - [`ProxySpec`]：`http://` / `socks5://` / `socks5h://`（可带 `user:pass@`）的解析。
//!   `Debug` 与 [`ProxySpec::display`] 都**不含密码**。
//! - [`resolve`]：把设置里的 [`NetworkSetting`]（直连 / 系统 / 自定义）落成具体代理或"直连"。
//! - [`system`]：系统代理探测（Linux/macOS 读环境变量，Windows 读 `Internet Settings` 注册表）。
//! - [`dial`]：同步拨号——直连或经代理隧道，分阶段计时、分阶段报错。
//!
//! 纯 `std`、同步。异步调用方（`vox-net`）把 [`dial`] 放进 `spawn_blocking`，拿到连好的
//! `TcpStream` 后转成 tokio 的流；TLS 永远在隧道**之后**由调用方自己套。

mod dial;
mod spec;
pub mod system;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use vox_core::settings::{NetworkMode, NetworkSetting};

pub use dial::{dial, DialError, Dialed, Stage};
pub use spec::{ProxyError, ProxyScheme, ProxySpec};

/// 设置 → 对 `target_host` 该用的代理。`Ok(None)` = 直连。
///
/// - `Direct`：直连。
/// - `System`：读系统代理；读不到、或目标在不走代理名单里就直连；回环地址恒直连。
/// - `Custom`：用 `proxy` 里填的；**格式不合法是错误**（不静默改成直连——那会让用户以为
///   走了代理实际暴露了真实出口）。空字符串同样是错误。
pub fn resolve(
    setting: &NetworkSetting,
    target_host: &str,
) -> Result<Option<ProxySpec>, ProxyError> {
    match setting.mode {
        NetworkMode::Direct => Ok(None),
        NetworkMode::System => Ok(system::detect(target_host)),
        NetworkMode::Custom => ProxySpec::parse(&setting.proxy).map(Some),
    }
}
