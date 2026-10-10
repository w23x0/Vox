//! 网络体检：`test_network`（测速）与 `detect_takeover`（代理软件接管检测）。
//!
//! 契约见 `docs/plans/NET.md` 与 `app/ui/src/types.ts` 的 `NetworkReport` / `TakeoverReport`
//! （字段名逐字一致）。
//!
//! **测速**：对一家服务商的每个候选接入点，按当前网络设置分别量
//! DNS → TCP → （代理隧道）→ TLS → 一次**不带密钥**的 HTTPS GET（预期 401/403，
//! 收到任何 HTTP 应答就算通），各段耗时或错误分类（dns / connect / tls / http / timeout）。
//! 各接入点并行测，互不拖累。拨号这一段与 WebSocket 传输共用 `vox-proxy::dial`，
//! 所以"测速通"与"真连得上"走的是同一条路。
//!
//! **接管检测**（只检测 + 引导，**绝不尝试绕过**）：
//! - 服务商域名解析到 `198.18.0.0/15` = 代理软件的"假 IP"模式；
//! - 网卡名像代理软件的虚拟网卡（Linux 读 `/sys/class/net`；Windows 用自带的
//!   `netsh interface show interface`，不新增依赖；其它平台只做假 IP 判定）。
//!
//! 不引 tokio：阻塞式，调用方放到工作线程里跑。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vox_core::settings::{AliyunRegion, ModelProvider, NetworkMode, NetworkSetting};
use vox_proxy::Stage;

/// 单个接入点的总预算。
const ENDPOINT_BUDGET: Duration = Duration::from_secs(8);
/// 域名解析（接管检测用）的预算。
const RESOLVE_BUDGET: Duration = Duration::from_secs(3);

/// 阿里云建议换地区的门槛：另一地区至少快这么多倍、且至少快这么多毫秒。
const SUGGEST_RATIO: f64 = 1.5;
const SUGGEST_MIN_GAIN_MS: u32 = 80;

// --- 契约类型 ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Dns,
    Connect,
    Tls,
    Http,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointResult {
    /// 稳定 id：阿里云 `cn` / `intl` / `hk`，其余 `default`。
    pub id: String,
    pub host: String,
    /// 是否就是当前设置正在用的那个接入点。
    pub current: bool,
    pub dns_ms: Option<u32>,
    pub connect_ms: Option<u32>,
    /// 代理握手（经代理时才有）。
    pub tunnel_ms: Option<u32>,
    pub tls_ms: Option<u32>,
    pub http_ms: Option<u32>,
    pub http_status: Option<u16>,
    /// 各段之和（成功时才有）。
    pub total_ms: Option<u32>,
    pub error: Option<Failure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// 更近的接入点 id / host，以及两边的总耗时。
    pub id: String,
    pub host: String,
    pub current_ms: u32,
    pub nearest_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkReport {
    pub provider: ModelProvider,
    pub mode: NetworkMode,
    /// 实际使用的代理（不含账号密码）；直连 / 没探测到系统代理 = `None`。
    pub via: Option<String>,
    pub results: Vec<EndpointResult>,
    /// 只有阿里云、且另一地区明显更近时才有。
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TakeoverReport {
    /// 假 IP 或代理软件网卡，任一命中。
    pub detected: bool,
    /// 有域名解析进了 `198.18.0.0/15`。
    pub fake_ip: bool,
    pub fake_ip_hosts: Vec<String>,
    /// 像代理软件的网卡名。
    pub interfaces: Vec<String>,
    /// 本平台是否能列网卡（不能的话 `interfaces` 恒空，只有假 IP 判定可信）。
    pub interfaces_scanned: bool,
}

// --- 候选接入点 -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub id: &'static str,
    pub host: String,
    pub port: u16,
    pub path: &'static str,
    pub current: bool,
}

fn ep(id: &'static str, host: &str, path: &'static str, current: bool) -> Endpoint {
    Endpoint {
        id,
        host: host.to_string(),
        port: 443,
        path,
        current,
    }
}

/// 一家服务商的候选接入点。`region` 只对阿里云有意义（标出"当前"是哪个）。
pub fn endpoints_of(provider: ModelProvider, region: AliyunRegion) -> Vec<Endpoint> {
    const ALIYUN_PATH: &str = "/api/v1/models?page_size=1";
    match provider {
        ModelProvider::Aliyun => vec![
            ep(
                "cn",
                "dashscope.aliyuncs.com",
                ALIYUN_PATH,
                region == AliyunRegion::Cn,
            ),
            ep(
                "intl",
                "dashscope-intl.aliyuncs.com",
                ALIYUN_PATH,
                region == AliyunRegion::Intl,
            ),
            ep(
                "hk",
                "cn-hongkong.dashscope.aliyuncs.com",
                ALIYUN_PATH,
                false,
            ),
        ],
        ModelProvider::Gpt => vec![ep("default", "api.openai.com", "/v1/models", true)],
        ModelProvider::Gemini => vec![ep(
            "default",
            "generativelanguage.googleapis.com",
            "/v1beta/models",
            true,
        )],
    }
}

// --- 测速 -------------------------------------------------------------------

/// 共享实现（Tauri 命令与无屏档都能调）。
pub fn test_network(
    provider: ModelProvider,
    network: &NetworkSetting,
    region: AliyunRegion,
) -> NetworkReport {
    run_test(
        provider,
        network,
        &endpoints_of(provider, region),
        default_tls(),
        ENDPOINT_BUDGET,
    )
}

fn run_test(
    provider: ModelProvider,
    network: &NetworkSetting,
    endpoints: &[Endpoint],
    tls: Arc<rustls::ClientConfig>,
    budget: Duration,
) -> NetworkReport {
    let (tx, rx) = mpsc::channel::<(usize, EndpointResult, Option<String>)>();
    for (index, endpoint) in endpoints.iter().cloned().enumerate() {
        let (tx, tls, network) = (tx.clone(), tls.clone(), network.clone());
        std::thread::spawn(move || {
            let (result, via) = measure(&endpoint, &network, &tls, budget);
            let _ = tx.send((index, result, via));
        });
    }
    drop(tx);

    // 兜底：某个线程卡在系统 DNS 里（`getaddrinfo` 没法取消）时，总时限一到就报超时，
    // 不让整张表一起挂着。
    let deadline = Instant::now() + budget + Duration::from_secs(1);
    let mut slots: Vec<Option<(EndpointResult, Option<String>)>> = vec![None; endpoints.len()];
    while slots.iter().any(Option::is_none) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((index, result, via)) => slots[index] = Some((result, via)),
            Err(_) => break,
        }
    }
    let mut via = None;
    let results: Vec<EndpointResult> = slots
        .into_iter()
        .zip(endpoints)
        .map(|(slot, endpoint)| match slot {
            Some((result, v)) => {
                via = via.take().or(v);
                result
            }
            None => failed(
                endpoint,
                FailureKind::Timeout,
                "timed out: the system resolver did not return".into(),
            ),
        })
        .collect();

    let suggestion = (provider == ModelProvider::Aliyun)
        .then(|| suggest(&results))
        .flatten();
    NetworkReport {
        provider,
        mode: network.mode,
        via,
        results,
        suggestion,
    }
}

