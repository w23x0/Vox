# Linux 适配方案

> 口径与 `docs/architecture/DECISIONS.md`、`docs/platform/SCOPE.md` 一致：**代码与本文件打架时以代码为准**，
> 然后回头把这里改对。
>
> 本文件回答三件事：**Linux 端怎么做、先做什么、哪一步会卡住。**
> 所有结论都带证据：本机实测（Ubuntu 26.04 / GNOME Wayland / PipeWire 1.6.2，
> 2026-09-20）或代码位置 `path:line`。
>
> `docs/platform/SCOPE.md` §C 是"哪天捡起 Linux 时"的调研沉淀；**本文件是执行方案**，
> 两者不一致时以本文件为准（§C 的粗略判断有三处已被代码核对推翻，见 §3.3）。

---

## 0. 结论速览

> **状态（2026-09-20）：P0–P4 全部落地**，真机验证见 §8。下面这张表是开工时的结论，
> 逐条都实现了（"编译过不了"那条现在是历史）。

| 层 | 结论 | 依据 |
| --- | --- | --- |
| 内核 `vox-core` / `vox-net` / `vox-dsp` | **一行都不用改**（除热键码表，见 §6） | 零平台依赖，已核对 |
| 九个 `ports` trait | **签名全都不用改**，Linux 端照着实现即可 | `crates/vox-core/src/ports.rs` |
| 音频 | 锚定 **PipeWire**（按进程抓音 + 虚拟麦都成立，且**不需要装任何东西**）；锚定范围已拍板，见 §9.1 | §2.1 §2.2 |
| 悬浮窗 | 锚定 **XWayland**（绝对定位 / 置顶 / RGBA 透明实测成立）；GNOME Wayland 原生**做不到** | §2.3 |
| 全局热键 | 锚定 **evdev**（`input` 组**通常不需要**——logind 的 uaccess ACL 会给活跃会话开权限）；GNOME 不支持 GlobalShortcuts portal | §2.4 |
| 密钥 | Secret Service（本机 `org.freedesktop.secrets` 在线） | §1 |
| VB-CABLE | `crates/vox-audio-win/src/cable.rs` 那 1362 行**整段删掉**，换成一个 PipeWire 虚拟 sink | §2.2 §5.1 |
| 编译 | 开工时**连 `cargo check` 都过不了**（装配层 `#![cfg(windows)]` + 无条件依赖 Win crate）→ **已打通**：`cargo test --workspace` 全绿（计数见 `docs/architecture/DIRECTIONS.md` §10.7） | §3.1 §8 |

顺序是 P0 编译打通 → P1 音频 → P2 悬浮窗与密钥 → P3 热键 → P4 打包与 CI，四步都做完了（§8）。

---

## 1. 本机环境实测

```
会话     : XDG_SESSION_TYPE=wayland, DESKTOP=ubuntu:GNOME, WAYLAND_DISPLAY=wayland-0,
           DISPLAY=:0 (XWayland :0 在跑), XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.6P9PV3
音频     : pipewire 1.6.2 + pipewire-pulse + wireplumber 全在跑；无 pulseaudio 守护进程；
           有 pw-cli/pw-link/pw-dump/pw-record/pw-play/wpctl；**没有 pactl / parec**
设备图    : Sinks = 1（HDMI GB206）；Sources = **0（本机没接麦克风）**；
           Clients 里能看到各程序（Steam / Telegram / Chrome …），说明 node 级枚举可行
工具链    : rustc/cargo 1.98.1（本轮 rustup 装到 ~/.cargo，已写进 ~/.bashrc、~/.profile）
           node v22.23.2 / npm 10.9.8 / PyGObject+GTK3 可用 / ffmpeg 可用 / docker 免 sudo
系统库    : 运行时全在（libpipewire-0.3.so.0 / libgtk-3 / libwebkit2gtk-4.1 / libsoup-3.0 /
           libayatana-appindicator3 / libX11 / libXtst）
           **开发包全缺**（libpipewire-0.3-dev / libgtk-3-dev / libwebkit2gtk-4.1-dev …，
           `pkg-config` 查不到 .pc）
其它     : 用户在 sudo/docker 组但 `sudo -n` 要密码；**不在 `input` 组**；
           /dev/input/event* 是 root:input 660 → 现在读不了
托盘     : GNOME 已启用 ubuntu-appindicators@ubuntu.com → 托盘可用
密钥     : org.freedesktop.secrets 在 session bus 上（gnome-keyring）→ keyring crate 可用
```

---

## 2. 四条决定性实测

### 2.1 按进程抓音成立（= Windows 的进程环回）

Windows 侧「听人说话」靠 `IAudioSessionManager2` 找到目标程序的 render session 再
`ActivateAudioInterfaceAsync` 环回。Linux 侧换成 **PipeWire 图里直接连线**：

```
# 目标程序（pw-play 代表任意 Stream/Output/Audio 节点）正在出声
pw-link pw-play:output_FL pw-record:input_FL     # rc=0
pw-link pw-play:output_FR pw-record:input_FR     # rc=0
→ 录到的音频 peak=0.0884 / rms=0.0542（源正弦 peak=0.0884），**不是静音**
```

对照实验（关键坑，纠正过一次）：`target.object` **指向程序的播放流节点也不被认**——
wireplumber 照样按策略把采集流连到**默认源**上。早先"抓到了"是巧合：那会儿默认源是
HDMI 的 monitor，而目标程序正好在往 HDMI 放音，于是"默认源"里就有同一段声音。
本机插上 USB 耳机（默认源变成真麦克风）之后立刻暴露：props 里 `target.object` 明明写着
目标节点，链路却连到 `alsa_input...`，录到的是环境噪声（peak 0.0124，而目标是 0.175）。

→ 结论：**按程序抓音一律不依赖 session manager**：`node.autoconnect=false` +
`link-factory` 按 node/port id 显式建链（`link_keeper.rs`）。麦克风才走自动连接。

### 2.2 虚拟麦成立（替代 VB-CABLE，且不用装东西、不用提权）

```
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=vox_test_sink
                              media.class=Audio/Sink object.linger=true audio.position=[FL FR] }'
→ 节点出现：82 | Audio/Sink | vox_test_sink；端口 88/90 in playback_FL/FR，95/101 out monitor_FL/FR
把声音灌进去再从 monitor 录：peak=0.143 rms=0.0572（源同前）→ 全链路通
```

标准用法：Vox 建一个自己的 sink → 翻译语音写进去 → 目标程序（VRChat / Discord）
在录音设置里选这个 sink 的 **monitor** 当麦克风。**"让用户去设置里选设备"这一步和
Windows 上选 VB-CABLE 完全一样**，所以用户引导可以照搬。

### 2.3 悬浮窗：XWayland 成立，Wayland 原生不成立

用 PyGObject（GTK3）实测，`GDK_BACKEND=x11`（即 XWayland）：

