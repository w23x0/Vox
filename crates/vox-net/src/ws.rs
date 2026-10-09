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
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use vox_core::cloud::{ConnectRequest, Incoming, Transport};
use vox_core::ports::{PortError, PortResult};

/// 套在 TLS 之下、tungstenite 之上的那层：只读方向拦超长 close 帧（见 [`CloseCatcher`]）。
type WsStream = WebSocketStream<MaybeTlsStream<CloseCatcher<tokio::net::TcpStream>>>;
type WsSink = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;

/// 下行缓冲上限。512 条足够容纳数秒突发译音，同时能阻止断开的 UI/工作线程
/// 让无界队列一直吃内存。满了时读任务自然向 TCP 施加背压。
const READER_QUEUE_SIZE: usize = 512;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

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
        }
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
            connect_with_close_catcher(req, caught.clone()).await
        })
        .ok_or_else(|| PortError::new("连接超时（30 秒内未完成握手）"))?
        .map_err(map_connect_error)?
        .0;

        // 实时音频是频繁的小帧；关闭 Nagle，避免小包在等待 ACK 时平白多挨一拍。
        if let Err(error) = stream.get_ref().get_ref().inner().set_nodelay(true) {
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

/// 自己建连（DNS + TCP + TLS + 升级），中途插一层 [`CloseCatcher`]。
///
/// 不走 `connect_async` 的原因只有一个：那一层没有地方插我们自己的字节旁路，
/// 而超长 close 帧必须在**进 tungstenite 之前**处理（见 [`CloseCatcher`]）。
/// DNS/端口/协议分支都照 `tokio_tungstenite::connect` 的写法抄。
async fn connect_with_close_catcher(
    request: tungstenite::handshake::client::Request,
    caught: CaughtCloseSlot,
) -> Result<(WsStream, tungstenite::handshake::client::Response), tungstenite::Error> {
    use tungstenite::error::UrlError;

    let uri = request.uri();
    let host = uri
        .host()
        .ok_or(tungstenite::Error::Url(UrlError::NoHostName))?;
    let port = uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("wss") => Some(443),
        Some("ws") => Some(80),
        _ => None,
    });
    let port = port.ok_or(tungstenite::Error::Url(UrlError::UnsupportedUrlScheme))?;

    let socket = tokio::net::TcpStream::connect((host, port))
        .await
        .map_err(tungstenite::Error::Io)?;
    let guarded = CloseCatcher::new(socket, caught);
    tokio_tungstenite::client_async_tls_with_config(request, guarded, None, None).await
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

/// TLS 之下的一层**只读旁路**：认出超长的 close 帧，把它截到 RFC 上限之内再交给
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
        while let Some((header_len, payload_len, total_len)) = frame_len(&rest[consumed..]) {
            if rest.len() - consumed < total_len {
                break;
            }
            let frame = &rest[consumed..consumed + total_len];
            if opcode_of(frame) == 0x8 && payload_len > MAX_CONTROL_PAYLOAD {
                let (code, reason) = split_close_payload(&frame[header_len..]);
                *self.caught.lock().expect("close 槽的锁不该中毒") =
                    Some(CaughtClose { code, reason });
                // 重新拼一条合法的 close 帧：同样的头，长度改成 125（2 字节码 +
                // 123 字节理由），理由按字节截断（多字节 UTF-8 截断了也只是
                // 服务端那条备注的一部分，完整原文已经另存了）。
                let mut truncated = Vec::with_capacity(header_len + MAX_CONTROL_PAYLOAD);
                truncated
                    .extend_from_slice(&rewrite_length(&frame[..header_len], MAX_CONTROL_PAYLOAD));
                truncated.extend_from_slice(&frame[header_len..header_len + MAX_CONTROL_PAYLOAD]);
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

/// 帧头 `(header_len, payload_len, total_len)`。字节不够就返回 `None`。
///
/// 帧头两字节：`b0 = FIN|RSV|opcode`，`b1 = MASK|len`。**掩码位在第二字节**
/// （服务端→客户端的帧一律不掩码，这里仍然照协议读，免得哪天接到会掩码的对端）。
fn frame_len(bytes: &[u8]) -> Option<(usize, usize, usize)> {
    let second = *bytes.get(1)?;
    let masked = second & 0x80 != 0;
    let short_len = (second & 0x7F) as usize;
    let (header_len, payload_len) = match short_len {
        126 => {
            let extended = bytes.get(2..4)?;
            (4, u16::from_be_bytes([extended[0], extended[1]]) as usize)
        }
        127 => {
            let extended = bytes.get(2..10)?;
            let raw: [u8; 8] = extended.try_into().expect("刚好 8 字节");
            (10, u64::from_be_bytes(raw) as usize)
        }
        len => (2, len),
    };
    let mask_len = if masked { 4 } else { 0 };
    let total = header_len + mask_len + payload_len;
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
            let mut scratch_buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut self.inner).poll_read(cx, &mut scratch_buf) {
                Poll::Ready(Ok(())) => {
                    let fresh = scratch_buf.filled().len();
                    if fresh == 0 {
                        // 对端真的收摊了（真 EOF）。
                        return Poll::Ready(Ok(()));
                    }
                    let out = self.push(&scratch_buf.filled()[..fresh]);
                    if out.is_empty() {
                        // 只到了一帧的头：接着读下一段，别把 0 字节当 EOF 交上去。
                        continue;
                    }
                    buf.put_slice(&out);
                    return Poll::Ready(Ok(()));
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
            if format!("{err}").contains("dns")
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
        assert_eq!(total_len, truncated.len(), "只能吐一条帧");
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
}