fn failed(endpoint: &Endpoint, kind: FailureKind, message: String) -> EndpointResult {
    EndpointResult {
        id: endpoint.id.to_string(),
        host: endpoint.host.clone(),
        current: endpoint.current,
        dns_ms: None,
        connect_ms: None,
        tunnel_ms: None,
        tls_ms: None,
        http_ms: None,
        http_status: None,
        total_ms: None,
        error: Some(Failure { kind, message }),
    }
}

fn ms(d: Duration) -> u32 {
    d.as_millis().min(u128::from(u32::MAX)) as u32
}

/// 测一个接入点。返回结果与实际使用的代理显示串。
fn measure(
    endpoint: &Endpoint,
    network: &NetworkSetting,
    tls: &Arc<rustls::ClientConfig>,
    budget: Duration,
) -> (EndpointResult, Option<String>) {
    let started = Instant::now();
    let proxy = match vox_proxy::resolve(network, &endpoint.host) {
        Ok(p) => p,
        Err(e) => {
            return (
                failed(
                    endpoint,
                    FailureKind::Connect,
                    format!("invalid proxy settings: {e}"),
                ),
                None,
            )
        }
    };
    let via = proxy.as_ref().map(vox_proxy::ProxySpec::display);
    let mut result = failed(endpoint, FailureKind::Connect, String::new());
    result.error = None;

    let dialed = match vox_proxy::dial(proxy.as_ref(), &endpoint.host, endpoint.port, budget) {
        Ok(d) => d,
        Err(e) => {
            let kind = match (e.timed_out, e.stage) {
                (true, _) => FailureKind::Timeout,
                (false, Stage::Dns) => FailureKind::Dns,
                (false, Stage::Connect | Stage::Tunnel) => FailureKind::Connect,
            };
            return (failed(endpoint, kind, e.message), via);
        }
    };
    result.dns_ms = dialed.dns.map(ms);
    result.connect_ms = Some(ms(dialed.connect));
    result.tunnel_ms = dialed.tunnel.map(ms);

    match tls_get(
        dialed.stream,
        endpoint,
        tls,
        budget.saturating_sub(started.elapsed()),
    ) {
        Ok((tls_time, http_time, status)) => {
            result.tls_ms = Some(ms(tls_time));
            result.http_ms = Some(ms(http_time));
            result.http_status = Some(status);
            result.total_ms = Some(
                result.dns_ms.unwrap_or(0)
                    + result.connect_ms.unwrap_or(0)
                    + result.tunnel_ms.unwrap_or(0)
                    + ms(tls_time)
                    + ms(http_time),
            );
        }
        Err((kind, message)) => {
            result.error = Some(Failure { kind, message });
        }
    }
    (result, via)
}