| 能力 | 结果 | 证据 |
| --- | --- | --- |
| RGBA visual（真透明） | ✅ | `rgba_visual=True` |
| 绝对定位 | ✅ | 要求 (120,80)，`xwininfo` 报 `absolute upper-left 120,80` |
| 置顶 | ✅ | `xprop`: `_NET_WM_STATE = _NET_WM_STATE_SKIP_PAGER, _NET_WM_STATE_SKIP_TASKBAR, _NET_WM_STATE_ABOVE` |
| 无边框 + 不抢焦点 | ✅ | `_MOTIF_WM_HINTS` 去装饰、`_NET_WM_WINDOW_TYPE_NOTIFICATION` |
| 鼠标穿透 | 🟡 X 层成立，端到端未验 | 给窗口设空输入域后 `XShapeGetRectangles(ShapeInput)=0`（X 接受且生效）；**但**XTest 在 XWayland 下不生效、XWarpPointer 驱动不了 Wayland 层指针，无人值守点不了一次 → 待人工确认 |

Wayland **原生**（`GDK_BACKEND=wayland`）：xdg-shell 里根本没有"客户端自定坐标"和
"置顶"这两个协议，GNOME 不认 wlr-layer-shell。→ 结论：**GNOME Wayland 下悬浮窗必须走
XWayland**（`GDK_BACKEND=x11`，对 GTK 是进程级变量，所以整个应用都得跑在 XWayland 上）。

> 与 `docs/platform/SCOPE.md` C6 的判断一致（"先试 Tauri 透明窗"），但实测把"先试"变成了
> "只有 XWayland 这条路"，并给出可执行的验证方式。

### 2.4 全局热键：只有 evdev 一条路

- GNOME Wayland **不支持** `org.freedesktop.portal.GlobalShortcuts`（KDE 支持）；
- XWayland 下 `XGrabKey` 只在有 X11 窗口拿到焦点时有效 → 不是全局；
- 唯一稳的是直接读 `/dev/input/event*`（evdev）。

权限这件事比想象的宽松：`/dev/input/event*` 名义上是 `root:input 660`，但**现代桌面
会话里 systemd-logind 会给活跃用户挂一条 `uaccess` ACL**（本机实测
`getfacl /dev/input/event8` → `user:w23x:rw-`），所以**通常不需要 `input` 组**；
headless / SSH / 别的用户的会话才需要 `sudo usermod -aG input $USER`。
应用起不来时会返回带着这条命令的错误，用户看到的是"怎么修"。

evdev 顺带解决两件事：键盘侧键（`BTN_SIDE`/`BTN_EXTRA`，对应 Win 的 XButton1/2）
和**按住说话需要"松开"事件**。

---

## 3. 代码现状：Linux 上现在编译不过

### 3.1 卡点（按修的顺序）

| # | 位置 | 问题 | 状态 |
| --- | --- | --- | --- |
| 1 | `app/src-tauri/src/lib.rs:16` | `#![cfg(windows)]` —— 整个装配层在 Linux 上编译成空 crate | **已修**：去掉 crate 级门控，改成 `platform/{mod,win,linux}` 分派 |
| 2 | `app/src-tauri/Cargo.toml:47-52` | 无条件依赖 `vox-audio-win` / `vox-input-win` / `vox-overlay-win` / `vox-osc-win` + `windows` crate（只有 `openvr` 是 `cfg(windows)`） | **已修**：三个 `-win` crate 与 `windows` 都挪进 `[target.'cfg(windows)'.dependencies]`；`vox-osc` 跨平台留普通依赖；Linux 侧加 `vox-audio-linux` / `keyring` / `chrono` |
| 3 | `crates/vox-overlay-win/src/{window,surface,text}.rs` | 直接 `use windows::*`，但 `windows` 依赖已按 `cfg(windows)` 门控 → 非 Windows 上连它自己都编不过 | **已修**：`lib.rs` 加 `#![cfg(windows)]`，非 Windows 上编译成空 lib |
| 4 | 四个 `-win` crate 的 `lib.rs` | 没有任何 `cfg(target_os)` 门控，"Windows 专属"是靠**编译不过**体现的 | **已修**：`vox-audio-win` / `vox-overlay-win` 加门控（`vox-input-win` 本来就有 `cfg` 兜底，`vox-osc-win` 见下） |
| 5 | `crates/vox-osc-win` | 纯 `std::net::UdpSocket` 的 OSC，却挂了 `#![cfg(windows)]` —— 白扔一个跨平台模块 | **已修**：改名 `crates/vox-osc`、去掉门控，装配层引用同步（Linux 上 3 个单测通过） |

还有一件被忽略的：两个 crate 的 `examples/`（`smoke.rs` 311 行、`live.rs` 107 行、
`snapshot.rs` 433 行）会跟着空 lib 一起编不过（`cargo test` **会**编 examples，所以这不是
"顺手"问题）。已改成"非 Windows 上是空 main、Windows 上 `#[path]` 挂回原实现"，原文件
移到 `examples/<name>/windows.rs`。

### 3.2 需要 `cfg` 拆分的装配层文件（9 个 + 2 个清单）

`lib.rs`（`assemble()` 注入点）、`main.rs`、`audio.rs`、`input.rs`、`overlay.rs`、
`winminmax.rs`（整文件 Win32，Linux 删）、`sys/secrets.rs`（DPAPI → 密钥服务）、
`sys/clock.rs`（`GetLocalTime`）、`sys/fatal.rs`（`MessageBoxW` 弹框），外加
`commands.rs`（26 个命令里 **6 个是 VB-CABLE 专属**）、`dto.rs`（`devices_dto` 探 VB-CABLE 状态）。

**注入点已经干净**：`vox_core::pipeline::Deps`（`pipeline/mod.rs:69`，5 个工厂）+
`Runtime::set_secret_store` / `set_hotkey_host` + `AppState.{registry,overlay}`。
内核一行不改就能换后端 —— 这是这个项目原本就留好的路，不是事后补票。

### 3.3 `docs/platform/SCOPE.md` §C 的三处偏差（以代码为准）

1. §C2 说"Linux 要新增三个 crate"——**对**，但 §C 没提 `vox-osc-win` 也白挂了 `cfg(windows)`；
2. §C9 把"最低内核/glibc"当成 `osver.rs` 的对应物——**不对**。`osver.rs` 管的是
   "系统支不支持进程环回"，Linux 侧唯一有意义的门槛是 **PipeWire 在不在、能不能建虚拟 sink**；
3. §C6/`docs/architecture/ARCHITECTURE.md` §5 说悬浮窗是"永久鼠标穿透、不含拖动交互"——**代码不是这样**：
   `vox-overlay-win` 会根据有没有字幕切 `WS_EX_TRANSPARENT`，并且**实现了完整的拖动/缩放**
   （`WM_NCHITTEST`）。Linux 侧要按**代码**的方针设计（可交互窗 + 可选穿透），别按文档。

---

## 4. 目标架构

### 4.1 crate 布局：三个兄弟 + 两处纯逻辑下放（不新增 crate）

