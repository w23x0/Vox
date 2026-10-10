use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use vox_proxy::testing::{http_connect_proxy, socks5_proxy};

use super::*;

const REMOTE_ONLY: &str = "remote-only.invalid";

/// 回环上的 TLS 服务端：握手后读到请求头就回 `status_line`，然后关。
/// 返回地址与（给客户端当信任根的）证书。
fn tls_server(name: &str, reply: &'static [u8]) -> (SocketAddr, CertificateDer<'static>) {
    let certified = rcgen::generate_simple_self_signed(vec![name.to_owned()]).unwrap();
    let cert = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut sock in listener.incoming().flatten() {
            let config = config.clone();
            std::thread::spawn(move || {
                let Ok(mut conn) = rustls::ServerConnection::new(config) else {
                    return;
                };
                let mut stream = rustls::Stream::new(&mut conn, &mut sock);
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let _ = stream.write_all(reply);
                let _ = stream.flush();
                conn.send_close_notify();
                let _ = conn.complete_io(&mut sock);
            });
        }
    });
    (addr, cert)
}

const UNAUTHORIZED: &[u8] =
    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

fn trusting(cert: CertificateDer<'static>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

fn local_endpoint(host: &str, port: u16, current: bool) -> Endpoint {
    Endpoint {
        id: "default",
        host: host.to_string(),
        port,
        path: "/v1/models",
        current,
    }
}

fn custom(proxy: &str) -> NetworkSetting {
    NetworkSetting {
        mode: NetworkMode::Custom,
        proxy: proxy.to_string(),
    }
}

const BUDGET: Duration = Duration::from_secs(5);

fn run(
    network: &NetworkSetting,
    endpoints: &[Endpoint],
    tls: Arc<rustls::ClientConfig>,
) -> NetworkReport {
    run_test(ModelProvider::Gpt, network, endpoints, tls, BUDGET)
}

// --- 测速 -------------------------------------------------------------------

#[test]
fn a_direct_probe_reports_every_stage_and_counts_401_as_reachable() {
    let (addr, cert) = tls_server("localhost", UNAUTHORIZED);
    let report = run(
        &NetworkSetting::new(NetworkMode::Direct),
        &[local_endpoint("localhost", addr.port(), true)],
        trusting(cert),
    );
    let r = &report.results[0];
    assert_eq!(r.error, None, "{r:?}");
    assert_eq!(r.http_status, Some(401));
    assert!(
        r.dns_ms.is_some() && r.connect_ms.is_some() && r.tls_ms.is_some() && r.http_ms.is_some()
    );
    assert_eq!(r.tunnel_ms, None, "直连没有隧道这一段");
    assert_eq!(report.via, None);
    let sum = r.dns_ms.unwrap() + r.connect_ms.unwrap() + r.tls_ms.unwrap() + r.http_ms.unwrap();
    assert_eq!(r.total_ms, Some(sum));
}

#[test]
fn the_probe_really_goes_through_an_http_connect_proxy() {
    let (addr, cert) = tls_server(REMOTE_ONLY, UNAUTHORIZED);
    let proxy = http_connect_proxy(Some(("alice", "hunter2")), vec![(REMOTE_ONLY.into(), addr)]);
    let report = run(
        &custom(&format!("http://alice:hunter2@{}", proxy.addr)),
        &[local_endpoint(REMOTE_ONLY, addr.port(), true)],
        trusting(cert),
    );
    let r = &report.results[0];
    assert_eq!(r.error, None, "{r:?}");
    assert_eq!(r.http_status, Some(401));
    assert!(
        r.tunnel_ms.is_some() && r.dns_ms.is_none(),
        "HTTP 代理：有隧道、无本地 DNS"
    );
    assert_eq!(proxy.requests().len(), 1);
    assert_eq!(
        report.via.as_deref(),
        Some(&*format!("http://{}", proxy.addr))
    );
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains("hunter2") && !json.contains("alice"),
        "报告不许带代理账号密码：{json}"
    );
}

