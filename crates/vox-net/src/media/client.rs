//! 媒体面出站：`MediaOut` —— 退避重连 + 攒块 + 保活，实现 `PlaybackSink`。
//!
//! 线程模型：**一条 OS 线程跑一个 1 线程 tokio runtime**，里面是一条"连接—服务—断开"的
//! 循环。`PlaybackSink::push` 只往共享队列里塞（`push` 在流水线线程上，**不许阻塞、不许新增分配**），
//! 发送由那条 tokio 任务完成。
//!
//! **位是装配期事实**（设计稿 §2.2.3）：`spawn` 成功 = 出站池起来了 = `net_out` 有定义者。
//! 此刻连没连上只进统计（`connected` / `reconnects`），**不进能力位**——否则对端重启会把
//! "这台设备有没有这个能力"翻来覆去。

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::stream::{SplitSink, StreamExt};
use futures_util::SinkExt;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use vox_core::ports::{PlaybackSink, PlaybackStats, PortError, PortResult};

use super::frame::{self, FrameHeader, FrameKind};
use super::pipe::{random_seq_seed, Counters, OutPipe, PipeConfig, PipeStats};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// 首次重连的退避起点。
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// 退避上限。
const MAX_BACKOFF: Duration = Duration::from_secs(4);
/// 一次握手的最长等待（连不上不算失败，但要有个头）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 出站池。`spawn` 起来了 = `net_out` 的定义者（设计稿 §2.2.3）。
pub struct MediaOut {
    shared: Arc<OutShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct OutShared {
    peer: String,
    token: String,
    cfg: PipeConfig,
    /// 单调毫秒的零点。
    epoch: Instant,
    state: Mutex<OutState>,
    /// 有新东西要发 / 关机时叫醒发送任务。
    wake: Notify,
    shutdown: CancellationToken,
    /// 最近一次连接失败的原因（链路状态，进日志与状态出口，**不进能力位**）。
    last_error: Mutex<Option<String>>,
}

struct OutState {
    /// `open()` 之后才有（网络出口的线率是 `open` 那一步定的）。
    pipe: Option<OutPipe>,
    counters: Counters,
    /// 线上的采样率（`open` 的 `source_rate`）。
    rate: u32,
    /// 真的写进 socket 的样本数（`PlaybackStats::rendered_samples` 的口径）。
    rendered_samples: u64,
    /// 上一次往 socket 写东西的时刻（保活用它算空闲）。
    last_send_ms: u64,
    /// `flush()` 的请求序号 / 已经办到的序号。
    flush_wanted: u64,
    flush_done: u64,
}

impl OutShared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
}

impl MediaOut {
    /// 起出站池（含退避重连）。**连不上不算失败**：那是链路状态，不是能力。
    pub fn spawn(peer: String, token: String, pipe: PipeConfig) -> PortResult<Self> {
        if token.is_empty() {
            return Err(PortError::new(
                "media plane requires a credential: token is empty, refusing to dial out (fail-closed)",
            ));
        }
        // scheme 白名单：配置面也拦一道，但打错成 `http://` 在这里就该拦住——
        // 否则池子会拿着一个永远连不上的地址退避重连，日志里只剩噪声。
        if !peer.starts_with("ws://") && !peer.starts_with("wss://") {
            return Err(PortError::new(format!(
                "media plane peer must be ws:// or wss://, got '{peer}'"
            )));
        }
        let shared = Arc::new(OutShared {
            peer,
            token,
            cfg: pipe,
            epoch: Instant::now(),
            state: Mutex::new(OutState {
                pipe: None,
                counters: Counters::default(),
                rate: 0,
                rendered_samples: 0,
                last_send_ms: 0,
                flush_wanted: 0,
                flush_done: 0,
            }),
            wake: Notify::new(),
            shutdown: CancellationToken::new(),
            last_error: Mutex::new(None),
        });

        let thread = std::thread::Builder::new()
            .name("vox-media-out".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || out_thread(shared)
            })
            .map_err(|e| {
                PortError::new(format!(
                    "could not create the media plane dial-out thread: {e}"
                ))
            })?;