| 现有 | Linux | 实现端口 |
| --- | --- | --- |
| `crates/vox-audio-win` | **`crates/vox-audio-linux`** | `CaptureSource` / `PlaybackSink` / `DeviceRegistry` |
| `crates/vox-input-win` | **`crates/vox-input-linux`** | `HotkeyHost`（+ 事件回调，见 §5.2） |
| `crates/vox-overlay-win` | **`crates/vox-overlay-linux`** | `SubtitleView` |
| `crates/vox-osc-win` | 改名为 **`crates/vox-osc`**，去掉 `#![cfg(windows)]`（**已完成**） | —— |

两处**纯逻辑**（已核对无 Win32 引用）不要复制两份，下放到平台中立 crate：

- `vox-audio-win/src/ring.rs`（无锁 SPSC 环，218 行）→ `vox-dsp`（播放侧两平台共用）；
- `vox-input-win/src/edge.rs`（按键边沿状态机 + 6 个单测，u16 码空间）→ `vox-core::hotkey`
  （两平台共用，Linux 侧喂 evdev 码）。

`vox-overlay-win` 里 `color.rs` / `geom.rs` / `canvas.rs` / `layout.rs` / `render.rs`
（除 `FontRaster` 字段）同样零 Win32 → 下放到 `vox-dsp` 或新建 `vox-overlay-core`；
倾向后者（渲染 ≠ DSP），但要保证 `render.rs` 的 `Glyph` / `FontMetrics` 数据形状不动，
只在 Linux 侧换一个光栅器。

### 4.2 `cfg` 方案（已按这个落地）

**Cargo 层**（`app/src-tauri/Cargo.toml`）：

```toml
[dependencies]
vox-osc = { path = "../../crates/vox-osc" }          # 纯 UDP，两平台共用

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [...] }      # 必须按 target 门控，见下
vox-audio-win / vox-input-win / vox-overlay-win       # 全是 Win32/WASAPI
openvr  = { version = "0.9", optional = true }

[target.'cfg(target_os = "linux")'.dependencies]
vox-audio-linux = { path = "../../crates/vox-audio-linux" }
keyring = "4"                                          # Secret Service（zbus，纯 Rust）
chrono  = { version = "0.4", default-features = false, features = ["clock", "std"] }
```

`windows` 必须按 target 门控：它的 `windows-future` 子 crate 在非 Windows 上**编不过**
（`windows_threading::submit` 不存在），会把整个 Linux workspace 检查卡死在依赖树里。

**源码层**：`src/platform/` 一层分派，`lib.rs` 里一个 `#[cfg]` 都没有：

```text
src/
├─ platform/
│  ├─ mod.rs            # cfg 分派 + VirtualDeviceStatus / GeometryCallback
│  ├─ win.rs            # 对现有 sys/clock、sys/secrets、input、overlay、winminmax 的薄包装
│  └─ linux/
│     ├─ mod.rs         # clock/secret_store/alert/capture/playback/registry/hotkeys/overlay 同名函数
│     ├─ clock.rs       # chrono::Local（已实现）
│     ├─ secrets.rs     # keyring → Secret Service（已实现）
│     └─ audio.rs       # registry 接 PipeWire；采集/播放是"上机就报错"的占位（P1）
├─ overlay.rs           # 字幕帧循环，两平台共用（只有 spawn 走 platform）
└─ commands.rs          # 26 个命令都在；VB-CABLE 那 4 个各自 cfg 一段
```

三处踩过的坑，写下来免得下次再踩：

1. **`#[tauri::command]` 生成的支持项没法 `pub use` 转发**。把 VB-CABLE 那四个命令
   整块塞进 `#[cfg(windows)] mod cable_admin` 之后，`commands::install_virtual_cable`
   就找不到了——`generate_handler!` 要的是宏生成的 `__cmd__install_virtual_cable`，
   而 `pub use` 只转发函数本身。做法改成：模块里放普通函数（`install` / `uninstall` /
   `blockers` / `set_multichannel`），模块外面保留四个同名命令，各自 `#[cfg]` 一段。
   （这条是 Windows 侧 gnu cross-check 抓出来的，Linux 直接编是绿的。）
2. **`audio.rs` 删掉了**：它的三个工厂现在住在 `platform/win.rs`，留着就是死代码
   （Windows 侧编译会报 3 个 dead_code 警告）。
3. **`OverlayHandle` 改成 `Arc<dyn SubtitleView>`**：装配层只调 trait 上的方法；
   Windows 那个 `Overlay::shutdown()` 是固有方法，trait 上没有，所以 `platform/win.rs`
   自己留一份 `OnceLock<Arc<Overlay>>` 用于关窗，帧线程靠 `platform::overlay_running()`
   发现"窗口自己没了"。`vox-core` 的 `ports.rs` 一个字没改。

装配层去掉 `#![cfg(windows)]` 后，`lib.rs` 的装配流程两边共用，只改这一处：

```rust
let clock = platform::clock();                                  // win: GetLocalTime / linux: chrono
runtime.set_secret_store(platform::secret_store(path));          // win: DPAPI / linux: keyring
let deps = Deps { transport: …, capture: platform::capture_factory(),
                  playback: platform::playback_factory(), denoise: …, resample: … };
let registry = platform::registry();
platform::enforce_min_size(&window);                             // win: WM_GETMINMAXINFO / linux: 空实现
match platform::start_hotkeys(runtime.clone()) { … }             // linux: 明确报错（P3）
for note in platform::startup_notes() { runtime.notify(…); }     // linux: PipeWire 在不在
```

---

## 5. 逐项实现方案

**新增依赖（本机查到的当前版本，2026-09-20）**：

| crate | 版本 | 用途 | 需要系统开发包？ |
| --- | --- | --- | --- |
| `pipewire` | 0.10.1 | 音频采集/播放/图操作/建虚拟 sink | 需要 `libpipewire-0.3-dev`（已装） |
| `evdev` | 0.13.2 | 全局热键（读 `/dev/input`） | 否（纯 Rust，靠内核接口） |
| `keyring` | 4.2.0 | 密钥存 Secret Service | 否（走 D-Bus） |
| `fontdb` + `swash` | 0.24 / 0.2 | 字体发现 + 字形光栅化 | 否（纯 Rust，不碰 fontconfig/freetype） |
| `gtk` | 0.18 | 悬浮窗（版本必须跟 tauri 2 用的那套对齐） | 复用 Tauri 已依赖的 GTK3 |

### 5.1 `vox-audio-linux`（最重的一块）

**依赖**：`pipewire`（pipewire-rs，绑 libpipewire-0.3）或裸 `pw-sys`。编译期需要
`libpipewire-0.3-dev`（`.pc` + 头文件）**和 `clang`**（`libspa-sys` 用 bindgen 生成绑定，
缺了报 `stdbool.h file not found` 并 panic），运行期只需要 `libpipewire-0.3.so.0`。

**图枚举的两个坑**（都是实测踩出来的）：

