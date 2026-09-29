# Vox 工程结构与模块职责

> 开工前的设计文档。代码按这份文档写；文档改了代码跟着改。
>
> **本文档已按现有代码核对过一遍（2026-08-18）。** 凡是文档与代码打架的地方，
> 一律**以代码为准**并在此文档里改掉。文中标 *（计划中）* 的条目表示代码里还没有，
> 属于目标状态——**这一轮核对后已经没有这样的条目了**，本文档描述的都是现有代码。

## 0. 它是什么

> **平台口径（2026-09-20）**：本文写于只有 Windows 的时候，正文里"Windows 外壳"的说法
> 现在指**平台外壳**——Windows 是 `vox-*-win`，Linux 是 `vox-*-linux`（见
> `docs/platform/LINUX.md`）。内核、端口 trait、线程拓扑、数据流这些与平台无关的部分
> 仍然逐条有效。

一个桌面实时语音翻译器（Windows / Linux），不绑定任何游戏或软件。两个能力：

| 能力 | 输入 | 输出 | 触发 |
| --- | --- | --- | --- |
| 对外说话 | 你的麦克风 | 外语**语音**灌进别的软件的麦克风 + 字幕 | 常驻，开关 / 按住说话 |
| 听人说话 | 指定某个程序的声音 | 中文**语音** + 字幕 | 常驻开关 |

对外说话和听人说话是**两条独立流水线，并行跑**（视频通话里你一边说一边听）。

## 1. 数据流

### 对外说话

```
麦克风 48k ─→ 单声道 ─→ RNNoise 降噪 ─→ 音量阀门 ─→ 48k→16k ─→ pcm16 ─→ WS 上传
                                                                              │
                  VB-CABLE Input ←─ 24k→设备率 ←─ 语音 pcm ←────────────────┤
                  字幕（暖白行）  ←──────────────  译文文字 ←────────────────┘
```

降噪在阀门**前面**：先把杂音清掉，音量判断才准。

降噪用 **RNNoise**（`nnnoiseless` 0.5.2），不是 DeepFilterNet——原因见 §9。
RNNoise 只吃 **48 kHz、480 采样一帧**，所以降噪必须放在重采样**之前**（麦克风原生
48 kHz 正好对上），顺序不能调。麦克风格式直接用 WASAPI `GetMixFormat` 的结果，
**协商到的不是 48 kHz 就整层跳过降噪**（`DENOISE_RATE` 对不上），不为了降噪去多插一次
重采样。

### 听人说话

```
进程环回 48k ─→ 单声道 ─→ 音量阀门 ─→ 48k→16k ─→ pcm16 ─→ WS 上传
（不降噪，数字音源本来就干净）                                │
                    耳机/默认输出 ←─ 重采样 ←─ 中文语音 ←────┤
                    字幕（冷白行） ←──────────  译文文字 ←────┘
```

## 2. 分层原则

**轻内核 + 平台外壳**。内核不 `use windows::*`，不碰 Tauri，不知道 WASAPI / PipeWire 存在。
内核要用平台能力时，只认 `ports.rs` 里的 trait；Windows 那边负责实现并在启动时注入。
搬到新平台只需要重写外壳那几个 crate —— Linux 就是这么做的（`vox-audio-linux` /
`vox-input-linux` / `vox-overlay-linux` + 装配层的 `platform/` 分派），内核一行没改。

## 3. 目录结构

