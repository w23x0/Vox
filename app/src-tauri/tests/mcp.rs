//! 装配层控制面的端到端用例：**产品路径**上 `tools/call` 真的可用。
//!
//! 这里验的不是"函数能跑"，而是三件外部可见的事：
//!
//! 1. `mcp::start`（`assemble()` 起控制面用的同一个入口）真起服务、真按 S1 §2.5.1 写握手文件
//!    （`<config_dir>/control.json`，Unix 下 `0600`）；
//! 2. 一个独立进程按 MCP 2026-07-28 的规矩（每请求 `_meta`、`Mcp-Method` / `Mcp-Name` 两个头、
//!    Bearer token）打进来，`tools/call` 拿到的是**账本真派生出来**的结果——不是 `-32603`
//!    （"后端未接入"）。这条正是第七轮的那个缺口：`voxctl serve` 不注入后端，装配层也不起服务；
//! 3. 开关关着（默认档）**什么都不发生**：不监听、不写握手文件。用户没开的位一律拒：
//!    写配置回 `config_write_denied`，开麦克风回 `permission_denied`，且账本一个字节都没动。
//!
//! 跑法（要看线上的原始字节就加 `--nocapture`）：
//!
//! ```text
//! cargo test -p vox --test mcp -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vox_core::capability::HostFacts;
use vox_core::composition::HostKind;
use vox_core::event::PipelineState;
use vox_core::ports::Clock;
use vox_core::runtime::Runtime;
use vox_core::settings::{ListenTarget, ModelProvider, Settings};
use vox_core::usage::Stamp;
use vox_mcp::mcp::meta;
use vox_mcp::transport::http::PATH;

// S4-A：控制面胶水搬进共享宿主层（`docs/plans/S4-A-HOST-LAYER.md` §W4）。**下面 7 条用例的
// 函数体一个字节都没动**——`mcp::start` / `mcp::STATE_FILE` / `mcp::ControlPlane` 都还对得上，
// 靠的就是这个别名。`vox_lib` 里**不留** `pub use vox_host::control` 之类的转发（不留 shim）。
use vox_host::control as mcp;
use vox_host::control::Switch;

/// 单调毫秒钟：控制面拿它算 compose token 的 TTL，用例不睡觉所以恒 0 也无所谓。
#[derive(Default)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            unix_secs: 0,
            year: 2026,
            month: 9,
            day: 22,
        }
    }
}

/// 一份"这台机器什么都能"的账本：档位上限里 8 位全开（`off` 空表），设备与密钥都配好。
/// 事实由外壳注入、位由芯算——用例只给事实。（**用户开关不在这里**：那是
/// `Settings.control`，由 [`turn_on`] 拨。）
fn runtime() -> Runtime {
    let mut settings = Settings::default();
    settings.speak.input_device = Some("Yeti Stereo Microphone".to_string());
    settings.speak.output_device = Some("CABLE Input (VB-Audio Virtual Cable)".to_string());
    settings.listen.target = Some(ListenTarget {
        executable: "Discord.exe".to_string(),
        display_name: "Discord".to_string(),
        include_process_tree: true,
    });
    let runtime = Runtime::new(settings, Arc::new(TestClock::default()));
    runtime.set_host_facts(HostFacts {
        host: HostKind::Windows,
        off: BTreeMap::new(),
        virtual_mic_device: None,
    });
    runtime.set_api_key_for(ModelProvider::Aliyun, "test-key");
    runtime
}

/// 在设置页上点一圈：打开控制面总开关；`allows` 决定四个授权位拨不拨。
///
/// 走 `Runtime::update_settings`（芯的唯一写入口），与界面上那个开关是同一条路。
fn turn_on(runtime: &Runtime, allows: bool) {
    runtime.update_settings(|settings| {
        settings.control.enabled = true;
        settings.control.allow_microphone = allows;
        settings.control.allow_system_audio = allows;
        settings.control.allow_audible_output = allows;
        settings.control.allow_config_write = allows;
    });
}

