# S4-A · 共享宿主层 `vox-host` —— 施工设计稿

> 状态：**[已拍板·未开工]**。本稿把 `docs/plans/S4-EMBEDDED-REFACTOR.md` §3「S4-A 共享宿主层
> `vox-host`（行为不变）」细化成可直接施工的版本，并回答该稿 §6 第 1 问。
> 上位口径：`docs/architecture/DIRECTIONS.md` §10.9（嵌入式优先 + 共享宿主层）、
> `.omp/RULES.md`、`.omp/AGENTS.md`。
>
> **本稿的"现状"是 2026-09-30 在分支 `s4a/design`（HEAD = `095eacc`，与 `main` 同一提交）
> 逐文件读代码核到的**。行号只作参考并注明"2026-09-30 核"，符号名优先（`RULES.md` #11）。
>
> **本轮没有 Rust 工具链**（`cargo` 在本 worktree 的沙箱里不可调用），所以**没有跑过任何编译、
> 测试、clippy 或 `cargo tree`**。凡涉及"跑出来的数"的地方一律写"未跑"（`RULES.md` #10）。
> §6 里的基线数字是**用户提供的**（"2026-09-30 在 main `095eacc` 实跑"），不是我跑出来的。

---

## 0. 一句话

把两份装配层（`app/src-tauri` 与 `crates/vox-headless`）里**同义的那七件**收进新 crate
`crates/vox-host`，两个入口各自只剩"本机有哪些端口 + 本机能力位 + 入口参数 + 宿主特有那几步"；
**行为不变**，既有测试逐字未改。范围严格限定在 S4-A，不碰算子链、不碰 ALSA、不碰局域网控制面。

---

## 1. 现状证据

### 1.1 目录与规模（2026-09-30 核）

| 文件 | 行数 | 说明 |
| --- | --- | --- |
| `app/src-tauri/src/persist.rs` | 436 | 桌面落盘 |
| `app/src-tauri/src/mcp.rs` | 375 | 桌面控制面胶水 |
| `app/src-tauri/src/dsp.rs` | 81 | 桌面 dsp 适配器 |
| `app/src-tauri/src/sys/secrets.rs` | 445 | Windows DPAPI 密钥 |
| `app/src-tauri/src/platform/linux/secrets.rs` | 143 | Linux Secret Service 密钥 |
| `app/src-tauri/src/lib.rs` | 334 | 桌面 `assemble()`（14 步）/ `shutdown()`（7 组） |
| `crates/vox-headless/src/persist.rs` | 218 | 无屏落盘 |
| `crates/vox-headless/src/mcp.rs` | 268 | 无屏控制面胶水 |
| `crates/vox-headless/src/dsp.rs` | 84 | 无屏 dsp 适配器 |
| `crates/vox-headless/src/secrets.rs` | 318 | 无屏 0600 文件密钥 |
| `crates/vox-headless/src/config.rs` | 261 | 无屏配置目录三级回落 + 读设置/用量 |
| `crates/vox-headless/src/headless.rs` | 632 | 无屏 `Assembly::assemble`（7 步）/ `Daemon::start` / `Daemon::shutdown`（3 步） |
| `crates/vox-headless/src/status.rs` | 299 | 无屏状态出口（报告 + 日志） |
| `crates/vox-dsp/src/{denoise,resample}.rs` | 161 / 237 | 降噪 / 重采样实现本体 |

### 1.2 「同义重复」——两份装配层里**逐字或近乎逐字**各有一份的

| # | 事项 | 桌面侧 | 无屏侧 | 重复程度（2026-09-30 读代码核） |
| --- | --- | --- | --- | --- |
| D1 | **落盘**：`Persist`（去抖 800 ms + 分段睡 250 ms + 原子写 `.tmp`→`rename`） | `app/src-tauri/src/persist.rs::Persist` | `crates/vox-headless/src/persist.rs::Persist` | 结构体、五个方法、线程体、锁粒度**逐字同形**。桌面多 `new()`（只读构造）、`load_settings`/`load_usage`、`secret_path()`；无屏多 `settings_path()`。桌面 `atomic_write` 多一句 `create_dir_all(parent)`，无屏没有。 |
| D2 | **控制面胶水**：`Switch` / `start()` / `prune_dead_credentials()` / `process_alive()` | `app/src-tauri/src/mcp.rs::Switch`、`:start`、`:prune_dead_credentials`、`:process_alive` | `crates/vox-headless/src/mcp.rs::Switch`、`:start`、`:prune_dead_credentials`、`:process_alive` | `Switch`（含 `OFF` / `from_settings`）、`prune_dead_credentials`、`start` 三处**逐字相同**（含注释与日志文案）。`process_alive` 桌面多一段 `#[cfg(windows)]` 的 `OpenProcess` 实现。桌面多 `Status` / `ControlPlane`（热切换），无屏没有。 |
| D3 | **dsp 适配器**：`Denoise` / `Resample` 的 `impl` + 两个工厂 | `app/src-tauri/src/dsp.rs` | `crates/vox-headless/src/dsp.rs` | 两份**逐字同形**（连三个 `#[test]` 的名字都一样，只差局部变量名 `d`/`r` vs `denoise`/`resample`）。 |
| D4 | **密钥的"选哪一个后端"** | `app/src-tauri/src/platform/{win,linux/mod}.rs::secret_store(path) -> Arc<dyn SecretStore>` | `crates/vox-headless/src/headless.rs::Assembly::assemble` 第 3 步直接 `Arc::new(SecretFile::new(&paths.dir))` | 三个后端（DPAPI / Secret Service / 0600 文件）**机制完全不同**，但"按平台挑一个、挂进账本、顺手报一条"这个**决策**在两个入口各写了一遍。 |
| D5 | **配置目录回落** | `app/src-tauri/src/lib.rs::assemble` 第 0 步 `app.path().app_config_dir()?` | `crates/vox-headless/src/config.rs::Paths::resolve` / `dir_from` / `dir_from_env` | 目录**来源**不同（Tauri 给 vs 环境变量回落），但"目录里那几个文件名"逐字相同：`settings.json` / `usage.json` / `control.json`（桌面 `persist.rs` 与 `mcp.rs::STATE_FILE`，无屏 `config.rs` 的三个常量）。**密钥文件名不同**：桌面 `secret.bin`（`persist.rs::secret_path`），无屏 `secret.json`（`config.rs::SECRET_FILE`）——**这一条是行为差异，抽的时候必须原样保留**。 |
| D6 | **装配与退出顺序** | `app/src-tauri/src/lib.rs::assemble`（14 步，`lib.rs:156-295`）/ `shutdown`（`lib.rs:313-334`） | `crates/vox-headless/src/headless.rs::Assembly::assemble`（7 步，`:97-185`）/ `Daemon::start`（`:220-252`）/ `Daemon::shutdown`（`:283-290`） | 步骤 1–5（落盘 → 设置+时钟+`Runtime` → 密钥 → 用量 → 宿主事实）**语义与顺序完全一致**；退出顺序的不变量也一致：**控制面 → 工作线程 → 落盘**（桌面多出托盘/热键/设备/悬浮/虚拟麦/OSC 那几步，插在同三个锚点之间）。 |
| D7 | **事件出口** | `app/src-tauri/src/events.rs::wire`（`handle.emit("vox://event", event.clone())`，`events.rs:65`） | `crates/vox-headless/src/status.rs::wire` / `log_event`（`status.rs:70-109`） | 出口不同（前端通道 vs 结构化日志），但**形态相同**：`runtime.add_listener(Listener)` + 一个 `Event → ()` 的分派函数。 |
| D8 | **状态出口（报告）** | `app/src-tauri/src/composition.rs::document` | `crates/vox-headless/src/status.rs::composition_json` | 两处**逐字同形**（5 行）：`let ledger: &dyn Ledger = runtime; vox_mcp::endpoints::document(ledger, &mut \|endpoint, error\| tracing::warn!(...))`。 |
| D9 | **设备目录扫描** | `app/src-tauri/src/devices.rs::scan` | `crates/vox-headless/src/headless.rs::scan_devices` | 两个 8 行函数**逐字同形**（任一项失败给空列表）。 |
| D10 | **`Clock`（Linux 那一份）** | `app/src-tauri/src/platform/linux/clock.rs::LocalClock` | `crates/vox-headless/src/sys/clock.rs::LocalClock` | 除模块头注释外**逐字相同**（`Instant` 单调 + `chrono::Local` 日期），两个 `#[test]` 也逐字相同。 |

### 1.3 「真不同」——只属于 Tauri 或只属于无屏，**不进 `vox-host`**

| # | 事项 | 归属 | 证据 |
| --- | --- | --- | --- |
| X1 | Tauri `Builder` / 4 个插件 / 26 条 `#[tauri::command]` / 关窗→托盘 | 桌面 | `app/src-tauri/src/lib.rs:68-149`；`commands.rs`（26 个命令体，其中 5 个 OSC、6 个 VB-CABLE、3 个 catalog、4 个 `open_*` 链接、1 个 `quit_app` 是真业务） |
| X2 | 窗口最小尺寸 `enforce_min_size(&WebviewWindow)` | 桌面 | `platform/linux/mod.rs:210`（Linux 是空实现）、`platform/win.rs:95`（接 `tauri::WebviewWindow`） |
| X3 | 托盘（`TrayIconBuilder` / `CheckMenuItem<tauri::Wry>`）与"关窗能不能收进托盘" | 桌面 | `app/src-tauri/src/tray.rs:16-38`；`platform/{mod,win,linux/mod}.rs::tray_host_available` |
| X4 | 悬浮字幕窗（`platform::spawn_overlay` → `OverlayHandle`）+ 字幕帧线程 | 桌面 | `platform/{win,linux/mod}.rs::spawn_overlay`（返回 `crate::state::OverlayHandle`）、`app/src-tauri/src/overlay.rs` |
| X5 | 全局热键（`start_hotkeys` / `stop_hotkeys`） | 桌面 | `platform/{win,linux/mod}.rs::start_hotkeys`；Linux 依赖 `vox-input-linux`、Windows 依赖 `app/src-tauri/src/input.rs` |
| X6 | SteamVR 头显字幕 | 桌面（Windows + feature） | `app/src-tauri/src/vr_overlay.rs`（整个模块 `#[cfg(all(windows, feature = "steamvr-overlay"))]`） |
| X7 | 开机自启同步（`sync_autostart` / `tauri_plugin_autostart`） | 桌面 | `app/src-tauri/src/events.rs:280-314` |
| X8 | VRChat OSC 客户端与"字幕逐字推进 ChatBox" | 桌面 | `app/src-tauri/src/events.rs:129-199`、`state.rs::osc`（`vox_osc`） |
| X9 | `VirtualDeviceStatus`（VB-CABLE 安装管理状态） | 桌面 | `platform/mod.rs:221` 定义，`platform/{win,linux/mod}.rs::virtual_device_status` 实现，唯一消费者 `app/src-tauri/src/dto.rs:198-205` → `DeviceSnapshotDto.virtual_cable_status` / `_16ch_status`，前端类型见 `app/ui/src/types.snapshot.ts:60,66` |
| X10 | `catalog_updater`（拉 GitHub raw / 校验 / 落盘覆盖版） | 桌面 | `app/src-tauri/src/catalog_updater.rs`（132 行，唯一 Tauri 依赖是调用方给的 `config_dir`）；三个命令在 `commands.rs:592-651` |
| X11 | 设备轮询线程（4 s tick + 顺手 `refresh_host_facts`） | 桌面 | `app/src-tauri/src/devices.rs::start` / `stop`（`:31-83`）；无屏**刻意没有**（`headless.rs:129-132` 注释："无屏档只扫一次"） |
| X12 | 隐藏 CLI `--print-composition`（排在 Tauri 之前、只 `Builder::build()` 不 `run()`） | 桌面 | `app/src-tauri/src/composition.rs::requested` / `print_and_exit`；`lib.rs:64-66` |
| X13 | 隐藏 CLI `--vox-restore-defaults`（`platform::pre_main`） | 桌面（Windows 有意义） | `platform/win.rs:25` → `vox_audio_win::restore_via_args_if_requested()` |
| X14 | `platform::pre_main` 的 GDK_BACKEND 切 X11 | 桌面 | `platform/linux/mod.rs:39-50` |
| X15 | 启动期致命提示（`MessageBoxW`） | 桌面（Windows） | `sys/fatal.rs::alert`；Linux 版是 `eprintln!`（`platform/linux/mod.rs:62`） |
| X16 | `SystemClock`（`GetLocalTime`） | 桌面（Windows） | `sys/clock.rs:11-49`；无屏侧没有对应物 |
| X17 | 无屏 CLI（`--config` / `--print-capabilities` / `--print-composition` / `--dry-run` / `--start` / `--run-for`）与 `Probe`（报告三模式不碰 PipeWire、不建目录） | 无屏 | `crates/vox-headless/src/cli.rs`、`headless.rs::Probe` |
| X18 | 无屏自建 tokio runtime（2 个工作线程） | 无屏 | `headless.rs:228-232`；桌面复用 `tauri::async_runtime::handle()`（`lib.rs:189`） |
| X19 | `LinuxHeadless` / 非 Linux 的 `other.rs`（明说做不到） | 无屏 | `crates/vox-headless/src/platform/{linux,other}.rs` |
| X20 | 虚拟麦接线（`virtual_mic_ensure` / `virtual_mic_shutdown`） | 两边都有但**语义不同** | 桌面 Linux 真建 PipeWire 节点、桌面 Windows 只读探测 VB-CABLE（`platform/linux/virtual_mic.rs` vs `platform/win.rs:193`）；无屏不做（位在 `host_ceiling(LinuxHeadless)` 之外，`crates/vox-headless/src/platform/linux.rs:62-69`） |

