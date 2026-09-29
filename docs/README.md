# Vox 文档索引

> 文档的唯一入口。**文件该放哪见 [`STRUCTURE.md`](STRUCTURE.md)**；方向、状态与文档之间的冲突裁决见
> [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md)。
> 总口径：**代码与文档打架时以代码为准**，然后回头把文档改对。

## 先读哪几份（按顺序）

1. [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) —— **方向总表 + 冲突裁决（新者胜）+ §10 当前执行计划**。开工前必读。
2. [`architecture/DECISIONS.md`](architecture/DECISIONS.md) —— 已拍板记录（A）/ 待拍板（B）/ 后端契约（C）。
3. [`architecture/ARCHITECTURE.md`](architecture/ARCHITECTURE.md) —— 现状分层（轻内核 + 平台外壳）。
4. [`STRUCTURE.md`](STRUCTURE.md) —— 文档与目录规划（文件该放哪、命名的规矩）。
5. [`plans/`](plans/) —— 施工设计稿，编号 `S<数字>-<主题>.md`（S 系列正在陆续产出）；**施工前按编号读对应那一份**，别读总表就动手。

`platform/` 与 `protocols/` 是"要用到时才翻"的：动手改某个平台的坑 → 读对应 `platform/<平台>.md`；
改 provider / 报文 → 读对应 `protocols/*.md`。`research/` **不是待办**，别照它施工。

## 状态标记

与 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) 同一套，另加 `[已归档]`：

**[已生效]** 代码已在跑｜**[已拍板·未开工]** 定了但代码里还没有｜**[未拍板]** 只是讨论过，不能当施工依据｜
**[已作废]** 被更新的方向推翻，别再引用｜**[已归档]** 过期但有价值的调研沉淀，保留留痕。

## architecture/ —— 现状与方向（不随平台变的东西）

| 文档 | 一句话 | 状态 |
| --- | --- | --- |
| [`ARCHITECTURE.md`](architecture/ARCHITECTURE.md) | 现状分层（芯 / 外壳 / 装配）：模块职责、端口 trait、线程拓扑、数据流 | **[已生效]**（2026-08-18 按代码核对过） |
| [`DECISIONS.md`](architecture/DECISIONS.md) | 拍板记录：A 已拍板 / B 待拍板 / C 前后端契约 | **[已生效]**，B 区仍有未答条目 |
| [`DIRECTIONS.md`](architecture/DIRECTIONS.md) | 方向总表 + 文档冲突裁决（§8 新者胜）+ 拍板后的执行计划（§10） | **[已生效]**，方向类问题的唯一权威 |

## platform/ —— 每个平台/宿主一档

| 文档 | 一句话 | 状态 |
| --- | --- | --- |
| [`SCOPE.md`](platform/SCOPE.md) | 平台范围决策 + 跨平台调研沉淀（A 已拍板 / B 暂缓依据 / C 未来起点） | **部分 [已作废]**：§A 第 1、2 条与 §C1 已被推翻（文件开头有告示，裁决见 `architecture/DIRECTIONS.md` §8 第 2/3/4 条）；§B 与 §C 其余仍有效 |
| [`LINUX.md`](platform/LINUX.md) | Linux 适配执行方案：锚定范围、P0–P4 步骤、逐条实测证据 | **[已生效]**，P0–P4 已落地；遗留见本文 §9 与 `architecture/DIRECTIONS.md` §2 |
| [`WINDOW_BEHAVIOR.md`](platform/WINDOW_BEHAVIOR.md) | Windows 窗口边缘两项未决问题（12px 大圆角没渲染 / 最小高度锁不到 38）的真机现状追踪 | **[未拍板]**，只能真机验证 |
| [`ANDROID.md`](platform/ANDROID.md) | 手机档：面对面翻译（自麦进 → 耳机/外放出 + 应用内字幕）；抓通话音频与虚拟麦结构上做不到（位报 `false` + 界面照实说）；含 Tauri v2 路线、Oboe 48k 单声道、Keystore、分发与审核 | **[已拍板·未开工]**（S2），**2026-09-26 起顺延到 S4-A 之后**（`architecture/DIRECTIONS.md` §8 第 15 行），**当前被环境阻塞**：需 Android NDK + `cargo-ndk` + `aarch64-linux-android`/`x86_64-linux-android` 两个 rust target，本机未装（装机需用户同意，见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.7「环境阻塞」）；一轮桌面调研，**未实机验证**，无一手出处的条目已逐一标 `[未核实]` |
| [`EMBEDDED.md`](platform/EMBEDDED.md) | 嵌入式档：小主板 ARM64 Linux（Rust Tier 1）复用 `vox-audio-linux` + systemd `--user` + 无屏三件（配置进 / 状态出 / 进程活）；MCU 档已砍，代价留档（`nnnoiseless`/`rubato` 都踩 std） | **2026-09-26 起优先于手机**，音频缺省改 ALSA、控制面可开局域网，见文件顶部状态注与 [`plans/S4-EMBEDDED-REFACTOR.md`](plans/S4-EMBEDDED-REFACTOR.md)；**[已拍板·未开工]**（S3）；一轮桌面调研，**未实机验证**，无一手出处的条目已逐一标 `[未核实]`。**第十五轮进度**：无屏外壳本体 `crates/vox-headless/` + systemd 两份 unit（`systemd/{user,system}/`）+ 样例配置 + README + 打包脚本 `tools/package-headless.sh` 已落地，`--print-composition` 可用；`background_service` 位仍如实 `not_wired`（unit 文件不算凭据）——**板子上的实机验证仍未做**，见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.7「第十五轮回填」 |

