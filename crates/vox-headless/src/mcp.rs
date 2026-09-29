//! 控制面（Agent 面）的装配胶水：把芯的账本接到 `vox-mcp` 上，并按用户开关决定起不起。
//!
//! 跟桌面档的同一件胶水（`app/src-tauri/src/mcp.rs`）**同形**——两份外壳都要这一段，
//! 而这一层里有 Tauri 类型与 `tokio` 之类不能共享的东西，所以各留各的一份。三件事：
//!
//! 1. 把"起不起、绑哪个端口"读成一份数据（[`Switch`]，唯一来源是账本里的 `Settings.control`）；
//! 2. 把 `Runtime`（唯一账本）交给 `vox-mcp` 的默认后端 [`LedgerBackend`] 再 `serve`——
//!    五格逐条转发给 `endpoints` / `session`，**没有第二份状态**；
//! 3. 起之前先擦掉上一次留下的**死凭据**（[`prune_dead_credentials`]）——无屏盒子上这条
//!    比桌面上更要紧：重启是常态（systemd `Restart=`、断电），而 SIGTERM 不会走到
//!    `ServerHandle` 的 `Drop`，握手文件就留在那儿，CLI 照着废凭据反复重试。
//!
//! **本文件不实现 `Grants`**（"用户让不让"）：那是 `vox-mcp` 那一侧的事
//! （`impl Grants for Runtime`，每次调用现读 `Settings.control.allow_*`）。这里交下去的
//! 就是同一个 `Runtime`；装配层自己再写一份就等于把授权位存成第二份真源。
//!
//! **没做的**：控制面的**起停与端口没有热切换**——`Settings.control.enabled` / `port`
//! 只有装配时读一次，改完要重启进程才生效（与桌面档同一条已知项，见 S1 设计稿）。

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde_json::Value;
use vox_core::runtime::Runtime;
use vox_core::settings::Settings;
use vox_mcp::session::LedgerBackend;
use vox_mcp::transport::http::{ServerHandle, ServerOptions};
use vox_mcp::{serve, BoxedBackend};

/// 握手文件名。路径固定 `<config_dir>/control.json`（S1 §2.5.1），内容
/// `{"port":…,"token":"<43 字符 base64url>","pid":…,"protocolVersion":"2026-07-28"}`，
/// Unix 下 `0600`（写入那一步在 `vox_mcp::transport::http`，本文件不重复实现）。
/// 与桌面档同名，好让 CLI 不必知道它在跟哪个外壳说话。
pub const STATE_FILE: &str = crate::config::CONTROL_FILE;

/// 控制面开关的一份快照：**起不起**、**绑哪个端口**。
///
/// 授权位（`allow_microphone` 那一类）**不在这里**：它们由 `vox-mcp` 的
/// `impl Grants for Runtime` 每次 `tools/call` 现读同一个 `Settings.control`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// 控制面总开关。**默认关**：装完不开，要用户明确打开（`ControlSettings::default`）。
    pub enabled: bool,
    /// 监听端口。`0` = 每次启动由系统分配（默认；实际端口只有握手文件里才知道）。
    pub port: u16,
}

impl Switch {
    /// 全关的那一档：不监听、不写握手文件。
    pub const OFF: Self = Self {
        enabled: false,
        port: 0,
    };

    /// 从账本设置读开关。**唯一**的来源（`Settings::normalize()` 已经把端口夹紧过：
    /// <1024 的一律归 0，那是内核的保留段）。
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.control.enabled,
            port: settings.control.port,
        }
    }
}

/// 起控制面前先擦掉上一次留下的**死凭据**：`control.json` 里记着那个进程的 pid，而它已经不在。
///
/// 判据只有一条：**文件里的 pid 已经不在了**。除此之外一律不动手——读不出来 / 不是 JSON /
/// 没有 pid 是"判不出来"，pid 还活着是"另一个实例正在服务"（或 pid 被别人复用）。
/// 留一份旧凭据只是丑；删掉正在服务的那个实例的凭据是事故。
fn prune_dead_credentials(state_file: &Path) {
    let Ok(text) = fs::read_to_string(state_file) else {
        return;
    };
    let Ok(document) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(pid) = document.get("pid").and_then(Value::as_u64) else {
        return;
    };
    let Ok(pid) = u32::try_from(pid) else {
        return;
    };
    if process_alive(pid) {
        return;
    }
    match fs::remove_file(state_file) {
        Ok(()) => tracing::info!(
            pid,
            state_file = %state_file.display(),
            "擦掉上一次留下的死凭据（那个进程已经不在了）"
        ),
        Err(error) => tracing::warn!(
            pid,
            error = %error,
            state_file = %state_file.display(),
            "死凭据没擦掉（不影响起控制面）"
        ),
    }
}

/// `pid` 还活着吗？只给 [`prune_dead_credentials`] 用。
///
/// 平台差异收在这个外壳文件里（芯里一行 `#[cfg]` 都没有）：判断"控制面那个进程还在不在"
/// 没有跨平台写法。两边都朝**宁可当它活着**偏——判错成"活着"只是留下一份旧凭据，
/// 判错成"死了"会删掉正在服务的那个实例的凭据。
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    // `kill(pid, 0)` 才是语义最准的写法，但 std 没有这个 API，也不为它引 libc：
    // `/proc/<pid>` 在就是有这么一个进程。两处已知偏差，方向都是"多报活"：
    // ① 僵尸进程的目录还在（毫秒级窗口）；② `hidepid=2` 挂载下看不见别人的进程。
    // 无屏档里控制面与入口是同一个进程、同一个用户，两个偏差都碰不到。
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    // 判不出来就不动它——偏差方向跟 Linux 那边一致。
    true
}

