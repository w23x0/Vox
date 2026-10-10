//! 系统代理探测。读不到就当没有（调用方直连）。
//!
//! - Linux / macOS：`HTTPS_PROXY` → `ALL_PROXY`（大小写都认），`NO_PROXY` 排除。
//! - Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings` 的
//!   `ProxyEnable` / `ProxyServer` / `ProxyOverride`。读注册表走系统自带的 `reg.exe query`
//!   （不新增依赖、不碰 Win32 API），解析是纯函数，在所有平台都有测试。
//!
//! 目标主机是回环地址时恒直连：本机服务不该绕出去。

use crate::spec::ProxySpec;

/// 对 `target_host`（走 https/wss）该用的系统代理。
pub fn detect(target_host: &str) -> Option<ProxySpec> {
    if is_loopback(target_host) {
        return None;
    }
    #[cfg(windows)]
    {
        let output = windows::query_internet_settings()?;
        parse_windows(&output, target_host)
    }
    #[cfg(not(windows))]
    {
        from_env(&|name| std::env::var(name).ok(), target_host)
    }
}

/// 环境变量版（`get` 注入，测试不用动真实环境）。
pub fn from_env(get: &dyn Fn(&str) -> Option<String>, target_host: &str) -> Option<ProxySpec> {
    let pick = |names: &[&str]| {
        names
            .iter()
            .filter_map(|n| get(n))
            .map(|v| v.trim().to_string())
            .find(|v| !v.is_empty())
    };
    let url = pick(&["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"])?;
    let no_proxy = pick(&["NO_PROXY", "no_proxy"]).unwrap_or_default();
    if bypassed(&no_proxy, target_host) {
        return None;
    }
    ProxySpec::parse(&url).ok()
}

/// Windows：`reg query` 的输出 → 代理。
pub fn parse_windows(reg_output: &str, target_host: &str) -> Option<ProxySpec> {
    let enabled = reg_value(reg_output, "ProxyEnable")?;
    let on = enabled.strip_prefix("0x").map_or_else(
        || enabled.parse::<u32>().ok(),
        |h| u32::from_str_radix(h, 16).ok(),
    )?;
    if on == 0 {
        return None;
    }
    let server = reg_value(reg_output, "ProxyServer")?;
    let overrides = reg_value(reg_output, "ProxyOverride").unwrap_or_default();
    if windows_bypassed(&overrides, target_host) {
        return None;
    }
    windows_server(&server)
}

/// `ProxyServer` 有两种写法：`host:port`（所有协议同一个 HTTP 代理），
/// 或 `http=h:p;https=h:p;socks=h:p`。我们的流量是 https/wss：先 `https=`，再 `socks=`。
fn windows_server(server: &str) -> Option<ProxySpec> {
    let server = server.trim();
    if !server.contains('=') {
        return ProxySpec::parse(server).ok();
    }
    let entry = |key: &str| {
        server.split(';').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case(key)
                .then(|| v.trim().to_string())
        })
    };
    if let Some(v) = entry("https") {
        return ProxySpec::parse(&format!("http://{v}")).ok();
    }
    let v = entry("socks")?;
    ProxySpec::parse(&format!("socks5://{v}")).ok()
}

fn windows_bypassed(overrides: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    overrides
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            let entry = entry.to_ascii_lowercase();
            if entry == "<local>" {
                return !host.contains('.');
            }
            glob_match(&entry, &host)
        })
}

/// 只支持 `*` 通配（Windows 的写法 `*.example.com`、`192.168.*`）。
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(at) = rest.find(part) else {
                return false;
            };
            rest = &rest[at + part.len()..];
        }
    }
    true
}

/// `reg query` 输出里一行形如 `    ProxyServer    REG_SZ    127.0.0.1:7890`。
fn reg_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != name {
            return None;
        }
        words.next()?; // REG_SZ / REG_DWORD
        let value: Vec<&str> = words.collect();
        (!value.is_empty()).then(|| value.join(" "))
    })
}

/// `NO_PROXY` 语义：逗号分隔；`*` 全部；`.example.com` 与 `example.com` 都匹配其子域与自身；
/// 其余按主机名/IP 原样相等。
pub fn bypassed(no_proxy: &str, host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    no_proxy
        .split(',')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let bare = entry.trim_start_matches('.');
            host == bare || host.ends_with(&format!(".{bare}"))
        })
}

fn is_loopback(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub(super) fn query_internet_settings() -> Option<String> {
        let output = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn env_picks_https_then_all_proxy_in_either_case() {
        let got = from_env(
            &env(&[("https_proxy", "http://a:1"), ("ALL_PROXY", "socks5://b:2")]),
            "api.openai.com",
        );
        assert_eq!(got.unwrap().display(), "http://a:1");
        let got = from_env(&env(&[("all_proxy", "socks5h://b:2")]), "api.openai.com");
        assert_eq!(got.unwrap().display(), "socks5h://b:2");
        assert!(
            from_env(&env(&[]), "api.openai.com").is_none(),
            "读不到就直连"
        );
        assert!(
            from_env(&env(&[("HTTPS_PROXY", "ftp://x:1")]), "h").is_none(),
            "不认识的协议当没有"
        );
    }

    #[test]
    fn no_proxy_excludes_hosts_and_subdomains() {
        let e = env(&[
            ("HTTPS_PROXY", "http://p:1"),
            ("no_proxy", "localhost, .corp.example,openai.com"),
        ]);
        assert!(
            from_env(&e, "api.openai.com").is_none(),
            "openai.com 的子域走直连"
        );
        assert!(from_env(&e, "x.corp.example").is_none());
        assert!(from_env(&e, "corp.example").is_none());
        assert!(from_env(&e, "generativelanguage.googleapis.com").is_some());
        assert!(!bypassed("notopenai.com", "api.openai.com"));
        assert!(bypassed("*", "anything.example"));
    }

    #[test]
    fn loopback_is_never_proxied_in_system_mode() {
        assert!(is_loopback("127.0.0.1") && is_loopback("::1") && is_loopback("localhost"));
        assert!(!is_loopback("api.openai.com"));
        assert!(detect("127.0.0.1").is_none());
    }

    const REG: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    127.0.0.1:7890\r\n    ProxyOverride    REG_SZ    localhost;*.corp.example;192.168.*;<local>\r\n";

    #[test]
    fn windows_registry_dump_is_understood() {
        let spec = parse_windows(REG, "api.openai.com").unwrap();
        assert_eq!(spec.display(), "http://127.0.0.1:7890");
        assert!(
            parse_windows(REG, "x.corp.example").is_none(),
            "ProxyOverride 通配"
        );
        assert!(parse_windows(REG, "192.168.1.5").is_none());
        assert!(
            parse_windows(REG, "intranet").is_none(),
            "<local> = 不含点的主机"
        );
        let off = REG.replace("0x1", "0x0");
        assert!(
            parse_windows(&off, "api.openai.com").is_none(),
            "ProxyEnable=0 = 直连"
        );
        assert!(parse_windows("", "h").is_none());
    }

    #[test]
    fn windows_per_protocol_server_string() {
        let reg = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    http=1.1.1.1:80;https=2.2.2.2:443;socks=3.3.3.3:1080\n";
        assert_eq!(
            parse_windows(reg, "h.example").unwrap().display(),
            "http://2.2.2.2:443"
        );
        let only_socks = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    http=1.1.1.1:80;socks=3.3.3.3:1080\n";
        assert_eq!(
            parse_windows(only_socks, "h.example").unwrap().display(),
            "socks5://3.3.3.3:1080"
        );
    }
}
