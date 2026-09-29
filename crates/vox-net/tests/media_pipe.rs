//! 媒体面帧层的集成用例：设计稿 `docs/plans/S3-NET-AUDIO.md` §4.2 的 12 条。
//!
//! 分工：**能确定性的就用注入时钟**（3/4/5 直接驱动 `JitterBuffer`，不碰 socket、
//! 不睡真实时间），必须过线的才开回环 socket（1/2/6/7/8/9/10/11/12 全在 `127.0.0.1`）。
//!
//! 时基给得宽的地方都写了理由：这些用例钉的是**样本守恒与状态机**，不是延迟实测
//! （设计稿 §4.5 明确不设延迟门槛）。

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use tokio::net::{TcpListener as TokioTcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response as ClientResponse;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use vox_core::cloud::protocol::{float_to_pcm16, pcm16_to_float};
use vox_core::ports::{AudioChunk, CaptureTarget};
use vox_net::media::frame::{self, FrameHeader, FrameKind, MAX_PAYLOAD};
use vox_net::media::pipe::{Emit, JitterBuffer, PipeConfig};
use vox_net::media::{MediaListener, MediaOptions, MediaOut, DEFAULT_PIPE};

const TOKEN: &str = "test-token-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
const RATE: u32 = 16_000;
const BLOCK: usize = 320;

/// 回环地址 + 系统分配端口。
fn free_addr() -> SocketAddr {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("回环上取个空闲端口");
    probe.local_addr().expect("拿到端口")
}

fn options(pipe: PipeConfig) -> MediaOptions {
    options_on(free_addr(), pipe)
}

fn options_on(listen: SocketAddr, pipe: PipeConfig) -> MediaOptions {
    MediaOptions {
        listen,
        rate_hz: RATE,
        token: TOKEN.to_string(),
        allowed_origins: Vec::new(),
        pipe,
    }
}

fn net_target() -> CaptureTarget {
    CaptureTarget::Net {
        pipe: DEFAULT_PIPE.to_string(),
    }
}

fn header(seq: u32) -> FrameHeader {
    FrameHeader {
        kind: FrameKind::Pcm16Le,
        seq,
        ts_ms: 0,
        rate: RATE,
        channels: 1,
    }
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn wire(kind: FrameKind, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame::encode(
        &FrameHeader {
            kind,
            seq,
            ts_ms: 0,
            rate: RATE,
            channels: 1,
        },
        payload,
        &mut out,
    );
    out
}

fn blocking(cond: impl Fn() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

async fn until(cond: impl Fn() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cond()
}

/// 测试侧的裸客户端：连上去（可选请求头），把响应也带回来。
async fn connect_raw(
    url: &str,
    headers: &[(&str, &str)],
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, ClientResponse), WsError> {
    let mut request = url.into_client_request().expect("URL 该能解析");
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_str(name).expect("请求头名合法"),
            HeaderValue::from_str(value).expect("请求头值合法"),
        );
    }
    connect_async(request).await
}

fn audio_url(listener: &MediaListener) -> String {
    format!("ws://{}{}", listener.local_addr(), vox_net::media::PATH)
}

/// 发一条帧过去；发送失败按"连接已经死了"处理（测试里不需要区分）。
async fn send_frame(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, bytes: Vec<u8>) {
    let _ = ws.send(Message::Binary(bytes.into())).await;
}

/// 对端是否真的断开了：读到 Close / 流结束 / 读错误都算。
async fn connection_died(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => return true,
            Ok(Some(Err(_))) => return true,
            // 我们只发不收帧；收到别的（比如服务端的保活）就继续等。
            Ok(Some(Ok(_))) => {}
        }
    }
}

// ── 1 ────────────────────────────────────────────────────────────────────────