```
Vox/
├─ Cargo.toml                  # workspace
├─ catalog/
│  ├─ aliyun.json             # 阿里云模型、语言、音色与 API 元数据
│  ├─ gemini.json             # Gemini Live Translation 元数据
│  └─ gpt.json                # GPT Realtime Translate 元数据
├─ docs/
│  ├─ README.md                # 文档索引（每份一句话 + 状态标记 + 先读哪几份）
│  ├─ STRUCTURE.md             # 放置规则（文件该放哪、命名、生命周期）
│  ├─ architecture/            # 本文档 ARCHITECTURE.md + DECISIONS.md + DIRECTIONS.md
│  ├─ platform/                # SCOPE.md / LINUX.md / WINDOW_BEHAVIOR.md
│  ├─ protocols/               # QWEN_PROTOCOL.md / GEMINI_PROTOCOL.md / PROVIDER_CATALOG.md /
│  │                           # VRC_OSC_PROTOCOL.md / DISCORD_PROTOCOL.md
│  ├─ plans/                   # 施工设计稿（architect 产物，编号 S0/S1/…）
│  └─ research/                # 过期调研沉淀（体检报告 / 交接单，已加状态头）
├─ crates/
│  ├─ vox-core/                # 【内核】平台无关，零重依赖
│  ├─ vox-net/                 # WS 传输实现（tokio + tokio-tungstenite）
│  ├─ vox-dsp/                 # 降噪（RNNoise）+ 重采样（rubato）
│  ├─ vox-audio-win/           # Windows 音频 I/O（WASAPI）
│  ├─ vox-overlay-win/         # Win32 原生悬浮字幕窗
│  ├─ vox-input-win/           # Windows 全局热键
│  ├─ vox-audio-linux/         # Linux 音频 I/O（PipeWire）
│  ├─ vox-overlay-linux/       # GTK 悬浮字幕窗（XWayland）
│  ├─ vox-input-linux/         # Linux 全局热键（evdev）
│  └─ vox-osc/                 # VRChat OSC 发送（纯 std，两个平台共用）
│  （另有平台中立的 vox-overlay-core：画布/布局/帧合成，两个平台共用）
├─ tools/
│  └─ linux-verify/            # Linux 界面像素级验证脚本（见 docs/platform/LINUX.md）
└─ app/
   ├─ src-tauri/               # 【外壳】Tauri 主程序，装配一切
   └─ ui/                      # 设置界面前端
```

workspace 成员是 `crates/` 下这 11 个**加上 `app/src-tauri`**，一共 12 个。

> 装配层必须在 workspace 里：否则 `cargo test --workspace` 和 `cargo clippy --workspace`
> 就**扫不到它**——而装配层是唯一能暴露跨 crate 类型对不上的地方，排除在验收命令之外没有意义。
> 代价是 `app/src-tauri` 跟着共用根 `Cargo.lock` 和 `target/`（十一个 crate 和装配层用同一套依赖版本）。

## 4. 内核 `vox-core`

平台无关，而且**没有任何重依赖**：整个 crate 只依赖 serde / serde\_json / base64 /
tracing / parking\_lot，**既没有 tokio 也没有 tokio-tungstenite**，
一个 async 函数都没有。

网络怎么办？内核只定义一个同步的 `Transport` trait（`cloud/mod.rs` 里），
真正的 WS 客户端在**独立的 `vox-net` crate**里（见 §5）。内核不知道 tokio 存在，
异步全被挡在 `vox-net` 内部。

| 文件 | 职责 |
| --- | --- |
| `runtime.rs` | **唯一账本**。持有全部设置 + 全部当前状态。所有读写都过它，任何人不许自己存一份状态副本。改动后广播事件给 UI 和悬浮窗。 |
| `settings.rs` | 设置的数据结构、默认值、校验、版本迁移（沿用旧 `config.py` 的 normalize/merge 思路） |
| `event.rs` | 内核 → 外壳的事件枚举，**10 个变体**：`SettingsChanged` / `PipelineState` / `GateStatus` / `SubtitleDelta` / `SourceDetected` / `SubtitleCleared` / `UsageChanged` / `MicActive` / `DevicesChanged` / `Notice`。序列化是 `#[serde(tag = "kind", rename_all = "snake_case")]`，所以前端按 `kind` 字段分派 |
| `ports.rs` | 内核对外壳的要求，全是 trait，共 9 个：采集源 `CaptureSource`、播放汇 `PlaybackSink`、**降噪 `Denoise`**、**重采样 `Resample`**、设备目录 `DeviceRegistry`、热键 `HotkeyHost`、字幕显示 `SubtitleView`、**密钥存储 `SecretStore`**、**时钟 `Clock`**（内核不直接读系统时钟，方便测试注入时间）。降噪和重采样两个 trait 都**不保证输出长度等于输入长度** |
| `catalog.rs` | 服务商目录查询、**激活方式 `ActivationMode`（开关/按住）**、**键名 ↔ Windows VK 映射**。Aliyun 与 Gemini 元数据由 `catalog/*.json` 经 `build.rs` 校验并生成，前端读取同一数据源。 |
| `gate.rs` | 音量阀门。**照抄旧 `vad.py`**，参数一个不改（手动 tail 150/preroll 100，电平默认 0.012，听人 0.006/600/200） |
| `hotkey.rs` | 热键的数据结构：修饰键 + 键名、合法性校验、冲突检测、非法值回退。**键名 ↔ VK 那张表不在这里，在 `catalog.rs`** |
| `latency.rs` | 实时链路的延迟统计：连接/首字/首声/首播/整轮等指标各留最近 64 个样本，对外只暴露 last / p50 / p95。只存时间与计数，不存音频或字幕内容 |
| `cloud/mod.rs` | `Transport` trait + 会话状态机：握手、上传音频、热更新、断线重连退避、致命错误判定 |
| `cloud/protocol.rs` | 协议的 serde 类型。收发的 JSON 长什么样只写在这一个文件里。详见 `docs/protocols/QWEN_PROTOCOL.md` |
| `usage.rs` | token 累计（总计/输入/输出，今日/本月，按模型分）+ 持久化 + 重置。**顶层文件，不在 `cloud/` 下面** |
| `subtitle.rs` | 字幕模型：逐字流入、每字自己的 TTL 和淡出进度、双行（对外/听人）分色 |
| `pipeline/mod.rs` | 流水线的执行骨架：`PipelineEngine`（管线程）+ `Deps`（外壳注入的一整套工厂）+ `Plan`（把两条流水线的差别压成一张作业单，骨架只认作业单）。节奏常量都在这里：采集块 `INPUT_BLOCK_MS = 20`、输入队列深度 `INPUT_QUEUE_SIZE = 8`（约 160 ms）、主循环一拍 `POLL_MS = 5`（也是 Stop 握手的最坏响应时间）、阀门状态限流 `GATE_THROTTLE_MS = 200`、降噪生效率 `DENOISE_RATE = 48_000` |
| `pipeline/speak.rs` | 对外说话流水线 |
| `pipeline/listen.rs` | 听人说话流水线 |

