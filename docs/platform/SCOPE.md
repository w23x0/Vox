# 平台范围决策

> 口径与 `docs/architecture/DECISIONS.md` 一致：**代码与本文件打架时以代码为准**，然后回头把这里改对。
>
> 本文件只回答一个问题：**Vox 跑在哪些平台、什么时候做。**
> 技术细节（音频/悬浮窗/热键的跨平台实现路径）也记在这里，因为它们直接决定"什么时候做"的答案。
>
> 三部分：
> **A. 已拍板** —— 定了的，别再翻。
> **B. 暂缓的依据** —— 为什么现在不做跨平台，以及哪些坑会等你。
> **C. 未来起点** —— 哪天捡起 Linux 时第一个该回答的问题和现成的调研沉淀。

---

## A. 已拍板

> ⚠️ **2026-09-20 变更**：下面第 1、2 条（"只做 Windows"、"跨平台延后不设时间表"）
> **已被推翻**。Linux 端已开工，锚定范围、实施步骤与实测证据见 `docs/platform/LINUX.md`
> 和 `docs/architecture/DECISIONS.md` A15。本节其余内容（尤其是 §B 的成本分析、§C 的调研沉淀）仍然有效，
> 只是 §C1 的"通用 Linux"目标已正式下调为"主流现代发行版默认配置（PipeWire 必需）"。

1. **目前只做 Windows，主线是打磨 Win。** Win 已能用、用户已在那；
   下一个小时投在 Win 的下一个功能/体验上回报最高。这是当前**唯一**主线。
2. **跨平台延后，不设时间表。** 不为了"覆盖更多用户"在当下分流精力。
   等单端立住、有余力，再回头评估。
3. **不换框架（不引入 Wails）。** 即便为了"更好跨平台"也不换。
   理由见 B3——Wails 跨平台省的只是后端语言层，本项目的跨平台成本全在
   原生 API 层（音频/UI/热键/密钥），两个框架都不帮忙省那一层；而换 Wails
   还要额外把现有 Rust 后端全部重写一遍，纯亏。
4. **现有架构本就为跨平台留路，这不是事后补票。** `vox-core` 平台无关
   （不 `use windows`、不碰 Tauri）、`ports.rs` 用 trait 抽象外壳能力
   （采集/播放/热键/字幕/密钥/时钟）、`vox-dsp` 与 `vox-net` 纯 Rust 零平台依赖。
   `docs/architecture/ARCHITECTURE.md` 第 2 条原话："想搬到 macOS/Linux，只需要重写外壳那几个 crate。"
   这是开工前设计文档里就定的，不是事后补救。
   ——记这条是为了防止以后误以为"跨平台是历史欠债"，从而冲动重写。

---

## B. 暂缓的依据

### B1. 三端工作量不是平均的 —— Linux 这端 ≈ Win + macOS

这是决定"暂缓"的核心事实。把三端摊开：

| 平台 | 路径 | 岔路 | 难度 |
| --- | --- | --- | --- |
| **Windows** | 单一路径，**已实现**（WASAPI 音频 / Win32 UI / DPAPI 密钥 / NSIS 分发） | 无 | 已跑通 |
| **macOS** | 单一路径，未实现（CoreAudio / AppKit / Keychain / dmg+公证） | 无 | 中等，无岔路 |
| **Linux** | **同一条路里还藏两个岔路**（桌面环境 × 音频后端） | **多个** | ≈ Win + macOS，甚至更多 |

"三端可用"听起来是 3 倍工作量，实际会是 5～6 倍，而且 **Linux 投入产出比最低**
（用户少、适配组合多）。

### B2. 维护成本比首写更可怕 —— 一人扛不动三端

- 一次功能改动 → 改三处 + 三套测试 + 三个平台各自的 bug。
- Linux 那端会长期背着"在别人机器上复现不了"的发行版/桌面环境差异问题。
- **本项目是自维护**，没有专人分头啃三端、没有测试矩阵。
- 每个小时投在 Linux 适配上的时间，都是从已能用、已在攒口碑的 Win 上抽走的。

