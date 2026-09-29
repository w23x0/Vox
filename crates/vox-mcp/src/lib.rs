//! Vox 控制面出口：动作清单 → MCP 协议面 + `voxctl`。
//!
//! **唯一真源**是 [`actions::ACTIONS`]：工具名、入/出参 schema、权限要求、是否长任务全写在那张
//! 数据表里。`tools/list`、`tools/call` 的参数校验、CLI 子命令都从它推导——清单是数据不是分支，
//! 也没有"万能 execute"后门。
//!
//! 分层（谁不知道谁）：
//!
//! ```text
//! jsonrpc    帧与规范错误码     —— 不认方法、不认账本
//! mcp        方法分派 + _meta   —— 认方法、认缓存定值，不认账本
//! transport  本机 HTTP 传输     —— 认 HTTP 帧、认 token，协议语义一行都不碰（SSE 只搬字节）
//! client     连本机 HTTP 的瘦客户端 —— CLI 与 stdio 桥共用；不实现协议语义、不碰账本
//! handlers   动作 → 后端         —— 唯一碰账本/设备的地方（后端由外壳注入）
//! session    会话 handle + token —— 认生命周期，投影与协议都不认
//! resources  字幕资源的形状     —— URI / 条目 / 快照 / "谁变了"，不认协议也不认账本
//! transcript 字幕变更检测器     —— 只认"上次发出去的样子"，不认账本也不认协议
//! endpoints  端点投影 + 清单文档 + 反方向表 —— 只认账本（`Ledger` 端口）与芯的清单类型
//! ledger     账本 + 授权端口     —— 唯一读账本/写账本/读授权位（`Runtime` 是唯一真实现）
//! ```
//!
//! 已经落地的：协议面（`server/discover` / `tools/list` / `tools/call`）+ 本机 Streamable HTTP
//! 传输（[`transport::http`]，只绑 `127.0.0.1`、单路径 `/mcp`、POST-only，外加
//! `subscriptions/listen` 的 SSE 长流）+ **端点投影**（[`endpoints`]：`Settings` + `HostFacts`
//! → `Composition`，含反方向表与四步校验链）+ **资源面**（[`resources`] + [`transcript`]：
//! `resources/list` / `resources/read` / `subscriptions/listen`，字幕走
//! `vox://session/<handle>/transcript`）+ **默认后端**（[`session::LedgerBackend`]，基于 vox-core
//! 账本）+ **两个客户端出口**（[`client`]：`voxctl` 的 5 个动作子命令；[`transport::stdio`]：
//! `voxctl serve-stdio` 的 stdio ↔ 本机 HTTP 桥）。三者都**不拥有账本**——账本只有装配层
//! （桌面 / 无屏档）注入的那一个。
//! 没落地的东西**也不广告**：`server/discover` 的 `capabilities` 里只有 `tools` 与 `resources`
//! 两位（`docs/plans/S1-AGENT-FACE.md` §2.3.3）——stdio 桥与 CLI 都只是同一条 HTTP 面的别的
//! 人口，**一个能力位都不为它们加**。
//!
//! 规矩（`.omp/RULES.md` + 设计稿）：
//!
//! - **音频永不进协议**：实时音频走媒体面自己的通道；字幕这类文本走 `resources` + 通知。
//! - **规范以 2026-07-28 为准**：无状态、每请求 `_meta` 带版本与能力、`resultType` 必填、
//!   不用 sampling / roots / logging、长任务用 Tasks 扩展（v1 不声明）。
//! - **零新依赖树**：只依赖 `serde` / `serde_json` / `base64`（token 编码）/ `getrandom`
//!   （随机源）与 workspace 内的 `vox-core`（清单类型 + 账本）。四个第三方包都已在
//!   `Cargo.lock` 里，一个包都不新增。
//! - **CLI 与 HTTP 用同一个 [`handle`]**：传输层只把 JSON-RPC 消息原样递过去，所以两个出口
//!   出来的字节必然相同（`voxctl --probe` 与 HTTP 面在 `tests/http.rs` 里逐字节对比）。

pub mod actions;
pub mod client;
pub mod endpoints;
pub mod handlers;
pub mod jsonrpc;
pub mod ledger;
pub mod mcp;
pub mod resources;
pub mod session;
pub mod transcript;
pub mod transport;

pub use actions::{
    Action, ActionId, DomainError, DomainErrorCode, Duration, EndpointId, Permission, ACTIONS,
};
pub use endpoints::{Endpoint, ENDPOINTS};
pub use handlers::{ActionCall, BoxedBackend, CallFailure, ControlBackend};
pub use ledger::{Denied, Grants, Ledger};
pub use mcp::{handle, Answer};
pub use session::LedgerBackend;
pub use transport::http::{serve, ServerHandle, ServerOptions};