fn io_kind(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// TLS 握手 + 一次无密钥 GET。返回（TLS 耗时, 首个应答行耗时, 状态码）。
fn tls_get(
    mut sock: std::net::TcpStream,
    endpoint: &Endpoint,
    tls: &Arc<rustls::ClientConfig>,
    budget: Duration,
) -> Result<(Duration, Duration, u16), (FailureKind, String)> {
    let started = Instant::now();
    let left = || {
        budget
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or((FailureKind::Timeout, "timed out".to_string()))
    };
    let set = |sock: &std::net::TcpStream, d: Duration| {
        sock.set_read_timeout(Some(d))
            .and_then(|()| sock.set_write_timeout(Some(d)))
            .map_err(|e| (FailureKind::Connect, e.to_string()))
    };

    let name = rustls::pki_types::ServerName::try_from(endpoint.host.clone())
        .map_err(|e| (FailureKind::Tls, format!("invalid host name: {e}")))?;
    let mut conn = rustls::ClientConnection::new(tls.clone(), name)
        .map_err(|e| (FailureKind::Tls, format!("TLS handshake failed: {e}")))?;
    set(&sock, left()?)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(|e| {
            if io_kind(&e) {
                (FailureKind::Timeout, "TLS handshake timed out".to_string())
            } else {
                (FailureKind::Tls, format!("TLS handshake failed: {e}"))
            }
        })?;
        set(&sock, left()?)?;
    }
    let tls_time = started.elapsed();

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: vox-netcheck\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        endpoint.path, endpoint.host
    );
    let http_started = Instant::now();
    let mut stream = rustls::Stream::new(&mut conn, &mut sock);
    let http_err = |e: std::io::Error| {
        if io_kind(&e) {
            (
                FailureKind::Timeout,
                "timed out waiting for the server reply".to_string(),
            )
        } else {
            (FailureKind::Http, format!("HTTP request failed: {e}"))
        }
    };
    stream.write_all(request.as_bytes()).map_err(http_err)?;
    let mut head = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n") && head.len() < 512 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => head.push(byte[0]),
            // 服务端不发 close_notify 就断：已经读到的行够用。
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !head.is_empty() => break,
            Err(e) => return Err(http_err(e)),
        }
    }
    let http_time = http_started.elapsed();
    let line = String::from_utf8_lossy(&head);
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| {
            (
                FailureKind::Http,
                "the server reply is not HTTP".to_string(),
            )
        })?;
    Ok((tls_time, http_time, status))
}

fn default_tls() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|b| b.with_root_certificates(roots).with_no_client_auth());
    // `ring` 提供者 + 默认协议版本不会失败；失败只可能是编译配置错了，直接让它崩在测试里。
    Arc::new(config.expect("rustls ring provider supports the default protocol versions"))
}

/// 阿里云：另一地区比当前地区快 1.5 倍以上且差 80ms 以上才建议换。
fn suggest(results: &[EndpointResult]) -> Option<Suggestion> {
    let current = results.iter().find(|r| r.current)?;
    let current_ms = current.total_ms?;
    let best = results
        .iter()
        .filter(|r| !r.current)
        .filter_map(|r| r.total_ms.map(|t| (r, t)))
        .min_by_key(|(_, t)| *t)?;
    let (nearest, nearest_ms) = best;
    let much_faster = f64::from(current_ms) >= f64::from(nearest_ms) * SUGGEST_RATIO;
    let enough_gain = current_ms.saturating_sub(nearest_ms) >= SUGGEST_MIN_GAIN_MS;
    (much_faster && enough_gain).then(|| Suggestion {
        id: nearest.id.clone(),
        host: nearest.host.clone(),
        current_ms,
        nearest_ms,
    })
}

// --- 接管检测 ---------------------------------------------------------------