## protocols/ —— 对外协议与数据

| 文档 | 一句话 | 状态 |
| --- | --- | --- |
| [`PROVIDER_CATALOG.md`](protocols/PROVIDER_CATALOG.md) | 服务商能力表（`catalog/*.json`）的唯一维护入口与更新口径 | **[已生效]** |
| [`QWEN_PROTOCOL.md`](protocols/QWEN_PROTOCOL.md) | 阿里云百炼实时翻译 WebSocket 协议实测规格（字段 / 事件 / 用量 / 坑） | **[已生效]** |
| [`GEMINI_PROTOCOL.md`](protocols/GEMINI_PROTOCOL.md) | Gemini Live Translation 协议：连接、鉴权、报文（2026-08-19 核对） | **[已生效]** |
| [`VRC_OSC_PROTOCOL.md`](protocols/VRC_OSC_PROTOCOL.md) | VRChat OSC「对外说话」增强预研：方向、能力边界、代价、待拍决策点 | **[未拍板]**，但**代码已落地**（`crates/vox-osc/` + 5 个命令 + `sections/Vrchat.tsx`；见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §7） |
| [`DISCORD_PROTOCOL.md`](protocols/DISCORD_PROTOCOL.md) | Discord 二期集成预研（按人独立 vs 频道混译、opus 解码、Bot 部署、延迟） | **[未拍板]**，六个问题全待答 |

## plans/ —— 施工设计稿（architect 产物：现状证据 → 目标形状 → 改动清单 → 验收标准）

| 文档 | 一句话 | 状态 |
| --- | --- | --- |
| [`S0-COMPOSITION-MANIFEST.md`](plans/S0-COMPOSITION-MANIFEST.md) | 组合清单（host/in/ops/out/life/ui/control + session）+ 统一能力位模型 | **[已生效]**（部分实现）：**芯侧已落地**——`crates/vox-core/src/{composition.rs,capability.rs}`、`Plan::build` 经清单中转、`Snapshot.capabilities`，独立复核判"芯侧可验收"；**外壳侧注入 + Linux 虚拟麦接线也已落地**（`platform::{host_kind,host_facts}` + 装配注入 + `virtual_mic.rs`，该位实测 `true`）；**UI 侧随位降级也已落地**（`app/ui/src/capabilities.ts` + `components/Capability.tsx` + `check:capabilities` 进 `npm run verify` 链 + `dto.rs` 的 `capabilities` 字段）；**第十五轮转正**：`--print-composition` 已**两档落地**（桌面 `app/src-tauri/src/composition.rs` 只 `Builder::build()` 不 `run()`；无屏 `crates/vox-headless/src/cli.rs` → `status.rs::composition_json`；两档同形、同一个派生 `vox_mcp::endpoints::manifest`——即 S1 的 `describe_endpoint` 用的那个函数）；`virtual_cable_installed` 本轮已拍**不拆**，降级为「仅 Windows 安装管理用」（"能不能用"只看 `virtual_mic` 能力位，见 `architecture/DIRECTIONS.md` §10.5）；**第九轮三条阻断项（D1/D2/D3）的修复已过独立复核**（`agent://AuditRound9`：真机 5 秒 152 次渲染、截图红 4760 / 绿 6144、`hide()` 后 0-0；4 个变异全红），明细见 `architecture/DIRECTIONS.md` §10.7「第十轮回填」 |
| [`S1-AGENT-FACE.md`](plans/S1-AGENT-FACE.md) | 控制面：动作清单 → CLI + MCP(2026-07-28)，本机 127.0.0.1 HTTP 为主通道 | **[已生效]**（部分实现）：**动作表 + 协议面 + 本机 HTTP + 端点投影 + 会话与 token + `Grants` 已落地**——`crates/vox-mcp/`（`cargo test -p vox-mcp` = **83 条**（2026-09-22 第十五轮收口实测；**当前值见 §10.7**）= 73 条集成（`tests/protocol.rs` 15 + `http.rs` 19 + `endpoints.rs` 15 + `resources.rs` 8 + `lifecycle.rs` 4 + `voxctl.rs` 12）+ 10 条 crate 内单测），`voxctl serve` 只绑 `127.0.0.1`、单路径 `/mcp`、POST-only；**第十轮补了两处安全护栏**（`control.enabled` 总闸 + token↔清单绑定，两条都有钉子用例、去掉即红）；**控制面已接进 app 产品路径**（`app/src-tauri/src/mcp.rs`，`assemble()` 第 14 步起服务，app 集成测试 **7 条** + 真 app curl 冒烟）；**资源面已落地**（`resources/list` / `resources/read` / `subscriptions/listen` SSE 长流，字幕资源 `vox://session/<handle>/transcript`；`server/discover` 的 `capabilities` 已含 `resources{listChanged,subscribe}`，两位都真会发通知）；**第十三轮补全**：stdio 桥（`voxctl serve-stdio`，`transport/stdio.rs`）+ 5 个动作子命令（`list-endpoints` / `describe-endpoint` / `compose-endpoint` / `session-open` / `session-close`）+ 设置页控制屏（`app/ui/src/sections/AgentControl.tsx` = `PAGE_NAV` 第 **8** 页，原 7 页）+ **热切换**（`app/src-tauri/src/mcp.rs::ControlPlane` 挂 `Event::SettingsChanged`，拨开关即起/停、换端口即重绑）；`check:agent` 随之进 `npm run verify` 链。**第十五轮收口**：`actions.rs::composition_schema!` 那一格**已接真 schemars**（`crates/vox-mcp/build.rs` 用 `schema_for!(vox_core::composition::Composition)` 生成 + `$ref` 重定域；feature `json-schema` 默认开、`--no-default-features` 走如实占位），`tests/protocol.rs` 增至 15 条——**S1 不再有未落地项**。进度回填见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.7 |
| [`S3-NET-AUDIO.md`](plans/S3-NET-AUDIO.md) | 网络音频进出（`net_in` / `net_out`）：媒体面协议与帧格式 + 两条腿网络化 + `NetIn`/`NetOut` 能力位定义者 + 心跳来源 + 安全 | **[已生效]**（部分实现）：**S3 帧层已落地**——`crates/vox-net/src/media/{mod,frame,pipe,server,client}.rs`（13 条 `tests/media_pipe.rs` + `examples/media_probe.rs`）+ `CaptureTarget::Net { pipe }`（已迁移四处穷尽 `match`）；**S3 后半未做**（无屏档端到端接媒体管子；`NetIn`/`NetOut` 两位仍恒假）。进度回填见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.7 |
| [`S4-EMBEDDED-REFACTOR.md`](plans/S4-EMBEDDED-REFACTOR.md) | 嵌入式优先的重构：共享宿主层 `vox-host` + 算子链执行器 + 轻量化（ALSA / 回声消除三档 / 局域网控制面 / 算子自计时） | **[已拍板·未开工]**（2026-09-26，第 1 版概要稿；各阶段开工前由 architect 细化），方向见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.9 |

