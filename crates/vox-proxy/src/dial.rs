//! 同步拨号：直连，或经 HTTP CONNECT / SOCKS5 隧道。分阶段计时、分阶段报错。

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::spec::{ProxyScheme, ProxySpec};

/// 失败在哪一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 本地解析目标域名（经 HTTP 代理或 `socks5h` 时没有这一段）。
    Dns,
    /// TCP 连接（经代理时是连代理本身）。
    Connect,
    /// 代理握手（CONNECT / SOCKS5）。
    Tunnel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialError {
    pub stage: Stage,
    /// 超时（而非明确拒绝 / 解析失败）。
    pub timed_out: bool,
    /// 不含账号密码。
    pub message: String,
}

impl fmt::Display for DialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DialError {}

impl DialError {
    fn new(stage: Stage, error: &io::Error, what: &str) -> Self {
        Self {
            stage,
            timed_out: matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            message: format!("{what}：{error}"),
        }
    }

    fn timeout(stage: Stage, what: &str) -> Self {
        Self {
            stage,
            timed_out: true,
            message: format!("{what}: timed out"),
        }
    }
}

/// 拨号结果：连好的流，加各段耗时。
#[derive(Debug)]
pub struct Dialed {
    pub stream: TcpStream,
    pub dns: Option<Duration>,
    pub connect: Duration,
    pub tunnel: Option<Duration>,
}

/// 连到 `host:port`。`proxy = None` 直连；否则经代理隧道。`timeout` 是整个拨号的总预算。
pub fn dial(
    proxy: Option<&ProxySpec>,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<Dialed, DialError> {
    let started = Instant::now();
    let left = |stage, what: &str| {
        timeout
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| DialError::timeout(stage, what))
    };

    let Some(proxy) = proxy else {
        let t = Instant::now();
        let addrs = resolve(host, port)
            .map_err(|e| DialError::new(Stage::Dns, &e, "DNS resolution failed"))?;
        let dns = t.elapsed();
        let t = Instant::now();
        let stream = connect_any(&addrs, left(Stage::Connect, "connect")?, "connect")?;
        return Ok(Dialed {
            stream,
            dns: Some(dns),
            connect: t.elapsed(),
            tunnel: None,
        });
    };

    // 非 socks5h：目标域名在本地解析（HTTP 代理与 socks5h 把域名原样交给代理）。
    let local_dns = matches!(proxy.scheme, ProxyScheme::Socks5 { remote_dns: false });
    let (target_ip, dns) = if local_dns {
        let t = Instant::now();
        let ip = resolve(host, port)
            .map_err(|e| DialError::new(Stage::Dns, &e, "DNS resolution failed"))?
            .first()
            .map(SocketAddr::ip)
            .ok_or_else(|| DialError {
                stage: Stage::Dns,
                timed_out: false,
                message: "DNS resolution failed: no address returned".into(),
            })?;
        (Some(ip), Some(t.elapsed()))
    } else {
        (None, None)
    };

    let t = Instant::now();
    let proxy_addrs = resolve(&proxy.host, proxy.port)
        .map_err(|e| DialError::new(Stage::Connect, &e, "could not resolve the proxy address"))?;
    let mut stream = connect_any(
        &proxy_addrs,
        left(Stage::Connect, "connect to proxy")?,
        "connect to proxy",
    )?;
    let connect = t.elapsed();

    let t = Instant::now();
    let budget = left(Stage::Tunnel, "proxy handshake")?;
    stream
        .set_read_timeout(Some(budget))
        .and_then(|()| stream.set_write_timeout(Some(budget)))
        .map_err(|e| DialError::new(Stage::Tunnel, &e, "proxy handshake"))?;
    handshake(&mut stream, proxy, host, port, target_ip)
        .map_err(|e| DialError::new(Stage::Tunnel, &e, "proxy handshake failed"))?;
    stream
        .set_read_timeout(None)
        .and_then(|()| stream.set_write_timeout(None))
        .map_err(|e| DialError::new(Stage::Tunnel, &e, "proxy handshake"))?;
    Ok(Dialed {
        stream,
        dns,
        connect,
        tunnel: Some(t.elapsed()),
    })
}

fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no address returned",
        ));
    }
    Ok(addrs)
}