### B3. Wails 不解决本项目的跨平台难题（驳"换框架更好跨平台"）

把"跨平台"拆层看，难点在哪一层一目了然：

| 层 | Wails (Go) | Tauri (Rust) | 跨平台省不省事 |
| --- | --- | --- | --- |
| 后端语言本身 | Go 编译目标多，`GOOS=linux` 一条命令 | `rustup target add` 一条命令 | **两者都省，差距很小** |
| 前端 | 一样 React/WebView | 一样 React/WebView | **完全一样** |
| **原生能力**（音频/UI/热键/密钥） | **照样自己写三套** | 自己写三套 | **这才是真成本，两框架都不帮你省** |

本项目的跨平台难度全在最后一行 —— WASAPI/PipeWire/CoreAudio、Win32/Wayland layer-shell、
DPAPI/Keychain、按进程环回 —— 这些是**操作系统 API 的差异**，跟后端用 Go 还是 Rust
没有关系。Wails 给的是"写一次 Go 能编译到三个系统"，但那段 Go 里调的还是三套不同的
原生 API，一行没帮你省。换 Wails 还要额外把现有精心设计的 Rust 架构（单账本、会话序号
防并发、`ports.rs` trait 抽象）用 Go 重写一遍，Go 没有等价物兜这些。**纯亏。**

---

## C. 未来起点（哪天捡起 Linux 时从这里开始）

> ⚠️ **2026-09-21 更新**：本节里关于 **macOS 的路线与决策点全部作废**（用户拍板：macOS 不做，
> 含 iOS / iPadOS / visionOS），保留作为调研沉淀。平台形态与"每类平台一般怎么做"的汇总
> 已搬到 `docs/architecture/DIRECTIONS.md` §2.5。§C 其余内容（Linux 岔路、crate 映射、虚拟麦）仍然有效。

> 这一节是调研沉淀，**不是行动项**。它存在的意义是：以后想做时，不用从零再调研一遍，
> 也不用重新踩同一批坑。读了这一节就知道第一个该拍什么、剩下的岔路怎么收敛。

### C1. ⭐ 上游决策：Linux 锚定范围 —— 捡起来时第一个回答的问题

**这条会一次性收敛下面所有岔路，所以必须先答。**

已拍板方向：**通用 Linux**（含老发行版 / X11 / ALSA，N×M 组合全适配）。
这是最贵的选项，也是当初决定"暂缓"的重要原因之一。捡起来时要在"投入"和"覆盖面"
之间重新权衡，下表是三档可选范围：

| 锚定范围 | 桌面/音频适配工作 | 能覆盖的用户 |
| --- | --- | --- |
| 只锚定某一发行版最新版 | 收敛成一条路（如 Wayland(KDE)+PipeWire） | 该发行版用户 |
| 主流发行版默认配置 | 几条路（KDE/GNOME on Wayland + PipeWire） | 大部分现代 Linux 用户 |
| **通用 Linux（含老发行版/X11/ALSA）** | **岔路全开，N×M 组合** | 所有 Linux 用户，但工作量爆炸 |

> 注：当初最初提的目标环境是 openSUSE Tumbleweed（默认 KDE on Wayland + PipeWire），
> 若那时只锚定它，两个岔路本可自动收敛。**最终选了通用 Linux，等于主动背上全部组合** ——
> 这是暂缓决策的核心理由之一。捡起来时可重新评估是否下调范围。

### C2. 整体依赖图