/// 代理软件"假 IP"模式用的保留段：`198.18.0.0/15`（RFC 2544 基准测试段）。
pub fn is_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 198 && (b == 18 || b == 19)
        }
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .is_some_and(|v4: Ipv4Addr| is_fake_ip(IpAddr::V4(v4))),
    }
}

/// Linux / macOS 常见的代理软件虚拟网卡名。
pub fn unix_tun_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("tun")
        || lower.starts_with("utun")
        || lower.starts_with("clash")
        || lower == "meta"
}

/// Windows 适配器名（含空格与本地化的前后缀，用包含匹配）。
pub fn windows_tun_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["meta", "clash", "wintun", "tap"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// 解析 `netsh interface show interface` 的输出，取出适配器名（前 3 列是状态与类型，
/// 其余是名字，名字里可以有空格）。
pub fn parse_netsh_adapters(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (_, _, _) = (words.next()?, words.next()?, words.next()?);
            let name: Vec<&str> = words.collect();
            (!name.is_empty()).then(|| name.join(" "))
        })
        .collect()
}

/// 本机网卡名。`None` = 本平台没法列。
fn adapter_names() -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::read_dir("/sys/class/net").ok()?;
        Some(
            dir.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("netsh")
            .args(["interface", "show", "interface"])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| parse_netsh_adapters(&String::from_utf8_lossy(&out.stdout)))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

/// 带时限的系统域名解析（`getaddrinfo` 没法取消，超时就丢下那个线程）。
fn resolve_with_budget(host: &str) -> Vec<IpAddr> {
    let (tx, rx) = mpsc::channel();
    let host = host.to_string();
    std::thread::spawn(move || {
        let ips: Vec<IpAddr> = (host.as_str(), 443)
            .to_socket_addrs()
            .map(|it| it.map(|a| a.ip()).collect())
            .unwrap_or_default();
        let _ = tx.send(ips);
    });
    rx.recv_timeout(RESOLVE_BUDGET).unwrap_or_default()
}

/// 共享实现：解析这家服务商的候选域名 + 扫网卡。
pub fn detect_takeover(provider: ModelProvider) -> TakeoverReport {
    let hosts: Vec<String> = endpoints_of(provider, AliyunRegion::Cn)
        .into_iter()
        .map(|e| e.host)
        .collect();
    let threads: Vec<_> = hosts
        .iter()
        .cloned()
        .map(|host| {
            std::thread::spawn(move || {
                let ips = resolve_with_budget(&host);
                (host, ips)
            })
        })
        .collect();
    let resolved: Vec<(String, Vec<IpAddr>)> = collect_resolved(threads);
    takeover_from(&resolved, adapter_names(), cfg!(windows))
}

/// 收齐解析线程的结果：一条线程一条记录。
///
/// 解析失败在 [`resolve_with_budget`] 里就已经是"空地址表"，走得到这里的只有 panic。
/// 那时给一条空记录，**不能把域名整条丢掉**——漏一个域名在报告里和"这台机器没被
/// 接管"长得一模一样，检测类功能假阴性比假阳性更难发现。
fn collect_resolved(
    threads: Vec<std::thread::JoinHandle<(String, Vec<IpAddr>)>>,
) -> Vec<(String, Vec<IpAddr>)> {
    threads
        .into_iter()
        .map(|t| t.join().unwrap_or_default())
        .collect()
}

/// 纯函数：解析结果 + 网卡名 → 报告。
///
/// `is_windows` 决定用哪一套网卡名判据（[`adapter_names`] 一边读 `/sys/class/net`、
/// 一边跑 `netsh`，两边的名字长相完全不同）。**由调用方显式给**：生产传编译期的
/// `cfg!(windows)`；测试传显式平台，断言不随编译目标漂移。
pub fn takeover_from(
    resolved: &[(String, Vec<IpAddr>)],
    adapters: Option<Vec<String>>,
    is_windows: bool,
) -> TakeoverReport {
    let fake_ip_hosts: Vec<String> = resolved
        .iter()
        .filter(|(_, ips)| ips.iter().any(|ip| is_fake_ip(*ip)))
        .map(|(host, _)| host.clone())
        .collect();
    let interfaces_scanned = adapters.is_some();
    let interfaces: Vec<String> = adapters
        .unwrap_or_default()
        .into_iter()
        .filter(|n| {
            if is_windows {
                windows_tun_name(n)
            } else {
                unix_tun_name(n)
            }
        })
        .collect();
    let fake_ip = !fake_ip_hosts.is_empty();
    TakeoverReport {
        detected: fake_ip || !interfaces.is_empty(),
        fake_ip,
        fake_ip_hosts,
        interfaces,
        interfaces_scanned,
    }
}

#[cfg(test)]
mod tests;
