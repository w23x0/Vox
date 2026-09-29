//! 本机 Streamable HTTP 传输面的用例（设计稿 §2.2、§2.3.1、§2.5）。
//!
//! 全部走**真 socket**：起一个 `127.0.0.1:0` 的服务，用裸 `TcpStream` 当客户端手写 HTTP。
//! 断言的是**外部可见**的契约——状态码、`-32020` / `-32602` / `-32022` / `-32601`、401/403
//! 的空 body、`202` 通知、握手文件的 `0600` 与包内字段——而不是实现细节。
//!
//! 这一层最容易出的洞全在**负例**上：少一个头校验就是协议面有洞，少一个 token 检查就是安全
//! 面有洞。因此每条负例都单独成用例，别把它们并进"正常路径"里顺手看一眼。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use vox_mcp::actions::ACTIONS;
use vox_mcp::mcp::meta;
use vox_mcp::transport::http::{ServerOptions, PATH};
use vox_mcp::{serve, ServerHandle};

/// 每个用例一个独立目录：用例是并行跑的，共用一个握手文件会互相踩。
fn state_file(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!("vox-mcp-http-{}-{name}", std::process::id()))
        .join("control.json")
}

fn options(name: &str) -> ServerOptions {
    let path = state_file(name);
    let _ = std::fs::remove_dir_all(path.parent().expect("父目录"));
    ServerOptions::new(path)
}

/// `_meta`：规范要求每请求必带 `protocolVersion` 与 `clientCapabilities`（`basic/index`）。
fn meta_object() -> Value {
    json!({
        meta::key::PROTOCOL_VERSION: meta::PROTOCOL_VERSION,
        meta::key::CLIENT_INFO: { "name": "curl", "version": "0" },
        meta::key::CLIENT_CAPABILITIES: {},
    })
}

fn body(id: i64, method: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": { "_meta": meta_object() },
    })
    .to_string()
}

fn discover_body() -> String {
    body(1, "server/discover")
}

fn tools_list_body() -> String {
    body(2, "tools/list")
}

fn tools_call_body(id: i64, name: &str, arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "_meta": meta_object(), "name": name, "arguments": arguments },
    })
    .to_string()
}

/// 一条待发的请求。默认把规范要求的三个头都带齐（`MCP-Protocol-Version` / `Mcp-Method` /
/// `Mcp-Name`，后两个从 body 推），用例只用改自己关心的那一个。
struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn post(body: &str, token: Option<&str>) -> Self {
        let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let version = message
            .get("params")
            .and_then(|params| params.get("_meta"))
            .and_then(|meta| meta.get(meta::key::PROTOCOL_VERSION))
            .and_then(Value::as_str)
            .unwrap_or(meta::PROTOCOL_VERSION);

        let mut request = Self {
            method: "POST".to_string(),
            target: PATH.to_string(),
            headers: vec![
                ("Host".to_string(), "127.0.0.1".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "Accept".to_string(),
                    "application/json, text/event-stream".to_string(),
                ),
                ("MCP-Protocol-Version".to_string(), version.to_string()),
            ],
            body: body.to_string(),
        };
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            request = request.set("Mcp-Method", method);
        }
        if let Some(name) = message
            .get("params")
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
        {
            request = request.set("Mcp-Name", name);
        }
        request.with_token(token)
    }

    fn with_token(mut self, token: Option<&str>) -> Self {
        if let Some(token) = token {
            self.headers
                .push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        self
    }

    /// 覆盖一个头（同名先删，免得出现两份）。
    fn set(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(header, _)| header != name);
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    fn remove(mut self, name: &str) -> Self {
        self.headers.retain(|(header, _)| header != name);
        self
    }

    fn method(mut self, method: &str) -> Self {
        self.method = method.to_string();
        self
    }

    fn target(mut self, target: &str) -> Self {
        self.target = target.to_string();
        self
    }

    fn body(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self
    }

    fn bytes(&self) -> Vec<u8> {
        let mut raw = format!("{} {} HTTP/1.1\r\n", self.method, self.target);
        for (name, value) in &self.headers {
            raw.push_str(name);
            raw.push_str(": ");
            raw.push_str(value);
            raw.push_str("\r\n");
        }
        raw.push_str(&format!("Content-Length: {}\r\n\r\n", self.body.len()));
        raw.push_str(&self.body);
        raw.into_bytes()
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!("body 不是 JSON（{error}）：{:?}", self.body);
        })
    }

    fn error_code(&self) -> i64 {
        self.json()["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("这不是 JSON-RPC 错误：{}", self.body))
    }
}

