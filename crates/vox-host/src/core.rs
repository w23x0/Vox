//! 芯的那一半：两个入口**逐字同序**的那 7 步。
//!
//! 装配与退出顺序在这一层，因为它是**两个外壳真正共有的那一段**——它们做的事一件不多、
//! 一件不少，只是前后各自插着宿主特有的步骤（桌面多出托盘/热键/设备/悬浮/虚拟麦/OSC，
//! 无屏多出自建 tokio runtime）。把共有的那 7 步收在这里，两边的注释就只剩"差在哪"。
//!
//! 有一步要挑明：**共有的顺序不等于"每次都在 `Core::assemble` 里做完"**。第 5–7 步
//! （宿主事实 / 启动提示 / 落盘监听）要看那一位"本机有什么"的定义者有没有就位——桌面档的
//! 五位事实要等悬浮窗/头显/事件桥/热键/托盘起来，所以它走 [`Finish::AfterHostSteps`] 自己补做。
//! 见 [`Finish`]。

use std::sync::Arc;

use vox_core::event::Notice;
use vox_core::ports::Clock;
use vox_core::runtime::{DeviceSnapshot, Runtime};

use crate::entry::HostPorts;
use crate::paths::{self, Paths};
use crate::persist::Persist;

/// 同步扫一遍设备目录。**今天两份 8 行的同形函数合并成这一份**（S4-A §1.2-D9 的
/// `devices::scan` 与 `headless::scan_devices`）——那两个函数都已删掉，现在只剩本这一处实现，
/// 两个外壳都在调它。
///
/// 跟两个外壳同口径：任一项失败就给空列表——界面上少几个选项，比整个面板打不开好。
pub fn scan_devices(registry: &dyn vox_core::ports::DeviceRegistry) -> DeviceSnapshot {
    DeviceSnapshot {
        inputs: registry.input_devices().unwrap_or_default(),
        outputs: registry.output_devices().unwrap_or_default(),
        audio_apps: registry.audio_apps().unwrap_or_default(),
        virtual_cable_installed: registry.virtual_cable_installed(),
    }
}

/// 装配时要不要顺手报"启动提示"（PipeWire 在不在、托盘有没有宿主）。
/// 桌面**总是**报；无屏**只常驻模式**报（报告三模式一个字节都不写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notes {
    Always,
    Never,
}

/// 落盘层取哪个构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistMode {
    /// [`Persist::start`]：建目录 + 起去抖线程。常驻模式。
    Writing,
    /// [`Persist::new`]：不建目录、不起线程。报告三模式（`--print-capabilities` /
    /// `--print-composition` / `--dry-run`）——今天无屏档靠 `run()` 里那句
    /// `if !args.mode.is_read_only() { paths.ensure_dir() }` 保证报告模式不留空目录，
    /// 抽进来之后由这一位再保证一次。
    ReadOnly,
}

/// 共有的第 5–7 步（宿主事实 / 启动提示 / 落盘监听）**在哪儿做完**。
///
/// 第 1–4 步（落盘 → 设置+时钟+`Runtime` → 密钥 → 用量）两个入口逐字同序，账本那时候刚建好、
/// 还没有任何宿主特有的东西起来。**第 5–7 步则要看那一位"本机有什么"的定义者有没有就位**：
///
/// - 无屏档：档位、PipeWire、systemd 单元这三样在装配期就已经是定值（`platform::host_facts`
///   里逐位写死了），所以三步都在 [`Core::assemble`] 里做完。
/// - 桌面档：五位宿主事实的定义者分别在悬浮窗 / 头显 / 事件桥 / 热键 / 托盘那几步里才起来，
///   启动提示要读托盘 `install()` 刚写下的"看得见吗"，而落盘由 `events::wire` 的**复合监听器**
///   一起做（同一个监听器里既转发给前端又落盘）。早做就会漏掉它们，所以三步都推给入口。
///
/// 这一位就是把这个差异说出来的：**顺序不变量保住的是"每一步在正确的时刻做"，
/// 而"正确的时刻"两个入口不一样。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// 第 5–7 步在 [`Core::assemble`] 里做完（无屏档）。
    InAssemble,
    /// 第 5–7 步**都不做**，由入口在自己的宿主特有步骤跑完之后自己做（桌面档）。
    /// 入口自己做完的形状照旧：注入事实、发启动提示、落盘监听（桌面的那一条已经包含在
    /// 自己的复合监听器里，所以**不用**再调 `Persist::attach_to`——挂两个监听器会让
    /// 每条事件被分派两次）。
    AfterHostSteps,
}

/// 芯的那一半：两个入口**逐字同序**的那 7 步跑完之后的产物。
pub struct Core {
    pub runtime: Runtime,
    pub persist: Arc<Persist>,
    pub paths: Paths,
    pub clock: Arc<dyn Clock>,
}