1. **必须跑两轮 roundtrip**，一轮不够。第一轮收到的是全局对象列表，我们是在回调里
   才去 bind 节点/客户端代理，这些 bind 请求排在 `core.sync` 之后才到服务端，
   所以它们的信息事件要第二轮才收得到。少跑一轮的表现是"快照永远是空的"。
2. **默认设备**（`DeviceInfo::is_default`）不在节点属性里，要 bind `Metadata` 对象
   收 `default.audio.source` / `default.audio.sink`，值是 `{"name":"<node.name>"}` 这种
   Spa JSON。实测能读出来（本机默认输出正确标成了 HDMI）。
3. `application.process.binary` 才是**真实二进制名**（`pw-play` 报的是 `pw-cat`，
   因为它是同一个二进制的软链），用它做 `executable` 与"按程序抓音"的匹配键是对的。
4. **采集只认 `chunk.size()` 指的那一段**：`Data::data()` 给的是整个映射缓冲，后面的
   字节是上一轮的残留。按整块算会把样本数放大十几倍（实测 4 秒抓出 4 608 000 个样本，
   正确值是 192 000）——而且峰值还是对的，不看样本数根本发现不了。
5. **`target.object` 认"程序的播放流节点"，不认 sink**：指向 app 的流节点时 wireplumber
   连得对（`pw-record --target=<node id>` 抓到与源一致的峰值）；指向 sink（想抓 monitor）
   会被忽略、偷偷连默认源，录出全 0。所以虚拟麦的回环验证必须用 `pw-link` 显式连。
6. 采集流**不要设 `RT_PROCESS`**：PipeWire 的 process 回调默认在实时线程上，而内核的
   `on_chunk` 会加锁 + 分配 `AudioChunk`，在实时线程里干这个是自找优先级反转。
   Windows 侧也是自己起的普通线程在采集。

**线程模型**（对应 `docs/architecture/ARCHITECTURE.md` §6 的"采集线程 / 播放渲染线程"）：
每个 `CaptureSource` / `PlaybackSink` 起一个**自己的 PipeWire 主循环线程**
（`pw::MainLoop` + `Context`），窗口与内核不碰它。
`stop()` 必须：销毁 stream → 让主循环退出 → **join 该线程** → 才返回。
这样才满足 trait 契约里"`stop` 要能保证回调不再触发"。

| 端口 | 实现 |
| --- | --- |
| `CaptureSource::start(Microphone(None/Some))` | **已落地**：`pw_stream` 方向 input，请求 **f32 / 48 kHz / 2ch**，`target.object` 指定设备或走默认源；返回**协商结果**。真机实测：流按 48 kHz 跑起来、样本数精确（2 秒 = 95 040 个单声道样本）。⚠️ 本机没接麦克风，真麦音频要硬件才能验 |
| `CaptureSource::start(ProcessLoopback{executable, include_tree})` | **已落地**：按 `application.process.binary` 找目标程序的**全部**播放流节点，`node.autoconnect=false`，由 `link_keeper`（自己的连接 + 线程）按 node/port id 把它们的输出端口连到我们的输入端口——**多条流混音**。`include_tree=true` 时沿 `/proc/<pid>/task/*/children` 递归把子进程的流也算进来。真机实测（两条流 220 Hz + 880 Hz）：拓扑上我们的输入端口各收两条 `pw-play:output_* → input_*` 链路，音频上 peak 0.1762、**RMS 0.0881 = √2 × 单条（0.0624）= 两条不同频率正弦的数字混音** |
| `CaptureSource::stop` | 见上（销毁 link + stream，join 线程） |
| `DeviceRegistry::input_devices` | 枚举 `media.class == Audio/Source` 的节点（含 `Audio/Source/Virtual`）；`is_default` 读 wireplumber metadata `default.audio.source` |
| `DeviceRegistry::output_devices` | 同上，`Audio/Sink` + `default.audio.sink` |
| `DeviceRegistry::audio_apps` | 枚举 `Stream/Output/Audio` 节点，按 client 归组 → `executable`（binary 名）/ `display_name`（application.name）/ `pid`；`active = node.state == RUNNING` |
| `DeviceRegistry::virtual_cable_installed` | Linux 上语义变为"PipeWire 可用"（虚拟设备随时能建）。UI 侧 VB-CABLE 那一页在 Linux 隐藏（见 §5.4） |
| `PlaybackSink::open(device, 24 kHz 输入率)` | **已落地**：`pw_stream` 方向 output，请求 48k/2ch f32；`vox-dsp::ring::DropRing` 供渲染回调取数据；24k → 目标率用注入的 `ResampleFactory`。真机实测：3 秒音渲染 264 696 个样本（≈2.75 s × 48 k × 2ch）、丢弃 0、`device_latency_ms` 21 ms（真的从 `pw_stream_get_time` 读出来的） |
| `PlaybackSink::stats()` | `pw_stream_get_time()` → `queued_samples` / `device_latency_ms`；`dropped_samples` 由环缓冲计数 |
| 虚拟麦 | **已落地**（`vox-audio-linux/src/virtual_sink.rs`）：`create_object("adapter", …)` + `factory.name=support.null-audio-sink` + `media.class=Audio/Sink`，固定名 `vox_virtual_mic`。真机验证：`wpctl status` 里出现「Vox Virtual Mic」，端口是 `playback_FL/FR` + `monitor_FL/FR`（立体声），退出即删不留幽灵设备；`cargo run -p vox-audio-linux --example virtual_mic` 可手动复现 |
| 能力门（替代 `osver.rs`） | 连不上 PipeWire socket / 版本 < 1.0 → `PortError` 带明确文案（"需要 PipeWire；纯 PulseAudio/ALSA 环境不支持按进程抓音"）。**不偷偷降级成整机环回**（沿用 `audio.rs:1-8` 的既有方针） |

`CaptureTarget` / `AudioApp` 这些内核类型**不需要改**：Windows 用 exe 名标识程序，
Linux 用同一字段承载 PipeWire 的 binary/application 名，`include_tree` 语义正好对上
"子进程也抓"。

### 5.2 `vox-input-linux`（已落地）

- **枚举**：`/dev/input/event*` → 用 `supported_keys()` 挑出像键盘（有 `KEY_A`+`KEY_Z`）
  或鼠标（有 `BTN_SIDE`/`BTN_EXTRA`）的设备；电源键、手柄之类不碰。
- **监听**：`evdev` crate，**每个设备一个读线程**；先 `poll(fd, POLLIN, 100ms)` 再
  `fetch_events()`——直接阻塞读会让 `stop()` join 不回来（这条是设计时就定下的）。
- **边沿判定**：复用 `vox-core::hotkey::EdgeTracker`（原 `vox-input-win/src/edge.rs`，
  现在在核心里，两个平台共用同一份状态机与 8 条测试）。
- **键码表**：`src/codes.rs`。**不能靠算术推**：evdev 的字母不连续（`KEY_A=30` 但
  `KEY_Z=44`），F 键也不连续（`KEY_F10=68`、`KEY_F11=87`）——当初图省事写过一版
  `KEY_A + offset`，被"字母范围连续"那条测试当场抓出来，现在是写死的表 +
  "UI 列出的键名全都能解析"这条断言。