/// 裸 HTTP 客户端：一条连接，能被复用（keep-alive 用例要用）。读超时 5 s——卡住就是失败。
struct Client {
    stream: TcpStream,
    inbox: Vec<u8>,
}

impl Client {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("连服务端");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("设读超时");
        Self {
            stream,
            inbox: Vec::new(),
        }
    }

    fn send(&mut self, request: Req) -> Response {
        self.stream
            .write_all(&request.bytes())
            .expect("写请求（服务端可能提前关了连接）");
        self.read_response()
    }

    fn read_response(&mut self) -> Response {
        let end = loop {
            if let Some(end) = find(&self.inbox, b"\r\n\r\n") {
                break end;
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).expect("读响应头");
            assert!(read > 0, "服务端在回完整响应前关了连接");
            self.inbox.extend_from_slice(&chunk[..read]);
        };

        let head = String::from_utf8(self.inbox.drain(..end + 4).collect()).expect("头是 UTF-8");
        let mut lines = head.split("\r\n");
        let status_line = lines.next().expect("状态行");
        let status = status_line
            .split(' ')
            .nth(1)
            .expect("状态码")
            .parse::<u16>()
            .expect("状态码是数字");
        let headers: Vec<(String, String)> = lines
            .filter(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        let length: usize = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.parse().expect("Content-Length 是数字"))
            .unwrap_or(0);

        while self.inbox.len() < length {
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).expect("读响应体");
            assert!(read > 0, "服务端在回完整响应体前关了连接");
            self.inbox.extend_from_slice(&chunk[..read]);
        }
        let body = String::from_utf8(self.inbox.drain(..length).collect()).expect("体是 UTF-8");

        Response {
            status,
            headers,
            body,
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 起服务 + 连一条连接。返回的 `ServerHandle` 一 drop 就停服并擦掉握手文件。
fn server_with_client(name: &str) -> (ServerHandle, Client) {
    let server = serve(options(name), None).expect("起控制面");
    let client = Client::connect(server.addr());
    (server, client)
}

/// 一条请求、一条新连接。**被拒绝的响应会关连接**（我们没读完对端的体，继续在一条已经错位的
/// 连接上解析下一条请求是自找麻烦），所以负例都走这个，别在一条连接上接着发。
fn send_once(addr: SocketAddr, request: Req) -> Response {
    Client::connect(addr).send(request)
}

#[test]
fn handshake_file_carries_port_token_pid_and_is_owner_only() {
    let path = state_file("handshake");
    let server = serve(ServerOptions::new(path.clone()), None).expect("起控制面");

    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("握手文件必须出现（CLI 靠它找端口与 token）"),
    )
    .expect("握手文件必须是 JSON");

    assert_eq!(
        document["port"].as_u64(),
        Some(u64::from(server.addr().port()))
    );
    let token = document["token"].as_str().expect("token 是字符串");
    assert_eq!(token, server.token());
    assert_eq!(token.len(), 43, "32 字节 base64url 无填充 = 43 字符");
    assert_eq!(
        document["pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    assert_eq!(
        document["protocolVersion"].as_str(),
        Some(meta::PROTOCOL_VERSION)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "握手文件里有 token，必须只有属主能读");
    }

    server.shutdown();
    assert!(!path.exists(), "停服要擦掉自己写的握手文件");
}

#[test]
fn discover_over_http_speaks_the_protocol_layer_result() {
    let (server, mut client) = server_with_client("discover");
    let response = client.send(Req::post(&discover_body(), Some(server.token())));

    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let result = &response.json()["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"], json!(["2026-07-28"]));
    // 位必须是事实：`tools` 不广告 `listChanged`（5 个工具恒定）；`resources` 两位都真会发通知
    // （字幕变更 + 会话开/关），没有 `extensions`（v1 不实现 Tasks）。
    assert_eq!(
        result["capabilities"],
        json!({
            "tools": {},
            "resources": { "listChanged": true, "subscribe": true },
        })
    );
    assert_eq!(result["ttlMs"], json!(3_600_000));
    assert_eq!(result["cacheScope"], "public");
    assert_eq!(
        result["_meta"][meta::key::SERVER_INFO]["name"],
        json!("vox")
    );
}

