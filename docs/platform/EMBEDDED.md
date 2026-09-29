# 嵌入式（小主板 ARM64 Linux）—— 能做什么 / 做不到什么 / 怎么落地

> **来源：一轮桌面调研（2026-09-21 ～ 09-22）＋ 未实机验证。**
> 平台结论来自官方文档直抓（逐条带 URL）；仓库内事实来自逐文件核对（带 `path`，符号名优先）。
> **本仓库还没有无屏外壳**（没有 headless 二进制，`app/src-tauri` 仍把 Tauri Builder 当应用本体），
> 所以凡属"板上真会怎样"而没拿到一手实测的判断一律标 **[未核实]**（口径见 §7）。
> **状态注（第十九轮文档同步，2026-09-22）：上面这句已过期，保留原文不改口** —— 无屏外壳本体已落地
> （`crates/vox-headless/`：`src/cli.rs` 的 `--print-composition`、`systemd/{user,system}/vox-headless.service`
> 两份 unit、样例配置、`tools/package-headless.sh`；`docs/README.md` 的 platform 表已按"第十五轮进度"同步）。
> 本文 §3.3 的表已按**实际落地的两份 unit**（`crates/vox-headless/systemd/{user,system}/vox-headless.service`）
> **逐键拆成两块**：**只有「① 已落地」那张表是 unit 实文件里真有的键**，「② 设计建议」那一张
> （`Type=notify` / `WatchdogSec=` / `SIGHUP` 重载）**两份 unit 里都没有**——别把整张表读成已落地；
> 其余"[未核实]"口径（板上实测）不变。
> 连带过期的还有 §3.10：第 6 条（无屏二进制 + unit + 样例配置）与第 8 条的 tar.gz 打包已落地，
> 但第 8 条的 **`deb` 仍未加**（`app/src-tauri/tauri.conf.json` 的 `bundle.targets` 今天仍是 `["nsis"]`）——该节标题的"尚未执行"按此读。
> 口径与 `docs/architecture/DECISIONS.md` 一致：**代码与本文件打架时以代码为准**，然后回头把这里改对。
> 排期见 `docs/architecture/DIRECTIONS.md` §10.2 的 **S3 嵌入式（小主板）**；清单实例见
> `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.4（端点最简实例）与 §2.5.1（位表"无屏 ARM64"列）。
> **MCU 档已砍**，代价留档在本文件 §5。
>
> **状态注（2026-09-26，方向变更，原文不改口）**：嵌入式已改为**直接做、优先于手机**，并随 S4 重构推进
> （`docs/architecture/DIRECTIONS.md` §10.9，施工稿 `docs/plans/S4-EMBEDDED-REFACTOR.md`）。本文有三处按新方向读：
> ① §1/§3.2 "音频原样复用 PipeWire" → **ALSA 为嵌入式缺省、PipeWire 可选**；
> ② §1 "端点形态 = `net_in → net_out`" → **组合式**：插了什么硬件报什么位，网络中继与"本地麦 + 喇叭"盒子是同一份二进制；
> ③ §1/§3.5 控制面只在本机 → **无屏档可开局域网监听（配对码 + token）**。另：本文未涉及的**回声消除**见 S4 §2.2。

---

## 0. 一句话

**嵌入式第一站 = 小主板 ARM64 Linux**；它**不是新平台**，是把已经落地的 Linux 端搬到一个**没有屏幕**的盒子上：
同一个 `vox-core`、同一个 PipeWire、同一个 `crates/vox-audio-linux`。
真正新增的工程量集中在**「无屏 + 自启 + 交付」三件**，**不在音频**（`docs/architecture/DIRECTIONS.md` §10.1-3）。

做不到的能力不删，**位报 `false(reason)` + 说"这台设备做不到"**；无屏设备没有屏幕，
所以那句"做不到"落在**控制面**（S1 的 `describe_endpoint` 返回同一份位报告）、**启动期 `Notice`（进 journal）**与**快照**里，而不是界面上。

---

## 1. 能做什么

| 能力 | 小主板上的形态 | 依据 |
| --- | --- | --- |
| 音频三件套 | **原样复用** `crates/vox-audio-linux`：采集 / 播放 / 设备目录（经 `app/src-tauri/src/platform/linux/audio.rs` 的 `capture_factory()` / `playback_factory()` / `registry()` 注入） | 该 crate 已实现 `vox_core::ports` 的 `CaptureSource` / `PlaybackSink` / `DeviceRegistry` |
| 芯 | `vox-core` / `vox-dsp` / `vox-net` / `vox-osc` **100% 复用**：`crates/vox-core/src` 里**零文件 IO、零 Tauri、零 tokio**（`std::fs` / `PathBuf` / `std::env` 全无匹配） | 本轮 grep 核对 |
| 端点形态 | `in: net_in` → `out: net_out` = **网络进、网络出**（S3 目标，现状未实现）；挂了 USB 声卡时可换成 `in: mic` / `out: playback{role: speaker}`，位由"设备在不在"决定 | `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.4 |
| 进程 | `systemd --user` 常驻 + `loginctl enable-linger`（**没人登录也活着**）；开机自启（`background_service` 位 = unit 在不在） | 见 §3.3 |
| 控制面 | MCP / CLI / **配置文件**（S1）；无屏设备最有用的是"现在到底在不在跑" → health 出口 | §3.5 |
| 交付 | `deb`（Debian 系一条命令装好）+「二进制 + unit + 样例配置」tar.gz | §3.8 |
| 界面 | **没有**：`captions` / `tray` / `global_hotkey` 位均为假——字幕退化成日志 + 可选订阅 | §2 |