        info!(peer = %shared.peer, "媒体面出站池已起来");
        Ok(Self {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// 腿级的播放汇（一次会话一个）。
    pub fn sink(&self) -> Box<dyn PlaybackSink> {
        Box::new(MediaOutSink {
            shared: Arc::clone(&self.shared),
        })
    }

    pub fn stats(&self) -> PipeStats {
        let state = self.shared.state.lock();
        state.counters.stats(state.rate, self.shared.now_ms())
    }

    /// 最近一次连不上的原因（连上之后清空）。给状态出口与探针用。
    pub fn last_error(&self) -> Option<String> {
        self.shared.last_error.lock().clone()
    }

    /// 关机：停池并等线程退出。
    ///
    /// 必须从**非 async 上下文**调用（外壳的关机路径就是普通线程）。
    pub fn shutdown(&self) {
        self.shared.shutdown.cancel();
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MediaOut {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn out_thread(shared: Arc<OutShared>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            error!(error = %e, "媒体面出站线程的 runtime 起不来");
            return;
        }
    };
    runtime.block_on(pool(shared));
}

/// 连接—服务—断开的主循环（退避在失败与断开之后都生效）。
async fn pool(shared: Arc<OutShared>) {
    let mut backoff = INITIAL_BACKOFF;
    // 上一次收场是失败/断开吗？是的话，这次连上算一次重连。
    let mut recovering = false;
    while !shared.shutdown.is_cancelled() {
        match connect(&shared).await {
            Ok(ws) => {
                {
                    let mut state = shared.state.lock();
                    state.counters.connected = true;
                    if recovering {
                        state.counters.reconnects += 1;
                    }
                    let now_ms = shared.now_ms();
                    state.last_send_ms = now_ms;
                    state.counters.last_frame_ms = now_ms;
                }
                *shared.last_error.lock() = None;
                info!(peer = %shared.peer, "媒体面出站已连上对端");
                serve(ws, &shared).await;
                {
                    let mut state = shared.state.lock();
                    state.counters.connected = false;
                    state.counters.on_disconnect();
                }
                warn!(peer = %shared.peer, "媒体面出站与对端断开，准备重连");
                recovering = true;
                backoff = INITIAL_BACKOFF;
            }
            Err(e) => {
                shared.state.lock().counters.connected = false;
                *shared.last_error.lock() = Some(e.message.clone());
                warn!(peer = %shared.peer, error = %e, "媒体面出站连不上对端");
                recovering = true;
            }
        }
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    debug!("媒体面出站线程退出");
}

/// 一次连接尝试。**关机要能立刻打断它**（否则外壳关机得干等超时）。
async fn connect(shared: &OutShared) -> PortResult<WsStream> {
    let mut request = shared
        .peer
        .as_str()
        .into_client_request()
        .map_err(|e| PortError::new(format!("invalid media plane peer address: {e}")))?;
    let mut bearer = String::with_capacity(shared.token.len() + 7);
    bearer.push_str("Bearer ");
    bearer.push_str(&shared.token);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&bearer)
            .map_err(|e| PortError::new(format!("invalid media plane auth header: {e}")))?,
    );

    tokio::select! {
        _ = shared.shutdown.cancelled() => Err(PortError::new("media plane dial-out has shut down")),
        result = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(request, Some(super::ws_config()), false),
        ) => match result {
            Ok(Ok((ws, _response))) => {
                // 实时音频是频繁的小帧：关掉 Nagle，别让小包白等一拍 ACK。
                if let Err(e) = ws.get_ref().get_ref().set_nodelay(true) {
                    debug!(error = %e, "媒体面出站设 TCP_NODELAY 失败，用系统默认");
                }
                Ok(ws)
            }
            Ok(Err(e)) => Err(map_connect_error(e)),
            Err(_) => Err(PortError::new("media plane dial-out timed out")),
        },
    }
}

fn map_connect_error(error: tungstenite::Error) -> PortError {
    PortError::new(format!("media plane dial-out handshake failed: {error}"))
}

/// 一条已建立连接的服务循环：攒块发帧、按空闲发保活、读对端的帧判活。
async fn serve(ws: WsStream, shared: &OutShared) {
    let (mut write, mut read) = ws.split();
    let mut seq = random_seq_seed(shared.peer.len() as u64);
    let mut payload: Vec<u8> = Vec::new();
    let mut wire: Vec<u8> = Vec::new();

    loop {
        // 1. 把该发的都发出去（整块 / flush 的余头 / 保活）。
        //    这里每帧一次小分配（`Message::Binary` 要所有权）：发送在 tokio 任务上，
        //    不在音频线程；零分配的那条路径是 `PlaybackSink::push`（只往队里追加）。
        while next_frame(shared, &mut seq, &mut payload, &mut wire) {
            if write
                .send(Message::Binary(wire.clone().into()))
                .await
                .is_err()
            {
                return;
            }
        }
        // 2. 睡到下一件事：新数据、保活点、对端判死点；期间也收帧。
        let wait = next_wait(shared);
        tokio::select! {
            _ = shared.shutdown.cancelled() => break,
            _ = shared.wake.notified() => {}
            _ = tokio::time::sleep(wait) => {}
            msg = read.next() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    let now_ms = shared.now_ms();
                    let rate = shared.state.lock().rate;
                    match frame::decode(&bytes, rate) {
                        Ok((header, payload)) => {
                            shared
                                .state
                                .lock()
                                .counters
                                .on_frame(&header, payload.len(), now_ms);
                        }
                        Err(e) => {
                            shared.state.lock().counters.on_protocol_error(&e);
                            warn!(peer = %shared.peer, error = %e, "媒体面出站收到坏帧，断开这条连接");
                            break;
                        }
                    }
                }
                Some(Ok(Message::Text(_))) => {
                    let e = super::frame::MediaError::TextFrame;
                    shared.state.lock().counters.on_protocol_error(&e);
                    warn!(peer = %shared.peer, error = %e, "媒体面出站收到文本帧，断开这条连接");
                    break;
                }
                // tungstenite 收到 Ping 会自动排一个 Pong；这里把它冲出去。
                Some(Ok(Message::Ping(_))) => {
                    let _ = write.flush().await;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    debug!(peer = %shared.peer, error = %e, "媒体面出站读失败");
                    break;
                }
                None => break,
            },
        }
        if peer_timed_out(shared, shared.now_ms()) {
            warn!(peer = %shared.peer, "媒体面出站对端超时（没收到任何帧），重连");
            break;
        }
    }
    let _ = close(write).await;
}