/// 监听侧 ↔ 出站侧在回环上跑 2 秒已知音频：样本与原样一致、顺序正确、帧数守恒。
///
/// "逐字节相等"落在 **PCM16 那一层**：管线两端都是 16 位样本，收到的 f32 与线上字节
/// 一一对应（`pcm16_to_float`）。芯的编码器对正半轴乘 32767（`float_to_pcm16` 的既有
/// 语义），所以期望值就用同一个编码器算出来——这正是"线上该是什么"。
///
/// 时基说明：缓冲故意给大（800 ms 预填 / 1.2 s 环），因为这个用例要证的是
/// **一块不多一块不少**，不是抖动吸收。
#[test]
fn a_round_trip_is_byte_identical() {
    let pipe = PipeConfig {
        jitter_ms: 800,
        pad_ms: 2_000,
        queue_ms: 1_200,
        keepalive_ms: 10_000,
        peer_timeout_ms: 15_000,
        ..PipeConfig::default()
    };
    let listener = MediaListener::bind(options(pipe)).expect("绑得上回环");
    let out = MediaOut::spawn(audio_url(&listener), TOKEN.to_string(), pipe).expect("出站池起得来");
    let mut sink = out.sink();
    sink.open(None, RATE).expect("网络出口只认 device = None");

    let collected: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut capture = listener.capture();
    let heard = Arc::clone(&collected);
    capture
        .start(
            &net_target(),
            pipe.block_ms,
            Box::new(move |chunk: AudioChunk| heard.lock().expect("锁没毒").extend(chunk.samples)),
        )
        .expect("网络采集源认得 CaptureTarget::Net");

    assert!(
        blocking(|| out.stats().connected, Duration::from_secs(5)),
        "5 秒内该连上监听侧"
    );

    let total = 2 * RATE as usize;
    let source: Vec<f32> = (0..total)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            (t * 440.0 * std::f32::consts::TAU).sin() * 0.5
        })
        .collect();
    let expected = pcm16_to_float(&float_to_pcm16(&source));

    // 先灌满预填所需的 800 ms，再按**绝对时刻表**喂（与真实对端同节奏）：每轮 sleep 的
    // 抖动会累积成"供给慢于排空"，那会在这个用例里冒充丢样本/补静音。
    let lead = (pipe.jitter_ms / pipe.block_ms) as usize;
    let started = Instant::now();
    for (i, chunk) in source.chunks(BLOCK).enumerate() {
        sink.push(chunk);
        let paced = (i + 1).saturating_sub(lead);
        if paced > 0 {
            let target = Duration::from_millis(paced as u64 * u64::from(pipe.block_ms));
            let elapsed = started.elapsed();
            if target > elapsed {
                std::thread::sleep(target - elapsed);
            }
        }
    }

    assert!(
        blocking(
            || collected.lock().expect("锁没毒").len() >= total,
            Duration::from_secs(10)
        ),
        "10 秒内该收够 2 秒音频（收到 {} 样本）",
        collected.lock().expect("锁没毒").len()
    );
    capture.stop();
    let heard = collected.lock().expect("锁没毒").clone();

    assert_eq!(
        heard.len(),
        total,
        "样本数必须不多不少（多出来就是补了静音）"
    );
    assert_eq!(heard, expected, "样本必须与线上 PCM16 逐个对应且顺序正确");

    let server = listener.stats();
    let client = out.stats();
    assert_eq!(server.dropped_ms, 0, "这个节奏不该丢样本");
    assert_eq!(server.padded_ms, 0, "这个节奏不该补静音");
    assert_eq!(server.frames_in, client.frames_out, "帧数要守恒");
    assert_eq!(server.frames_in, 100, "2 秒 / 20 ms = 100 帧");
}

// ── 2 ────────────────────────────────────────────────────────────────────────

