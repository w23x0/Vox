//! channel-based 同步↔异步桥接 WebSocket 传输层。
//!
//! 架构：`connect()` 时在 tokio runtime 上 spawn 一个读循环任务（`reader_task`），
//! 它持有 `WebSocketStream` 的读半边，把收到的帧通过 `mpsc` 推给主线程；
//! 写半边留在主线程侧通过 `block_on` 发送（写不需要超时，且 DashScope 不会
//! 背压到阻塞写的地步）。`recv` 用 `recv_timeout` 等 channel，超时时连接不受影响。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use futures_util::SinkExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use vox_core::cloud::{ConnectRequest, Incoming, Transport};
use vox_core::ports::{PortError, PortResult};
use vox_core::settings::NetworkSetting;

/// 套在 **TLS 之上**（明文一侧）、tungstenite 之下的那层：只读方向拦超长 close 帧
/// （见 [`CloseCatcher`]）。放在 TLS 之下看到的是密文，会把握手字节当 WS 帧去改。
type WsStream = WebSocketStream<CloseCatcher<Wire>>;
type WsSink = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;

/// 下行缓冲上限。512 条足够容纳数秒突发译音，同时能阻止断开的 UI/工作线程
/// 让无界队列一直吃内存。满了时读任务自然向 TCP 施加背压。
const READER_QUEUE_SIZE: usize = 512;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// TCP 拨号（含代理握手）的预算。
const DIAL_BUDGET: Duration = Duration::from_secs(25);

/// 一次读进旁路的字节数上限。服务端的音频/文字帧远小于它，close 帧也就 100 多字节。
const SPY_BUFFER: usize = 16 * 1024;

/// RFC 6455 的控制帧负载上限。超了 tungstenite 直接报 `Control frame too big`。
const MAX_CONTROL_PAYLOAD: usize = 125;
/// close 帧的负载前 2 字节是状态码。
const CLOSE_CODE_LEN: usize = 2;
/// 还没拼出完整帧的字节上限。真音频帧远小于它（24 kHz PCM16 一秒 ≈ 48 KB）；
/// 超过就说明对端报了个荒谬的长度，交给 tungstenite 报错而不是继续喂内存。
const MAX_PENDING: usize = 8 * 1024 * 1024;

/// 读循环向主线程投递的消息。
enum ReaderMsg {
    /// 一条文本帧（或从 binary 帧 lossy 解码的文本）。
    Text(String),
    /// 对端发了 close frame。
    Closed(String),
    /// 读循环自己炸了（网络断了之类）。
    Error(String),
}

/// 连接建立后的活跃状态。
struct ActiveConn {
    write: WsSink,
    rx: mpsc::Receiver<ReaderMsg>,
    shutdown: CancellationToken,
}

/// `vox_core::cloud::Transport` 的 WebSocket 实现。
///
/// 线程安全：本身是 `Send`（trait 要求），但不是 `Sync`——同一时刻只有一个
/// 流水线线程在用它。
pub struct WsTransport {
    handle: Handle,
    /// 自己起的 runtime（standalone 模式）。持有它只为保活，不直接用。
    _owned_rt: Option<Runtime>,
    conn: Option<ActiveConn>,
    /// `wss://` 的证书信任配置。生产恒为 webpki 内置根；测试可注入自签根。
    tls: Arc<rustls::ClientConfig>,
}

impl WsTransport {
    /// 复用已有 runtime（Tauri 场景下 app 已经有一个在跑的 tokio runtime）。
    pub fn new(handle: Handle) -> Self {
        Self::with_runtime(handle, None)
    }

    /// 自己起一个小 runtime，适合测试和 CLI 场景。
    pub fn standalone() -> PortResult<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| PortError::new(format!("无法启动网络运行时：{e}")))?;
        let handle = rt.handle().clone();
        Ok(Self::with_runtime(handle, Some(rt)))
    }

    /// 两条构造路径的唯一收束点：装 CryptoProvider（幂等）、记下 runtime 句柄。
    fn with_runtime(handle: Handle, owned_rt: Option<Runtime>) -> Self {
        ensure_crypto_provider();
        Self {
            handle,
            _owned_rt: owned_rt,
            conn: None,
            tls: default_tls_config(),
        }
    }

    /// 只给测试：换掉信任的根证书，好对着本地自签的 TLS 服务端连。
    #[cfg(test)]
    fn with_tls_config(mut self, tls: Arc<rustls::ClientConfig>) -> Self {
        self.tls = tls;
        self
    }

    /// 如果还有连接就关掉，为下一次 `connect` 腾地方。
    fn teardown(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.shutdown.cancel();
            // 尝试发 close frame，但不等太久——对端可能已经消失了。
            let sink = conn.write;
            let handle = self.handle.clone();
            // spawn 是 fire-and-forget，不需要 await JoinHandle。
            drop(handle.spawn(async move {
                let mut sink = sink;
                let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
            }));
        }
    }
}

impl Transport for WsTransport {
    fn connect(&mut self, request: &ConnectRequest) -> PortResult<()> {
        // 先关掉旧连接（如果有的话）。
        self.teardown();

        // 构造带 Authorization 头的 HTTP 请求——直接传 URL 字符串会丢自定义头，
        // 必须用 tungstenite 的 Request builder。（这是个常见坑）
        let mut req = request
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| PortError::new(format!("URL 格式错误：{e}")))?;
        if !request.auth_header.is_empty() {
            req.headers_mut().insert(
                "Authorization",
                request
                    .auth_header
                    .parse()
                    .map_err(|e| PortError::new(format!("鉴权头格式错误：{e}")))?,
            );
        }

        let safe_url = request.url.split("?key=").next().unwrap_or(&request.url);
        debug!(url = %safe_url, "正在连接 WebSocket");

        let caught = close_reason_slot();

        // 在 runtime 上执行握手，阻塞当前线程等结果。
        // 30 秒连接超时——DNS 解析 + TCP 三次握手 + TLS 协商 + HTTP 升级。
        let stream: WsStream = block_on_timeout(&self.handle, Duration::from_secs(30), async {
            connect_with_close_catcher(
                req,
                caught.clone(),
                self.tls.clone(),
                request.network.clone(),
            )
            .await
        })
        .ok_or_else(|| PortError::new("连接超时（30 秒内未完成握手）"))?
        .map_err(map_connect_error)?
        .0;

        // 实时音频是频繁的小帧；关闭 Nagle，避免小包在等待 ACK 时平白多挨一拍。
        if let Err(error) = stream.get_ref().inner().tcp().set_nodelay(true) {
            debug!(%error, "TCP_NODELAY 设置失败，继续使用系统默认行为");
        }

        let (write, read) = stream.split();

        let (tx, rx) = mpsc::channel(READER_QUEUE_SIZE);
        let shutdown = CancellationToken::new();