#[test]
fn tools_list_over_http_keeps_the_action_table_order() {
    let (server, mut client) = server_with_client("tools-list");
    let response = client.send(Req::post(&tools_list_body(), Some(server.token())));

    assert_eq!(response.status, 200);
    let result = response.json();
    let result = &result["result"];
    let names: Vec<&str> = result["tools"]
        .as_array()
        .expect("tools 是数组")
        .iter()
        .map(|tool| tool["name"].as_str().expect("每条都有 name"))
        .collect();
    let expected: Vec<&str> = ACTIONS.iter().map(|action| action.name).collect();
    assert_eq!(names, expected, "tools/list 顺序 = ACTIONS 顺序");
    for tool in result["tools"].as_array().expect("tools 是数组") {
        for key in [
            "title",
            "description",
            "inputSchema",
            "outputSchema",
            "annotations",
        ] {
            assert!(!tool[key].is_null(), "{key} 必须逐条给出");
        }
    }
    assert_eq!(result["ttlMs"], json!(3_600_000));
    assert_eq!(result["cacheScope"], "public");
}

#[test]
fn http_bytes_equal_the_cli_probe_bytes() {
    // "同一份定义、两个出口"的机械证明：CLI 的 `--probe` 与 HTTP 面走的是同一个 `handle`，
    // 所以对同一条请求（同 id、同方法），两边吐出的字节必须一模一样。CLI 只是把 `handle`
    // 的输出打到 stdout。
    let (server, mut client) = server_with_client("same-handle");

    for (method, body) in [
        ("server/discover", body(1, "server/discover")),
        ("tools/list", body(1, "tools/list")),
    ] {
        let probe = Command::new(env!("CARGO_BIN_EXE_voxctl"))
            .args(["--probe", method, "--json"])
            .output()
            .expect("跑 voxctl --probe");
        assert_eq!(probe.status.code(), Some(0), "--probe {method} 必须成功");
        let probe_stdout = String::from_utf8(probe.stdout).expect("stdout 是 UTF-8");

        let response = client.send(Req::post(&body, Some(server.token())));
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body,
            probe_stdout.trim_end(),
            "{method}：HTTP 的 body 必须与 `voxctl --probe {method} --json` 的 stdout 逐字节相同"
        );
    }
}

#[test]
fn token_is_required_and_compared_exactly() {
    let server = serve(options("token"), None).expect("起控制面");
    let addr = server.addr();

    let missing = send_once(addr, Req::post(&discover_body(), None));
    assert_eq!(missing.status, 401);
    assert_eq!(missing.body, "", "401 不带 JSON-RPC body（设计稿 §2.5.1）");

    // 同长度但不同内容 —— 常数时间比较也必须判错。
    let mut wrong: Vec<u8> = server.token().bytes().collect();
    wrong[0] = if wrong[0] == b'A' { b'B' } else { b'A' };
    let wrong = String::from_utf8(wrong).expect("token 是 ASCII");
    let rejected = send_once(addr, Req::post(&discover_body(), Some(&wrong)));
    assert_eq!(rejected.status, 401);
    assert_eq!(rejected.body, "");

    // 长度都不同的（比如有人拿握手文件里的 `pid` 当 token）。
    let short = send_once(addr, Req::post(&discover_body(), Some("12345")));
    assert_eq!(short.status, 401);

    let ok = send_once(addr, Req::post(&discover_body(), Some(server.token())));
    assert_eq!(ok.status, 200);
}

#[test]
fn any_origin_header_is_forbidden() {
    // 设计稿 §2.5.1：**任何**带 `Origin` 的请求一律 403（规范 MUST 校验，我们取最严）。
    // 本机来源也一样 403——代价是浏览器版 Inspector 用不了，这是已经拍过的取舍。
    let server = serve(options("origin"), None).expect("起控制面");
    let addr = server.addr();

    for origin in [
        "http://evil.example",
        "http://127.0.0.1:47123",
        "http://localhost",
        "null",
    ] {
        let response = send_once(
            addr,
            Req::post(&discover_body(), Some(server.token())).set("Origin", origin),
        );
        assert_eq!(response.status, 403, "Origin: {origin} 必须 403");
        assert_eq!(response.body, "");
    }
}