内核当前实际的模块清单（`lib.rs` 里 `pub mod` 的那些）：`catalog`、`cloud`、
`event`、`gate`、`hotkey`、`latency`、`pipeline`、`ports`、`runtime`、`settings`、
`subtitle`、`usage`。当前是对外说话、听人说话两条流水线。

外壳注入平台能力用的是 **工厂**（`Deps` 里那五个 `*Factory`）而不是实例：
每次 Start 都要一个全新的采集源 / socket / 降噪器，复用上一次的实例会带着上一段
会话的内部状态（重采样器的缓冲、降噪器的历史）污染新会话。

### 账本的规矩

- 状态就一份，在 `Runtime` 里，`RwLock` 保护。
- 命令带**单调递增序号**，序号**在锁内**分配，防止并发控制命令乱序生效。
- 会话回调要**验身份**：旧会话的迟到回调直接丢弃，不许污染新会话。
- 停止走**握手**，不留孤儿会话；停止时把「麦克风活跃」标志复位。
- 阀门初始状态**跟着会话配置一起下发**，不靠事后补一条命令。
- 一个热键只有一条生效路径，不许两处都监听同一个键。

（以上六条是旧版踩出来的坑，新版不许踩回去。）

## 5. 平台外壳（Windows 为主，Linux 见 docs/platform/LINUX.md）

头两个 crate 其实**跟 Windows 无关**（纯 Rust，能跟着内核一起搬平台），
列在这里只是因为它们是「外壳侧实现」而不是内核。