/// 每个用例一个独立配置目录（用例并行跑，共用一个握手文件会互相踩）。
fn config_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vox-mcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建配置目录");
    dir
}

/// 一个外部 Agent 拿到的全部凭据：**只从握手文件里读**（产品路径就是这么走的），
/// 顺带把 S1 §2.5.1 那份契约核实一遍。
fn handshake(dir: &Path) -> (SocketAddr, String) {
    let path = dir.join(mcp::STATE_FILE);
    let text = std::fs::read_to_string(&path).expect("开关开了就必须有握手文件");
    let document: Value = serde_json::from_str(&text).expect("握手文件是 JSON");

    let port = document["port"].as_u64().expect("port 是数字") as u16;
    let token = document["token"].as_str().expect("token 是字符串");
    assert_eq!(
        document["protocolVersion"].as_str(),
        Some(meta::PROTOCOL_VERSION),
        "握手文件要写清协议版本"
    );
    assert_eq!(
        document["pid"].as_u64(),
        Some(u64::from(std::process::id())),
        "pid 是写这个文件的进程"
    );
    assert_eq!(token.len(), 43, "32 字节 base64url = 43 字符：{token}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "握手文件里有 token，必须只有属主能读");
    }

    (SocketAddr::from(([127, 0, 0, 1], port)), token.to_string())
}

/// 裸 HTTP 客户端：一个外部 Agent 的最小形态（MCP 的三个头 + Bearer token）。
struct Agent {
    stream: TcpStream,
    inbox: Vec<u8>,
    token: String,
    status: u16,
}

impl Agent {
    fn connect(addr: SocketAddr, token: &str) -> Self {
        let stream = TcpStream::connect(addr).expect("连服务端");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("设读超时");
        Self {
            stream,
            inbox: Vec::new(),
            token: token.to_string(),
            status: 0,
        }
    }

    /// `server/discover`（不需要 `Mcp-Name`）。
    fn discover(&mut self, id: i64) -> Value {
        self.send(id, "server/discover", None, json!({}))
    }

    /// `tools/call`。
    fn call(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        self.send(id, "tools/call", Some(name), arguments)
    }