#[test]
fn request_headers_must_match_the_body() {
    let server = serve(options("headers"), None).expect("起控制面");
    let addr = server.addr();
    let token = server.token().to_string();
    let call = tools_call_body(3, "list_endpoints", json!({}));

    let cases: Vec<(&str, &str, Req)> = vec![
        (
            "Mcp-Method",
            "与 body 的 method 不符",
            Req::post(&call, Some(&token)).set("Mcp-Method", "tools/list"),
        ),
        (
            "Mcp-Method",
            "缺这个头",
            Req::post(&discover_body(), Some(&token)).remove("Mcp-Method"),
        ),
        (
            "MCP-Protocol-Version",
            "与 body 的 protocolVersion 不符",
            Req::post(&discover_body(), Some(&token)).set("MCP-Protocol-Version", "2025-06-18"),
        ),
        (
            "MCP-Protocol-Version",
            "缺这个头",
            Req::post(&discover_body(), Some(&token)).remove("MCP-Protocol-Version"),
        ),
        (
            "Mcp-Name",
            "tools/call 不带",
            Req::post(&call, Some(&token)).remove("Mcp-Name"),
        ),
        (
            "Mcp-Name",
            "与 body 的 name 不符",
            Req::post(&call, Some(&token)).set("Mcp-Name", "describe_endpoint"),
        ),
    ];

    for (header, what, request) in cases {
        let response = send_once(addr, request);
        assert_eq!(response.status, 400, "{header}：{what} → 400");
        assert_eq!(response.error_code(), -32020, "{header}：{what}");
        assert_eq!(
            response.json()["error"]["data"]["header"],
            json!(header),
            "错误里要指出是哪个头"
        );
    }
}

#[test]
fn meta_and_version_problems_are_400() {
    let server = serve(options("meta"), None).expect("起控制面");
    let addr = server.addr();
    let token = server.token().to_string();

    // `_meta` 缺必填 → 400 + -32602（规范：`_meta` 问题在 HTTP 上必须 400）。
    let missing_capabilities = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/list",
        "params": { "_meta": json!({
            meta::key::PROTOCOL_VERSION: meta::PROTOCOL_VERSION,
        })},
    })
    .to_string();
    let response = send_once(addr, Req::post(&missing_capabilities, Some(&token)));
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), -32602);

    // 版本不支持 → 400 + -32022，并且带 `data.supported`（头和 body 都改成同一个旧版本：
    // 头与 body 一致，所以先过头部校验，落到协议层报"版本不支持"）。
    let old_version = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "server/discover",
        "params": { "_meta": json!({
            meta::key::PROTOCOL_VERSION: "2025-06-18",
            meta::key::CLIENT_CAPABILITIES: {},
        })},
    })
    .to_string();
    let response = send_once(addr, Req::post(&old_version, Some(&token)));
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), -32022);
    assert_eq!(
        response.json()["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );
}

#[test]
fn unknown_method_and_unknown_path_are_404() {
    let server = serve(options("not-found"), None).expect("起控制面");
    let addr = server.addr();
    let token = server.token().to_string();

    // 未知方法：404 + -32601（规范 `streamable-http#protocol-version-header`）。
    let response = send_once(addr, Req::post(&body(6, "no_such_method"), Some(&token)));
    assert_eq!(response.status, 404);
    assert_eq!(response.error_code(), -32601);

    // 单路径：别处的路径一律 404。
    let response = send_once(addr, Req::post(&discover_body(), Some(&token)).target("/"));
    assert_eq!(response.status, 404);
}

#[test]
fn get_and_delete_are_405() {
    // 本版规范没有 GET 端点也没有 session（`#earlier-streamable-http-revisions`）：收到了就 405。
    let server = serve(options("methods"), None).expect("起控制面");
    let addr = server.addr();

    for method in ["GET", "DELETE", "PUT"] {
        let response = send_once(
            addr,
            Req::post(&discover_body(), Some(server.token()))
                .method(method)
                .body(""),
        );
        assert_eq!(response.status, 405, "{method} 必须 405");
        assert_eq!(response.header("allow"), Some("POST"));
    }
}

#[test]
fn notification_post_is_202_without_body() {
    // 规范：通知一律 202 Accepted、无 body（接收方 MUST NOT 回通知）。
    let server = serve(options("notification"), None).expect("起控制面");
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": { "_meta": meta_object() },
    })
    .to_string();

    let mut client = Client::connect(server.addr());
    let response = client.send(Req::post(&notification, Some(server.token())));
    assert_eq!(response.status, 202);
    assert_eq!(response.body, "");

    // 通知不该把连接带走：客户端在这条连接上接着发一条正常请求也必须能通。
    let response = client.send(Req::post(&discover_body(), Some(server.token())));
    assert_eq!(response.status, 200);
}

