//! VRChat OpenSoundControl (OSC) 发送。
//!
//! 把一条 VRChat OSC 消息拼成线路帧，用 UDP 发到 `127.0.0.1:<port>`。VRChat 的
//! OSC 收端口默认 **9000**。本模块只写库不读：发聊天框、写头像参数——
//! 把译文写回 VRChat 聊天框。
//!
//! 纯 `std` 实现，只依赖 `std::net::UdpSocket`，不引任何第三方依赖。

// 这里原来挂着 `#![cfg(windows)]`，但整个 crate 一行 Win32 都没有：VRChat 的 OSC
// 就是往 127.0.0.1:9000 发 UDP，Linux 上一样能用。

use std::net::Ipv4Addr;
use std::net::UdpSocket;

/// 一条 OSC 参数。布尔在 OSC 里用类型标签 `T`/`F` 表达，不占数据段。
#[derive(Debug, Clone, PartialEq, Eq)]
enum OscValue {
    String(String),
    Boolean(bool),
}

/// VRChat OSC 客户端：一个 UDP 套接字 + 发送端口 + 运行期配置。
///
/// 绑定到 `127.0.0.1:0`（随机本地口）并设为非阻塞；`send` 只发不等待回环。
///
/// `chat_enabled` 控制事件桥是否自动把译文写进聊天框，`avatar_param` 是
/// [`Self::set_avatar_bool`] 用的头像参数名——这两项是装配层（`app/src-tauri`）
/// 存进同一个槽、由事件桥 fast path 读走的状态。
pub struct OscClient {
    udp: UdpSocket,
    port: u16,
    /// 自动把译文发进 VRChat 聊天框的开关。
    chat_enabled: bool,
    /// `set_avatar_bool` 落笔的头像参数名。
    avatar_param: String,
    /// 是否随「对外说话流水线在跑」自动亮头像指示灯。
    avatar_enabled: bool,
}

impl OscClient {
    /// 建一个 客户端，绑到同机随机本地口、设非阻塞。`port` 是 VRChat 监听的 OSC 端口
    /// （VRChat 官方默认 9000）。
    pub fn new(port: u16) -> Result<Self, String> {
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("failed to bind the local UDP socket: {e}"))?;
        udp.set_nonblocking(true)
            .map_err(|e| format!("failed to set the UDP socket non-blocking: {e}"))?;
        Ok(Self {
            udp,
            port,
            chat_enabled: true,
            avatar_param: String::new(),
            avatar_enabled: false,
        })
    }

    /// 聊天框自动发送开关。
    pub fn set_chat_enabled(&mut self, enabled: bool) {
        self.chat_enabled = enabled;
    }

    /// 聊天框自动发送是否打开。
    pub fn chat_enabled(&self) -> bool {
        self.chat_enabled
    }

    /// 设置 `set_avatar_bool` 用的头像参数名。
    pub fn set_avatar_param(&mut self, param: impl Into<String>) {
        self.avatar_param = param.into();
    }

    /// `set_avatar_bool` 用的头像参数名。
    pub fn avatar_param(&self) -> &str {
        &self.avatar_param
    }

    /// 是否自动依据「对外说话在跑」点亮头像指示灯。
    pub fn set_avatar_enabled(&mut self, enabled: bool) {
        self.avatar_enabled = enabled;
    }

    /// 头像指示灯自动点亮是否打开。
    pub fn avatar_enabled(&self) -> bool {
        self.avatar_enabled
    }

    /// 发一条任意 OSC 消息（address + 参数列表）到目标端口。
    fn send(&self, address: &str, args: &[OscValue]) -> Result<(), String> {
        let frame = build(address, args);
        self.udp
            .send_to(&frame, (Ipv4Addr::LOCALHOST, self.port))
            .map(|_| ())
            .map_err(|e| format!("failed to send OSC to 127.0.0.1:{}: {e}", self.port))
    }

    /// 发 `/chatbox/input` 把一段文字写进 VRChat 聊天框。
    ///
    /// 帧参数是 `String(text)` + `Boolean(immediate)`。`immediate=true` 直接把文字
    /// 作为一条聊天消息发出去（VRChat 真发、不经过输入框）；`immediate=false` 则
    /// 是弹起输入框并用这段文字预填，等用户自己按回车。这里不做翻转——传什么就
    /// 发给 VRChat 什么。
    pub fn chatbox(&self, text: &str, immediate: bool) -> Result<(), String> {
        self.send(
            "/chatbox/input",
            &[
                OscValue::String(text.to_string()),
                OscValue::Boolean(immediate),
            ],
        )
    }

    /// 写一个头像参数：`/avatar/parameters/<param>` + `Boolean(on)`。
    pub fn set_avatar_bool(&self, param: &str, on: bool) -> Result<(), String> {
        let address = format!("/avatar/parameters/{param}");
        self.send(&address, &[OscValue::Boolean(on)])
    }
}