/// 一次 100 ms（5 块）的突发 → 排空侧**每块 320 样本**地回调，块数与样本数守恒。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_is_reblocked_to_the_manifest_block_ms() {
    let pipe = PipeConfig::default();
    let listener = MediaListener::bind(options(pipe)).expect("绑得上回环");

    let sizes: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut capture = listener.capture();
    let (sizes_sink, samples_sink) = (Arc::clone(&sizes), Arc::clone(&samples));
    capture
        .start(
            &net_target(),
            pipe.block_ms,
            Box::new(move |chunk: AudioChunk| {
                sizes_sink.lock().expect("锁没毒").push(chunk.samples.len());
                samples_sink
                    .lock()
                    .expect("锁没毒")
                    .extend_from_slice(&chunk.samples);
            }),
        )
        .expect("网络采集源认得 CaptureTarget::Net");

    let (mut ws, _) = connect_raw(
        &audio_url(&listener),
        &[("Authorization", &format!("Bearer {TOKEN}"))],
    )
    .await
    .expect("带对 token 该连得上");

    let burst: Vec<i16> = (0..5 * BLOCK).map(|i| (i % 700) as i16).collect();
    let mut expected = Vec::new();
    for (seq, chunk) in burst.chunks(BLOCK).enumerate() {
        let payload = pcm_bytes(chunk);
        expected.extend(pcm16_to_float(&payload));
        send_frame(&mut ws, wire(FrameKind::Pcm16Le, seq as u32, &payload)).await;
    }

    assert!(
        until(
            || sizes.lock().expect("锁没毒").len() >= 5,
            Duration::from_secs(3)
        )
        .await,
        "5 块突发该在 3 秒内排完（拿到 {:?}）",
        sizes.lock().expect("锁没毒")
    );
    let sizes = sizes.lock().expect("锁没毒").clone();
    let heard = samples.lock().expect("锁没毒").clone();

    assert_eq!(
        sizes,
        vec![BLOCK; 5],
        "重切块后每块 320 样本（@16k 的 20 ms）"
    );
    assert_eq!(heard.len(), 5 * BLOCK, "总样本数守恒");
    assert_eq!(heard, expected, "突发的内容与顺序都要原样出来");
    assert_eq!(
        listener.stats().dropped_ms,
        0,
        "100 ms 突发没有超环（160 ms）"
    );
}

// ── 3 ────────────────────────────────────────────────────────────────────────

/// 预填未满 `jitter_ms` 一次回调都没有；满了之后按 `block_ms` 的绝对时刻表回调。
///
/// 纯状态机 + 注入时钟：不碰 socket、不睡真实时间。
#[test]
fn the_jitter_buffer_prefills_before_the_first_callback() {
    let cfg = PipeConfig::default(); // 预填 40 ms = 640 样本
    let mut jitter = JitterBuffer::new(cfg, RATE);
    let mut out = Vec::new();
    jitter.on_connected(0);

    // 只有一块（20 ms < 40 ms）：过多久都不许回调。
    jitter.on_frame(&header(0), &pcm_bytes(&[1000; BLOCK]), 0);
    for t in (0..=200).step_by(20) {
        assert_eq!(jitter.tick(t, &mut out), None, "预填没满，t={t} 不许回调");
    }

    // 第二块到 → 预填满（640 样本）→ 开始按 20 ms 一拍。
    jitter.on_frame(&header(1), &pcm_bytes(&[1000; BLOCK]), 40);
    assert_eq!(
        jitter.tick(40, &mut out),
        None,
        "刚满足预填，第一拍在下一个 block_ms"
    );
    assert_eq!(jitter.tick(60, &mut out), Some(Emit::Data));
    assert_eq!(out.len(), BLOCK);
    assert_eq!(jitter.tick(79, &mut out), None, "没到点不发第二块");
    assert_eq!(jitter.tick(80, &mut out), Some(Emit::Data));
    assert_eq!(jitter.buffered_samples(), 0);
}

// ── 4 ────────────────────────────────────────────────────────────────────────