| crate | 职责 |
| --- | --- |
| `vox-net` | **唯一的 WS 客户端**。`WsTransport` 实现内核的 `Transport` trait，可连接 DashScope 或 Gemini；内部把异步桥回同步接口（`block_on` + mpsc 通道 + reader 任务 + `CancellationToken`）。可以复用外壳已有的 tokio 运行时（`new(Handle)`），也能自建一个 2 worker 的小运行时（`standalone()`）。**内核的异步全被挡在这一层里面。** |
| `vox-dsp` | RNNoise 降噪封装（`nnnoiseless` 0.5.2，纯 Rust，权重内嵌）+ rubato 重采样封装（`SincFixedIn`，**任意率互转**，同率直通；实际用到的是 48k→16k 和 24k→设备率） |
| `vox-audio-win` | 麦克风采集；**按设备名**输出到 CABLE Input；**进程环回**采集指定程序的声音；设备枚举与自动选择；输出采样率探测；VB-CABLE 检测与静默安装；**系统版本检测**（进程环回要求 build ≥ 20348） |
| `vox-overlay-win` | Win32 分层窗（per-pixel alpha 真透明，不用 WebView2）；CPU 渲染中日韩文字；**永久鼠标穿透的纯显示窗**，不包含按钮、状态、token 计数或拖动交互；读取设置中的位置大小 |
| `vox-input-win` | 全局热键监听（含鼠标侧键） |
| `app/src-tauri` | 装配：建 Runtime、注入 Windows 实现、起悬浮窗线程、开热键线程、暴露 **26 个** Tauri 命令给前端、托盘、开机自启、单实例。另外自己负责**落盘去抖 + 原子写**（`persist.rs`）、**DPAPI 加密存密钥**（`sys/secrets.rs`）、**系统时钟**（`sys/clock.rs`）、**设备低频轮询**（`devices.rs`）、**事件桥**（`events.rs`：一条事件同时喂前端、悬浮窗、落盘、开机自启开关） |
| `app/ui` | 设置界面。**侧栏切页**，共 **7** 页（见 `nav.ts`）：首页、模型服务商、听人说话、设置、用量、字幕外观、关于。首页分别在两张主卡内配置服务商、语言和音色；模型由服务商能力表固定。服务商页管理密钥并展示完整能力。前端只认一条事件通道 `vox://event`（`api.ts` 里的 `EVENT_CHANNEL`）。运行期依赖只有 react + react-dom + @tauri-apps/api + @tauri-apps/plugin-updater |

## 6. 线程拓扑

| 线程 | 干什么 | 谁起的 |
| --- | --- | --- |
| 主线程 | Tauri 事件循环。**归 Tauri 独占**，不塞别的活 | `app/src-tauri` |
| 悬浮窗线程 | 自己的 Win32 消息泵。外面靠通道 + `PostMessage` 叫醒它，绝不跨线程直接改窗口 | `vox-overlay-win` |
| 字幕帧线程 | 定时 `prune_subtitles()` + `subtitle_frame()`；只在帧变化时推给悬浮窗，没字幕或隐藏时降频 | `app/src-tauri` |
| 热键线程 | 25 ms 轮询 `GetAsyncKeyState`（沿用旧版做法，够用且不吃全局钩子的坑） | `vox-input-win` |
| 设备轮询线程 | 低频枚举输入/输出/可听程序。插拔耳机、开关程序都要能自己反映出来，不该逼用户点刷新 | `app/src-tauri` |
| tokio 运行时 | 两条 WS 连接、重连。**复用 Tauri 自己那个 runtime**（`WsTransport::new(Handle)`），不另起第二个 | Tauri |
| 采集线程（`vox-capture` / `vox-loopback`） | 跟着 WASAPI 的事件句柄走，**只准搬数据**：切成定长块就丢进内核的输入队列，不做任何处理 | `vox-audio-win` |
| 播放渲染线程（`vox-playback`） | 跟着 WASAPI 的事件句柄走，从**无锁环形缓冲**定长拷一把填进渲染缓冲。缓冲空了自动补零变静音，**不 alloc 不加锁不打日志** | `vox-audio-win` |
| 每条流水线的工作线程（`vox-speak` / `vox-listen`） | 从输入队列取块，做单声道/降噪/阀门/重采样/编码，交给 tokio 发出去；收到的语音再写进播放侧的环形缓冲 | `vox-core::PipelineEngine` |
| 落盘去抖线程（`vox-persist`） | 800 ms 醒一次，有脏才写 `settings.json` / `usage.json`，先写 `.tmp` 再 rename | `app/src-tauri` |

关于 tokio：**内核看不见 tokio**（`vox-core` 里一个 async 都没有），
但运行时实例是**外壳给的**：装配层把 `tauri::async_runtime::handle().inner()`
传进 `WsTransport::new()`，两条 WS 会话共用 Tauri 那批工作线程。
`vox-net` 也提供 `standalone()` 自建一个 2 worker 的小运行时，但**装配层不用它**：
一个进程里跑两个 tokio 运行时只是白占线程。

关于环形缓冲，采集侧和播放侧是**两条不同的路**：
- **采集侧没有环形缓冲**：采集线程把定长块推给 `PipelineEngine` 的输入队列
  （`Mutex` + `Condvar`，深度 `INPUT_QUEUE_SIZE = 8`，满了**丢最旧的**并打限流日志），
  工作线程从这个队列取。
- **无锁环形缓冲只在播放侧**（`vox-audio-win::ring::DropRing`，5 秒容量，
  满了也丢最旧的）：工作线程写，渲染线程读。这条路上确实不加锁不打日志。