## research/ —— 过期但有价值的调研沉淀（**不是待办**，别照它施工）

| 文档 | 一句话 | 状态 |
| --- | --- | --- |
| [`BACKEND_HEALTH_CHECK.md`](research/BACKEND_HEALTH_CHECK.md) | 后端 Rust 代码体检报告（冗余、热点、建议清单） | **[已归档]**：条目已全部落地，见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §7 |
| [`UI_HEALTHCHECK_PLAN.md`](research/UI_HEALTHCHECK_PLAN.md) | 前端代码体检执行方案（T/D/R 编号作业清单） | **[已归档]**：T1–T9、D1、D2、R1、R2 已落地，仅 R3 未开工 |
| [`FOCUS_HANDOFF.md`](research/FOCUS_HANDOFF.md) | 首页布局 + 二维网格焦点（列记忆）交接单 | **[已归档]**：已落地，见 [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §7 |
| [`TASK_REPORT_agent.md`](research/TASK_REPORT_agent.md) | 三项问题交办单（方向键 / i18n 多语 / 更新模块两条路径） | **[已归档]**：P0、P1 与更新模块路径 B 已落地，路径 A 未做 |

## 质量门口径

- 计数只写真值，写哪一次就注哪个日期；**本节里的数一律是"带轮次的快照"，当前值一律以
  [`architecture/DIRECTIONS.md`](architecture/DIRECTIONS.md) §10.7 为准**（取数与逐项依据也在那里）。
- Rust：`cargo test --workspace` 第十五轮收口时 **570 passed / 0 failed / 5 ignored**（2026-09-22 实测，
  第十六轮复核重跑同值）；**当前值见 §10.7**；`cargo clippy --workspace`、
  `cargo fmt --all -- --check` 的口径同 §10.7。
- 前端：`npm run verify`（`app/ui/package.json` 的 `verify`）——链 =
  `build` → `check:classes` → `check:preview` → `a11y` → `qa:narrow` → `check:cable` → `check:capabilities` →
  `check:agent` → `qa:home`。

## 维护规矩

- 新文档先看 [`STRUCTURE.md`](STRUCTURE.md) 的放置规则，再在本文件对应表格补一行（一句话 + 状态标记）。
- 被取代 / 已完成的文档：**不改内文**，只在第一行加状态头（格式见 `STRUCTURE.md` §1），过期调研移进 `research/`。
- 引用别的文档时写全路径 `docs/<目录>/<文件>.md`，别写裸文件名——目录一动就断链。