#[test]
fn socks5h_probe_lets_the_proxy_resolve_the_name() {
    let (addr, cert) = tls_server(REMOTE_ONLY, UNAUTHORIZED);
    let proxy = socks5_proxy(None, vec![(REMOTE_ONLY.into(), addr)]);
    let report = run(
        &custom(&format!("socks5h://{}", proxy.addr)),
        &[local_endpoint(REMOTE_ONLY, addr.port(), true)],
        trusting(cert),
    );
    assert_eq!(report.results[0].error, None, "{:?}", report.results[0]);
    assert_eq!(
        proxy.requests(),
        vec![format!("domain:{REMOTE_ONLY}:{}", addr.port())]
    );
}

#[test]
fn failures_are_classified_by_stage() {
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let direct = NetworkSetting::new(NetworkMode::Direct);

    // connect：端口没人听
    let (addr, cert) = tls_server("localhost", UNAUTHORIZED);
    let report = run(
        &direct,
        &[
            local_endpoint("127.0.0.1", closed, true),
            // tls：证书不被信任（默认根里没有自签证书）
            local_endpoint("localhost", addr.port(), false),
        ],
        default_tls(),
    );
    assert_eq!(
        report.results[0].error.as_ref().unwrap().kind,
        FailureKind::Connect
    );
    assert_eq!(
        report.results[1].error.as_ref().unwrap().kind,
        FailureKind::Tls
    );
    assert!(
        report.results[1].connect_ms.is_some(),
        "失败前已走完的阶段要保留"
    );
    let _ = cert;

    // http：TLS 通了但对端说的不是 HTTP
    let (addr, cert) = tls_server("localhost", b"SSH-2.0-nothing\r\n\r\n");
    let report = run(
        &direct,
        &[local_endpoint("localhost", addr.port(), true)],
        trusting(cert),
    );
    assert_eq!(
        report.results[0].error.as_ref().unwrap().kind,
        FailureKind::Http
    );

    // connect：代理要认证，给的密码不对（报错里不许带密码）
    let (addr, cert) = tls_server(REMOTE_ONLY, UNAUTHORIZED);
    let proxy = http_connect_proxy(Some(("alice", "hunter2")), vec![(REMOTE_ONLY.into(), addr)]);
    let report = run(
        &custom(&format!("http://alice:wrong-pass@{}", proxy.addr)),
        &[local_endpoint(REMOTE_ONLY, addr.port(), true)],
        trusting(cert),
    );
    let failure = report.results[0].error.as_ref().unwrap();
    assert_eq!(failure.kind, FailureKind::Connect);
    assert!(failure.message.contains("407") && !failure.message.contains("wrong-pass"));

    // connect：自定义代理格式不对 = 报错，不直连
    let report = run(
        &custom("ftp://x:1"),
        &[local_endpoint("localhost", addr.port(), true)],
        default_tls(),
    );
    assert!(report.results[0]
        .error
        .as_ref()
        .unwrap()
        .message
        .contains("ftp"));
}

#[test]
fn a_server_that_never_answers_is_a_timeout_not_a_hang() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let started = Instant::now();
    let report = run_test(
        ModelProvider::Gpt,
        &NetworkSetting::new(NetworkMode::Direct),
        &[local_endpoint("localhost", port, true)],
        default_tls(),
        Duration::from_millis(500),
    );
    assert_eq!(
        report.results[0].error.as_ref().unwrap().kind,
        FailureKind::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(4));
}

// --- 阿里云地区建议 ----------------------------------------------------------

fn timed(id: &str, current: bool, total: Option<u32>) -> EndpointResult {
    EndpointResult {
        id: id.into(),
        host: format!("{id}.example"),
        current,
        dns_ms: None,
        connect_ms: None,
        tunnel_ms: None,
        tls_ms: None,
        http_ms: None,
        http_status: total.map(|_| 401),
        total_ms: total,
        bound_iface: None,
        error: total.is_none().then(|| Failure {
            kind: FailureKind::Timeout,
            message: "超时".into(),
        }),
    }
}

