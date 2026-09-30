//! 装配层本体：把芯、PipeWire 音频、配置、状态出口、控制面拼成一个能跑的进程。
//!
//! 两步走，对应两种模式：
//!
//! - [`Assembly`]：**芯的那一半**——本机的端口列成一份 [`HostPorts`]，剩下的共用步骤交给
//!   [`vox_host::Core::assemble`]（落盘 → 设置+时钟+账本 → 密钥 → 用量 → 事实 → 启动提示 →
//!   落盘监听），再扫一次设备目录。`--print-capabilities` / `--print-composition` /
//!   `--dry-run` 到这一步就够，而且走 [`Probe::Nothing`]：**不碰 PipeWire**（不枚举设备、
//!   不探可用性），也不建配置目录（只读）。
//! - [`Daemon`]：**跑得起来的那一半**——接上网络运行时与流水线引擎，起控制面，开工。
//!
//! 跟桌面档（`app/src-tauri/src/lib.rs::assemble`）的对照：
//!
//! | 桌面档那一步 | 无屏档 |
//! | --- | --- |
//! | 设置 + 时钟 + `Runtime` | 一样，但配置目录来自 `--config` / 环境变量（没有 Tauri） |
//! | 密钥库（DPAPI / Secret Service） | 文件 + 0600（无屏盒子上常常没有 Secret Service） |
//! | 用量账本 | 一样 |
//! | 窗口先亮出来 | **没有窗口** |
//! | 流水线引擎（复用 Tauri 的 tokio） | 一样，但 tokio 由本进程自己起 |
//! | 悬浮窗 / 头显 / 设备轮询 / 事件桥 / 热键 / 托盘 / 启动提示 | **整块不做**：前三个在上限之外，事件桥变日志（`status::wire`），后两个没有 |
//! | 虚拟麦接线 + 注入事实 | 只注入事实；**不建虚拟麦节点**（位在上限之外） |
//! | 事实 / 启动提示 / 落盘监听推到最后（`Finish::AfterHostSteps`） | **在 `Core::assemble` 里做完**（`Finish::InAssemble`）：这一档的事实定义者装配期就已经是定值，不像桌面档要等悬浮窗/托盘 |
//! | 控制面（最后一步） | 一样，**排在事实注入之后**：事实没齐就开门 = 广告了做不到的事 |
//!
//! 退出顺序也照桌面档那套理由（[`Daemon::shutdown`]）：控制面 → 工作线程 → 落盘。

use std::sync::Arc;
use std::thread;

use vox_core::composition::Composition;
use vox_core::pipeline::{PipelineEngine, TransportFactory};
use vox_core::runtime::Runtime;
use vox_core::{Pipeline, Settings};
use vox_host::core::{Finish, Notes, PersistMode};
use vox_host::paths::Paths;
use vox_host::secrets::SecretBackend;
use vox_host::{Core, HostPorts};
use vox_mcp::transport::http::ServerHandle;

use crate::cli::{self, Mode, Start};
use crate::config;
use crate::{platform, status};

/// 装配层的错误：一律"是什么就是什么"，没有自定义错误类型——这一层只把别处的失败串起来。
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// 网络运行时的工作线程数。跟 `vox_net::WsTransport::standalone()` 同值（2）：
/// 一条腿一根 socket，两条腿也就两根，多起的线程在 1–2 核的小板子上纯属白占。
const NET_WORKER_THREADS: usize = 2;

/// 装配时**碰不碰这台机器上的 PipeWire**。
///
/// 报告三模式（`--print-capabilities` / `--print-composition` / `--dry-run`）走
/// [`Probe::Nothing`]：它们的输出只吃**设置 + [`HostFacts`]**——清单是
/// `Composition::of(设置, 事实)`、能力报告是"档位上限 − 关掉的位"，设备目录与
/// "PipeWire 在不在"一条都不进任何一格。而这两样都要真连一次 PipeWire
/// （`LinuxDeviceRegistry` 每次调用连一轮、`pipewire_available()` 也要连）：打一份 JSON
/// 不该去连音频服务。
///
/// 常驻模式走 [`Probe::PipeWire`]：设备目录要进账本（`set_devices`，界面/控制面才看得到
/// 插拔后的设备），启动提示要进 journal（`startup_notes`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// 连一次 PipeWire：枚举设备目录 + 探可用性（发一条启动提示）。
    PipeWire,
    /// 什么都不碰。
    Nothing,
}