### 1.4 孤儿规则论证的核实（S4 概要稿 §1-第 7 条）

**结论：论证成立。** 三条证据（2026-09-30 核）：

1. `crates/vox-dsp/Cargo.toml:9` 已有 `vox-core.workspace = true` —— `vox-dsp` **已经**依赖 `vox-core`，
   不需要新增任何依赖行，`Cargo.lock` 也不动。
2. trait 的定义处：`vox_core::ports::Denoise`（`crates/vox-core/src/ports.rs:132`）与
   `vox_core::ports::Resample`（同文件 `:143`）。两个 trait 都是 `pub`。
3. 反向依赖不存在：`crates/vox-core/Cargo.toml` 的 `[dependencies]` 里**没有** `vox-dsp`
   （只有 serde / serde_json / base64 / tracing / parking_lot / 可选 schemars），所以不存在环。

于是 `impl vox_core::ports::Denoise for vox_dsp::Denoiser` 满足孤儿规则（**本 crate 的本地类型 +
外部 trait**），两份 `dsp.rs` 的 newtype 可以直接删，零 shim。

> 反过来说，两份 `dsp.rs` 头注释里那句"两个都不是**本** crate 的，孤儿规则不让我们直接 impl"
> **在本仓库是错的**（S4 概要稿 §1-7 已指出）。按 `RULES.md` #3，抽走之后要顺手把这句话改掉
> ——但那是新文件里的注释，不改旧文件。

---

## 2. 边界：概要稿 §6 第 1 问的三条

### 2.1 `catalog_updater` —— **不进 `vox-host`**

理由（三条，按权重）：

1. **它没有第二个消费者。** 三个命令 `read_catalog_override` / `check_catalog_update` /
   `apply_catalog_update` 只在 `app/src-tauri/src/commands.rs:592-651`，无屏档今天**没有任何
   触发点**（`docs/platform/EMBEDDED.md` §3.5-① 已经写明这个缺口）。把它抽进共享层 =
   为了一个不存在的第二份去抽。
2. **它会让 `vox-host` 背上 `reqwest` + TLS。** `catalog_updater.rs:47-48` 自己建
   `reqwest::Client`，唯一一个 async 的地方就是 `fetch`。`vox-host` 若要它，就得同时接受
   `reqwest`（进而 `rustls`/`ring`）进每一条宿主二进制，包括将来的 MCU 档 —— 与 §0「先搭骨架」
   的最小依赖相悖。而它自己**没有任何非 Tauri 依赖**（`config_dir` 由调用方给），所以留在
   `app/src-tauri` 一点代价都没有。
3. **它的正确归属是控制面，不是宿主层。** `docs/platform/EMBEDDED.md` §3.5-① 给的修法是
   "把 `apply_update` 提进控制面" —— 那是 S4-C「局域网控制面」那一张工单的事
   （S4 概要稿 §3 的 S4-C 表里已列），在 S4-A 做属于抢跑。

**留在哪：** `app/src-tauri/src/catalog_updater.rs` 原样不动。
**S4-C 的接口预留：** `apply_update(config_dir, provider) -> Result<(String, String), String>`
已经是纯函数形状，届时直接挂到 `vox-mcp` 的动作清单上即可，不需要在 S4-A 先动它。

### 2.2 设备轮询 —— **线程不进 `vox-host`，扫描函数进**

拆成两半看：

- **`scan(registry) -> DeviceSnapshot`（两个 8 行的同形函数）→ 进 `vox-host`。**
  它是 `&dyn DeviceRegistry` 上的纯函数，零平台依赖，两边都要（桌面在轮询线程与
  `refresh_devices` 命令里用，无屏在 `Assembly::assemble` 第 5 步用一次）。抽走消掉 D9。
- **4 秒轮询线程（`devices.rs::start` / `stop`）→ 留在桌面。**
  三条理由：① 它的存在理由是"有人要观察"——桌面有界面要秒级反映插拔；无屏
  **刻意不装**（`headless.rs:129-132` 的注释写得很清楚："无屏档只扫一次……没有界面要秒级反映"），
  把它塞进共享层等于给无屏档加一个不启动的线程（`.omp/AGENTS.md` §3.7）。② 它每个 tick 调
  `crate::platform::host_facts()`（`devices.rs:93`），而 `host_facts()` 住在**入口**的
  `platform/` 里 —— 要抽线程就得连 `platform/` 一起抽，那是 S4-C 的大刀。③
  `refresh_host_facts` 的"只在真的变了才注入"判据（`devices.rs:92-100`）直接依赖
  `Runtime::host_facts() == facts`，那是入口级的事实报告。

**留在哪：** `app/src-tauri/src/devices.rs` 只改一行（`crate::devices::scan` → `vox_host::scan_devices`）。

### 2.3 `VirtualDeviceStatus` —— **不进 `vox-host`，S4-A 也不删**

- **不进 `vox-host`**：它的字段（`installed` / `install_pending_reboot` / `uninstall_incomplete` /
  `not_installed` / `not_applicable`）是**纯 Windows 安装器的 UI 词汇**，无屏档没有"安装"这一步
  （`platform/linux/mod.rs:212-221` 直接返回 `not_applicable`）。共享层放它等于给无屏档
  塞一个恒定值。
- **S4-A 不删**：`docs/architecture/DIRECTIONS.md` §10.7 的 `shell-dev` 行确实把"删
  `VirtualDeviceStatus`"记成"仍未落地"，但删它要同时改三个地方：`platform/mod.rs`（定义）、
  `platform/{win,linux/mod}.rs`（两份实现）、`app/src-tauri/src/dto.rs:198-205` + 它的两个
  `#[cfg(test)]` 构造点（`dto.rs:253-255`），以及前端类型
  `app/ui/src/types.snapshot.ts:58-66`（`virtual_cable_status` / `virtual_cable_16ch_status`）。
  这是**跨 crate（Rust→TS）的接口变更**，与"S4-A 行为不变"冲突，且会把 `app/ui` 拖进一批
  core/shell 工单的文件集（RULES #8 一个文件一个 owner）。
  → **单独开一张工单**（owner: shell-dev，排在 S4-B 之后），不在 S4-A 计数里。

**留在哪：** `app/src-tauri/src/platform/**` + `dto.rs`，S4-A 全程不动。

---

## 3. 目标形状

### 3.1 crate 依赖约束（硬）

```
vox-host 依赖：vox-core, vox-mcp, vox-dsp, serde, serde_json, tracing, parking_lot,
              chrono（只要 clock+std）
              目标平台门控：windows（DPAPI + process_alive）
              目标平台 + feature 门控：keyring（Secret Service）
vox-host 禁止：tauri / tauri-plugin-* / gtk / webkit / wry / tao / openvr
vox-host 允许：tokio —— **本版不允许**（理由见 §3.6）
```

**为什么不允许 tokio：** 共享层要碰的每一件都不需要它。控制面走 `vox-mcp::transport::http`，
而 `crates/vox-mcp/Cargo.toml:25-27` 自己就写明"传输面**不要 tokio**……`std::net::TcpListener`
+ 每连接一个线程就够了"；`catalog_updater`（唯一 async 的一方）按 §2.1 不进共享层；流水线
引擎的 `TransportFactory` 由**入口**建好后注入（桌面复用 Tauri 的 runtime，无屏自建，
`headless.rs:228-232`），而 runtime 句柄的**保活**必须留在入口（无屏的 `Daemon._net` 字段
就是干这个的，`headless.rs:210-216`）。→ tokio 留在两个入口的依赖表里，`vox-host` 不碰。

### 3.2 模块清单

```
crates/vox-host/
├── Cargo.toml
├── src/
│   ├── lib.rs          模块地图 + 公共 API 汇总（照 crates/vox-headless/src/lib.rs 的写法）
│   ├── paths.rs        配置目录回落（Paths / dir_from / dir_from_env）+ 读设置/用量
│   ├── persist.rs      Persist：只读构造 / 写盘构造 + 去抖线程 + 原子写
│   ├── clock.rs        LocalClock（chrono），供两个 Linux 入口共用
│   ├── secrets/
│   │   ├── mod.rs      SecretBackend 枚举 + secret_store() 选择函数 + stored_keys 判据
│   │   ├── file.rs     FileStore：0600 + 环境变量覆盖（今天 crates/vox-headless/src/secrets.rs）
│   │   ├── dpapi.rs    #[cfg(windows)] DpapiSecretStore（今天 app/src-tauri/src/sys/secrets.rs）
│   │   └── service.rs  #[cfg(all(target_os="linux", feature="secret-service"))]
│   │                  SecretServiceStore（今天 app/src-tauri/src/platform/linux/secrets.rs）
│   ├── control.rs      Switch / Status / ControlPlane / start / prune_dead_credentials
│   │                  / process_alive
│   ├── events.rs       trait EventSink + LogSink（今天 crates/vox-headless/src/status.rs 的日志那半）
│   ├── report.rs       capabilities_json / composition_json（两端逐字同形的两个出口）
│   ├── core.rs         Core::assemble（共用装配步骤 1–7）、scan_devices
│   └── entry.rs        HostPorts：宿主入口注入的那份东西
└── tests/
    └── host.rs        跨模块的集成用例（落盘 / 控制面握手文件 / 报告形状）
```

