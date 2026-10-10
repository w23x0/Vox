//! 直连时把出站 socket 绑到物理默认路由网卡（Linux，`SO_BINDTODEVICE`）。
//!
//! 背景：Clash / mihomo 的 TUN 全局模式（auto-route）会用策略路由把所有流量卷进 TUN 网卡，
//! 连"直接连接"也不例外（全局模式下规则不生效）。绑到物理网卡后，路由查找只看这块网卡
//! 上的路由，不会命中 TUN 的默认路由，于是真直连。内核 5.7+ 不需要特权。
//!
//! 全程 best-effort：探测不到网卡、绑定失败都退回"不绑"（原行为），只 warn 一次，不影响连通。
//! 非 Linux 上整个模块是空操作。

/// 一行 `/proc/net/route` 里和默认路由有关的字段。
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultRoute {
    iface: String,
    metric: u32,
}

/// 解析 `/proc/net/route` 全文，取出所有"启用的默认路由"。
#[cfg(any(target_os = "linux", test))]
fn parse_default_routes(text: &str) -> Vec<DefaultRoute> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
            if cols.len() < 8 {
                return None;
            }
            let flags = u32::from_str_radix(cols[3], 16).ok()?;
            let metric = cols[6].parse().ok()?;
            let is_default = cols[1] == "00000000" && cols[7] == "00000000";
            // RTF_UP = 0x1
            (is_default && flags & 1 != 0).then(|| DefaultRoute {
                iface: cols[0].to_string(),
                metric,
            })
        })
        .collect()
}

/// 名字像虚拟 / 隧道 / 容器网卡（不是出国门的物理口）。
#[cfg(any(target_os = "linux", test))]
fn virtual_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "tun",
        "utun",
        "meta",
        "clash",
        "tailscale",
        "docker",
        "br-",
        "veth",
        "lo",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
}

/// 在默认路由里挑物理网卡：跳过虚拟网卡与 `is_tun` 判定为 TUN 的，取 metric 最小的。
#[cfg(any(target_os = "linux", test))]
fn choose_iface(routes: &[DefaultRoute], is_tun: impl Fn(&str) -> bool) -> Option<String> {
    routes
        .iter()
        .filter(|r| !virtual_name(&r.iface) && !is_tun(&r.iface))
        .min_by_key(|r| r.metric)
        .map(|r| r.iface.clone())
}

/// 当前物理默认路由网卡名；探测不到返回 `None`。
#[cfg(target_os = "linux")]
pub(crate) fn physical_iface() -> Option<String> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    choose_iface(&parse_default_routes(&text), |name| {
        std::path::Path::new("/sys/class/net")
            .join(name)
            .join("tun_flags")
            .exists()
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn physical_iface() -> Option<String> {
    None
}

/// 198.18.0.0/15（代理软件的 fake-ip 段）：目标落在这里说明 DNS 已被劫持，绑网卡也救不回来。
pub(crate) fn is_fake_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 198 && (b == 18 || b == 19)
        }
        std::net::IpAddr::V6(_) => false,
    }
}

/// 只 warn 一次，免得每次连接都刷屏。
pub(crate) fn warn_once(flag: &'static std::sync::atomic::AtomicBool, msg: &str) {
    if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!("{msg}");
    }
}

/// 向 `addr` 发起一次绑定到 `iface` 的 TCP 连接。绑定本身失败（EPERM 等）时返回 `Err(BindFailed)`，
/// 让调用方退回普通连接；连接失败返回 `Err(Connect)`。
#[cfg(target_os = "linux")]
pub(crate) enum BoundError {
    BindFailed(std::io::Error),
    Connect(std::io::Error),
}

#[cfg(target_os = "linux")]
pub(crate) fn connect_bound(
    addr: &std::net::SocketAddr,
    iface: &str,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, BoundError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(*addr),
        Type::STREAM,
        Some(Protocol::TCP),
    )
    .map_err(BoundError::BindFailed)?;
    socket
        .bind_device(Some(iface.as_bytes()))
        .map_err(BoundError::BindFailed)?;
    socket
        .connect_timeout(&(*addr).into(), timeout)
        .map_err(BoundError::Connect)?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
Meta\t00000000\t0100000A\t0003\t0\t0\t0\t00000000\t0\t0\t0
eno1\t00000000\t0100150A\t0003\t0\t0\t100\t00000000\t0\t0\t0
wlan0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
eno1\t0000150A\t00000000\t0001\t0\t0\t100\t0000FFFF\t0\t0\t0
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
";

    #[test]
    fn parses_only_default_routes() {
        let routes = parse_default_routes(SAMPLE);
        let names: Vec<_> = routes.iter().map(|r| r.iface.as_str()).collect();
        assert_eq!(names, ["Meta", "eno1", "wlan0"]);
    }

    #[test]
    fn skips_down_routes() {
        let text = "Iface\tD\tG\tF\n eno1\t00000000\t0100150A\t0002\t0\t0\t10\t00000000\n";
        assert!(parse_default_routes(text).is_empty());
    }

    #[test]
    fn picks_lowest_metric_physical() {
        let routes = parse_default_routes(SAMPLE);
        assert_eq!(choose_iface(&routes, |_| false).as_deref(), Some("eno1"));
    }

    #[test]
    fn skips_tun_by_flag_even_with_plain_name() {
        let routes = parse_default_routes(SAMPLE);
        let got = choose_iface(&routes, |n| n == "eno1");
        assert_eq!(got.as_deref(), Some("wlan0"));
    }

    #[test]
    fn none_when_only_virtual() {
        let text = "Iface\tD\tG\tF\tR\tU\tM\tMask\nutun3\t00000000\t1\t0003\t0\t0\t0\t00000000\n";
        assert_eq!(choose_iface(&parse_default_routes(text), |_| false), None);
    }

    #[test]
    fn virtual_names() {
        for n in [
            "tun0",
            "utun4",
            "Meta",
            "tailscale0",
            "docker0",
            "br-ab12",
            "veth1",
            "lo",
        ] {
            assert!(virtual_name(n), "{n}");
        }
        for n in ["eno1", "wlan0", "enp3s0"] {
            assert!(!virtual_name(n), "{n}");
        }
    }

    #[test]
    fn fake_ip_range() {
        assert!(is_fake_ip("198.18.0.5".parse().unwrap()));
        assert!(is_fake_ip("198.19.255.1".parse().unwrap()));
        assert!(!is_fake_ip("198.20.0.1".parse().unwrap()));
    }
}