/// 欠载补静音，累计超过 `pad_ms` → 停止回调 + `idle_ms` 增长（"没有输入"不许被静音伪装）。
#[test]
fn an_underrun_pads_silence_up_to_pad_ms_then_stops() {
    let cfg = PipeConfig::default(); // 补静音上限 200 ms = 10 块
    let mut jitter = JitterBuffer::new(cfg, RATE);
    let mut out = Vec::new();
    jitter.on_connected(0);

    // 两块真实音频：预填满，两拍出完。
    jitter.on_frame(&header(0), &pcm_bytes(&[500; 2 * BLOCK]), 0);
    assert_eq!(jitter.tick(0, &mut out), None);
    assert_eq!(jitter.tick(20, &mut out), Some(Emit::Data));
    assert_eq!(jitter.tick(40, &mut out), Some(Emit::Data));

    // 环空了：连续补静音，补满 pad_ms 就停。
    let mut silences = 0;
    for step in 0..12 {
        let now = 60 + step * 20;
        match jitter.tick(now, &mut out) {
            Some(Emit::Silence) => {
                silences += 1;
                assert!(out.iter().all(|s| *s == 0.0), "补的必须是静音");
            }
            None => {}
            Some(Emit::Data) => panic!("环是空的，不可能有真实数据"),
        }
    }
    assert_eq!(silences, 10, "补到 pad_ms（200 ms / 20 ms）为止");
    assert_eq!(jitter.stats(260).padded_ms, 200);
    assert_eq!(
        jitter.tick(1000, &mut out),
        None,
        "进了 idle 就不许再补静音"
    );

    let idle_a = jitter.stats(300).idle_ms;
    let idle_b = jitter.stats(500).idle_ms;
    assert!(idle_a > 0, "停了回调之后 idle_ms 该开始算");
    assert!(idle_b > idle_a, "idle_ms 该随时间增长：{idle_a} → {idle_b}");

    // 又来声音：重新预填，之后恢复真实数据。
    jitter.on_frame(&header(1), &pcm_bytes(&[700; 2 * BLOCK]), 1000);
    assert_eq!(jitter.tick(1000, &mut out), None, "重新预填，先不回调");
    assert_eq!(jitter.tick(1020, &mut out), Some(Emit::Data));
    assert_eq!(out[0], frame::sample_from_pcm16(700));
}

// ── 5 ────────────────────────────────────────────────────────────────────────

/// 环满丢最旧，`dropped_ms` 增长，深度不变（照 `INPUT_QUEUE_SIZE` 的既有策略）。
#[test]
fn an_overrun_drops_the_oldest_block() {
    let cfg = PipeConfig::default(); // 环 = 160 ms = 2560 样本 = 8 块
    let mut jitter = JitterBuffer::new(cfg, RATE);
    let mut out = Vec::new();
    jitter.on_connected(0);

    // 10 块一起来（200 ms > 环的 160 ms）。
    let burst: Vec<i16> = (0..10 * BLOCK).map(|i| (i % 1000) as i16).collect();
    for (seq, chunk) in burst.chunks(BLOCK).enumerate() {
        jitter.on_frame(&header(seq as u32), &pcm_bytes(chunk), 0);
    }

    let stats = jitter.stats(0);
    assert_eq!(stats.dropped_ms, 40, "丢掉的正好是最旧的两块（2 × 20 ms）");
    assert_eq!(
        jitter.buffered_samples(),
        2560,
        "深度不变：上限就是 queue_ms"
    );

    // 丢的是**最旧**：第一块（样本 0..320）不在了，第一块出来的是样本 640 起。
    assert_eq!(jitter.tick(0, &mut out), None, "预填在第一次 tick 时满足");
    assert_eq!(jitter.tick(20, &mut out), Some(Emit::Data));
    assert_eq!(out[0], frame::sample_from_pcm16(640), "从第 641 个样本开始");
    assert_eq!(out[BLOCK - 1], frame::sample_from_pcm16(959));
}

// ── 6 ────────────────────────────────────────────────────────────────────────