/// 逐个地址试，总预算 `budget` 平分（至少 200ms）。
fn connect_any(addrs: &[SocketAddr], budget: Duration, what: &str) -> Result<TcpStream, DialError> {
    let started = Instant::now();
    let mut last: Option<io::Error> = None;
    for (i, addr) in addrs.iter().enumerate() {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let share = (remaining / (addrs.len() - i) as u32).max(Duration::from_millis(200));
        match TcpStream::connect_timeout(addr, share.min(remaining)) {
            Ok(stream) => {
                // 实时音频是小帧；与原 tokio 路径一样关 Nagle。
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => DialError::new(Stage::Connect, &e, what),
        None => DialError::timeout(Stage::Connect, what),
    })
}

/// 在已连上代理的 `stream` 上握手，让它变成通往 `host:port` 的隧道。只给 [`dial`] 用。
///
/// `resolved`：`socks5://`（非 h）已在本地解析出的目标 IP；其余情形传 `None`。
/// 调用方负责给 `stream` 设读写超时。
fn handshake<S: Read + Write>(
    stream: &mut S,
    proxy: &ProxySpec,
    host: &str,
    port: u16,
    resolved: Option<IpAddr>,
) -> io::Result<()> {
    match proxy.scheme {
        ProxyScheme::Http => http_connect(stream, proxy, host, port),
        ProxyScheme::Socks5 { remote_dns } => {
            let target = match (remote_dns, resolved, host.parse::<IpAddr>()) {
                (_, _, Ok(ip)) => Target::Ip(ip),
                (false, Some(ip), _) => Target::Ip(ip),
                (false, None, _) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "socks5 (not socks5h) must resolve the target address locally first",
                    ))
                }
                (true, _, _) => Target::Domain(host),
            };
            socks5(stream, proxy, target, port)
        }
    }
}

// --- HTTP CONNECT -----------------------------------------------------------

fn http_connect<S: Read + Write>(
    stream: &mut S,
    proxy: &ProxySpec,
    host: &str,
    port: u16,
) -> io::Result<()> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some((user, pass)) = &proxy.auth {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    // 只读到空行为止：多读一个字节就会吞掉隧道里 TLS 服务端的首包。
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy response header is too long",
            ));
        }
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy closed the connection during the handshake",
            ));
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    let status_line = line.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "proxy response is not HTTP"))?;
    match status {
        200..=299 => Ok(()),
        407 => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "proxy requires credentials (HTTP 407); username or password is wrong",
        )),
        other => Err(io::Error::other(format!(
            "proxy refused the tunnel (HTTP {other})"
        ))),
    }
}

// --- SOCKS5 -----------------------------------------------------------------

enum Target<'a> {
    Domain(&'a str),
    Ip(IpAddr),
}

fn socks5<S: Read + Write>(
    stream: &mut S,
    proxy: &ProxySpec,
    target: Target<'_>,
    port: u16,
) -> io::Result<()> {
    let bad = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());

    // 问候：只提议"无认证"，有账号时再加"用户名密码"。
    if proxy.auth.is_some() {
        stream.write_all(&[5, 2, 0x00, 0x02])?;
    } else {
        stream.write_all(&[5, 1, 0x00])?;
    }
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply)?;
    if reply[0] != 5 {
        return Err(bad("peer is not a SOCKS5 proxy"));
    }
    match reply[1] {
        0x00 => {}
        0x02 => {
            let (user, pass) = proxy
                .auth
                .as_ref()
                .ok_or_else(|| bad("proxy requires credentials but none are set"))?;
            let mut msg = vec![1, user.len() as u8];
            msg.extend_from_slice(user.as_bytes());
            msg.push(pass.len() as u8);
            msg.extend_from_slice(pass.as_bytes());
            stream.write_all(&msg)?;
            let mut status = [0u8; 2];
            stream.read_exact(&mut status)?;
            if status[1] != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SOCKS5 proxy rejected the username or password",
                ));
            }
        }
        0xFF => {
            return Err(bad(
                "proxy accepts none of the offered authentication methods",
            ))
        }
        other => {
            return Err(bad(&format!(
                "proxy chose an unknown authentication method 0x{other:02x}"
            )))
        }
    }

    // CONNECT 请求。
    let mut req = vec![5, 1, 0];
    match target {
        Target::Ip(IpAddr::V4(ip)) => {
            req.push(1);
            req.extend_from_slice(&ip.octets());
        }
        Target::Ip(IpAddr::V6(ip)) => {
            req.push(4);
            req.extend_from_slice(&ip.octets());
        }
        Target::Domain(name) => {
            if name.len() > 255 {
                return Err(bad("host name is too long"));
            }
            req.push(3);
            req.push(name.len() as u8);
            req.extend_from_slice(name.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req)?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[0] != 5 {
        return Err(bad("malformed proxy reply"));
    }
    if head[1] != 0 {
        return Err(io::Error::other(socks5_reply_text(head[1])));
    }
    // 把绑定地址读完，别留在流里污染后面的 TLS。
    let tail = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            len[0] as usize + 2
        }
        _ => return Err(bad("proxy reply has an unknown address type")),
    };
    let mut skip = vec![0u8; tail];
    stream.read_exact(&mut skip)?;
    Ok(())
}

fn socks5_reply_text(code: u8) -> String {
    let text = match code {
        1 => "proxy failure",
        2 => "connection not allowed by proxy rules",
        3 => "network unreachable from proxy",
        4 => "host unreachable from proxy",
        5 => "connection refused by target",
        6 => "TTL expired",
        7 => "command not supported by proxy",
        8 => "address type not supported by proxy",
        _ => "unknown error",
    };
    format!("SOCKS5 proxy refused the connection ({code}): {text}")
}

#[cfg(test)]
mod tests;
