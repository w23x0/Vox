//! 装配层本体：把芯、PipeWire 音频、配置、状态出口、控制面拼成一个能跑的进程。
//!
//! 两步走，对应两种模式：
//!
//! - [`Assembly`]：**芯的那一半**——配置目录 → 设置 → 时钟 → 账本 → 密钥 → 用量 →
//!   音频三件套 → 设备快照 → 宿主事实。`--print-capabilities` / `--print-composition` /
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
//! | 控制面（最后一步） | 一样，**排在事实注入之后**：事实没齐就开门 = 广告了做不到的事 |
//!
//! 退出顺序也照桌面档那套理由（[`Daemon::shutdown`]）：控制面 → 工作线程 → 落盘。

use std::sync::Arc;
use std::thread;

use vox_core::composition::Composition;
use vox_core::event::{Event, Notice};
use vox_core::pipeline::{Deps, PipelineEngine};
use vox_core::ports::DeviceRegistry;
use vox_core::runtime::{DeviceSnapshot, PipelineControl, Runtime};
use vox_core::{Pipeline, Settings};
use vox_mcp::transport::http::ServerHandle;

use crate::cli::{self, Mode, Start};
use crate::config::{self, Paths};
use crate::persist::Persist;
use crate::secrets::SecretFile;
use crate::{dsp, mcp, platform, status, sys};

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
    runtime: Runtime,
    paths: Paths,
    persist: Arc<Persist>,
    /// 音频三件套（采集 / 播放 / 设备目录）。常驻模式下设备快照与事实在
    /// [`Assembly::assemble`] 里就用它扫过一遍，工厂留给 [`Daemon`] 注入引擎；
    /// 报告模式（[`Probe::Nothing`]）只装工厂、不连 PipeWire。
    platform: platform::Platform,
}