/// 只有监听、没有对端：`on_chunk` 零调用、`padded_ms == 0`（"对端还没来" ≠ "抖动"）。
#[test]
fn no_peer_means_no_callback_at_all() {
    let listener = MediaListener::bind(options(PipeConfig::default())).expect("绑得上回环");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut capture = listener.capture();
    let counter = Arc::clone(&calls);
    capture
        .start(
            &net_target(),
            PipeConfig::default().block_ms,
            Box::new(move |_chunk: AudioChunk| {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("网络采集源认得 CaptureTarget::Net");

    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(calls.load(Ordering::Relaxed), 0, "没有对端就不该有回调");
    let stats = listener.stats();
    assert_eq!(stats.padded_ms, 0, "没对端不是抖动，不许补静音");
    assert!(!stats.connected);
    assert_eq!(stats.frames_in, 0);
    capture.stop();
}

// ── 7 ────────────────────────────────────────────────────────────────────────

/// 错 token → `401`（**不是**"连上再关"）；`Origin` 不在白名单 → `403`；
/// `allowed_origins` 为空时带 `Origin` 一律 `403`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_token_is_rejected_before_upgrade() {
    let mut options = options(PipeConfig::default());
    options.allowed_origins = vec!["http://allowed.example".to_string()];
    let listener = MediaListener::bind(options).expect("绑得上回环");
    let url = audio_url(&listener);

    // ① token 错 → 401（握手就断，客户端拿到的是 HTTP 错误，不是 101）。
    let err = connect_raw(&url, &[("Authorization", "Bearer wrong-token")])
        .await
        .expect_err("错 token 必须连不上");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
        other => panic!("该在 upgrade 之前回 401，实际是 {other:?}"),
    }

    // ② Origin 不在白名单 → 403（即使 token 是对的）。
    let err = connect_raw(
        &url,
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Origin", "http://evil.example"),
        ],
    )
    .await
    .expect_err("Origin 不在白名单必须连不上");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::FORBIDDEN),
        other => panic!("该回 403，实际是 {other:?}"),
    }

    // ③ 白名单为空（fail-closed）→ 带 Origin 的握手一律 403。
    let strict =
        MediaListener::bind(options_on(free_addr(), PipeConfig::default())).expect("绑得上回环");
    let err = connect_raw(
        &audio_url(&strict),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Origin", "http://allowed.example"),
        ],
    )
    .await
    .expect_err("空白名单 = 拒一切带 Origin 的握手");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::FORBIDDEN),
        other => panic!("该回 403，实际是 {other:?}"),
    }

    // ④ 白名单命中 + token 对 → 连得上（否则上面三条可能只是"什么都连不上"）。
    let (_ws, response) = connect_raw(
        &url,
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Origin", "http://allowed.example"),
        ],
    )
    .await
    .expect("白名单内的 Origin + 对 token 该连得上");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    assert_eq!(
        listener.stats().frames_in,
        0,
        "鉴权失败与成功的握手都不该产帧"
    );
}

// ── 8 ────────────────────────────────────────────────────────────────────────

/// `Sec-WebSocket-Protocol: vox.media.v1.<token>` 能连上，且 101 回同一个子协议名。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_browser_subprotocol_channel_works() {
    let listener = MediaListener::bind(options(PipeConfig::default())).expect("绑得上回环");
    let protocol = format!("{}{TOKEN}", vox_net::media::SUBPROTOCOL_PREFIX);

    {
        let (_ws, response) = connect_raw(
            &audio_url(&listener),
            &[("Sec-WebSocket-Protocol", protocol.as_str())],
        )
        .await
        .expect("浏览器那条通道该能连上");

        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            response
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|value| value.to_str().ok()),
            Some(protocol.as_str()),
            "101 里要回同一个子协议名（浏览器会校验）"
        );
    }
    // 上面那条连接收干净了再试下一条（一个监听同时只服务一条连接）。
    assert!(
        until(|| !listener.stats().connected, Duration::from_secs(3)).await,
        "上一条连接该收掉了"
    );

    // 子协议里塞错 token → 401（这条通道也认凭据，不是"有子协议就放行"）。
    let err = connect_raw(
        &audio_url(&listener),
        &[(
            "Sec-WebSocket-Protocol",
            &format!("{}nope", vox_net::media::SUBPROTOCOL_PREFIX),
        )],
    )
    .await
    .expect_err("子协议里 token 错就该被拒");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
        other => panic!("该回 401，实际是 {other:?}"),
    }
}

// ── 9 ────────────────────────────────────────────────────────────────────────