- **修饰键左右两个码**：`KEY_LEFTCTRL` / `KEY_RIGHTCTRL` 任一按下都算 Ctrl，
  所以内核的 `BindingCode.modifier_groups` 是"组内或、组间与"。
- **权限**：读不了设备时返回的错误里带 `sudo usermod -aG input $USER`；
  桌面会话通常靠 logind 的 uaccess ACL 就够（实测见 §2.4）。
- **退化路径**（没做，记一笔）：X11 会话下可加 `XGrabKey` 分支；GNOME 没有 GlobalShortcuts portal。

### 5.3 `vox-overlay-linux`

**能直接复用的**（已 grep 确认零 Win32）：`color.rs`、`geom.rs`、`canvas.rs`（预乘 BGRA 软件画布）、
`layout.rs`（行度量 / DPI / 换行）、`render.rs`（帧合成、逐字淡出、换行上移动画）。

**要新写的**三块：

1. **光栅器**（替换 `text.rs` 的 GDI `CreateFontIndirectW`+`ExtTextOutW`）：
   用 `fontdb` + `swash`（**纯 Rust，不需要 fontconfig/freetype 开发包**）按 `Glyph`/`FontMetrics`
   现有形状输出覆盖掩码。字体族默认值 `Microsoft YaHei UI` 在 `vox-core/src/settings.rs:236`
   是硬编码 —— 需要改成按平台取默认（Linux：从 `Noto Sans CJK SC` / `Source Han Sans` /
   `WenQuanYi` 里挑第一个存在的），并保留用户可选。
2. **窗口**（替换 `window.rs`）：GTK3 无边框窗（`set_decorated(false)` + `set_keep_above(true)` +
   RGBA visual + `move(x,y)`），`DrawingArea` 的 draw 回调里把 canvas 用 cairo 贴上。
   **必须在 GTK 主线程执行**（与 Windows 的"自有线程 + 消息泵"不同）→ 保留"最新状态邮箱 +
   唤醒"的结构，但唤醒改成 `glib::MainContext::invoke`，让 Tauri 的主循环执行窗口操作。
3. **穿透**：`gdk::Window::input_shape_combine_region` 传空区域（X11 层实测生效，§2.3）。
   按代码现状，这个开关跟"有没有字幕 / 用户要不要拖动"联动。

**桌面环境分支（已落地）**：GNOME → 整个应用以 `GDK_BACKEND=x11`（XWayland）跑，
定位/置顶实测成立，切换发生在 `platform/linux::pre_main()`（GTK 初始化之前设环境变量）；
KDE/wlroots → 以后可加 `gtk-layer-shell`；纯 Wayland 且没有 XWayland → 保持原样（悬浮窗由
合成器摆位，不能自定坐标）。**字幕降级到主窗口内嵌**这条还没做（要动前端）。

**实测结论（真机，XWayland）**：

| 检查 | 结果 |
| --- | --- |
| 窗口位置/尺寸 | `880x200+865+1190`，`IsViewable` |
| 真透明 | `Depth: 32`（ARGB visual `0x254`） |
| 置顶 | `_NET_WM_STATE = SKIP_PAGER, SKIP_TASKBAR, **ABOVE**` |
| 类型 | `_NET_WM_WINDOW_TYPE_NOTIFICATION` |
| 鼠标穿透 | `XShapeGetRectangles(ShapeInput) = 0` 个矩形（完全穿透） |
| 绘制路径 | 探针确认 `connect_draw` 每帧都在跑（画布尺寸随内容变） |
| 像素内容 | 离屏 `snapshot` 例子的 BMP 人工核对（中日文/混排/淡出/空帧/超长行滚动） |

两个踩出来的坑：

1. **不可缩放窗口上 `resize()` 会被 GTK 忽略**（实测窗口高度一直不动）。而且按设计
   窗口高度本来就该由用户设置决定、不跟着内容变（Windows 侧也只有拖动才改 `rect.h`），
   所以 Linux 这边**根本不改窗口尺寸**，视口 = 窗口当前尺寸（`area.allocated_width/height()`）。
2. **GTK 只能在主线程碰**：建窗要求主线程（装配层 `assemble()` 就在主线程，不是就报错）；
   帧线程只写邮箱 + `glib::MainContext::invoke` 叫醒主线程重画，自己不碰任何 GTK 对象。

### 5.4 其余端口与装配层

| 组件 | Windows 现状 | Linux 方案 |
| --- | --- | --- |
| `sys/secrets.rs` | DPAPI（`CryptProtectData`） | **已落地**：`keyring` → Secret Service（`platform/linux/secrets.rs`）；本机实测存→读→删往返成功（`cargo test -p vox --lib -- --ignored secret_service_round_trip`）。没有 Secret Service 的机器会拿到明确错误，**不做明文兜底** |
| `sys/clock.rs` | `GetLocalTime` | **已落地**：`chrono::Local`（`platform/linux/clock.rs`），`now_ms` 仍走单调 `Instant` |
| `sys/fatal.rs` | `MessageBoxW` | stderr + GTK 对话框（GTK 已在依赖里） |
| `winminmax.rs` | `WM_GETMINMAXINFO` 子类化 | **删**；`tauri.conf.json` 已有 `minWidth/minHeight`，Linux 上 `set_min_size` 就够 |
| `commands.rs` 6 个 VB-CABLE 命令 | 下载/UAC/静默安装/阻塞进程 | **已落地**：Windows 实现收进 `#[cfg(windows)] mod cable_admin`，同名命令在 Linux 上回"此平台不需要装虚拟声卡"；前端拿 `virtual_cable_status == "not_applicable"` 就该整块隐藏 VB-CABLE 管理页（**已落地**：`app/ui/src/sections/CableManager.tsx` 的 `manageable` 判据——`not_applicable` 时整块走"没有装卸这一步"的分支；该字段今天只决定这一页摆哪些管理动作，"能不能用"一律读 `virtual_mic` 能力位） |
| `dto.rs::devices_dto` | 探 VB-CABLE / 16 声道状态 | Linux 返回虚拟麦状态 |
| `state.rs` | `OverlayHandle = Arc<vox_overlay_win::Overlay>` | 改成 `cfg` 别名指向 Linux 实现 |
| 托盘 / 单实例 / 自启 | Tauri 插件 | 跨平台，配置微调；GNOME 需要 appindicator 扩展（**本机已启用**） |
| updater | GitHub latest.json + minisign | Linux 上 tauri updater 只支持 AppImage → 待定（§9） |
| `tauri.conf.json` | `bundle.targets = ["nsis"]` | **已落地**：新增 `app/src-tauri/tauri.linux.conf.json`（Tauri 的平台配置文件，构建时自动合并）：targets = `deb` / `rpm` / `appimage`，图标只用 png，deb/rpm 的依赖列表写死（`libwebkit2gtk-4.1-0` / `libgtk-3-0` / `libayatana-appindicator3-1` / `libpipewire-0.3-0`），AppImage 关掉 gstreamer（`bundleMediaFramework: false`，省 15–35 MB） |
| `.github/workflows/release.yml` | `runs-on: windows-latest` | **已落地**：新增 `publish-tauri-linux` job（**ubuntu-24.04**）：装 §7 那套依赖（含 `libpipewire-0.3-dev`、`clang`、`patchelf`）+ Playwright 浏览器 + CJK 字体 → `cargo test --workspace` → `npm run verify` → tauri-action 出 deb/rpm/AppImage，跟 Windows job 共用同一个 release |
| 构建镜像为什么是 24.04 | —— | **22.04 编不过**：它的 PipeWire 头文件是 0.3.48，而 `libspa` 0.10 的 Rust 代码要求更新的 SPA 结构（`spa_video_info_raw` 的 `flags`/`modifier`），bindgen 按旧头生成 → 7 个编译错误（v0.2.0 那次 CI 实测）。24.04 是 PipeWire 1.0.5，正好对上"PipeWire 1.0+"的锚定。**代价**：产物 glibc 基线 2.39（Ubuntu 24.04+ / Fedora 40+ / Debian 13+），22.04 用户跑不了 |