impl Probe {
    /// 这个模式要不要碰 PipeWire：报告三模式不要，常驻模式要。
    pub fn of(mode: Mode) -> Self {
        if mode.is_read_only() {
            Probe::Nothing
        } else {
            Probe::PipeWire
        }
    }
}

/// 芯的那一半：所有模式都要的那一份。
pub struct Assembly {
    /// 共享宿主层跑完共用那几步之后的产物：账本、落盘器、配置目录、时钟。
    core: Core,
    /// 本机的端口。`Core::assemble` 只读它，**留给 [`Daemon::start`] 装引擎**——
    /// 报告三模式走不到那一步，所以这一份在那里只是原样带过去。
    ports: HostPorts,
}

/// 这一档的"本机有什么"，一次列清。
///
/// 装配读它建账本、装引擎读它拿三个音频工厂——**入口参数只有这一处**（S4-A W5）。
/// 音频三件套来自 [`platform::platform`]：**连接本身不在这儿探**（`PipeWire 在不在`是运行期
/// 事实，见 [`Probe`]），起流那一刻才知道采不采得到声，这里只把三个工厂装好。
fn host_ports(paths: &Paths) -> Result<HostPorts> {
    let platform = platform::platform()?;
    Ok(HostPorts {
        kind: platform::host_kind,
        facts: platform::host_facts,
        clock: vox_host::clock::local_clock(),
        // 密钥：文件 0600 + 环境变量覆盖（无屏盒子上常常没有 Secret Service）。
        // 文件名是这一档的口味（`config::secret_path` → `secret.json`），机制在
        // `vox_host::secrets::file`；共享层要的是**选择**（枚举），不是建好的存储。
        secret: SecretBackend::File {
            path: config::secret_path(paths),
        },
        capture: platform.capture,
        playback: platform.playback,
        registry: platform.registry,
        startup_notes: platform::startup_notes,
    })
}

impl Assembly {
    /// 装配。每一步的顺序都有理由，注释里逐条写清了。
    ///
    /// 共用的那 7 步在 [`Core::assemble`] 里（与桌面档逐条同源，理由随注释搬过去了）；
    /// 剩下的是这一档特有的：装 [`HostPorts`]、扫一次设备目录。
    ///
    /// `probe` 决定要不要碰 PipeWire（见 [`Probe`]）：报告三模式传
    /// [`Probe::Nothing`]，那时设备目录是空的、也没有启动提示。
    ///
    /// `persist_mode` 决定落盘层建不建配置目录（见 [`PersistMode`]）：报告三模式传
    /// [`PersistMode::ReadOnly`]——"打一份 JSON 就在别人机器上留一个空目录"是副作用，
    /// `tests/headless_entry.rs` 里有两条黑盒用例钉着它。
    pub fn assemble(
        paths: Paths,
        ports: HostPorts,
        probe: Probe,
        persist_mode: PersistMode,
    ) -> Result<Self> {
        // 1–4 步（落盘 → 设置+时钟+`Runtime` → 密钥 → 用量）与桌面档逐条同源；
        // 第 5–7 步（事实 → 启动提示 → 落盘监听）也在这里做完（`Finish::InAssemble`）：
        // 这一档的事实定义者（档位、PipeWire 有没有、systemd 单元）装配期就都是定值，
        // 不像桌面档要等悬浮窗 / 事件桥 / 热键 / 托盘起来才敢注入。
        // 启动提示受 `Probe` 挡：报告三模式连 notice 都不打出来。
        let notes = if probe == Probe::PipeWire {
            Notes::Always
        } else {
            Notes::Never
        };
        let core = Core::assemble(paths, &ports, persist_mode, notes, Finish::InAssemble)?;

        // 5. 设备快照。无屏档**只扫一次**：没有界面要秒级反映插拔，插拔后重启进程即可
        //    （周期性枚举留给下一轮，见 EMBEDDED §3.2）。报告模式连这一次都不扫
        //    （`LinuxDeviceRegistry` 每次调用连一轮 PipeWire，而清单与能力报告都不吃
        //    设备——见 `Probe`）。
        //    排在 `Core::assemble` 之后：账本得先在。它与注入事实的先后因此与今天相反，
        //    但两者都可观测的出口（事件 / journal）这时都还没挂上，无屏档看不到差别。
        if probe == Probe::PipeWire {
            let devices = vox_host::scan_devices(ports.registry.as_ref());
            tracing::info!(
                inputs = devices.inputs.len(),
                outputs = devices.outputs.len(),
                audio_apps = devices.audio_apps.len(),
                "设备目录已扫（无屏档只在装配时扫一次）"
            );
            core.runtime.set_devices(devices);
        } else {
            tracing::debug!("报告模式：不扫设备目录（清单与能力报告都不吃设备）");
        }

        let switch = vox_host::Switch::from_settings(&core.runtime.settings());
        tracing::info!(
            enabled = switch.enabled,
            port = switch.port,
            "控制面开关（来自 settings.control；改完要重启进程才生效）"
        );

        Ok(Self { core, ports })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.core.runtime
    }