/// 帧头 `rate` 与声明率不符 → 断连 + `rate_mismatch` 计数（**不许**静默重采样）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rate_mismatch_kills_the_connection() {
    let listener = MediaListener::bind(options(PipeConfig::default())).expect("绑得上回环");
    let (mut ws, _) = connect_raw(
        &audio_url(&listener),
        &[("Authorization", &format!("Bearer {TOKEN}"))],
    )
    .await
    .expect("带对 token 该连得上");

    let payload = pcm_bytes(&[0; BLOCK]);
    let mut bytes = wire(FrameKind::Pcm16Le, 0, &payload);
    let wrong = 8_000u32.to_le_bytes();
    bytes[14..18].copy_from_slice(&wrong);
    send_frame(&mut ws, bytes).await;

    assert!(
        until(
            || listener.stats().rate_mismatch == 1,
            Duration::from_secs(3)
        )
        .await,
        "该记一次 rate_mismatch（实际 {:?}）",
        listener.stats()
    );
    assert!(connection_died(&mut ws).await, "率不符必须断连");
    let stats = listener.stats();
    assert_eq!(stats.bad_frames, 0, "率不符是会话不一致，不是坏帧");
    assert_eq!(stats.frames_in, 0, "被判错的帧不算「收到」");
}

// ── 10 ───────────────────────────────────────────────────────────────────────

/// 坏 magic / 版本 / `kind` / `flags` / `channels≠1` / 超长 / 文本帧：
/// 逐条 `bad_frames` + 断连。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_frame_is_counted_and_the_connection_dies() {
    let listener = MediaListener::bind(options(PipeConfig::default())).expect("绑得上回环");
    let url = audio_url(&listener);

    let good_payload = pcm_bytes(&[0; BLOCK]);
    let mut bad_magic = wire(FrameKind::Pcm16Le, 0, &good_payload);
    bad_magic[0] = b'X';
    let mut bad_version = wire(FrameKind::Pcm16Le, 0, &good_payload);
    bad_version[2] = 2;
    let mut bad_kind = wire(FrameKind::Pcm16Le, 0, &good_payload);
    bad_kind[3] = 9;
    let mut bad_flags = wire(FrameKind::Pcm16Le, 0, &good_payload);
    bad_flags[4] = 1;
    let mut bad_channels = wire(FrameKind::Pcm16Le, 0, &good_payload);
    bad_channels[18] = 2;
    let oversize = wire(FrameKind::Pcm16Le, 0, &vec![0; MAX_PAYLOAD + 1]);

    let binary_cases: Vec<(&str, Vec<u8>)> = vec![
        ("magic", bad_magic),
        ("版本", bad_version),
        ("kind", bad_kind),
        ("flags", bad_flags),
        ("channels", bad_channels),
        ("超长载荷", oversize),
    ];

    let mut expected = 0u64;
    for (what, bytes) in binary_cases {
        let (mut ws, _) = connect_raw(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
            .await
            .unwrap_or_else(|e| panic!("{what}：带对 token 该连得上，实际 {e:?}"));
        send_frame(&mut ws, bytes).await;
        expected += 1;
        assert!(
            until(
                || listener.stats().bad_frames == expected,
                Duration::from_secs(3)
            )
            .await,
            "{what}：该记一条 bad_frames（实际 {:?}）",
            listener.stats()
        );
        assert!(connection_died(&mut ws).await, "{what}：坏帧必须断连");
    }

    // 文本帧：不是"内容坏了"，是协议错误（音频管子不认协议方言）。
    let (mut ws, _) = connect_raw(&url, &[("Authorization", &format!("Bearer {TOKEN}"))])
        .await
        .expect("带对 token 该连得上");
    let _ = ws.send(Message::Text("{\"hello\":1}".into())).await;
    expected += 1;
    assert!(
        until(
            || listener.stats().bad_frames == expected,
            Duration::from_secs(3)
        )
        .await,
        "文本帧该记一条 bad_frames（实际 {:?}）",
        listener.stats()
    );
    assert!(connection_died(&mut ws).await, "文本帧必须断连");
    assert_eq!(listener.stats().rate_mismatch, 0);
}