#[test]
fn suggest_needs_both_a_1_5x_ratio_and_an_80ms_gain() {
    let pick = |cur: u32, other: u32| {
        suggest(&[
            timed("cn", true, Some(cur)),
            timed("intl", false, Some(other)),
        ])
    };
    assert_eq!(
        pick(300, 100).map(|s| s.id),
        Some("intl".into()),
        "3 倍且差 200ms"
    );
    assert!(pick(300, 200).is_some(), "比值恰好 1.5、差 100ms：该建议");
    assert!(pick(299, 200).is_none(), "比值不到 1.5");
    assert!(pick(160, 80).is_some(), "恰好差 80ms 且 2 倍");
    assert!(pick(159, 80).is_none(), "差 79ms");
    assert!(
        pick(100, 40).is_none(),
        "2.5 倍但只差 60ms：延迟本来就小，不值得换"
    );
    assert!(pick(100, 100).is_none());
}

#[test]
fn suggest_picks_the_fastest_other_region_and_ignores_failures() {
    let s = suggest(&[
        timed("cn", true, Some(500)),
        timed("intl", false, Some(200)),
        timed("hk", false, Some(100)),
    ])
    .unwrap();
    assert_eq!(
        (s.id.as_str(), s.current_ms, s.nearest_ms),
        ("hk", 500, 100)
    );
    assert!(
        suggest(&[timed("cn", true, None), timed("intl", false, Some(50))]).is_none(),
        "当前不通：不按这条规则建议"
    );
    assert!(suggest(&[timed("cn", true, Some(500)), timed("intl", false, None)]).is_none());
}

#[test]
fn only_aliyun_gets_a_suggestion_and_the_candidates_match_the_spec() {
    let hosts = |p, r| {
        endpoints_of(p, r)
            .into_iter()
            .map(|e| e.host)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        hosts(ModelProvider::Aliyun, AliyunRegion::Cn),
        [
            "dashscope.aliyuncs.com",
            "dashscope-intl.aliyuncs.com",
            "cn-hongkong.dashscope.aliyuncs.com"
        ]
    );
    assert_eq!(
        hosts(ModelProvider::Gpt, AliyunRegion::Cn),
        ["api.openai.com"]
    );
    assert_eq!(
        hosts(ModelProvider::Gemini, AliyunRegion::Cn),
        ["generativelanguage.googleapis.com"]
    );
    let current: Vec<_> = endpoints_of(ModelProvider::Aliyun, AliyunRegion::Intl)
        .into_iter()
        .filter(|e| e.current)
        .map(|e| e.id)
        .collect();
    assert_eq!(current, ["intl"], "当前地区要标出来");
}

// --- 接管检测 ---------------------------------------------------------------

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn fake_ip_range_is_exactly_198_18_0_0_slash_15() {
    assert!(is_fake_ip(ip("198.18.0.1")) && is_fake_ip(ip("198.18.0.0")));
    assert!(is_fake_ip(ip("198.19.255.255")));
    assert!(!is_fake_ip(ip("198.17.255.255")) && !is_fake_ip(ip("198.20.0.0")));
    assert!(!is_fake_ip(ip("8.8.8.8")) && !is_fake_ip(ip("::1")));
    assert!(is_fake_ip(ip("::ffff:198.18.1.1")), "v4 映射地址同样算");
}