---

## 2. 硬件 / 系统做不到什么

> 每一条都写成"**位报 `false(reason)` + 说这台设备做不到**"，不是"这条腿不存在"。
> 位与 reason 的词汇表见 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.1 / §2.5.2。

| 位 | 小主板报什么 | 这台设备做不到什么（说法） |
| --- | --- | --- |
| `captions` | `false(unsupported)` | 无屏：没有地方显示字幕 → 退化成**日志/订阅出口**（`SubtitleDelta` 照发，不影响芯） |
| `virtual_mic` | `false(unsupported)` | 无屏设备不需要"给别的程序当麦克风"（`out` 里本来就没有 `virtual_mic`） |
| `global_hotkey` | `false(unsupported)` | 没键盘：`start_hotkeys` 直接不启动；芯里 `hotkeys` 本来就是 `Option`，不调也不影响 |
| `tray` | `false(unsupported)` | 没有 StatusNotifier 宿主：`TrayIconBuilder::build()` **照样返回 Ok** 但没东西画它（"假成功"，见 §4-5） |
| `program_tap` | `false(unsupported)` | 无屏档不本机抓程序，"听人说话"在这档 = **从网络收别人的声音**（`net_in`），腿仍在 |
| `mic` | `false(unsupported)`；挂 USB 声卡且枚举得到时 `true` **`[未核实]`** | 没接声卡就"没有麦克风可用" |
| `net_in` / `net_out` | `false(unsupported)`（实现前恒假，规则 R8） | **S3 目标**：实现前不许报 `true` |
| `file_config` | 实现前恒假 | 现在改 `settings.json` 不会热加载 → 文件"能读**不能当控制面**"；S3 要把它变成真控制面 |
| `background_service` | `true`（systemd `--user`）**`[未核实]`** | —（这档**能**无人值守常驻，是相对手机档的优势） |
| `vr_captions` | `false(unsupported)` | — |

---

## 3. 怎么落地

### 3.1 平台与工具链：ARM64 Linux = Rust Tier 1

- `aarch64-unknown-linux-gnu` 是 **Tier 1（with host tools）**，"64-bit **little endian** ARMv8-A Linux 4.1+（glibc 2.17+）"：
  <https://doc.rust-lang.org/rustc/platform-support/aarch64-unknown-linux-gnu.html>。
  大端 `aarch64_be-unknown-linux-gnu` 只到 **Tier 3**（<https://doc.rust-lang.org/rustc/platform-support.html>）。
- 交叉编译要**目标架构的 sysroot + linker**（`rustup target add` 只装标准库）：
  <https://rust-lang.github.io/rustup/cross-compilation.html>。
- **第一站建议在板上原生编译**（Tier 1 with host tools 支持），省掉一整套交叉 sysroot。
  真正贵的是**音频 crate 的交叉**：`crates/vox-audio-linux/Cargo.toml` 写明 `pipewire = "0.10"`（`pipewire-rs`）编译期要
  **`libpipewire-0.3-dev`（`.pc` + 头文件）与 clang**（`libspa-sys` 的 `build.rs` 走 bindgen），运行期只要 `libpipewire-0.3.so.0`。

### 3.2 音频：还是 PipeWire（外加两个"必须装"的东西）

- **daemon + 会话管理器都要在**：PipeWire daemon 只是"让设备与应用交换数据的框架"，
  **决定打开哪些设备、谁连谁的是会话管理器（WirePlumber / Media Session）**——没有它，声卡不会被打开。
  <https://docs.pipewire.org/page_daemon.html>、<https://docs.pipewire.org/page_session_manager.html>
- **实时优先级**：来自 `RLIMIT_RTPRIO`（`rt.prio` 默认 88）；没配好且 **D-Bus 可用**时才回退到 Portal Realtime / RTKit
  → 无桌面、无 D-Bus 会话的盒子上 RT 很可能拿不到。<https://docs.pipewire.org/page_module_rt.html>
- **建议**：USB 声卡（免驱 UAC）比板载 3.5 mm 稳；设备选择走既有 `registry()`，
  把 `app/src-tauri/src/devices.rs` 的 4 秒轮询**放宽到 30 s**（无 UI 时只把变化记日志）。