// ── 11 ───────────────────────────────────────────────────────────────────────

/// `push` 攒成 `block_ms` 一帧；空闲 `keepalive_ms` 发一个 20 字节保活帧；`flush` 发余头。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sink_coalesces_and_sends_a_keepalive() {
    let listener = TokioTcpListener::bind("127.0.0.1:0")
        .await
        .expect("测试侧的裸服务端");
    let addr = listener.local_addr().expect("拿到端口");
    let seen: Arc<Mutex<Vec<(FrameKind, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (mut write, mut read) = ws.split();
        while let Some(Ok(message)) = read.next().await {
            if let Message::Binary(bytes) = message {
                match frame::decode(&bytes, RATE) {
                    Ok((head, payload)) => recorder
                        .lock()
                        .expect("锁没毒")
                        .push((head.kind, payload.len())),
                    Err(e) => panic!("出站侧发出来的帧必须是合法帧：{e}"),
                }
            }
        }
        let _ = write.close().await;
    });

    let pipe = PipeConfig {
        keepalive_ms: 250,
        ..PipeConfig::default()
    };
    let out = MediaOut::spawn(format!("ws://{addr}/audio"), TOKEN.to_string(), pipe)
        .expect("出站池起得来");
    let mut sink = out.sink();
    sink.open(None, RATE).expect("网络出口只认 device = None");

    // 不足一块：一帧都不发（攒块，别把 WS 打碎）。
    sink.push(&vec![0.1f32; 100]);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        seen.lock().expect("锁没毒").is_empty(),
        "不到一块不许发帧（实际 {:?}）",
        seen.lock().expect("锁没毒")
    );

    // 凑够一块（100 + 300 = 400 样本 → 一帧 320，余 80）。
    sink.push(&vec![0.1f32; 300]);
    assert!(
        until(
            || !seen.lock().expect("锁没毒").is_empty(),
            Duration::from_secs(2)
        )
        .await,
        "攒够一块该发一帧"
    );
    assert_eq!(
        seen.lock().expect("锁没毒")[0],
        (FrameKind::Pcm16Le, 2 * BLOCK),
        "一帧就是一块（320 样本 = 640 字节）"
    );

    // 空闲超过 keepalive_ms → 一个 20 字节的保活帧。
    assert!(
        until(
            || seen
                .lock()
                .expect("锁没毒")
                .iter()
                .any(|(kind, len)| *kind == FrameKind::KeepAlive && *len == 0),
            Duration::from_secs(3)
        )
        .await,
        "空闲 250 ms 该发一个保活帧（实际 {:?}）",
        seen.lock().expect("锁没毒")
    );

    // flush：余头（80 样本 = 160 字节）跟着一个保活收尾。
    sink.flush();
    assert!(
        until(
            || seen
                .lock()
                .expect("锁没毒")
                .iter()
                .any(|(kind, len)| *kind == FrameKind::Pcm16Le && *len == 160),
            Duration::from_secs(2)
        )
        .await,
        "flush 该把不足一块的余头发出去（实际 {:?}）",
        seen.lock().expect("锁没毒")
    );
    let seen = seen.lock().expect("锁没毒").clone();
    let stats = out.stats();
    assert_eq!(stats.dropped_ms, 0);
    assert_eq!(
        stats.frames_out as usize,
        seen.len(),
        "统计里的帧数要与对端真的收到的帧数一致"
    );
    assert!(seen.len() >= 3, "整块 + 余头 + 保活至少三帧，实际 {seen:?}");
    assert_eq!(
        sink.stats().rendered_samples,
        (BLOCK + 80) as u64,
        "真的写进 socket 的样本数 = 整块 + 余头"
    );
}

