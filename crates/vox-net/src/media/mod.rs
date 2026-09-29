//! 媒体面：网络音频进出的帧层与管子（S3）。
//!
//! **与 `ws.rs` 的边界**：`ws.rs` 实现的是 `vox_core::cloud::Transport`（云端协议：文本帧 +
//! `ConnectRequest`），这个模块实现的是媒体面（**二进制音频帧 + 自带节拍**）。两者共用
//! tokio / tokio-tungstenite，但**协议、路径、凭据三样都不共用**（设计稿 §2.1.4、§2.5.1）：
//! `Transport` 的契约一个字没动。
//!
//! # 协议（v1）
//!
//! 裸 WebSocket（`ws://` / `wss://`）+ 20 字节定长头的二进制帧，载荷 PCM16LE 单声道。
//! 逐字节布局见 [`frame`]。抖动缓冲 / 重切块 / 欠载补静音 / 过载丢最旧见 [`pipe`]。
//!
//! - 入站：[`MediaListener`] 绑地址（`net_in` 的定义者）→ [`CaptureSource`] 给腿用。
//! - 出站：[`MediaOut`] 起池（`net_out` 的定义者）→ [`PlaybackSink`] 给腿用。
//!
//! [`CaptureSource`]: vox_core::ports::CaptureSource
//! [`PlaybackSink`]: vox_core::ports::PlaybackSink
//!
//! # 凭据与 Origin
//!
//! 两条通道（都不是 URL query）：`Authorization: Bearer <token>`（非浏览器），
//! `Sec-WebSocket-Protocol: vox.media.v1.<token>`（浏览器只能设子协议）。
//! 带 `Origin` 的握手必须逐字命中 `MediaOptions::allowed_origins`，空白名单 = 拒一切
//! （任意网页都能向 `localhost` 开 WebSocket，"同机"不等于"可信"）。

pub mod frame;
pub mod pipe;

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

mod client;
mod server;

pub use client::MediaOut;
pub use server::{MediaListener, MediaOptions, PATH, SUBPROTOCOL_PREFIX};

/// v1 只有一个逻辑管道名。`pipe` 是**装配**（名字），地址是**环境**（配置面 + `media.json`）。
pub const DEFAULT_PIPE: &str = "default";

/// 媒体面的 WS 参数（进站与出站共用一套）。
///
/// tungstenite 的缺省是 64 MiB 消息上限 + 128 KiB 读缓冲——对着一个**监听在局域网上**
/// 的端口，那等于把"先让我吃 64 MiB 再判协议错"送给任何连得上的人（S3 的目标还是小主板）。
/// 这里收紧到两倍帧长：**真正的 8 KiB 载荷上限在 `frame::decode` 里**，留一倍余量是为了
/// 让"超长"那条错由我们自己数进 `bad_frames`，而不是被 WS 层提前吃掉。
pub(crate) fn ws_config() -> WebSocketConfig {
    let bound = 2 * (frame::HEADER_LEN + frame::MAX_PAYLOAD);
    WebSocketConfig::default()
        .read_buffer_size(4 * 1024)
        .max_message_size(Some(bound))
        .max_frame_size(Some(bound))
}
