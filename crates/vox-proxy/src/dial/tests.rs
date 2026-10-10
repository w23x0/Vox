use super::*;
use crate::testing::{echo_server, http_connect_proxy, socks5_proxy};

const T: Duration = Duration::from_secs(5);

fn ping(mut stream: TcpStream) -> Vec<u8> {
    stream.write_all(b"ping-through-tunnel").unwrap();
    let mut got = vec![0u8; 19];
    stream.read_exact(&mut got).unwrap();
    got
}

fn spec(text: &str) -> ProxySpec {
    ProxySpec::parse(text).unwrap()
}

#[test]
fn direct_dial_reaches_the_target() {
    let echo = echo_server();
    let dialed = dial(None, "127.0.0.1", echo.port(), T).unwrap();
    assert!(dialed.dns.is_some() && dialed.tunnel.is_none());
    assert_eq!(ping(dialed.stream), b"ping-through-tunnel");
}

#[test]
fn http_connect_tunnels_to_the_requested_authority() {
    let echo = echo_server();
    let proxy = http_connect_proxy(None, vec![]);
    let spec = spec(&format!("http://{}", proxy.addr));
    let dialed = dial(Some(&spec), "127.0.0.1", echo.port(), T).unwrap();
    assert!(dialed.dns.is_none(), "HTTP 代理不在本地解析目标");
    assert!(dialed.tunnel.is_some());
    assert_eq!(ping(dialed.stream), b"ping-through-tunnel");
    assert_eq!(
        proxy.requests(),
        vec![format!("CONNECT 127.0.0.1:{} HTTP/1.1", echo.port())]
    );
}

#[test]
fn http_connect_hands_the_hostname_to_the_proxy() {
    let echo = echo_server();
    let proxy = http_connect_proxy(None, vec![("remote-only.invalid".into(), echo)]);
    let spec = spec(&format!("http://{}", proxy.addr));
    let dialed = dial(Some(&spec), "remote-only.invalid", 443, T).unwrap();
    assert_eq!(ping(dialed.stream), b"ping-through-tunnel");
    assert_eq!(
        proxy.requests(),
        vec!["CONNECT remote-only.invalid:443 HTTP/1.1".to_string()]
    );
}

#[test]
fn http_connect_sends_basic_credentials_and_reports_407_without_leaking_them() {
    let echo = echo_server();
    let proxy = http_connect_proxy(Some(("alice", "hunter2")), vec![]);
    let good = spec(&format!("http://alice:hunter2@{}", proxy.addr));
    assert!(dial(Some(&good), "127.0.0.1", echo.port(), T).is_ok());

    let bad = spec(&format!("http://alice:wrong-pass@{}", proxy.addr));
    let err = dial(Some(&bad), "127.0.0.1", echo.port(), T).unwrap_err();
    assert_eq!(err.stage, Stage::Tunnel);
    assert!(err.message.contains("407"), "{err}");
    assert!(
        !err.message.contains("wrong-pass") && !err.message.contains("alice"),
        "{err}"
    );
}

#[test]
fn socks5_resolves_locally_and_sends_an_ip() {
    let echo = echo_server();
    let proxy = socks5_proxy(None, vec![]);
    let spec = spec(&format!("socks5://{}", proxy.addr));
    let dialed = dial(Some(&spec), "localhost", echo.port(), T).unwrap();
    assert!(dialed.dns.is_some(), "socks5 在本地解析目标");
    assert_eq!(ping(dialed.stream), b"ping-through-tunnel");
    let seen = proxy.requests();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].starts_with("ip:"),
        "非 h 版本发给代理的必须是 IP：{seen:?}"
    );
}

#[test]
fn socks5h_hands_the_domain_to_the_proxy_and_never_resolves_locally() {
    let echo = echo_server();
    let proxy = socks5_proxy(None, vec![("remote-only.invalid".into(), echo)]);
    let spec = spec(&format!("socks5h://{}", proxy.addr));
    // `.invalid` 本地解析必然失败；能连上就只能是代理解析的。
    let dialed = dial(Some(&spec), "remote-only.invalid", 443, T).unwrap();
    assert!(dialed.dns.is_none());
    assert_eq!(ping(dialed.stream), b"ping-through-tunnel");
    assert_eq!(
        proxy.requests(),
        vec!["domain:remote-only.invalid:443".to_string()]
    );
}

#[test]
fn socks5_password_auth_accepts_the_right_one_and_refuses_the_wrong_one() {
    let echo = echo_server();
    let proxy = socks5_proxy(Some(("alice", "hunter2")), vec![]);
    let good = spec(&format!("socks5h://alice:hunter2@{}", proxy.addr));
    assert_eq!(
        ping(
            dial(Some(&good), "127.0.0.1", echo.port(), T)
                .unwrap()
                .stream
        ),
        b"ping-through-tunnel"
    );
    let bad = spec(&format!("socks5h://alice:nope@{}", proxy.addr));
    let err = dial(Some(&bad), "127.0.0.1", echo.port(), T).unwrap_err();
    assert_eq!(err.stage, Stage::Tunnel);
    assert!(
        !err.message.contains("nope") && !err.message.contains("hunter2"),
        "{err}"
    );
    let anonymous = spec(&format!("socks5h://{}", proxy.addr));
    assert_eq!(
        dial(Some(&anonymous), "127.0.0.1", echo.port(), T)
            .unwrap_err()
            .stage,
        Stage::Tunnel,
        "代理要认证而我们没给"
    );
}

#[test]
fn proxy_that_cannot_reach_the_target_fails_in_the_tunnel_stage() {
    let proxy = socks5_proxy(None, vec![]);
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let spec = spec(&format!("socks5h://{}", proxy.addr));
    let err = dial(Some(&spec), "127.0.0.1", closed, T).unwrap_err();
    assert_eq!(err.stage, Stage::Tunnel);
    assert!(err.message.contains("SOCKS5"), "{err}");
}

#[test]
fn a_dead_proxy_fails_in_the_connect_stage() {
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let spec = spec(&format!("http://127.0.0.1:{closed}"));
    let err = dial(Some(&spec), "example.invalid", 443, T).unwrap_err();
    assert_eq!(err.stage, Stage::Connect);
    assert!(!err.timed_out);
}

#[test]
fn a_proxy_that_never_answers_times_out_in_the_tunnel_stage() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let spec = spec(&format!("http://{}", silent.local_addr().unwrap()));
    let err = dial(
        Some(&spec),
        "example.invalid",
        443,
        Duration::from_millis(400),
    )
    .unwrap_err();
    assert_eq!(err.stage, Stage::Tunnel);
    assert!(err.timed_out, "{err:?}");
}