- 采样率纪律见 §4-8。

### 3.3 进程与生命周期：一个 systemd unit

字段照官方语义挑（<https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html>）。
下面**分两张表**：**① 已落地**＝`crates/vox-headless/systemd/{user,system}/vox-headless.service`
两份实文件里**真有的键**（逐键对得上）；**② 设计建议**＝本节当初挑字段时想过、但**两份 unit 还没写**的键。

**① 已落地**（键与取值照两份 unit 原文；两档取值不同的地方已注明）

| 字段（unit 原文） | 为什么这么选 |
| --- | --- |
| `Type=exec` | 长跑服务推荐：进程起不来会**真的报失败**（两档都取 `exec`） |
| `ExecStart=… --config <path> --start all` | 配置路径**显式写出来**（用户档 `%h/.local/bin/vox-headless --config %h/.config/vox/settings.json`；系统档 `/usr/local/bin/vox-headless --config /var/lib/vox/settings.json`）；`--start all` 两条腿都开，没配好的那条留在"没开" |
| `Restart=on-failure` + `RestartSec=3` + `RestartSteps=5` + `RestartMaxDelaySec=60` | 指数退避（**落地取 5 步、封顶 60 s**；当初的建议区间是 3–5 步）。**要 systemd ≥ 254**：更老的版本不认后两个键、会忽略它们并退回恒定 `RestartSec=3`（重启照旧，只是不退避） |
| `StateDirectory=` / `RuntimeDirectory=` | **状态目录**（属于服务自己的运行期/状态文件）—— 让 systemd 建目录 + 挂依赖，**别自己在 unit 里 `mkdir`**（<https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>）。**本档的配置不由它承载**：配置路径走 `ExecStart=` 的 `--config <path>`（两份 unit 见 `crates/vox-headless/systemd/{user,system}/vox-headless.service`），所以这两个目录取 `vox-headless`、**故意不与配置目录 `vox` 同名** |
| `LimitRTPRIO=95`（两档同值） | `RLIMIT_RTPRIO` 的**上限**（`systemd.exec` 的 `LimitRTPRIO=`）—— PipeWire 的 `module-rt` 靠它把音频线程提上去；拿不到不会起不来，只是退化成普通调度。**§3.4 表里那句"具体字段名 S3 落地时按该页确认"指的就是它**：两份 unit 已写 `LimitRTPRIO=95`（§3.4 原文按"先标注不改写"留待下轮） |
| `Environment=VOX_LOG=vox_headless=info,warn`（两档同值） | 无人值守：只打本 crate 的 info、其余 warn；要细节改这一个变量 |
| `NoNewPrivileges=true`（两档同值） | 这套服务不需要任何额外特权 |
| `User=` / `Group=`（**仅系统档**，都取 `vox`） | 系统档的专用服务用户；用户档不写（就是装它的那个用户） |
| `Environment=XDG_RUNTIME_DIR=/run/user/%U`（**仅系统档**） | 系统档要自己把 PipeWire 的 socket 目录（用户运行期目录）指过去；`%U` = `User=` 那个用户的 UID |

两份 unit 的 `[Service]` 段**只有上表这些键**；两档不同的 `After=` 与 `[Install]` 的 `WantedBy=` 见下面那段
（`Description=` / `Documentation=` 两档相同）。

**② 设计建议（两份 unit 里都没有，落地时再加）**

| 字段 | 为什么建议这么选 | 现状 |
| --- | --- | --- |
| `Type=notify` | 我们自己在"连接就绪"后发 `READY=1`，让依赖它的 unit 等我们**真的可用** | **未实现**：两份 unit 都是 `Type=exec`，代码里也没有发 `READY=1`（`sd_notify`）的地方 |
| `WatchdogSec=` + 定期 `WATCHDOG=1` | "卡死但没退出"也能被拉起来（超时判定失败并 `SIGABRT`）；配套的 `Restart=on-watchdog` 一并没写 | **未实现**：两份 unit 里没有 `WatchdogSec=` |
| `SIGHUP` 重载 | 无屏没有 UI，改配置后要能重载（`Settings` 已是纯数据 + `normalize()`，语义现成） | **未实现**：见 §3.5-① 第 2 条与 §3.10 第 4 条，尚无信号处理 |

**`--user` 还是 `--system`：音频优先 `--user`。** systemd 把每个 **system** 服务放进各自的 cpu cgroup，
而该 cgroup 的 RT 预算是 **0** → 申请实时优先级直接 `EPERM`；原话是
"**By default, user applications run in the root cgroup of the "cpu" hierarchy, which avoids these problems**"
（<https://systemd.io/MY_SERVICE_CANT_GET_REALTIME/>）。
选 `--system` 就得显式处理 cgroup（同一篇给了 `ControlGroup=` / `cpu.rt_runtime_us` 两条绕法）。

**没人登录也要活** → `loginctl enable-linger`（"a user manager is spawned for the user at boot and kept around after logouts"，
<https://www.freedesktop.org/software/systemd/man/latest/loginctl.html>）。