/// 按开关起控制面。
///
/// - 开关关着 → `Ok(None)`：**不监听、不写握手文件**（默认档，fail-closed 的样子）。
/// - 开关开着 → 真起服务，返回的 [`ServerHandle`] 由装配层保管到退出（`shutdown()`）。
///
/// 两条路都先走一遍 [`prune_dead_credentials`]（开关关着也要：这时没人会写新的）。
///
/// 服务端参数照 S1 §2.5.1：只绑 `127.0.0.1`、端口取开关里的值（`0` = 系统分配）、握手文件
/// `<config_dir>/control.json`、其余上限走 `ServerOptions::new` 的默认值。`Runtime` 是
/// `Arc` 的壳，克隆等于共享——控制面与入口看到的是同一个账本。
pub fn start(
    runtime: &Runtime,
    config_dir: &Path,
    switch: Switch,
) -> io::Result<Option<ServerHandle>> {
    let state_file: PathBuf = config_dir.join(STATE_FILE);
    prune_dead_credentials(&state_file);

    if !switch.enabled {
        return Ok(None);
    }

    let options =
        ServerOptions::new(&state_file).bind(SocketAddr::from(([127, 0, 0, 1], switch.port)));
    // 账本与授权位都是这一个 `Runtime`：`impl Ledger for Runtime` 逐格转发，
    // `impl Grants for Runtime` 每次调用现读 `Settings.control`（总开关一关，下一次调用全拒）。
    let backend: BoxedBackend = Box::new(LedgerBackend::new(runtime.clone(), runtime.clone()));
    let server = serve(options, Some(backend))?;

    // token **不进日志**：它就在握手文件里，有需要自己去读（S1 §2.5.1 的凭据）。
    tracing::info!(
        addr = %server.addr(),
        state_file = %state_file.display(),
        "控制面已起（本机回环，凭据在握手文件里）"
    );
    Ok(Some(server))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::clock;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vb-headless-mcp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn runtime(settings: Settings) -> Runtime {
        Runtime::new(settings, clock::local())
    }

    #[test]
    fn switch_follows_the_ledger_and_defaults_to_off() {
        assert_eq!(Switch::from_settings(&Settings::default()), Switch::OFF);
        let mut settings = Settings::default();
        settings.control.enabled = true;
        settings.control.port = 47123;
        assert_eq!(
            Switch::from_settings(&settings),
            Switch {
                enabled: true,
                port: 47123
            }
        );
    }

    #[test]
    fn the_switch_off_means_no_listener_and_no_credentials() {
        let dir = temp_dir("off");
        let runtime = runtime(Settings::default());
        let handle = start(&runtime, &dir, Switch::OFF).expect("关着不该报错");
        assert!(handle.is_none(), "开关关着不许起服务");
        assert!(!dir.join(STATE_FILE).exists(), "开关关着不许写握手文件");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_switch_on_serves_and_writes_a_handshake_file() {
        let dir = temp_dir("on");
        let runtime = runtime(Settings::default());
        let server = start(
            &runtime,
            &dir,
            Switch {
                enabled: true,
                port: 0,
            },
        )
        .expect("起控制面")
        .expect("开关开着该有句柄");

        assert!(server.addr().ip().is_loopback(), "只许绑回环");
        let document: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(STATE_FILE)).expect("握手文件"))
                .expect("握手文件是 JSON");
        assert_eq!(document["port"].as_u64(), Some(server.addr().port().into()));
        assert_eq!(document["pid"].as_u64(), Some(std::process::id().into()));
        assert_eq!(
            document["token"].as_str().map(str::len),
            Some(43),
            "token 是 32 字节 base64url"
        );
        assert_eq!(document["protocolVersion"], "2026-07-28");

        // 收摊要把凭据带走（正常退出这条路：`shutdown()` 是装配层必须走的那一步，
        // 因为无屏档的 SIGTERM 不会走到 `Drop`）。
        server.shutdown();
        assert!(!dir.join(STATE_FILE).exists(), "停服该擦掉握手文件");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 死凭据清扫的三条边界：判得出来才动手，判不出来一律不动。
    #[test]
    fn dead_credentials_are_pruned_only_when_the_pid_is_known_dead() {
        let dir = temp_dir("prune");
        let state_file = dir.join(STATE_FILE);

        // ① pid 已经不在（Linux 的 pid_max 远小于这个数，永远不会有这么一个进程）→ 擦掉。
        fs::write(&state_file, r#"{"port":1,"token":"x","pid":4000000000}"#).expect("写死凭据");
        prune_dead_credentials(&state_file);
        assert!(!state_file.exists(), "pid 死了就该擦掉");

        // ② 自己这个进程还活着 → 不动（那是"另一个实例正在服务"或 pid 被复用）。
        fs::write(
            &state_file,
            format!(r#"{{"port":1,"token":"x","pid":{}}}"#, std::process::id()),
        )
        .expect("写活凭据");
        prune_dead_credentials(&state_file);
        assert!(state_file.exists(), "pid 活着就一个字都不许动");

        // ③ 判不出来（不是 JSON / 没有 pid）→ 不动，而且不许 panic。
        fs::write(&state_file, "这不是 JSON").expect("写坏文件");
        prune_dead_credentials(&state_file);
        assert!(state_file.exists(), "读不出来就不动手");
        fs::write(&state_file, r#"{"port":1,"token":"x"}"#).expect("写缺 pid 的文件");
        prune_dead_credentials(&state_file);
        assert!(state_file.exists(), "没有 pid 就不动手");

        let _ = fs::remove_dir_all(&dir);
    }
}
