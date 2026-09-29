# VRChat OSC 对外说话增强（预研方案 · 未拍板）

> **状态：探索性方案文档，不是任何拍板结果。**
> **代码已落地（2026-09-21）**：`crates/vox-osc/` + `osc_start` / `osc_update` / `osc_stop` /
> `osc_send_chatbox` / `osc_set_avatar` 五个命令 + `app/ui/src/sections/Vrchat.tsx` 已在。
> 但**方案本身仍未拍板**，下方 §5 的决策点与 §6「预想，没动代码」的措辞已过期 ——
> 现状以 [`docs/architecture/DIRECTIONS.md`](../architecture/DIRECTIONS.md) §7 为准。
> 本文把「在 VRChat 里，用 VRChat 官方 **OSC（OpenSoundControl）** 协议，把 Vox 的
> **对外说话**（译文语音 + 字幕）同步进 VRChat」的方向、能力边界、工程代价、卡住的决策点全部摊开。
> **所有带 ⚠️ 的条目都待你确认。** 拍板结果写进 [`docs/architecture/DECISIONS.md`](../architecture/DECISIONS.md) B 区，
> 代码落地后以代码为准。
>
> 定位：**外部模块**（参考 `CONTRIBUTING.md` 的扩展点范式与 `docs/protocols/DISCORD_PROTOCOL.md`），
> **不碰**麦克风 / VB-CABLE / 环回 / 协议内部。
> 姊妹模块：现有 `vox-window-ocr-win`（OCR）是「**读** VRChat 输入框」，本模块是「**写** 进 VRChat」。

---

## 1. 为什么现在要做

对外说话当前已经把**译文语音**灌进了 VB-CABLE 虚拟麦克风——VRChat 里把麦克风选成
`CABLE Input`，朋友们就能听到你的译文语音。这一路**不需要任何 VRChat 专属代码**就能跑。

缺的是「让 VRChat **显示**你正在说什么」这半:

| 想要的效果 | 现状 | 靠什么补上 |
| --- | --- | --- |
| 朋友们听到你的译文语音 | ✔ 已有（VB-CABLE） | 免额外做 |
| 朋友们在世界里**看到**你说的译文文字 | ✘ 要新做 | **VRChat OSC → ChatBox 文本** |
| 你自己 / 别人从你的**虚拟形象**上看出「正在翻译」 | ✘ 要新做 | **VRChat OSC → `/avatar/parameters/*`** |

目标一致的现成工具是 **VRCOSC / OSC 外挂**，但它们要么是独立程序、要么是 mod，
要么与翻译流水线脱钩。Vox 本来就**已经产出译文字幕**，只是没接上 VRChat 的
「写」通道——把这一步做成本地外部模块，是顺着现有边缘扩展，不重写核心。

⚠️ **没拍板过**：要不要做到 ChatBox 文本这一档？还是只要虚拟形象指示（轻量）？
这个分叉决定整个模块的引擎面积。见 D1。

---

## 2. 核心形态（当前理解 · 非定论）

```
  你的麦克风 → RNNoise → 翻译 → 译文字幕(SubtitleDelta, track=speak)
                                          │            │
                    VB-CABLE → VRChat 话筒      │            │  新增：一个 OSC 网(Client)
                    （已有，不动）                    ▼            ▼
                                          ┌───────────────────────╮
                                          │ vox-osc (新 crate)       │
                                          │  UDP 9000  → VRChat       │
                                          │   · /chatbox/input        │ ← 译文文字进 ChatBox
                                          │   · /avatar/parameters/*  │ ← 开麦/翻译状态指示灯
                                          └───────────────────────┘
```

要点：

- **只写不读（第一版）**：Vox 单方面把状态/文本发进 VRChat。不需要收 VRChat 的
  9001 反馈来驱动任何核心逻辑。⚠️ 是否要「收」由 D2 定。
- **开关**：在**主界面的一个独立 VRChat 页**（仿「OCR」页）里开关整个 OSC 模块；
  开 / 关、以及 ChatBox 文本、指示灯是否各自分控，见 D1/D4。
- **不碰已有链路**：对外说话的 VB-CABLE 语音、RNNoise、16k 约定、单事件通道，全不动。
- **识别 VRChat 是否开了 OSC**：VRChat 的 OSC **默认关**。用户得先在 VRC 设置里
  「OSC → Enable OSC + Allow Trusted OSC」打开。这边 UI 要给一句检测/引导（见 §5 体验）。

---

## 3. 能力边界（把「能做到/做不到」分开）