Unit 里 **`After=pipewire.service` 不要写**（PipeWire 是**用户**服务）；`After=` 这一行**两档分开写、别互抄**
（落地的两份 unit 见 `crates/vox-headless/systemd/{user,system}/vox-headless.service`）：

- **用户单元**：`After=default.target` —— systemd **用户**单元的标准写法（`[Install]` 的 `WantedBy=default.target` 同款）；
- **系统单元**：`After=network.target` —— **`default.target` 在系统管理器里不是一个"能等"的东西**，
  把用户单元那一行照抄过来等于写了个没用的依赖；系统档要等的是网络（两条腿的云端都在网上；
  `network.target` 只保证网络栈起好、不保证真连上，连不上由 `Restart=on-failure` 与起流时的 `Notice` 兜底）。

起来之后自己探测 PipeWire 可用性 —— 现成函数是 `vox_audio_linux::pipewire_available()`（`crates/vox-audio-linux/src/lib.rs`）。

**容器方案（可选）不建议做第一站**：容器里跑 PipeWire 客户端多一层 socket/权限问题，
放行声卡要 `--device` / `--group-add` / `-v` 三件套（<https://docs.docker.com/reference/cli/docker/container/run/>），
而且解决不了"PipeWire 会话归谁"。

### 3.4 权限（无屏盒子上"权限"= 设备访问 + 实时优先级 + 文件）

| 要什么 | 为什么 | 怎么给 |
| --- | --- | --- |
| 声卡访问（`/dev/snd`） | 我们的进程只是连 PipeWire socket，真正打开设备的是 daemon；用户会话下 logind 的 uaccess ACL 会给活跃会话开设备权限（`docs/platform/LINUX.md` §2.4 对 `/dev/input` 的实测结论；`/dev/snd` 同理 **`[未核实]`**） | 走 `systemctl --user` + `enable-linger`（§3.3）；容器方案另要 `--device` / `--group-add` |
| 实时优先级（`RLIMIT_RTPRIO`） | PipeWire 的 `module-rt` 靠它；拿不到就回退 Portal Realtime / RTKit（**要 D-Bus**），无桌面盒子上很可能两个都没有 | 首选 `--user`：**system** 服务在 cpu cgroup 里 RT 预算为 0，申请即 `EPERM`（§3.3）。必要时由 unit 抬高 `RLIMIT_RTPRIO`（`systemd.exec` 的执行环境字段，<https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>；**具体字段名 S3 落地时按该页确认**） |
| 全局热键（读 `/dev/input`，要 `input` 组） | 这档**不启动热键**（`global_hotkey` 位为假）→ **不需要给** | 真要给是 `usermod -aG input`（现成文案在 `platform/linux/mod.rs` 的热键错误里） |
| 密钥文件读写 | 无 D-Bus / 无 keyring 守护时的兜底路径 | **0600**，或 systemd `LoadCredential=` 传口令（§3.6） |
| 出站网络（上行 WebSocket） | 只是出站 TCP/TLS | 不需要提权 |

### 3.5 无屏三件：配置从哪进、状态往哪出、进程怎么活

**① 配置从哪进**

现状是 `app/src-tauri/src/persist.rs`：`settings.json` / `usage.json` 落在 `app_config_dir`
（`app/src-tauri/src/lib.rs` 的 `assemble()` 用 `app.path().app_config_dir()` 取，identifier `com.vox.app`），
**800 ms 去抖 + 原子写**（先写 `.tmp` 再 `rename`），密钥单独 `secret.bin`。无屏要补三件：

1. **目录可指定**：`VOX_CONFIG_DIR` → `$XDG_CONFIG_HOME/vox` → Tauri `app_config_dir` **三级回落**
   （同一个盒子上"服务跑在哪个用户下"会决定目录，而 systemd 服务常常不是桌面用户）。
2. **配置重载**：现在改配置**只有 UI 一条路** → 无屏要有 `SIGHUP` 重载或文件监听
   （`Settings` 已是纯数据 + `normalize()`，重载语义现成）。
3. **环境变量覆盖**：已有 `VOX_LOG`；再加 provider / 密钥路径等，让"一次性试跑"不必先写文件。

**模型目录的坑**：`crates/vox-core/build.rs` 把 `catalog/*.json` **编译期烘焙进二进制**；
运行期只有 `app_config_dir/catalog/*.json` 这一层覆盖，而覆盖是靠 Tauri 命令落盘的
（`app/src-tauri/src/commands.rs` 的 `read_catalog_override` / `check_catalog_update` / `apply_catalog_update`，
实现在 `app/src-tauri/src/catalog_updater.rs`）。
→ **无屏设备等于没有热更新模型目录的路径**：第一站要么接受"跟发版走"，
要么把 `catalog_updater::apply_update` 提进控制面（它是普通函数，唯一 Tauri 依赖就是那个 `config_dir`）。

