# vox-headless —— 无屏档（小主板 ARM64 Linux）入口

跟 `app/src-tauri`（桌面档）**并列的第二个外壳**，不是它的分支：同一份芯（`vox-core`）、同一份
PipeWire 后端（`vox-audio-linux`），差别只有"入口"与"起了哪些东西"。**不依赖 Tauri**，所以盒子上
不需要 GTK / WebKitGTK（验收项：`cargo tree -p vox-headless | grep -c tauri` = 0）。

起因与逐条依据在 `docs/platform/EMBEDDED.md`（本文件只讲**怎么用**）。

## 1. 跑起来

```bash
vox-headless [--config <settings.json>]
                   [--print-capabilities | --print-composition | --dry-run]
                   [--start <speak|listen|all>] [--run-for <秒>]
```

| 开关 | 做什么 |
| --- | --- |
| `--config <path>` | `settings.json` 的**文件**路径。不给就按三级回落取：`$VOX_CONFIG_DIR` → `$XDG_CONFIG_HOME/vox` → `$HOME/.config/vox` |
| `--print-capabilities` | 装配后打一份 `CapabilityReport` JSON（stdout）就退。**只读**：不碰 PipeWire（不枚举设备、不探可用性）、不建配置目录、不写文件、不监听端口 |
| `--print-composition` | 装配后打一份**两条腿的清单 + 有效能力位**的 JSON（stdout）就退（见 §3）。**只读**，同上 |
| `--dry-run` | 只装配：验两条腿的清单装不装得上、打能力报告，**不碰 PipeWire（不枚举设备、不探可用性）、不建配置目录、不写文件、不监听端口、不开流、不起流水线**，退出 |
| `--start <speak\|listen\|all>` | 一直跑的模式下，起来就开哪几条腿。不给 = 什么都不开（只当控制面宿主） |
| `--run-for <秒>` | 跑够这么多秒自己收摊（冒烟/自检用）；`0` 或不给 = 一直跑 |

退出码：`0` 成功 ｜ `2` 运行期失败 ｜ `3` 用法错误。

**配置目录里那四个文件**（与桌面档同名同义）：`settings.json` / `usage.json` / `secret.json`
（0600，无屏盒子上常常没有 Secret Service，这一份就是兜底）/ `control.json`（控制面握手文件）。
API 密钥也可以走环境变量覆盖：`VOX_API_KEY_ALIYUN` / `VOX_API_KEY_GEMINI`（优先于文件）。
日志级别走 `VOX_LOG`（tracing 的 EnvFilter 语法）。

样例配置见 `settings.example.json`——它是**部分配置**：没写到的格子走出厂缺省（读设置走
`Settings::from_json` 的 migrate + normalize，坏配置也只会退回缺省，不会让服务起不来）。
**控制面（Agent 面）默认是关的**，要开得自己在 `settings.json` 里写：

```json
{ "control": { "enabled": true, "port": 0 } }
```

## 2. 装成服务（systemd）

两份 unit，**推荐用户单元**：

| 文件 | 装到 | 形态 |
| --- | --- | --- |
| `systemd/user/vox-headless.service` | `~/.config/systemd/user/` | `systemd --user`，**推荐**（实时优先级那条路不被 cgroup 挡住） |
| `systemd/system/vox-headless.service` | `/etc/systemd/system/` | 系统服务，带 `User=`；有两条固有代价，装之前读它文件头 |

两份都有：`Type=exec`、`Restart=on-failure`（+ `RestartSec` / `RestartSteps` / `RestartMaxDelaySec`
指数退避，**要 systemd ≥ 254**，见 §2.4）、`StateDirectory=` / `RuntimeDirectory=`（都叫
`vox-headless`，**故意不与配置目录 `vox` 同名**，理由见 §2.4）、`LimitRTPRIO=`、
`VOX_LOG=`。系统单元的依赖是 `After=network.target`（用户单元是 `After=default.target`，
两者不是一回事，别互抄）。

### 2.1 用户单元（推荐）

```bash
install -Dm755  bin/vox-headless                      ~/.local/bin/vox-headless
install -Dm644  systemd/user/vox-headless.service     ~/.config/systemd/user/vox-headless.service
mkdir -p ~/.config/vox && install -Dm644 settings.example.json ~/.config/vox/settings.json
$EDITOR ~/.config/vox/settings.json   # 按需改：目标语言、要抓的程序（密钥见 §2.3）
systemctl --user daemon-reload
systemctl --user enable --now vox-headless
loginctl enable-linger "$USER"          # 没人登录也活着（只做一次）
journalctl --user -u vox-headless -f
```

### 2.2 系统单元（备用）