/// 拼一条 OSC 线路帧。
///
/// 布局：`<地址>\0凑4` + `,<类型标签>\0凑4` + 各参数数据段。
/// - 地址 NUL 结尾、补到 4 的倍数；
/// - 类型标签串以逗号开头，逐个参数打字符：`s`=String、`T`/`F`=true/false；
/// - String 数据 NUL 结尾、补 4 对齐；
/// - Boolean 只打标签（`T`/`F`），不占数据段。
fn build(address: &str, args: &[OscValue]) -> Vec<u8> {
    let mut frame = Vec::new();

    // ① 地址：NUL 结尾 + 4 对齐。
    push_osc_string(&mut frame, address);

    // ② 类型标签：逗号开头，每个参数一个字符，NUL 结尾 + 4 对齐。
    frame.push(b',');
    for arg in args {
        match arg {
            OscValue::String(_) => frame.push(b's'),
            OscValue::Boolean(true) => frame.push(b'T'),
            OscValue::Boolean(false) => frame.push(b'F'),
        }
    }
    frame.push(0);
    pad4(&mut frame);

    // ③ 数据段：string 依次排入（顺序与类型标签一致），bool 不占数据段。
    for arg in args {
        if let OscValue::String(s) = arg {
            push_osc_string(&mut frame, s);
        }
    }

    frame
}

/// 写入一个 OSC string 段：内容 + NUL 结尾 + 补零到 4 的倍数。
fn push_osc_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    pad4(buf);
}

/// 补零到 4 的倍数。
fn pad4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/chatbox/input` 文本帧的字节布局（`immediate=false` → 标签 `sF`）。
    #[test]
    fn chatbox_text_frame_layout() {
        // 参数是 String("Hello") + Boolean(false)（标签 `sF`）。
        let args = vec![
            OscValue::String("Hello".to_string()),
            OscValue::Boolean(false),
        ];
        let frame = build("/chatbox/input", &args);

        // ① 地址段：`/chatbox/input\0` → 14 字节，补到 16。
        assert_eq!(&frame[..16], b"/chatbox/input\0\0");
        // ② 类型标签段从 16 起：`,sF\0` 补到…… 实际 4 字节整。
        assert_eq!(&frame[16..20], b",sF\0");
        // ③ "Hello\0" 从 20 起，补到 28。
        assert_eq!(&frame[20..26], b"Hello\0");
        // 总长 28，对齐 4。
        assert_eq!(frame.len(), 28);
        assert_eq!(frame.len() % 4, 0);
    }

    /// 一个纯 bool 帧（头像参数开）。
    #[test]
    fn bool_frame_layout() {
        let frame = build("/avatar/parameters/VoxFeel", &[OscValue::Boolean(true)]);

        // 地址 `/avatar/parameters/VoxFeel` 26 字节 + NUL = 27，补 1 → 28。
        assert_eq!(&frame[..27], b"/avatar/parameters/VoxFeel\0");
        assert_eq!(frame[27], 0, "地址段补到 4 的倍数（27→28）");
        // 类型标签 `,T\0\0`。
        assert_eq!(&frame[28..32], b",T\0\0");
        // 无数据段，总长 32。
        assert_eq!(frame.len(), 32);
        assert_eq!(frame.len() % 4, 0);
    }

    /// `chatbox(text, immediate)` 把第二个布尔**原样**写进帧（不取反）：
    /// `immediate=true` → 标签 `,sT`，VRChat 直接当作聊天消息发出去。
    /// 与 `chatbox_text_frame_layout` 里的 `,sF`（`false`=弹输入框预填）成对，
    /// 合起来覆盖 VRChat `/chatbox/input` 的两档语义。
    #[test]
    fn chatbox_true_encodes_boolean_true() {
        let args = vec![
            OscValue::String("hi".to_string()),
            OscValue::Boolean(true), // chatbox(text, immediate=true)
        ];
        let frame = build("/chatbox/input", &args);
        // 地址段 "/chatbox/input\0"=14→补到16；标签段 `,sT\0`。
        assert_eq!(&frame[..16], b"/chatbox/input\0\0");
        assert_eq!(&frame[16..20], b",sT\0");
    }
}