**② 状态往哪出**（三档，按代价排）

1. **stderr → journald**：现有 `app/src-tauri/src/sys/log.rs` 已经**只写 stderr** 且 `with_ansi(false)`，
   systemd 直接收进 journal，**零改动就能用**。
2. **结构化日志**：`tracing-subscriber` 加 `.json()`，字段对齐 OTel Logs 数据模型
   （`Timestamp` / `SeverityNumber` / `Body` / `Attributes`；<https://opentelemetry.io/docs/specs/otel/logs/data-model/>）
   → 以后接采集器不用改代码。
3. **health / control endpoint**：无屏设备最有用的是"现在到底在不在跑"。
   第一站给一个**最小的 HTTP（或 unix socket）**：`GET /health`（进程活 + 两条流水线状态 + 最近一次错误）
   + `POST /session`（开关）。

**③ 进程怎么活** → §3.3。

### 3.6 密钥：无屏盒子上大概率没有 Secret Service

- 现状：`app/src-tauri/src/sys/secrets.rs`（Win DPAPI）+ `app/src-tauri/src/platform/linux/secrets.rs`
  （`keyring` → Secret Service，走 zbus）。
- 真实问题：Secret Service 需要 **D-Bus 会话总线 + gnome-keyring / KWallet 这类守护进程**。
  `systemctl --user` + linger 会有 `dbus-user-session`，但**盒子上不一定装了 keyring 守护**
  （Raspberry Pi OS Lite 就是纯命令行版）。
- 兜底（`docs/platform/SCOPE.md` §C8 已提过）：**文件 + 权限 0600**，或 `age` 加密 + 独立口令来自
  systemd `LoadCredential=` / `CREDENTIALS`（<https://systemd.io/> 有 `CREDENTIALS` 专页）。
- 第一站建议：`SecretStore` 接口不变，只是**多一个"无 D-Bus 就落文件"的实现**，
  并在启动时把这件事记成一条 `Notice`。

### 3.7 界面 / 热键 / 托盘：整块不启动

| 现在会启动的东西 | 无屏怎么办 |
| --- | --- |
| `app/src-tauri/src/overlay.rs` 的 `overlay::start()`（30 fps 帧线程 + 200 ms 空闲轮询） | **不启动**（无屏下 GTK 起不来） |
| `platform/linux/mod.rs::spawn_overlay`（GTK 透明窗 + XWayland） | **不启动** |
| `platform/linux/mod.rs::start_hotkeys`（evdev 读 `/dev/input`，要 `input` 组） | **不启动**；`Runtime::set_hotkey_host` 不调用即可（芯里是 `Option`） |
| `platform/linux/mod.rs::pre_main`（Wayland 会话强制切 X11 后端） | **跳过** |
| `app/src-tauri/src/tray.rs` | **不安装**；`can_hide_to_tray()` 那条"关窗收进托盘"的逻辑整个不适用 |

### 3.8 分发与更新

- `app/src-tauri/tauri.conf.json` 现在 `bundle.targets = ["nsis"]`（Windows 专用）、`resources` 带托盘图标。
  第一站加 **`deb`**；无屏产物单独出「tar.gz（二进制 + unit + 样例配置）」，**不塞托盘图标**。
- 更新器继续用 `tauri-plugin-updater`（本身跨平台）；无屏的"重启生效"交给 `Restart=always`。

### 3.9 无屏要拆的装配层假设（现状 → 目标）

**不需要拆的**：`crates/vox-core`（芯完全平台无关，含 `ports.rs` 的 9 个 trait）、`crates/vox-dsp`、`crates/vox-net`、`crates/vox-osc`、`crates/vox-audio-linux`。