```bash
sudo useradd --system --home /var/lib/vox --shell /usr/sbin/nologin vox
sudo install -Dm755 bin/vox-headless                  /usr/local/bin/vox-headless
sudo install -Dm644 systemd/system/vox-headless.service /etc/systemd/system/vox-headless.service
# 配置目录（= `--config` 指的那一份）自己建、归给服务用户：systemd 的 `StateDirectory=` 是
# 另一个名字（`/var/lib/vox-headless`，见 §2.4；那个落点**本机没真跑**，部署机装完核一次），
# 所以这个目录**不会**被它建出来，而服务要往里写 usage.json / secret.json / control.json。
sudo install -d -m755 -o vox -g vox /var/lib/vox
sudo install -Dm644 -o vox -g vox settings.example.json /var/lib/vox/settings.json
sudo systemctl daemon-reload && sudo systemctl enable --now vox-headless
```

### 2.3 两条必须知道的

- **实时优先级**：PipeWire 的 `module-rt` 要 `RLIMIT_RTPRIO`。systemd 给每个 **system** 服务单独的
  cpu cgroup，其 `cpu.rt_runtime_us` 缺省是 **0** → 申请 RT 直接 `EPERM`
  （<https://systemd.io/MY_SERVICE_CANT_GET_REALTIME/>）。所以：**优先 `--user`**；非要用系统单元就得
  `LimitRTPRIO=` **加上**给 cgroup 放行预算（`systemctl set-property … CPUAccounting=yes` + drop-in 写
  `cpu.rt_runtime_us`，见那份 unit 的文件头）。拿不到 RT 不会让服务起不来，只是音频线程退化成普通调度。
- **PipeWire 是用户服务**：system 单元里**不要**写 `After=pipewire.service`（那个名字在 system
  管理器里不存在），它要的是用户的运行期目录（系统单元那份里给了 `XDG_RUNTIME_DIR=/run/user/%U`）。
  本程序启动时自己探 PipeWire（探不到只记一条提示），不影响它接控制面。
- **API 密钥怎么给**（两条路，环境变量优先）：
  ① 环境变量 `VOX_API_KEY_ALIYUN` / `VOX_API_KEY_GEMINI`。服务里用 drop-in 给——
     `systemctl --user edit vox-headless` 然后 `[Service]` 段写
     `Environment=VOX_API_KEY_ALIYUN=sk-…`。**别写进 unit 文件本身**（那份会进版本库/镜像）。
  ② 配置目录下的 `secret.json`（0600），格式就是一个对象：`{"aliyun":"sk-…"}`。
     密钥落盘时启动日志里会有一条明文警告（无屏盒子上常常没有 Secret Service，这一份是兜底）。

**没有 `Type=notify` / `WatchdogSec=`**：那两样要本程序在就绪时发 `READY=1`、活着时定期发
`WATCHDOG=1`——**这一版还没实现**（发了才算数），所以 unit 里不写，免得 systemd 等一个永远不来的
通知。目前"活没活着"看 `Type=exec` + `Restart=on-failure` + 日志。

### 2.4 版本下限与两个名字

- **`RestartSteps=` / `RestartMaxDelaySec=` 要 systemd ≥ 254**（本仓库的 unit 在 259 上验过）。
  更老的 systemd **不认这两个键**：它忽略它们、退回"每次恒定等 `RestartSec=3`"——重启照旧，
  只是不会指数退避（journal 里会各刷一条 "Unknown key" 警告）。老系统上要么升 systemd，
  要么把那两行删掉（删掉后行为就是"恒定 3 秒"）。
- **`StateDirectory=` / `RuntimeDirectory=` 取 `vox-headless`，不取 `vox`**：状态目录
  与配置目录**同名**时，systemd 认为这是"从 253 及更早版本升上来的老部署"，会把状态目录
  **软链**到配置目录上（`~/.local/state/vox -> ../../.config/vox`），并**在第一次启动
  时**——也就是软链建出来**之前**的那一次——记一条迁移消息，**之后不再记**。判据是"状态目录还
  不在、同名配置目录已在"：软链一建出来，这个条件就不成立了。本机 systemd 259 实测（探针 unit：
  同名 `~/.config/<名字>` 已建、`~/.local/state/<名字>` 未建）：连起 3 次，那条
  `Unit state directory … missing but matching configuration directory … exists, assuming update
  from systemd 253 or older, creating compatibility symlink` 只出 **1** 条；把软链删掉再起，才又出
  1 条。名字不同则根本不走这条兼容路径——直接建真目录、一条消息都不记。今天这两个目录里还没有
  任何文件（见 §5），留着是给"属于服务自己的运行期/状态文件"占位。**代价**：`StateDirectory=`
  不再顺带把配置目录建出来——系统单元的 `/var/lib/vox` 因此要在安装时自己建（见 §2.2），
  用户单元的 `~/.config/vox` 本来就在安装步骤里 `mkdir -p`。
- **`StateDirectory=` 的落点**看 `systemd.exec` 的表 2（"Automatic directory creation and
  environment variables"）：`StateDirectory=` 的 system 列是 `/var/lib/`、user 列是
  `$XDG_STATE_HOME`（缺省 `~/.local/state`）。所以系统单元这份落在
  `/var/lib/vox-headless`，用户单元那份落在 `~/.local/state/vox-headless`。
  **本机没有 root，系统单元这一条没真跑**（系统服务起不了，只能过 `systemd-analyze verify` 的
  静态检查）——落点是从 `man systemd.exec` 的规则读出来的，**部署机首次安装时用
  `ls -ld /var/lib/vox-headless` 核一次**。用户单元那一份本机实测过（用**同名**的探针 unit：
  `StateDirectory=vox-headless`，且 `~/.config/vox-headless` 不存在）：建出来的是真目录
  `~/.local/state/vox-headless`，journal 里一条迁移消息都没有。

