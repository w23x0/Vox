//! 控制面（Agent 面）的装配胶水：把芯的账本接到 `vox-mcp` 上，并按用户开关决定起不起。
//!
//! 这一层**只做两件事**，一条业务判断都没有：
//!
//! 1. 把"起不起、绑哪个端口"读成一份数据（[`Switch`]，唯一来源是账本里的 `Settings.control`）；
//! 2. 把 `Runtime`（唯一账本）交给 `vox-mcp` 的默认后端 [`LedgerBackend`] 再 `serve`——
//!    五格逐条转发给 `endpoints` / `session`，**没有第二份状态**。
//!
//! **本文件不实现 `Grants`**（"用户让不让"）：那是 `vox-mcp` 那一侧的事
//! （`impl Grants for Runtime`，每次调用现读 `Settings.control.allow_*`），装配层自己再写一份
//! 就等于把授权位存成第二份真源。这里交下去的就是同一个 `Runtime`。
//!
//! 三个闸门里另外两道也不在这里（S1 §2.5.2）：闸门②"本机能不能"由芯的能力位回答
//! （后端直接问 `Runtime::capabilities()`），闸门③"结构上装不装得上"由
//! `Composition::validate()` / `missing_on()` 回答。
//!
//! 起停两个位置（顺序是有讲究的，两个入口的注释里各有一句）：
//!
//! - 起：[`start`] 排在 `assemble()` **最后**（`set_host_facts` 之后）。事实还没齐就开门，
//!   先连上来的客户端会拿到一份建立在默认事实上的清单——那是"广告了做不到的事"。
//! - 停：`shutdown()` 排在 `persist.flush()` **之前**。外部进程随时可能发来一次
//!   `compose_endpoint apply`，而 [`ServerHandle::shutdown`] 会等当前那次调用真跑完再 join
//!   （不打断半截的配置写入）；不等它，那次写就会落在 flush 之后，静默丢失。
//!
//! 还有一件顺手的事：起之前先擦掉上一次留下的**死凭据**（[`prune_dead_credentials`]）。
//! 「停」这条路走全了才会清掉 `control.json`，强杀/崩溃时它留着，而那张纸上的端口与 token
//! 属于一个已经不存在的进程——留着只会让 CLI 拿着废凭据反复重试。无屏盒子上这条比桌面上
//! 更要紧：重启是常态（systemd `Restart=`、断电），而 SIGTERM 不会走到 `ServerHandle` 的
//! `Drop`，握手文件就留在那儿。
//!
//! **热切换**：起停不再只读一次。设置页拨一下开关，`SettingsChanged` 一到，
//! [`ControlPlane`] 挂的那个监听器就按新开关起/停——换端口就重绑（旧的 `control.json`
//! 由 `ServerHandle::shutdown` 擦掉，新的一份由 `serve` 写）。**授权不受起停影响**：
//! `Grants` 每次 `tools/call` 现读 `Settings.control.allow_*`，总闸一关，下一次调用立刻全拒
//! （fail-closed），监听还在也一样。
//!
//! **无屏档不装热切换**（`ControlPlane::install` 只由桌面调）：它今天就没有那个监听器，
//! `Settings.control.enabled` / `port` 只有装配时读一次。无屏在 `run_daemon` 里调
//! [`start`] 一次——这是**行为不变**的要求，不是遗漏。

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use vox_core::event::{Event, Notice};
use vox_core::runtime::{Listener, Runtime};
use vox_core::settings::Settings;
use vox_mcp::session::LedgerBackend;
use vox_mcp::transport::http::{ServerHandle, ServerOptions};
use vox_mcp::{serve, BoxedBackend};

/// 握手文件名。路径固定 `<config_dir>/control.json`（S1 §2.5.1），内容
/// `{"port":…,"token":"<43 字符 base64url>","pid":…,"protocolVersion":"2026-07-28"}`，
/// Unix 下 `0600`（写入那一步在 `vox_mcp::transport::http`，本文件不重复实现）。
/// 两个外壳同名，好让 CLI 不必知道它在跟哪个外壳说话。
pub use crate::paths::CONTROL_FILE;
/// 旧名：入口与测试里一直这么叫（`mcp::STATE_FILE`），指的就是 [`CONTROL_FILE`] 那个文件名。
pub const STATE_FILE: &str = CONTROL_FILE;