| 文件 | 现在的假设 | 无屏要变成 |
| --- | --- | --- |
| `app/src-tauri/src/lib.rs` 的 `run()`（`tauri::Builder` 的单实例 / opener / autostart / updater **四个插件**、`invoke_handler!` 注册的 26 条命令、`RunEvent::WindowEvent{label=="main"}` 关窗→托盘） | "应用 = 一个 Tauri 进程 + 一个主窗口" | "应用 = Runtime + Persist + 一组端口"；Tauri 只是**其中一种外壳**；无屏入口是不注册任何窗口的二进制 / feature |
| `app/src-tauri/src/commands.rs`（**26 条 `#[tauri::command]` 是唯一控制面**，函数体本身就是业务胶水） | "界面是唯一控制面" | 把**函数体**下沉成平台无关 API（`snapshot` / `update_settings` / `start` / `stop` / `toggle` / `set_key` / `refresh_devices` / `catalog_*`…），Tauri 壳与 CLI/HTTP 壳都只是薄包装 |
| `app/src-tauri/src/events.rs`（唯一出口是前端通道 `vox://event`） | "事件出口 = 前端通道" | 抽 `trait EventSink`：Tauri emit（现有）+ **JSON-lines 到 stdout** + 可选本地 HTTP 订阅 |
| `app/src-tauri/src/platform/linux/mod.rs` | 平台函数与 Tauri 同 crate | 本轮核对：**只有 `enforce_min_size(&WebviewWindow)` 接 Tauri 类型**；`pre_main` / `clock` / `secret_store` / `alert` / `capture_factory` / `playback_factory` / `registry` / `start_hotkeys` / `spawn_overlay` 都不接（`tray_host_available` / `startup_notes` 走 zbus，也不接）。剩下的是装配层自己的类型（`crate::state::OverlayHandle`、`super::VirtualDeviceStatus`）→ 把 `platform/` 抽成独立 crate（或加 `headless` feature）是**最小的结构性改动**，收益是无屏二进制不必拖进 Tauri / WebKitGTK / GTK |
| `app/src-tauri/src/persist.rs` | 配置目录由 Tauri 给 | §3.5 的三级回落 + `SIGHUP` 重载 |
| `app/src-tauri/src/devices.rs`（4 s 轮询） | 界面要秒级反映插拔 | 保留但放宽（无 UI 时只记日志） |
| `app/src-tauri/src/sys/fatal.rs` | 启动失败用 `MessageBoxW` 弹框（Win 专属） | Linux 侧已是 stderr + 日志；无屏靠 systemd 失败状态 + journal |
| `app/src-tauri/src/sys/log.rs` | 只写 stderr，默认 filter `debug` | 无屏默认更安静 + 可选 JSON 输出（§3.5-②） |

### 3.10 建议动作（按性价比，**尚未执行**）

1. **把 `platform/` 从 Tauri crate 里抽出来**（最高性价比，见上表第 4 行）。
2. **控制面下沉**：26 条命令的函数体 → 平台无关 API（参数/返回沿用 `app/src-tauri/src/dto.rs`）；
   验收 = 同一组测试对"Tauri 薄壳"与"无壳 CLI"两个入口都通过。
3. **事件出口抽象**：`events.rs` → `trait EventSink` + 三个实现（Tauri / JSON-lines / HTTP 订阅）。
4. **配置入口去 Tauri 化** + `SIGHUP` 重载（§3.5-①）。
5. **`sys/log.rs` 加 JSON 开关**（字段对齐 OTel Logs）。
6. **新增无屏二进制 + systemd unit + 样例配置**（不建窗口、不建托盘、不启 overlay/热键）。
7. **密钥降级路径**（§3.6）。
8. **打包**：加 `deb`；无屏产物另出 tar.gz。
9. **在板上跑一次实测**：把 `docs/architecture/DIRECTIONS.md` §9.1 的基准（采集 3.03 ms/s、播放 1.73 ms/s）
   在 ARM64 上重跑，并确认 `rustfft` 那条路是否真走 NEON **[未核实]**。

---

## 4. 坑与限制（为什么 + 出处）

1. **无屏 + 系统服务拿不到实时优先级**。为什么：systemd 把 system 服务放进各自 cpu cgroup，RT 预算为 0；
   PipeWire 的 `module-rt` 又要 `RLIMIT_RTPRIO`。→ **用 `systemctl --user` + `enable-linger`**。
   （<https://systemd.io/MY_SERVICE_CANT_GET_REALTIME/>、<https://docs.pipewire.org/page_module_rt.html>）
2. **PipeWire 没有会话管理器 = 声卡不会被打开**。为什么：daemon 只是框架，决定"打开哪些设备、谁连谁"的是会话管理器。
   （<https://docs.pipewire.org/page_daemon.html>、<https://docs.pipewire.org/page_session_manager.html>）
3. **交叉编译音频 crate 比交叉编译芯贵**。为什么：`pipewire-rs` → `libspa-sys` 的 `build.rs` 用 bindgen（要 clang）
   读 `libpipewire-0.3` 头文件 → 交叉需要目标 sysroot；而 `aarch64-unknown-linux-gnu` 是 Tier 1 **with host tools**，
   板上原生编译更省事。（`crates/vox-audio-linux/Cargo.toml`；rustc platform-support 页）
4. **容器的声卡放行是手工活**：容器默认没有 `/dev/snd`、不在 `audio` 组、看不到宿主 `$XDG_RUNTIME_DIR` 里的 PipeWire socket。
   （<https://docs.docker.com/reference/cli/docker/container/run/>）
5. **托盘在无屏/无桌面是"假成功"**：`TrayIconBuilder::build()` 在没有 StatusNotifier 宿主的环境**照样返回 Ok**，
   图标对象建出来了但没有东西画它（`app/src-tauri/src/tray.rs` 头注释 + `tray_host_available()`）。
   → 无屏必须完全不依赖它，甚至不该编译进去。