### 3.3 公共 API（写到能照着编码的程度）

```rust
// ── lib.rs ────────────────────────────────────────────────────────────────
pub mod clock;
pub mod control;
pub mod core;
pub mod entry;
pub mod events;
pub mod paths;
pub mod persist;
pub mod report;
pub mod secrets;

pub use control::{ControlPlane, Status, Switch, CONTROL_FILE};
pub use core::{scan_devices, Core};
pub use entry::HostPorts;
pub use events::{EventSink, LogSink};
pub use paths::{dir_from, dir_from_env, Paths, SETTINGS_FILE, USAGE_FILE};
pub use persist::Persist;
```

```rust
// ── paths.rs ──────────────────────────────────────────────────────────────
/// 配置目录里的文件名。**两个外壳逐字相同**（今天分别在
/// `app/src-tauri/src/persist.rs`、`app/src-tauri/src/mcp.rs::STATE_FILE` 与
/// `crates/vox-headless/src/config.rs`）。
pub const SETTINGS_FILE: &str = "settings.json";
pub const USAGE_FILE:   &str = "usage.json";
pub const CONTROL_FILE: &str = "control.json";

/// 配置目录的环境变量（无屏回落的第一优先）。桌面不用这条（由 Tauri 给）。
pub const ENV_CONFIG_DIR: &str = "VOX_CONFIG_DIR";
pub const APP_DIR: &str = "vox";

/// 纯函数：三级回落的后三级（`--config` 由调用方优先，它只看父目录）。
/// **从 `crates/vox-headless/src/config.rs::dir_from` 逐字搬来**。
pub fn dir_from(
    vox: Option<&Path>,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf>;

/// 从当前进程的环境取配置目录。**逐字搬来**。
pub fn dir_from_env() -> Option<PathBuf>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths { pub dir: PathBuf, pub settings: PathBuf }

impl Paths {
    /// 桌面档用：Tauri 给了目录，直接定下路径。**纯路径运算，不碰盘**。
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self;

    /// 无屏档 `--config <settings.json>`：给的是**文件**，取它的父目录当配置目录。
    /// 校验（是目录 / 没有文件名 / 没有父目录 → 报错）逐字搬自
    /// `crates/vox-headless/src/config.rs::Paths::resolve` 的 `Some(path)` 分支。
    pub fn from_settings_file(path: &Path) -> Result<Self, String>;

    /// 无屏档缺省：三级回落。**逐字搬自 `Paths::resolve` 的 `None` 分支**。
    pub fn from_env() -> Result<Self, String>;

    /// 只给会写盘的那条路调。**逐字搬来**。
    pub fn ensure_dir(&self);

    pub fn settings(&self) -> &Path { &self.settings }
    pub fn usage(&self) -> PathBuf { self.dir.join(USAGE_FILE) }
    pub fn control(&self) -> PathBuf { self.dir.join(CONTROL_FILE) }
    /// 密钥文件**不进这里**：桌面是 `secret.bin`、无屏是 `secret.json`，由
    /// `SecretBackend::File { path }` 各自显式给（见 §3.5）。
}

/// 读设置。读不出来 / 坏了 → 默认值。**从 `crates/vox-headless/src/config.rs::load_settings`
/// 逐字搬来**；桌面 `Persist::load_settings` 改成调它（行为一致，见 §4.3）。
pub fn load_settings(path: &Path) -> Settings;

/// 读用量账本。同上，**逐字搬自 `config.rs::load_usage`**。
pub fn load_usage(path: &Path) -> UsageLedger;
```

```rust
// ── persist.rs ────────────────────────────────────────────────────────────
/// 去抖线程的唤醒间隔 / 分段睡的粒度。**两个值与今天两份实现逐字相同**（800 ms / 250 ms）。
const FLUSH_INTERVAL: Duration = Duration::from_millis(800);
const SLEEP_SLICE: Duration = Duration::from_millis(250);

pub struct Persist { /* dir, dirty: Mutex<Dirty>, stop: AtomicBool, flusher: Mutex<Option<JoinHandle>> */ }

impl Persist {
    /// **只读构造**：不起线程、**不建目录**。给 `--print-composition` 与单测用。
    /// 逐字搬自 `app/src-tauri/src/persist.rs::Persist::new`。
    pub fn new(dir: PathBuf) -> Self;

    /// **写盘构造**：`create_dir_all(dir)`（幂等、失败不致命）+ 起去抖线程 + 返回 `Arc`。
    /// 逐字搬自 `app/src-tauri/src/persist.rs::Persist::start`。
    pub fn start(dir: PathBuf) -> Arc<Self>;

    pub fn settings_path(&self) -> PathBuf { self.dir.join(SETTINGS_FILE) }
    pub fn usage_path(&self) -> PathBuf { self.dir.join(USAGE_FILE) }
    pub fn dir(&self) -> &Path { &self.dir }

    pub fn load_settings(&self) -> Settings { paths::load_settings(&self.settings_path()) }
    pub fn load_usage(&self) -> UsageLedger { paths::load_usage(&self.usage_path()) }

    pub fn save_settings(&self, settings: &Settings);
    pub fn save_usage(&self, usage: &UsageLedger);

    /// 落盘监听：设置/用量一变就标脏（真写盘在去抖线程里）。
    /// 今天是两个入口各写 6 行（`crates/vox-headless/src/headless.rs:165-170`）；
    /// 桌面**不走这条**（它的 `events::wire` 一个监听器里既转发又落盘，见 §4.3）。
    pub fn attach_to(self: &Arc<Self>, runtime: &Runtime);

    pub fn flush(&self);
}
```

```rust
// ── events.rs ─────────────────────────────────────────────────────────────
/// 事件出口。**只有一个方法**：把一条芯事件送到"外面去"。
/// Tauri 前端通道 / 结构化日志 / 将来的 HTTP 订阅都只是它的实现。
///
/// 为什么是 `&Event` 而不是 `Event`：Tauri 的 `Emitter::emit` 要 owned（`events.rs:65`
/// 今天 `event.clone()`），实现自己 clone；签名收 `&Event` 让"只读"出口（`LogSink`）
/// 零分配。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &vox_core::event::Event);
}

/// 结构化日志出口。**从 `crates/vox-headless/src/status.rs::{log_event, log_notice,
/// track_name}` 逐字搬来**（含"字幕正文不进日志"那条纪律）。
pub struct LogSink;
impl EventSink for LogSink { /* 同 log_event */ }
```

```rust
// ── report.rs ─────────────────────────────────────────────────────────────
/// `CapabilityReport` 的 JSON。**与今天的
/// `crates/vox-headless/src/status.rs::capabilities_json` 逐字相同**。
pub fn capabilities_json(runtime: &Runtime) -> Result<String, serde_json::Error>;

/// 两份清单 + 有效位。**与今天的
/// `crates/vox-headless/src/status.rs::composition_json` 与
/// `app/src-tauri/src/composition.rs::document` 逐字相同**（三者本来就同形）。
pub fn composition_json(runtime: &Runtime) -> Result<String, serde_json::Error>;
```

```rust
// ── core.rs ───────────────────────────────────────────────────────────────
/// 同步扫一遍设备目录。**今天两份 8 行的同形函数合并成这一份**。
pub fn scan_devices(registry: &dyn vox_core::ports::DeviceRegistry) -> vox_core::runtime::DeviceSnapshot;

/// 装配时要不要顺手报"启动提示"（PipeWire 在不在、托盘有没有宿主）。
/// 桌面**总是**报（`lib.rs:260-262`）；无屏**只常驻模式**报
/// （`headless.rs:157-161` 的 `probe == Probe::PipeWire`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notes { Always, Never }

/// 落盘层取哪个构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistMode {
    /// `Persist::start`：建目录 + 起去抖线程。常驻模式。
    Writing,
    /// `Persist::new`：不建目录、不起线程。报告三模式（`--print-capabilities` /
    /// `--print-composition` / `--dry-run`）——今天无屏档靠
    /// `run()` 里那句 `if !args.mode.is_read_only() { paths.ensure_dir() }`
    /// （`headless.rs:321-323`）保证报告模式不留空目录，抽进来之后由这一位表达。
    ReadOnly,
}

/// 芯的那一半：两个入口**逐字同序**的那 7 步。
pub struct Core {
    pub runtime: Runtime,
    pub persist: Arc<Persist>,
    pub paths: Paths,
    pub clock: Arc<dyn Clock>,
}

impl Core {
    /// 步骤（与今天两个入口的顺序逐条对齐，理由随注释搬过来）：
    /// 1. `Persist`（`PersistMode` 决定 new/start）
    /// 2. `Settings` + `Clock` → `Runtime::new`
    /// 3. 密钥后端 → `runtime.set_secret_store`；`File` 后端且盘上真有密钥 → 一条
    ///    `Notice::warning`（今天 `crates/vox-headless/src/headless.rs:116-124`）
    /// 4. `runtime.load_usage(...)` —— **必须在挂落盘监听之前**
    /// 5. `runtime.set_host_facts(ports.facts())` —— **必须在控制面开门之前**
    /// 6. `ports.startup_notes()` → `Notice`（受 `Notes` 控制）
    /// 7. `persist.attach_to(&runtime)`（无屏走这条；桌面由自己的复合监听器代替）
    pub fn assemble(
        paths: Paths,
        ports: &HostPorts,
        persist_mode: PersistMode,
        notes: Notes,
    ) -> Result<Core, Box<dyn std::error::Error + Send + Sync>>;

    pub fn runtime(&self) -> &Runtime { &self.runtime }
    pub fn persist(&self) -> &Arc<Persist> { &self.persist }
    pub fn paths(&self) -> &Paths { &self.paths }
}

// 退出顺序不进 vox-host（§9 M1）：唯一的硬不变量「控制面 < 工作线程 < 落盘」
// 由两个入口的顺序代码 + 注释守住。
```

```rust
// ── entry.rs ──────────────────────────────────────────────────────────────
/// 宿主入口注入进来的那一份。**只放"本机有什么"，不放"这个进程怎么活"**。
///
/// 用 `fn()` 而不是值：`host_facts()` 在桌面每 4 秒被 `devices.rs::refresh_host_facts`
/// 读一次（`devices.rs:93`），函数指针让那份"每 tick 重算"的语义保持原样
/// （今天也是调同一个函数指针语义的 `platform::host_facts()`）。
#[derive(Clone)]
pub struct HostPorts {
    /// 声明自己是哪一档。桌面 = `platform::host_kind`；无屏 = `platform::host_kind`。
    pub kind: fn() -> vox_core::composition::HostKind,
    /// 报这台机器现在的事实（**只报"关掉的位"**，位由芯算）。
    pub facts: fn() -> vox_core::capability::HostFacts,
    /// 本机时钟。
    pub clock: Arc<dyn Clock>,
    /// 密钥后端。见 §3.5。
    pub secret: SecretBackend,
    /// 采集 / 播放 / 设备目录三个工厂。**由入口建好后按值交进来**
    /// （`CaptureFactory` 等是 `Box<dyn Fn…>`，不是 `Clone`）。
    pub capture: vox_core::pipeline::CaptureFactory,
    pub playback: vox_core::pipeline::PlaybackFactory,
    pub registry: Arc<dyn vox_core::ports::DeviceRegistry>,
    /// 传输工厂。**tokio `Handle` 由入口先备好再闭包捕获**（桌面复用 Tauri 的，
    /// 无屏自建 runtime），runtime 的保活留在入口。
    pub transport: vox_core::pipeline::TransportFactory,
    /// 启动期要告诉用户的话（进 `Notice` / journal，不弹窗）。
    pub startup_notes: fn() -> Vec<String>,
}

