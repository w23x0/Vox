# 文档与目录规划

> 口径沿用 `DECISIONS.md`：**代码与文档打架时以代码为准**，然后回头把文档改对。
> 冲突裁决见 `docs/architecture/DIRECTIONS.md` §8（文档之间冲突**新者胜**）。
> 本文件只回答两件事：**文件该放哪**、**目录怎么长**。

## 1. 文档目标结构

```
docs/
├─ README.md              索引：每份文档一句话 + 状态标记 + 该先读哪几份（唯一入口）
├─ STRUCTURE.md           本文件（放置规则）
├─ architecture/          现状与方向：不随平台变的东西
│   ├─ ARCHITECTURE.md    现状分层（芯 / 外壳 / 装配）
│   ├─ DECISIONS.md       已拍板（A）/ 待拍板（B）/ 后端契约（C）
│   └─ DIRECTIONS.md      方向总表 + 冲突裁决 + 当前执行计划（§10）
├─ platform/              每个平台/宿主一档，写"这个平台能做什么、不能做什么、怎么落地"
│   ├─ SCOPE.md           平台范围与决策（原 PLATFORM_SCOPE.md）
│   ├─ LINUX.md           原 PLATFORM_LINUX.md
│   ├─ WINDOW_BEHAVIOR.md Windows 窗口边缘未决项
│   ├─ ANDROID.md         手机档（已建：来源=一轮调研，未实机验证）
│   └─ EMBEDDED.md        小主板/MCU 档（已建：来源=一轮调研，未实机验证）
├─ protocols/             对外协议与数据：provider、VRChat、Discord
│   ├─ PROVIDER_CATALOG.md
│   ├─ QWEN_PROTOCOL.md  GEMINI_PROTOCOL.md
│   └─ VRC_OSC_PROTOCOL.md  DISCORD_PROTOCOL.md
├─ plans/                 施工设计稿（architect 的产物，编号 + 主题）
│   └─ S0-COMPOSITION-MANIFEST.md … S4-EMBEDDED-REFACTOR.md
└─ research/              过期但有价值的调研沉淀（**不是待办**，别照它施工）
    ├─ BACKEND_HEALTH_CHECK.md
    ├─ UI_HEALTHCHECK_PLAN.md
    ├─ FOCUS_HANDOFF.md
    └─ TASK_REPORT_agent.md
```

### 放置规则

| 新东西是… | 放哪 |
| --- | --- |
| 平台能做什么 / 怎么落地 / 该平台的坑 | `platform/<平台>.md`（一平台一档，合并进已有文件，不要每轮新建） |
| 与平台无关的现状、决策、方向 | `architecture/` |
| provider / 外部协议 / 报文格式 | `protocols/` |
| 施工设计稿（architect 产物，带编号与验收标准） | `plans/<编号>-<主题>.md` |
| 一次性调研、已被取代的清单、交接单 | `research/`（**文件顶部必须加状态头**：何时、被谁取代） |

**命名**：目录全小写；文件用 `大写_下划线.md`；计划稿前缀 `S<数字>-`；不建"杂项""其他"目录。

**状态头**：任何"已完成/已过期"的文档，第一行加：

```
> 状态：已完成（2026-09-21）｜ 被 docs/plans/<新文件> 取代 ｜ 保留作留痕，别照它施工。
```

## 2. 代码目录（现状 + 将新增）

```
crates/
  vox-core/ vox-net/ vox-dsp/ vox-osc/ vox-overlay-core/     芯（平台无关）
  vox-{audio,input,overlay}-win/ vox-{audio,input,overlay}-linux/   平台外壳
  vox-mcp/             已建：动作清单 + 协议面 + 本机 HTTP（含 `subscriptions/listen` 的 SSE 长流）+ 端点投影 + 会话/token + 资源面（`resources`/`subscriptions/listen`）；stdio 桥（`transport/stdio.rs`）与 5 个动作子命令已落地（`voxctl serve` / `serve-stdio` / `list-endpoints` / `describe-endpoint` / `compose-endpoint` / `session-open` / `session-close` + 瘦客户端 `client.rs`）
  vox-headless/  已建（无屏档入口，与 app/src-tauri 并列的第二个外壳）：芯 + PipeWire + 无屏三件 + 控制面，零 Tauri；`--config <settings.json>` / `--print-capabilities` / `--dry-run`（一直跑的模式另有 `--start` / `--run-for`）
  vox-host/            （待建 S4-A）共享宿主层：装配顺序 / 持久化 / 密钥选择 / 控制面胶水 / EventSink，各宿主入口只写薄壳
  vox-audio-alsa/      （待建 S4-C）ALSA 采集/播放，嵌入式缺省音频后端
  vox-audio-android/   （待建 S2，顺延到 S4-A 之后）手机音频外壳
app/
  src-tauri/           装配层（命令、事件桥、platform/ 分流、sys/）
  ui/                  React 界面
  android/             （待建 S2）Tauri Android 工程
catalog/               模型服务商元数据（前后端共读）
tools/
  linux-verify/        已有：界面像素级验证
  bench-dsp/           已建：可复现的 DSP 算子成本表（独立 crate，不随 workspace 跑）
.omp/
  AGENTS.md RULES.md   项目上下文与硬规矩
  agents/*.md          代理定义（architect / core-dev / shell-dev / agent-face-dev / docs-scribe / verifier）
```

### 目录规矩

- **芯不许知道平台**；平台差异只出现在 `vox-*-<platform>` 与装配层的 `cfg` 分流。
- **一个能力一个 crate**：新增宿主（手机/嵌入式）时，加"外壳 crate + 装配层分流 + platform/<平台>.md"，不新建整棵树。
- **一次性脚本不许留在 crate 里**：验证/基准类的东西进 `tools/`。
- **agent 定义进 `.omp/agents/`**：一个角色一个文件，职责与边界写清楚；跨角色的改动由用户/Main 排期。

## 3. 文档生命周期

1. 有方向 → 先动 `architecture/DIRECTIONS.md`（新增条目 + 冲突行）。
2. 要施工 → architect 出 `plans/<编号>-<主题>.md`（现状证据 → 目标形状 → 改动清单 → 验收标准）。
3. 施工完 → 计划稿顶部加状态头；`DIRECTIONS.md` 的"已完成/归档"节补一行。
4. 被取代的旧清单 → 移进 `research/`，**不改内容**，只加状态头。
