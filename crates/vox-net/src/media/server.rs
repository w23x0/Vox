//! 媒体面入站：`/audio` 监听、握手鉴权、收帧进抖动缓冲、 paced drain 出 `AudioChunk`。
//!
//! 线程模型（照设计稿 §2.4.2）：
//!
//! ```text
//! 监听线程（自带 1 线程 tokio runtime）
//!   accept ─► 鉴权（握手回调里）─► 读循环 ─► 校验帧头 ─► 抖动环
//!                                                       │
//! 排空线程（OS 线程 + parking_lot Condvar）◄─────────────┘
//!   每 block_ms 醒一次 ─► on_chunk（真实音频 / 欠载补静音 / 超过 pad_ms 停止）
//! ```
//!
//! **零新增依赖**：tokio / tokio-tungstenite / futures-util / tokio-util / parking_lot
//! 都已在 workspace 与 `Cargo.lock` 里。

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::stream::{SplitSink, StreamExt};
use futures_util::SinkExt;
use parking_lot::{Condvar, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{
    header, HeaderValue, Response as HttpResponse, StatusCode,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async_with_config, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use vox_core::ports::{
    AudioChunk, CaptureFormat, CaptureSource, CaptureTarget, PortError, PortResult,
};

use super::frame::{self, FrameHeader, FrameKind, MediaError};
use super::pipe::{random_seq_seed, JitterBuffer, PipeConfig, PipeStats};
use super::{ws_config, DEFAULT_PIPE};

/// 媒体面唯一路径。控制面是 `/mcp`，两条管子互不可用（设计稿 §2.5.1）。
pub const PATH: &str = "/audio";

/// 浏览器那条凭据通道的子协议前缀（`Sec-WebSocket-Protocol: vox.media.v1.<token>`）。
pub const SUBPROTOCOL_PREFIX: &str = "vox.media.v1.";

/// 监听参数。由外壳装配期填（`Settings.net` + `SecretStore`）。
#[derive(Debug, Clone)]
pub struct MediaOptions {
    /// 监听地址。端口 `0` = 系统分配（`local_addr()` 报实际值）。
    pub listen: SocketAddr,
    /// 声明采样率（Hz）。入站每一帧都要相符。
    pub rate_hz: u32,
    /// 媒体面凭据（43 字符）。与控制面是**两套**，互不可用。
    pub token: String,
    /// 允许的浏览器 `Origin`。空 = 拒绝一切带 `Origin` 的握手（fail-closed）。
    pub allowed_origins: Vec<String>,
    pub pipe: PipeConfig,
}

/// 入站监听。**绑上才算 `net_in` 的定义者**（设计稿 §2.2.3）。
pub struct MediaListener {
    shared: Arc<Shared>,
    accept: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    options: MediaOptions,
    local_addr: SocketAddr,
    /// 单调毫秒的零点（每帧的时间戳都以它为原点）。
    epoch: Instant,
    /// 抖动缓冲：读循环写、排空线程读。
    state: Mutex<JitterBuffer>,
    /// 与 `state` 配对的唤醒。
    wake: Condvar,
    /// 当前连接的中断开关（`stop()` / `shutdown()` 掐它）。
    peer_cancel: Mutex<Option<CancellationToken>>,
    /// 连接槽位：同一时刻只服务一条连接（第二条在握手前就拒，不静默抢）。
    peer_taken: AtomicBool,
    /// 腿槽位：一个监听只服务一条腿（第二条腿的 `start` 报错）。
    leg_taken: AtomicBool,
    shutdown: CancellationToken,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// 掐掉当前连接（`stop()` 里排在"排空线程先停"之后）。
    fn cancel_peer(&self) {
        if let Some(cancel) = self.peer_cancel.lock().take() {
            cancel.cancel();
        }
    }
}

impl MediaListener {
    /// 真的绑上才算（`net_in` 的定义者）。失败 → `PortError`（外壳据此报 `not_wired` / `busy`）。
    pub fn bind(options: MediaOptions) -> PortResult<Self> {
        if options.token.is_empty() {
            return Err(PortError::new(
                "媒体面必须有凭据：token 是空的，拒绝监听（fail-closed）",
            ));
        }
        if options.rate_hz == 0 {
            // 率是会话身份（每一帧都要相符）。0 会让"一块多少样本"退化成 1 个样本，
            // 是配置没填对，不是"随便挑一个"。
            return Err(PortError::new(
                "媒体面的声明采样率不能是 0 Hz（配置没填对，拒绝监听）",
            ));
        }
        let std_listener = StdTcpListener::bind(options.listen)
            .map_err(|e| PortError::new(format!("媒体面监听 {} 失败：{e}", options.listen)))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| PortError::new(format!("媒体面监听套接字设非阻塞失败：{e}")))?;
        let local_addr = std_listener
            .local_addr()
            .map_err(|e| PortError::new(format!("读不到媒体面实际监听地址：{e}")))?;

        let rate_hz = options.rate_hz;
        let shared = Arc::new(Shared {
            state: Mutex::new(JitterBuffer::new(options.pipe, rate_hz)),
            options,
            local_addr,
            epoch: Instant::now(),
            wake: Condvar::new(),
            peer_cancel: Mutex::new(None),
            peer_taken: AtomicBool::new(false),
            leg_taken: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        });

        let thread = std::thread::Builder::new()
            .name("vox-media-listen".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_thread(std_listener, shared)
            })
            .map_err(|e| PortError::new(format!("创建媒体面监听线程失败：{e}")))?;

        info!(listen = %local_addr, rate = rate_hz, "媒体面开始监听");
        Ok(Self {
            shared,
            accept: Mutex::new(Some(thread)),
        })
    }

    /// 实际监听地址（端口 `0` 时就是系统分配的那个）。
    pub fn local_addr(&self) -> SocketAddr {
        self.shared.local_addr
    }

    /// 腿级的采集源（一次会话一个；第二条腿 `start` 时会报错）。
    pub fn capture(&self) -> Box<dyn CaptureSource> {
        Box::new(MediaCapture {
            shared: Arc::clone(&self.shared),
            stop: Arc::new(AtomicBool::new(false)),
            claimed: false,
            drain: Mutex::new(None),
        })
    }

    pub fn stats(&self) -> PipeStats {
        self.shared.state.lock().stats(self.shared.now_ms())
    }

    /// 关机：掐连接、停监听线程，等它退出。
    ///
    /// 必须从**非 async 上下文**调用（外壳的关机路径就是普通线程）。
    pub fn shutdown(&self) {
        self.shared.shutdown.cancel();
        self.shared.cancel_peer();
        self.shared.wake.notify_all();
        if let Some(thread) = self.accept.lock().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MediaListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 监听线程：自带一个 1 线程 runtime（不要求调用方有 async context）。
fn accept_thread(listener: StdTcpListener, shared: Arc<Shared>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            error!(error = %e, "媒体面监听线程的 runtime 起不来");
            return;
        }
    };
    runtime.block_on(async move {
        let listener = match TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(e) => {
                error!(error = %e, "媒体面监听套接字转 tokio 失败");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = shared.shutdown.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, addr)) => {
                        tokio::spawn(serve(stream, addr, Arc::clone(&shared)));
                    }
                    Err(e) => {
                        warn!(error = %e, "媒体面 accept 失败");
                        // accept 失败不该把我们拖进忙等。
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    });
}