| 能力 | 走 OSC | 归属 |
| --- | --- | --- |
| 译文文字显示在世界 ChatBox | ✔ `/chatbox/input`（144 字符上限） | ChatBox |
| 一个可被虚拟形象读取的「在翻译」开关 | ✔ `/avatar/parameters/<名>` | Avatar param |
| 通知对方「正在输入/正在翻」 | ✔ `/chatbox/typing`（可选） | ChatBox |
| 替你在 VRC 里**说话/发声** | ✘ | 不行 |
| 让别人直接听到译文语音 | ✘（**这本来就是 VB-CABLE 的事，不归 OSC**） | 已有 |
| 从 VRC 读回「我切了频道/角色」来控制行为 | 🔘 下行再多读一条就行，第一版可选 | 待 D2 |

**一句话边界**：OSC 管「**显示**」层面——让你的译文出现在 VRC 的视野里；「**播放**」层面
永远留给 VB-CABLE（已有）。

---

## 4. VRChat OSC 协议现状（要先立的事实）

> 以下是我掌握的 VRChat OSC 已知事实。**落地前要真机各通各一次验证**（D5），
> 尤其 `friendlyName` 白名单机制细节——那是「为什么用户得先手动开一次」的根源。

- **数据面**：OSC over UDP。外部程序往本地 `127.0.0.1:9000` 发，VRC 往 `9001` 回（可在
  `%USERPROFILE%\AppData\LocalLow\VRChat\VRChat\config.json` 里改收端口）。
- **关未知信**：VRC 默认**不信任**任何 OSC 发送方。用户需在 VRC 菜单
  Settings → OSC → **Enable OSC** 并勾 **Allow Trusted OSC**，且 `config.json` 里
  `oscPort`/`friendlyName` 对得上，才会接受。
- **ChatBox**：发到 `/chatbox` 变体，参数是「一段文本 + 一个 bool」。首个参数为显示文本
  （≤144 字，`\n` 换行、`<nop>` 可用），第二个 bool 控制「不要刷屏通知到社交」。
  `/chatbox` 变体还有 `/chatbox/typing`（bool）可以打「正在输入」。
- **Avatar 参数**：`/avatar/parameters/` + 参数名，值为 float/bool/int，驱动的角色动画。
  参数名要跟角色的 VRC Avatar 参数表对得上，属「角色制作者决定叫什么」。

> 更细的规格（`config.json` 具体字段、`friendlyName` 大小写匹配的白名单规则、收发的权威
> 文档地址）在 POC 阶段逐条真机验证，验过的写进 `docs/` 一份「OSC 备忘」，不事先猜。

---

## 5. 关键工程决策点（⚠️ 全部待定，未拍板）

### D1. 第一版做到哪一层？

| | 方案 | 效果 | 规模 |
| --- | --- | --- | --- |
| a | **只做 ChatBox 文本**：对外说出的译文，实时推进 VRC ChatBox | 朋友世界里直接看到译文气泡 | 中 |
| b | **只做 Avatar 指示灯**：翻译/开麦时设一个 `/avatar/param` 开关 | 虚拟形象上有「正在翻」 | 小 |
| c | **a + b 一起** | 完整 | 中上（本就共享同一条本地 UDP 通道） |

> ⚠️ 未定。推荐 **c**——两者共用同一条 OSC 帧通道，工程量差不了多少，
> 但**都得用户先手动在 VRC 里开 OSC**（§7，这个引导 UI 无论如何都要做）。

### 5.1. 发什么内容进 ChatBox（译文节奏）

译文来源是 `SubtitleDelta{ track=Speak, text, done }`（现有已广播）。要定的：
- only 在 `done` 整句时推，还是边出边改（跟随把文本增量续进 ChatBox）？
  - ⚠️ ChatBox 是**整行替换**更新，逐字刷会很吵。倾向：**整句 `done` 才推**，或
    每 N 字节合并推。你定「实时感」 vs 「不刷屏」的取舍。
- 144 字被截断怎么给用户知道？—— 建议加一个可选的 `…` 尾巴，UI 上说明。

### 5.3. Avatar 参数叫什么、几个

- 需要 1～2 个参数名（如 `VoxSpeaking`、`VoxTranslating`）。角色上没有同名参数就不显示，
  这是**角色制作者**的事，我们用通配不报错；UI 允许自定义参数名。

### 5.4. friendlyName 与 OSC 白名单怎么处理

- 需要在 VRC 的 `config.json` 写入一个如 `Vox` 的 `friendlyName`，用户才能在
  VRC 的 OSC 面板看到/允许这个设备。**这是「第一次必须手动」的根源**，UI 上要给步骤。
- ⚠️ 直接改写 VRC 的 `config.json` 有一定风险面（VRC 自己也在写同文件），要不要写、
  写成什么样，得定。

### 5.5. 要不要收 VRChat 的「查询/反馈」

- 第一版只发送不读取。若想在 UI 里显示「VRChat 已接收 / 没开 OSC」，得向 `9000` 发某个
  参数查询、再从 `9001` 收回复——这是第二版的增强，不入第一版。