---

## 6. 内核要动的最小改动（3 处）

1. **Windows VK 码下移**：`vox-core/src/hotkey.rs:13-15`（`VK_CONTROL/VK_MENU/VK_SHIFT`）
   与 `catalog.rs` 的 `key_vk` 是内核里唯一的平台泄漏。做法：内核只保留**键名**与
   UI 用的合法键集合（`key_options` / `is_known_key`），**码表**移到 `vox-input-win`
   （`name → VK`）与 `vox-input-linux`（`name → evdev KEY_*`）各自维护。
   `Hotkey::key_vk()` / `modifier_vks()` 从内核删除，改由平台侧解析。
2. **复用模块下放**（§4.1）：`ring.rs` → `vox-dsp`，`edge.rs` → `vox-core::hotkey`，
   原处保留一行 `pub use` 过渡，随后删掉。
3. **`settings.rs:236` 字体默认值平台化**（`Microsoft YaHei UI` → 平台默认 CJK 字体）。

`ports.rs` 九个 trait **一个字都不用改**（已逐个核对签名）。

---

## 7. 前置条件（需要 sudo / 人工）

```bash
# 1. 音频 + Tauri 的开发包（编译期必需）
#    Tauri 官方 Linux 前置（v2.tauri.app/start/prerequisites）：
#      libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev
#      libssl-dev libayatana-appindicator3-dev librsvg2-dev
#    本项目另外要的：libpipewire-0.3-dev（音频后端）、patchelf（AppImage 打包）、
#    clang + libclang-dev（libspa-sys 的 build.rs 用 bindgen 生成绑定，
#    缺了会报 "stdbool.h file not found" 并 panic）。
sudo apt install -y build-essential pkg-config curl wget file libxdo-dev libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf \
  libpipewire-0.3-dev clang libclang-dev

# 2. 全局热键（否则只能跑不含热键的版本）
sudo usermod -aG input $USER   # 之后必须重新登录

# 3. Rust 与前端依赖
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default -c clippy
cd app/ui && npm ci
```

本机（Ubuntu 26.04）这些**已经装好并验证过**：`cargo test --workspace` → 全绿（计数见
`docs/architecture/DIRECTIONS.md` §10.7），
`app/ui` 的 `npm run verify` 全绿（链以 `app/ui/package.json` 的 `verify` 为准；写稿当时 9 步：
`build` → `check:classes` → `check:preview` → `a11y` → `qa:narrow` → `check:cable` →
`check:capabilities` → `check:agent` → `qa:home`）。

**测试环境的一个坑**：本机 PipeWire 图里 **Sources = 0（没接麦克风）**，"对外说话"整条链路
没法用真麦测。验证办法：用 §2.2 的 null sink + `pw-loopback` 造一个合成源喂正弦/语音，
先证明"采集 → 降噪 → 阀门 → 重采样 → 上传"整条链路，再等真麦。

**不需要 Windows 机器也能 verify Windows 侧**（本轮实测跑通）：

```bash
rustup target add x86_64-pc-windows-gnu          # 必须 gnu，不能 msvc，原因见下
sudo apt install -y gcc-mingw-w64-x86-64
cargo check -p vox --target x86_64-pc-windows-gnu            # 整个装配层，含 Win32 路径
cargo check -p vox-audio-win -p vox-overlay-win \
            --target x86_64-pc-windows-msvc --all-targets          # 纯 Rust 的那几个 crate
```

`cargo check` 不链接，所以缺 MSVC 工具链也能跑。**msvc 目标只对不碰 C 的 crate 有效**：
装配层会拉进 `ring` 之类要编译 C 的依赖，cc-rs 找不到 `lib.exe` 直接失败（本轮实测）；
换 `x86_64-pc-windows-gnu` + mingw 就能过，代价是链接期的问题查不出来（链接本来就是
CI 的事）。有了这条，改跨平台代码不用再靠一台 Windows 机器兜底——P0 的装配层改动
就是靠它做双向验证的。

**验证工具（可选，只影响"能不能自己看结果"）**：

```bash
sudo apt install -y python3-gi-cairo gir1.2-webkit2-4.1 imagemagick
```

- `python3-gi-cairo`：缺了它 pygobject 拿不到 cairo 的外部类型转换器，
  `get_snapshot_finish` / `input_shape_combine_region` 都会报
  `Couldn't find foreign struct converter`；
- `gir1.2-webkit2-4.1` + `imagemagick`：`tools/linux-verify/` 里那两个脚本要用
  （截图验证见 §8 末尾）。

---

## 8. 分阶段与验收

| 阶段 | 内容 | 验收（可执行的证据） |
| --- | --- | --- |
| **P0 编译打通** | 四个 `-win` crate 自我门控；装配层去 `#![cfg(windows)]`；`platform/` 分派骨架；`vox-osc` 去 cfg | Linux 上 `cargo check --workspace` 通过；`cargo test -p vox-core -p vox-dsp -p vox-net` 全绿（本机需先装 §7 的开发包跑 workspace 检查） |
| **P1 音频** | `vox-audio-linux` 的 `DeviceRegistry` + `CaptureSource`（麦克风 + 按进程）+ `PlaybackSink` + 虚拟 sink | 写一个 example（照 `vox-audio-win/examples/smoke.rs`）：抓一个正在出声的程序 → 录 2 s → RMS 显著非零；播 24k 语音进虚拟 sink → 从 monitor 录回 → 非静音（§2.1/§2.2 的手工实验固化成 example） |
| **P2 悬浮窗 + 密钥** | `vox-overlay-linux`（GTK + swash）+ keyring | GNOME 下字幕窗能出现在指定坐标、置顶、透明、逐字淡出与 Win 版一致（用 `vox-overlay-win/examples/snapshot.rs` 的思路做无头渲染比对，再加一次肉眼确认） |
| **P3 热键** | `vox-input-linux` evdev + 权限缺失提示 | 按住说话/开关两种模式在真机上生效，键盘与鼠标侧键都能绑 |
| **P4 打包与 CI** | `tauri.linux.conf.json` + release job | `npm run tauri:build` 出 deb/AppImage，装完能启动 |