    pub fn paths(&self) -> &Paths {
        &self.core.paths
    }

    /// 状态出口：能力位报告的 JSON（`--print-capabilities` / `--dry-run` 打的就是它）。
    pub fn capabilities_json(&self) -> Result<String> {
        Ok(status::capabilities_json(&self.core.runtime)?)
    }

    /// 状态出口：**两份清单 + 有效能力位**的 JSON（`--print-composition` 打的就是它，S0 §4.3-A）。
    pub fn composition_json(&self) -> Result<String> {
        Ok(status::composition_json(&self.core.runtime)?)
    }
}

/// 跑得起来的那一半。
pub struct Daemon {
    /// 共享宿主层那一份（账本、落盘、配置目录、时钟）。
    core: Core,
    engine: Arc<PipelineEngine>,
    /// 网络运行时。`WsTransport::new(handle)` 只拿 `Handle`，而句柄**不保活** runtime，
    /// 所以这里得留一份；它在运行期只被读一次（`HostPorts::engine` 的传输工厂闭包拿的是
    /// `Handle`），所以字段名前缀下划线。声明在 `engine` **之后**：析构按声明顺序来，
    /// 先收线程再收 runtime。
    _net: tokio::runtime::Runtime,
}

impl Daemon {
    /// 接上网络运行时与流水线引擎（`HostPorts::engine` 同时把引擎注入账本）。
    pub fn start(assembly: Assembly) -> Result<Self> {
        let Assembly { core, ports } = assembly;

        // 网络运行时由本进程自建（X18：桌面档复用 Tauri 那一个）。**报告三模式走不到这里**，
        // 所以那个 runtime 不会被建出来。
        let net = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(NET_WORKER_THREADS)
            .enable_all()
            .build()?;
        // 一条腿一根新 socket，都跑在同一个 runtime 上（跟桌面档复用 Tauri 那个 runtime 同理）。
        let handle = net.handle().clone();
        let transport: TransportFactory =
            Box::new(move || Box::new(vox_net::WsTransport::new(handle.clone())));

        let engine = ports.engine(&core.runtime, transport);

        Ok(Self {
            core,
            engine,
            _net: net,
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.core.runtime
    }

    pub fn paths(&self) -> &Paths {
        &self.core.paths
    }

    /// 按开关起控制面。**只在这里起**：事实已经注入完了（`Core::assemble` 的第 5 步），
    /// 早开门会让先连上来的客户端拿到一份建立在默认事实上的清单——那是"广告了做不到的事"。
    pub fn control_plane(&self) -> std::io::Result<Option<ServerHandle>> {
        let switch = vox_host::Switch::from_settings(&self.core.runtime.settings());
        vox_host::control::start(&self.core.runtime, &self.core.paths.dir, switch)
    }

    /// 按 `--start` 开腿。无屏设备没有界面可按，开腿这件事只能由启动参数（= unit）或
    /// 控制面的 `session_open` 说。
    pub fn start_pipelines(&self, start: Start) {
        for pipeline in pipelines_of(start) {
            self.runtime().start(pipeline);
            // 立刻读一次状态：账本没接下这次启动（比如还没配密钥）时会留在 `Idle`，
            // 原因由芯发一条 `Notice`（`status::wire` 会把它记下来）。
            let state = self.runtime().pipeline_state(pipeline);
            tracing::info!(pipeline = pipeline.label(), state = state.label(), "开工");
        }
    }

    /// 收摊。**刻意保持顺序代码**，不抽成步骤链（S4-A §9 M1）：下面那句不变量是这一层唯一
    /// 要保护的东西。跟桌面档 `lib.rs::shutdown` 同一套理由。
    ///
    /// 1. **控制面先停**：它是外部进程能碰账本的那条路，而 `shutdown()` 会等当前那次调用
    ///    真跑完（不打断半截的配置写入）；不等它，那次写就落在 flush 之后，静默丢失。
    /// 2. **工作线程其次**：`engine.shutdown()` 走停止握手、等线程真收摊，它们也会 emit 事件。
    /// 3. **最后落盘**：此时没有别的线程能碰账本了（无屏档没有设备轮询线程）。
    ///
    /// **唯一要守住的不变量：`control.shutdown()` < `engine.shutdown()` < `persist.flush()`。**
    pub fn shutdown(self, control: Option<ServerHandle>) {
        if let Some(control) = control {
            control.shutdown();
        }
        self.engine.shutdown();
        self.core.persist.flush();
        tracing::info!("已收摊");
    }
}

fn pipelines_of(start: Start) -> Vec<Pipeline> {
    match start {
        Start::None => Vec::new(),
        Start::Speak => vec![Pipeline::Speak],
        Start::Listen => vec![Pipeline::Listen],
        Start::All => vec![Pipeline::Speak, Pipeline::Listen],
    }
}

/// 入口：解析好的参数 + 装配 + 分派。`main.rs` 只调这一个函数。
pub fn run(args: cli::Args) -> Result<()> {
    let paths = match args.config.as_deref() {
        Some(path) => Paths::from_settings_file(path)?,
        None => Paths::from_env()?,
    };
    // 只有常驻模式会写盘（设置 / 用量 / 密钥 / 握手文件都落在配置目录里），所以只有它建目录。
    // 报告三模式**只读**：`--print-composition` 打一份 JSON 就在别人机器上留一个空目录，那是副作用。
    // 这句同时管着 `control.json` / `secret.json` 的父目录，落盘层自己那份
    // （`Persist::new` 不建目录）只是额外保证，不是替代。
    if !args.mode.is_read_only() {
        paths.ensure_dir();
    }
    let assembly = Assembly::assemble(
        paths.clone(),
        host_ports(&paths)?,
        Probe::of(args.mode),
        if args.mode.is_read_only() {
            PersistMode::ReadOnly
        } else {
            PersistMode::Writing
        },
    )?;

    match args.mode {
        Mode::PrintCapabilities => {
            println!("{}", assembly.capabilities_json()?);
            Ok(())
        }
        // S0 §4.3-A：装配完打两份清单就退（不碰 PipeWire、不建目录、不写文件、不监听端口、不起流水线）。
        Mode::PrintComposition => {
            println!("{}", assembly.composition_json()?);
            Ok(())
        }
        Mode::DryRun => {
            dry_run(&assembly);
            Ok(())
        }
        Mode::Run => run_daemon(args, assembly),
    }
}

/// 只走装配：报到这一步为止的结论，然后退出。
///
/// 除了能力位报告，还会把**两条腿的清单**各过一遍（`Composition::of` → `validate` →
/// `missing_on`）——那正是 `Plan::build` 在 Start 那一刻要做的事，不碰设备、不连云端
/// （`session_config_for` 是纯派生），所以这一步能在 systemd 起来之前当自检用。
///
/// 整个进程是**只读**的：不扫设备目录、不探 PipeWire（[`Probe::Nothing`]）、不建配置目录、
/// 不写文件、不监听端口、不起流水线（见 [`run`] 与 [`Paths::ensure_dir`]）。
fn dry_run(assembly: &Assembly) {
    let facts = assembly.runtime().host_facts();
    tracing::info!(
        tier = ?facts.host,
        config_dir = %assembly.paths().dir.display(),
        "试装完成：不监听端口、不碰 PipeWire（不枚举设备、不探可用性）、不建配置目录、不开流、不起流水线"
    );
    let legs = legs_to_check(&assembly.runtime().settings());
    let labels: Vec<&str> = legs.iter().map(|pipeline| pipeline.label()).collect();
    let problems = check_manifests(assembly.runtime(), &legs);
    if problems.is_empty() {
        tracing::info!(legs = ?labels, "清单自检通过：上面这几条腿都装得上");
    } else {
        for problem in problems {
            tracing::warn!("{problem}");
        }
    }
    match assembly.capabilities_json() {
        Ok(json) => println!("{json}"),
        Err(error) => tracing::error!(error = %error, "能力报告打不出来"),
    }
}

/// 逐条验清单：`Composition::of`（本机派生，位为假的格子在这步被关掉）→ `validate`
/// （结构合法）→ `missing_on`（装得上去）。返回"装不上的地方"，空 = 都能装。
///
/// 要验哪几条腿由调用方给（见 [`legs_to_check`]）：没选目标程序的"听人说话"本来就该
/// 起不来（芯在 `Runtime::start` 里也这么拒），那不是"装不上"，是"还没配"。
fn check_manifests(runtime: &Runtime, legs: &[Pipeline]) -> Vec<String> {
    let settings = runtime.settings();
    let facts = runtime.host_facts();
    let caps = runtime.capabilities();
    let mut problems = Vec::new();
    for pipeline in legs {
        let pipeline = *pipeline;
        let label = pipeline.label();
        let config = Runtime::session_config_for(&settings, pipeline);
        let composition = match Composition::of(&config, &facts) {
            Ok(composition) => composition,
            Err(error) => {
                problems.push(format!("{label}：清单派生失败：{error}"));
                continue;
            }
        };
        if let Err(errors) = composition.validate() {
            problems.push(format!("{label}：清单结构不合法：{}", json_of(&errors)));
        }
        let missing = composition.missing_on(&caps);
        if !missing.is_empty() {
            problems.push(format!(
                "{label}：这台机器装不上的地方：{}",
                json_of(&missing)
            ));
        }
    }
    problems
}

/// 自检要验哪几条腿。
///
/// - **对外说话**恒验：它的清单只依赖 `mic` 这一位（无屏档按上限报开）与用户的设备选择。
/// - **听人说话**只在**选了目标程序**之后才验：没选时芯自己就不让这条腿起
///   （`Runtime::start` → `Notice::error("请先选择监听程序")`），把它算成"装不上"会误导。
///   选了之后验——无屏档这一轮会**如实报出来装不上**（输入是"抓某个程序的环回"，而
///   `program_tap` 在这一档的上限之外；这档的"听"应该是 `net_in`，S3 目标、还没实现，
///   见 `docs/platform/EMBEDDED.md` §2）。报出来比让用户按下去才发现好。
fn legs_to_check(settings: &Settings) -> Vec<Pipeline> {
    let mut legs = vec![Pipeline::Speak];
    if settings.listen.target.is_some() {
        legs.push(Pipeline::Listen);
    } else {
        tracing::debug!("听人说话还没选目标程序：这次自检跳过它（不是装不上，是还没配）");
    }
    legs
}

/// `CompositionError` 的线上形状（`kind` + 明细）就是给人看的那一份——不另拼句子。
fn json_of(errors: &[vox_core::CompositionError]) -> String {
    serde_json::to_string(errors).unwrap_or_else(|_| format!("{errors:?}"))
}

fn run_daemon(args: cli::Args, assembly: Assembly) -> Result<()> {
    let daemon = Daemon::start(assembly)?;
    // 事件桥要**最早**挂上：流水线一起来就可能发事件，晚挂就漏掉启动那几步。
    status::wire(daemon.runtime());

    // 控制面：开关关着就是 `None`（不监听、不写握手文件）。起不来不致命——入口本身、
    // 密钥、流水线都不依赖它，记一条日志说清原因。
    let control = match daemon.control_plane() {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(error = %error, "控制面没起来（Agent 连不上）");
            None
        }
    };
    tracing::info!(
        control = control
            .as_ref()
            .map(|server| server.addr().to_string())
            .unwrap_or_else(|| "关着".to_string()),
        "无屏入口已跑起来"
    );

    daemon.start_pipelines(args.start);

    match args.run_for {
        Some(duration) => {
            tracing::info!(seconds = duration.as_secs(), "跑够这么久就收摊");
            thread::sleep(duration);
        }
        None => park_forever(),
    }

    daemon.shutdown(control);
    Ok(())
}

/// 一直跑（systemd `Type=exec` 的常驻形态）。**刻意不写信号处理**：那要额外依赖，
/// 而 SIGTERM / SIGINT 的默认处置本来就是终止进程（代价见 `vox_host::persist` 头注释）。
/// `park` 会被虚假唤醒打断，所以套一层循环。
fn park_forever() -> ! {
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!(
            "vb-headless-assembly-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Paths::from_settings_file(&dir.join(vox_host::SETTINGS_FILE)).expect("解析路径")
    }

    /// 装配的测试入口：先把这一档的端口列好（`host_ports`），再走共享层那几步。
    /// 跟生产路径上 `run()` 的顺序一致（`host_ports(&paths)` 在前、`Assembly::assemble` 在后）。
    fn assembly(paths: Paths, probe: Probe, persist_mode: PersistMode) -> Result<Assembly> {
        let ports = host_ports(&paths)?;
        Assembly::assemble(paths, ports, probe, persist_mode)
    }

    /// 装配出来的账本必须是这一档的事实：档位 `linux_headless`、位由芯算。
    #[cfg(target_os = "linux")]
    #[test]
    fn assembly_injects_the_headless_facts() {
        let paths = temp_paths("facts");
        let dir = paths.dir.clone();
        let assembly = assembly(paths, Probe::PipeWire, PersistMode::Writing)
            .expect("装配（Linux 上音频三件套装得起来）");
        let facts = assembly.runtime().host_facts();
        assert_eq!(facts.host, vox_core::HostKind::LinuxHeadless);
        assert!(facts.excess_off_bits().is_empty());
        // 报告能打出来，且就是这一档。
        let report: serde_json::Value =
            serde_json::from_str(&assembly.capabilities_json().expect("报告")).expect("合法 JSON");
        assert_eq!(report["tier"], "linux_headless");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 报告模式**不扫设备**是安全的：清单只吃设置 + `HostFacts`，所以"扫过设备"与"没扫"
    /// 装配出来的那一份 JSON 必须逐字相同——谁把设备目录塞进清单/能力报告，这条就红。
    /// 顺带钉住 `Probe::Nothing` 确实没扫：账本里的设备目录是空的。
    #[cfg(target_os = "linux")]
    #[test]
    fn the_report_does_not_depend_on_the_device_snapshot() {
        let scanned = {
            let paths = temp_paths("probe-pipewire");
            let dir = paths.dir.clone();
            let assembly = assembly(paths, Probe::PipeWire, PersistMode::Writing).expect("装配");
            let json = assembly.composition_json().expect("清单");
            let _ = std::fs::remove_dir_all(&dir);
            json
        };
        let unscanned = {
            let paths = temp_paths("probe-nothing");
            let dir = paths.dir.clone();
            let assembly = assembly(paths, Probe::Nothing, PersistMode::Writing).expect("装配");
            assert!(
                assembly.runtime().snapshot().devices.inputs.is_empty(),
                "报告模式不该扫设备目录"
            );
            let json = assembly.composition_json().expect("清单");
            let _ = std::fs::remove_dir_all(&dir);
            json
        };
        assert_eq!(scanned, unscanned, "清单不该吃设备目录");
    }

    /// 无屏档没有界面可按：缺省（不给 `--start`）**一条腿都不许开**。
    #[test]
    fn default_start_opens_nothing() {
        assert!(pipelines_of(Start::None).is_empty());
        assert_eq!(pipelines_of(Start::Speak), vec![Pipeline::Speak]);
        assert_eq!(
            pipelines_of(Start::All),
            vec![Pipeline::Speak, Pipeline::Listen]
        );
    }

    /// 没配密钥时开腿要如实留在"没开"（不许假装在跑），而且原因有一条 `Notice` 说清楚。
    #[test]
    fn starting_without_a_key_refuses_instead_of_pretending() {
        let paths = temp_paths("no-key");
        #[cfg(target_os = "linux")]
        let daemon = {
            let assembly = assembly(paths, Probe::PipeWire, PersistMode::Writing).expect("装配");
            Daemon::start(assembly).expect("起引擎")
        };
        #[cfg(not(target_os = "linux"))]
        let daemon = {
            // 非 Linux：装配本来就该失败（`platform::platform()` 明确报错），
            // 这条用例在那边的意义到此为止。
            assert!(assembly(paths, Probe::PipeWire, PersistMode::Writing).is_err());
            return;
        };

        daemon.start_pipelines(Start::Speak);
        assert_eq!(
            daemon.runtime().pipeline_state(Pipeline::Speak),
            vox_core::PipelineState::Idle,
            "没有密钥就不该进 Starting"
        );
        let notices = daemon.runtime().snapshot().notices;
        assert!(
            notices
                .iter()
                .any(|notice| notice.text.contains("API 密钥")),
            "拒绝的理由要说清楚：{notices:?}"
        );

        let dir = daemon.paths().dir.clone();
        daemon.shutdown(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 清单自检：缺省配置下（听人说话还没选目标程序）一切都干净。
    /// `mic` 按上限报开、`background_service` 关着也不影响清单结构。
    #[cfg(target_os = "linux")]
    #[test]
    fn manifest_check_is_clean_on_defaults() {
        let paths = temp_paths("manifests");
        let dir = paths.dir.clone();
        let assembly = assembly(paths, Probe::Nothing, PersistMode::Writing).expect("装配");
        let legs = legs_to_check(&assembly.runtime().settings());
        assert_eq!(legs, vec![Pipeline::Speak]);
        let problems = check_manifests(assembly.runtime(), &legs);
        assert!(
            problems.is_empty(),
            "缺省配置下不该有装不上的地方：{problems:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 选了目标程序之后，无屏档的自检会**如实报出**"听人说话这一轮装不上"：
    /// 它的输入是抓某个程序的环回，而 `program_tap` 在无屏档的上限之外
    /// （这档的"听"应该是 `net_in`，还没实现——EMBEDDED §2）。
    /// 自检的意义就在这儿：别让用户按下去才发现。
    #[cfg(target_os = "linux")]
    #[test]
    fn listen_leg_is_reported_unusable_until_net_in_lands() {
        let paths = temp_paths("listen-leg");
        let dir = paths.dir.clone();
        // 解析路径**不建目录**（只读的契约，见 `config.rs` 的用例），这条要写设置文件，所以自己建。
        paths.ensure_dir();
        std::fs::write(
            &paths.settings,
            r#"{"listen":{"target":{"executable":"Discord","display_name":"Discord"}}}"#,
        )
        .expect("写设置");

        let assembly = assembly(paths, Probe::Nothing, PersistMode::Writing).expect("装配");
        let legs = legs_to_check(&assembly.runtime().settings());
        assert_eq!(legs, vec![Pipeline::Speak, Pipeline::Listen]);
        let problems = check_manifests(assembly.runtime(), &legs);
        assert_eq!(problems.len(), 1, "只该报听人说话那一条：{problems:?}");
        assert!(problems[0].contains("听人说话"), "{problems:?}");
        assert!(
            problems[0].contains("missing_input"),
            "要说清是哪一类问题：{problems:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