### 5.6. 真机验证（⚠️ 唯一「必须有人动手」的一项）

> OSC 到底收不收得到、friendlyName 白名单怎么走，**静态文档答不了**，要真机：
> VRChat 开着、OSC 开、然后本机发一条 ChatBox 抓包看 VRC 收不收。这条是**影响可见功能**
> 的待办，优先级最高（对照 `docs/architecture/DECISIONS.md` B9 的「真 key 抓一次报文」）。

---

## 6. 工程落地（预想，没动代码）

- **新 crate**：`crates/vox-osc/`（纯 UDP 发包，无重依赖，普通 `std::net::UdpSocket`；
  VRChat 在 Linux 上同样收 `127.0.0.1:9000`，所以这个 crate 是跨平台的）。
  - 只管「把一条 OSC 消息组包发到 127.0.0.1:port」。不做协议内其他东西。
  - 对外裸露两个入口：`send_chatbox(text)`、`set_parameter(name, value)`；附带 `friendly_name` 注册/落盘。
- **装配层接线**（`app/src-tauri`）：
  - 新命令 `osc_start / osc_stop`、`osc_send_chatbox(text)`、（可选）`osc_set_avatar_running(bool)`。
  - 状态：`OSC 模块是否启动`挂到 `Runtime` 附近或单独 slot（参照现在 `ocr` 的 `state.ocr` 槽）。
  - 事件：**不新增事件通道**，仍走 `vox://event`。是否需要给 UI「OSC 没开」提示走现有 `Notice`。
  - 数据钩子：把「对外说话」的 `SubtitleDelta`（Speak track 且 `done`）喂给 `send_chatbox`。
- **前端**：
  - **独立 VRChat 页**（仿现有 `ocr` 页）挂进 `nav.ts`；主开关 + 分项开关 + ChatBox 字数说明。
  - 复用现有 `SettingsItem` / `Slider` / `badge` 等控件（对齐 GlassUI 设计系统）。
  - i18n：zh / ja / en 三语补 key。

---

## 7. 用户体验（「单独的 VRChat 界面可选择开关」）

主界面侧栏加一个 **VRChat（OSC）** 页，包含至少：
- **主开关**：整块 OSC 同步开 / 关（默认关）。
- **连接状态徽标**：OSC 模块是否已启动；是否检测到 VRChat 已开启 OSC（第一版靠「发出去不弹错」
  反推，或读 VRChat `config.json` 的端口看本机 UDP 是否在收）。
- **首次引导**：三步（在 VRChat 设置里开 OSC + Allow Trusted；把 VRChat 的 `friendlyName` 对到
  Vox；回这里按「测试」）。做成 `Ocr` 页那样可折叠的三条。
- **分项**：ChatBox 开关、Avatar 指示灯开关 + 参数名输入、ChatBox 推送节奏（整句 / 流式）。

> 开关的「主动选」这一诉求是满足的——它跟现有 `ocr`、`CableManager` 页的开关形态对齐。

---

## B. 与现有架构的接合（预想，没动代码）

- **新 crate** 位：`crates/vox-osc/`——只做「UDP OSC 帧拼装 + 发送 + `friendlyName` 落盘」。
  跟现有 `vox-net`（WS）无关，跟 `vox-window-ocr-win`（读）方向相对。
- **零侵入核心**：RNNoise、16k 采样约定、重采样、音量阀门、VB-CABLE、`snake_case` 字段、单事件通道全部不变。
- **只新增一条出站边**：从 `SubtitleDelta(Speak, done)` 连一条可选边到 OSC 包发送。

---

## C. 待你确认清单（全部未拍板）

- [ ] **Q1 范围**：只做 ChatBox（a）、只做 Avatar 灯（b）、还是两个都做（c，推荐）？
- [ ] **Q2 反馈**：要不要第二版读 VRChat 的 `9001` 反馈（状态灯「真打没打」）？
- [ ] **Q3 写配置文件**：`friendlyName` 要不要写进 VRChat 的 `config.json`（风险面：VRChat 自己也在写同文件），
   还是让用户手动在 VRChat 里配？
- [ ] **Q4 ChatBox 节奏**：整句 done 才推 / 每 N 字流式推 / 其它？144 字截断怎么处理？
- [ ] **Q5 Avatar 参数**：句「正在翻译」的参数名让它可填；默认建议 `VoxSpeaking`（bool）一个，
  认同吗，还是铺两个？
- [ ] **Q6 真机 POC**：先发一条 ChatBox + 设一个参数验证收不收（对应 5.6），真机对一遍？

→ **结论之前都不写代码。** 你一拍其中任一条，我写进 `docs/architecture/DECISIONS.md` B 区并打日期。