        // spawn 读循环。
        let token = shutdown.clone();
        self.handle.spawn(reader_task(read, tx, token, caught));

        self.conn = Some(ActiveConn {
            write,
            rx,
            shutdown,
        });

        debug!("WebSocket 握手成功");
        Ok(())
    }

    fn send(&mut self, text: &str) -> PortResult<()> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| PortError::new("发送失败：连接未建立"))?;

        let msg = Message::Text(text.to_owned().into());
        block_on_timeout(&self.handle, WRITE_TIMEOUT, conn.write.send(msg))
            .ok_or_else(|| PortError::new("发送超时（5 秒内 socket 未能写出）"))?
            .map_err(|e| PortError::new(format!("发送失败：{e}")))?;
        Ok(())
    }

    fn recv(&mut self, timeout_ms: u32) -> PortResult<Option<Incoming>> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| PortError::new("接收失败：连接未建立"))?;

        if timeout_ms == 0 {
            return match conn.rx.try_recv() {
                Ok(msg) => Ok(Some(reader_msg(msg))),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => Ok(Some(Incoming::Closed(
                    "连接意外断开（读循环已退出）".to_owned(),
                ))),
            };
        }

        // 用 tokio 的 timeout 等 channel——超时时 channel 和读循环都不受影响。
        let dur = Duration::from_millis(timeout_ms as u64);
        match block_on_timeout(&self.handle, dur, conn.rx.recv()) {
            // 超时——安静路径，连接还活着。
            None => Ok(None),
            // channel 关了（读循环退出了但没来得及发 Error/Closed）。
            Some(None) => Ok(Some(Incoming::Closed(
                "连接意外断开（读循环已退出）".to_owned(),
            ))),
            Some(Some(msg)) => Ok(Some(reader_msg(msg))),
        }
    }

    fn close(&mut self) {
        self.teardown();
    }
}

/// 在 runtime 上阻塞等一个 future，限时 `limit`：超时返回 `None`。
///
/// 握手、写、读三处都是同一个形状（`block_on` + `tokio::time::timeout`）；
/// 超时不算连接出问题，错误分类由各调用点自己映射。
fn block_on_timeout<T>(
    handle: &Handle,
    limit: Duration,
    fut: impl Future<Output = T>,
) -> Option<T> {
    handle.block_on(async { tokio::time::timeout(limit, fut).await.ok() })
}

/// 自己建连（DNS + TCP + TLS + 升级），在 **TLS 之上** 插一层 [`CloseCatcher`]。
///
/// 不走 `connect_async` 的原因只有一个：那一层没有地方插我们自己的字节旁路，
/// 而超长 close 帧必须在**进 tungstenite 之前**处理（见 [`CloseCatcher`]）。
/// 层次：TCP → TLS（`wss` 才有）→ `CloseCatcher`（明文）→ WS 握手与帧。
/// `tls` 是证书信任配置，生产用 [`default_tls_config`]，测试注入自签根。
async fn connect_with_close_catcher(
    request: tungstenite::handshake::client::Request,
    caught: CaughtCloseSlot,
    tls: Arc<rustls::ClientConfig>,
    network: NetworkSetting,
) -> Result<(WsStream, tungstenite::handshake::client::Response), tungstenite::Error> {
    use tungstenite::error::UrlError;

    let uri = request.uri();
    let host = uri
        .host()
        .ok_or(tungstenite::Error::Url(UrlError::NoHostName))?
        .to_string();
    let secure = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => return Err(tungstenite::Error::Url(UrlError::UnsupportedUrlScheme)),
    };
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });

    let socket = dial_tcp(&network, host.clone(), port)
        .await
        .map_err(tungstenite::Error::Io)?;
    let wire = if secure {
        let name = rustls::pki_types::ServerName::try_from(host)
            .map_err(|_| tungstenite::Error::Url(UrlError::NoHostName))?;
        let stream = tokio_rustls::TlsConnector::from(tls)
            .connect(name, socket)
            .await
            .map_err(tungstenite::Error::Io)?;
        Wire::Tls(Box::new(stream))
    } else {
        Wire::Plain(socket)
    };
    tokio_tungstenite::client_async_with_config(request, CloseCatcher::new(wire, caught), None)
        .await
}