/// 一条连接的全过程：占槽 → 握手鉴权 → 读帧入环 → 收尾。
async fn serve(stream: TcpStream, addr: SocketAddr, shared: Arc<Shared>) {
    if shared.peer_taken.swap(true, Ordering::AcqRel) {
        reject_raw(stream, "409 Conflict").await;
        warn!(%addr, "媒体面已经在服务一条连接，拒绝并发连接");
        return;
    }
    let _slot = PeerSlot(Arc::clone(&shared));

    let auth = Arc::clone(&shared);
    // 返回类型是 tungstenite 的 `Callback` 契约写死的（`ErrorResponse` 136 字节）：
    // 装箱要改库的类型，握手一次一个、也不在热路径上，所以就地静音这一条。
    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, resp: Response| match authorize(req, &auth) {
        Ok(None) => Ok(resp),
        Ok(Some(protocol)) => {
            let mut resp = resp;
            if let Ok(value) = HeaderValue::from_str(&protocol) {
                // 浏览器那条通道：101 里回同一个子协议名（客户端会校验）。
                resp.headers_mut().insert("Sec-WebSocket-Protocol", value);
            }
            Ok(resp)
        }
        Err(status) => Err(reject_response(status)),
    };

    let ws: WebSocketStream<TcpStream> =
        match accept_hdr_async_with_config(stream, callback, Some(ws_config())).await {
            Ok(ws) => ws,
            Err(e) => {
                debug!(%addr, error = %e, "媒体面握手没通过");
                return;
            }
        };

    {
        let mut state = shared.state.lock();
        state.on_connected(shared.now_ms());
    }
    shared.wake.notify_all();

    let cancel = CancellationToken::new();
    *shared.peer_cancel.lock() = Some(cancel.clone());
    info!(%addr, "媒体面对端已连上");

    let cfg = shared.options.pipe;
    let mut seq = random_seq_seed(u64::from(addr.port()));
    let mut last_send_ms = shared.now_ms();
    let mut wire: Vec<u8> = Vec::new();

    let (mut write, mut read) = ws.split();
    loop {
        let now_ms = shared.now_ms();
        let keepalive_wait = u64::from(cfg.keepalive_ms)
            .saturating_sub(now_ms.saturating_sub(last_send_ms))
            .max(1);
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = cancel.cancelled() => break,
            // 入站侧也要发保活：出站侧的 `peer_timeout_ms` 判死靠"收到过帧"，
            // 如果只有出站单向发保活，那条判死规则会把自己人误杀（对端从不说话）。
            _ = tokio::time::sleep(Duration::from_millis(keepalive_wait)) => {
                let now_ms = shared.now_ms();
                let header = FrameHeader {
                    kind: FrameKind::KeepAlive,
                    seq,
                    ts_ms: now_ms as u32,
                    rate: shared.options.rate_hz,
                    channels: 1,
                };
                seq = seq.wrapping_add(1);
                frame::encode(&header, &[], &mut wire);
                shared.state.lock().on_own_frame(wire.len());
                if write.send(Message::Binary(wire.clone().into())).await.is_err() {
                    break;
                }
                last_send_ms = now_ms;
            }
            msg = read.next() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Binary(bytes)) => {
                        let now_ms = shared.now_ms();
                        match frame::decode(&bytes, shared.options.rate_hz) {
                            Ok((header, payload)) => {
                                {
                                    let mut state = shared.state.lock();
                                    state.on_frame(&header, payload, now_ms);
                                }
                                shared.wake.notify_all();
                            }
                            Err(error) => {
                                shared.state.lock().on_protocol_error(&error);
                                warn!(%addr, error = %error, "媒体面收到坏帧，断开这条连接");
                                break;
                            }
                        }
                    }
                    Ok(Message::Text(_)) => {
                        let error = MediaError::TextFrame;
                        shared.state.lock().on_protocol_error(&error);
                        warn!(%addr, error = %error, "媒体面收到文本帧，断开这条连接");
                        break;
                    }
                    // tungstenite 收到 Ping 会自动排一个 Pong；这里把它冲出去。
                    Ok(Message::Ping(_)) => {
                        let _ = write.flush().await;
                    }
                    Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                    Ok(Message::Close(_)) => break,
                    Err(e) => {
                        debug!(%addr, error = %e, "媒体面读失败");
                        break;
                    }
                }
            }
        }
    }

    {
        let mut state = shared.state.lock();
        state.on_disconnected(shared.now_ms());
    }
    shared.wake.notify_all();
    *shared.peer_cancel.lock() = None;
    let _ = close(write).await;
    info!(%addr, "媒体面对端已断开");
}