另外 WASAPI 这边**没有真正的"回调"**：采集和渲染都是自己起的线程等事件句柄，
不是驱动回调我们的函数。

## 7. 已拍板

> 拍板记录连同**待拍板清单**已经整理进 `docs/architecture/DECISIONS.md`，那份是权威版本。
> 下面这份留作速查。

1. 项目落在 `C:\Users\Wang\Desktop\Vox\`，`VRCQ\` 原样保留只作参考。
2. 激活方式两种可切：**开关**（默认，按一下开、再按一下关）和**按住说话**。音量阀门只在开关模式下生效。
3. 用量**只显示已用**（总计/输入/输出，今日/本月，按模型），加一个「重置计数」。不做配额、不算剩余、不查账户余额。
4. VB-CABLE：安装时检测，没有就从官网下载并静默安装（会弹一次 UAC，可能要重启）。不打包进安装程序。
   「静默」指的是**安装器自己不弹界面**，不是背着用户装：VB-CABLE 是 donationware，
   授权前提是用户看得见产品名并有机会去捐赠，所以 `cable.rs` 用 `ProductDisclosure`
   凭证把这条钉死——界面没先展示 `PRODUCT_NAME` / `PRODUCT_URL` / `DONATION_URL`
   就拿不到凭证，`download` 和 `install` 都进不去。
5. 悬浮窗：**永久鼠标穿透、只显示字幕**；开关、设置、状态和 token 计数全部留在主界面。
6. 双流水线同时开时：**一个窗，两行分色**（听人冷白 `#eef6ff`，对外暖白 `#fff4de`）。

## 8. 从旧版原样搬过来的资产（不重新推导）

- 阀门参数：手动 tail 150 / preroll 100；电平默认 0.012；听人 0.006 / 600 / 200
- 设备自动选择的打分规则
- 输出采样率探测顺序：设备默认 → 24000 → 48000 → 44100，不匹配就重采样
- 字幕样式：深色底 alpha 165，近白文字；逐字 TTL 默认 2600 ms，淡出 900 ms
- 默认音色 Tina；模型、语言和音色的当前官方清单不再视为旧版固定资产，统一在 `catalog/aliyun.json` 维护
- 键名 ↔ Windows VK 映射（含鼠标侧键 XButton1/2，故意不含 Tab）
- 用量持久化格式：`{"<model>": {total_tokens, input_tokens, output_tokens, updated_at}}`
  ——新版在这个基础上**多了 `turns` 以及 `daily` / `daily_date` / `monthly` / `monthly_month`
  两个桶**（跨天跨月自动清零），累计量那几个字段是 `#[serde(flatten)]` 平铺在顶层的，
  所以旧文件能直接读进来。

## 9. 降噪为什么是 RNNoise 而不是 DeepFilterNet

原计划用 DeepFilterNet，实际做不到，换成了 RNNoise。**代码里已经是 RNNoise**
（`vox-dsp` 依赖 `nnnoiseless = "0.5.2"`，没有任何 `deep_filter` 依赖）。

换的原因：crates.io 上的 `deep_filter` 0.2.5 **只发布了 FFT / ERB 这些前处理原语，
没有神经网络推理，也没有权重文件**——光靠它根本跑不起来一个降噪器。要用真的
DeepFilterNet 就得自己塞 ONNX Runtime 或 tract，再外挂几十 MB 权重，
给一个实时语音小工具背这套包袱不值当。

RNNoise 这边：

| | |
| --- | --- |
| 库 | `nnnoiseless` 0.5.2，**纯 Rust**，无 C 依赖、无 build.rs 编译链 |
| 许可 | BSD-3-Clause，可商用 |
| 权重 | **内嵌在库里**，不用额外分发文件 |
| 帧格式 | 固定 **48 kHz、480 采样一帧**（10 ms） |
| 代价 | 降噪强度不如 DeepFilterNet，对稳态噪声（风扇、底噪）够用，对突发噪声（键盘、爆音）一般 |

对我们的场景够了：麦克风原生就是 48 kHz，帧长天然对齐，且降噪的首要目的是
**让音量阀门判断准**，不是追求录音棚音质。

> 这项替换属于**事后补票**，见 `docs/architecture/DECISIONS.md` 的待拍板清单第 1 条。
<!-- 精简：237 行 → 229 行；2026-09-21 补齐失实计数后 245 行 -->