async fn close(mut write: SplitSink<WsStream, Message>) {
    let _ = write.close().await;
}

/// 这一次该往 socket 写什么。要写就把字节编进 `wire`，返回 `true`。
///
/// 顺序是有讲究的：**先整块、再 flush 的余头、最后才是保活**（保活是"没话说"时才发的）。
fn next_frame(
    shared: &OutShared,
    seq: &mut u32,
    payload: &mut Vec<u8>,
    wire: &mut Vec<u8>,
) -> bool {
    let mut state = shared.state.lock();
    let now_ms = shared.now_ms();
    let cfg = shared.cfg;
    let flush_wanted = state.flush_wanted > state.flush_done;
    let last_send_ms = state.last_send_ms;
    let kind = {
        let Some(pipe) = state.pipe.as_mut() else {
            return false;
        };
        if pipe.take_block(payload) || (flush_wanted && pipe.take_remainder(payload)) {
            // 整块优先；没有整块时，flush 请求可以把不足一块的余头送出去。
            FrameKind::Pcm16Le
        } else if flush_wanted || now_ms.saturating_sub(last_send_ms) >= u64::from(cfg.keepalive_ms)
        {
            payload.clear();
            FrameKind::KeepAlive
        } else {
            return false;
        }
    };
    if flush_wanted && kind == FrameKind::KeepAlive {
        // flush 的收尾保活发出去，才算这次 flush 办完（余头 + 一个保活，设计稿 §2.4.3）。
        state.flush_done = state.flush_wanted;
    }

    let header = FrameHeader {
        kind,
        seq: *seq,
        ts_ms: now_ms as u32,
        rate: state.rate,
        channels: 1,
    };
    *seq = seq.wrapping_add(1);
    frame::encode(&header, payload, wire);
    state.counters.frames_out += 1;
    state.counters.bytes_out += wire.len() as u64;
    state.last_send_ms = now_ms;
    if kind == FrameKind::Pcm16Le {
        state.rendered_samples += payload.len() as u64 / 2;
    }
    true
}