impl Assembly {
    /// 装配。每一步的顺序都有理由，注释里逐条写清了。
    ///
    /// `probe` 决定要不要碰 PipeWire（见 [`Probe`]）：报告三模式传
    /// [`Probe::Nothing`]，那时设备目录是空的、也没有启动提示。
    pub fn assemble(paths: Paths, probe: Probe) -> Result<Self> {
        // 1. 落盘层 + 设置。读不出来就用默认值（配置坏了也不该让服务起不来——
        //    起不来连控制面都没有，用户就没法远程修它）。
        let persist = Persist::start(paths.dir.clone());
        let settings: Settings = config::load_settings(&paths.settings);
        tracing::info!(
            settings = %paths.settings.display(),
            config_dir = %paths.dir.display(),
            "配置已读"
        );

        // 2. 时钟 + 账本。改配置的写入口只有 `Runtime::update_settings` 一个。
        let runtime = Runtime::new(settings, sys::clock::local());

        // 3. 密钥：文件 0600 + 环境变量覆盖（无屏盒子上常常没有 Secret Service）。
        //    顺手把存着的密钥读进来。
        let secrets = SecretFile::new(&paths.dir);
        let has_stored_keys = secrets.stored_keys();
        runtime.set_secret_store(Arc::new(secrets));
        if has_stored_keys {
            // 明文这件事必须让人知道（`secrets.rs` 头注释里有代价说明）。
            let path = paths.secret();
            tracing::warn!(path = %path.display(), "API 密钥以明文（0600）存在这个文件里");
            runtime.notify(Notice::warning(format!(
                "API 密钥以明文（0600）存在 {}：无屏档没有 Secret Service，这一份就是兜底",
                path.display()
            )));
        }

        // 4. 用量账本。**要在挂落盘监听之前**灌进去，免得刚读出来就又标脏写一遍。
        runtime.load_usage(config::load_usage(&paths.usage()));

        // 5. 音频三件套 + 设备快照 + 宿主事实。无屏档**只扫一次**：没有界面要秒级反映
        //    插拔，插拔后重启进程即可（周期性枚举留给下一轮，见 EMBEDDED §3.2）。
        //    报告模式连这一次都不扫（`LinuxDeviceRegistry` 每次调用连一轮 PipeWire，
        //    而清单与能力报告都不吃设备——见 `Probe`）。
        let platform = platform::platform()?;
        if probe == Probe::PipeWire {
            let devices = scan_devices(platform.registry.as_ref());
            tracing::info!(
                inputs = devices.inputs.len(),
                outputs = devices.outputs.len(),
                audio_apps = devices.audio_apps.len(),
                "设备目录已扫（无屏档只在装配时扫一次）"
            );
            runtime.set_devices(devices);
        } else {
            tracing::debug!("报告模式：不扫设备目录（清单与能力报告都不吃设备）");
        }
        // 事实**只报关掉的位**，位由芯算（档位上限 − 关掉的）。定义者逐条见 `platform::host_facts`。
        let facts = platform::host_facts();
        tracing::info!(
            tier = ?facts.host,
            off = ?facts.off.keys().map(|bit| bit.id()).collect::<Vec<_>>(),
            "宿主事实已注入（位由芯算）"
        );
        runtime.set_host_facts(facts);

        // 6. 启动提示：只是给人看的事实（PipeWire 在不在），不改任何位。探它要连一次
        //    PipeWire，而提示的出口是 journal（报告模式连 notice 都不打出来），所以报告模式跳过。
        if probe == Probe::PipeWire {
            for note in platform::startup_notes() {
                runtime.notify(Notice::warning(note));
            }
        }

        // 7. 落盘监听：设置/用量一变就标脏（真正的写盘在去抖线程里）。
        //    排在 `load_usage` 之后（否则刚读出来的那份会被当成"变了"再写一遍）。
        let sink = Arc::clone(&persist);
        runtime.add_listener(Arc::new(move |event| match event {
            Event::SettingsChanged { settings } => sink.save_settings(settings),
            Event::UsageChanged { usage } => sink.save_usage(usage),
            _ => {}
        }));

        let switch = mcp::Switch::from_settings(&runtime.settings());
        tracing::info!(
            enabled = switch.enabled,
            port = switch.port,
            "控制面开关（来自 settings.control；改完要重启进程才生效）"
        );

        Ok(Self {
            runtime,
            paths,
            persist,
            platform,
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// 状态出口：能力位报告的 JSON（`--print-capabilities` / `--dry-run` 打的就是它）。
    pub fn capabilities_json(&self) -> Result<String> {
        Ok(status::capabilities_json(&self.runtime)?)
    }

    /// 状态出口：**两份清单 + 有效能力位**的 JSON（`--print-composition` 打的就是它，S0 §4.3-A）。
    pub fn composition_json(&self) -> Result<String> {
        Ok(status::composition_json(&self.runtime)?)
    }
}

/// 跑得起来的那一半。
pub struct Daemon {
    runtime: Runtime,
    paths: Paths,
    persist: Arc<Persist>,
    engine: Arc<PipelineEngine>,
    /// 网络运行时。`WsTransport::new(handle)` 只拿 `Handle`，而句柄**不保活** runtime，
    /// 所以这里得留一份；它在运行期只被读一次（`Deps` 的工厂闭包拿的是 `Handle`），
    /// 所以字段名前缀下划线。声明在 `engine` **之后**：析构按声明顺序来，先收线程再收 runtime。
    _net: tokio::runtime::Runtime,
}

impl Daemon {
    /// 接上网络运行时与流水线引擎，并把引擎注入账本（`Runtime` 只认 [`PipelineControl`]）。
    pub fn start(assembly: Assembly) -> Result<Self> {
        let Assembly {
            runtime,
            paths,
            persist,
            platform,
        } = assembly;

        let net = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(NET_WORKER_THREADS)
            .enable_all()
            .build()?;
        let handle = net.handle().clone();
        let deps = Deps {
            // 一条腿一根新 socket，都跑在同一个 runtime 上（跟桌面档复用 Tauri 那个 runtime 同理）。
            transport: Box::new(move || Box::new(vox_net::WsTransport::new(handle.clone()))),
            capture: platform.capture,
            playback: platform.playback,
            denoise: dsp::denoise_factory(),
            resample: dsp::resample_factory(),
        };

        let engine = PipelineEngine::new(runtime.clone(), deps);
        runtime.set_control(Arc::clone(&engine) as Arc<dyn PipelineControl>);

        Ok(Self {
            runtime,
            paths,
            persist,
            engine,
            _net: net,
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// 按开关起控制面。**只在这里起**：事实已经注入完了（[`Assembly::assemble`] 的第 5 步），
    /// 早开门会让先连上来的客户端拿到一份建立在默认事实上的清单——那是"广告了做不到的事"。
    pub fn control_plane(&self) -> std::io::Result<Option<ServerHandle>> {
        let switch = mcp::Switch::from_settings(&self.runtime.settings());
        mcp::start(&self.runtime, &self.paths.dir, switch)
    }

    /// 按 `--start` 开腿。无屏设备没有界面可按，开腿这件事只能由启动参数（= unit）或
    /// 控制面的 `session_open` 说。
    pub fn start_pipelines(&self, start: Start) {
        for pipeline in pipelines_of(start) {
            self.runtime.start(pipeline);
            // 立刻读一次状态：账本没接下这次启动（比如还没配密钥）时会留在 `Idle`，
            // 原因由芯发一条 `Notice`（`status::wire` 会把它记下来）。
            let state = self.runtime.pipeline_state(pipeline);
            tracing::info!(pipeline = pipeline.label(), state = state.label(), "开工");
        }
    }

    /// 收摊。顺序有讲究（跟桌面档 `lib.rs::shutdown` 同一套理由）：
    ///
    /// 1. **控制面先停**：它是外部进程能碰账本的那条路，而 `shutdown()` 会等当前那次调用
    ///    真跑完（不打断半截的配置写入）；不等它，那次写就落在 flush 之后，静默丢失。
    /// 2. **工作线程其次**：`engine.shutdown()` 走停止握手、等线程真收摊，它们也会 emit 事件。
    /// 3. **最后落盘**：此时没有别的线程能碰账本了（无屏档没有设备轮询线程）。
    pub fn shutdown(self, control: Option<ServerHandle>) {
        if let Some(control) = control {
            control.shutdown();
        }
        self.engine.shutdown();
        self.persist.flush();
        tracing::info!("已收摊");
    }
}

/// 同步扫一遍设备目录。**只有常驻模式会调**（[`Probe::PipeWire`]）：每调一次就连一轮
/// PipeWire（`LinuxDeviceRegistry` 无状态），而报告三模式的输出一条设备都不吃。
///
/// 跟桌面档同口径：任一项失败就给空列表（少几个选项，比整个面板打不开好）——无屏档这里是
/// "报告里少几行"，不影响流水线。
fn scan_devices(registry: &dyn DeviceRegistry) -> DeviceSnapshot {
    DeviceSnapshot {
        inputs: registry.input_devices().unwrap_or_default(),
        outputs: registry.output_devices().unwrap_or_default(),
        audio_apps: registry.audio_apps().unwrap_or_default(),
        virtual_cable_installed: registry.virtual_cable_installed(),
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
    let paths = Paths::resolve(args.config.as_deref())?;
    // 只有常驻模式会写盘（设置 / 用量 / 密钥 / 握手文件都落在配置目录里），所以只有它建目录。
    // 报告三模式**只读**：`--print-composition` 打一份 JSON 就在别人机器上留一个空目录，那是副作用。
    if !args.mode.is_read_only() {
        paths.ensure_dir();
    }
    let assembly = Assembly::assemble(paths, Probe::of(args.mode))?;

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
/// 不写文件、不监听端口、不起流水线（见 [`run`] 与 [`crate::config::Paths::ensure_dir`]）。
fn dry_run(assembly: &Assembly) {
    let facts = assembly.runtime.host_facts();
    tracing::info!(
        tier = ?facts.host,
        config_dir = %assembly.paths.dir.display(),
        "试装完成：不监听端口、不碰 PipeWire（不枚举设备、不探可用性）、不建配置目录、不开流、不起流水线"
    );
    let legs = legs_to_check(&assembly.runtime.settings());
    let labels: Vec<&str> = legs.iter().map(|pipeline| pipeline.label()).collect();
    let problems = check_manifests(&assembly.runtime, &legs);
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
/// 而 SIGTERM / SIGINT 的默认处置本来就是终止进程（代价见 `persist.rs` 头注释）。
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
        Paths::resolve(Some(&dir.join(config::SETTINGS_FILE))).expect("解析路径")
    }

    /// 装配出来的账本必须是这一档的事实：档位 `linux_headless`、位由芯算。
    #[cfg(target_os = "linux")]
    #[test]
    fn assembly_injects_the_headless_facts() {
        let paths = temp_paths("facts");
        let dir = paths.dir.clone();
        let assembly =
            Assembly::assemble(paths, Probe::PipeWire).expect("装配（Linux 上音频三件套装得起来）");
        let facts = assembly.runtime.host_facts();
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
            let assembly = Assembly::assemble(paths, Probe::PipeWire).expect("装配");
            let json = assembly.composition_json().expect("清单");
            let _ = std::fs::remove_dir_all(&dir);
            json
        };
        let unscanned = {
            let paths = temp_paths("probe-nothing");
            let dir = paths.dir.clone();
            let assembly = Assembly::assemble(paths, Probe::Nothing).expect("装配");
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
            let assembly = Assembly::assemble(paths, Probe::PipeWire).expect("装配");
            Daemon::start(assembly).expect("起引擎")
        };
        #[cfg(not(target_os = "linux"))]
        let daemon = {
            // 非 Linux：装配本来就该失败（`platform::platform()` 明确报错），
            // 这条用例在那边的意义到此为止。
            assert!(Assembly::assemble(paths, Probe::PipeWire).is_err());
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

        let dir = daemon.paths.dir.clone();
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
        let assembly = Assembly::assemble(paths, Probe::Nothing).expect("装配");
        let legs = legs_to_check(&assembly.runtime.settings());
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

        let assembly = Assembly::assemble(paths, Probe::Nothing).expect("装配");
        let legs = legs_to_check(&assembly.runtime.settings());
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