impl HostPorts {
    /// 降噪 / 重采样两个工厂**不进这里**：它们在 S4-A 之后由 `vox-dsp` 直接提供
    /// （`vox_dsp::ports::{denoise_factory, resample_factory}`），
    /// `Core::engine()` 用 `vox_dsp::ports::*` 填 `Deps`，入口不再有发言权
    /// —— 这正是 §1.2-D3 抽走之后剩下的唯一出处。
    pub fn deps(&self) -> vox_core::pipeline::Deps;
    /// `PipelineEngine::new` + `runtime.set_control` —— 桌面是 `lib.rs:190-200`、
    /// 无屏是 `headless.rs:242-243`，逐条同形。
    pub fn engine(&self, runtime: &Runtime) -> Arc<PipelineEngine>;
}
```

### 3.4 `EventSink` 的两个实现怎么落

| 实现 | 放哪 | 形状 |
| --- | --- | --- |
| `LogSink` | `crates/vox-host/src/events.rs` | 从 `crates/vox-headless/src/status.rs` 搬来。`Core::assemble` 不挂它 —— 无屏入口在 `run_daemon` 里**最早**挂（今天 `headless.rs:436`，"流水线一起来就可能发事件，晚挂就漏掉启动那几步"）。 |
| Tauri 前端通道 | `app/src-tauri/src/events.rs`（新写 6 行） | `struct FrontendSink(tauri::AppHandle);` + `impl EventSink for FrontendSink { fn emit(&self, event) { self.0.emit(EVENT_CHANNEL, event.clone()) } }`。**这是 `vox-host` 之外唯一新增的 trait 实现，编译期保证 `vox-host` 不碰 Tauri。** |

桌面那个复合监听器（`events.rs::wire` 里 `match event` 的 200 行：自启同步、字幕 restyle、托盘同步、
OSC ChatBox 推进、门状态缓存）**整体留在 `events.rs` 不动**，只在最前面把
`handle.emit(EVENT_CHANNEL, event.clone())` 换成 `sink.emit(event)` —— 见 §4.3 的行为等价论证。

### 3.5 平台特有的密钥后端怎么分流

```rust
// crates/vox-host/src/secrets/mod.rs
/// 密钥后端。**这是"选择"这一层，机制全在各自的实现里。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretBackend {
    /// Windows：DPAPI 加密落盘（`#[cfg(windows)]`；别的平台上选它会得到一个
    /// 明确报错的 `PortError`，不是静默降级）。
    Dpapi { path: PathBuf },
    /// 桌面 Linux：Secret Service（feature `secret-service`，默认**关**）。
    SecretService,
    /// 无 D-Bus / 无 keyring 的机器：配置目录下的 0600 明文文件。
    File { path: PathBuf },
}

impl SecretBackend {
    /// 造出 `Arc<dyn SecretStore>`。**不实现 `SecretStore`** —— 这一层没有状态。
    pub fn build(&self) -> Arc<dyn vox_core::ports::SecretStore>;

    /// "盘上真的存了明文密钥"这条提示的判据。**只有 `File` 后端可能为真**
    /// （DPAPI 是密文、Secret Service 不落盘），所以其余两个恒 `false`。
    /// 今天无屏在 `headless.rs:114` 判的是 `SecretFile::stored_keys()`。
    pub fn has_plaintext_keys(&self) -> bool;
}
```

分流的三条硬规矩：

1. **`Cargo.toml` 一次写到位**（W1 建 crate 时就写），之后任何工单都不再动它：
   ```toml
   [dependencies]
   vox-core.workspace = true
   vox-mcp = { path = "../vox-mcp" }
   vox-dsp.workspace = true
   serde.workspace = true
   serde_json.workspace = true
   tracing.workspace = true
   parking_lot.workspace = true
   chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }

   [features]
   # Secret Service（gnome-keyring / KWallet，走 D-Bus）。**默认关**：
   # 只有桌面 Linux 需要；无屏盒子常常没有 keyring 守护（EMBEDDED §3.6），
   # 把它留成开着的默认会让 `cargo tree -p vox-headless` 拖进 keyring/zbus。
   secret-service = ["dep:keyring"]
   default = []

   [target.'cfg(windows)'.dependencies]
   windows = { workspace = true, features = [
     "Win32_Foundation", "Win32_Security_Cryptography", "Win32_System_SystemInformation",
     # `control.rs::process_alive` 的 Windows 分支（OpenProcess/CloseHandle）。
     "Win32_System_Threading",
   ] }

   [target.'cfg(target_os = "linux")'.dependencies]
   keyring = { version = "4", optional = true }
   ```
2. **`app/src-tauri/Cargo.toml` 开 `features = ["secret-service"]`；`vox-headless` 不开。**
   → `cargo tree -p vox-headless` 里既没有 `tauri`/`gtk`/`webkit`，也没有 `keyring`/`zbus`。
3. **`vox-host` 自己不写 `#[cfg]` 之外的东西**：`secrets/mod.rs::build` 里
   `SecretBackend::Dpapi { .. }` 在非 Windows 上返回
   `Arc::new(UnavailableStore("DPAPI 只在 Windows 上可用"))`（一个把每次调用都报错的
   `SecretStore` 实现），**不静默换成别的后端**（`RULES.md` #4）。这条路径今天没人走
   （桌面只在 Windows 上选它），但它必须有确定的行为，不能是 `unimplemented!()`。

### 3.6 两个入口变薄之后长什么样

```rust
// app/src-tauri/src/lib.rs::assemble（示意，S4-A 之后的形状）
fn assemble(app: &tauri::AppHandle) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    // 入口参数：Tauri 给的目录 + 平台实现给的那一整套。
    let paths = vox_host::paths::Paths::from_dir(app.path().app_config_dir()?);
    let ports = HostPorts {
        kind: platform::host_kind,
        facts: platform::host_facts,
        clock: platform::clock(),
        secret: SecretBackend::Dpapi { path: paths.dir.join("secret.bin") },   // Windows
        //                                                                // Linux: SecretService
        capture: platform::capture_factory(),
        playback: platform::playback_factory(),
        registry: platform::registry(),
        transport: net::transport_factory(tauri::async_runtime::handle().inner().clone()),
        startup_notes: platform::startup_notes,
    };
    // 共享的 7 步。
    let core = vox_host::Core::assemble(paths, &ports, PersistMode::Writing, Notes::Always)?;
    // 下面全是 Tauri 特有的 7 步：窗口 → 引擎 → 悬浮 → 头显 → 设备轮询 → 事件桥 → 热键/托盘
    // → 虚拟麦接线 → 控制面 install+reconcile。**顺序与今天逐条相同。**
    // ...
}
```

```rust
// crates/vox-headless/src/headless.rs（示意）
let paths = match args.config.as_deref() {
    Some(p) => Paths::from_settings_file(p)?,
    None => Paths::from_env()?,
};
let read_only = args.mode.is_read_only();
let ports = HostPorts { kind: platform::host_kind, facts: platform::host_facts,
                        clock: sys::clock::local(), secret: SecretBackend::File { path: paths.secret_json() },
                        /* 三个音频工厂来自 platform::platform() */ /* transport 自建 tokio 后闭包捕获 */ };
let core = Core::assemble(paths, &ports,
                          if read_only { PersistMode::ReadOnly } else { PersistMode::Writing },
                          if read_only { Notes::Never } else { Notes::Always })?;
```

**注意 `Paths` 不再提供 `secret()`**（两个外壳文件名不同，见 §1.2-D5）：
无屏入口在自己那份 `config.rs` 里保留 `pub const SECRET_FILE: &str = "secret.json"`，
桌面在 `lib.rs` 里写 `paths.dir().join("secret.bin")`。

---

## 4. 改动清单

> 每个工单独占一份文件清单，**工单之间文件不重叠**（`RULES.md` #8）。
> 每个工单收口时 `cargo fmt --all --check` + `cargo test --workspace` + `cargo clippy --workspace --all-targets` 三门全绿。

### W1 · `vox-host` 骨架（纯新文件，零行为变化）

- **owner**：core-dev　**前置**：无　**可与 W2 并行**（文件集不相交）

| 动作 | 文件 |
| --- | --- |
| 修改 | `Cargo.toml`（`[workspace].members` 加 `crates/vox-host`；`[workspace.dependencies]` 加 `vox-host = { path = "crates/vox-host" }`） |
| 新增 | `crates/vox-host/Cargo.toml` |
| 新增 | `crates/vox-host/src/{lib,paths,persist,clock,secrets/mod,secrets/file,secrets/dpapi,secrets/service,control,events,report,core,entry}.rs` |
| 新增 | `crates/vox-host/tests/host.rs` |
| 修改 | `app/src-tauri/Cargo.toml`（加 `vox-host.workspace = true`、`features = ["secret-service"]`） |
| 修改 | `crates/vox-headless/Cargo.toml`（加 `vox-host.workspace = true`） |

**搬入但不改一个字节的既有 `#[cfg(test)]` 模块**（这是"逐字未改"的第一次落地）：

| 来源 | 目标 | 条数 |
| --- | --- | --- |
| `app/src-tauri/src/persist.rs` 的 `mod tests`（8 条） | `vox-host/src/persist.rs` | 8 |
| `crates/vox-headless/src/persist.rs` 的 `mod tests`（3 条） | 同上（与上 8 条并存） | 3 |
| `crates/vox-headless/src/config.rs` 的 `mod tests` 中与 `dir_from` / `Paths` 有关的 5 条 | `vox-host/src/paths.rs` | 5 |
| `crates/vox-headless/src/config.rs` 的 `a_broken_settings_file_falls_back_to_defaults` | `vox-host/src/paths.rs` | 1 |
| `crates/vox-headless/src/secrets.rs` 的 `mod tests`（5 条） | `vox-host/src/secrets/file.rs` | 5 |
| `app/src-tauri/src/sys/secrets.rs` 的 `mod tests`（8 条） | `vox-host/src/secrets/dpapi.rs` | 8 |
| `app/src-tauri/src/platform/linux/secrets.rs` 的 `mod tests`（2 条） | `vox-host/src/secrets/service.rs` | 2 |
| `crates/vox-headless/src/mcp.rs` 的 `mod tests`（4 条） | `vox-host/src/control.rs` | 4 |
| `crates/vox-headless/src/status.rs::subtitle_text_never_reaches_the_log` | `vox-host/src/events.rs` | 1 |
| `app/src-tauri/src/platform/linux/clock.rs` 的 `mod tests`（2 条）+ `crates/vox-headless/src/sys/clock.rs` 的（2 条） | `vox-host/src/clock.rs` | 2（两份合成一份，见 §6.3） |
| `crates/vox-headless/src/status.rs` 另 4 条（`the_status_exit_reports_the_headless_tier` / `the_composition_exit_prints_both_legs_and_the_bits` / `the_listen_leg_prints_with_an_empty_input_and_no_denoise` / `wiring_is_idempotent`） | **留在** `crates/vox-headless/src/status.rs`（它们断言的是无屏档的事实，搬走就没意义了） | — |

