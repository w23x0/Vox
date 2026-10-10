//! 测试替身：本机上的 HTTP CONNECT 代理与最小 SOCKS5 代理（std 线程，不联网）。
//!
//! 只在 `cfg(test)` 或开 `testing` feature 时编译；`vox-net` / `vox-host` 的测试靠它验证
//! "流量真的走了代理"。代理会记下每次请求的目标，测试据此断言"域名是交给代理解析的"。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;

/// 一个在跑的假代理。线程随进程结束，不需要手动停。
pub struct FakeProxy {
    pub addr: SocketAddr,
    /// 每条隧道请求的目标，HTTP 记 `CONNECT host:port`，SOCKS5 记 `domain:host:port` / `ip:addr:port`。
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl FakeProxy {
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// `names`：代理"自己知道"的域名 → 真实地址（模拟只有代理那一侧才解析得出的域名）。
pub type Hosts = Vec<(String, SocketAddr)>;

fn open_target(host: &str, port: u16, hosts: &Hosts) -> std::io::Result<TcpStream> {
    if let Some((_, addr)) = hosts.iter().find(|(name, _)| name == host) {
        return TcpStream::connect(addr);
    }
    TcpStream::connect((host, port))
}

fn pipe(a: TcpStream, b: TcpStream) {
    let (mut a_read, mut b_write) = (a.try_clone().unwrap(), b.try_clone().unwrap());
    let (mut b_read, mut a_write) = (b, a);
    let up = thread::spawn(move || {
        let _ = std::io::copy(&mut a_read, &mut b_write);
        let _ = b_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b_read, &mut a_write);
    let _ = a_write.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
}

/// HTTP CONNECT 代理。`auth` 设了就要求 `Proxy-Authorization: Basic`，不对回 407。
pub fn http_connect_proxy(auth: Option<(&str, &str)>, hosts: Hosts) -> FakeProxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let want = auth.map(|(u, p)| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
        )
    });
    thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let (log, want, hosts) = (log.clone(), want.clone(), hosts.clone());
            thread::spawn(move || {
                let mut client = client;
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let first = text.lines().next().unwrap_or("").to_string();
                log.lock().unwrap().push(first.clone());
                if let Some(want) = want {
                    let ok = text
                        .lines()
                        .any(|l| l.eq_ignore_ascii_case(&format!("Proxy-Authorization: {want}")));
                    if !ok {
                        let _ = client.write_all(
                            b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                        );
                        return;
                    }
                }
                let authority = first.split_whitespace().nth(1).unwrap_or("");
                let (host, port) = authority.rsplit_once(':').unwrap_or((authority, "0"));
                let host = host.trim_matches(['[', ']']);
                match open_target(host, port.parse().unwrap_or(0), &hosts) {
                    Ok(upstream) => {
                        let _ = client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                        pipe(client, upstream);
                    }
                    Err(_) => {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                    }
                }
            });
        }
    });
    FakeProxy { addr, requests }
}

/// 最小 SOCKS5 代理（CONNECT；无认证，或用户名密码）。
pub fn socks5_proxy(auth: Option<(&str, &str)>, hosts: Hosts) -> FakeProxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let want = auth.map(|(u, p)| (u.to_string(), p.to_string()));
    thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let (log, want, hosts) = (log.clone(), want.clone(), hosts.clone());
            thread::spawn(move || {
                let _ = serve_socks5(client, &log, want, &hosts);
            });
        }
    });
    FakeProxy { addr, requests }
}

fn serve_socks5(
    mut c: TcpStream,
    log: &Mutex<Vec<String>>,
    want: Option<(String, String)>,
    hosts: &Hosts,
) -> std::io::Result<()> {
    let mut hello = [0u8; 2];
    c.read_exact(&mut hello)?;
    let mut methods = vec![0u8; hello[1] as usize];
    c.read_exact(&mut methods)?;
    match &want {
        None => c.write_all(&[5, 0])?,
        Some((user, pass)) => {
            if !methods.contains(&2) {
                return c.write_all(&[5, 0xFF]);
            }
            c.write_all(&[5, 2])?;
            let mut ver = [0u8; 2];
            c.read_exact(&mut ver)?;
            let mut u = vec![0u8; ver[1] as usize];
            c.read_exact(&mut u)?;
            let mut plen = [0u8; 1];
            c.read_exact(&mut plen)?;
            let mut p = vec![0u8; plen[0] as usize];
            c.read_exact(&mut p)?;
            if u != user.as_bytes() || p != pass.as_bytes() {
                return c.write_all(&[1, 1]);
            }
            c.write_all(&[1, 0])?;
        }
    }
    let mut req = [0u8; 4];
    c.read_exact(&mut req)?;
    let (label, host): (String, String) = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            c.read_exact(&mut a)?;
            let ip = Ipv4Addr::from(a).to_string();
            (format!("ip:{ip}"), ip)
        }
        4 => {
            let mut a = [0u8; 16];
            c.read_exact(&mut a)?;
            let ip = Ipv6Addr::from(a).to_string();
            (format!("ip:{ip}"), ip)
        }
        3 => {
            let mut len = [0u8; 1];
            c.read_exact(&mut len)?;
            let mut name = vec![0u8; len[0] as usize];
            c.read_exact(&mut name)?;
            let name = String::from_utf8_lossy(&name).into_owned();
            (format!("domain:{name}"), name)
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    c.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    log.lock().unwrap().push(format!("{label}:{port}"));
    match open_target(&host, port, hosts) {
        Ok(upstream) => {
            c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
            pipe(c, upstream);
        }
        Err(_) => c.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0])?,
    }
    Ok(())
}

/// 回环上一个会回显固定字节的 TCP 服务（测隧道通不通用）。
pub fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut buf = [0u8; 256];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 || s.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}
