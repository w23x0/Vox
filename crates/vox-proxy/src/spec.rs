//! 代理地址的解析与脱敏显示。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    /// HTTP 代理，隧道用 `CONNECT`。
    Http,
    /// SOCKS5。`remote_dns = true`（`socks5h://`）时把域名交给代理解析；
    /// 否则本地先解析成 IP 再交给代理。
    Socks5 { remote_dns: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    Empty,
    UnsupportedScheme(String),
    MissingHost,
    BadPort(String),
    /// 账号或密码超出协议允许的长度（SOCKS5 各 255 字节）。
    CredentialTooLong,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("代理地址为空"),
            Self::UnsupportedScheme(s) => {
                write!(f, "不支持的代理协议 {s}://（可用 http、socks5、socks5h）")
            }
            Self::MissingHost => f.write_str("代理地址缺少主机名"),
            Self::BadPort(p) => write!(f, "代理端口无效：{p}"),
            Self::CredentialTooLong => f.write_str("代理账号或密码太长"),
        }
    }
}

impl std::error::Error for ProxyError {}

#[derive(Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    /// （账号, 密码），已做百分号解码。
    pub auth: Option<(String, String)>,
}

impl ProxySpec {
    /// 解析 `scheme://[user[:pass]@]host:port`。缺 scheme 当 `http://`；缺端口：
    /// http 取 8080、socks 取 1080。
    pub fn parse(input: &str) -> Result<Self, ProxyError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ProxyError::Empty);
        }
        let (scheme_text, rest) = match input.split_once("://") {
            Some((s, rest)) => (s.to_ascii_lowercase(), rest),
            None => ("http".to_string(), input),
        };
        let scheme = match scheme_text.as_str() {
            "http" => ProxyScheme::Http,
            "socks5" => ProxyScheme::Socks5 { remote_dns: false },
            "socks5h" => ProxyScheme::Socks5 { remote_dns: true },
            other => return Err(ProxyError::UnsupportedScheme(other.to_string())),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
            None => (None, authority),
        };
        let (host, port_text) = split_host_port(hostport);
        if host.is_empty() {
            return Err(ProxyError::MissingHost);
        }
        let port = match port_text {
            Some(text) => match text.parse::<u16>() {
                Ok(p) if p != 0 => p,
                _ => return Err(ProxyError::BadPort(text.to_string())),
            },
            None => match scheme {
                ProxyScheme::Http => 8080,
                ProxyScheme::Socks5 { .. } => 1080,
            },
        };
        let auth = match userinfo {
            None => None,
            Some(info) => {
                let (user, pass) = info.split_once(':').unwrap_or((info, ""));
                let (user, pass) = (percent_decode(user), percent_decode(pass));
                if user.len() > 255 || pass.len() > 255 {
                    return Err(ProxyError::CredentialTooLong);
                }
                Some((user, pass))
            }
        };
        Ok(Self {
            scheme,
            host: host.to_string(),
            port,
            auth,
        })
    }

    /// 不含账号密码的显示形式（日志 / 界面 / 错误信息只许用这个）。
    pub fn display(&self) -> String {
        let scheme = match self.scheme {
            ProxyScheme::Http => "http",
            ProxyScheme::Socks5 { remote_dns: false } => "socks5",
            ProxyScheme::Socks5 { remote_dns: true } => "socks5h",
        };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Debug for ProxySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProxySpec({}{})",
            self.display(),
            if self.auth.is_some() { ", auth" } else { "" }
        )
    }
}

/// `host:port` / `[v6]:port` / `host` → （主机, 端口文本）。
fn split_host_port(text: &str) -> (&str, Option<&str>) {
    if let Some(stripped) = text.strip_prefix('[') {
        if let Some((host, tail)) = stripped.split_once(']') {
            return (host, tail.strip_prefix(':'));
        }
    }
    match text.rsplit_once(':') {
        // 裸 IPv6 没有方括号时不止一个冒号：整串当主机。
        Some((host, _)) if host.contains(':') => (text, None),
        Some((host, port)) => (host, Some(port)),
        None => (text, None),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_schemes_and_defaults() {
        let http = ProxySpec::parse("http://127.0.0.1:7890").unwrap();
        assert_eq!(http.scheme, ProxyScheme::Http);
        assert_eq!((http.host.as_str(), http.port), ("127.0.0.1", 7890));
        let s5 = ProxySpec::parse("socks5://h.example:1081").unwrap();
        assert_eq!(s5.scheme, ProxyScheme::Socks5 { remote_dns: false });
        let s5h = ProxySpec::parse("SOCKS5H://h.example").unwrap();
        assert_eq!(s5h.scheme, ProxyScheme::Socks5 { remote_dns: true });
        assert_eq!(s5h.port, 1080);
        let bare = ProxySpec::parse("10.0.0.2:3128").unwrap();
        assert_eq!(bare.scheme, ProxyScheme::Http);
        assert_eq!(bare.port, 3128);
        let v6 = ProxySpec::parse("socks5h://[::1]:9050").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 9050));
        assert_eq!(v6.display(), "socks5h://[::1]:9050");
    }

    #[test]
    fn credentials_are_decoded_and_never_displayed() {
        let spec = ProxySpec::parse("socks5h://al%40ice:p%3Ass@127.0.0.1:1080").unwrap();
        assert_eq!(spec.auth, Some(("al@ice".into(), "p:ss".into())));
        assert_eq!(spec.display(), "socks5h://127.0.0.1:1080");
        let printed = format!("{spec:?}");
        assert!(
            !printed.contains("p:ss") && !printed.contains("al@ice"),
            "{printed}"
        );
        assert!(printed.contains("auth"));
        let user_only = ProxySpec::parse("http://bob@h:1").unwrap();
        assert_eq!(user_only.auth, Some(("bob".into(), String::new())));
    }

    #[test]
    fn rejects_garbage_with_a_reason() {
        assert_eq!(ProxySpec::parse("  "), Err(ProxyError::Empty));
        assert!(matches!(
            ProxySpec::parse("ftp://h:1"),
            Err(ProxyError::UnsupportedScheme(_))
        ));
        assert_eq!(ProxySpec::parse("http://:80"), Err(ProxyError::MissingHost));
        assert!(matches!(
            ProxySpec::parse("http://h:99999"),
            Err(ProxyError::BadPort(_))
        ));
        assert!(matches!(
            ProxySpec::parse("http://h:0"),
            Err(ProxyError::BadPort(_))
        ));
        let long = "x".repeat(256);
        assert_eq!(
            ProxySpec::parse(&format!("socks5://{long}:p@h:1")),
            Err(ProxyError::CredentialTooLong)
        );
    }
}