```
目标：Win / Linux / macOS  x64 可用版（通用 Linux）
│
├─ Windows ✅（单一路径，已实现）
│   音频→WASAPI   UI→Win32   密钥→DPAPI   分发→NSIS
│
├─ macOS ❌（2026-09-21 拍板：**不做**，连 iOS / visionOS 一起；
│        下面关于 macOS 的调研仅作沉淀，不再当待办）
│   音频→CoreAudio  UI→AppKit/NSWindow  密钥→Keychain  分发→dmg+公证
│   难点：进程级抓取（需 tap）+ Hardened Runtime 公证
│
└─ Linux 🔴（岔路在这 —— "Linux 可用"的真问题）
    │
    ├─ 桌面环境岔路 ─┬─→ 卡住「悬浮窗」（先试 Tauri 自带透明窗）
    │               └─→ 卡住「全局热键」
    │
    └─ 音频后端岔路 ─┬─→ 卡住「麦克风采集」「播放」
                    ├─→ 卡住「按进程环回」（最难）
                    └─→ 卡住「虚拟麦克风」
```

Win / macOS 只要"每端各写一套实现"；Linux 要"先回答两个岔路，再写实现"。

### C3. Linux 岔路 A：桌面环境（X11 vs Wayland）

直接决定悬浮窗（透明 + 鼠标穿透 + 置顶）和全局热键能不能做。

| 分支 | 悬浮窗透明穿透 | 全局热键 | 状态 |
| --- | --- | --- | --- |
| **X11** | 能做（透明/置顶/穿透齐全） | 能做（全局 grab） | 在被淘汰，但兼容性最好 |
| **Wayland / wlroots 系**（Sway/Hyprland 等） | 能做（layer-shell） | 受限 | 协议支持最好 |
| **Wayland / KDE Plasma** | 部分（layer-shell 支持不完整） | 受限 | openSUSE 默认桌面之一 |
| **Wayland / GNOME** | 基本不行（拒收 layer-shell，靠扩展） | 受限 | 最封闭一档 |

传导链：悬浮窗透明穿透 → 能不能用 Tauri 自带透明窗 → **取决于目标桌面环境认不认
Wayland 的透明+置顶+穿透协议**。GNOME on Wayland 大概率逼你放弃 Tauri、退到 winit，
甚至部分功能做不了。

### C4. Linux 岔路 B：音频后端（PipeWire / PulseAudio / ALSA）

决定音频四个功能的上限，尤其"按进程环回"。

| 分支 | 麦克风/播放 | 按进程环回 | 虚拟麦克风 |
| --- | --- | --- | --- |
| **PipeWire** | ✅ | ✅ 能按 node 过滤目标进程 | ✅ 原生虚拟 sink |
| **PulseAudio** | ✅ | ⚠️ 只能整机环回或靠 pavucontrol per-app 监听 | ✅ 虚拟 sink |
| **ALSA 直连** | ✅ | ❌ 没有进程级概念，做不了 | ❌ 无优雅方案 |

传导链："听人说话按进程抓音" → **取决于音频后端是不是 PipeWire**。
PulseAudio / ALSA 下只能降级整机环回或做不了。

### C5. 移植难度排序（从难到易）

1. **音频：按进程环回** —— Linux 上明显比 Win 难（PipeWire 能做但要碰底层）。
2. **悬浮窗透明+穿透** —— 前端唯一真难点（见 C6）。
3. **音频：麦克风采集 + 播放** —— PipeWire/PulseAudio 成熟，不难。
4. **全局热键** —— Wayland 下受限，可能降级成窗口快捷键。
5. **虚拟麦克风** —— Linux 上反而比 Win 省事（见 C7）。
6. **主界面前端**（React）—— 不用动。
7. **内核 / 网络 / DSP**（`vox-core`/`vox-net`/`vox-dsp`）—— 不用动。

### C6. 悬浮窗跨平台选型（调研结论，不是行动项）

优先级：

1. **先试 Tauri 开第二个透明窗**（Linux 后端 WebKitGTK）—— 最省事，复用现有 Tauri，
   大概率够用。先验证 WebKitGTK 下透明 / 穿透 / 置顶到底行不行。
2. 不行，退 **`winit` + `softbuffer`**（CPU 自绘，字幕这点像素够）或
   **`winit` + `wgpu`/`tiny-skia`**（要 GPU 合成再上）。`winit` 是跨平台窗口库事实标准，
   平台覆盖最广。
