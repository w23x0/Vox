# 项目方向总表

> 口径与 `docs/architecture/DECISIONS.md` 一致：**代码与文档打架时以代码为准**，然后回头把文档改对。
> 文档之间打架时，**以时间较新的为准** —— 裁决结果集中在 §8，冲突处不在这里重开讨论。
>
> 本文只做**汇总 + 裁决 + 状态标注**，理由与细节回链原文件（`docs/architecture/DECISIONS.md` 管"为什么"，
> 这里管"现在算什么数"）。时间基准：**2026-09-21**。

状态标记：

| 标记 | 意思 |
| --- | --- |
| **[已生效]** | 已经在代码里、已经在跑，改它要另开决策 |
| **[已拍板·未开工]** | 定了，但代码里还没有 |
| **[未拍板]** | 只是讨论过，不能当施工依据 |
| **[已作废]** | 被更新的方向推翻，别再引用 |

---

## 1. 项目定位

**[已生效] 芯是平台无关的。** `vox-core` / `vox-net` / `vox-dsp` 不碰平台 API，
外壳能力走 `ports.rs` 的 trait（采集 / 播放 / 热键 / 字幕 / 密钥 / 时钟）。
这是"能装进别的设备"的前提，**已经做对了**，不是欠债（`docs/architecture/ARCHITECTURE.md` §2）。

**[未拍板] 目标定位（2026-09-21 会话，用户原话）：**

- **重心不是 VRChat / Discord**。VRChat OSC、Discord 都只是**出口之一**。
- 目标是**"能够兼容所有电子产品的开发平台"**：手机、平板、网页、电脑、服务器、
  小盒子、开发板、眼镜、头显、插件、车、音箱。
- 目的是把这些**云端实时大模型**收敛到一套统一架构上。
- **配置不能只能靠 GUI 点** —— 要 CLI，或者直接做 MCP，**与 Agent 高度配合**。
- 两条流水线是写死的，**内部逻辑架构要重构**。
- 桌面程序要**解耦**；降噪 / 重采样要看**性能**，"是不是需要去设计一些算子"。

> 该会话以"不着急做代码，先头脑风暴"开始、以"算了 不讨论了"结束，
> **这些方向没有任何一条落成文件或代码**（会话原话："架构方向没有落成文件")。
> 因此上面全部标 **[未拍板]**，包括"唯一的方向"那句 —— 那是当时的口头方向，不是拍板。

**[已作废] 把 VRChat 当产品重心。** 见 §8 第 1 行。

---

## 2. 平台

**[已生效]（2026-09-20 拍板，`docs/architecture/DECISIONS.md` A15 + `docs/platform/LINUX.md`）**

| 平台 | 状态 | 关键约束 |
| --- | --- | --- |
| Windows | ✅ 已发货 | x64；「听人说话」的进程环回需 **build ≥ 20348**（Win11/Server2022） |
| Linux | ✅ 自 `v0.2.1` 起可用（P0–P4 全落地） | **PipeWire 必需**，不满足直接报错，**不做整机环回降级**；热键走 evdev（需 `input` 组）；悬浮窗在 Wayland 会话里走 XWayland，纯 Wayland 无 XWayland 时降级成窗口内字幕 |
| macOS | ❌ **不做**（2026-09-21 用户拍板） | 连同 Apple 全家（iOS / iPadOS / visionOS）一起不做 |

**[已作废] 锚定"通用 Linux"（含老发行版 / X11 / ALSA）。**
已正式下调为**"主流现代发行版默认配置 = PipeWire + Wayland/X11 都支持"**（`docs/platform/LINUX.md` §9.1）。

**[已作废] macOS 的前置问题（2026-09-21 随"不做"一起作废）。** 原讨论是"要不要自己发
HAL 驱动 / 把现有 `cable.rs` 的合规流程指向 BlackHole"——这个问题现在不用答了。

**[遗留]**（`docs/platform/LINUX.md` §9）： glibc 基线 2.39（Ubuntu 24.04+）；
覆盖 Ubuntu 22.04 的两条路都没走；更新器在 Linux 只支持 AppImage，deb/rpm 手动下载；
打包格式（deb/rpm/AppImage/Flatpak/Snap）未定；穿透只能人工点一次验收；
同一程序多播放流的混合策略；蓝牙 48k 抖动时的采样率回退；ARM64/Apple Silicon 延后
（`docs/platform/SCOPE.md` §C9），不再被 macOS 拖着。

**[修正] 跨平台成本估算下调（2026-09-21 讨论）。** `docs/platform/SCOPE.md` §B1 的"三端 ≈ 5～6 倍
工作量"不再准确：真正吃工作量的不是"多写一套实现"，而是**实现前要先答几个岔路**——
Linux 贵在那两个岔路（桌面环境 × 音频后端），一旦拍成"PipeWire 必需 + 会话层不写分支"，
它就退化成"照抄一张端口表"。同一条方法论适用于下面每个新平台。

---

## 2.5 其它平台形态与一般做法（2026-09-21 调研）

> 结论先给：**手机 / 平板 / 网页 / 电脑 / 服务器 / 小盒子 / 开发板 / 眼镜 / 头显 / 插件 / 车 /
> 音箱** 看着 12 种，按"寄生还是宿主、有没有屏幕、能不能装我们的程序"归并，只有 **4 种活法**。
> 每条都附了官方文档出处；抓不到出处的标 **[未核实]**。

### 2.5.1 四种活法

| 活法 | 有哪些 | 能不能跑我们的芯 | 声音从哪来 |
| --- | --- | --- | --- |
| **A. 完整设备**（有系统、有屏、能装程序） | 电脑、手机、平板、车机（Android Automotive）、带屏音箱 | 能（重编一套平台外壳） | 本机麦克风 / 系统内某个程序 / 网络 |
| **B. 无屏设备**（有系统、能装程序、没人看屏幕） | 服务器、小盒子、开发板（树莓派这类）、无屏音箱 | 能，**Linux 那份几乎直接搬** | 网络 / USB 声卡 |
| **C. 寄生的**（跑在别人的程序或网页里） | 网页、插件（OBS / 音频软件 / 编辑器）、部分眼镜 | 能，但芯要编成库或服务 | 宿主喂进来 |
| **D. 只当端点**（算力极小，装不了我们的程序） | 单片机（ESP32 这类）、便宜蓝牙音箱/耳机 | 不能，只做"采集 + 上传" | 硬件麦克风 |

### 2.5.2 逐个平台：一般怎么做

**手机 / 平板（Android）** —— Android 是 Linux 内核 + 上层框架，Rust 能直接用 NDK 编进去。
- 界面要重写（桌面那套 Tauri 界面不能搬），一般是"原生界面 + Rust 核心"，两者用 JNI/UniFFI 对接。
- 听本机麦克风：要运行时权限；要**一直听着**必须开"前台服务"（Android 14 起必须声明服务类型，
  麦克风类还要运行时权限，见 [Foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types)）。