**本工单新增的用例**（会推高总数，见 §6.3）：
`a_file_backend_says_yes_when_the_key_is_on_disk`、`file_and_dpapi_backends_keep_their_own_filenames`。

**本工单不做**：不动任何入口的 `.rs` 文件；旧文件**全部保留**（搬迁在 W3–W5 做）。
→ 短暂存在两份实现是**有意**的：每个工单单独可绿，重复在 W3/W4/W5 消除。

### W2 · `Denoise` / `Resample` 的 `impl` 挪进 `vox-dsp`

- **owner**：core-dev　**前置**：无　**可与 W1 并行**

| 动作 | 文件 |
| --- | --- |
| 新增 | `crates/vox-dsp/src/ports.rs` |
| 修改 | `crates/vox-dsp/src/lib.rs`（`mod ports; pub use ports::{denoise_factory, resample_factory};`） |
| 删除 | `app/src-tauri/src/dsp.rs` |
| 删除 | `crates/vox-headless/src/dsp.rs` |
| 修改 | `app/src-tauri/src/lib.rs`（`:30` 删 `mod dsp;`；`:196-197` 两处工厂换 `vox_dsp::ports::*`） |
| 修改 | `app/src-tauri/src/platform/win.rs`（`:48`） |
| 修改 | `app/src-tauri/src/platform/linux/audio.rs`（`:19`） |
| 修改 | `crates/vox-headless/src/lib.rs`（`:34` 删 `mod dsp;`） |
| 修改 | `crates/vox-headless/src/headless.rs`（`:41` 的 `use crate::{dsp, ...}`、`:238-239`） |
| 修改 | `crates/vox-headless/src/platform/linux.rs`（`:34`） |

**`crates/vox-dsp/Cargo.toml` 不用改**（`vox-core` 已在，见 §1.4），`Cargo.lock` 零变化。
**被改公共接口的调用方全部清单**（`RULES.md` #5，`grep` 实跑，2026-09-30）：

| 符号 | 调用方 |
| --- | --- |
| `denoise_factory()` | `app/src-tauri/src/lib.rs:196`；`crates/vox-headless/src/headless.rs:238` |
| `resample_factory()` | `app/src-tauri/src/lib.rs:197`；`app/src-tauri/src/platform/win.rs:48`；`app/src-tauri/src/platform/linux/audio.rs:19`；`crates/vox-headless/src/headless.rs:239`；`crates/vox-headless/src/platform/linux.rs:34` |

**测试**：三份同形的 `#[test]`（`denoise_adapter_forwards` / `resample_adapter_changes_rate` /
`resample_same_rate_passes_through`）合成 `crates/vox-dsp/src/ports.rs` 的一份，函数体逐字取
`app/src-tauri/src/dsp.rs` 那一版（局部变量名 `d`/`r`）。**净减 3 条**。

### W3 · 配置目录 / 落盘 / 密钥后端

- **owner**：shell-dev（三个密钥机制是平台外壳；`LocalClock` 是 Linux 外壳件）
  **前置**：W1、W2

| 动作 | 文件 |
| --- | --- |
| 修改 | `crates/vox-host/src/paths.rs`、`persist.rs`、`secrets/**`、`clock.rs`（把 W1 里的纯新文件接上入口用得到的最终形状） |
| 删除 | `app/src-tauri/src/persist.rs` |
| 删除 | `app/src-tauri/src/sys/secrets.rs` |
| 删除 | `app/src-tauri/src/platform/linux/secrets.rs` |
| 删除 | `app/src-tauri/src/platform/linux/clock.rs` |
| 删除 | `crates/vox-headless/src/persist.rs` |
| 删除 | `crates/vox-headless/src/secrets.rs` |
| 删除 | `crates/vox-headless/src/sys/clock.rs` |
| 修改 | `app/src-tauri/src/sys/mod.rs`（删 `pub mod secrets;` 与 `#[cfg(windows)] pub mod clock;` 的 secrets 那条） |
| 修改 | `app/src-tauri/src/platform/mod.rs`（删 `pub use` 链上的 `VirtualDeviceStatus` 之外的引用准备；`secret_store` 改指向 `vox_host::secrets`） |
| 修改 | `app/src-tauri/src/platform/win.rs`（`secret_store` → `SecretBackend::Dpapi`；`clock()` 仍用 `crate::sys::clock::SystemClock`） |
| 修改 | `app/src-tauri/src/platform/linux/mod.rs`（`secret_store` → `SecretBackend::SecretService`；`clock()` → `vox_host::clock::LocalClock`） |
| 修改 | `app/src-tauri/src/lib.rs`（`:36` 删 `mod persist;`；`:157-172` 三步换 `Paths::from_dir` + `Core::assemble`；`:169` 的 `persist.secret_path()` → `paths.dir().join("secret.bin")`） |
| 修改 | `app/src-tauri/src/state.rs`（`:27` / `:51` 的 `Arc<crate::persist::Persist>` → `Arc<vox_host::Persist>`） |
| 修改 | `app/src-tauri/src/composition.rs`（`:62` `crate::persist::Persist::new` → `vox_host::Persist::new`） |
| 修改 | `crates/vox-headless/src/lib.rs`（`:36,37,40` 删 `mod persist; mod secrets;`、`pub use persist::Persist;`） |
| 修改 | `crates/vox-headless/src/sys/mod.rs`（删 `pub mod clock;`） |
| 修改 | `crates/vox-headless/src/config.rs`（`Paths` 搬走后只留 `SECRET_FILE` 常量 + `from_settings_file` 的 CLI 侧错误文案 + `mod tests` 里仍属于本文件的那几条；`dir_from` / `load_settings` / `load_usage` 删） |
| 修改 | `crates/vox-headless/src/headless.rs`（第 1、3、4 步换 `Core::assemble`；`sys::clock::local()` → `vox_host::clock::local_clock()`） |

**行为等价论证（三处必须显式核）**：

1. **报告模式不建目录。** 今天无屏靠 `headless.rs:321-323` 的
   `if !args.mode.is_read_only() { paths.ensure_dir(); }` 保证。抽进 `PersistMode::ReadOnly`
   之后，那句**保留不动**（它同时管 `control.json` / `secret.json` 的父目录），`PersistMode`
   只是额外保证 `Persist::new` 不会 `create_dir_all`。用例
   `crates/vox-headless/tests/headless_entry.rs` 里有断言"报告模式不留空目录"，必须仍绿。
2. **桌面 `Persist::load_settings` 的 warn 文案。** 今天桌面在文件存在但读不了时
   `tracing::warn!("读取 settings.json 失败，用默认值：{e}")`（`persist.rs:92`），无屏是
   `tracing::warn!(path = …, error = …, "读设置失败，用默认值")`（`config.rs:151`）。
   → **采用无屏那一版**（结构化、含路径），因为它**信息量严格更大**；这会让桌面那一条
   日志文案变化，是 S4-A 唯一一处**可观察的日志差异**，在 §7 风险里点名。
3. **密钥文件名。** 桌面 `secret.bin`、无屏 `secret.json`，由 `SecretBackend` 各带各的
   `path`，一个字节都不改（见 §3.3 的 `Paths` 注释）。

**测试净变化**：`-2`（两份 `LocalClock` 的 4 条合成 2 条）。其它都是搬家，条数不变。

### W4 · 控制面胶水 + 事件出口 + 报告出口

- **owner**：core-dev　**前置**：W3

| 动作 | 文件 |
| --- | --- |
| 修改 | `crates/vox-host/src/{control,events,report}.rs` |
| 删除 | `app/src-tauri/src/mcp.rs` |
| 删除 | `crates/vox-headless/src/mcp.rs` |
| 修改 | `app/src-tauri/src/lib.rs`（`:33` `pub mod mcp;` → 删；`:205` `mcp::ControlPlane::new` → `vox_host::ControlPlane::new`；`:289-292` 换路径） |
| 修改 | `app/src-tauri/src/state.rs`（`:43,52` → `vox_host::ControlPlane`） |
| 修改 | `app/src-tauri/src/dto.rs`（`:106,267,495` `crate::mcp::Status` → `vox_host::Status`） |
| 修改 | `app/src-tauri/src/events.rs`（新增 `FrontendSink` + `impl EventSink`；`:65` 的 `handle.emit(...)` → `sink.emit(event)`；**`mod tests` 一个字不动**） |
| 修改 | `app/src-tauri/src/composition.rs`（`:94-99` 的 `document` 变 1 行转发 `vox_host::report::composition_json`；**`mod tests` 一个字不动**，因为它调的是本地 `document(&runtime)`） |
| 修改 | `app/src-tauri/tests/mcp.rs`（**只改第 38 行的 import**，见下） |
| 修改 | `crates/vox-headless/src/lib.rs`（`:36` 删 `mod mcp;`） |
| 修改 | `crates/vox-headless/src/headless.rs`（`:35,39,41`、`:172`、`:260-263`） |
| 修改 | `crates/vox-headless/src/status.rs`（删 `log_event` / `log_notice` / `track_name` 与那条日志用例；`wire()` 变成挂 `vox_host::LogSink` 的 3 行；`capabilities_json` / `composition_json` 变成转发；**余下 4 条 `mod tests` 一个字不动**） |

**`app/src-tauri/tests/mcp.rs` 的改法（RULES #5 的关键）**：
今天第 38 行是 `use vox_lib::mcp::{self, Switch};`，7 条用例的函数体里有 6 处 `mcp::start`、
3 处 `mcp::STATE_FILE`。改成：

```rust
use vox_host::control as mcp;         // 唯一一行改动
use vox_host::control::Switch;
```

→ **函数体逐字不变**（`mcp::start` / `mcp::STATE_FILE` 都还对得上）。
`vox-host` 已经是 `vox` 的正常依赖，测试不需要额外的 `[dev-dependencies]`。
**不许写 `pub use vox_host::control;` 留在 `vox_lib` 里当 shim**（RULES #5）。

**桌面 `ControlPlane` 的热切换保留、无屏不装。** 无屏今天**没有** `SettingsChanged` → 起停的
监听器（`crates/vox-headless/src/mcp.rs:17-18` 明写"没做的：控制面的起停与端口没有热切换"）。
S4-A 保持这个差异：无屏 `run_daemon` 调 `vox_host::control::start(...)` 一次；
`ControlPlane::install()` **只由桌面调**。这是**行为不变**的要求，不是遗漏。
（无屏什么时候接热切换，交给 S4-C 的局域网控制面那张工单。）