/// 下一次醒来的等待时长（保活点与判死点里更近的那个）。
fn next_wait(shared: &OutShared) -> Duration {
    let state = shared.state.lock();
    let now_ms = shared.now_ms();
    let idle = now_ms.saturating_sub(state.last_send_ms);
    let to_keepalive = u64::from(shared.cfg.keepalive_ms)
        .saturating_sub(idle)
        .max(1);
    let age = now_ms.saturating_sub(state.counters.last_frame_ms);
    let to_timeout = u64::from(shared.cfg.peer_timeout_ms)
        .saturating_sub(age)
        .max(1);
    Duration::from_millis(to_keepalive.min(to_timeout))
}

/// `peer_timeout_ms` 内没收到任何帧 → 判对端掉了（TCP 还活着不代表程序还在）。
fn peer_timed_out(shared: &OutShared, now_ms: u64) -> bool {
    let state = shared.state.lock();
    now_ms.saturating_sub(state.counters.last_frame_ms) > u64::from(shared.cfg.peer_timeout_ms)
}

/// 出站播放汇：`push` 只入队，发送在 tokio 任务里。
struct MediaOutSink {
    shared: Arc<OutShared>,
}

impl PlaybackSink for MediaOutSink {
    fn open(&mut self, device: Option<&str>, source_rate: u32) -> PortResult<u32> {
        if device.is_some() {
            return Err(PortError::new(
                "the network output is not a device: the media plane only accepts device = None",
            ));
        }
        if source_rate == 0 {
            return Err(PortError::new(
                "the network output has no wire rate (source_rate = 0): a line without a rate must not be opened",
            ));
        }
        let mut state = self.shared.state.lock();
        if state.rate != 0 && state.rate != source_rate {
            // 换率 = 换一条线：旧队列里那些样本按"丢掉"记，不许静默吞掉。
            state.counters.dropped_samples +=
                state.pipe.as_ref().map_or(0, |pipe| pipe.pending_samples()) as u64;
        }
        if state.pipe.is_none() || state.rate != source_rate {
            state.pipe = Some(OutPipe::new(self.shared.cfg, source_rate));
        }
        state.rate = source_rate;
        Ok(source_rate)
    }

    fn push(&mut self, samples: &[f32]) {
        {
            let mut state = self.shared.state.lock();
            match state.pipe.as_mut() {
                Some(pipe) => {
                    let dropped = pipe.push(samples);
                    state.counters.dropped_samples += dropped as u64;
                }
                // 还没 `open()`：没有线率可依，样本只能丢——但记数，不静默吞掉。
                None => state.counters.dropped_samples += samples.len() as u64,
            }
        }
        self.shared.wake.notify_one();
    }

    fn stats(&self) -> PlaybackStats {
        let state = self.shared.state.lock();
        PlaybackStats {
            queued_samples: state.pipe.as_ref().map_or(0, |pipe| pipe.pending_samples()),
            sample_rate: state.rate,
            channels: 1,
            rendered_samples: state.rendered_samples,
            dropped_samples: state.counters.dropped_samples,
            device_latency_ms: 0,
        }
    }

    fn flush(&mut self) {
        self.shared.state.lock().flush_wanted += 1;
        self.shared.wake.notify_one();
    }

    fn close(&mut self) {
        self.shared.shutdown.cancel();
        self.shared.wake.notify_one();
    }
}