// ── 12 ───────────────────────────────────────────────────────────────────────
/// 对端先不在 → `connected == false` 但**不算失败**；对端起来后自动连上，`reconnects` 增长。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_outbound_pool_reconnects_with_backoff() {
    let addr = free_addr();
    let out = MediaOut::spawn(
        format!("ws://{addr}{}", vox_net::media::PATH),
        TOKEN.to_string(),
        PipeConfig::default(),
    )
    .expect("对端不在也要能把池子起起来");

    assert!(
        !out.stats().connected,
        "对端还没起来，此刻不该报连上（链路状态进统计，不进能力位）"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!out.stats().connected, "对端一直不在就该一直是 false");
    assert_eq!(out.stats().reconnects, 0, "还没连上过，不算重连");
    assert!(out.last_error().is_some(), "连不上要留下原因（链路状态）");

    // 绑上就算这个用例的"对端起来了"；绑完一直持有到用例结束。
    let _listener =
        MediaListener::bind(options_on(addr, PipeConfig::default())).expect("现在才把对端绑起来");
    assert!(
        until(|| out.stats().connected, Duration::from_secs(5)).await,
        "对端起来后该自动连上"
    );
    assert!(
        out.stats().reconnects >= 1,
        "从「连不上」恢复过来要记一次重连（实际 {}）",
        out.stats().reconnects
    );
    assert_eq!(
        out.last_error(),
        None,
        "连上之后要清掉：它说的是链路的**当前**状态"
    );
}

// ── 13（设计稿 §2.6.2 的行为契约表）─────────────────────────────────────────────

/// 两个端口只服务自己那一格，别的**报错**（不许静默换源 / 静默当成设备），
/// 一个监听同时只服务一条腿。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_media_ports_refuse_what_they_do_not_serve() {
    let listener = MediaListener::bind(options(PipeConfig::default())).expect("绑得上回环");

    // 采集源：只认 `CaptureTarget::Net`。
    let mut capture = listener.capture();
    let err = capture
        .start(
            &CaptureTarget::Microphone(None),
            20,
            Box::new(|_chunk: AudioChunk| {}),
        )
        .expect_err("设备采集目标不归媒体面");
    assert!(err.message.contains("Net"), "报错要说清只认哪一格：{err}");
    let err = capture
        .start(
            &CaptureTarget::Net {
                pipe: "not-default".to_string(),
            },
            20,
            Box::new(|_chunk: AudioChunk| {}),
        )
        .expect_err("未知管道名要在装配期报错，不许当成 default");
    assert!(err.message.contains("not-default"), "报错要点名：{err}");
    capture
        .start(&net_target(), 20, Box::new(|_chunk: AudioChunk| {}))
        .expect("认自己那一格");

    // 第二条腿：同一个监听同时只能服务一条腿。
    let mut second = listener.capture();
    let err = second
        .start(&net_target(), 20, Box::new(|_chunk: AudioChunk| {}))
        .expect_err("第二条腿该报错，不许静默抢连接");
    assert!(err.message.contains("另一条腿"), "报错要说清原因：{err}");

    // 释放之后能重新开工（stop 要放掉腿槽位）。
    capture.stop();
    second
        .start(&net_target(), 20, Box::new(|_chunk: AudioChunk| {}))
        .expect("上一条腿停了之后，槽位该空出来");
    second.stop();

    // 播放汇：网络出口不是设备。
    let out = MediaOut::spawn(
        format!("ws://{}", listener.local_addr()),
        TOKEN.to_string(),
        PipeConfig::default(),
    )
    .expect("出站池起得来");
    let mut sink = out.sink();
    let err = sink
        .open(Some("某个声卡"), RATE)
        .expect_err("网络出口不是设备");
    assert!(
        err.message.contains("device"),
        "报错要说清只认哪一格：{err}"
    );
    sink.open(None, RATE).expect("device = None 才认");

    // 对端地址的 scheme 在这里就拦，别拿着死地址一直退避重连。
    let err = match MediaOut::spawn(
        "http://127.0.0.1:1/audio".to_string(),
        TOKEN.to_string(),
        PipeConfig::default(),
    ) {
        Err(e) => e,
        Ok(_) => panic!("非 ws/wss 的对端该在装配期报错"),
    };
    assert!(err.message.contains("ws://"), "报错要说清白名单：{err}");
}