6. **配置目录随"谁在跑"而变**：现在唯一来源是 `app_config_dir()`（identifier 决定路径），换成 systemd 服务用户后就指向别处。
7. **模型目录热更新在无屏上没有触发点**（§3.5-①），要么跟发版，要么把 `apply_update` 接进控制面。
8. **48 kHz 是全链路假设，换设备就会咬人**：降噪只在 48 kHz 有效
   （`crates/vox-dsp/src/denoise.rs` 的 `NATIVE_SAMPLE_RATE = 48_000`、`FRAME_SIZE = 480`；`vox-core` 的 `DENOISE_RATE = 48_000`）；
   **16 kHz 是上行协议**（`crates/vox-core/src/cloud/protocol.rs` 的 `INPUT_SAMPLE_RATE = 16_000`）；
   **24 kHz 是回放协议**（同文件 `OUTPUT_SAMPLE_RATE = 24_000`）——三处各自写死。
   USB 声卡只给 44.1 kHz 时**整条重采样链就上线了**，而 `crates/vox-dsp/src/resample.rs` 现在只有贵的 `SincFixedIn` 一档
   （`docs/architecture/DIRECTIONS.md` §3.3 路线五记着"重采样只有贵的 sinc 那一档"）。
9. **tokio 只在 `vox-net`，但它会按核数起线程**：多线程调度器默认"每个 CPU 核一个工作线程"
   （<https://docs.rs/tokio/latest/tokio/runtime/index.html>）。芯的流水线是 std 线程 + 5 ms 轮询，不吃 tokio；
   低功耗 1–2 核盒子上值得量一下内存（current-thread 调度器不起工作线程）。