#[test]
fn header_values_are_decoded_from_the_base64_sentinel() {
    // 规范：头值只能放 ASCII 白名单，非 ASCII 的值客户端包成 `=?base64?<base64>?=`，
    // 服务端**先解码再比**（设计稿 §2.3.1 第 8 条）。
    let server = serve(options("sentinel"), None).expect("起控制面");
    let addr = server.addr();
    let token = server.token().to_string();

    // `=?base64?dG9vbHMvbGlzdA==?=` 就是 "tools/list"：解码后与 body 的 method 一致 → 放行。
    let encoded = Req::post(&body(8, "tools/list"), Some(&token))
        .set("Mcp-Method", "=?base64?dG9vbHMvbGlzdA==?=");
    let response = send_once(addr, encoded);
    assert_eq!(response.status, 200, "sentinel 解码后应当对得上");
    assert_eq!(response.json()["result"]["ttlMs"], json!(3_600_000));

    // 写了 sentinel 却解不出来 = 坏头 → 400 + -32020，不是"当成普通字符串比一比"。
    let broken = Req::post(&body(9, "tools/list"), Some(&token))
        .set("Mcp-Method", "=?base64?这不是 base64?=");
    let response = send_once(addr, broken);
    assert_eq!(response.status, 400);
    assert_eq!(response.error_code(), -32020);
    assert_eq!(
        response.json()["error"]["data"]["header"],
        json!("Mcp-Method")
    );
}

#[test]
fn resources_read_requires_the_mcp_name_header() {
    // 规范的表（`streamable-http#request-metadata`）：`Mcp-Name` 对 `resources/read` 必带，
    // 且必须等于 body 里的 `params.uri`。这三条走的是与 `tools/call` 同一条校验路径。
    let server = serve(options("resource-name"), None).expect("起控制面");
    let addr = server.addr();
    let token = server.token().to_string();
    let uri = "vox://session/s_a/transcript";
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "resources/read",
        "params": { "_meta": meta_object(), "uri": uri },
    })
    .to_string();

    let missing = send_once(addr, Req::post(&body, Some(&token)).remove("Mcp-Name"));
    assert_eq!(missing.status, 400);
    assert_eq!(missing.error_code(), -32020);
    assert_eq!(missing.json()["error"]["data"]["header"], json!("Mcp-Name"));

    let wrong = send_once(
        addr,
        Req::post(&body, Some(&token)).set("Mcp-Name", "vox://session/s_b/transcript"),
    );
    assert_eq!(wrong.status, 400);
    assert_eq!(wrong.error_code(), -32020);
    assert_eq!(
        wrong.json()["error"]["data"]["received"],
        json!("vox://session/s_b/transcript")
    );

    // 头对上之后才轮到协议层：没注入后端 → 500 + -32603（**不是** -32020）。
    let passed = send_once(addr, Req::post(&body, Some(&token)).set("Mcp-Name", uri));
    assert_eq!(passed.status, 500);
    assert_eq!(passed.error_code(), -32603);
}

#[test]
fn listen_without_a_backend_is_a_plain_error_not_a_stream() {
    // `subscriptions/listen` 没有后端就**不开流**：回一条普通的 JSON 错误（带 Content-Length）。
    // 一条永远不响的长流比一条错误坏得多——客户端会以为"订阅成功，只是还没变化"。
    let server = serve(options("listen-no-backend"), None).expect("起控制面");
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "subscriptions/listen",
        "params": { "_meta": meta_object(), "notifications": {} },
    })
    .to_string();

    let response = send_once(server.addr(), Req::post(&body, Some(server.token())));
    assert_eq!(response.status, 500);
    assert_eq!(response.error_code(), -32603);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert!(response.json()["error"]["message"]
        .as_str()
        .expect("message")
        .contains("后端"));
}