/// TCP 拨号：按网络设置直连或经代理隧道（HTTP CONNECT / SOCKS5），TLS 由调用方在其上套。
///
/// 拨号本身是同步的（`vox-proxy` 与测速、HTTP 列表共用一份实现），放进 `spawn_blocking`，
/// 握手完成后把已连好的流交给 tokio。整个连接另有 30 秒总超时，这里的预算略小一点，
/// 让"哪一段慢"的错误先于总超时出来。
async fn dial_tcp(
    network: &NetworkSetting,
    host: String,
    port: u16,
) -> std::io::Result<tokio::net::TcpStream> {
    let proxy = vox_proxy::resolve(network, &host)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    if let Some(proxy) = &proxy {
        debug!(proxy = %proxy.display(), "经代理连接");
    }
    let dialed = tokio::task::spawn_blocking(move || {
        vox_proxy::dial(proxy.as_ref(), &host, port, DIAL_BUDGET)
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(|e| {
        let kind = if e.timed_out {
            std::io::ErrorKind::TimedOut
        } else {
            std::io::ErrorKind::Other
        };
        std::io::Error::new(kind, e.message)
    })?;
    dialed.stream.set_nonblocking(true)?;
    tokio::net::TcpStream::from_std(dialed.stream)
}

/// 生产用的 TLS 信任配置：webpki 内置根证书。
fn default_tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// TCP 之上的明文/TLS 二选一。`CloseCatcher` 包的是它——也就是 TLS 解出来的明文。
enum Wire {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
}

impl Wire {
    /// 最底下的 TCP 连接（设置 TCP 选项用）。
    fn tcp(&self) -> &tokio::net::TcpStream {
        match self {
            Wire::Plain(tcp) => tcp,
            Wire::Tls(tls) => tls.get_ref().0,
        }
    }
}

impl AsyncRead for Wire {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Wire::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Wire::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Wire {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            Wire::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Wire::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Wire::Plain(s) => Pin::new(s).poll_flush(cx),
            Wire::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Wire::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Wire::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

// --- 超长 close 帧 ----------------------------------------------------------

/// 被旁路截下来的那一条 close 帧（**原文**，未截断）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CaughtClose {
    code: Option<u16>,
    reason: String,
}

/// 旁路与读循环之间的共享槽：读循环在收到 `Message::Close` 时来问"刚才截到的那条
/// 原文是什么"。两者跑在同一个任务里，用 `Mutex` 只是为了能跨 `AsyncRead` 的边界传。
type CaughtCloseSlot = Arc<Mutex<Option<CaughtClose>>>;

fn close_reason_slot() -> CaughtCloseSlot {
    Arc::new(Mutex::new(None))
}

/// TLS 之上（明文一侧）的一层**只读旁路**：认出超长的 close 帧，把它截到 RFC 上限之内再交给
/// tungstenite，同时把原文另存给上层。
///
/// 为什么必须有：百炼在模型额度用尽时会发一条负载 182 字节的 close 帧（code 1007），
/// 而 RFC 6455 规定控制帧负载不超过 125 字节——tungstenite 忠实地报
/// `Control frame too big`，用户只看到一个看不懂的协议错误，真正的原因一个字都看不到
/// （见 `docs/plans/MODEL-TABLE.md` §1.3）。
///
/// 写方向原样透传：我们发出的字节（含 `Authorization`）不经过这层，
/// 所以密钥不会因为这层逻辑出现在日志里。
struct CloseCatcher<S> {
    inner: S,
    /// 还没处理完的字节。两种东西会留在这儿：**没读完的 HTTP 升级应答**，以及
    /// **拼不完整的帧**（控制帧不分片，但 TCP 会分片，一条 close 帧很可能横跨两次读）。
    pending: Vec<u8>,
    /// 已处理好、等着交给上层的字节。`push` 一次可能吐出比上层缓冲大得多的量
    /// （底层一次读 16 KiB，tungstenite 给的缓冲只有 4 KiB），所以必须按上层
    /// 能收的量分批吐，余量留到下一次 poll——一次塞太多 `put_slice` 会 panic。
    ready: Vec<u8>,
    /// `ready` 里已经交出去的前缀长度。
    ready_pos: usize,
    caught: CaughtCloseSlot,
    /// 升级应答的 `\r\n\r\n` 是不是已经过去了。
    ///
    /// **不能靠"握手 future 返回了"来切换**：服务端可以在同一个 TCP 段里把升级应答和
    /// 第一帧 close 一起发过来（百炼额度用尽时正是这样），那时握手还没返回，
    /// 按标志位切就会让那条超长 close 帧原样进 tungstenite，症状和没做这层一模一样。
    upgraded: bool,
}

impl<S> CloseCatcher<S> {
    fn new(inner: S, caught: CaughtCloseSlot) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            ready: Vec::new(),
            ready_pos: 0,
            caught,
            upgraded: false,
        }
    }

    /// 底下那层（拿设置 TCP 选项用）。
    fn inner(&self) -> &S {
        &self.inner
    }

    /// 吃进新到的字节，吐出可以交给 tungstenite 的字节。**可能什么都不吐**
    /// （升级应答没收全、或者只到了一帧的头），这时上层要接着读——所以
    /// [`AsyncRead::poll_read`] 收到空输出时会继续读，而不是把 0 字节当 EOF 交上去。
    fn push(&mut self, fresh: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(fresh);
        let mut out = Vec::with_capacity(self.pending.len());
        let mut start = 0usize;

        if !self.upgraded {
            match find_subslice(&self.pending, b"\r\n\r\n") {
                // 升级应答原样透传；应答之后紧跟的字节已经是 WebSocket 帧了。
                Some(end) => {
                    out.extend_from_slice(&self.pending[..end + 4]);
                    start = end + 4;
                    self.upgraded = true;
                }
                // 应答没收全：整段留在 `pending` 里等下一段，这时一个字都不能往外吐
                // （tungstenite 在等应答，它等不到就永远建不起来）。
                None => return out,
            }
        }

        let rest = &self.pending[start..];
        let mut consumed = 0usize;
        while let Some((header_len, payload_len, total)) = frame_len(&rest[consumed..]) {
            // 长度算不出来（溢出）：不可能是合法帧，剩下的原样放行，让 tungstenite 去报错。
            let Some(total_len) = total else {
                out.extend_from_slice(&rest[consumed..]);
                consumed = rest.len();
                break;
            };
            if rest.len() - consumed < total_len {
                break;
            }
            let frame = &rest[consumed..consumed + total_len];
            let masked = frame[1] & 0x80 != 0;
            if opcode_of(frame) == 0x8 && payload_len > MAX_CONTROL_PAYLOAD && !masked {
                let (code, reason) = split_close_payload(&frame[header_len..]);
                *self.caught.lock().expect("close 槽的锁不该中毒") =
                    Some(CaughtClose { code, reason });
                // 重新拼一条合法的 close 帧：同样的头，负载 = 2 字节码 + 至多 123 字节
                // 理由。截断点退回到 UTF-8 字符边界，否则 tungstenite 会报 UTF-8 错误，
                // 读循环走 Err 分支（完整原文已经另存在槽里）。
                let reason_bytes = &frame[header_len + CLOSE_CODE_LEN..];
                let mut keep = (MAX_CONTROL_PAYLOAD - CLOSE_CODE_LEN).min(reason_bytes.len());
                while keep > 0 && (reason_bytes[keep] & 0xC0) == 0x80 {
                    keep -= 1;
                }
                let new_len = CLOSE_CODE_LEN + keep;
                let mut truncated = Vec::with_capacity(header_len + new_len);
                truncated.extend_from_slice(&rewrite_length(&frame[..header_len], new_len));
                truncated.extend_from_slice(&frame[header_len..header_len + new_len]);
                out.extend_from_slice(&truncated);
            } else {
                out.extend_from_slice(frame);
            }
            consumed += total_len;
        }

        let leftover = rest[consumed..].to_vec();
        self.pending.clear();
        self.pending.extend_from_slice(&leftover);
        // 对端报了一个荒谬的帧长度：别把内存一路喂给它，原样交给 tungstenite
        // 去按它自己的上限报错（默认 64 MiB）。没有这道闸，一个假头就能把进程撑爆。
        if consumed == 0 && start == 0 && leftover.len() > MAX_PENDING {
            out.extend_from_slice(&leftover);
            self.pending.clear();
        }
        out
    }
}

/// 找一段字节第一次出现的位置。
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 帧头 `(header_len, payload_len, total_len)`。字节不够就返回 `None`；
/// 总长度算不出来（64 位长度字段溢出 `usize`）时 `total_len` 是 `None`。
///
/// 帧头两字节：`b0 = FIN|RSV|opcode`，`b1 = MASK|len`。**掩码位在第二字节**。
/// 这层**只改写服务端→客户端的未掩码 close 帧**（协议规定服务端不掩码）；
/// 掩码帧照常算长度、原样放行。
fn frame_len(bytes: &[u8]) -> Option<(usize, usize, Option<usize>)> {
    let second = *bytes.get(1)?;
    let masked = second & 0x80 != 0;
    let short_len = (second & 0x7F) as usize;
    let (header_len, payload_len): (usize, usize) = match short_len {
        126 => {
            let extended = bytes.get(2..4)?;
            (4, u16::from_be_bytes([extended[0], extended[1]]) as usize)
        }
        127 => {
            let extended = bytes.get(2..10)?;
            let raw: [u8; 8] = extended.try_into().expect("刚好 8 字节");
            (
                10,
                usize::try_from(u64::from_be_bytes(raw)).unwrap_or(usize::MAX),
            )
        }
        len => (2, len),
    };
    let mask_len = if masked { 4 } else { 0 };
    let total = header_len
        .checked_add(mask_len)
        .and_then(|head| head.checked_add(payload_len));
    Some((header_len + mask_len, payload_len, total))
}

fn opcode_of(frame: &[u8]) -> u8 {
    frame[0] & 0x0F
}

/// close 帧的负载：前 2 字节大端状态码，其余是 UTF-8 理由。
fn split_close_payload(payload: &[u8]) -> (Option<u16>, String) {
    if payload.len() < CLOSE_CODE_LEN {
        return (None, String::new());
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    (
        Some(code),
        String::from_utf8_lossy(&payload[CLOSE_CODE_LEN..]).into_owned(),
    )
}

/// 把帧头的长度字段改成 `new_len`（其余位原样保留：FIN/opcode/mask 位不动）。
fn rewrite_length(header: &[u8], new_len: usize) -> Vec<u8> {
    let mut out = header.to_vec();
    match out[1] & 0x7F {
        126 => out[2..4].copy_from_slice(&(new_len as u16).to_be_bytes()),
        127 => out[2..10].copy_from_slice(&(new_len as u64).to_be_bytes()),
        _ => out[1] = (out[1] & 0x80) | (new_len as u8),
    }
    out
}

impl<S: AsyncRead + Unpin> AsyncRead for CloseCatcher<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut scratch = [0u8; SPY_BUFFER];
        loop {
            // 有存货先吐存货（只吐上层放得下的量），不去碰底层：
            // 这时返回 Ready，上层下一次 poll 会接着来取，不存在"该醒没醒"。
            if self.ready_pos < self.ready.len() {
                let take = (self.ready.len() - self.ready_pos).min(buf.remaining());
                let start = self.ready_pos;
                buf.put_slice(&self.ready[start..start + take]);
                self.ready_pos += take;
                if self.ready_pos == self.ready.len() {
                    self.ready.clear();
                    self.ready_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }
            let mut scratch_buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut self.inner).poll_read(cx, &mut scratch_buf) {
                Poll::Ready(Ok(())) => {
                    let fresh = scratch_buf.filled().len();
                    if fresh == 0 {
                        // 对端真的收摊了（真 EOF）。
                        return Poll::Ready(Ok(()));
                    }
                    // 空输出（只到了一帧的头）：接着读下一段，别把 0 字节当 EOF 交上去。
                    // 非空输出存进 `ready`，下一圈循环按上层缓冲大小分批吐。
                    self.ready = self.push(&scratch_buf.filled()[..fresh]);
                    self.ready_pos = 0;
                }
                other => return other,
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CloseCatcher<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// --- 读循环 ------------------------------------------------------------------

/// 确保进程级 CryptoProvider 已安装。rustls 0.23 需要显式选一个 provider
/// （aws-lc-rs 或 ring），否则首次 TLS 握手时 panic。装过就跳过（幂等）。
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 从 WebSocket 读帧、过滤 Ping/Pong、投递给主线程。
///
/// tungstenite 0.30 的客户端**会自动回复 Ping**（在 `read()` 内部处理的），
/// 所以这里只需要忽略 Pong 帧，不需要自己发 Pong。
async fn reader_task(
    mut read: WsReader,
    tx: mpsc::Sender<ReaderMsg>,
    shutdown: CancellationToken,
    caught: CaughtCloseSlot,
) {
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                break;
            }
            frame = read.next() => {
                match frame {
                    None => {
                        // Stream 结束，对端关了。
                        let _ = tx.send(ReaderMsg::Closed(
                            "对端关闭了连接".to_owned(),
                        )).await;
                        break;
                    }
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Text(t) => {
                                if tx.send(ReaderMsg::Text(t.to_string())).await.is_err() {
                                    break; // 主线程不收了
                                }
                            }
                            Message::Binary(b) => {
                                // DashScope 正常只发 text，但防御性地解码 binary。
                                let text = String::from_utf8_lossy(&b).into_owned();
                                if tx.send(ReaderMsg::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            Message::Close(frame) => {
                                let reason = close_reason(frame, &caught);
                                let _ = tx.send(ReaderMsg::Closed(reason)).await;
                                break;
                            }
                            // Ping: tungstenite 已自动回了 Pong，这里不需要做任何事。
                            // Pong: 忽略。
                            Message::Ping(_) | Message::Pong(_) => {}
                            // tungstenite 0.30 还有个 Frame variant，不应该出现在这里。
                            _ => {}
                        }
                    }
                    Some(Err(e)) => {
                        // 旁路已经截到过 close 原文就以它为准：那才是服务端的真话，
                        // 比 tungstenite 的协议错误有用得多。
                        let has_caught = caught.lock().map(|slot| slot.is_some()).unwrap_or(false);
                        if has_caught {
                            let _ = tx.send(ReaderMsg::Closed(close_reason(None, &caught))).await;
                            break;
                        }
                        let reason = format!("连接读取出错：{e}");
                        let _ = tx.send(ReaderMsg::Error(reason)).await;
                        break;
                    }
                }
            }
        }
    }
}

fn reader_msg(msg: ReaderMsg) -> Incoming {
    match msg {
        ReaderMsg::Text(text) => Incoming::Text(text),
        ReaderMsg::Closed(reason) | ReaderMsg::Error(reason) => Incoming::Closed(reason),
    }
}

/// 服务端 close 的原因。旁路截到过超长帧就用**原文**（`CloseCatcher` 截出来的
/// 那一条是空的或缺头的），否则用 tungstenite 交上来的这条。
fn close_reason(
    frame: Option<tungstenite::protocol::CloseFrame>,
    caught: &CaughtCloseSlot,
) -> String {
    if let Ok(mut slot) = caught.lock() {
        if let Some(caught) = slot.take() {
            return format!(
                "服务端关闭连接（{}）：{}",
                caught
                    .code
                    .map_or_else(|| "无状态码".to_string(), |code| code.to_string()),
                caught.reason
            );
        }
    }
    match frame {
        Some(close) => format!("服务端关闭连接（{}）：{}", close.code, close.reason),
        None => "服务端关闭连接（无附加信息）".to_owned(),
    }
}

// --- 错误映射 ----------------------------------------------------------------

/// 把 tungstenite 的连接错误翻译成中文 PortError，并尽量区分失败类别。
fn map_connect_error(err: tungstenite::Error) -> PortError {
    match &err {
        tungstenite::Error::Http(response) => {
            let status = response.status();
            let body = response
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b))
                .unwrap_or_default();
            PortError::new(format!("服务端拒绝了连接（HTTP {status}）：{body}",))
        }
        tungstenite::Error::HttpFormat(_) => PortError::new(format!("HTTP 协议格式错误：{err}")),
        tungstenite::Error::Io(io_err) => {
            let kind = io_err.kind();
            // 区分 DNS / TCP 连接失败。
            if format!("{err}").contains("域名解析失败")
                || format!("{err}").contains("dns")
                || format!("{err}").contains("resolve")
                || format!("{err}").contains("getaddrinfo")
            {
                PortError::new(format!("DNS 解析失败，检查网络连接：{err}"))
            } else {
                PortError::new(format!("网络连接失败（{kind:?}）：{err}"))
            }
        }
        tungstenite::Error::Tls(_) => {
            PortError::new(format!("TLS 握手失败，可能是证书或网络代理问题：{err}"))
        }
        _ => PortError::new(format!("WebSocket 连接失败：{err}")),
    }
}