    fn send(&mut self, id: i64, method: &str, name: Option<&str>, arguments: Value) -> Value {
        let body = if method == "tools/call" {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": {
                    "_meta": {
                        meta::key::PROTOCOL_VERSION: meta::PROTOCOL_VERSION,
                        meta::key::CLIENT_INFO: { "name": "external-agent", "version": "0" },
                        meta::key::CLIENT_CAPABILITIES: {},
                    },
                    "name": name,
                    "arguments": arguments,
                },
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": {
                    "_meta": {
                        meta::key::PROTOCOL_VERSION: meta::PROTOCOL_VERSION,
                        meta::key::CLIENT_INFO: { "name": "external-agent", "version": "0" },
                        meta::key::CLIENT_CAPABILITIES: {},
                    },
                },
            })
        }
        .to_string();

        let mut request = format!(
            "POST {PATH} HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: {version}\r\n\
             Mcp-Method: {method}\r\n\
             Authorization: Bearer {token}\r\n",
            version = meta::PROTOCOL_VERSION,
            token = self.token,
        );
        if let Some(name) = name {
            request.push_str(&format!("Mcp-Name: {name}\r\n"));
        }
        request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));

        self.stream
            .write_all(request.as_bytes())
            .expect("写请求（服务端可能提前关了连接）");
        let response = self.read();
        println!("--- {method} {name:?} → HTTP {}\n{response}\n", self.status);
        assert_eq!(self.status, 200, "正常路径必须 200");
        serde_json::from_str(&response).expect("响应必须是 JSON")
    }

    fn read(&mut self) -> String {
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
        let length: usize = head
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse().expect("Content-Length 是数字"))
            .unwrap_or(0);
        while self.inbox.len() < length {
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).expect("读响应体");
            assert!(read > 0, "服务端在回完整响应前关了连接");
            self.inbox.extend_from_slice(&chunk[..read]);
        }
        self.status = head
            .split(' ')
            .nth(1)
            .expect("状态码")
            .parse()
            .expect("状态码是数字");
        String::from_utf8(self.inbox.drain(..length).collect()).expect("体是 UTF-8")
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 产品路径：设置里打开控制面 → `mcp::start` 起服务 → 外部 Agent 只拿握手文件里的凭据调通
/// `tools/call`，连"授权后的写"也是真的落到账本上。
#[test]
fn an_agent_drives_the_control_plane_over_the_product_path() {
    let dir = config_dir("product");
    let runtime = runtime();
    turn_on(&runtime, true);
    let server = mcp::start(&runtime, &dir, Switch::from_settings(&runtime.settings()))
        .expect("开关开着就该起得来")
        .expect("开关开着必须返回句柄");
    let (addr, token) = handshake(&dir);
    assert_eq!(server.addr(), addr, "握手文件里的端口就是真监听的端口");

    let mut agent = Agent::connect(addr, &token);

    // ① `server/discover`：版本、能力、身份。能力面**只广告真能应答的东西**。
    let discovered = agent.discover(1);
    assert!(
        discovered.get("error").is_none(),
        "discover 不该报错：{discovered}"
    );
    let result = &discovered["result"];
    assert_eq!(result["resultType"], json!("complete"), "{discovered}");
    // `tools` 不广告 `listChanged`（5 个工具恒定）；`resources` 两位都是事实（字幕变更与会话开/关
    // 都真会发通知），资源面这一轮落的（`crates/vox-mcp/src/mcp/subscriptions.rs`）。
    assert_eq!(
        result["capabilities"],
        json!({
            "tools": {},
            "resources": { "listChanged": true, "subscribe": true },
        }),
        "{discovered}"
    );
    assert!(
        result["supportedVersions"]
            .as_array()
            .expect("版本表")
            .contains(&json!(meta::PROTOCOL_VERSION)),
        "{discovered}"
    );

    // ② `list_endpoints`：产品路径上**不是** `-32603`（后端真接上了）。
    let listed = agent.call(2, "list_endpoints", json!({}));
    assert!(
        listed.get("error").is_none(),
        "装配层没把后端接上（-32603）：{listed}"
    );
    assert_eq!(
        listed["result"]["resultType"],
        json!("complete"),
        "{listed}"
    );
    let structured = &listed["result"]["structuredContent"];
    assert_eq!(structured["device"]["tier"], json!("windows"), "{listed}");
    assert_eq!(
        structured["endpoints"].as_array().expect("端点表").len(),
        2,
        "两条腿都得在：{listed}"
    );

    // ③ `describe_endpoint`：清单、能力位、可改的格，三样都要是真派生出来的。
    let described = agent.call(3, "describe_endpoint", json!({ "endpoint": "speak" }));
    assert!(described.get("error").is_none(), "{described}");
    assert_eq!(
        described["result"]["resultType"],
        json!("complete"),
        "{described}"
    );
    let structured = &described["result"]["structuredContent"];
    assert_eq!(structured["endpoint"], json!("speak"), "{described}");
    assert_eq!(
        structured["manifest"]["host"],
        json!("windows"),
        "{described}"
    );
    assert_eq!(
        structured["manifest"]["out"][0]["role"],
        json!("virtual_mic"),
        "{described}"
    );
    assert_eq!(
        structured["capabilities"]["tier"],
        json!("windows"),
        "{described}"
    );
    assert_eq!(
        structured["capabilities"]["host"]["mic"],
        json!({ "enabled": true, "reason": null }),
        "8 位全开的事实要如实报到这一格：{described}"
    );
    let editable = structured["editable"].as_array().expect("editable");
    assert!(editable.len() >= 9, "§2.1.4 那张表至少 9 格：{described}");
    assert!(
        editable.contains(&json!("session.params.target_language")),
        "模型要能改目标语言：{described}"
    );
    // 用户位来自设置：`allow_microphone = true`（`Grants` 每次调用现读，不经装配层搬运）。
    let permissions = structured["permissions"].as_array().expect("权限表");
    assert!(
        permissions
            .iter()
            .any(|entry| entry["permission"] == json!("microphone")
                && entry["user_granted"] == json!(true)),
        "设置里的麦克风位要如实映到这里：{described}"
    );

    // ④ `compose_endpoint` 的 dry-run（`apply: false`）也走真后端：它算差异、签 token，
    //    但**一个字节都不写账本**（"用户同意"落在下一次 apply 的 token 上）。
    let before = runtime.settings();
    let mut composition = structured["manifest"].clone();
    composition["session"]["params"]["target_language"] = json!("en");
    let dry_run = agent.call(
        4,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": composition, "apply": false }),
    );
    assert!(dry_run.get("error").is_none(), "{dry_run}");
    let structured = &dry_run["result"]["structuredContent"];
    assert_eq!(
        structured["changed"][0]["path"],
        json!("session.params.target_language"),
        "{dry_run}"
    );
    let token = structured["token"]
        .as_str()
        .expect("dry-run 要签一个一次性 token")
        .to_string();
    assert_eq!(
        runtime.settings(),
        before,
        "dry-run 不许碰账本（只有带 token 的 apply 才写）"
    );

    // ⑤ 带 token apply：这一次用户位开着，写要**真的落到账本**上，返回的是重新派生出来的清单。
    let applied = agent.call(
        5,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": composition, "apply": true, "token": token }),
    );
    assert!(applied.get("error").is_none(), "{applied}");
    let structured = &applied["result"]["structuredContent"];
    assert_eq!(structured["applied"], json!(true), "{applied}");
    assert_eq!(
        structured["manifest"]["session"]["params"]["target_language"],
        json!("en"),
        "{applied}"
    );
    assert_eq!(
        runtime.settings().speak.target_language,
        "en",
        "控制面写的就是芯账本那一份（没有第二份状态）"
    );

    // ⑥ 同一个 token 重放 → 被拒（两段式确认的全部意义）。
    let replayed = agent.call(
        6,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": composition, "apply": true, "token": token }),
    );
    assert_eq!(replayed["result"]["isError"], json!(true), "{replayed}");
    assert_eq!(
        replayed["result"]["structuredContent"]["error"]["code"],
        json!("compose_token_stale"),
        "{replayed}"
    );
}