**测试净变化**：0（`app/src-tauri/tests/mcp.rs` 7 条 + `crates/vox-headless/src/mcp.rs` 4 条
都活着，只是换了住址 / 换了 import）。

### W5 · 装配与退出顺序收进 `vox-host`，两个入口最终瘦身

- **owner**：core-dev　**前置**：W4

| 动作 | 文件 |
| --- | --- |
| 修改 | `crates/vox-host/src/{core,entry}.rs`（接上 W1 里的雏形） |
| 修改 | `app/src-tauri/src/lib.rs`（`assemble` 换成 `Core::assemble` + `ports.deps()` / `ports.engine()`；`shutdown` **保持顺序代码**，只把 `persist.flush()` 等调用换到 `vox_host` 类型上，见 §9 M1） |
| 修改 | `app/src-tauri/src/devices.rs`（`:103` `scan` → `vox_host::scan_devices`；`mod tests` 不动） |
| 修改 | `app/src-tauri/src/commands.rs`（`:199,279,288,379,467` 五处 `crate::devices::scan(registry.as_ref())` → `vox_host::scan_devices(...)`；`mod tests` 不动） |
| 修改 | `crates/vox-headless/src/headless.rs`（`Assembly::assemble` / `Daemon::start` / `Daemon::shutdown` 接 `Core` / `HostPorts`；`Daemon::shutdown` 保持顺序代码） |
| 修改 | `crates/vox-headless/src/platform/linux.rs`（`platform()` 改成返回三个工厂，被 `HostPorts` 收走；**`mod tests` 不动**） |

**两个入口的退出步骤清单（S4-A 后顺序与今天逐条相同，仍写成顺序代码）**：

```
桌面 lib.rs::shutdown
  1 tray::begin_shutdown
  2 state.control.shutdown()                    ← 控制面锚点
  3 platform::stop_hotkeys
  4 devices::stop
  5 overlay::stop
  6 [cfg(windows+steamvr)] vr_overlay::stop
  7 state.engine.shutdown()                     ← 工作线程锚点
  8 platform::virtual_mic_shutdown
  9 platform::shutdown_overlay
  10 drop(osc)
  11 persist.flush()                            ← 落盘锚点

无屏 Daemon::shutdown
  1 control.shutdown()                          ← 控制面锚点
  2 engine.shutdown()                           ← 工作线程锚点
  3 persist.flush()                             ← 落盘锚点
```

**测试净变化**：0。

### W6 · 文档回填（不写代码）

- **owner**：docs-scribe　**前置**：W5　**独占**：
  `docs/plans/S4-EMBEDDED-REFACTOR.md`（§6 第 1 问标"已答"、§3 的 S4-A 段标"已落地"）、
  `docs/platform/EMBEDDED.md`（§3.10 第 1–3 条标已做、状态头加一行）、
  `docs/architecture/DIRECTIONS.md` §10.7 的 `shell-dev` 行回填。

### 4.1 工单依赖图

```
W1 (vox-host 骨架)  ─┐
                     ├─→ W3 (配置·落盘·密钥) ──→ W4 (控制面·事件·报告) ──→ W5 (装配·退出) ──→ W6 (文档)
W2 (dsp 端口 impl)  ─┘
```

**W1 ∥ W2 是本稿里唯一能并行的两条**（文件集不相交，已逐条核过）。W3 之后全串行 ——
原因是 `app/src-tauri/src/lib.rs`、`crates/vox-headless/src/{lib,headless}.rs`、
`app/src-tauri/src/platform/{win,linux/mod}.rs` 这五个文件**每一个工单都要碰**，
而 `RULES.md` #8 不许两个代理同时改一个文件。想拆得更细就得先把入口拆开，那是 S4-C 的大刀。

### 4.2 与 S4 概要稿的三处偏离（§8 "新者胜" 适用，此处是新稿推翻旧稿）

| # | 概要稿 S4-A 的说法 | 本稿的做法 | 为什么 |
| --- | --- | --- | --- |
| P1 | "把 `platform/` 从 Tauri crate 里抽出来"（EMBEDDED §3.10 第 1 条） | **不抽**。`vox-host` 只收"端口工厂 + `host_kind`/`host_facts`"这些**值**，由入口注入 | `platform/mod.rs` 里有 `enforce_min_size(&WebviewWindow)`（`platform/linux/mod.rs:210`）、`spawn_overlay` 返回 `crate::state::OverlayHandle`、以及 `VirtualDeviceStatus`（X2/X4/X9）——抽它必然把 Tauri 类型带进共享层。抽 `platform/` 是 S4-C 的一刀，本稿只把它的**可共享的那一面**变成 `HostPorts` |
| P2 | "`Denoise`/`Resample` 的 impl 挪进 `vox-dsp`，删掉两份 `dsp.rs`" | **照做，且论证已核实成立**（§1.4） | — |
| P3 | "控制面胶水（`Switch`/`LedgerBackend`/握手文件清扫）" | 照做；额外带上 `Status` / `ControlPlane`（桌面独有的那半） | 那半留在 `app/src-tauri` 就等于 D2 只删一半，下次加第三档宿主还要再抄一遍 `Status` |

---

## 5. 被改公共接口的全部调用方（`RULES.md` #5）

> 下面每一条的调用方都是 2026-09-30 用 `grep` 在 `crates/` 与 `app/src-tauri/` 上实跑得到的。

| 被改的符号 | 新住址 | 全部调用方 |
| --- | --- | --- |
| `Persist`（`new` / `start` / `save_settings` / `save_usage` / `flush` / `load_settings` / `load_usage` / `secret_path`） | `vox_host::persist::Persist` | `app/src-tauri/src/lib.rs:157,160,163,169,172,333`；`app/src-tauri/src/state.rs:27,51`；`app/src-tauri/src/events.rs:78,109`；`app/src-tauri/src/composition.rs:62`；`crates/vox-headless/src/lib.rs:44`；`crates/vox-headless/src/headless.rs:39,100,101,127,165,288` |
| `Persist::secret_path()` | **删**（改成入口自己拼） | 唯一调用方 `app/src-tauri/src/lib.rs:169` |
| `Paths` / `dir_from` / `dir_from_env` / `load_settings(path)` / `load_usage(path)` | `vox_host::paths::*` | `crates/vox-headless/src/{cli,config,headless,secrets}.rs`（`config.rs` 自己的 7 条用例 + `headless.rs:101,118,127` + `secrets.rs:26` 的 `SECRET_FILE`） |
| `mcp::Switch` / `mcp::start` / `mcp::STATE_FILE` / `mcp::Status` / `mcp::ControlPlane` | `vox_host::control::*` | `app/src-tauri/src/lib.rs:205,292`；`app/src-tauri/src/state.rs:43,52`；`app/src-tauri/src/dto.rs:106,267,495`；`app/src-tauri/tests/mcp.rs:38,105,268,451,479,569,573,587,601,607`；`crates/vox-headless/src/headless.rs:172,261,262` |
| `dsp::denoise_factory` / `dsp::resample_factory` | `vox_dsp::ports::*` | 见 §W2 的表（6 处） |
| `platform::secret_store` | `vox_host::secrets::SecretBackend` | `app/src-tauri/src/platform/win.rs:33`；`app/src-tauri/src/platform/linux/mod.rs:56`；`app/src-tauri/src/lib.rs:169` |
| `platform::clock`（Linux 那份） | `vox_host::clock::LocalClock` | `app/src-tauri/src/platform/linux/mod.rs:52`；`crates/vox-headless/src/{headless,status,platform/linux}.rs` 的测试与 `sys::clock::local()` |
| `status::{log_event, log_notice, track_name}` | `vox_host::events::LogSink` | `crates/vox-headless/src/status.rs::{76,111,124,226,241,247}`（后四处是那条日志用例自己） |
| `status::composition_json` / `composition.rs::document` | `vox_host::report::composition_json` | `crates/vox-headless/src/status.rs:62,169,196,203`；`app/src-tauri/src/composition.rs:94,96,111,116` |
| `devices::scan` / `headless::scan_devices` | `vox_host::core::scan_devices` | `app/src-tauri/src/devices.rs:41,103`；`app/src-tauri/src/commands.rs:199,279,288,379,467`（**5 处**，全在 `crate::devices::scan(registry.as_ref())` 形状上；`commands.rs` 因此也要进 W5 的文件清单）；`crates/vox-headless/src/headless.rs:135,298` |
| `sys::secrets::DpapiSecretStore` | `vox_host::secrets::dpapi::DpapiSecretStore` | `app/src-tauri/src/platform/win.rs:34` |
| `platform::linux::secrets::SecretServiceStore` | `vox_host::secrets::service::SecretServiceStore` | `app/src-tauri/src/platform/linux/mod.rs:59` |
| `secrets::SecretFile` | `vox_host::secrets::file::FileStore` | `crates/vox-headless/src/headless.rs:40,113,115` |

**不动的**（S4-A 里明确不碰）：`Runtime` 全部方法、`Composition` / `HostFacts` / `CapabilityReport`、
`ports.rs` 的 9 个 trait、`vox-mcp` 的全部公共 API、`Event` 枚举与 serde 形态、
`app/ui/**`（前端一行不改）。

---

## 6. 验收标准

### 6.1 基线（用户提供的，2026-09-30 在 `main` `095eacc` 实跑）

| 门 | 基线 |
| --- | --- |
| `cargo fmt --all --check` | 通过（零输出） |
| `cargo test --workspace` | **601 passed / 0 failed / 5 ignored** |
| `cargo clippy --workspace --all-targets` | 通过（0 warning） |

> **这三行不是我跑出来的**（本 worktree 的沙箱里 `cargo` 不可调用）。

### 6.2 每个工单收口必跑（命令 + 期望）

```bash
# ① 格式
cargo fmt --all --check                 # 期望：零输出、退出码 0

# ② 全量测试
cargo test --workspace 2>&1 | tail -30  # 期望：0 failed；passed 数见 §6.3 的表；ignored 恒 5

# ③ lint
cargo clippy --workspace --all-targets  # 期望：0 warning

# ④ 共享层不碰界面（**S4-A 的核心断言**）
cargo tree -p vox-host | grep -ciE 'tauri|tao|wry|gtk|webkit|zbus'   # 期望：0
#   说明：zbus 是"托盘有没有宿主"的探针（platform/linux/mod.rs::tray_host_available），
#   它留在 app；vox-host 的 `secret-service` feature 拉进来的 `keyring` 会传递引入 zbus，
#   所以这一条要在**默认 feature**（secret-service 关）下跑。

# ⑤ 无屏档不拖界面与 keyring
cargo tree -p vox-headless | grep -cE '^(tauri|.*tao|.*wry|.*gtk|.*webkit|keyring|zbus)'  # 期望：0
# 更宽松的写法（对整棵树 grep，格式无关）：
cargo tree -p vox-headless | grep -ciE '\b(tauri|tao|wry|gtk|webkit2gtk|keyring|zbus)\b'  # 期望：0

# ⑥ 共享层真的不引 tokio
cargo tree -p vox-host | grep -c '^tokio'   # 期望：0

# ⑦ 芯仍然是干净的（回归：`.omp/AGENTS.md` 硬约束）
grep -rniE '\b(tauri|tokio|std::fs|PathBuf|std::env)\b' crates/vox-core/src/ | grep -v '^\S*:.*//'   # 期望：零命中

# ⑧ 两个入口真的薄了（S4-A 的目标形状）
ls app/src-tauri/src/persist.rs app/src-tauri/src/mcp.rs app/src-tauri/src/dsp.rs \
   crates/vox-headless/src/persist.rs crates/vox-headless/src/mcp.rs \
   crates/vox-headless/src/dsp.rs crates/vox-headless/src/secrets.rs 2>/dev/null   # 期望：零输出
grep -rn "orphan\|孤儿规则" --include=*.rs crates/vox-dsp/src/          # 期望：零命中（那句错误的注释已经不在了）
```