#[test]
fn a_fake_ip_answer_is_flagged_even_without_a_tun_adapter() {
    let resolved = vec![
        (
            "dashscope.aliyuncs.com".to_string(),
            vec![ip("198.18.0.23")],
        ),
        (
            "dashscope-intl.aliyuncs.com".to_string(),
            vec![ip("47.74.1.2")],
        ),
    ];
    // 数据集里的网卡名是 Linux `/sys/class/net` 的长相，所以显式给"非 Windows"：
    // 原先这里按编译目标暗中选判据，同一份数据在 Windows 上会被判成没有虚拟网卡。
    let report = takeover_from(&resolved, Some(vec!["eth0".into(), "lo".into()]), false);
    assert!(report.detected && report.fake_ip);
    assert_eq!(report.fake_ip_hosts, ["dashscope.aliyuncs.com"]);
    assert!(report.interfaces.is_empty() && report.interfaces_scanned);

    let clean = takeover_from(
        &[("a".into(), vec![ip("47.74.1.2")])],
        Some(vec!["eth0".into()]),
        false,
    );
    assert!(!clean.detected && !clean.fake_ip);
}

#[test]
fn the_platform_flag_picks_the_matching_adapter_name_rules() {
    let names = Some(vec![
        "tun0".into(),
        "utun3".into(),
        "Wintun Userspace Tunnel".into(),
        "eth0".into(),
    ]);
    let unix = takeover_from(&[], names.clone(), false);
    assert_eq!(
        unix.interfaces,
        ["tun0", "utun3"],
        "unix 判据只认 tun*/utun*/clash*/Meta"
    );
    let windows = takeover_from(&[], names, true);
    assert_eq!(
        windows.interfaces,
        ["Wintun Userspace Tunnel"],
        "Windows 判据认 netsh 报的名字里的 meta/clash/wintun/tap"
    );
}

#[test]
fn a_tun_adapter_alone_is_detected_and_unscannable_platforms_say_so() {
    let report = takeover_from(
        &[],
        Some(vec!["eth0".into(), "tun0".into(), "Meta".into()]),
        false,
    );
    assert!(report.detected && !report.fake_ip);
    assert_eq!(report.interfaces, ["tun0", "Meta"]);

    let blind = takeover_from(&[], None, false);
    assert!(
        !blind.detected && !blind.interfaces_scanned,
        "列不了网卡就老实说，不假装没有"
    );
}

#[test]
fn adapter_name_rules() {
    for name in [
        "tun0",
        "tun",
        "utun3",
        "Meta",
        "meta",
        "clash0",
        "Clash-tun",
    ] {
        assert!(unix_tun_name(name), "{name}");
    }
    for name in [
        "eth0",
        "wlan0",
        "lo",
        "docker0",
        "metadata0",
        "virbr0",
        "tap0",
    ] {
        assert!(!unix_tun_name(name), "{name}");
    }
    for name in [
        "Meta Tunnel",
        "Clash",
        "Wintun Userspace Tunnel",
        "TAP-Windows Adapter V9",
        "以太网 Meta",
    ] {
        assert!(windows_tun_name(name), "{name}");
    }
    for name in [
        "Ethernet",
        "Wi-Fi",
        "Loopback Pseudo-Interface 1",
        "vEthernet (WSL)",
    ] {
        assert!(!windows_tun_name(name), "{name}");
    }
}

#[test]
fn netsh_output_yields_adapter_names_with_spaces() {
    let out = "\r\nAdmin State    State          Type             Interface Name\r\n-------------------------------------------------------------------------\r\nEnabled        Connected      Dedicated        Ethernet 2\r\nEnabled        Connected      Dedicated        Meta\r\nDisabled       Disconnected   Dedicated        Wi-Fi\r\n";
    let names = parse_netsh_adapters(out);
    assert!(names.contains(&"Ethernet 2".to_string()) && names.contains(&"Meta".to_string()));
    assert!(windows_tun_name("Meta"));
}

#[test]
fn a_thread_that_dies_still_counts_as_one_domain() {
    let threads = vec![
        std::thread::spawn(|| ("dashscope.aliyuncs.com".to_string(), Vec::new())),
        std::thread::spawn(|| panic!("解析线程炸了")),
        std::thread::spawn(|| ("x.example".to_string(), Vec::new())),
    ];
    let rows = collect_resolved(threads);
    assert_eq!(rows.len(), 3, "少一行 = 漏报一个域名");
}