#[test]
fn tools_call_without_a_backend_is_500_and_says_so() {
    // `voxctl serve` 不注入后端（账本在装配层）。那时 `tools/call` 必须**如实**报错，
    // 不许假成功：-32603 + 一句"后端未接入"。
    let server = serve(options("no-backend"), None).expect("起控制面");
    let response = send_once(
        server.addr(),
        Req::post(
            &tools_call_body(7, "list_endpoints", json!({})),
            Some(server.token()),
        ),
    );

    assert_eq!(response.status, 500);
    assert_eq!(response.error_code(), -32603);
    assert!(response.json()["error"]["message"]
        .as_str()
        .expect("message")
        .contains("后端"));
}

#[test]
fn oversized_head_and_body_are_rejected() {
    // 头 16 KiB / 体 1 MiB 是设计稿 §2.5.1 的硬上限。这里把上限压小，免得用例自己搬 1 MiB。
    let server = {
        let mut options = options("limits");
        options.max_body_bytes = 64;
        serve(options, None).expect("起控制面")
    };
    let addr = server.addr();
    let token = server.token().to_string();

    let fat_header = Req::post(&discover_body(), Some(&token)).set("X-Pad", &"a".repeat(20_000));
    let response = send_once(addr, fat_header);
    assert_eq!(response.status, 431, "超过 16 KiB 的请求头 → 431");

    let fat_body = Req::post(&discover_body(), Some(&token)).body(&"x".repeat(4096));
    let response = send_once(addr, fat_body);
    assert_eq!(response.status, 413, "超过 max_body_bytes 的体 → 413");
}

#[test]
fn one_connection_serves_several_requests() {
    // HTTP/1.1 默认长连接：宿主（Claude Desktop 那类）会复用连接，别在每条响应后关连接。
    let (server, mut client) = server_with_client("keep-alive");
    let token = server.token().to_string();

    let first = client.send(Req::post(&discover_body(), Some(&token)));
    assert_eq!(first.status, 200);
    assert_eq!(first.header("connection"), Some("keep-alive"));

    let second = client.send(Req::post(&tools_list_body(), Some(&token)));
    assert_eq!(second.status, 200);
    assert_eq!(
        second.json()["result"]["tools"].as_array().map(Vec::len),
        Some(ACTIONS.len())
    );
}

#[test]
fn serve_refuses_a_non_loopback_bind() {
    // 设计稿 §2.5.1：只绑 127.0.0.1，明确不做 0.0.0.0 / 远程。传了别的地址要**拒绝**，
    // 不许"悄悄降级"或照着绑上去。
    let address: SocketAddr = "0.0.0.0:0".parse().expect("地址");
    match serve(options("loopback").bind(address), None) {
        Ok(handle) => panic!("必须拒绝非回环地址，却绑到了 {}", handle.addr()),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput),
    }
}

#[test]
fn voxctl_serve_binary_serves_the_protocol() {
    // 进程级证据：`voxctl serve` 起来之后，外部进程（这里用裸 TCP 冒充）能真的调通。
    let path = state_file("voxctl-serve");
    let _ = std::fs::remove_dir_all(path.parent().expect("父目录"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_voxctl"))
        .arg("serve")
        .arg("--state-file")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("起 voxctl serve");

    let deadline = Instant::now() + Duration::from_secs(10);
    let document: Value = loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(document) = serde_json::from_str(&text) {
                break document;
            }
        }
        assert!(Instant::now() < deadline, "voxctl serve 没写出握手文件");
        thread::sleep(Duration::from_millis(20));
    };

    let port = document["port"].as_u64().expect("port") as u16;
    let token = document["token"].as_str().expect("token").to_string();
    let mut client = Client::connect(SocketAddr::from(([127, 0, 0, 1], port)));

    let response = client.send(Req::post(&discover_body(), Some(&token)));
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["result"]["resultType"], "complete");

    // 没带 token 的负例在进程级也要成立。
    let response = client.send(Req::post(&discover_body(), None));
    assert_eq!(response.status, 401);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(path.parent().expect("父目录"));
}