/// 固定端口那一档：`control.port` 不是 0 时，绑的就是它，握手文件里写的也是同一个端口
/// （S1 §2.5.1 让用户能在设置里固定端口）。
#[test]
fn a_configured_port_is_the_one_that_gets_bound() {
    let dir = config_dir("fixed-port");
    let runtime = runtime();

    // "问内核要一个此刻空闲的端口"只借到**此刻**这一个事实：把那个监听器 drop 掉之后、控制面
    // 真正绑上之前的这一小段窗口里，谁都能把它抢走（宿主机上并行的另一个 cargo 任务、别的
    // 用例、任何碰巧要这个号的进程）。那不是被测行为失败，是借来的端口没了——抢不到就重新要
    // 一个再来，最多 `ATTEMPTS` 次。（只吞 `EADDRINUSE`：别的错误照旧当场爆。）
    const ATTEMPTS: u32 = 8;
    let mut stolen = None;
    let mut started = None;
    for _ in 0..ATTEMPTS {
        let free = std::net::TcpListener::bind("127.0.0.1:0").expect("问内核要端口");
        let port = free.local_addr().expect("local_addr").port();
        drop(free);

        runtime.update_settings(|settings| {
            settings.control.enabled = true;
            settings.control.port = port;
        });
        match mcp::start(&runtime, &dir, Switch::from_settings(&runtime.settings())) {
            Ok(server) => {
                started = Some((port, server));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => stolen = Some(error),
            Err(error) => panic!("起控制面：{error}"),
        }
    }
    let Some((port, _server)) = started else {
        panic!("{ATTEMPTS} 次都没借到端口（每次都被抢）：{stolen:?}");
    };

    let (addr, _token) = handshake(&dir);
    assert_eq!(addr.port(), port, "设置里固定的端口就是绑到的端口");
}

/// 负例：总开关开着、**授权位一个都没拨**时——探路照旧（`describe_endpoint` 能看），
/// 写入与会话一律被拒，而且账本一个字节都没动。
#[test]
fn an_unlicensed_agent_cannot_write_and_cannot_open_the_mic() {
    let dir = config_dir("denied");
    let runtime = runtime();
    turn_on(&runtime, false);
    // 基线在"开关已打开、授权位全关"之后取：这条用例只关心被拒的调用有没有碰账本。
    let before = runtime.settings();
    // 句柄必须绑住：一 drop 就等于停服（`ServerHandle` 的 `Drop` 会停监听并擦掉握手文件），
    // 所以这里用带名字的 `_server`，不是 `let _ =`。
    let _server = mcp::start(&runtime, &dir, Switch::from_settings(&runtime.settings()))
        .expect("起控制面")
        .expect("开关开着");
    let (addr, token) = handshake(&dir);
    let mut agent = Agent::connect(addr, &token);

    // ① 用户位如实报假：`describe_endpoint` 的 `permissions` 那一格不是装饰，它读的就是 `Grants`。
    let described = agent.call(1, "describe_endpoint", json!({ "endpoint": "speak" }));
    let manifest = described["result"]["structuredContent"]["manifest"].clone();
    let permissions = described["result"]["structuredContent"]["permissions"]
        .as_array()
        .expect("权限表")
        .clone();
    for permission in ["microphone", "audible_output"] {
        assert!(
            permissions
                .iter()
                .any(|entry| entry["permission"] == json!(permission)
                    && entry["user_granted"] == json!(false)),
            "{permission} 该如实报「用户没让」：{described}"
        );
    }

    // ② 照产品流程 dry-run 拿一次性 token（dry-run 不写东西，所以照样放行）。
    let mut composition = manifest;
    composition["session"]["params"]["target_language"] = json!("en");
    let dry_run = agent.call(
        2,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": composition, "apply": false }),
    );
    assert_eq!(dry_run["result"]["isError"], Value::Null, "{dry_run}");
    let token = dry_run["result"]["structuredContent"]["token"]
        .as_str()
        .expect("dry-run 签一个 token")
        .to_string();

    // ③ 带着 token apply：**没开"允许改配置"，改写落到账本之前就被拒**。
    let applied = agent.call(
        3,
        "compose_endpoint",
        json!({ "endpoint": "speak", "composition": composition, "apply": true, "token": token }),
    );
    let structured = &applied["result"]["structuredContent"];
    assert_eq!(
        applied["result"]["isError"],
        json!(true),
        "没开改配置位就该被拒：{applied}"
    );
    assert_eq!(
        structured["error"]["code"],
        json!("config_write_denied"),
        "{applied}"
    );
    assert_eq!(
        runtime.settings(),
        before,
        "被拒的写入不许碰账本（界面看到的还是原来那份）"
    );

    // ④ 会话也开不了：`speak` 要麦克风位，用户没开 → `permission_denied` + `gate: vox_user`。
    let opened = agent.call(4, "session_open", json!({ "endpoint": "speak" }));
    assert_eq!(opened["result"]["isError"], json!(true), "{opened}");
    let error = &opened["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], json!("permission_denied"), "{opened}");
    assert_eq!(error["detail"]["gate"], json!("vox_user"), "{opened}");
    assert_eq!(
        error["detail"]["permission"],
        json!("microphone"),
        "{opened}"
    );
    assert_eq!(
        runtime.pipeline_state(vox_core::event::Pipeline::Speak),
        PipelineState::Idle,
        "被授权位挡住时不许把腿拉起来"
    );
}

/// 默认档：开关关着 → 不监听、不写握手文件（fail-closed）。
#[test]
fn the_switch_off_starts_nothing() {
    let dir = config_dir("off");
    let runtime = runtime();

    // 产品默认档就是这个：`from_settings` 读的是账本里的设置，而那几位默认为假。
    assert_eq!(
        Switch::from_settings(&runtime.settings()),
        Switch::OFF,
        "装完不开：默认必须全关"
    );
    let started = mcp::start(&runtime, &dir, Switch::from_settings(&runtime.settings()))
        .expect("关着不该报错");
    assert!(started.is_none(), "开关关着不许起服务");
    assert!(
        !dir.join(mcp::STATE_FILE).exists(),
        "没起服务就不许在配置目录里留凭据"
    );
}

/// 强杀留下的死凭据：`control.json` 里那个 pid 已经不在了，起控制面时先擦掉它。
///
/// 反过来那一半同样要钉住——**判不出来**（半截 JSON）与 **pid 还活着**（另一个实例正在
/// 服务，或 pid 被复用）的凭据一个字节都不许动。擦错方向删掉的是正在服务的那个实例的
/// 端口与 token。
#[test]
fn a_dead_handshake_is_swept_and_a_live_one_is_kept() {
    let dir = config_dir("stale-handshake");
    let runtime = runtime();
    let path = dir.join(mcp::STATE_FILE);
    let handshake = |pid: u32| {
        json!({
            "port": 47123,
            "token": "L".repeat(43),
            "pid": pid,
            "protocolVersion": meta::PROTOCOL_VERSION,
        })
        .to_string()
    };

    // ① 死 pid（上一次被强杀留下来的那种）。开关关着也擦：这时候根本没人会写新凭据，
    //    不擦就能一直烂在配置目录里。
    std::fs::write(&path, handshake(impossible_pid())).expect("写一份死凭据");
    let started = mcp::start(&runtime, &dir, Switch::OFF).expect("关着不该报错");
    assert!(started.is_none(), "开关关着不许起服务");
    assert!(!path.exists(), "pid 已经不在了的凭据必须擦掉");

    // ② pid 还活着：留着。
    std::fs::write(&path, handshake(std::process::id())).expect("写一份活凭据");
    mcp::start(&runtime, &dir, Switch::OFF).expect("关着不该报错");
    assert!(
        path.exists(),
        "pid 还活着的凭据不许动——那可能是另一个实例正在用的"
    );

    // ③ 读不明白的：留着（判不出来就不动手）。
    std::fs::write(&path, "{\"port\":47123,").expect("写一份半截凭据");
    mcp::start(&runtime, &dir, Switch::OFF).expect("关着不该报错");
    assert!(path.exists(), "解析不出来的凭据不许动");
}

/// 热切换：设置页上拨一下开关，服务**立刻**起/停，握手文件跟着出现/消失。
///
/// 走的是产品路径的那条线（用例里没有 Tauri，`install()` 之后每一步都与产品代码逐字相同）：
/// `Runtime::update_settings`（界面那个开关的写入口）→ `SettingsChanged` → `ControlPlane`
/// 挂的监听器 → 起/停。**同步**：监听器是在 `update_settings` 里同步调的，所以界面那次
/// invoke 一返回，服务就已经起好/停干净了。
#[test]
fn the_switch_hot_starts_and_stops_the_plane() {
    let dir = config_dir("hot-switch");
    let runtime = runtime();
    let plane = Arc::new(mcp::ControlPlane::new(runtime.clone(), dir.clone()));
    plane.install();
    let handshake_path = dir.join(mcp::STATE_FILE);

    // ① 默认档：开关关着 → 不监听、不写握手文件（`assemble()` 第 14 步就是这么走的）。
    plane.reconcile(Switch::from_settings(&runtime.settings()));
    let status = plane.status();
    assert!(!status.enabled && !status.running, "{status:?}");
    assert!(status.error.is_none(), "没失败过就不许有 error：{status:?}");
    assert_eq!(status.bound_port, None);
    assert_eq!(status.state_file, handshake_path.to_string_lossy());
    assert!(!handshake_path.exists(), "开关关着不许留凭据");

    // ② 拨开：真起服务、真写握手文件。
    runtime.update_settings(|settings| settings.control.enabled = true);
    let status = plane.status();
    assert!(status.enabled && status.running, "{status:?}");
    let (addr, token) = handshake(&dir);
    assert_eq!(
        status.bound_port,
        Some(addr.port()),
        "快照里的端口就是真监听的那个：{status:?}"
    );
    // 端口是系统分配（`control.port == 0`）：快照里要如实报"要的是 0、绑上的是这个号"。
    assert_eq!(status.port, 0);

    // ③ 真的能连（不是"起了个空壳"）：拿握手文件里的凭据问一声 `server/discover`。
    let mut agent = Agent::connect(addr, &token);
    assert!(agent.discover(1).get("error").is_none());

    // ④ 开关没变就不许动：改个不相干的设置（字号阈值）不该把 Agent 的连接踢断。
    runtime.update_settings(|settings| settings.speak.gate_threshold = 0.1);
    let (same_addr, same_token) = handshake(&dir);
    assert_eq!(same_addr, addr, "开关没动就不该重绑");
    assert_eq!(same_token, token, "开关没动就不该换 token（那是重起）");
    assert!(plane.status().running);

    // ⑤ 拨回去：停服、擦掉自己写的凭据，端口也不再接受连接（监听真的撤了）。
    runtime.update_settings(|settings| settings.control.enabled = false);
    let status = plane.status();
    assert!(!status.enabled && !status.running, "{status:?}");
    assert!(!handshake_path.exists(), "停服必须擦掉自己写的凭据");
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
        "停服之后旧端口不该还接得上：{addr}"
    );

    // ⑥ 端口变了要重绑：借一个此刻空闲的端口，改设置 → 按新端口重起，token 也是新的一份
    //    （真的是重起，不是把旧监听留着）。抢不到就再借一个——那不是被测行为失败。
    const ATTEMPTS: u32 = 8;
    let mut rebound = None;
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let free = std::net::TcpListener::bind("127.0.0.1:0").expect("问内核要端口");
        let port = free.local_addr().expect("local_addr").port();
        drop(free);
        runtime.update_settings(|settings| {
            settings.control.enabled = true;
            settings.control.port = port;
        });
        let status = plane.status();
        if status.running {
            rebound = Some((port, status));
            break;
        }
        last = Some(status);
    }
    let (port, status) = rebound
        .unwrap_or_else(|| panic!("{ATTEMPTS} 次都没绑上借来的端口（每次都被抢）：{last:?}"));
    assert_eq!(status.bound_port, Some(port), "{status:?}");
    let (addr, rebound_token) = handshake(&dir);
    assert_eq!(addr.port(), port, "握手文件里写的必须是新端口");
    assert_ne!(rebound_token, token, "换端口是重起：token 也该是新的");
    let mut agent = Agent::connect(addr, &rebound_token);
    assert!(agent.discover(2).get("error").is_none());

    // ⑦ 起不来时**如实报错**，不留半截凭据：占住端口再让控制面去绑它。
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").expect("占住一个端口");
    let busy = squatter.local_addr().expect("local_addr").port();
    runtime.update_settings(|settings| {
        settings.control.enabled = true;
        settings.control.port = busy;
    });
    let status = plane.status();
    assert!(status.enabled, "设置里要的就是开着：{status:?}");
    assert!(!status.running, "端口被占，起不来：{status:?}");
    assert_eq!(status.bound_port, None);
    assert!(
        status.error.is_some(),
        "起不来必须给出原因（界面那一格就显示它）：{status:?}"
    );
    assert!(
        !handshake_path.exists(),
        "没起来就不许留凭据（上一条的凭据已经被停服擦掉了）"
    );

    // ⑧ 失败之后**不许自己反复重试**：改个不相干的设置，error 与开关都保持原样
    //    （要重试就动开关——关掉再打开、或换个端口）。
    let error = status.error.clone();
    runtime.update_settings(|settings| settings.speak.gate_threshold = 0.05);
    let again = plane.status();
    assert_eq!(again.error, error, "没动开关就不该重试起服");
    assert!(!again.running);

    // ⑨ 退出前收摊：`shutdown()` 之后开关记的还是那一档，但监听与凭据都没了。
    drop(squatter);
    plane.shutdown();
    assert!(!plane.status().running);
    assert!(!handshake_path.exists());
}