impl Core {
    /// 步骤（与今天两个入口的顺序逐条对齐，理由随注释搬过来）：
    ///
    /// 1. `Persist`（`PersistMode` 决定 new/start）
    /// 2. `Settings` + `Clock` → `Runtime::new`
    /// 3. 密钥后端 → `runtime.set_secret_store`；`File` 后端且盘上真有密钥 → 一条
    ///    `Notice::warning`（明文兜底的代价必须让人知道）
    /// 4. `runtime.load_usage(...)` —— **必须在挂落盘监听之前**，免得刚读出来就又标脏写一遍
    /// 5. `runtime.set_host_facts((ports.facts)())` —— **必须在控制面开门之前**，
    ///    事实还没齐就开门，先连上来的客户端会拿到一份建立在默认事实上的清单
    /// 6. `(ports.startup_notes)()` → `Notice`（受 `Notes` 控制）
    /// 7. `persist.attach_to(&runtime)`
    ///
    /// 第 5–7 步受 [`Finish`] 控制：桌面档的宿主特有步骤（悬浮窗 / 头显 / 事件桥 / 热键 /
    /// 托盘）是那五位事实的定义者，早注入会漏掉它们，所以它传 [`Finish::AfterHostSteps`]，
    /// 自己在那几步之后补做——见 [`Finish`] 的注释。控制面**不归这一层**（桌面是
    /// `ControlPlane::install` + `reconcile`，无屏是 `control::start`），但两个入口都在
    /// 事实注入之后才开门，那条不变量因此在这两个入口各自的顺序代码里。
    pub fn assemble(
        paths: Paths,
        ports: &HostPorts,
        persist_mode: PersistMode,
        notes: Notes,
        finish: Finish,
    ) -> Result<Core, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 落盘层。读不出来就用默认值（配置坏了也不该让服务起不来——
        //    起不来连控制面都没有，用户就没法远程修它）。
        let persist = match persist_mode {
            PersistMode::Writing => Persist::start(paths.dir.clone()),
            PersistMode::ReadOnly => Arc::new(Persist::new(paths.dir.clone())),
        };

        // 2. 时钟 + 账本。改配置的写入口只有 `Runtime::update_settings` 一个。
        let settings = paths::load_settings(&paths.settings);
        tracing::info!(
            settings = %paths.settings.display(),
            config_dir = %paths.dir.display(),
            "配置已读"
        );
        let runtime = Runtime::new(settings, Arc::clone(&ports.clock));

        // 3. 密钥：后端由入口按平台挑好挂进账本；顺手把存着的密钥读进来。
        //    文件后端且盘上真的有密钥时补一条 warning——那是明文兜底的代价说明。
        let has_plaintext_keys = ports.secret.has_plaintext_keys();
        runtime.set_secret_store(ports.secret.build());
        if has_plaintext_keys {
            if let crate::secrets::SecretBackend::File { path } = &ports.secret {
                tracing::warn!(path = %path.display(), "API 密钥以明文（0600）存在这个文件里");
                runtime.notify(Notice::warning(format!(
                    "API 密钥以明文（0600）存在 {}：这一档没有 Secret Service，这一份就是兜底",
                    path.display()
                )));
            }
        }

        // 4. 用量账本。**要在挂落盘监听之前**灌进去。
        runtime.load_usage(paths::load_usage(&paths.usage()));

        if finish == Finish::InAssemble {
            // 5. 宿主事实。**只报关掉的位**，位由芯算（档位上限 − 关掉的）。
            //    必须在控制面开门之前注入。
            let facts = (ports.facts)();
            tracing::info!(
                tier = ?facts.host,
                off = ?facts.off.keys().map(|bit| bit.id()).collect::<Vec<_>>(),
                "宿主事实已注入（位由芯算）"
            );
            runtime.set_host_facts(facts);

            // 6. 启动提示：只是给人看的事实（PipeWire 在不在、托盘有没有宿主），不改任何位。
            //    探它要连一次 PipeWire，而提示的出口是 journal，所以报告模式跳过。
            if notes == Notes::Always {
                for note in (ports.startup_notes)() {
                    runtime.notify(Notice::warning(note));
                }
            }

            // 7. 落盘监听：设置/用量一变就标脏（真正的写盘在去抖线程里）。
            //    排在 `load_usage` 之后（否则刚读出来的那份会被当成"变了"再写一遍）。
            persist.attach_to(&runtime);
        }

        Ok(Core {
            runtime,
            persist,
            paths,
            clock: Arc::clone(&ports.clock),
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn persist(&self) -> &Arc<Persist> {
        &self.persist
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }
}

// 退出顺序**不进** `vox-host`（`docs/plans/S4-A-HOST-LAYER.md` §9 M1）：唯一的硬不变量
// 「控制面 < 工作线程 < 落盘」由两个入口各自的顺序代码 + 一行注释守住。闭包链不比顺序代码
// 清楚，而这一层唯一要保护的东西就是那个顺序。