顺序不能改：P0 是所有事的前提；P1 风险最高（进程环回是 `docs/platform/SCOPE.md` §C5 里排第一的难点），
所以先做。

**已落地（P0–P4 全部完成）**：

```
Linux   : cargo check --workspace                       → 通过（含装配层）
Linux   : cargo test --workspace                        → 全绿（本块是 P0–P4 当轮的实测快照；计数与
            逐 crate 明细随时点变化，当前数字一律见 docs/architecture/DIRECTIONS.md §10.7）
Linux   : cargo clippy --workspace --all-targets        → 新增代码零警告（vox-core/vox-net
            的 3 条是既有的，不在本轮范围内）
Linux   : ./target/debug/vox（GDK_BACKEND=x11）    → 真机启动成功：窗口 960x640、
            居中 (825,413)、IsViewable、_NET_WM_STATE_FOCUSED，WebKitWebProcess /
            WebKitNetworkProcess 子进程都在，stderr 只剩一条 appindicator 弃用警告
Linux   : cargo test -p vox --lib -- --ignored secret_service_round_trip
                                                        → 密钥服务存/读/删往返通过
Linux   : cargo run -p vox-audio-linux --example devices → 与同刻 pw-dump 一致
Linux   : cargo test -p vox-audio-linux -- --ignored virtual_sink_lifecycle
          → 虚拟麦"建 → 图里查得到 → 删 → 查不到"往返通过（真机 PipeWire）
Linux   : cargo run -p vox-audio-linux --example virtual_mic + wpctl/pw-dump
          → 系统 Sinks 里出现「Vox Virtual Mic」，monitor_FL/FR 端口齐全，退出后消失
Linux   : smoke -- tone 3       → 渲染 264 696 样本、丢弃 0、设备延迟 21 ms
Linux   : smoke -- app pw-cat 4 → 协商 48k/2ch、192 000 个单声道样本（精确）、峰值 0.0884
Linux   : smoke -- vmic 20 + pw-record 录 monitor（pw-link 显式连）
          → 录音峰值 0.3000（= 播放源幅度），有声起点正是建链那一刻 → 回环 PASS
Linux   : smoke -- app pw-cat 8（两条 220/880 Hz 流同时放）
          → 拓扑：采集流（autoconnect=False）的输入端口各收两条 pw-play 链路；
            音频：peak 0.1762、RMS 0.0881 = √2 × 单条 = 两条不同频率的数字混音
Linux   : smoke -- mic 2        → 自动连到默认源（本机后来插了 USB 耳机，有真麦克风）
Linux   : cargo run -p vox-overlay-linux --example live + xwininfo/xprop/xshape
          → 悬浮窗 880x200 置顶（_NET_WM_STATE_ABOVE）、Depth 32（真透明）、
            输入域 0 矩形（鼠标完全穿透）、connect_draw 每帧在跑
Linux   : cargo run -p vox-overlay-linux --example snapshot
          → 6 个场景离屏渲染成 BMP，中文/日文/中英混排/逐字淡出/空帧/超长行滚动
            全部符合设计（人工核对过像素）
Linux   : 真机 app 日志 → 热键监听打开并盯住 4 个输入设备（键盘 ×2、鼠标 ×2）
Linux   : npm run tauri:build -- --bundles deb
          → 出 `target/release/bundle/deb/Vox_0.1.4_amd64.deb`（12 MB）；
            Depends 干净（libpipewire-0.3-0 + Tauri 自动识别的 webkit/gtk/appindicator，
            我们自己写的那三个会跟自动识别重复，已删）；.desktop 有 Categories
            （Tauri 默认模板给的是空分类）
Linux   : sudo dpkg -i 那个 deb → /usr/bin/vox 能起：主窗口 960x640 可见、
            热键监听盯上真键盘、托盘初始化；验完 `dpkg -r vox-bridge` 卸掉
Linux   : sudo <evdev_end_to_end 测试二进制> --ignored
          → 造一个 uinput 虚拟键盘，真的打 KEY_F8 按下+松开，
            监听器收到 SpeakPressed + SpeakReleased（按住说话就靠这个 release）
Windows : cargo check -p vox --target x86_64-pc-windows-gnu       → 通过（全量，含装配层）
Windows : cargo check -p vox-audio-win -p vox-overlay-win -p vox-osc --target
          x86_64-pc-windows-msvc --all-targets                          → 通过
UI      : cd app/ui && npm run verify → 全绿
```

**像素级确认（2026-09-20 补齐）**：原来这里写着"没做成"——rootless XWayland 下
`ffmpeg -f x11grab` 抓根窗口只有黑屏（X root 上没有合成结果）、GNOME Shell 的
`org.gnome.Shell.Screenshot` 报 `AccessDenied`、portal 截图要人工点同意。换两条路之后
两个层次都验到了，脚本收在 `tools/linux-verify/`：

```bash
# ① WebKitGTK 自己吐渲染结果（同一份 2.52.6，不需要合成器配合）
cd app/ui && npm run dev
GDK_BACKEND=x11 python3 tools/linux-verify/webkit_shot.py \
    "http://127.0.0.1:5183/?mock=1" /tmp/ui.png
# ② 真 app 窗口的像素（ImageMagick 对 X 窗口做 XGetImage）
./target/debug/vox &
import -window "$(xwininfo -root -tree | grep '"Vox"' | awk '{print $1}')" /tmp/app.png
```

结果：① 前端在 WebKitGTK 里正常渲染（侧栏 7 项、两张流水线卡、Maple Mono CN 中文、
主题色都对，见提交里贴的截图结论）；② **真窗口抓出来就是真界面**——同一份前端、真实后端
数据（"请先配置 阿里云百炼 API 密钥"、"未发现麦克风"、"未发现音频程序"，都跟本机
PipeWire 图的实际状态一致）。这同时证明了三件事：NVIDIA + WebKitGTK 的 DMABUF 渲染器
在本机**没有**黑屏问题、前端与后端 IPC 是通的、设备枚举的结果真的到了界面上。

剩下的：无（P0–P4 全部完成）。

---

## 9. 未决与风险