### 6.3 「既有测试逐字未改」怎么核

> **先说清一件事**：加了 `vox-host` 之后 `passed` 的**总数一定不是 601**。判据不是"总数不变"，
> 而是"**今天那 601 条一条不少、一条内容没动**"。所以分三类核。

**(A) 集成测试文件（`tests/` 目录）：逐字比对，一个字节都不许改。**

```bash
# 列出今天存在的集成测试文件，逐个与基线提交比
for f in crates/vox-mcp/tests/*.rs crates/vox-headless/tests/*.rs \
         app/src-tauri/tests/*.rs crates/vox-net/tests/*.rs \
         crates/vox-input-linux/tests/*.rs; do
  printf '%s: ' "$f"
  git diff --numstat 095eacc -- "$f" | awk '{print "+"$1" -"$2}' || true
done
```

| 文件 | 允许的差异 |
| --- | --- |
| `crates/vox-mcp/tests/*.rs`（6 个） | **零差异** |
| `crates/vox-net/tests/media_pipe.rs` | **零差异** |
| `crates/vox-input-linux/tests/evdev_end_to_end.rs` | **零差异** |
| `crates/vox-headless/tests/headless_entry.rs` | **零差异**（它只通过 `CARGO_BIN_EXE_vox-headless` 驱动二进制，是黑盒） |
| `app/src-tauri/tests/mcp.rs` | **只允许第 38 行 import 变化**（`use vox_lib::mcp::{self, Switch};` → `use vox_host::control as mcp;` + `use vox_host::control::Switch;`）。核法：`git diff 095eacc -- app/src-tauri/tests/mcp.rs` 必须只显示这一处 hunk；7 条 `#[test] fn` 的**函数体逐字相同** |

**(B) `#[cfg(test)] mod tests` 里被搬家的：按"函数体逐字"核，不按"文件路径"核。**

搬家必然改 `use` 行（`use super::*;` 背后的模块路径变了），所以定义如下：

> **「逐字未改」= 每个 `#[test] fn` 的签名行、函数体、断言、注释，逐字节相同；
> 允许变的只有 `#[cfg(test)] mod tests { … }` 块内的 `use` 声明行，
> 以及允许一处在测试模块顶部加 `use vox_host::paths::load_settings;` 这类"为搬家补的 import"。**

核法（逐个搬过去的模块）：

```bash
# 以 persist 为例：从基线提交里把桌面那份的测试块抠出来，跟新文件里的比
git show 095eacc:app/src-tauri/src/persist.rs \
  | awk '/^#\[cfg\(test\)\]/{f=1} f' > /tmp/old_persist_tests.rs
awk '/^#\[cfg\(test\)\]/{f=1} f' crates/vox-host/src/persist.rs > /tmp/new_persist_tests.rs
# 去掉 use 行之后比
diff <(grep -v '^\s*use ' /tmp/old_persist_tests.rs) \
     <(grep -v '^\s*use ' /tmp/new_persist_tests.rs)
# 期望：只报出"桌面独有的 8 条 + 无屏独有的 3 条合并"造成的增行，且**没有任何一条被改写**
```

同一套命令对下列 11 个来源块各跑一遍（清单见 §W1 的表）：
`persist`(两份)、`paths/dir_from`(6 条)、`paths/load_settings`(1 条)、
`secrets/file`(5 条)、`secrets/dpapi`(8 条)、`secrets/service`(2 条)、
`control`(4 条)、`events::LogSink`(1 条)、`clock`(两份合成 1 份，见下)。

**(C) 留在原地的 `#[cfg(test)]` 模块：一个字都不许动。**

这些文件只改了非测试代码，测试块必须 `git diff` 零输出：

| 文件 | 为什么能不动 |
| --- | --- |
| `app/src-tauri/src/events.rs` | `mod tests` 只测 `truncate_for_chat` / `subtitle_style_changed` / `autostart_status` / 事件 serde 形状，四样都不动 |
| `app/src-tauri/src/dto.rs` | 只把 `crate::mcp::Status` 换成 `vox_host::Status`（类型同一、字段名同一） |
| `app/src-tauri/src/devices.rs` | `mod tests` 只测 `refresh_host_facts`（留在本文件） |
| `app/src-tauri/src/composition.rs` | 测试调本地 `document(&runtime)`，而 `document` 变成 1 行转发 → 函数体不变 |
| `crates/vox-headless/src/status.rs` | 4 条用例调本地 `wire()` / `capabilities_json()` / `composition_json()`，三个都变成本地转发 → 函数体不变 |
| `crates/vox-headless/src/config.rs` | 留下的是 `SECRET_FILE` 与 CLI 侧错误文案；`mod tests` 里 `explicit_env_dir_wins` / `xdg_then_home_are_the_fallbacks` / `empty_env_values_are_unset` / `config_file_decides_the_directory` / `resolving_paths_does_not_create_the_directory` / `a_directory_is_rejected_with_a_hint` 搬去 `paths.rs`（函数体逐字），`a_broken_settings_file_falls_back_to_defaults` 也搬去 `paths.rs` |
| `crates/vox-headless/src/platform/linux.rs` | 4 条用例断言的是**无屏档的能力位事实**，`host_facts()` 一个字没改 |
| `crates/vox-headless/src/platform/other.rs` | 完全不动 |
| `crates/vox-core/**` / `crates/vox-mcp/**` / `crates/vox-overlay-*/**` / `crates/vox-audio-*/**` / `crates/vox-osc/**` / `crates/vox-net/**` | S4-A 完全不碰 |

**(D) 计数表（可逐条对账）。**

`passed` 的净变化只有三处，其余全是 1:1 搬家：

| 变化项 | Δ passed（Linux 上） |
| --- | --- |
| W1：`vox-host` 骨架里的既有测试全部 1:1 搬入 | ±0 |
| W1：新增用例 2 条（`a_file_backend_says_yes_when_the_key_is_on_disk` / `file_and_dpapi_backends_keep_their_own_filenames`；第二条 `#[cfg(windows)]`） | **+1**（Linux 上第二条不跑） |
| W2：两份 `dsp.rs` 各 3 条同形用例（共 6 条）合成 3 条 | **−3** |
| W3：两份 `LocalClock` 的 2+2 条合成 2 条 | **−2** |
| W3/W4/W5：其余全是搬家 | ±0 |
| **合计** | **−4** → 期望 `cargo test --workspace` 报 **597 passed / 0 failed / 5 ignored**（以 §9 M2 的测试名对账为准，总数只作旁证） |

> **`ignored` 恒为 5**。注意别把两个数混在一起：
> `grep -rn "#\[ignore" --include=*.rs crates app/src-tauri | wc -l` 在 2026-09-30 核到的是
> **25 处 `#[ignore…]` 属性**（含 Windows 专属文件里的），而 `cargo test --workspace`
> 报的 `5 ignored` 是**在 Linux 上真的被编译出来并标了 ignore 的那 5 条**。
> 两条都核：属性数应仍为 25（搬家时属性随函数体逐字走），ignored 数应仍为 5。

**(E) 手工冒烟（每轮收口各跑一次，两档各一遍）。**

```bash
# 桌面档（Linux 上跑）
cargo run -p vox -- --print-composition | jq -c '{tier: .capabilities.tier, speak_in: .speak.in[0].kind, ops: [.speak.ops[].kind]}'
# 期望：tier = "linux_desktop"，speak_in = "mic"，ops = ["mono","denoise","gate","resample"]

# 无屏档
cargo run -p vox-headless --bin vox-headless -- --print-composition | jq -c '{tier: .capabilities.tier, listen: .listen}'
# 期望：tier = "linux_headless"，listen = null（缺省没选目标程序），errors 长度 1

# 报告模式不留空目录（S4-A 特意保住的行为）
D=$(mktemp -d); cargo run -p vox-headless --bin vox-headless -- --config "$D/settings.json" --print-capabilities >/dev/null
[ ! -d "$D" ] && echo "OK: 报告模式没建目录" || echo "FAIL: $D 被建出来了"

# 控制面握手文件（两端同形）
cargo run -p vox-headless --bin vox-headless -- --config "$D/settings.json" --run-for 1 2>/dev/null
#   配 settings.json 打开 control 后：$D/control.json 出现，退出后消失
```

### 6.4 S4-A 整体验收（全部工单收口后）

| # | 判据 |
| --- | --- |
| A1 | §6.2 的 ①–⑧ 全过（命令在上面，期望已写） |
| A2 | §6.3 的 (A)(B)(C)(D) 全过，`cargo test --workspace` = **597 / 0 / 5**，且 §9 M2 的测试名对账通过 |
| A3 | `app/src-tauri/src/` 与 `crates/vox-headless/src/` 里**不再有** §1.2 表中的 D1–D10 任何一件的第二份实现（`grep` 逐条核，命令在 §6.2 ⑧） |
| A4 | 差分台（S0 那一套做法）**不需要**跑：S4-A 不改 `vox-core` 一行，`Plan` / `Composition` / `Worker` 全部未动，事件轨迹**按构造**逐字相同。仍然建议跑一遍 `cargo run -p vox-headless -- --dry-run` 与桌面 `--print-composition` 贴 JSON 对比（见 §6.3-E） |
| A5 | 两个 `--print-composition` 的输出**骨架逐字相同、取值按档位不同**（`tier` 一个 `linux_desktop` 一个 `linux_headless`；`speak.ops[].kind` 两边一致）——因为它们现在打的是**同一个** `vox_host::report::composition_json` |
| A6 | `git diff --stat 095eacc -- crates/vox-core/src crates/vox-mcp/src crates/vox-overlay-core/src crates/vox-osc/src app/ui` = **零输出**（S4-A 不许碰这五处） |

---

## 7. 风险与未决

### 7.1 风险