## 3. 验收读法（机器读的那一面）

```bash
# 这台盒子能做什么（能力位报告）
vox-headless --config <settings.json> --print-capabilities | jq '.tier'          # => "linux_headless"

# 两条腿现在会怎么装（S0 §4.3-A 的验收出口）
vox-headless --config <settings.json> --print-composition | jq '.speak.ops[].kind'
#   => "mono" "denoise" "gate" "resample"
vox-headless --config <settings.json> --print-composition | jq '.capabilities.host.mic'
#   => { "enabled": true, "reason": null }
vox-headless --config <settings.json> --print-composition | jq '.listen.in'
#   => []（无屏档不本机抓程序：`program_tap` 在这一档的上限之外；这档的"听"是 net_in，还没实现）

# 逐条验清单装不装得上（只读：不碰 PipeWire、不连云端、不建目录、不写文件；unit 起来之前当自检用）
vox-headless --config <settings.json> --dry-run
```

**三条报告命令都是只读的**（`--print-capabilities` / `--print-composition` / `--dry-run`）：
清单与能力位只由**设置 + 宿主事实**决定（`Composition::of(设置, 事实)` / "档位上限 − 关掉的位"），
所以它们**连 PipeWire 都不连**（不枚举设备目录、也不探"PipeWire 在不在"）、**不建配置目录**、
不写任何文件、不监听端口、不起流水线。

`--print-composition` 的 stdout **只有那一份 JSON**（日志走 stderr），键是：
`capabilities`（当前有效位）、`speak` / `listen`（这条腿现在派出来的清单，派不出来就是 `null`）、
`errors`（派不出来的理由，`code` 是 S1 的 `DomainErrorCode`）。清单本身与 S1 的
`describe_endpoint` 用**同一个函数**派生（`vox_mcp::endpoints::manifest`），所以打印出来的与 Agent
面看到的是同一份；这份 JSON 的**组装**也只有一个函数（`vox_mcp::endpoints::document`）。桌面档是
同一条命令（`app/src-tauri/src/composition.rs`），调的是同一个组装函数，形状逐字相同。

## 4. 打包

```bash
tools/package-headless.sh                                      # 本机（在板上原生编译）
tools/package-headless.sh --target aarch64-unknown-linux-gnu    # 交叉（要目标 sysroot + linker）
tools/package-headless.sh --profile debug                      # 冒烟用，快
```

产物：`tools/bundle/vox-headless-<版本>-<目标三元组>.tar.gz`（`--out` 可改；缺省那个目录
是仓库 `.gitignore` 里排除掉的，跟 `/tools/signing/` 同一类），里面是

```text
bin/vox-headless                     # 二进制
systemd/user/vox-headless.service    # §2.1
systemd/system/vox-headless.service  # §2.2
settings.example.json                      # 部分配置样例
README.md                                  # 本文件
```

tar 里刻意**不含**托盘图标之类桌面档的资源；归档参数（`--sort=name` / `--mtime=@0` /
`--owner=0`）、`SOURCE_DATE_EPOCH` 与二进制路径都固定，同一份源码 + 同一套工具链出同一个哈希。

## 5. 还没做（**不许广告**）

- `net_in` / `net_out` 两位恒假（S3 目标）：无屏档的"听人说话"要等网络入口落地；今天它的输入是
  `program_tap`（这一档没有），所以 `listen` 那条腿在自检里会被如实报成"装不上"。
- `file_config` 恒假：改 `settings.json` 不会热加载（要重启进程）。
- `background_service` 位仍是 `false(not_wired)`：unit 与打包这一轮落地了，但**位还没有检测者**
  （问 systemd 要状态得走 D-Bus；不拿"unit 文件在"当"服务在跑"的凭据）。
- 没有 health / status HTTP 出口：今天的状态出口就是本文件 §3 的那两条命令 + journal。
- 没有 `Type=notify` / `WatchdogSec=` 支持（见 §2.3）。
- 虚拟麦 / 热键 / 托盘 / 悬浮字幕窗**整块不启动**（这四位在 `host_ceiling(linux_headless)` 之外，
  位恒 `false(unsupported)`）。

## 6. 跟桌面档的三处刻意差别

1. **不引入 Tauri**（盒子装不了也不需要 GTK/WebKitGTK）。
2. **不启动热键 / 托盘 / 悬浮字幕窗**：起了也没人看（没有屏幕、没有键盘、没有 StatusNotifier 宿主）。
3. **不建虚拟麦节点**：无屏档的出口是声卡/网络，不给别的程序当麦克风。

源码地图、装配顺序与每一条的理由写在 `src/lib.rs` / `src/headless.rs` 的头注释里。
