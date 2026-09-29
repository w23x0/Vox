//! 传输面：把**同一条** [`crate::handle`] 接到线上。
//!
//! 分工（设计稿 §2.2.1 的传输矩阵）：控制面的**主通道**是本机 Streamable HTTP（只绑
//! `127.0.0.1`、单路径 `/mcp`、POST-only）；stdio 桥（`voxctl serve-stdio`）是桌面宿主的
//! 另一种绑定，它只是"stdio ↔ 本机 HTTP"的转发器——**不拥有账本、不开设备**（第二个
//! Vox 实例会抢声卡）。
//!
//! 这一层只做三件事：解析 HTTP 帧、校验请求头与 token、把 JSON-RPC 消息原样交给
//! [`crate::handle`]。协议语义（方法、`_meta`、错误码、缓存提示）一行都不在这里——所以
//! CLI 的 `--probe` 与 HTTP 面出来的字节必然一致：它们是**同一个函数**对同一份输入的输出。
//!
//! 为什么手写阻塞实现、不用 hyper/axum（设计稿 §2.6）：
//!
//! - 本版规范把服务端压成"单路径 + 只认 POST + 无 session + 无 GET + 无 SSE 续传"
//!   （§2.3.1 第 10 条），路由与帧解析加起来比接线代码短；
//! - [`ControlBackend`](crate::handlers::ControlBackend) 的契约本身就是**同步阻塞**的
//!   （"本 crate 零 async，与芯没有 async 的口径一致"），异步运行时只会逼出一层
//!   `spawn_blocking` 包装；
//! - 零新依赖：`std::net` + 每连接一个线程就够，`Cargo.lock` 一个包都不加。
//!
//! 两个子模块的分工：`http` 是**服务端**（也是握手文件的格式所有者），`stdio` 是**客户端那一侧
//! 的桥**（换行分隔的 JSON-RPC ⇄ 本机 HTTP 转发，`voxctl serve-stdio` 起它）。桥用的 HTTP
//! 客户端在 [`crate::client`]——与 CLI 的动作子命令是同一条瘦客户端路径，不写第二份。
pub mod http;
pub mod stdio;