/// **一直关着**的启动路径也要擦死凭据。
///
/// `ControlPlane` 的"开关没变就不动手"不能把这件事吃掉：擦死凭据那一步在 `mcp::start` 里、
/// 起服之前，而开关关着时它是**唯一**会去擦的人。所以第一次 `reconcile` 必须真走一遍
/// （`applied` 初值是 `None`，不是 `Switch::OFF`）——不然强杀留下的那张纸会一直烂在配置目录里。
#[test]
fn a_quiet_start_still_sweeps_dead_credentials() {
    let dir = config_dir("quiet-start");
    let runtime = runtime();
    let path = dir.join(mcp::STATE_FILE);
    std::fs::write(
        &path,
        json!({
            "port": 47123,
            "token": "L".repeat(43),
            "pid": impossible_pid(),
            "protocolVersion": meta::PROTOCOL_VERSION,
        })
        .to_string(),
    )
    .expect("写一份死凭据");

    // 产品路径：构造 → 挂监听器 → 按当前设置（默认全关）reconcile，就是 `assemble()` 第 14 步。
    let plane = Arc::new(mcp::ControlPlane::new(runtime.clone(), dir.clone()));
    plane.install();
    plane.reconcile(Switch::from_settings(&runtime.settings()));

    assert!(!plane.status().running, "开关关着不该起服务");
    assert!(!path.exists(), "开关关着也要擦掉上一次留下的死凭据");
}

/// 一个**不可能存在**的 pid：Linux 上取 `/proc/sys/kernel/pid_max` 之上的号（内核只会把
/// `1..=pid_max` 分给进程），别的平台取 32 位最大号（Windows 的 `OpenProcess` 对无效 pid
/// 回 `ERROR_INVALID_PARAMETER`）。
///
/// 故意不"起一个子进程再等它退出"来拿一个真实的死 pid：那要在并行跑的用例里 fork，而 fork
/// 会把同进程里别处（别的用例）正开着的监听 socket 复制给子进程——子进程退出之前那个端口
/// 就还占着，`a_configured_port_is_the_one_that_gets_bound` 于是随机吃到 `EADDRINUSE`
/// （实测：换成不起子进程之前，并行跑 20 次有 9 次是红的）。这里要钉的是"这个 pid 没有活着的
/// 进程"，用一个不可能存在的号来钉就够了，也干净。
fn impossible_pid() -> u32 {
    #[cfg(target_os = "linux")]
    {
        let pid_max = std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .unwrap_or(4_194_304);
        pid_max + 1
    }
    #[cfg(not(target_os = "linux"))]
    {
        u32::MAX
    }
}