| # | 风险 | 影响 | 缓解 |
| --- | --- | --- | --- |
| R1 | **`Persist` 两份实现的差异比看起来多。** 桌面有 `new()`/`load_*`/`secret_path()`/`atomic_write` 里的 `create_dir_all`；无屏有 `SLEEP_SLICE` 常量名与 `settings_path()`。合并时漏一条 = 静默的行为变化（最典型：报告模式建出了空目录，桌面 `--print-composition` 侧也会） | 中 | §6.3-E 的"报告模式不留空目录"冒烟 + `persistence` 的 11 条 1:1 搬家用例 |
| R2 | **`ControlPlane::Inner::apply` 有一条靠"没有任何 `tools/call` 能改开关"维持的不变量**（`app/src-tauri/src/mcp.rs:113-118`）。搬到 `vox-host` 后如果两个入口的构造顺序变了，这条可能被破坏 | 高（会 join 自己 → 死锁） | `ControlPlane` 的构造与 `install()` 的调用点**都留在桌面 `lib.rs` 的原位**（第 13 步注入事实、第 14 步 `install`），本稿不改它们的位置；`reconcile` 的同步语义与 `Notice` 文案逐字搬 |
| R3 | **`EventSink` 换掉 `handle.emit` 之后，桌面那条复合监听器的顺序变了。** 今天每条事件是"先 emit 给前端、再落盘"；`FrontendSink` 抽出来后仍是同一个监听器的第一句，**顺序不变** | 低 | §W4 明确只替换那一行；`events.rs::mod tests` 不动作为回归 |
| R4 | **桌面 `load_settings` 的 warn 文案会变**（结构化字段取代了拼接字符串，见 §W3 行为等价论证第 2 条） | 低（可观察，但只是日志） | 已在 §7.2 列为"有意接受"，在 W3 的 PR 描述里点名 |
| R5 | **`cargo tree` 的输出格式随 cargo 版本变**，§6.2 的 ④⑤ 用的 grep 模式可能过松或过紧 | 中 | ④⑤ 各给了一宽一窄两个写法；两条都要跑，**两条都必须是 0** |
| R6 | **新增 crate 会让 `cargo test --workspace` 的总数从 601 变成 599**（§6.3-D 的 −4/+2）。老口径"必须是 601"会误报 | 中 | §6.3-D 给了逐项对账表；verifier 按表核，不要只盯总数 |

### 7.2 有意接受的行为差异（一处，且只有一处）

> **桌面 `Persist::load_settings` 在"文件存在但读不出来"时的那条 `tracing::warn` 从
> `"读取 settings.json 失败，用默认值：{e}"` 变成 `tracing::warn!(path=…, error=…, "读设置失败，用默认值")`。**
>
> 理由：两个入口共用一份实现，而无屏那一版**结构化、含路径、信息量严格更大**。
> 影响面：只影响一条 warning 的渲染形状，不影响任何控制流（两版都走默认值）。
> 已在 W3 的行为等价论证里点名；verifier 复核时按"接受"处理，不当缺陷。

### 7.3 未决 / `[未核实]`

| # | 事项 | 状态 |
| --- | --- | --- |
| U1 | ~~**`Shutdown` 用 `Vec<(&'static str, Box<dyn FnOnce()>)>` 会不会因为闭包捕获而让 `lib.rs::shutdown` 变难读**（今天 15 行顺序代码 vs 15 行 `step(...)` 链） | **未决**。本稿选了显式链（因为顺序不变量是这一层唯一要保护的东西），但没实测哪种在 review 里更好懂。verifier 复核时如果觉得链更难读，可以退回"顺序代码 + 一条钉住顺序的用例"~~ → **已拍板：不做 `Shutdown`，见 §9 M1** |
| U2 | **无屏档什么时候接控制面热切换** | **不在 S4-A**。无屏今天没有 `SettingsChanged` 监听器（`crates/vox-headless/src/mcp.rs:17-18`），S4-A 保持不变。留给 S4-C 的局域网控制面 |
| U3 | **`app/src-tauri/src/platform/` 抽 crate 的时机与切法** | **不在 S4-A**（§4.2-P1）。P1 里的 `enforce_min_size` / `OverlayHandle` / `VirtualDeviceStatus` 三个 Tauri 耦合点要先拆，其中 `VirtualDeviceStatus` 还要跨到 `app/ui` |
| U4 | **`SecretBackend::Dpapi` 在非 Windows 上的 `UnavailableStore` 会不会被误用**（比如将来的 Android 档照抄桌面代码） | **未核实**。本稿让它**每次调用都报 `PortError`**（不静默降级），但没有用例能证明"将来没人这么写"。S4-C 加 Android 入口时要看这一条 |
| U5 | **`chrono` 进 `vox-host` 是否值得** | **未决**。为 2 条 `LocalClock` 用例 + 1 个 `LocalClock` 类型引入 `chrono`（已在 `Cargo.lock`，不新增包），净收益是消掉 D10。判断权在 verifier：若认为不值得，W3 可以把 `clock.rs` 整条退回"留在两个入口"（`vox-headless/src/sys/clock.rs` 与 `app/src-tauri/src/platform/linux/clock.rs` 都不动），代价是 §6.3-D 的 Δ 变成 `0` 而非 `−2` |
| U6 | **`report.rs` 归 `vox-host` 是否越界**（它依赖 `vox-mcp`，让共享层有了"协议面"的依赖） | **未核实**。`vox-mcp` 只依赖 `vox-core`（`crates/vox-mcp/Cargo.toml:47`），所以没有环；而且 `control.rs` 本来就要它。若 verifier 认为"共享层不该知道协议面"，可把 `report.rs` 留在两个入口（代价：D8 的 5 行重复保留） |
| U7 | **本轮没有跑过任何编译/测试/clippy/cargo tree** | 事实，不是未决。本 worktree 的沙箱里 `cargo` 不可调用。§6 的所有期望值都是从**读代码**推出来的，第一次实跑可能暴露签名/feature 上的小出入（最可能的两处：`keyring` 的 optional + target 门控组合写法、`windows` feature 名 `Win32_System_Threading` 在 `vox-host` 里够不够用） |
| U8 | **ARM64 交叉编译** | S4-C 的 CI 交叉编译那张工单的事，S4-A 不涉及 |

---

## 8. 与既有决策的关系

- **不推翻 `DIRECTIONS.md` §8 的任何一行。** S4-A 是 §10.9 第 3 条（"共享宿主层 `vox-host`"）的
  施工化，不改 §8 的裁决。
- **§4.2 的 P1 是对 `EMBEDDED.md` §3.10 第 1 条的收窄**，不是推翻：那一条的**目标**
  （"无屏二进制不必拖进 Tauri / WebKitGTK / GTK"）今天**已经**由 `crates/vox-headless`
  这个独立 crate 达成了（`crates/vox-headless/Cargo.toml:8-16` 的注释 + §6.2 ⑤ 的检查），
  剩下的只是"桌面那份 `platform/` 还长在 Tauri crate 里"这一层组织问题。W6 里要按这个口径
  回填 `EMBEDDED.md` §3.10。
- **`DIRECTIONS.md` §10.7 `shell-dev` 行那条"仍未落地：删 `VirtualDeviceStatus`"** 在本稿里
  被明确**排除出 S4-A**（§2.3），要单独开工单。

---

## 9. Main 拍板（2026-09-30，审稿后追加；与上文冲突以本节为准）

- **M1 · 不做 `Shutdown` 类型（答 U1）。** 桌面退出 11 步里夹着托盘/热键/悬浮/头显/虚拟麦等宿主特有步骤，闭包链不比顺序代码清楚；
  唯一不变量「控制面 < 工作线程 < 落盘」在两个入口的 `shutdown` 里用顺序代码 + 一行注释守住。上文 §3.3 / W1 / W5 / §6.3-D 已同步改掉。
- **M2 · 测试对账按测试名集合，不按总数。** 施工前后各跑一次
  `cargo test --workspace -- --list 2>/dev/null | grep ': test$' | sed 's/: test$//' | awk -F'::' '{print $NF}' | sort`，
  基线存档在 Main 的 scratchpad（`baseline-tests.txt`）。要求：基线里每一个测试函数名在施工后仍存在（按函数名比，模块路径可变），
  唯一允许消失的是合并项（W2 的 dsp 三条、W3 的 clock 两条，都是"同名两份变一份"，按名字比不会消失）。新增名字只允许 §W1 列出的那两条。
- **M3 · U5（`chrono` 进 `vox-host`）、U6（`report.rs` 进 `vox-host`）按原稿执行。**
- **M4 · 施工环境。** 工作会话可以调用 `cargo build/test/clippy/fmt`；worktree 共用 `CARGO_TARGET_DIR=/home/w23x/vox-wt/target`。
  U7 里预判的两处（`keyring` optional + target 门控写法、`windows` feature 名）由 W1 实跑确认，不对就改 `Cargo.toml` 并在报告里写明。
- **M5 · W1 审查裁决（2026-09-30，W1 已合入）。**
  1. `vox-host` 里的 `config_file_decides_the_directory` 去掉了 `paths.secret()` 那一句断言（`Paths` 不再管密钥文件名）。**W3 必须让"无屏密钥文件是 `<dir>/secret.json`"这条断言继续活在 `crates/vox-headless`**（它的归属地），不许随旧文件一起消失。
  2. 类型名保留 `SecretFile`（不改 `FileStore`），W3 迁移时写 `vox_host::secrets::file::SecretFile`。
  3. `tests/host.rs` 不建（0 条用例的文件就是占位，RULES #4）。§3.2 模块图里那一行作废。
  4. W3 收尾时：`SecretFile::new(config_dir)` 若已无调用方就删（只留 `at_path`）；`SECRET_FILE` 只保留 `vox_host::secrets::file::SECRET_FILE` 一份定义，`vox-headless` 引用它，不留重复常量。
  5. `HostPorts` 不 derive `Clone`（字段是 `Box<dyn Fn>`）；时钟构造函数名是 `vox_host::clock::local_clock()`。
  6. W1 期间旧实现与 `vox-host` 并存，测试名是"翻倍"的：W1 合入后 main 为 641 passed / 0 failed / 6 ignored（多出的 ignored 是 `secret-service` feature 统一后 `vox` 测试二进制里编进了 `secret_service_round_trip`）。§6.3-D 的总数期望作废，**以测试名对账为准**：W3–W5 每一步都要求"main 上已有的测试名一个不少"。
  7. ~~§6.3-E 桌面 `speak.ops` 应为 5 节含 `level`~~ **（W3 实测更正）**：`speak.ops` 就是 `["mono","denoise","gate","resample"]` 4 节；`level` 是门的模式（`ops[2].config.kind == "level"`），不是独立一节。原稿 §6.3-E 的期望值是对的。
- **M6 · W3 审查裁决（2026-09-30，W3 已合入）。**
  1. `SecretFile::new(config_dir)` 保留：W1 逐字搬进 `vox-host` 的 5 条用例在用它，生产侧走 `at_path`。
  2. W3 在 `crates/vox-headless/src/headless.rs::Assembly::assemble` 临时加的 `persist_mode` 参数、第 7 步改成的 `persist.attach_to(&runtime)`，由 **W5** 在接 `Core::assemble` 时一并收掉。
  3. W3 已改过 W4 名下的 `crates/vox-headless/src/{mcp,status}.rs`（只动 clock / `STATE_FILE` 引用，删 `sys::clock` 逼的），W4 在其基础上继续。
  4. §6.3-E"报告模式不留空目录"的冒烟命令原稿是坏的（`mktemp -d` 已建出目录，判据恒假），应改为拿一个**尚不存在的子目录**：`D=$(mktemp -d)/cfg; … --config "$D/settings.json" --print-capabilities; [ ! -d "$D" ]`。
  5. 入口 crate 里失去用处的直接依赖已删：`app/src-tauri` 的 `keyring`、`chrono`，`vox-headless` 的 `chrono`（`zbus` 仍被 `platform::tray_host_available` 直接用，保留）。