- "听别的程序的声音"：有官方接口，但门槛很高——要 `RECORD_AUDIO`、要用户每次点同意
  （`createScreenCaptureIntent` 授权）、要同一用户空间，**而且被听的 App 必须允许被录**
  （它的 capture policy 得是 `ALLOW_CAPTURE_BY_ALL`，usage 得是 `MEDIA`/`GAME`/`UNKNOWN`，
  见 [Capture video and audio playback](https://developer.android.com/media/platform/av-capture)）。
  → 现实后果：**语音通话类音频（usage 是 `VOICE_COMMUNICATION`）抓不到**，
  所以在手机上"听别人语音通话"这条基本走不通。
- "把译音灌进别的 App 的麦克风"：Android 没有官方虚拟麦克风接口，普通 App 做不到，
  要 root 或系统签名 **[未核实]**。→ 手机上现实做法是**戴耳机听译音** + 自己的字幕。
- 后台存活：系统随时会杀，必须按"随时停、随时接"设计。

**手机 / 平板（iOS / iPadOS）与头显（visionOS）** —— **都不做**（2026-09-21 拍板），记两条事实备查：
- 不给别的 App 造麦克风：Apple 的插件体系（AUv3）是"宿主 App 加载的音频组件"，
  不是系统级虚拟设备（见 [AUAudioUnit](https://developer.apple.com/documentation/audiotoolbox/auaudiounit)），
  能加载它的只有音频工作站那类软件，Discord/微信不在其中。
- 抓别的 App 的声音：系统不提供。→ iOS 上只能"自己的 App 自己听说"。

**网页（浏览器）** —— 网页只做界面和麦克风，重活放服务器（WebRTC / WebSocket 传声音）。
- 麦克风：`getUserMedia`，要 HTTPS + 用户授权。
- "系统 / 其它标签页的声音"：`getDisplayMedia` 可以，但**用户每次都要点一次选择共享对象、
  授权不能记住**，而且"返回的流里可能根本没有音轨"（见
  [getDisplayMedia](https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getDisplayMedia)）。
- 给系统当麦克风：浏览器做不到。
- 后台：标签页切到后台会被限速/暂停，"一直开着"不能假设。
- 在浏览器里跑现在的芯：要编成 WebAssembly，降噪/重采样这类重活会很吃紧。

**服务器 / 小盒子 / 开发板（Linux 系）** —— **最接近已有成果的一类**。
- `docs/platform/LINUX.md` 已实测：PipeWire 下"按程序抓音"与"虚拟麦"都不需要装东西。
- 真正的差别只有一个：**没有屏幕**。→ 配置从文件/环境变量/接口来，状态往外发（日志/HTTP/MCP），
  进程交给 systemd / Docker 管（容器里要放行声卡设备）。
- 树莓派这类 = 同一份 ARM64 Linux；性能弱，必须能关降噪、换便宜的重采样（见 §3.3 路线五）。

**单片机（ESP32 这类）** —— 装不了我们的程序（没有 Linux、内存小）。
- 一般做法：设备只做"采集 + 上传"，翻译放服务器；官方有音频框架
  [ESP-ADF](https://docs.espressif.com/projects/esp-adf/en/latest/index.html)，
  并且已经支持用 MCP 调用外部服务。→ 对我们是"加一种声音插头"，不是加一种宿主。

**智能音箱（Alexa / 小爱这类）** —— 拿不到原始音频。
- 只能做"技能"：定义"用户说什么 → 你的云端返回什么"（intents / utterances + 云端后端），
  见 [Steps to Build a Custom Skill](https://developer.amazon.com/en-US/docs/alexa/custom-skills/steps-to-build-a-custom-skill.html)。
- 想真拿音频自己翻译，只能自己做设备，或把它当**蓝牙 / AUX 音箱**用（= 当时的一个播放出口）。

**车** —— 两条完全不同的路（见 [Android for Cars](https://developer.android.com/training/cars)）：
- **Android Auto**：车只是显示器，程序在手机上跑 → 属于"寄生"，而且要按车厂的模板做界面，
  只允许导航/媒体/通讯/POI/天气/IoT 这几类。
- **Android Automotive OS**：车里自带 Android，可以真装程序，但同样只允许那几类，且驾驶中限制多。
- 现实做法：先把车当"蓝牙音箱 + 手机在跑"，而不是先去开发车机版。

**插件 / 寄生的形态** —— 通用套路就是把芯编成库，宿主调用、宿主喂声音（对应 §3.3 路线三）。
- OBS 有官方 C/C++ 插件模板 + CMake 工程（[obs-plugintemplate](https://github.com/obsproject/obs-plugintemplate)）；
- 音频软件走 AUv3 / VST3 / CLAP 这类插件标准；浏览器扩展能直接拿标签页声音；
- 聊天软件想"按人分开听"必须走它自己的机器人接口（Discord Bot），抓音频做不到（见 §6.2 B13）。
- 好处：**零平台成本，界面也不用自己写**（宿主自带）。

**头显 / 眼镜** —— 分三种 **[未核实，Meta 官方文档未取到]**：
- 内置 Android 的一体机（Quest 这类）：按 Android 那套做（麦克风权限、前台服务、生命周期）；
- 连电脑/手机的（Xreal 这类）：多数只是"显示 + 传感器"，实际跑在你的电脑/手机上 → 寄生；
- Apple Vision Pro：跟 iOS 同一条限制。

### 2.5.3 判断一个新平台能不能上的四个问题

1. **能不能装/怎么跑**：有 Linux / Android / 浏览器吗？都没有 → 归 D 类（只当前端）。
2. **声音从哪进、往哪出**：本机？某个程序？网络？宿主？——四选一或组合。
3. **谁管它的命**：用户点开就活？没人看也要活？被宿主加载才活？会不会随时被杀？
4. **配置和状态从哪进哪出**：屏幕 / 配置文件 / 接口 / 宿主界面？

四问都能一句话答完，就说明这一层已经想清楚；答不出来，就是要先拍的那个岔路。

### 2.5.4 对项目的直接影响

- **"把译音灌进别人的麦克风"只有桌面（Win / Linux）能做**：Android 要 root，iOS 没有 →
  它应该写成**可选能力**，而不是产品的必需项。
- **手机上"听人说话"很难**：通话类音频抓不到（被听方不允许），微信/QQ 这类更不给你接口。
  手机上现实的产品形态是"**戴耳机听译音 + 看字幕**"。
- **无屏设备（服务器 / 盒子）不需要虚拟麦克风和抓程序**：只要"网络进 + 网络出"——正好是已经有的 WebSocket。
- **手机 / 网页最贵的两件事是界面和"随时被杀"**，不是音频。
- **插件形态是最便宜的扩张**（零平台成本，界面宿主给）。

---

## 3. 架构重构方向（2026-09-21 头脑风暴）

> **全部 [未拍板]**。以下是当时摊开的框架，作为下次讨论的起点，不是施工单。

### 3.1 设备收敛

12 类设备 → **3 种外形**（有屏完整的 / 无屏后台的 / 寄生的），共用一个芯。
架构分 **4 层**：芯 → 声音进出 → 宿主 → 界面；**规矩只有一条：每层只跟紧挨着的下一层打交道**。

成品 = 从四层里各挑一个拼起来：
**1 个芯 + 4 种声音插头**（本机 / 系统内部 / 网络 / 宿主给的）+
**3 种宿主管法**（自己启动的 / 无屏后台跑的 / 被别人加载的）+ **4 种界面**（桌面/手机/网页/无）。

### 3.2 芯里 6 条写死的假设（= 全部工程量）

| 芯现在的假设 | 挡住了谁 |
| --- | --- |
| 写死"两条流水线" | 会议（N 人）、车（前后排）、音箱（一路）、插件 |
| 配置只能"本机文件夹 + 界面改" | 服务器、盒子、开发板、无屏音箱 |
| 声音只有"本机麦克风/喇叭/某程序" | 服务器、网页、插件、音箱（声音得从网络或宿主来） |
| 界面与芯同进程（Tauri 桌面程序） | 手机、网页、插件 |
| 降噪 / 重采样被当作"必须有" | 开发板（跑不动）、网页（只能靠网页技术扛） |
| 假设"一直开着不中断" | 手机（后台会被杀）、网页（关掉就没了）、车（熄火就停） |

### 3.3 五条技术路线（编号沿用当时讨论）

- **路线一 ⭐｜MCP 只是"出口 C"，芯里那份"我能做什么"的动作清单才是本体。**
  出口：界面（人点）/ CLI（脚本）/ MCP（AI）/ 进程内调用（插件、网页后端）/ 配置文件 + 状态文件（无屏设备）。
  清单是数据、不含协议代码，所以芯保持又小又干净；加一种出口不动芯。
- **路线二｜控制走 MCP，声音绝不走 MCP。** 动作与查询走 MCP，字幕（每秒几次到几十次）可以走 MCP 通知管道，
  **音频（每秒几万字节 × 2 路）另开专用管子**。这条界线要写进芯的规矩里。
- **路线三｜声音管子由宿主提供。** 芯只认"有人往里塞、有人从里取"：
  同进程（回调 + 内存队列）/ 跨进程（本地 socket）/ 跨设备（WebRTC 或现有 WebSocket）。
  "从别的设备收声音"与"给云模型发声音"是同一件事 —— 共用同一种管子。
- **路线四 ⭐｜心跳由上游管子驱动，不再由声卡驱动。** 现在的滴答来自声卡（每 20 ms 一帧），
  而盒子 / 服务器 / 音箱没有声卡。"谁喂你谁叫醒你"最干净：声卡、定时器、网络包三种来源
  都变成管子的实现细节 → **芯与音频设备彻底解耦**，也顺手解决手机后台被杀、网页关掉就没。
- **路线五｜性能分档 = 开机自测 + 算子成本表，不按平台硬编码。** 启动跑固定小基准，
  对照算子成本表决定这台设备搭哪几块算子。
  **现状缺口**：去噪已经可选（代码里已有开关，采样率不匹配会自动跳过 ✓）；
  **重采样只有贵的 sinc 那一档**，弱设备前必须补一个便宜档。

**另外两条当时的判断：**

- 通用网关那套（LiteLLM / Vercel AI SDK）**不适用**：它们解决的是"一个入口接上百家模型"，
  而本项目只有三家、每家固定一个模型、延迟敏感 —— 借"内部说自己的话 + 每家一层薄翻译"这一半即可。
- MCP 官方已废弃"让 AI 帮忙跑模型"这条路线、建议直接集成各家模型接口 →
  **"芯直连云模型"的设计是对的**，不要改成让 AI 去翻译。

### 3.4 建议的动手顺序（当时给的，未拍板）

1. 先列清"现在哪些行为写死了必须由界面触发" → 把**生命周期 + 配置**从芯里赶出去
   （一刀同时打开 MCP / 无屏设备 / 手机后台，不动算子、不动音频）。
2. 把"两条流水线"改成**算子链**（取声 → 单声道 → 去噪 → 阀门 → 重采样 → 切块 → 上传；
   收包 → 拆块 → 重采样 → 混音 → 出声）；规矩：每块只做一件事、进出都是音频、不偷偷记状态。
3. 做"网络声音"这一种插头（服务器 / 盒子 / 网页后端 / 音箱共用）。
4. 最后做手机界面与网页界面。**眼镜 / 头显有现成优势（SteamVR 那块），单独一条线，不占工期。**

### 3.5 运行时可组合 + Agent 面（2026-09-21 追加，**未拍板**）

**前沿现状（2026-09-21 查证）：** MCP 已把"动作清单 / 可读状态 / 变更通知"标准化
（[spec 2025-06-18](https://modelcontextprotocol.io/specification/2025-06-18)：服务器给
tools / resources / prompts，客户端给 sampling / roots / elicitation）。更关键的是
**"让模型直接写代码、在沙盒里跑"已经成了主流方向**：
Anthropic《[Code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp)》(2025-11-04)
把工具变成**代码 API**（按文件树暴露、按需读取），原文自报工具定义从 15 万 token 降到 2 千
（省 98.7%，本会话直接抓过该页面核到原句）；
Cloudflare 称之为 **Code Mode**（[原文](https://blog.cloudflare.com/code-mode/)）：**只给模型一个工具
——"执行这段代码"**，代码跑在 V8 isolate 里（毫秒启动、每跑一次新建、不用容器），
沙盒只能通过 RPC 调回真正的 MCP 服务器。工程侧还有两条现成路子：
[Extism](https://extism.org/docs/overview)（通用 wasm 插件系统，宿主语言无关）与
[WAMR](https://github.com/wasm-micro-runtime/wasm-micro-runtime)（wasm 跑在 MCU 上，带 Zephyr/ESP-IDF 目录）。

**结论：设备、端点、Agent 出口是同一个模型的三次实例化。**

一条**组合清单**（运行时读，不是编译期写死；下面是 2026-09-21 的**示意，非枚举**）：

```
{ host: native | wasm | browser | mcu          # 谁在跑
  in:   [mic | program_tap | net_in | host_feed]      # 声音从哪来
  ops:  [mono, denoise?, gate?, resample{fast|sinc}, chunk, ...]   # 算子链
  out:  [virtual_mic | speaker | net_out | captions | host_sink]   # 往哪去
  life: interactive | daemon | foreground_service | hosted   # 谁管它的命（见下注）
  ui:   gui | tui | web | none
  control: [cli, mcp, config_file, inproc_api] }      # 谁在操控它
```

> **与实装对齐（2026-09-22 回填，只改引用不改结论）**：上面这份是示意。`life` 那一行原稿写的是
> `daemon | foreground_service | loaded_once`，与实装枚举 `vox_core::composition::Life`
> （`Interactive` / `Daemon` / `ForegroundService` / `Hosted`，`crates/vox-core/src/composition.rs`）
> 不符，已按枚举对齐；其余各格的取值同理**以 `Composition` 的类型为准**，这份草图只表示"有这几格"。

- **设备** = 一张完整清单（桌面 = `mic+program_tap → 全算子 → virtual_mic+speaker+captions`）。
- **端点** = 同一模型的最简实例（`in:[net_in] out:[net_out]`，没有 ops、没有 ui）
  → **端点和设备不需要两套抽象**，这也解释了为什么"网络声音插头"能同时服务服务器/盒子/网页后端。
- **每个 in/op/out 是一个可替换组件**，两种打包：`rustc` 静态（桌面/服务器/手机）+ `wasm` 组件
  （浏览器 / Agent 沙盒 / **单片机**）。单片机 = 把清单里跑得动的算子编成 wasm 跑 WAMR，
  跑不动的（去噪、sinc 重采样）直接从清单里删掉。
- **Agent 面**：一份"能做什么"的动作清单是唯一真源 —— 同一份定义生成三个出口：
  ① 可被代码调用的接口（**第一形态**，因为 Code Mode 要求"工具像代码"）
  ② MCP 包装 ③ CLI 包装。状态用 resources（只读快照）、变更用通知。
  **声音永不走控制面**（§3.3 路线二不变）。
- **沙盒不由我们造**：宿主（Claude/Cloudflare 那类 Agent 运行时）提供沙盒，我们只提供
  "可被代码调用 + 有 schema" 的接口。这样"寄生形态"落地成本极低，且天然跨平台。

**最短落地顺序（比 §3.4 更小的一刀）：**
1. 把现有两条流水线写成**一份清单**（先只是描述，不改行为）；
2. 让 `in/out` 成为可选项（trait 实例 + 从清单选），`ops` 变成可裁剪序列；
3. 从同一份动作定义生成 CLI 与 MCP 两个出口，GUI 退成消费者之一。

**下一步要掰的两条：路线一（清单 vs MCP 的边界）和路线四（心跳从哪来）。**
（§3.5 把路线一推进了一步：清单的第一形态是"代码接口"，MCP 只是包装。）

---

## 4. 现行产品行为（**仍生效**，除非 §3 落地）

来自 `docs/architecture/DECISIONS.md` A 区与 `README.md`，逐条都还在跑：

- 两种激活方式可切：**开关**（默认）/ **按住说话**；音量阀门只在开关模式生效。
- **用量只显示已用**（总计/输入/输出，今日/本月，按模型分 + 重置计数）；不做配额、不算剩余、不查余额。
- **悬浮窗永久鼠标穿透、只显示字幕**；不放开关/设置/状态/token，不响应悬停拖动。
- 双流水线同时开：**一个窗、两行分色**（听人冷白 `#eef6ff`，对外暖白 `#fff4de`）。
- **API 密钥不进配置文件**（Win DPAPI / Linux Secret Service），配置文件可随手分享。
- **模型不让用户选**，每家只启用当前维护表里的最佳专用实时翻译模型。
- **听人说话固定译成中文**；**源语言可给选**，默认自动识别，服务端回报识别结果。
- LiveTranslate 流式字幕走 `.text` + `stash`，整句重写时**整行替换**。
- 「听人说话」**不设电平门**（`threshold<=0` 无条件放行，省 token 靠服务端 VAD）。
- **UI 对齐 GlassUI**（视觉权威源）；主按钮保持蓝色。
- 发版：推 `v*` tag 触发 CI 构建 + 签名（A14）；Linux 额外出 deb/rpm/AppImage。
- 「听人说话」在 GPT 上**没有回合结束事件、没有断句检测、不回报用量** →
  用量页与部分延迟指标在 GPT 上恒为空。**方向（§3，未拍板）**：补能力位（`has_usage` 等）让界面按位降级，
  而不是显示一堆 0；缺口由我们自己补（例如半秒静音自行判定回合结束）。

---

## 5. Provider / 数据层

**[已生效] 现状：** `catalog/*.json` 是单数据源，前后端共读；`catalog_updater` 支持运行时覆盖。

**[已识别缺陷，未修]：**

| 问题 | 证据 |
| --- | --- |
| 知识重复且互相矛盾（采样率写 5 处） | `catalog/aliyun.json` 文字描述、`catalog/gpt.json` 数字 24000、`cloud/protocol.rs` 写死 16000、`cloud/gemini.rs` 再写一遍、`app/ui/src/sections/Aliyun.tsx:203` 界面写死 `PCM16LE · 16 kHz` → **界面对 GPT 说假话**（实际 24 kHz） |
| 表更新只生效一半 | 前端读覆盖版，**后端那份是编译期烘焙进二进制的**（`vox-core/build.rs`） |
| 能力位太薄 | 只有 4 个布尔：`voice_selection` / `voice_clone` / `source_language` / `hot_update_language`；缺"有没有回合结束""会不会回报用量""格式""有没有断句" |
| 表建错层级 | 按 **provider** 建，知识其实该按 **model** 建（`docs/architecture/DECISIONS.md` B11 实证：老模型 18 种语言与新模型 29 种不是子集，而 `AUDIO_OUTPUT_LANGUAGES` 是全局一份） |
| 协议分支散在代码里 | 当前 `cloud/` 下 `match self.provider` 共 7 处（2026-09-21 重构后计数，此前更多） |

**[未拍板] 目标形状（2026-09-21 一次讨论）：** **一张表 + 一套内部母语 + 每家一层薄翻译官**。
表的每一行是一个**模型**，字段分三类（怎么连 / 会什么 / 怎么说话）；
先统一三份 JSON 的字段形状（纯整理、不改行为），再把写死的采样率/端点/认证头/音频格式搬进表，
再把缺的概念做成能力位，最后才动翻译官。

**[待核实]** 当时提到"普通麦克风/喇叭不必每平台写一套"（`miniaudio` / `cpal` 可覆盖
Windows/macOS/Linux/Android/iOS/浏览器）—— 那次没有拉到平台清单，**属于未核实信息**。
**必须一机一套的只有两件**：抓"某一个程序"的声音、做"虚拟麦克风"（手机只能做一半，
浏览器和苹果基本做不到）→ 应写成**可选能力**而非必需。

---

## 6. 未决清单（等人拍板）

### 6.1 2026-09-21 架构线（优先级最高的新账）

> 本期已做完一轮分工调研 + 独立自审，**可执行的分阶段计划在 §9**；下面这几条是"待拍的问题"，
> 与 §9.4 的四个问题有重叠。

1. 路线一：清单 vs MCP 的边界 —— 芯里那份"能做什么"的清单长什么样、五个出口怎么翻译。
2. 路线四：心跳来源 —— 确认"上游管子叫醒"这条路。
3. 路线三：声音管子选型（WebRTC vs 现有 WebSocket）。
4. 路线五：性能分档的阈值与算子成本表怎么定。
5. **先做哪几类设备**（手机？网页？盒子？）—— 这个排序直接决定先拆哪个文件。
6. §3.5 追加的两条：**组合清单的字段先定哪几个**（host/in/ops/out/life/ui/control）；
   **mcu 档要不要现在就留**（wasm 算子 + WAMR），还是先只在设计里保留位置。

### 6.2 更早的开放项（都还在）

| 来源 | 事项 | 卡在哪 |
| --- | --- | --- |
| `docs/architecture/DECISIONS.md` B9 | 拿**真 key** 抓一次报文，确认 LiveTranslate 流式字段 | 要真 key |
| `docs/architecture/DECISIONS.md` B12 / `docs/platform/WINDOW_BEHAVIOR.md` | ① 12px 大圆角真机没渲染（优先）② 最小高度锁不到 38 | 只能真机（Win） |
| `docs/protocols/VRC_OSC_PROTOCOL.md` §5.6 | VRChat 真机抓包（文档自称"影响可见功能的待办，优先级最高"） | 要真机 + 真 VRChat |
| `docs/protocols/VRC_OSC_PROTOCOL.md` §5.1–5.5 | ChatBox 推送节奏 / 截断提示 / avatar 参数名 / 是否改写 VRC `config.json` / 是否读 VRC 反馈 | 未拍板（代码已落地，见 §7） |
| `docs/architecture/DECISIONS.md` B13 / `docs/protocols/DISCORD_PROTOCOL.md` | Discord 按人独立：Q1 分叉 + D1 opus 解码 / D2 协议层 / D3 Bot 部署 / D4 延迟 | 六问全待答 |
| `docs/architecture/DECISIONS.md` B3 | 「听人说话」老系统的整机环回兜底要不要做 | 未答 |
| `docs/architecture/DECISIONS.md` B10 | 术语表（热词）做不做 | 已定"这版不做、先占字段" |
| `docs/platform/LINUX.md` §9 | 多节点混合策略 / 采样率回退 / glibc 基线 / 更新器只支持 AppImage | 见 §2 |
| `docs/platform/SCOPE.md` §C9 | ARM64；Linux 打包格式 | 未答（macOS 已拍"不做"） |
| `docs/research/BACKEND_HEALTH_CHECK.md` 4.2 | `ws.rs::map_connect_error` 用字符串匹配判 DNS | 文档自己结论"收益小，可不动" |
| `docs/research/UI_HEALTHCHECK_PLAN.md` R3 | 拆 `App.tsx` 的 `document.keydown` 大闭包（`nav-route.ts`） | 从未开工，文档要求单独点头 |

---

## 7. 已完成 / 归档（**别再当待办**）

- **2026-09-21 代码精简一轮（10 个 commit，已推送）**：`f12b8b2` 删零引用依赖 →
  `1088da5`/`e96c725` core、`6749fb3` transport、`0752fc7` win-audio、`ee17cbb` overlay、
  `6e279ca` linux、`7fa69dc` app、`5018591` ui 各自收掉重复与死代码 → `dd07536` 修文档失实项
  （`INPUT_BLOCK_MS/INPUT_QUEUE_SIZE/POLL_MS` = 20/8/5、workspace 12 个成员、26 个 Tauri 命令）。
- **`docs/research/BACKEND_HEALTH_CHECK.md` 全部条目已落地**：WASAPI 样板抽到 `capture/shared.rs`；
  `ProcGuard` 消失（`DeviceSetGuard` 只剩一份定义，其余走 `com.rs` 的 `OwnedHandle`）；
  `build.rs` 两个重复结构体合成 `MiniCatalog`；`apply_update` 复用 `remote_url()`；
  `lib.rs` 重导出收窄；`finish_segment` / `needs_resample` / `with_endpoint_fallback` /
  `default_model(provider)` 全仓已无；`vox-audio-win/src/resample.rs` 已删，
  改成 `Resample` trait + `ResampleFactory` 注入。
  → **文档状态已回填**（2026-09-22 该文件顶部已加状态头）；本节仍是权威口径。
- **`docs/research/UI_HEALTHCHECK_PLAN.md`**：第一批 T1–T9、D1、D2、R1、R2 均已落地
  （`ui/ConfirmButton.tsx`、`voices.ts::voiceOptions`、`sections/PipelineCard.tsx`、
  `sections/CableManager.tsx`、`sections/home_logic.ts`）。只剩 R3。
- **`docs/research/FOCUS_HANDOFF.md` 的"下一步"已完成**：`lib/focus.ts` 已是**物理列槽模型**
  （`pickBestSlotNearest` / `bestSlot`），不再需要改 `stepUpDown`。整份交接单可归档。
- **`docs/research/TASK_REPORT_agent.md`**：P0 / P1 全部落地；更新模块**路径 B**（模型目录在线更新）已实现
  （`app/src-tauri/src/catalog_updater.rs`）。**路径 A**（整程序自动更新）仍是"以后需要时再做"。
- **VRChat OSC**：`crates/vox-osc/` + `osc_start/osc_update/osc_stop/osc_send_chatbox/osc_set_avatar`
  命令 + `sections/Vrchat.tsx` 都已落地 —— `docs/protocols/VRC_OSC_PROTOCOL.md` §6"没动代码"已过期。
- **Linux 端 P0–P4 全部落地**（`docs/platform/LINUX.md` §0/§8），剩余见 §2 遗留项。

---

## 8. 冲突裁决表（新者胜）

| # | 议题 | 旧方向（来源 · 时间） | 新方向（来源 · 时间） | 结论 |
| --- | --- | --- | --- | --- |
| 1 | 项目重心 | 以 VRChat/OSC 玩法为主线（README、`docs/protocols/VRC_OSC_PROTOCOL.md` · ≤09-20） | 面向全设备的翻译平台，VRChat/Discord 只是出口之一（会话 01a0c29c · **09-21**） | **以 09-21 为准**；但 VRChat 那套代码照旧有效，只是不再是重心 |
| 2 | 平台范围 | 只做 Windows，跨平台延后不设时间表（`docs/platform/SCOPE.md` A1/A2 · ≤09-19） | Win 主线 + Linux 并行（`docs/architecture/DECISIONS.md` A15 · **09-20**） | **A15 为准**；`docs/platform/SCOPE.md` 开头已加"第 1、2 条被推翻"的告示 |
| 3 | Linux 锚定范围 | 通用 Linux（含老发行版 / X11 / ALSA）（`docs/platform/SCOPE.md` §C1） | 主流现代发行版默认配置，PipeWire 必需（`docs/platform/LINUX.md` §9.1 · **09-20**） | **以 §9.1 为准** |
| 4 | 跨平台成本 | 三端 ≈ Win + macOS 的 5～6 倍（`docs/platform/SCOPE.md` §B1） | 下调：贵在"岔路数量"而非"多写一套"（09-21） | **以 09-21 为准** |
| 5 | 内部结构 | 两条固定流水线（`docs/architecture/ARCHITECTURE.md` · 08-18，现状） | N 路 / 算子链（09-21，**未拍板**） | 现状仍是两条；目标是算子链，**拍板前不动手** |
| 6 | 配置入口 | 只能 GUI 操作（现状） | CLI / MCP 优先，GUI 降成"出口之一"（09-21，未拍板） | 同上 |
| 7 | MCP 的定位 | ——（09-21 前无此议题） | MCP 只是"出口 C"，芯里一份动作清单才是本体；**声音绝不走 MCP**（09-21，未拍板） | 新增，未拍板 |
| 8 | 项目位置 | `C:\Users\Wang\Desktop\Vox`（`docs/architecture/DECISIONS.md` A1） | `/home/w23x/Vox`（当前工作区） | **以实际工作区为准**（A1 待改） |
| 9 | 发版方式 | 本地手动构建（A14 之前） | 推 `v*` tag → CI 构建 + 签名（A14 · 08-22）；Linux 出 deb/rpm/AppImage（09-20/21） | **A14 + Linux 打包为准** |
| 10 | 「听人说话」在 GPT 上没有用量/回合结束 | 界面照常显示用量与延迟，结果恒为空（现状） | 补能力位、按位降级，缺口自己造（09-21，未拍板） | 尚未落地，**临时按现状** |
| 11 | 降噪是否必需 | 曾被当作流水线固定一环 | 已是可选算子（代码已有开关 + 采样率不匹配自动跳过） | **以代码为准**：可选 |
| 12 | 虚拟麦实现 | Windows 装 VB-CABLE（A4） | Linux 用 PipeWire sink，零安装（09-20） | **不冲突，按平台**；Apple 平台不做，其"无原生 API"不再相关 |
| 13 | macOS / Apple 平台 | 未实现，且要先答"要不要虚拟麦"（`docs/platform/SCOPE.md` §C · ≤09-20） | **不做**（用户拍板 · **09-21**） | **以 09-21 为准**；iOS / iPadOS / visionOS 一并作废 |
| 14 | 手机上怎么复用现有资源 | HostShells：手机端最省的是原生壳（JNI/UniFFI），Tauri 移动端只在确需那套 WebView UI 时才值得（09-21 上午） | AndroidPath + 我的裁决：走 **Tauri v2 Android**（界面/芯都已在这套里，省掉原生 UI + JNI 桥）（09-21 晚） | **以 09-21 晚为准**；真机性能与插件分级需实测（见 §10.3-1） |
| 15 | 先做哪类设备 | 先手机（Android），再嵌入式（§10.0 第 1 条 · 09-21 晚） | **直接做嵌入式，手机顺延**（用户拍板 · **09-26**，§10.9） | **以 09-26 为准** |
| 16 | 内部结构（接第 5 行） | 两条固定流水线；算子链"拍板前不动手"（第 5 行 · 09-21） | **拍板做算子链**：清单即作业单，`Worker` 按 `Composition.ops` 执行（**09-26**，`docs/plans/S4-EMBEDDED-REFACTOR.md` S4-B） | **以 09-26 为准**，第 5 行结论作废 |
| 17 | 嵌入式音频后端 | 复用 `vox-audio-linux`（PipeWire）（`docs/platform/EMBEDDED.md` §3.2 · 09-22） | **ALSA 为嵌入式缺省**，PipeWire 降为可选（**09-26**，S4-C） | **以 09-26 为准** |
| 18 | 控制面监听范围 | 只绑 `127.0.0.1`（S1 · 09-22） | 缺省仍本机；**无屏档可开局域网监听，必须配对码 + token 鉴权**（**09-26**，S4-C） | **以 09-26 为准**；桌面缺省行为不变 |

---

## 9. 下一步计划（2026-09-21 调研 + 自审后给出，**已被 §10 取代**）

> ⚠️ 本节是**上一版建议**（用户已拍板四项，见 §10）。保留它是为了留痕：§9.1 的三条打星号结论
> 仍然有效，§9.2 的阶段划分已被 §10.2 的 S 系列替换。

> 本节是**建议**不是拍板。依据：6 份分工调研（MCP/Code Mode 前沿、wasm 组件与装配、嵌入式与 MCU、
> 宿主外壳、本仓库热路径审计、DSP 算子实测）+ 1 份独立审计（14 条缺陷，全部次要，0 条需撤回结论；
> 审计自己也独立重跑了基准数字并逐项复现）。原始报告在 omp 会话产物：`agent://AgentFace` /
> `agent://RuntimeCompose` / `agent://Embedded` / `agent://HostShells` / `agent://RepoOptimize` /
> `agent://BenchDsp` / `agent://AuditReports`（本机 `~/.omp` 会话 01a0c445）。

### 9.1 三条打星号的结论（引用时必须带前提）

1. **"wasm 不能做实时音频热路径" 是推理，不是实测** —— 组件边界拷贝的开销没有实测过，
   而实测的另一面是：**整条 DSP 链只要 0.3% 单核**（采集 3.03 ms/s、播放 1.73 ms/s，余量 ≈200×）。
   两件事不一定矛盾（瓶颈可能是延迟抖动而不是吞吐），但**别当已定事实**。
2. **RNNoise 在 ESP32-S3 的 CPU 占用是"自算"**（官方 AFE 表：内部 RAM 48.7–91.1 KB +
   PSRAM ≈820 KB，单核 30.6–32.2%），不是我们实机测的。
3. **MCP 2026-07-28 的宿主覆盖很低**：官方客户端矩阵只列了 MCP Apps / OAuth-CC / Enterprise-Auth /
   Skills，**Tasks 一行都没有**。按新规范做，短期内可能"没有宿主可测"。

### 9.2 分阶段建议（含粗估，人日；"agent 并行"= 可交给子代理并行）

| 阶段 | 内容 | 产出（可验收） | 粗估 | 备注 |
| --- | --- | --- | --- | --- |
| **P0 地基** | 定稿**组合清单**字段（host/in/ops/out/life/ui/control）；把现有两条流水线**描述成清单**（不改行为） | 一份 `pipeline` 清单 + 3 个正交测试证明"清单能表达现状" | 1–3 | **阻塞 P1/P3/P4**，必须最先 |
| **P1 Agent 面最小切片** | 单一真源的**动作清单** → 生成 CLI + MCP(stdio)；5 个工具（list / describe / compose / session_open / session_close）；字幕走 `resources` + `subscriptions/listen`；**不做** `search_tools`（未到 1%~5% 阈值）、**不用** sampling / roots / logging（已弃用） | 本地 MCP 客户端能连上并完成一次"组合→开会话→读字幕" | 3–5 | 会话 id 必须**显式传参**（新规范已删 session）；先做 `server/discover` 兼容探测 |
| **P2 分配/抖动清理** | 按审计表：overlay 每帧 LCS 短路 + 复用矩阵、denoise 三处临时 Vec、gate 每块 `to_vec`、`to_mono` 单声道 clone、overlay-linux 每帧 surface 复用、Tauri 事件 clone、UI 双 setState；补 `DropRing` SPSC 并发测试与 runtime 锁内序号测试 | 分配次数归零的清单 + 新增并发测试 | 3–4 | **CPU 不是瓶颈**（0.3% 单核），目标是**抖动**与弱设备余量；可与 P1 并行，但同文件改动需串行 |
| **P3 宿主扩展** | 先做**插件式宿主**（OBS 类，性价比最高、零平台成本、界面宿主给）+ **无屏模式**（配置文件 + 日志/接口，不需要虚拟麦）；手机（前台服务 + JNI/UniFFI）与网页（wasm + AudioWorklet，需 COOP/COEP 才能用 SharedArrayBuffer）各算一档 | 一个真跑起来的非桌面宿主 | 5–10 | **需要先拍"先做哪类设备"** |
| **P4 wasm 算子落地** | 只把**纯算子**（下混 / 切块 / 可选 gate）做成 wasm 组件（组件模型或 Extism），热路径（采集/播放/denoise/sinc）保持原生；端点和设备同一套清单 | 同一算子原生/wasm 双跑，行为一致 | 5–10 | 与 P1/P3 有耦合；先做**成本实测**再决定做几层 |
| **P5 MCU 档** | 端侧只做"采集 → NS/VAD/AGC → 16 kHz 单声道上行"，重采样与翻译留服务器；与 ESP-ADF/ESP-SR 或 WAMR 对接 | 一块开发板能出声音到服务器 | 10–20 | **风险最高**；`vox-dsp` 现有代码上不了 MCU（`nnnoiseless`/`rubato` 都是 std 路径） |

### 9.3 推荐顺序（我的判断，供你否）

```
P0 地基 ──┬─→ P1 Agent 面（主线，就是你定的方向）
          ├─→ P2 分配清理（独立、低风险，可随时插入）
          └─→ P3 宿主扩展 ──→ P4 wasm 算子 ──→ P5 MCU 档
```

理由：P0 同时是 P1/P3/P4 的前置；P2 与主线无耦合（可并行）；P3 之后每条设备档都要 P4 的"算子可换档"；
P5 的性价比最低（端侧只做上行、且现有 DSP 要重写），**建议在 P3/P4 跑通之前不碰**。

### 9.4 需要你拍的四个问题

1. **先做哪类设备**：插件宿主 / 无屏 / 手机 / 网页？（P3 入口，直接决定先拆哪个文件）
2. **MCU 档现在留位还是砍掉**？（留位 = 清单字段现在就带上 mcu，实现延后）
3. **P2 现在做不做**？（实测 CPU 富余 200×，纯清理是"卫生"而不是"救命"）
4. **MCP 版本策略**：直接上 2026-07-28（新、宿主覆盖低），还是先只做 stdio + CLI（不押注协议版本）？

---

## 10. 拍板后的执行计划（2026-09-21 晚，**已拍板**）

### 10.0 本轮已拍板

| # | 问题 | 拍板 |
| --- | --- | --- |
| 1 | 先做哪类设备 | **先手机（Android），再嵌入式（小主板 ARM64 Linux）** |
| 2 | MCU 档现在留位还是砍掉 | **砍掉**（清单不预留 mcu 字段；MCU 视为远期，代价见 §10.2） |
| 3 | P2 分配/抖动清理现在做不做 | **不做**（无法实测收益，跳过；保留为可随时捡起的独立任务） |
| 4 | MCP 版本策略 | **直接上 2026-07-28** |

> **2026-09-26 补注**：第 1 条已被 §10.9 推翻（直接做嵌入式，手机顺延），见 §8 第 15 行；原文保留作留痕。
>
> 二次调研（`agent://AndroidPath`、`agent://EmbeddedFirst`）+ 我自己的核对，修正了上一版的几处判断，见 §10.1。

### 10.1 全局再分析：四个结论

1. **平台差异 ≈ 能力位差异，应该用同一套机制表达。**
   仓库已经有一个先例：provider 侧的 `ProviderCapabilities`（4 个布尔）+ 点名查询
   `catalog::supports(provider, bit)`（两者都在 `crates/vox-core/src/catalog.rs`；原先那 4 个
   `supports_*()` 自由函数已被它取代），并且真被消费（`crates/vox-core/src/cloud/gpt.rs` 里
   `catalog::supports(…)` 三处按位分支、不支持的字段就不发）。**宿主/设备侧现在是缺这一层的** —— 手机能做什么、不能做什么（没有虚拟麦、抓不到
   通话音频、悬浮窗要特殊权限）应该同样是"能力位 + 界面按位降级"，而不是散在平台外壳的 if 里。
   **结论：把 §5 的 provider 能力位与 §3.5 的组合清单合成一个能力模型，是本轮唯一的架构级决定。**
2. **手机不是"缩小版桌面"，产品形态要砍一刀。** 三项硬限制：① 「听人说话」抓不到通话音频
   （`AudioPlaybackCapture` 只认 `USAGE_MEDIA/GAME/UNKNOWN` 且对方必须允许被录；
   [`av-capture`](https://developer.android.com/media/platform/av-capture)）；② 没有虚拟麦克风，
   "译音灌进别的 App 的麦克风"在 Android 上不成立；③ 悬浮窗要 `SYSTEM_ALERT_WINDOW`，且 Android 12 起
   **穿透触摸会被丢弃**（[行为变更](https://developer.android.com/about/versions/12/behavior-changes-all)），
   桌面那套"永久穿透常驻"语义在手机上不存在。
   → 手机版的价值是 **"面对面翻译"（自己的麦克风 + 耳机/外放 + 应用内字幕）**，
   不是"把桌面功能搬过去"。**这条要写进产品定位，而不是当技术细节处理。**
3. **「直接上 2026-07-28」在手机上反而是对的。** 手机里没有 CLI，stdio 也不现实；而新规范把协议
   做成**无状态**（删了 session 与握手，改 `server/discover` + 每请求 `_meta` 带版本），
   天然适合走**本机 loopback 的 Streamable HTTP**。也就是说版本选择与手机形态是同一件事，
   不存在"为了新而新"。风险仍是 §9.1 第 3 条（宿主覆盖低）——所以**同时保留 CLI/stdio 出口**，
   把协议面做成可替换的出口而不是芯的一部分。
4. **嵌入式第一站几乎不用动芯。** 实测核对：`crates/vox-core/src/` 对 `std::fs` / `PathBuf` /
   `std::env` / `tauri` / `tokio` **零命中**；`aarch64-unknown-linux-gnu` 是 Rust
   **Tier 1（含 host tools）**、小端；音频后端还是同一个 PipeWire、同一个 `vox-audio-linux`。
   真正要新写的只有"无屏外壳"三件：**配置从哪进、状态往哪出、进程怎么活**
   （systemd `--user` unit + `RuntimeDirectory/StateDirectory`，注意
   [系统服务默认拿不到 RT 预算](https://systemd.io/MY_SERVICE_CANT_GET_REALTIME/)）。
   **MCU 砍掉后，成本不是消失而是上移**：`nnnoiseless` → `rustfft/realfft`、`rubato` → `realfft`，
   全是 std 路径，上 MCU 等于**换实现**（走厂商 AFE），比上一版估的 10–20 人日更贵。

### 10.2 执行阶段（S 系列，粗估人日）

| 阶段 | 内容 | 产出（可验收） | 粗估 |
| --- | --- | --- | --- |
| **S0 地基** | 组合清单定稿（**不含 mcu**）+ 统一能力模型（provider 能力位与宿主能力位同一套）；两条流水线描述成清单（不改行为） | 清单 + 能力位表 + 正交测试证明"清单能表达现状" | 1–3 |
| **S1 Agent 面** | 动作清单 → 出口：**HTTP 优先**（本机 loopback，为手机准备）+ CLI/stdio（桌面/脚本）；5 个工具；字幕走 `resources` + `subscriptions/listen`；不做 `search_tools`；不用 sampling/roots/logging | 桌面与手机都能被外部 Agent 完成"组合→开会话→读字幕" | 3–5 |
| **S2 手机壳（Android）** | ① **Tauri v2 Android 路线**（现有 `app/ui` + `app/src-tauri` 直接编入 APK，省掉"原生 UI + 自搭 JNI 桥"两层，官方 `tauri android init/dev/build`）② 音频换 **Oboe**：请求 48k 单声道 f32（正好对上 RNNoise 的 48k/480 帧），缓冲**偏大**（`PowerSaving`，真延迟在云端）③ `microphone` 型前台服务 + `RECORD_AUDIO` + 通知权限，必须**从可见 Activity 启动**，按"随时被杀"设计 ④ **应用内字幕**（跨 App 悬浮窗延后）⑤ Keystore 版 `SecretStore` ⑥ Play 合规（FGS 声明表、target SDK、**16 KB page size**） | 手机上面对面翻译可用：说中文→耳机出声译音 + 屏幕字幕 | 12–25 |
| **S3 嵌入式（小主板）** | 复用 `vox-audio-linux`；无屏外壳（systemd --user + 配置/状态/日志出口）；关掉字幕/热键/托盘/虚拟麦 | 一块 ARM64 板子上能跑通"网络进、网络出"，且能被 §S1 的控制面管 | 5–8 |
| 后续（**本次不承诺**） | 插件宿主（OBS 类）、wasm 算子档、跨 App 悬浮窗、MCU | — | — |

**顺序理由**：S0 是 S1/S2/S3 的共同前置；S1 与 S2 可并行（不同 crate/文件），但 S1 的出口形态必须先定
（手机没有 CLI）；S3 排在手机之后是因为它复用的东西最多、风险最低，适合在手机这条硬骨头之后用低成本
拿下一整类设备。

### 10.3 需要你留意的三个判断（不阻塞开工）

1. **Tauri 移动端 vs 原生 UI：我选了 Tauri 端**（与上一版 HostShells 的建议相反）。
   理由：本项目界面已经是 Tauri + React，Rust 芯已经在 Tauri 进程里；走原生等于多写一套 UI + 一座 JNI 桥。
   **代价**：WebView 在 Android 上的性能与插件成熟度需真机验证，且 Tauri 官方插件有 `full/partial/none`
   平台分级（托盘/单实例之类在 Android 上是 `none`，无屏外壳那套要能整体关掉）。
2. **手机产品定位要改口径**："听人说话（抓别人的声音）"在 Android 上**结构上做不到**（通话类），
   建议手机版文案只承诺"面对面翻译 + 字幕"。
3. **跳过 P2 的残留风险**：不做分配清理，若手机真机出现掉帧/回声抖动，第一嫌疑是
   overlay 每帧 LCS 与 denoise 每帧 3 次临时 `Vec`（`agent://RepoOptimize` 表 #1/#2）。
   届时按"条件触发"再做，不预先做。

### 10.4 第一轮代理团队产物（2026-09-22）

> **复核状态（已完成）**：两个独立 verifier 分别核了文档迁移与两份设计稿。
> 结论：**都能验收**，无一条结论需要撤回；审计报出的重要缺陷（S0 行号漂移、S1 的 schemars 依赖事实、
> S1 错误码表漏项）**已由原作者修完并附证据**，文档侧 6 条次要缺陷已由 Main 修掉。
> 过程改进：**引用要耐久**——施工单优先写符号名、行号只作参考，已写进 `.omp/AGENTS.md`。

**地基（已完成）**：

- 文档规划正文：`docs/STRUCTURE.md`（放哪、怎么命名、状态头、生命周期）。
- 文档索引：`docs/README.md`；15 份文档已迁入 `docs/{architecture,platform,protocols,research}/`，
  `research/` 四份加了状态头（**别再当待办**）。
- 代理团队：`.omp/AGENTS.md`（项目上下文）、`.omp/RULES.md`（硬规矩）、
  `.omp/agents/{architect,core-dev,shell-dev,agent-face-dev,docs-scribe,verifier}.md`。
  工作方式：**architect 出设计稿 → dev 实现 → verifier 独立复核 → Main 汇总**；一个文件一个 owner。

**设计稿（可直接进施工）**：

- `docs/plans/S0-COMPOSITION-MANIFEST.md` —— 组合清单（八格：host/in/ops/out/life/ui/control + session）
  + 统一能力位模型（provider 位留在 catalog；宿主位由"档位上限表 × 外壳注入的 HostFacts"得出）
  + 改动清单（含 owner）+ 验收标准。核心结论：**现状两条流水线可用清单完全表达**，
  改动被限制在 `Plan::build` 内部中转一次，Worker 主体与既有测试不动。
- `docs/plans/S1-AGENT-FACE.md` —— 控制面设计：动作清单（数据表 + 穷尽 `match`）、
  5 个工具、CLI(voxctl) 与 MCP 同源生成、**本机 127.0.0.1 Streamable HTTP 为主通道**（为手机准备）、
  `server/discover`/`_meta`/`subscriptions/listen`/Tasks 的逐条兼容点、字幕走 resources、
  安全（token 握手 + Origin 403 + 三闸门）、新 crate `crates/vox-mcp/` 只依赖芯。

**⚠️ 设计稿挖出的产品级问题（需用户裁）**：

**[待裁 1] Linux 的虚拟麦是"位说 ON、功能不存在"。** `VirtualSink` 有完整实现
（`crates/vox-audio-linux/src/virtual_sink.rs`）但**只在 `examples/` 被调用**，`app/src-tauri/`
一处都没有；Linux 默认 `speak.output_device = None` → 译音播到系统默认输出。
后果：**Linux 上"对方能在通话里听到译音"这条功能是断的**，而界面还在引导用户去选
"Vox Virtual Mic"。两条路：(a) 接线（把虚拟麦接进装配层，行为变更）；
(b) 位如实报假 + 界面按能力位降级（承认 Linux 只有"字幕 + 本地播放"）。

**[待裁 2]** 组合清单是否接受第 8 个可空格 `session`（备选是塞进 `out`）。
**[待裁 3]** provider 的 4 个待补位（`usage_reporting`/`speech_activity`/`turn_end`/`source_transcript`）
现在进 `catalog/*.json`，还是只进代码枚举。
**[待裁 4]** `schemars` 版本：**锁 0.8.22**（lock 里已有、零新增包，可能要补注解）还是
**上 1.2.2**（要新增 `schemars_derive` 1.x）。我倾向锁 0.8.22——这是唯一一处给芯加依赖的地方，
能不引新包就不引。（verifier 已核实：lock 里有 0.8.22 / 0.9.0 / 1.2.2 三个版本，但 derive 只有 0.8.22。）

**下一轮入口**：verifier 复核通过后 → S0 实现（owner: core-dev，`Plan::build` 中转 + 能力位落地）
→ S1 实现（owner: agent-face-dev，前五步不依赖 S0）。

### 10.5 重构前提与拍板（2026-09-22）

> 用户口径（原话）：**"不砍，我们是双向的。但我们这个是模块化设计——如果硬件不支持我们就没办法，
> 但不是不做。我们现在这个方向是向移动端的。旧代码原地重构吧，相当于我们桌面端重构成现在这个多兼容版本了。"**

**由此确立的三条原则（高于本节的任何具体清单）**：

1. **功能不删，只按能力位开门/关门。** 「对外说话」与「听人说话」**都留在架构里**；
   某宿主/硬件做不到（例：安卓抓不到通话音频、手机没有虚拟麦）→ 该宿主的能力位报 `false`，
   界面按位降级并在文案里说清"这台设备做不到"，**不是把功能从产品里去掉**。
2. **能力位必须是事实，不是期望。** 位为 `true` 就必须由"真的把这条路打开的那段代码"负责
   （落实 S0 §2.6 的 R6 定义者规则）。因此 **Linux 虚拟麦不能继续"位说 ON、功能不存在"**：
   该接线就接线，**接线之前位如实报 `false`**。
3. **桌面不是被冻结的旧版，而是新架构里的一档宿主。** 原地重构（同仓库）；
   现有 `vox-*-win` / `vox-*-linux` 实现收进"宿主外壳"层，与新宿主（手机、嵌入式）并列。

**功能处置表（按第 1 条修正后的版本）**：

| 处置 | 内容 |
| --- | --- |
| **在架构内（按宿主能力位开门）** | 对外说话（麦克风→译音→耳机/虚拟麦）；听人说话（抓某个程序/通话→译音+字幕）；字幕（应用内 / 跨 App 悬浮窗）；三种 provider；密钥；设置；**控制面 MCP/CLI（Agent 面）** |
| **在架构内**（已接线，本机位实测 `true`；引导文案可恢复） | **Linux 虚拟麦**（owner shell-dev，S0 §3.2）：`VirtualSink` 已接进装配层——`platform/linux/virtual_mic.rs` 持句柄（`ensure()` 建节点 / `shutdown()` 删节点，排在 `engine.shutdown()` 之后），`host_facts()` 的 `virtual_mic` 位以"句柄真的建出来"为凭据（本机实测为 `true`），译音按**节点名** `vox_virtual_mic` 送进去；`off(not_wired)` 现在只剩"`ensure()` 还没跑过"这一种含义 |
| **实现排期靠后（能力位先报真实值）** | 虚拟麦（Win VB-CABLE）；跨 App 悬浮窗；VRChat OSC；Discord；SteamVR；全局热键 |
| **真正砍掉** | 用量/计费页与对外账本（内部日志保留）；三语 UI 收敛为 **zh + en**；OCR 姊妹模块；两套窗口边缘特例（`winminmax.rs`、大圆角/最小高度两项）——它们与服务端/多宿主无关 |

**Main 拍掉的四项**：① Linux 虚拟麦 → **接线，接线前位报 false、文案撤下**（不再是"永久延后"）；
② 组合清单**接受第 8 格 `session`**；③ provider 4 个待补位**只进代码枚举**；
④ `schemars` **锁 0.8.22**。

### 10.6 三个产品级问题的答复（已拍）

1. **「听人说话」保不保** → **保**（双向）。抓取能力按宿主能力位开门/关门。
2. **桌面算不算目标平台** → **算，但它只是"多兼容版本"里的一档宿主**；原地重构，不冻结、不并行维护。
3. **旧代码处置** → **原地重构**（同仓库；被重构取代的模块直接删，不留兼容层）。

**结论：本轮无待拍项。** 下一轮 = 按新原则修订两份设计稿 + 开 S0/S1 实现（见 §10.7）。

### 10.7 第二轮分工与实际进度（2026-09-22 起）

> **写法**：表格保留原分工的「任务 + 依赖」两列，末列「实际进度」按**代码里能看到的实情**回填（"代码与文档打架以代码为准"），当轮落地即更新。

| 代理 | 任务 | 依赖 | 实际进度（2026-09-22 回填） |
| --- | --- | --- | --- |
| `architect`（S0 作者） | 按 §10.5 三条原则修订 `docs/plans/S0-COMPOSITION-MANIFEST.md`：能力位=宿主×硬件事实的映射、双流水线都在、desktop 作为宿主之一、Linux 虚拟麦接线进改动清单 | — | **已完成**（修订第 2 版；两个独立 verifier 的复核结论见 §10.4） |
| `agent-face-dev` | 开 S1 前五步（协议校验 / `server/discover` / `tools/list` / CLI 骨架，不依赖 S0） | — | **已落地**：动作表 `actions.rs`（5 条动作，唯一真源）+ 协议面 `mcp/` + 本机 HTTP 传输（`src/transport/http.rs`：只绑 `127.0.0.1`、单路径 `/mcp`、POST-only，`voxctl serve --state-file <path> [--port <n>]`）+ **端点投影**（`src/endpoints.rs` 的 `Settings` → 清单，`tests/endpoints.rs` 15 条）+ **会话与 token**（`src/session.rs`：handle 注册表 + `c_` 前缀一次性 compose token，`tests/lifecycle.rs`）+ **`Grants`**（`crates/vox-mcp/src/ledger.rs` 的 `impl Grants for Runtime`，每次 `tools/call` 现读 `Settings.control.allow_*`，总开关关着一位都不开）；另有 `tests/protocol.rs` 13 条 + `tests/http.rs` **19 条** + `tests/resources.rs` 8 条 + `tests/voxctl.rs` 12 条 + `tests/lifecycle.rs` 4 条——`cargo test -p vox-mcp` 第十四轮当时共 **81 条** = 71 条集成 + 10 条 crate 内单测（第十四轮实测，见本节末「第十四轮收口」）。**第十轮补注**：`tests/endpoints.rs` 已由 13 条增至 **15 条**——新增的两条正是本轮两处安全护栏的钉子（见本节末「第十轮回填」）。**第十四轮复核转正**：本格当时列的"未落地"三项（资源面 / stdio 桥 / 5 个动作子命令）**现已全部落地**，逐项证据见本节末「第十四轮收口」；**第十五轮收口**：`actions.rs::composition_schema!` 那一格已由 `build.rs` 从 `Composition` 的类型生成（`vox-mcp` 的 feature `json-schema` 默认开、转发芯的 feature；`--no-default-features` 走如实放宽的占位），`tests/protocol.rs` 增至 **15 条**（新增 `every_ref_resolves_inside_its_own_schema` 与 `the_composition_cell_is_generated_from_the_manifest_type`），`cargo test -p vox-mcp` 第十五轮当时共 **83 条** = 73 条集成 + 10 条 crate 内单测——**S1 不再有未落地项** |
| `core-dev` | 落 S0 实现（`Composition` 类型 + `Plan::build` 中转 + 能力位） | 等 S0 修订稿 | **芯侧已完成**：新增 `crates/vox-core/src/composition.rs`、`crates/vox-core/src/capability.rs`（`Composition`/`CapabilitySet`/`HostFacts`/`host_ceiling`/`CapabilityReport`/`UnavailableReason::NotWired`）；`Plan::build(config, facts)` 已改成经清单中转（`crates/vox-core/src/pipeline/mod.rs`：`build` → `Composition::of` → `Plan::from`）；`Snapshot.capabilities`（`crates/vox-core/src/runtime.rs`）与 `Runtime::set_host_facts` 已就位；`Settings.control`（`ControlSettings`：`enabled`/`port`/`allow_*`/`transcript_notify_ms`）已入芯（`crates/vox-core/src/settings.rs:76`，缺省 fail-closed + `normalize()` 夹紧）；`cargo test --workspace` 全绿 |
| `shell-dev` | 外壳侧接线：`platform::host_kind()` / `platform::host_facts()` + `assemble()` 里注入一次；**Linux 虚拟麦**接进装配层（S0 §3.2 ①②）；**控制面接进 app 产品路径**（S1 装配侧） | S0 芯侧 | **已完成**（外壳侧本体，本机实测）：`app/src-tauri/src/platform/**` 的 `host_kind()`/`host_facts()` 两边同名（`mod.rs`/`win.rs`/`linux/mod.rs`），`lib.rs` 装配第 13 步 `virtual_mic_ensure()` + `set_host_facts(platform::host_facts())`，`devices.rs` 的 4 s tick 里 `refresh_host_facts()`（只在事实变了才再注入）；Linux 虚拟麦接线本体在 `platform/linux/virtual_mic.rs`，位此刻为 `true`（凭据 = 句柄真的建出来了；本机 `cargo test -p vox --lib -- --ignored virtual_mic_is_wired` 通过）。**S0 的 UI 能力位已落地**：`app/ui/src/capabilities.ts`（`hostBit`/`capabilityNote`）+ `components/Capability.tsx`，各 section（`CableManager`/`Subtitle`/`Settings`/`Vrchat`/`PipelineCard`）按位降级；`Snapshot.capabilities` 在 `app/src-tauri/src/dto.rs` 落地；`check:capabilities` 已进 `npm run verify`。**控制面已接进 app 产品路径**：`app/src-tauri/src/mcp.rs`（装配胶水：`Switch::from_settings` + `LedgerBackend::new(runtime, runtime)` + `serve`，不自己实现 `Grants`），`lib.rs` 装配第 14 步 `mcp::start`（排在 `set_host_facts` 之后）、`shutdown()` 里先停控制面再 `persist.flush()`；**app 集成测试 4 条**（`app/src-tauri/tests/mcp.rs`：产品路径上 `tools/call` 真的可用、开关关着不监听不写握手文件、未授权的位一律拒；**第十轮增至 5 条**——新增 `a_dead_handshake_is_swept_and_a_live_one_is_kept`：pid 已死的 `control.json` 起服务前被清扫、还活着的原样保留）+ 真 app curl 冒烟走通。**第十四轮复核转正**：设置页控制屏（`app/ui/src/sections/AgentControl.tsx` + `nav.ts` 的 `agent` 项 = `PAGE_NAV` **8 页**，原 7 页）与 **热切换**（`app/src-tauri/src/mcp.rs::ControlPlane::install()` 挂 `Event::SettingsChanged`）**都已落地**，app 集成用例随之由 4 → **7 条**（`app/src-tauri/tests/mcp.rs`，第十三轮补 `the_switch_hot_starts_and_stops_the_plane` / `a_quiet_start_still_sweeps_dead_credentials`）。**第十五轮转正**：`--print-composition` 已**两档落地**（桌面 `app/src-tauri/src/composition.rs`、无屏 `crates/vox-headless/src/cli.rs`，证据见本节末「第十五轮回填」）。**仍未落地**：删 `VirtualDeviceStatus`（语义已降级为"仅 Windows 安装管理"，"能不能用"只看 `virtual_mic` 位——该符号今天仍在 `app/src-tauri/src/platform/{mod,win,linux/mod}.rs`） |
| `verifier` | 复核修订稿与首批实现 | 以上各步之后 | **已完成（芯侧可验收）**：独立复核 `crates/vox-core` 实现 —— 既有测试逐字未改、差分台 10 组配置 0/10 轨迹不同（"行为不变"有证据）、`Plan` 结构未改、清单实例与实现输出逐字相等。结论：**芯侧可以验收，S0 整体尚不能验收**（UI 侧/`--print-composition`/Linux 真机三项未落地），另带 3 条星号：`Composition::of` 签名的文档漂移（D1）、缺省事实 fail-open + 旧 `virtual_cable_installed` 未拆（D2/D3）、虚拟麦缺省解析待真机实测（D9）；明细见 `agent://AuditS0Impl`。**缺陷已派修**（本轮：D1 `Composition::of` 签名统一 / D2 `virtual_cable_installed` 降级为 Windows 安装管理专用 / D3 `HostFacts::uninjected()` 缺省 fail-closed / D4 `in: []` 进 `Composition::validate()` / D6 `ops[gate]` 移出 editable 表）；原判"Linux 真机未落地"一项已转正（本机 ignored 测试通过，见 `shell-dev` 行）。（**第九轮回填补注**：该轮所列"UI 侧未落地"现已落地，"Linux 真机"已转正，只剩 `--print-composition` 仍未落地——见上面的 `shell-dev` 行；本格保留第二轮当时的结论原文。） |

**第十轮回填（2026-09-22，只回填进度与状态，不改任何结论）**

**S0：三侧落地 + 第九轮阻断项收口。** 芯侧（`crates/vox-core/src/{composition.rs,capability.rs}`）、
外壳侧（`platform::{host_kind,host_facts}` + `assemble()` 注入 + Linux 虚拟麦接线）、UI 侧
（`app/ui/src/capabilities.ts` + `components/Capability.tsx` + `check:capabilities`）都已落地；
第九轮那三条阻断项（D1 存活凭据、D2 跨线程同结论、D3 判据抽纯函数）的修复**已过独立复核**
（`agent://AuditRound9`）：复核自己重跑了真机探针
`cargo run -p vox-overlay-linux --example frame_loop_probe -- 5` —— **5 秒 152 次渲染**、
截图 `#FF0000` **4760** / `#00FF00` **6144**、`hide()` 后 **0 / 0**；4 个变异（存活凭据换回 `thread_local`、
三处定义者各改恒 `ON`）**全部把对应用例打红**，结论"**可以验收**"（三条不阻断的星号见该产物）。
**仍未落地**：`--print-composition`（`grep -rn "print.composition" --include=*.rs .` 零命中；
S0 §4.3-A / §4.4 的命令今天仍跑不了）。（**第十五轮转正**：该命令已两档落地，
`grep -rn "print-composition" --include=*.rs .` 今天有命中——见本节末「第十五轮回填」；本句保留第十轮当时的记录。）

**S1 完成度**（照 `crates/vox-mcp/**` + `app/src-tauri/src/mcp.rs` 逐文件核）：已落地 = 动作表
`actions.rs` + 协议面 `mcp/` + 本机 HTTP 传输（`transport/http.rs`：只绑 `127.0.0.1`、单路径 `/mcp`、
POST-only）+ 端点投影（`endpoints.rs`：`Settings` → 清单）+ 会话与 token（`session.rs`：handle 注册表
+ `c_` 前缀一次性 compose token）+ `Grants`（`ledger.rs` 的 `impl Grants for Runtime`）+
**控制面接进 app 产品路径**（`app/src-tauri/src/mcp.rs`；`assemble()` 最后一步 `mcp::start`，
`shutdown()` 里先停控制面再 `persist.flush()`）+ **第十轮两处安全护栏**：
① **总闸** —— `Grants::user_granted` 与 `config_write_allowed` 都是 `control.enabled && …`
（`Settings.control.enabled` 关着时一位都不开，服务万一还在跑也全拒）；
② **token ↔ 清单绑定** —— `Tokens::redeem` 要求 `pending.manifest == manifest`
（token 只对"签它时那一份清单"有效，换一份清单核销必失败）。两条都有钉子用例
（`tests/endpoints.rs` 的 `the_control_master_switch_gates_every_grant_bit` /
`a_compose_token_is_bound_to_the_manifest_it_was_signed_for`），**去掉护栏即红**（第九轮复核顺手独立观察到
前者的一次"去掉即红"：`endpoints.rs` 先红在 `config_write_allowed`、后红在 `user_granted`）。
**第十四轮收口 · S1 完成度（2026-09-22 复核，照代码逐项核过）**：上面第十轮那版列的"仍未落地"五项
**现已全部转过正**（本节按代码实情重写，不再保留已失实的清单）：
① **资源面已落地且已广告** —— `src/resources.rs` + `src/transcript.rs` + `mcp/{resources,subscriptions}.rs`
与 `subscriptions/listen` 的 SSE 长流都在，`server/discover` 的 `capabilities` 里是
`"resources": {"listChanged": true, "subscribe": true}`（`crates/vox-mcp/src/mcp/mod.rs`），两位**都真会发通知**
（钉子用例 `tests/resources.rs::every_advertised_capability_bit_has_a_real_notification` 拿返回值比对真发出去的消息）；
② **stdio 桥已落地** —— `voxctl serve-stdio`（`crates/vox-mcp/src/transport/stdio.rs`；`transport/` 下今天有
`http.rs` + `stdio.rs`，与 CLI 共用瘦客户端 `src/client.rs`，不拥有账本、不开设备）；
③ **5 个动作子命令已落地** —— `voxctl list-endpoints` / `describe-endpoint` / `compose-endpoint` /
`session-open` / `session-close`（`cargo run -p vox-mcp --bin voxctl -- --help` 自报"动作子命令（5 个：…）"，
帮助与参数由各自的 `inputSchema` 生成；`tests/voxctl.rs` 12 条钉着退出码 0/1/2/3 与逐字节一致）；
④ **设置页控制屏已落地** —— `app/ui/src/sections/AgentControl.tsx`（`App.tsx` 的 `PAGES.agent`）+ `nav.ts` 的
`agent` 项：`PAGE_NAV` 第十四轮当时 **8 页**（第十轮当时是 7 页）；`check:agent`（`app/ui/scripts/check-agent.mjs`）
已进 `npm run verify` 链；
⑤ **热切换已落地** —— `app/src-tauri/src/mcp.rs::ControlPlane` 把起停挂在 `Event::SettingsChanged` 上
（`install()` 装监听器、`reconcile()` 按新开关起/停，换端口即重绑并擦掉旧的 `control.json`），
真 app 实测：拨开关 → `control.json` 出现/消失 + 新端口新 token；app 集成用例
`the_switch_hot_starts_and_stops_the_plane` 钉着。（**授权不受此限**：`Grants` 每次 `tools/call` 现读，
总闸一关下一次调用立刻全拒。）

**无屏档 `vox-headless` 已落地**（第十四轮补记）：`crates/vox-headless/` 是与 `app/src-tauri`
并列的第二个外壳、**零 Tauri**（入口 `src/main.rs`，模块 `cli.rs` / `config.rs` / `dsp.rs` / `headless.rs` /
`mcp.rs` / `persist.rs` / `secrets.rs` / `status.rs` + `platform/` `sys/`）；
它的清单三格不再手写，由芯的 `HostKind::shell()` 按档位派生 = `life: daemon` / `ui: none` /
`control: ["mcp","cli","config_file"]`（`crates/vox-core/src/composition.rs` 的 `LinuxHeadless` 分支 +
`HEADLESS_CONTROL`）；CLI 面 `vox-headless [--config <settings.json>] [--print-capabilities | --dry-run]`
（另有一直跑的 `--start` / `--run-for`），控制面复用同一份 `serve`（`src/mcp.rs` 注入 `LedgerBackend`）。

**第十五轮回填（2026-09-22，只回填进度与计数，不改任何结论）**

1. **`--print-composition` 两档都落地**（S0 §4.3-A 的验收出口，两档**同形**）：
   - 无屏档：`crates/vox-headless/src/cli.rs` 的 `--print-composition`（`Mode::PrintComposition`）
     → `crates/vox-headless/src/status.rs::composition_json`；
   - 桌面档：`app/src-tauri/src/composition.rs`（`FLAG = "--print-composition"`，`requested()` / `print_and_exit()`；
     排在 Tauri 之前、只 `Builder::build()` 不 `run()`，不建窗口 / 不注册命令 / 不起托盘热键）；
   - 两边**同一个派生**：`vox_mcp::endpoints::manifest`（`crates/vox-mcp/src/endpoints.rs`）——
     **与 S1 的 `describe_endpoint` 是同一个函数**，所以这份打印与 Agent 面看到的是同一份清单；
     清单走 `wire` 文本往返一趟，与线上形态逐字同形；退出码 `0` 成功 / `2` 打不出来。
2. **无屏档交付补齐**：systemd 两份 unit（`crates/vox-headless/systemd/user/vox-headless.service`
   与 `.../system/vox-headless.service`）+ 样例配置 `crates/vox-headless/settings.example.json`
   + `crates/vox-headless/README.md` + 可复现打包脚本 `tools/package-headless.sh`
   （tar 的排序 / 时间戳 / 属主钉死、二进制 `--locked` 编；产物落 `tools/bundle/`，已 gitignore）。
3. **`composition_schema!` 已接真 schemars**：`crates/vox-mcp/build.rs` 用
   `schema_for!(vox_core::composition::Composition)` 生成并把 `$ref` 重定域到本 schema 内，
   `crates/vox-mcp/src/actions.rs::composition_schema!` 用 `include_str!` 读成字面量塞进 `concat!`；
   `vox-mcp` 的 feature `json-schema` **默认开**（转发芯的 feature），`--no-default-features` 时
   build 脚本什么都不写、宏走**如实占位**（不是假 schema）。
4. **无屏档 `background_service` 仍如实 `not_wired`**：unit 与打包这一轮落地了，但这一位**还没有检测者**
   （问 systemd 要状态得走 D-Bus），`crates/vox-headless/src/platform/linux.rs` 照实报
   `false(not_wired)`——**不拿"unit 文件存在"当凭据**；`crates/vox-headless/README.md` §5
   "还没做（不许广告）"与 `crates/vox-headless/src/status.rs` 的用例钉着这一条。
5. **计数（2026-09-22 第二十三轮收口实测）**：`cargo test --workspace` = **601 passed / 0 failed / 5 ignored**
   （第十五轮当时是 570、第十四轮当时是 561、第十轮当时是 477）；
   `cargo test -p vox-mcp -- --list | grep -c ': test$'` = **87**（= 76 条集成 + 11 条 crate 内单测；
   第十五轮当时是 83；其中 `tests/protocol.rs` 15 条）。
6. **S3 帧层已落地（第二十三轮）**：`crates/vox-net/src/media/{mod,frame,pipe,server,client}.rs`
   已落地（13 条 `tests/media_pipe.rs` + `examples/media_probe.rs`），`CaptureTarget::Net { pipe }`
   （`crates/vox-core/src/ports.rs`）已加并迁移四处穷尽 `match`；**S3 后半未做**——无屏档端到端接媒体管子
   （`Plan::build` 对 `Input::NetIn` 仍如实报错）与 `NetIn`/`NetOut` 能力位定义者（仍不在 `host_ceiling` 里）。

**闸门与门（第十轮）：** 第十轮当时五个自查脚本（`a11y` / `qa:narrow` / `check:cable` / `check:capabilities` /
`qa:home`；第十三轮起的 `check:agent` 也走同一份 `preview.mjs`）的"起一个**自己的** vite preview"
统一到 `app/ui/scripts/preview.mjs`：端口由内核分配
（`listen(0)`）+ 起服务**前**再确认端口空 + 直接跑 `vite.js` 并带 `--strictPort` + 就绪后真 HTTP 取根 +
就绪后**再**确认子进程还活着（"端口通了"到"能开跑"之间那条 TOCTOU 缝）与 `expectOwnBuild` 逐字节比对
入口脚本（产物同一性）；新增 `check:preview`（`app/ui/scripts/check-preview.mjs`）**自查这道闸本身**
（端口被占必须失败 / 产物不是我们的必须认出来 / 反向对照能起能停），已进 `npm run verify` 链
（`app/ui/package.json`）。另：`app/src-tauri/src/mcp.rs` 起控制面前先清扫"pid 已死"的旧 `control.json`
（`prune_dead_credentials`，判据只有"文件里的 pid 已经不在了"这一条；读不出来 / pid 还活着一律不动手）。
工作区已跑 `cargo fmt --all`（清掉此前 38 处 fmt 漂移，`cargo fmt --all -- --check` 零输出）；
`cargo test --workspace` = **601 passed / 0 failed / 5 ignored**（第二十三轮收口 2026-09-22 实测；
第十五轮当时是 570、第十四轮当时是 561、第十轮当时是 477）、
clippy **0 warning**、`npm run verify` 全绿——链见 `app/ui/package.json` 的 `verify`
（写稿当时 9 步，以脚本为准）：
`build` → `check:classes` → `check:preview` → `a11y` → `qa:narrow` → `check:cable` → `check:capabilities`
→ **`check:agent`**（第十三轮新增，查设置页控制屏那一页）→ `qa:home`。

**已知的、有意不做的（第十轮记下，别再当缺陷重报）**

| 事项 | 不做的是什么 / 为什么有意 | 依据 |
| --- | --- | --- |
| `mic` 位**按档位上限报开**（麦克风被独占 / 没授权时仍报 `true`） | 它唯一可能的定义者在**会话期**——开会话时才 `CaptureSource::start()` 起流，装配期拿不到任何否定证据；代价 = 界面**不会提前**把麦克风那一块灰掉，失败推迟到用户按"对外说话"那一刻由 `PortError` → `Notice` 兜底。要收口只能把定义者挪到"起流结果"（那时 `mic` 变成会话期事实，与其余装配期事实不同类），**S0 不做**；**新增位不许照抄这一条** | `docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.4（`mic` 那一行 + 第四轮 F1 三条 + §5.1-13 / §5.3 第 2 行）；钉子用例 `platform::tests::mic_is_reported_on_the_ceiling_until_there_is_a_definer` |
| `platform::tests::captions_bit_is_the_same_answer_on_the_poll_thread` **抓不到**第七轮那次回归（别把它当护栏） | 测试进程里没有真窗，主线程与轮询线程都读到 `false`，把探针改回 `thread_local` 它**仍然全绿**（第九轮复核 M1 实测）。它钉的只是"`captions_status` 里不许按线程分支"；那次回归真正的钉子是**进程级凭据用例** `vox_overlay_linux::window::tests::running_is_a_process_wide_fact` + **真机探针** `crates/vox-overlay-linux/examples/frame_loop_probe.rs`。**不为它加"真窗"单测**（真窗在单测里造不出来） | `agent://AuditRound9` 缺陷表第 1 条；`docs/plans/S0-COMPOSITION-MANIFEST.md` §2.5.4 末"一条钉子抓不到的东西" |
| `voxctl serve` 起的服务**仍 `backend = None`** | 账本与设备只在装配层注入——`voxctl` 不拥有账本：CLI 起这份是协议面的第二个入口，`server/discover` / `tools/list` 直接可用，`tools/call` 如实回 `-32603`"后端未接入"。**产品路径不靠它**：app 装配时注入 `LedgerBackend`（`app/src-tauri/src/mcp.rs`） | `crates/vox-mcp/src/bin/voxctl.rs`（`http::serve(options, None)`）；`crates/vox-mcp/src/transport/http.rs` 的 `pub fn serve`（写稿当时的行号 `:153-155` 已漂移，实为 `:204`） |

**顺带（与本轮分工同批）**：DSP 算子成本基准工程已收编进仓库 —— `tools/bench-dsp/`，独立 crate（自带 `[workspace]`、不在根 workspace 成员里），只读依赖 `crates/vox-dsp` 与 `crates/vox-core`。

**环境阻塞（S2 手机壳 / Android）**：S2 需要 **Android NDK + `cargo-ndk` + `aarch64-linux-android`、`x86_64-linux-android` 两个 rustc target**。
本机现状（2026-09-22 实测）：**SDK 在**（`~/Android/Sdk`，`ANDROID_HOME`/`ANDROID_SDK_ROOT` 已指向它）、**`adb` 在 PATH 里**（`/usr/bin/adb`）、**JDK 17 在**（`/usr/lib/jvm/java-17-openjdk-amd64`，`java -version` = 17.0.20）；
但**没有 NDK**（`~/Android/Sdk/ndk` 不存在）、**没有 android rust target**（`rustup target list --installed` 只有 `x86_64-{unknown-linux-gnu,pc-windows-msvc,pc-windows-gnu}`）、**没有 `cargo-ndk`**（`which cargo-ndk` 零命中）。
装 NDK 是 **GB 级下载 + 系统变更**（装 SDK 组件、加 rustup target、装 `cargo-ndk`）→ **需用户明确同意**才动。
**未装之前，S2 只能做不依赖 SDK 的部分——目前为零**：`crates/vox-audio-android/`（手机音频外壳）、`app/android/`（Tauri Android 工程）与任何实机验证，每一步都要先过 NDK 这一关。
平台侧结论（能做什么 / 做不到什么 / 怎么落地）见 `docs/platform/ANDROID.md`。

### 10.8 收尾拍板（2026-09-22 会话结束）

| # | 事项 | 拍板 | 备注 |
| --- | --- | --- | --- |
| 1 | Android NDK 装不装 | **装，但下次会话执行**（本次不装） | 本机现状：`~/Android/Sdk`、`adb`、JDK 17 都在；缺 **NDK** + `aarch64-linux-android` / `x86_64-linux-android` target + `cargo-ndk`。装法：`sdkmanager --install "ndk;<版本>"` + `rustup target add …` + `cargo install cargo-ndk`。装完 S2 才能真开工（Tauri v2 Android 外壳 + Oboe + 前台服务，编译级可验） |
| 2 | 下一步优先哪条 | **下次再看** | 候选：S3 后半（清单→作业单打通 / 无屏档网络音频入口）· 手机 S2（依赖 #1）· 插件宿主 · 先清 6 条复核星号 |
| 3 | 提交策略 | **每轮收口可提交并推送**（已获授权） | 见 `.omp/RULES.md` #1 的修订；分组提交（core / mcp / headless / net / app / docs+tools） |

> **2026-09-26 补注**：第 2 条"下一步优先哪条"已由 §10.9 答复（嵌入式优先 + S4 重构）；第 1 条（装 NDK）随手机顺延，不再是下一步。

**本次收尾的基线**：HEAD = `0b0c555`（已推送 `origin/master`），工作区干净；
`cargo test --workspace` = **601 passed / 0 failed / 5 ignored**、`cargo clippy --workspace --all-targets` 0 warning、`cargo fmt --all --check` 干净。

**下一轮的入口**：`.omp/agents/` 六个角色 + `.omp/RULES.md`；施工单 `docs/plans/{S0-COMPOSITION-MANIFEST,S1-AGENT-FACE,S3-NET-AUDIO}.md`；进度台账 §10.7。

### 10.9 嵌入式优先与重构核心（2026-09-26，**已拍板**）

> 来源：2026-09-26 会话（Claude Code），用户逐条回应我对嵌入式方向的评审。施工设计稿：
> [`docs/plans/S4-EMBEDDED-REFACTOR.md`](../plans/S4-EMBEDDED-REFACTOR.md)。

**用户口径（原话摘要）**：硬件"属于组合式，有什么就用什么"；回声消除"对哦，怎么办"；PipeWire"有点重了，
可能是目前没有想的那么轻，还在手机等产品上"；局域网控制"这方面是没考虑到"；重构"没有抓到重构这个核心，
还是一直在修修改改"；性能"目前测速不了，我是建议能直接做到嵌入式的，而不是手机"。

| # | 事项 | 拍板 | 落在 |
| --- | --- | --- | --- |
| 1 | 设备顺序 | **直接做嵌入式**（小主板 ARM64 Linux），手机 S2 顺延到 S4-A 之后 | §8 第 15 行 |
| 2 | 嵌入式的产品形态 | **组合式**：一份二进制，插了什么硬件就报什么能力位、按位装配（网络中继 / 本地麦 + 喇叭的翻译盒子都是它） | S4 §2.1 |
| 3 | 重构核心 | ① **共享宿主层 `vox-host`**：装配层只写一份，桌面/无屏/手机都是薄入口；② **算子链**：流水线按清单执行，不再两条写死 | S4-A / S4-B；§8 第 16 行 |
| 4 | 回声消除 | **三档按位自动选**：硬件 AEC > 软件 AEC（同声卡，参考信号取自我们自己的播放）> 半双工门（保底） | S4 §2.2 |
| 5 | 音频后端 | **ALSA 为嵌入式缺省**，PipeWire 可选 | S4 §2.3；§8 第 17 行 |
| 6 | 控制面 | 无屏档可开**局域网监听 + 配对码/token 鉴权**，缺省仍本机 | S4 §2.4；§8 第 18 行 |
| 7 | 没有板子怎么办 | 不写"够用"结论；交付**算子自计时** + **CI 交叉编译 ARM64 + qemu-user 跑测试**；设计预算按 Cortex-A53 四核 1 GHz / 512 MB | S4 §2.5 |

**执行顺序**：S4-A（`vox-host`，行为不变）→ S4-B（算子链，行为不变）→ S4-C（ALSA / 半双工 / AEC / 廉价重采样 /
局域网控制面 / 自计时 / CI 交叉编译，逐个插）→ S4-D（deb）。各阶段开工前由 architect 细化 S4 稿。

**对旧文档的影响**：`docs/platform/EMBEDDED.md` 里"复用 PipeWire 作唯一音频""端点形态 = `net_in → net_out`"
"控制面只在本机"三处按本节读（该文件顶部已加状态注）；§10.0 第 1 条、§10.8 第 2 条已加补注。

**本节写成时的基线**：HEAD = `36813d3`；本轮环境没有 Rust 工具链，**未跑** `cargo test` / `clippy`。

---

## 参考与关联

- 决策与理由：`docs/architecture/DECISIONS.md`（A 已拍板 / B 待拍板 / C 后端契约）。
- 平台范围与调研沉淀：`docs/platform/SCOPE.md`；Linux 执行与实测：`docs/platform/LINUX.md`。
- 架构分层（现状描述）：`docs/architecture/ARCHITECTURE.md`。
- 2026-09-21 那轮讨论的完整原文：omp 会话 `01a0c29c-befd-70c1-b21d-e297d1c481f4`
  （`~/.omp/agent/sessions/-Vox/2026-09-21T06-17-27-549Z_…jsonl`），`omp -r 01a0c29c` 可续。