3. **不选 GPUI**（Zed 的框架）：它是为"做应用"写的，不优先暴露透明/穿透/置顶这些
   底层窗口语义；Linux/Wayland 是它最弱的后端；API 跟着 Zed 走，无稳定性。
   且本项目主界面是 Tauri+React，引 GPUI 等于塞第二套渲染栈，复杂度翻倍。

> 一个易被忽略的点：Win 版不用 WebView2 是因为"Win32 上透明会留实心方块" ——
> **这是 Windows WebView2 的特定缺陷，WebKitGTK 不一定有**。所以 Linux 上先试 Tauri
> 透明窗是有现实依据的，不是盲目乐观。

### C7. 虚拟麦克风：Linux 反而比 Win 省事

- **不用安装任何东西** —— Win 上 `cable.rs` 那套下载 + UAC + 静默安装 +
  `ProductDisclosure` 捐赠凭证，Linux 上**整段删掉**。PipeWire/PulseAudio 原生就能
  创建虚拟设备，调一次 API 或一条 `pw-cli`/`pactl` 命令的事。
- **概念还在，形态变了**：标准做法是建一个 PipeWire 虚拟 sink → Vox 把翻译
  语音写进它 → 它自动带 monitor source → 目标软件（VRChat/Discord）在录音设置里
  选这个 monitor 当麦克风。
- **唯一保留的用户引导**：目标软件仍要手动选那个虚拟设备 —— 这跟 Win 上选 VB-CABLE
  是同一个动作，体验一致。
- ⚠️ **没有"按进程自动注入"**：想偷偷塞音频进某程序的麦克风而不让它去设置里选，
  Linux 上同样做不到（也没必要做）。

### C8. 要新增的 crate（捡起来时的工作清单）

对应三个 `-win` crate 各写一个 Linux 兄弟，外加装配层切换：

| 现有（Win） | 新增（Linux） | 实现端口 |
| --- | --- | --- |
| `vox-audio-win` | `vox-audio-linux` | `CaptureSource` / `PlaybackSink` / `DeviceRegistry` |
| `vox-input-win` | `vox-input-linux` | `HotkeyHost` |
| `vox-overlay-win` | `vox-overlay-linux` | `SubtitleView` |
| `app/src-tauri`（装配） | 加 `#[cfg(target_os)]` 条件依赖 + 分支 | 注入对应平台实现 |

装配层还要换的 Win 专属件：

- `sys/secrets.rs`（DPAPI）→ Linux 用 **`keyring` crate**（Secret Service）或文件 + `age` 加密。
- `winminmax.rs`（WM_GETMINMAXINFO 最小尺寸强制）→ 删或换 winit 的 `set_min_inner_size`。
- `tauri.conf.json` 的 `bundle.targets` 从 `["nsis"]` 加上 `deb`/`appimage`/`rpm`。
- 托盘 / 单实例 / 自启 / updater 这些 Tauri 插件本身跨平台，配置微调即可。

### C9. 其它维度（暂都不做，记一笔免得重复讨论）

- **ARM64**：延后，待内核逻辑优化后再评估。现在锁 `x86_64-pc-windows-msvc`。
- **系统版本下限（Linux 内部）**：对应 Win 的 build ≥20348（`osver.rs`）。通用 Linux
  路线下要定最低内核/glibc 版本，捡起来时再定。
- **打包格式取舍**（deb/rpm/AppImage/Flatpak/Snap）：影响装机门槛和自动更新怎么走，
  捡起来时再定。

---

## 参考与关联

- 架构与分层原则：`docs/architecture/ARCHITECTURE.md` §2（轻内核 + Windows 外壳）、§5（外壳 crate 职责）。
- 已拍板记录：`docs/architecture/DECISIONS.md`（口径与本文件一致；本文件只管"平台范围"）。
- 进程环回的版本门槛（Win 侧已有决策）：`docs/architecture/DECISIONS.md` B2、`crates/vox-audio-win/src/osver.rs`。
- VB-CABLE 现状（Win）：`docs/architecture/DECISIONS.md` A4、`crates/vox-audio-win/src/cable.rs`。