10. **功耗/散热：Pi 5 不是问题，Pi Zero 级才是**。Pi 5 裸板典型工作电流 800 mA（≈4 W），官方推荐 27 W 电源；
    80–85 °C 起降频、85 °C 硬限。而整条 DSP 链实测只吃 **0.3% 单核**。
    （[电源](https://raw.githubusercontent.com/raspberrypi/documentation/master/documentation/asciidoc/computers/raspberry-pi/power-supplies.adoc)、
    [频率管理](https://raw.githubusercontent.com/raspberrypi/documentation/master/documentation/asciidoc/computers/raspberry-pi/frequency-management.adoc)）
    → **散热不是第一站的阻塞项**；"便宜的板子能不能扛住降噪 + 重采样"才是要实测的。

---

## 5. MCU 档：已砍，以及以后补的代价

**已砍**（`docs/architecture/DIRECTIONS.md` §10.0 第 2 条）：**清单不预留 `mcu` 字段**，
`docs/plans/S0-COMPOSITION-MANIFEST.md` 明确"不定义 mcu 变体、不定义任何 mcu 专属子字段"。
砍掉的代价 ≈ 0（不影响现状）；以后补要付的是下面这个价。

### 5.1 现有代码上不去 MCU（两条腿都踩 std）

| 现有件 | 为什么上不去 | 要换成什么 |
| --- | --- | --- |
| `crates/vox-dsp/src/denoise.rs`（`nnnoiseless::DenoiseState`，`Box<>` 分配，48 kHz / 480 帧） | **`nnnoiseless` 0.5.2 不是 `no_std`**：依赖 `easyfft` → `rustfft` / `realfft`，源码里直接用 `std::cell::RefCell` / `std::sync::Arc` | 厂商 AFE 的 NS（如 ESP-SR `nsnet2`），或自写定点 NS |
| `crates/vox-dsp/src/resample.rs`（`rubato::SincFixedIn`，128 点 sinc） | **`rubato` 0.16.2 默认 feature `fft_resampler`** → `realfft` + `num-complex`，全是 std 世界 | MCU 上重采样本来就"留服务器"（这条可以不换，直接删） |
| `crates/vox-core/src/pipeline/*`（std 线程 + `parking_lot` + 5 ms 轮询 + 20 ms 块） | MCU 上是 FreeRTOS / Zephyr 任务 + DMA 中断，没有 `std::thread` | 端侧另写"采集 → AFE → 上行"小循环；芯这份**不参与** |
| 采集（`CaptureSource`，Linux 侧 PipeWire） | MCU 没有 PipeWire | **I2S + DMA**（ESP-IDF `i2s_channel_read` / 回调直接摸 DMA 缓冲）或 **PDM/DMIC** |
| `app/src-tauri`（Tauri + WebView） | 整块不存在 | — |

→ 上 MCU 等于**换实现**，不是"复用 Rust"。Rust 复用率 ≈ 0。

### 5.2 代价区间与硬门槛

- **打通 demo**（一块开发板：麦克风 → AFE → 16 kHz 单声道 → WebSocket 上行 → 服务器出字幕）：**10–20 人日**，
  **几乎全是 C/FreeRTOS 侧新代码**（沿用 `docs/architecture/DIRECTIONS.md` §9.2 P5 的粗估）。
- **产品级**（可出厂的端侧盒子）：**30–50 人日**。差额来自麦克风阵列/回采通道选型与调参（AEC 要参考通道）、
  上行协议与断线重连、配网/设备发现、固件 OTA、PSRAM 版本模组的供应与成本。
- **内存/算力是硬门槛**：ESP-SR 官方 AFE 表 `MR, VC, HIGH_PERF` = 内部 RAM **91.1 KB** + **PSRAM 822.2 KB**，
  feed 已吃掉**单核 32.2%**（`LOW_COST` = 48.7 KB + 819.7 KB / 30.6%）
  （<https://docs.espressif.com/projects/esp-sr/en/latest/esp32s3/benchmark/README.html>）
  → **没有 PSRAM 的模组直接出局**；端侧 AFE 一栈给全 AEC / NS / BSS / VAD / AGC / WakeNet，
  输入只吃 **16-bit、16 kHz 交织**数据（<https://docs.espressif.com/projects/esp-sr/en/latest/esp32s3/audio_front_end/README.html>）。
  **注意：这些数字是厂商自算，非我方实测**（`DIRECTIONS.md` §9.1 第 2 条已标）。
- **厂商 SDK 锁定**：ESP-ADF **v3.0 与 v2.x 在 API 与行为上不兼容**（官方原文）
  （<https://docs.espressif.com/projects/esp-adf/en/latest/index.html>）→ 以后升级 = 迁移成本。
- **两条 MCU 生态路线**：C/FreeRTOS（ESP-IDF I2S：<https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-reference/peripherals/i2s.html>）
  或 Zephyr（Audio Codec / DMIC / I2S / DAI：<https://docs.zephyrproject.org/latest/hardware/peripherals/audio/index.html>）。
  "wasm 跑实时音频算子"那条要落到 WAMR（体积上没问题：cortex-m4f 上 fast interpreter ≈58.9 K text、
  aot ≈29.4 K、libc-wasi ≈21.4 K：<https://github.com/wasm-micro-runtime/wasm-micro-runtime>），
  但**至今没有我方实测**，别把 WAMR 当既定方案。
- **端侧只做上行会把服务端缺口放大**：`DIRECTIONS.md` §4 记着"听人说话在 GPT 上没有回合结束事件、没有断句检测"；
  端侧把 VAD 做完之后，**断句/回合结束的责任仍在服务器**。

---

## 6. 待验证项（**全部未实机验证**）

1. **小主板实测全部缺失**：本文所有"性能够用"的结论都是从 **x86 实测（0.3% 单核）＋ 架构 Tier 1** 推的，
   **板上没跑过**。第一站第一件事就是补这个实测。 **`[未核实]`**
2. **`loginctl enable-linger` 之后 PipeWire / WirePlumber 用户服务是否自动随 linger 启动**：
   linger 拉起的是 **user manager**，`pipewire.service` / `wireplumber.service` 是否被 enable 仍取决于发行版。 **`[未核实]`**
3. **ARM64 上 `rustfft` 是否真走 NEON 路径**（`nnnoiseless` 的 FFT 后端是 `easyfft` → `rustfft`）。 **`[未核实]`**
4. **无屏装配能否真的完全不依赖 Tauri 跑起来**（`platform/` 抽离是证明手段）。
5. **RK3588 具体板子的散热与电源数值**：只确认 Radxa 有官方文档站（<https://docs.radxa.com/en/rock5/rock5b>），
   没抓到 5B 的电源/温度原始数字。 **`[未核实]`**
6. **`pactl load-module module-null-sink` 在 PipeWire 的 Pulse 兼容层里是否原样可用**：
   官方文档给的造虚拟 sink 的路子是 PipeWire 的 `module-loopback`
   （<https://docs.pipewire.org/page_module_loopback.html>）＋ Pulse 兼容页只证明了"允许加载模块"。
   第一站不需要虚拟麦，所以不影响结论。 **`[部分未核实]`**
7. **"配置进文件、状态出日志/接口"这套能不能真被运维用起来**（否则无屏设备等于黑盒）。
8. Pi 具体型号建议与 OS edition：官方 `Raspberry Pi OS Lite`（command-line-only）"useful for headless servers, embedded systems"，
   64 位版适用于 Pi 3/4/5（[rpi-os-introduction](https://raw.githubusercontent.com/raspberrypi/documentation/master/documentation/asciidoc/computers/os/rpi-os-introduction.adoc)）
   —— 选型本身待第一站实测后定。

---

## 7. 标注与引用口径

- **[未核实]**：没有一手出处、或属于"板上真会怎样"但**未实机验证**的条目。
- 本文件所有外部事实都带 URL；仓库内事实带 `path`（符号名优先，行号只在必要处）。
- **不改事实内容**：若本文件某条被新证据推翻，**先加状态头/标注**，别直接改写结论（`docs/STRUCTURE.md` §3）。
- 位与 reason 的词汇表是 `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.1 / §2.5.2（`UnavailableReason`）；
  无屏档的"界面降级"落在控制面与日志，不是屏幕（同文件 §2.6 R9 的"这台设备做不到"）。