// --- 测试 --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use std::net::SocketAddr;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    use vox_core::cloud::{ConnectRequest, Incoming, Transport};

    /// 测试用的 runtime。服务器跑在里面，transport 在当前线程用 `new(handle)` 构造
    /// 并直接调用——因为测试是普通 `#[test]`，不在 async context 里，所以 block_on 合法。
    fn test_runtime() -> Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// 起一个本地 WebSocket echo 服务器。
    async fn start_echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut write, mut read) = ws.split();
                while let Some(Ok(msg)) = read.next().await {
                    match msg {
                        Message::Text(_) | Message::Binary(_) => {
                            if write.send(msg).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        });
        addr
    }

    /// 起一个带自定义握手逻辑的服务器（能检查请求头）。
    /// 通过 oneshot 把收到的 Authorization 头传出来。
    async fn start_header_checking_server(
        auth_tx: tokio::sync::oneshot::Sender<Option<String>>,
    ) -> SocketAddr {
        use tokio_tungstenite::tungstenite::handshake::server::Callback;

        /// 握手回调：抄下 Authorization 头，然后原样放行。
        struct CaptureAuth<'a>(&'a mut Option<String>);

        impl Callback for CaptureAuth<'_> {
            fn on_request(self, req: &Request, resp: Response) -> Result<Response, ErrorResponse> {
                *self.0 = req
                    .headers()
                    .get("Authorization")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                Ok(resp)
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let mut captured_auth: Option<String> = None;
                let ws =
                    tokio_tungstenite::accept_hdr_async(stream, CaptureAuth(&mut captured_auth))
                        .await
                        .unwrap();
                let _ = auth_tx.send(captured_auth);
                let (mut write, mut read) = ws.split();
                while let Some(Ok(msg)) = read.next().await {
                    match msg {
                        Message::Text(_) | Message::Binary(_) => {
                            if write.send(msg).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        });
        addr
    }

    /// 手工造一个 **超长** close 帧的字节（服务端→客户端方向，**不加掩码**）。
    ///
    /// 百炼在额度用尽时发的那种帧：负载 182 字节，远超 RFC 的 125 字节上限，
    /// tungstenite 只会报 `Control frame too big`。
    fn oversized_close_frame(code: u16, reason: &str) -> Vec<u8> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        let mut frame = vec![0x88u8, 126]; // FIN + close，长度走 16 位扩展字段
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    /// 起一个"握完手就甩一条手造超长 close 帧"的服务器。
    ///
    /// 用 tungstenite 造不出这种帧（它自己就会拒），所以握手走 tungstenite、
    /// 发帧走裸字节：握完手把 `WebSocketStream` 拆回裸 `TcpStream` 再写。
    async fn start_oversized_close_server(reason: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut raw = ws.into_inner();
            let frame = oversized_close_frame(1007, reason);
            let _ = raw.write_all(&frame).await;
            let _ = raw.flush().await;
            // 别急着关：客户端要时间把这一帧读完。
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
        addr
    }

    /// 起一个收到连接就主动关闭的服务器。
    async fn start_closing_server(code: u16, reason: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut write, _read) = ws.split();
                let close_frame = tungstenite::protocol::CloseFrame {
                    code: tungstenite::protocol::frame::coding::CloseCode::from(code),
                    reason: reason.into(),
                };
                let _ = write.send(Message::Close(Some(close_frame))).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        addr
    }

    /// 起一个直接回 HTTP 401 的服务器（不做 WebSocket 升级）。
    async fn start_rejecting_server(status: u16) -> SocketAddr {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    tokio::io::AsyncReadExt::read(&mut stream, &mut buf),
                )
                .await;
                let body = format!("rejected with {status}");
                let resp = format!(
                    "HTTP/1.1 {status} Rejected\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n\
                     {body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        addr
    }

    fn make_request(addr: SocketAddr) -> ConnectRequest {
        ConnectRequest {
            url: format!("ws://127.0.0.1:{}", addr.port()),
            auth_header: "Bearer sk-test-key".to_owned(),
            network: NetworkSetting::default(),
        }
    }

    #[test]
    fn connect_send_recv_roundtrip() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let mut transport = WsTransport::new(rt.handle().clone());

        transport.connect(&make_request(addr)).unwrap();
        transport.send(r#"{"hello":"world"}"#).unwrap();

        let msg = transport.recv(2000).unwrap();
        assert_eq!(msg, Some(Incoming::Text(r#"{"hello":"world"}"#.to_owned())));
    }

    #[test]
    fn authorization_header_arrives_at_server() {
        let rt = test_runtime();
        let (auth_tx, auth_rx) = tokio::sync::oneshot::channel();
        let addr = rt.block_on(start_header_checking_server(auth_tx));
        let mut transport = WsTransport::new(rt.handle().clone());

        transport.connect(&make_request(addr)).unwrap();
        transport.send("ping").unwrap();
        let _ = transport.recv(500).unwrap();
        transport.close();

        let captured = rt.block_on(auth_rx).unwrap();
        assert_eq!(captured, Some("Bearer sk-test-key".to_owned()));
    }

    #[test]
    fn recv_timeout_returns_none_and_connection_survives() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();

        // 没人给我们发消息，超时应该返回 None。
        let result = transport.recv(200).unwrap();
        assert_eq!(result, None, "超时应该返回 None 而不是错误");

        // 连接还活着——能正常收发。
        transport.send("still alive").unwrap();
        let msg = transport.recv(2000).unwrap();
        assert_eq!(msg, Some(Incoming::Text("still alive".to_owned())));
    }

    #[test]
    fn server_close_surfaces_as_incoming_closed() {
        let rt = test_runtime();
        let addr = rt.block_on(start_closing_server(1000, "再见"));
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();

        let msg = transport.recv(2000).unwrap();
        match msg {
            Some(Incoming::Closed(reason)) => {
                assert!(reason.contains("1000"), "应该包含 close code: {reason}");
                assert!(reason.contains("再见"), "应该包含 reason 文本: {reason}");
            }
            other => panic!("应该是 Closed，实际是 {other:?}"),
        }
    }

    #[test]
    fn close_twice_does_not_panic() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();
        transport.close();
        transport.close();
    }

    #[test]
    fn rejected_handshake_produces_port_error_with_status() {
        let rt = test_runtime();
        let addr = rt.block_on(start_rejecting_server(401));
        let mut transport = WsTransport::new(rt.handle().clone());

        let err = transport.connect(&make_request(addr)).unwrap_err();
        assert!(
            err.message.contains("401"),
            "错误消息里应该有 HTTP 状态码: {}",
            err.message
        );
    }

    #[test]
    fn send_before_connect_fails_gracefully() {
        let rt = test_runtime();
        let mut transport = WsTransport::new(rt.handle().clone());
        let err = transport.send("hello").unwrap_err();
        assert!(err.message.contains("未建立"));
    }

    #[test]
    fn recv_before_connect_fails_gracefully() {
        let rt = test_runtime();
        let mut transport = WsTransport::new(rt.handle().clone());
        let err = transport.recv(100).unwrap_err();
        assert!(err.message.contains("未建立"));
    }

    /// 百炼额度用尽时真发过来的那条（2026-10-09 `--raw` 抓帧实测，182 字节）。
    const QUOTA_REASON: &str = "The free tier of the model has been exhausted. If you wish to \
        continue access the model on a paid basis, please disable the \"use free tier only\" \
        mode in the management console.";

    /// 超长 close 帧要**原样**交到上层，而不是变成一句 `Control frame too big`。
    /// 少一个字节的转述都不行：用户要照原文去控制台处理额度。
    #[test]
    fn an_oversized_close_frame_survives_with_its_whole_reason() {
        let rt = test_runtime();
        let addr = rt.block_on(start_oversized_close_server(QUOTA_REASON));
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();

        match transport.recv(2000).unwrap() {
            Some(Incoming::Closed(reason)) => {
                assert!(reason.contains("1007"), "close code 要透传：{reason}");
                assert_eq!(
                    reason.matches(QUOTA_REASON).count(),
                    1,
                    "服务端原文要一字不差地交上来：{reason}"
                );
                // 上层据此给出"去控制台关掉仅免费额度"的提示。
                assert!(vox_core::cloud::explain_close(&reason).is_some());
            }
            other => panic!("应该是 Closed，实际是 {other:?}"),
        }
    }

    /// 自签证书 + 服务端 acceptor。返回 acceptor 与证书（给客户端当信任根）。
    fn tls_fixture() -> (
        tokio_rustls::TlsAcceptor,
        rustls::pki_types::CertificateDer<'static>,
    ) {
        tls_fixture_for("localhost")
    }

    /// 同上，证书签给 `name`（走代理远端解析时，连接的主机名不是 localhost）。
    fn tls_fixture_for(
        name: &str,
    ) -> (
        tokio_rustls::TlsAcceptor,
        rustls::pki_types::CertificateDer<'static>,
    ) {
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
        let certified = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
        let cert = certified.cert.der().clone();
        let key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        (tokio_rustls::TlsAcceptor::from(Arc::new(config)), cert)
    }

    /// 只信任 `cert` 的客户端 TLS 配置。
    fn client_trusting(
        cert: rustls::pki_types::CertificateDer<'static>,
    ) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }

    type TlsServerWs =
        tokio_tungstenite::WebSocketStream<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>;

    /// 起一个本地 **TLS** WebSocket 服务端，握完手把连接交给 `script`。
    ///
    /// 这条路径专门防"`CloseCatcher` 套在 TLS 之下"：那样它看到的是密文，
    /// 会把 TLS 握手字节当 WS 帧去截断，真连 `wss://` 全部失败，而 `ws://` 的测试看不出来。
    async fn start_tls_server<F, Fut>(acceptor: tokio_rustls::TlsAcceptor, script: F) -> SocketAddr
    where
        F: FnOnce(TlsServerWs) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let Ok(ws) = tokio_tungstenite::accept_async(tls).await else {
                return;
            };
            script(ws).await;
        });
        addr
    }

    fn wss_request(addr: SocketAddr) -> ConnectRequest {
        ConnectRequest {
            url: format!("wss://localhost:{}", addr.port()),
            auth_header: "Bearer sk-test-key".to_owned(),
            network: NetworkSetting::default(),
        }
    }

    /// 先回一条普通文本帧，再甩一条超长 close 帧。
    async fn text_then_oversized_close(mut ws: TlsServerWs, reason: &'static str) {
        ws.send(Message::Text("hello over tls".into()))
            .await
            .unwrap();
        let mut raw = ws.into_inner();
        let _ = raw.write_all(&oversized_close_frame(1007, reason)).await;
        let _ = raw.flush().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    #[test]
    fn an_oversized_close_frame_survives_over_tls_too() {
        let rt = test_runtime();
        let (acceptor, cert) = tls_fixture();
        let addr = rt.block_on(start_tls_server(acceptor, |ws| {
            text_then_oversized_close(ws, QUOTA_REASON)
        }));
        let mut transport =
            WsTransport::new(rt.handle().clone()).with_tls_config(client_trusting(cert));
        transport.connect(&wss_request(addr)).unwrap();

        assert_eq!(
            transport.recv(2000).unwrap(),
            Some(Incoming::Text("hello over tls".to_owned())),
            "TLS 之上的普通帧要原样到达"
        );
        match transport.recv(2000).unwrap() {
            Some(Incoming::Closed(reason)) => {
                assert!(reason.contains("1007"), "close code 要透传：{reason}");
                assert_eq!(
                    reason.matches(QUOTA_REASON).count(),
                    1,
                    "TLS 下也要拿到完整原文：{reason}"
                );
            }
            other => panic!("应该是 Closed，实际是 {other:?}"),
        }
    }

    /// 证书校验必须开着：服务端用自签证书、客户端不信任它 → 连接失败。
    #[test]
    fn an_untrusted_certificate_is_refused() {
        let rt = test_runtime();
        let (acceptor, _cert) = tls_fixture();
        let addr = rt.block_on(start_tls_server(acceptor, |ws| async move {
            drop(ws);
        }));
        // 默认配置只信任 webpki 内置根，自签证书不在其中。
        let mut transport = WsTransport::new(rt.handle().clone());
        assert!(
            transport.connect(&wss_request(addr)).is_err(),
            "自签证书不该被接受"
        );
    }

    /// 400 条 2.9–4.7 KB 的文本帧一次灌过来，客户端先睡一会儿再收：底层一次读 16 KiB，
    /// 而 tungstenite 给的读缓冲只有 4 KiB，`CloseCatcher` 一次吐出的量必须按对方放得下的
    /// 量分批，否则 `put_slice` 越界 panic、读任务死掉。
    const BURST_FRAMES: usize = 400;

    fn burst_payload(index: usize) -> String {
        let len = 2900 + (index * 7) % 1800;
        let mut text = format!("{index:04}:");
        text.extend(std::iter::repeat_n('x', len));
        text
    }

    fn assert_burst_received(transport: &mut WsTransport) {
        std::thread::sleep(Duration::from_millis(500));
        for index in 0..BURST_FRAMES {
            match transport.recv(5000).unwrap() {
                Some(Incoming::Text(text)) => {
                    assert_eq!(text, burst_payload(index), "第 {index} 条要按序原样到达")
                }
                other => panic!("第 {index} 条应该是文本帧，实际是 {other:?}"),
            }
        }
    }

    #[test]
    fn a_burst_of_large_frames_is_delivered_in_order() {
        let rt = test_runtime();
        let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();
        rt.spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            for index in 0..BURST_FRAMES {
                ws.feed(Message::Text(burst_payload(index).into()))
                    .await
                    .unwrap();
            }
            ws.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        });
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();
        assert_burst_received(&mut transport);
    }

    #[test]
    fn a_burst_of_large_frames_is_delivered_in_order_over_tls() {
        let rt = test_runtime();
        let (acceptor, cert) = tls_fixture();
        let addr = rt.block_on(start_tls_server(acceptor, |mut ws| async move {
            for index in 0..BURST_FRAMES {
                ws.feed(Message::Text(burst_payload(index).into()))
                    .await
                    .unwrap();
            }
            ws.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        }));
        let mut transport =
            WsTransport::new(rt.handle().clone()).with_tls_config(client_trusting(cert));
        transport.connect(&wss_request(addr)).unwrap();
        assert_burst_received(&mut transport);
    }

    /// 理由是 "X" + 100 个汉字：第 123 字节落在汉字中间。截断点必须退回字符边界，
    /// 否则 tungstenite 报 UTF-8 错误、读循环走 Err 分支，原文就丢了。
    #[test]
    fn a_multibyte_close_reason_is_cut_on_a_char_boundary() {
        let reason: &'static str = Box::leak(format!("X{}", "额".repeat(100)).into_boxed_str());
        assert!(
            reason.len() > MAX_CONTROL_PAYLOAD
                && !reason.is_char_boundary(MAX_CONTROL_PAYLOAD - CLOSE_CODE_LEN)
        );
        let rt = test_runtime();
        let addr = rt.block_on(start_oversized_close_server(reason));
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&make_request(addr)).unwrap();
        match transport.recv(2000).unwrap() {
            Some(Incoming::Closed(text)) => assert_eq!(
                text.matches(reason).count(),
                1,
                "中文原因要一字不差：{text}"
            ),
            other => panic!("应该是 Closed，实际是 {other:?}"),
        }
    }

    /// 同上，直接看改写出来的帧：截断后的负载是合法 UTF-8。
    #[test]
    fn the_truncated_close_frame_is_valid_utf8() {
        let reason = format!("X{}", "额".repeat(100));
        let slot = close_reason_slot();
        let mut catcher = CloseCatcher::new((), slot);
        catcher.upgraded = true;
        let out = catcher.push(&oversized_close_frame(1007, &reason));
        let (header_len, payload_len, _) = frame_len(&out).unwrap();
        assert!(payload_len <= MAX_CONTROL_PAYLOAD);
        assert!(std::str::from_utf8(&out[header_len + CLOSE_CODE_LEN..]).is_ok());
    }

    /// 64 位长度字段是 `u64::MAX`：加头长会溢出 `usize`。不能 panic，
    /// 字节原样放行交给 tungstenite 去报错。
    #[test]
    fn an_absurd_64_bit_length_does_not_overflow() {
        let mut frame = vec![0x88u8, 127];
        frame.extend_from_slice(&u64::MAX.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        let (_, _, total) = frame_len(&frame).unwrap();
        assert_eq!(total, None);

        let mut catcher = CloseCatcher::new((), close_reason_slot());
        catcher.upgraded = true;
        assert_eq!(catcher.push(&frame), frame, "算不出长度就原样放行");
    }

    /// 截帧这层只动超长 close 帧：升级应答原样透传、正常帧原样透传，
    /// 帧边界跨两次读也要拼得回来。
    #[test]
    fn the_catcher_leaves_ordinary_frames_and_split_frames_alone() {
        let slot = close_reason_slot();
        let mut catcher = CloseCatcher::new((), slot.clone());

        // 升级应答：先只到一半，一个字节都不能吐（tungstenite 在等它）。
        let response = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n";
        let head = catcher.push(&response[..20]);
        assert!(head.is_empty());
        let mut handshake_out = head;
        handshake_out.extend_from_slice(&catcher.push(&response[20..]));
        assert_eq!(
            handshake_out, response,
            "升级应答要原样透传，一个字节都不能少"
        );

        // 一条 300 字节的文本帧（126 位扩展长度），原样吐出。
        let mut text = vec![0x81u8, 126];
        let payload = vec![b'x'; 300];
        text.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        text.extend_from_slice(&payload);
        let rest = catcher.push(&text);
        assert_eq!(rest, text, "正常帧不能被动过");

        // 半帧先到：什么都不吐，等下一段（TCP 会把帧切开）。
        let close = oversized_close_frame(1007, QUOTA_REASON);
        let (head, tail) = close.split_at(3);
        assert!(catcher.push(head).is_empty(), "半帧不能当整帧交出去");
        let truncated = catcher.push(tail);
        let (_, payload_len, total_len) = frame_len(&truncated).expect("要吐一条完整帧");
        assert_eq!(total_len, Some(truncated.len()), "只能吐一条帧");
        assert_eq!(payload_len, MAX_CONTROL_PAYLOAD, "负载要截到 RFC 上限之内");
        let caught = slot.lock().unwrap().clone().expect("要截到原文");
        assert_eq!(caught.code, Some(1007));
        assert_eq!(caught.reason, QUOTA_REASON);
    }

    /// 服务端把升级应答和超长 close 帧**放在同一个 TCP 段**里发过来（百炼额度用尽时
    /// 正是这样）：应答原样透传，紧跟着的那条帧照样要截。靠"握手返回了再切"是接不住的。
    #[test]
    fn an_upgrade_response_glued_to_the_close_frame_is_still_handled() {
        let slot = close_reason_slot();
        let mut catcher = CloseCatcher::new((), slot.clone());
        let mut glued = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        glued.extend_from_slice(&oversized_close_frame(1007, QUOTA_REASON));

        let out = catcher.push(&glued);
        let header_end = find_subslice(&glued, b"\r\n\r\n").expect("应答头");
        assert_eq!(&out[..header_end + 4], &glued[..header_end + 4]);
        let (_, payload_len, _) = frame_len(&out[header_end + 4..]).expect("后面还有一帧");
        assert_eq!(payload_len, MAX_CONTROL_PAYLOAD);
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().reason, QUOTA_REASON);
    }

    /// 对端报一个 64 MiB 的帧长度然后慢慢喂：这层不能跟着它一起把内存喂爆。
    /// 攒过上限就把字节原样放行（tungstenite 会按它自己的上限报错）。
    #[test]
    fn an_absurd_frame_length_is_not_buffered_forever() {
        let slot = close_reason_slot();
        let mut catcher = CloseCatcher::new((), slot.clone());
        let mut glued = b"HTTP/1.1 101 Switching Protocols\r\n\r\n".to_vec();
        glued.extend_from_slice(&[0x82, 127]); // 文本帧 + 64 位长度
        glued.extend_from_slice(&(u64::MAX / 2).to_be_bytes());
        catcher.push(&glued);

        let junk = vec![0u8; MAX_PENDING + 1];
        let out = catcher.push(&junk);
        assert!(
            out.ends_with(&junk),
            "攒过上限就要放行，让 tungstenite 去报错，而不是继续攒"
        );
        assert_eq!(out.len(), junk.len() + 10, "连那个假头一起放行");
    }

    // --- 经代理 ------------------------------------------------------------

    use vox_core::settings::NetworkMode;
    use vox_proxy::testing::{http_connect_proxy, socks5_proxy};

    const REMOTE_ONLY: &str = "remote-only.invalid";

    fn via(proxy: &str) -> NetworkSetting {
        NetworkSetting {
            mode: NetworkMode::Custom,
            proxy: proxy.to_owned(),
        }
    }

    /// 服务端证书签给一个**只有代理那一侧能解析**的名字；TLS、CloseCatcher、WS 全在隧道之后。
    fn remote_only_wss() -> (Runtime, ConnectRequest, WsTransport, SocketAddr) {
        let rt = test_runtime();
        let (acceptor, cert) = tls_fixture_for(REMOTE_ONLY);
        let addr = rt.block_on(start_tls_server(acceptor, |ws| {
            text_then_oversized_close(ws, QUOTA_REASON)
        }));
        let transport =
            WsTransport::new(rt.handle().clone()).with_tls_config(client_trusting(cert));
        let request = ConnectRequest {
            url: format!("wss://{REMOTE_ONLY}:{}", addr.port()),
            auth_header: String::new(),
            network: NetworkSetting::default(),
        };
        (rt, request, transport, addr)
    }

    fn assert_frames_survived_the_tunnel(transport: &mut WsTransport) {
        assert_eq!(
            transport.recv(2000).unwrap(),
            Some(Incoming::Text("hello over tls".to_owned()))
        );
        match transport.recv(2000).unwrap() {
            Some(Incoming::Closed(reason)) => assert_eq!(
                reason.matches(QUOTA_REASON).count(),
                1,
                "隧道里也要拿到完整的超长 close 原文：{reason}"
            ),
            other => panic!("应该是 Closed，实际是 {other:?}"),
        }
    }

    #[test]
    fn wss_goes_through_an_http_connect_tunnel() {
        let (rt, mut request, mut transport, addr) = remote_only_wss();
        let proxy = http_connect_proxy(
            Some(("alice", "hunter2")),
            vec![(REMOTE_ONLY.to_owned(), addr)],
        );
        request.network = via(&format!("http://alice:hunter2@{}", proxy.addr));
        transport.connect(&request).unwrap();
        assert_frames_survived_the_tunnel(&mut transport);
        assert_eq!(
            proxy.requests(),
            vec![format!("CONNECT {REMOTE_ONLY}:{} HTTP/1.1", addr.port())],
            "流量必须真的走了代理"
        );
        drop(rt);
    }

    #[test]
    fn wss_goes_through_socks5h_and_the_proxy_resolves_the_name() {
        let (rt, mut request, mut transport, addr) = remote_only_wss();
        let proxy = socks5_proxy(
            Some(("alice", "hunter2")),
            vec![(REMOTE_ONLY.to_owned(), addr)],
        );
        request.network = via(&format!("socks5h://alice:hunter2@{}", proxy.addr));
        transport.connect(&request).unwrap();
        assert_frames_survived_the_tunnel(&mut transport);
        assert_eq!(
            proxy.requests(),
            vec![format!("domain:{REMOTE_ONLY}:{}", addr.port())],
            "socks5h 要把域名原样交给代理，而不是本地解析成 IP"
        );
        drop(rt);
    }

    /// 本地解析 `.invalid` 必然失败：不走 socks5h 就连不上，证明上面那条不是碰巧本地解析成功。
    #[test]
    fn plain_socks5_cannot_reach_a_name_only_the_proxy_knows() {
        let (rt, mut request, mut transport, addr) = remote_only_wss();
        let proxy = socks5_proxy(None, vec![(REMOTE_ONLY.to_owned(), addr)]);
        request.network = via(&format!("socks5://{}", proxy.addr));
        let err = transport.connect(&request).unwrap_err();
        assert!(err.to_string().contains("域名解析失败"), "{err}");
        assert!(proxy.requests().is_empty());
        drop(rt);
    }

    /// 代理挂了就是挂了：绝不悄悄改成直连（那会让用户以为在走代理）。
    #[test]
    fn a_dead_proxy_does_not_fall_back_to_a_direct_connection() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut request = make_request(addr);
        request.network = via(&format!("http://u:secret-pass@127.0.0.1:{dead}"));
        let mut transport = WsTransport::new(rt.handle().clone());
        let err = transport.connect(&request).unwrap_err().to_string();
        assert!(err.contains("连接代理"), "{err}");
        assert!(!err.contains("secret-pass"), "错误里不许有代理密码：{err}");
    }

    #[test]
    fn a_malformed_custom_proxy_is_an_error_not_a_direct_connection() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let mut request = make_request(addr);
        request.network = via("ftp://nope:1");
        let mut transport = WsTransport::new(rt.handle().clone());
        let err = transport.connect(&request).unwrap_err().to_string();
        assert!(err.contains("ftp"), "{err}");
        request.network = via("");
        assert!(transport.connect(&request).is_err(), "自定义但没填 = 错误");
    }

    #[test]
    fn plain_ws_also_tunnels() {
        let rt = test_runtime();
        let addr = rt.block_on(start_echo_server());
        let proxy = http_connect_proxy(None, vec![]);
        let mut request = make_request(addr);
        request.network = via(&format!("http://{}", proxy.addr));
        let mut transport = WsTransport::new(rt.handle().clone());
        transport.connect(&request).unwrap();
        transport.send("hi").unwrap();
        assert_eq!(
            transport.recv(2000).unwrap(),
            Some(Incoming::Text("hi".to_owned()))
        );
        assert_eq!(proxy.requests().len(), 1);
    }
}
