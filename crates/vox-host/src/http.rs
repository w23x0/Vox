//! 阻塞式 HTTP 的网络出口：让 `ureq` 的拨号走 `vox-proxy::dial`。
//!
//! 为什么不用 ureq 自带的 `Proxy` 配置（N2 的"二选一"）：
//! - SOCKS 要开 `socks-proxy` feature，多拉一个 `socks` crate（Windows 上还带 `winapi`）；
//! - 自带那套与 WebSocket 传输是两份实现，"测速通"和"列表通"、"真连得上"就可能各说各话；
//! - 这里把 ureq 的"解析 + 建 TCP"两步都换成同一份 `vox-proxy::dial`（直连与经代理同一条路），
//!   TLS 仍交给 ureq 自带的 rustls 连接器，证书校验一点没动。
//!
//! 代价是用了 ureq 的 `unversioned` 接口（官方说明"不受 semver 承诺"）；`Cargo.toml` 钉死
//! 大版本 3，升级时编译器会拦下。**升级 ureq 需人工复核这一段**：`Connector` /
//! `Transport` / `Resolver` 三个 trait 的语义这条路上全用到了。

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, RustlsConnector,
    Transport,
};
use ureq::Error;
use vox_core::settings::NetworkSetting;
use vox_proxy::ProxySpec;

/// 一次 GET 的整体超时（含拨号）。
pub(crate) const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// 按网络设置造一个 agent。`target_host` 用来判断系统代理的"不走代理"名单。
/// 自定义代理格式不合法 = 错误（不会悄悄改成直连）。
pub(crate) fn agent_for(
    network: &NetworkSetting,
    target_host: &str,
) -> Result<ureq::Agent, String> {
    let proxy = vox_proxy::resolve(network, target_host)
        .map_err(|e| format!("invalid proxy settings: {e}"))?;
    let config = Config::builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build();
    let connector = DialConnector { proxy }.chain(RustlsConnector::default());
    Ok(ureq::Agent::with_parts(config, connector, SkipResolve))
}

/// 名字解析整个交给 [`DialConnector`]（直连时本地解析，经代理时按协议决定），
/// 所以 ureq 这一步只占个位。
#[derive(Debug)]
struct SkipResolve;

impl Resolver for SkipResolve {
    fn resolve(
        &self,
        _uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let mut addrs = self.empty();
        addrs.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        Ok(addrs)
    }
}

/// 拨好的 TCP 流 + ureq 的缓冲。ureq 自带的 `TcpTransport` 没有公开构造，这里是同样的
/// 读写循环（超时映射成 ureq 的 `Timeout`）。
struct DialedTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
}

impl std::fmt::Debug for DialedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialedTransport")
            .field("peer", &self.stream.peer_addr().ok())
            .finish()
    }
}

fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

impl Transport for DialedTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.stream
            .set_write_timeout(timeout.not_zero().map(|d| *d))?;
        let output = &self.buffers.output()[..amount];
        match self.stream.write_all(output) {
            Ok(()) => Ok(()),
            Err(e) if timed_out(&e) => Err(Error::Timeout(timeout.reason)),
            Err(e) => Err(e.into()),
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.stream
            .set_read_timeout(timeout.not_zero().map(|d| *d))?;
        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(n) => n,
            Err(e) if timed_out(&e) => return Err(Error::Timeout(timeout.reason)),
            Err(e) => return Err(e.into()),
        };
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        // 连接池复用前的探活：空闲连接上不该有字节可读；读到 0 / 报错 = 对端已关。
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut probe = [0u8];
        let alive = matches!(
            self.stream.read(&mut probe),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock
        );
        alive && self.stream.set_nonblocking(false).is_ok()
    }
}

#[derive(Debug)]
struct DialConnector {
    proxy: Option<ProxySpec>,
}

impl<In: Transport> Connector<In> for DialConnector {
    type Out = Either<In, DialedTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        if chained.is_some() {
            return Ok(chained.map(Either::A));
        }
        let uri = details.uri;
        let host = uri.host().ok_or(Error::HostNotFound)?;
        let port = uri
            .port_u16()
            .unwrap_or(if details.needs_tls() { 443 } else { 80 });
        let dialed = vox_proxy::dial(self.proxy.as_ref(), host, port, HTTP_TIMEOUT)
            .map_err(|e| Error::Io(io::Error::other(e.message)))?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(DialedTransport {
            stream: dialed.stream,
            buffers,
        })))
    }
}