/// 控制面开关的一份快照：**起不起**、**绑哪个端口**。
///
/// 授权位（`allow_microphone` 那一类）**不在这里**：它们是 `Grants` 的输入，由 `vox-mcp` 的
/// `impl Grants for Runtime` 现读同一个 `Settings.control`（每次 `tools/call` 读一次快照，
/// 界面上一开一关下一次调用立刻见效）。装配层再搬一份就是把授权位存成第二份真源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// 控制面总开关。**默认关**：装完不开，要用户明确打开。
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

    /// 从账本设置读开关。**唯一**的来源（`Settings.control`，`Settings::normalize()` 已经把
    /// 端口夹紧过：<1024 的一律归 0，那是内核的保留段）。
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.control.enabled,
            port: settings.control.port,
        }
    }
}

/// 控制面**现在的样子**：设置里要的那一档 + 真起了没有。
///
/// 快照把它原样交给界面（`SnapshotDto.control`），界面**按这份观察值渲染**——不许拿
/// `Settings.control` 自己推"应该"在跑。要什么是用户的事，起没起是事实（S0 §2.6 R5 的同一条）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// 后端最后一次按下去的开关（`Settings.control.enabled` 的观察值，不是"用户刚点的那个"）。
    pub enabled: bool,
    /// 同一次按下去的端口；`0` = 由系统分配。
    pub port: u16,
    /// **事实**：真的在监听吗（句柄在 = 在）。
    pub running: bool,
    /// 实际绑上的端口（`port` 是 0 时只有这里才知道）；没在跑时 `None`。
    pub bound_port: Option<u16>,
    /// 起不来的原因（给人看的那句）；没失败过 = `None`。
    pub error: Option<String>,
    /// 握手文件路径（Agent 的端口与 token 在里面）。路径固定，与起没起无关。
    pub state_file: String,
}

/// 控制面的起停。**唯一**一处"按开关起/停"的实现。
///
/// 两条路都走它：装配时（事实齐了才开门）与热切换（[`ControlPlane::install`]
/// 挂的监听器——`SettingsChanged` 一到就按新开关起/停）。
///
/// 动手的判据只有一条：**开关变了才动**。`applied` 是上一档开关，与新的逐字相等就什么都不做——
/// 改字幕字号、拖滑块、换音色都不该把 Agent 的连接踢断，端口没变也不该重绑。
///
/// **同步跑，而且会 join**：`reconcile` 在发事件的线程上跑（`Runtime::update_settings` 的监听器
/// 是同步调的），停服时 `ServerHandle::shutdown` 会 join 服务自己的连接线程。所以有一条必须
/// 成立的不变量：**没有任何一条 `tools/call` 能改这一档开关**——否则那次调用会 join 自己。
/// 今天它成立：`compose_endpoint` 只写 `editable` 里那几格，而 `control` 是清单里"必须逐字相同"
/// 的设备/外壳属性（`crates/vox-mcp/src/endpoints.rs::why`），草稿又是 `ledger.settings()` 的克隆
/// （`draft_settings`），那一格原样带过 → 开关没变 → 上面那条提前返回。
pub struct ControlPlane {
    runtime: Runtime,
    config_dir: PathBuf,
    inner: parking_lot::Mutex<Inner>,
}

struct Inner {
    /// 上一档开关：与新的逐字相等就什么都不做。`None` = 还一档都没按过
    /// （构造之后、第一次 `reconcile` 之前）——那一次必须真走一遍，**哪怕两档都是"全关"**：
    /// 关着的那条路也要擦死凭据（见 [`Inner::apply`]）。
    applied: Option<Switch>,
    /// 正在监听的句柄；没起 = `None`。**"起没起"的事实就是它**。
    server: Option<ServerHandle>,
    /// 最近一次起服失败的原因（成功一次 / 关一次就清掉）。
    error: Option<String>,
}