async fn close(mut write: SplitSink<WebSocketStream<TcpStream>, Message>) {
    let _ = write.close().await;
}

/// 连接槽位的释放器：无论这条连接怎么退出都会放掉槽位。
struct PeerSlot(Arc<Shared>);

impl Drop for PeerSlot {
    fn drop(&mut self) {
        self.0.peer_taken.store(false, Ordering::Release);
    }
}

/// 握手前的鉴权。`Ok(None)` = 走请求头那条通道；`Ok(Some(协议名))` = 走浏览器子协议通道。
fn authorize(req: &Request, shared: &Shared) -> Result<Option<String>, StatusCode> {
    // 单路径：别的路径连握手都不给（不泄漏"这里有个媒体面"以外的信息）。
    if req.uri().path() != PATH {
        return Err(StatusCode::NOT_FOUND);
    }

    // 带 Origin 的握手必须逐字命中白名单（空白名单 = 拒一切浏览器）。
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
        if !shared
            .options
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
        {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    // 通道①：非浏览器（盒子、探针、CLI）走 Authorization: Bearer。
    if let Some(value) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = value.strip_prefix("Bearer ") {
            if secret_eq(token, &shared.options.token) {
                return Ok(None);
            }
        }
    }

    // 通道②：浏览器的 `WebSocket` 构造函数只能设子协议、不能设请求头。
    if let Some(protocols) = req
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|value| value.to_str().ok())
    {
        for protocol in protocols.split(',') {
            let protocol = protocol.trim();
            if let Some(token) = protocol.strip_prefix(SUBPROTOCOL_PREFIX) {
                if secret_eq(token, &shared.options.token) {
                    return Ok(Some(protocol.to_string()));
                }
            }
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

/// 常数时间比较（长度不同直接判否：长度本身不是秘密）。
fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 握手被拒时的响应：**不带 body**，也不回帧。
fn reject_response(status: StatusCode) -> ErrorResponse {
    let mut response = HttpResponse::new(None);
    *response.status_mut() = status;
    response
}

/// 并发连接在**握手之前**就被拒：直接回一行 HTTP，不做升级。
async fn reject_raw(mut stream: TcpStream, line: &str) {
    let mut response = String::with_capacity(64);
    response.push_str("HTTP/1.1 ");
    response.push_str(line);
    response.push_str("\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// `net_in` 的 `CaptureSource`：从抖动环里按节拍取音频，喂给芯的回调。
struct MediaCapture {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    /// 有没有占着腿槽位。
    claimed: bool,
    drain: Mutex<Option<JoinHandle<()>>>,
}

impl CaptureSource for MediaCapture {
    fn start(
        &mut self,
        target: &CaptureTarget,
        block_ms: u32,
        on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
    ) -> PortResult<CaptureFormat> {
        let CaptureTarget::Net { pipe } = target else {
            return Err(PortError::new(
                "媒体面采集源只服务 CaptureTarget::Net（不静默换源）",
            ));
        };
        if pipe != DEFAULT_PIPE {
            return Err(PortError::new(format!(
                "未知的媒体面管道名「{pipe}」（v1 只有「{DEFAULT_PIPE}」）"
            )));
        }
        if self.shared.leg_taken.swap(true, Ordering::AcqRel) {
            return Err(PortError::new(
                "这个媒体面监听已经在服务另一条腿（一条监听只服务一条腿）",
            ));
        }
        self.claimed = true;
        let cfg = self.shared.options.pipe;
        if block_ms != cfg.block_ms {
            warn!(
                want = block_ms,
                pipe = cfg.block_ms,
                "媒体面的块长以管子配置为准（外壳该用同一个常数填两边）"
            );
        }

        self.stop.store(false, Ordering::Release);
        let shared = Arc::clone(&self.shared);
        let stop = Arc::clone(&self.stop);
        let thread = std::thread::Builder::new()
            .name("vox-media-drain".into())
            .spawn(move || drain_thread(shared, stop, on_chunk))
            .map_err(|e| {
                self.claimed = false;
                self.shared.leg_taken.store(false, Ordering::Release);
                PortError::new(format!("创建媒体面排空线程失败：{e}"))
            })?;
        *self.drain.lock() = Some(thread);

        Ok(CaptureFormat {
            sample_rate: self.shared.options.rate_hz,
            channels: 1,
        })
    }

    fn stop(&mut self) {
        // 先停排空线程：它一 join 完，回调就不可能再触发。
        self.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(thread) = self.drain.lock().take() {
            let _ = thread.join();
        }
        if self.claimed {
            self.claimed = false;
            self.shared.leg_taken.store(false, Ordering::Release);
        }
        // 再断连接。
        self.shared.cancel_peer();
    }
}

impl Drop for MediaCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 排空线程：按绝对时刻表出块（真实音频 / 欠载补静音）。
fn drain_thread(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    mut on_chunk: Box<dyn FnMut(AudioChunk) + Send>,
) {
    let rate = shared.options.rate_hz;
    let block = shared.options.pipe.block_samples(rate);
    let mut guard = shared.state.lock();
    let mut samples: Vec<f32> = Vec::with_capacity(block);
    loop {
        if stop.load(Ordering::Acquire) || shared.shutdown.is_cancelled() {
            break;
        }
        let now_ms = shared.now_ms();
        let emit = guard.tick(now_ms, &mut samples);
        let wait = guard.wait_ms(now_ms);
        match emit {
            // 没对端 / 预填中 / 已进 idle：不回调（"对端还没来" ≠ "抖动"）。
            None => {
                shared
                    .wake
                    .wait_for(&mut guard, Duration::from_millis(wait));
            }
            Some(_) => {
                // 回调不许在持锁时调用（回调那一侧可能会读 `stats()`）。
                drop(guard);
                // 每块一次 `Vec<f32>`（回调拿走所有权）——与麦克风源同款。
                let chunk = AudioChunk {
                    samples: std::mem::take(&mut samples),
                    sample_rate: rate,
                    channels: 1,
                };
                samples = Vec::with_capacity(block);
                on_chunk(chunk);
                guard = shared.state.lock();
            }
        }
    }
}