1. **锚定范围（已拍板）**：**锚定"主流现代发行版默认配置" = PipeWire 必需 +
   Wayland 与 X11 会话都支持**（GNOME 的 Wayland 会话里悬浮窗走 XWayland）。依据：

   | 事实 | 来源 |
   | --- | --- |
   | PipeWire 成为默认音频服务：Fedora 34（2021-04）→ Pop!_OS 22.04（2022）→ Ubuntu 22.10 → Debian 12 Bookworm（2023） | Wikipedia/PipeWire History |
   | Wayland 是 GNOME（Fedora 25+ / Ubuntu 22.04+ / Debian 12）与 KDE Plasma 6 的默认会话 | 同上 + 发行版默认会话 |
   | 仍留在 X11 的多是老 LTS / Cinnamon / XFCE / 老 NVIDIA 驱动 | —— |

   结论：**PipeWire 满不满足是唯一真门槛**（不满足就明确报错，不做整机环回降级）；
   会话层（Wayland vs X11）**不额外写分支**——evdev 热键两者都通，悬浮窗同一份 GTK 代码
   在 X11 会话里就是原生 X11、在 Wayland 会话里靠 XWayland，只有"纯 Wayland 且没有
   XWayland"才降级成窗口内字幕。`docs/platform/SCOPE.md` §C1 记的原始目标"通用 Linux
   （含老发行版 / ALSA）"**正式下调**，理由：那是 N×M 组合，且 ALSA 下"按进程抓音"
   根本不存在。
2. **更新器**：tauri updater 在 Linux **只支持 AppImage**。现状是三种包都发，
   release 说明里写清楚"deb/rpm 用户要手动下载新包"（AppImage 能应用内更新）。
2b. **glibc 基线 2.39（Ubuntu 24.04+）**：为了让 CI 能用上 PipeWire 1.0 的头文件，
   构建镜像从 22.04 提到了 24.04。想覆盖 Ubuntu 22.04（PipeWire 0.3.48）有两条路：
   ①在 22.04 容器里装新一点的 PipeWire 头文件（只给 bindgen 用，链接仍走系统库）；
   ②把 `pipewire` crate 降到能对上 0.3.48 的版本（要改我们这边的 API 调用）。
   两条都没做——先按"主流现代发行版默认配置"这一档走。
3. **多节点混合**：同一程序多条音频流（Chromium 每标签页一条）会全被混进来。Windows 侧
   是按 session 选；Linux 侧要先决定"全混"还是"选最响/最新的一条"。
4. **采样率**：本方案让 PipeWire 做 48k 转换（省掉 `rates.rs` 那套探测）。若某些蓝牙设备
   在 48k 下抖动，要回退到"按设备原生率 + 我们自己重采样"。
5. **穿透只能人工点一次**（§2.3）。X 层已经验到输入域被清空（`XShapeGetRectangles(ShapeInput)`
   = 0 个矩形），但"点击真的落到下层窗口"在本机**没法自动验**——这是环境限制，不是没试：
   - `XTest` 的**按键**注入在 rootless XWayland 下到不了 X 客户端。实测方法：两个 GTK 窗口
     叠在同一矩形（下层 catcher、上层 overlay **不设**穿透）做对照，`XWarpPointer` 进矩形后
     `XTestFakeButtonEvent(1)`，**对照组也收不到 `button-press-event`** → 注入没落到 X 窗口上
     （Wayland 侧指针没动，动的只是 XWayland 内部的 sprite 坐标：`XQueryPointer` 能读到新坐标，
     合成器却不知道）；
   - `XTestFakeMotionEvent` 同样是哑的（sprite 坐标不变），`XWarpPointer` 只改 XWayland 自己的坐标；
   - 剩下能自动的只有 portal 截图/远程桌面，都要人工点同意——等于还是要人。
   留作人工验收：开悬浮窗 → 鼠标移到字幕上点一下 → 点击该落到下面的窗口（比如终端）而不是被字幕吃掉。
6. **NVIDIA + WebKitGTK**：本机是 NVIDIA 显卡，WebKitGTK 的 DMABUF 渲染器在 NVIDIA 上有
   已知黑屏/花屏问题。**已实测排除**（2026-09-20）：用 WebKitGTK 自己的 `get_snapshot`
   渲染前端、再用 `import -window` 抓真 app 窗口，两次都正常出图（方法见 §8 末尾），
   不需要 `WEBKIT_DISABLE_DMABUF_RENDERER=1`。别的 NVIDIA 机器上仍建议第一眼确认一次。
7. **托盘**：GNOME 默认不带托盘，靠 `ubuntu-appindicators` 扩展（本机已启用，别的机器不一定）
   → 关窗收托盘的行为在裸 GNOME 上要有兜底。**已实现**（2026-09-20）：
   `TrayIconBuilder::build()` 在"没人显示托盘"的环境下**照样返回 Ok**，所以光看建没建成功
   不够——新增 `platform::tray_host_available()` 问 D-Bus 上有没有 StatusNotifier 宿主
   （`org.kde.` / `org.ayatana.StatusNotifierWatcher`，占住了或"可激活"都算有：KDE 平时不跑
   watcher，注册图标时才靠 D-Bus 激活拉起来）。没宿主时关窗**最小化**而不是 `hide()`，
   并在启动时推一条 Notice 说明；有宿主时行为不变（收进托盘，托盘菜单/再启动一次都能叫回窗口）。

   真机验证（都是本机、真窗口、真 X 事件）：

   | 场景 | 探针 | 关窗后 | 进程 |
   | --- | --- | --- | --- |
   | 正常会话（GNOME + AppIndicator 扩展，`busctl` 里 `org.kde.StatusNotifierWatcher` 有主） | `= true` | `WM_STATE: Withdrawn`（收进托盘） | 活着 |
   | `DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/vox-no-such-bus`（模拟裸 GNOME） | `= false` | `WM_STATE: Iconic`（最小化） | 活着 |

   外加一条：窗口被收进托盘后再启动一次 → 单实例插件把已运行的实例叫到前面
   （实测窗口从 `Withdrawn` 回到 `IsViewable`）。
8. ~~同一程序多条播放流只抓主目标~~ **已实现**（`link_keeper.rs`）。做这一版踩出来
   三个坑，都记在这儿，免得下次重踩：
   - **不能在采集线程的主循环上 roundtrip**：`probe::roundtrip()` 会 `main_loop.quit()`，
     之后再 `run()` 立刻返回 → 流被拆掉、`start` 等不到协商结果（实测必然超时）。
     所以建链另起一个连接（守护线程自己的主循环）。
   - **`stream.node_id()` 在服务端建出节点之前返回 `PW_ID_ANY`**（实测 4294967295），
     而"等节点建出来"又需要 roundtrip —— 绕回来了。解法：采集流用一个**唯一名字**
     （`vox-capture-<pid>-<纳秒>`），守护按名字在图里找它。
   - **守护的 `start()` 不能等第一次结果**：采集线程要接着跑主循环、节点才会被建出来，
     在 `start()` 里等就成了死锁（守护等节点、采集线程等守护，2 秒后双双超时）。

---

## 参考与关联

- 平台范围与调研沉淀：`docs/platform/SCOPE.md`（§C 与本文件冲突处以本文件为准）
- 架构与分层：`docs/architecture/ARCHITECTURE.md` §2 / §5 / §6
- 拍板记录：`docs/architecture/DECISIONS.md`
- 端口契约：`crates/vox-core/src/ports.rs`