impl ControlPlane {
    /// 纯构造：**不起服务、也不挂监听器**（构造 ≠ 开门，见 [`ControlPlane::install`]）。
    pub fn new(runtime: Runtime, config_dir: PathBuf) -> Self {
        Self {
            runtime,
            config_dir,
            inner: parking_lot::Mutex::new(Inner {
                applied: None,
                server: None,
                error: None,
            }),
        }
    }

    /// 挂上 `SettingsChanged` 监听器：改完开关立刻起/停，不用重启应用。
    ///
    /// **必须排在 `set_host_facts()` 之后**。监听器一挂上，任何一次
    /// 设置变更都会走到 [`ControlPlane::reconcile`]；事实还没齐就挂，先连上来的客户端会拿到一份
    /// 建立在默认事实上的清单——那是"广告了做不到的事"。
    ///
    /// 闭包只捕 `Weak`：捕 `Arc` 会造出引用环（`Inner.listeners` → 闭包 → `ControlPlane` →
    /// `Runtime` → `Inner`），环上的引用计数永远降不到 0，控制面就再也不会析构（入口
    /// `events.rs` 的监听器头注释里有同一条的完整推导）。
    pub fn install(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let listener: Listener = Arc::new(move |event: &Event| {
            let Event::SettingsChanged { settings } = event else {
                return;
            };
            // upgrade 失败 = 已经析构（进程退出中），这时起停没有意义。
            let Some(plane) = weak.upgrade() else { return };
            plane.reconcile(Switch::from_settings(settings));
        });
        self.runtime.add_listener(listener);
    }

    /// 控制面现在的样子（快照与设置页读它）。
    pub fn status(&self) -> Status {
        let inner = self.inner.lock();
        // 还一档都没按过 = 全关那一档（没监听、也没失败过）——第一次 `reconcile` 之前的诚实答案。
        let applied = inner.applied.unwrap_or(Switch::OFF);
        Status {
            enabled: applied.enabled,
            port: applied.port,
            running: inner.server.is_some(),
            bound_port: inner.server.as_ref().map(|server| server.addr().port()),
            error: inner.error.clone(),
            state_file: self
                .config_dir
                .join(STATE_FILE)
                .to_string_lossy()
                .into_owned(),
        }
    }

    /// 按开关起/停。开关没变就什么都不做（见 [`ControlPlane`] 的说明）。
    ///
    /// 在**发事件的那个线程上同步跑**：`Runtime::update_settings` 的监听器是同步调的，所以
    /// 界面那次 `update_settings` 一返回，服务就已经起好/停干净了（设置页据此拉快照，
    /// 不会拿到半截状态）。停服会等当前那次 `tools/call` 跑完，不打断半截的配置写入。
    pub fn reconcile(&self, switch: Switch) {
        let failure = {
            let mut inner = self.inner.lock();
            inner.apply(&self.runtime, &self.config_dir, switch)
        };
        if let Some(error) = failure {
            // 起不来不致命：界面、密钥、流水线都不依赖它——记一条 Notice 说清原因；
            // 同一句也在 `Status::error` 里，设置页那一格照实显示。
            self.runtime.notify(Notice::warning(format!(
                "控制面没起来（Agent 连不上）：{error}"
            )));
        }
    }

    /// 退出前收摊：停监听、擦掉自己写的那份握手文件。
    ///
    /// 两个入口的 `shutdown` 都必须**在 `persist.flush()` 之前**调它——它是外部进程能碰账本
    /// 的那条路。
    pub fn shutdown(&self) {
        self.inner.lock().stop();
    }
}

impl Inner {
    /// 把开关拨到 `switch`，返回起服失败的原因（成功 / 没动手 = `None`）。
    fn apply(&mut self, runtime: &Runtime, config_dir: &Path, switch: Switch) -> Option<String> {
        if self.applied == Some(switch) {
            return None;
        }
        // 换开关 = 换监听：先把旧的收干净。`shutdown()` 会等当前那次 `tools/call` 跑完，
        // 并擦掉自己写的那份 `control.json`——端口变了以后，那张纸上的端口就是错的
        // （不擦的话 CLI 会拿着废凭据反复重试）。
        self.stop();

        // **关着也要走一遍 `start`**：它在起服务之前先擦掉上一次留下的死凭据，而开关关着时
        // 它是唯一会去擦的人（"这台机器上没有控制面"这件事，比留一张指向不存在进程的纸准确）。
        // 开关关着时它只做这一件事，返回 `Ok(None)`。
        let mut failure = None;
        match start(runtime, config_dir, switch) {
            Ok(Some(server)) => self.server = Some(server),
            Ok(None) => {}
            Err(error) => failure = Some(error.to_string()),
        }
        self.error = failure.clone();
        // **失败也记**：不记的话，之后每一条 `SettingsChanged`（改字号、拖滑块）都会重试一次
        // 起服，屏幕上刷一串同样的 Notice。要重试就动开关——关掉再打开，或换个端口。
        self.applied = Some(switch);
        failure
    }

    fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown();
        }
    }
}

/// 起控制面前先擦掉上一次留下的**死凭据**：`control.json` 里记着那个进程的 pid，而它已经不在。
///
/// 为什么会有死凭据：正常停服（`ServerHandle` 的 `Drop`）自己会擦掉它，但被 SIGTERM 强杀、
/// 崩溃、断电时那一步没跑成，文件就留在配置目录里。它记着的端口与 token 属于一个**不存在**
/// 的进程——CLI 照着它连，看到的是"连不上"，而不是"这台机器上没有控制面"；总开关关着的时候
/// 更没人会去覆盖它，能一直烂在那儿。
///
/// 判据只有一条：**文件里的 pid 已经不在了**。除此之外一律不动手——
/// 读不出来 / 不是 JSON / 没有 pid 是"判不出来"，pid 还活着是"另一个实例正在服务"
/// （或 pid 被别人复用）。留一份旧凭据只是丑；删掉正在服务的那个实例的凭据是事故。
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
/// 没有跨平台写法。两个平台都朝**宁可当它活着**偏——判错成"活着"只是留下一份旧凭据，
/// 判错成"死了"会删掉正在服务的那个实例的凭据。
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    // `kill(pid, 0)` 才是语义最准的写法，但 std 没有这个 API，也不为它引 libc：
    // `/proc/<pid>` 在就是有这么一个进程（内核给每个进程都建这个目录）。
    // 两处已知偏差，方向都是"多报活"：① 僵尸进程的目录还在（父进程收尸之前，毫秒级窗口）；
    // ② `hidepid=2` 挂载下看不见别人的进程。无屏档里控制面与入口是同一个进程、同一个用户，
    // 两个偏差都碰不到。
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: 只要"看一眼"的权限，不终止、不写、不改。
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(process) => {
            // SAFETY: 句柄是本函数刚打开的，只关一次。
            unsafe {
                let _ = CloseHandle(process);
            }
            true
        }
        // `ERROR_INVALID_PARAMETER`(87) 是"没有这个 pid"；其余（最典型是 access denied：
        // 进程在，只是不让开）一律当活着。错误码要先包成 HRESULT 才跟 `Error::code()` 可比
        // （`Error::from_thread` 内部就是这么映射的）。
        Err(error) => error.code() != windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_alive(_pid: u32) -> bool {
    // 判不出来就不动它——偏差方向跟上面两个平台一致。
    true
}

/// 按开关起控制面。
///
/// - 开关关着 → `Ok(None)`：**不监听、不写握手文件**（默认档，fail-closed 的样子）。
/// - 开关开着 → 真起服务，返回的 [`ServerHandle`] 由 [`ControlPlane`] 保管到下一次换开关或退出
///   （句柄一 drop 就等于停服）。
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
    // 账本与授权位都是这一个 `Runtime`（`Arc` 的壳，克隆等于共享）：
    // - `impl Ledger for Runtime`：设置、能力位、会话配置、起停——逐格转发，没有第二份状态；
    // - `impl Grants for Runtime`：`Settings.control.allow_*` **每次调用现读**，所以界面上一开
    //   一关，下一次 `tools/call` 立刻见效，不需要重启，也不需要谁把位搬过来。
    // 总开关关着时这一位一位都不开（那两道 fail-closed 在 `vox-mcp` 的实现里）。
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
    use crate::clock;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vb-headless-mcp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn runtime(settings: Settings) -> Runtime {
        Runtime::new(settings, clock::local_clock())
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
