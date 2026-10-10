# NOTICE-I18N：后端提示文案多语言化

状态：Main 已拍板 K1–K5（2026-10-10），本稿为施工与验收依据。

## 1. 决定

| # | 决定 | 落地 |
|---|---|---|
| K1 | 通知结构化 `{ key, params, detail? }` | `vox_core::notice::Msg`；`Notice` 把它 `#[serde(flatten)]` 进去，线上形状 `{severity, key, params, detail?, pipeline}`。`detail` 是服务端/系统原文，不翻译，渲染时用该语言的分隔符接在译文后面。前端回退：当前语言 → English → key 本身（**不回退中文**）。 |
| K2 | 先清点所有发出点再逐个迁移 | 见 §3 清单。`tracing::` 日志不动。 |
| K3 | 命令的 `Err` 同一结构 | Tauri 命令一律 `Result<T, Msg>`；前端 `errorText(e, t)` 统一展示，所有 `String(error)` 调用点迁走。 |
| K4 | 无屏档用 English 渲染，与前端 en 同源 | **同源方案 = 同一个文件**：`catalog/notices.json`（zh/en/ja 三语 + 分隔符）。Rust 用 `include_str!` 读，前端用 JSON import 合进 i18n 字典。不生成代码、不存两份。`Msg::render_en()` 给无屏档日志与 MCP 账本用。 |
| K5 | `check-i18n.mjs` 检查 key 齐全 | 脚本从 `crates/vox-core/src/notice.rs` 的 `declare_keys!` 里取出 Rust 声明的全部 key，逐个核 zh/en/ja 都有非空文案、占位符三语一致、JSON 里没有多余 key。Rust 侧另有等价单测（`notice::tests`）。 |

## 2. 形状

```text
catalog/notices.json
  { schema_version, detail_separator:{zh,en,ja}, messages:{ "<key>": {zh,en,ja}, … } }
```

- key 前缀：`notice.*`（经 `Runtime::notify` 发出的）、`error.*`（命令 `Err`）。
- `params` 全是字符串，模板里用 `{name}`；缺参数原样留着 `{name}`。
- `PortError` 保持 `{ message }`（开发者诊断串，等同 detail），另加可选 `user: Option<Msg>`：
  外壳认得出的、用户能照着做的失败（找不到设备、程序没在出声、没有 input 组权限…）直接带 key；
  其余由内核在**阶段**上补 key（`PortError::at(stage_key)` → `Msg{stage_key, detail: message}`）。
- 服务端错误 `cloud::explain_error / explain_close` 改返回 `Msg`，服务端原话放 `detail`。
- `PipelineStatus.last_error` 由 `Option<String>` 改 `Option<Msg>`；MCP 账本用 `render_en()`。
- `Runtime::start` 等发 `Notice` 的位置都改用 `notice::key::*` 常量，写错 key 编译不过。

## 3. 发出点清单（K2，已全部迁移）

清点方法：`grep -rn "Notice::\|PortError::new\|PortError {\|Result<.*String>\|Err(\"" crates app --include=*.rs`，排除 `tracing::`。

| 层 | 发出点 | key |
|---|---|---|
| `vox-core::runtime` | 能力位越界、读/存密钥失败、两条"已自动重启"、缺密钥、没选监听程序、热键注册失败、流水线失败 | `hostFactsExcess` `keyReadFailed` `keySaveFailed` `restartSpeak` `restartListen` `apiKeyMissing` `listenTargetMissing` `hotkeyRegisterFailed` `pipelineFailed` |
| `vox-core::pipeline` | 装配清单不合法 / 无输入、连不上、播放/采集/链失败（按**阶段**补 key）、原声输出/回听/译音输出失败、降噪降级、服务端 close/错误 | `sessionPlanInvalid` `noCaptureInput` `connectFailed` `playbackOpenFailed` `captureStartFailed` `chainBuildFailed` `passthroughOutputFailed` `monitorFailed` `translationOutputFailed` `denoiseDisabled` `server.*`（8 条，`explain_error` / `explain_close`） |
| `vox-host` | 控制面没起来、明文密钥提示 | `controlPlaneDown` `plaintextKeys` |
| 装配层（桌面/无屏） | 启动提示 4 条（PipeWire、托盘、无屏需 Linux）、悬浮窗 3 条、设备扫描线程、全局热键、托盘、虚拟麦 2 条 | `pipewireMissing(Headless)` `trayMissing` `headlessNeedsLinux` `overlay*` `deviceScanCrashed` `hotkeyHostFailed` `trayFailed` `cableHideFailed` `cableRestoreFailed` |
| 外壳 `PortError::user`（用户能照着做的） | 找不到输入/输出设备、程序没在出声/没在运行、input 组权限、ALSA 不能按程序抓音 | `inputDeviceNotFound` `outputDeviceNotFound` `programSilent` `programNotRunning` `inputPermission` `captureByProgramUnsupported` |
| Tauri 命令 `Err`（K3，共 42 处） | 未知流水线/服务商、设置不合法、OSC、设备扫描任务、配置目录、打开浏览器、目录在线更新、虚拟麦（Windows） | `error.*`（37 条） |

外壳 / 密钥后端 / OSC 里剩下的 `PortError::new(...)`、`map_err(...)` 诊断串（约 120 条）一律改成 **English 诊断串**——它们作为 `detail` 原样跟在阶段 key 的译文后面，不再混进中文。

## 3.1 K4 / K5 的落地

- K4 选「同一个文件」：`catalog/notices.json` 只有一份，Rust `include_str!`、前端 JSON import；无屏档日志用 `Msg::render_en()`，MCP 账本的 `last_error` 同样。不存在两份 en 文案。
- K5：`check-i18n.mjs` 读 `notice.rs` 的 `declare_keys!`，三语 / 占位符 / 孤儿 key 一并核；Rust 单测 `every_declared_key_has_all_three_languages_and_no_orphans` 做同一件事。

## 4. 测试与变异

- `notice::tests`：每个声明的 key 在 JSON 里三语齐全；占位符三语一致；JSON 无多余 key；`render_en` 带 detail 原文。
- `check-i18n.mjs`：缺 key 报错；回退顺序（zh 缺 → en；en 也缺 → key，不是中文）；detail 原样保留。
- 变异：删一条 ja 文案 / 把回退改成 zh / 渲染时吞掉 detail——各自应红。

## 5. 后续（不在本单）

- **NET 工单的文件**（已合入 main，本单只合并、不改它们的文案）：`crates/vox-net/**`（`ws.rs`、`media/*` 里的 `PortError::new` 中文诊断串，约 36 处）、`crates/vox-proxy/**`（`spec.rs` 的 `Display`、`dial.rs` 的 `DialError.message`：代理地址为空 / 域名解析失败 / 代理握手…，测速与接管检测报告里直接显示）、`crates/vox-host/src/{netcheck*,models*,http.rs}`（`ModelListError.message`、测速阶段文案）。它们的诊断串仍是中文，会作为 `detail` 或报告字段原样出现；按本稿做法改成 English 诊断串或带 key 的 `PortError::user` / 报告里存 `Msg`。本单只迁了 NET 新增的两条命令外壳错误（`error.netTestTask`、`error.takeoverTask`）与前端 `netTestFailed` 的渲染。
- **overlay 工单的文件**：`crates/vox-overlay-*` 的 `PortError::new`（字体、窗口，约 20 处）同上。
- **Windows 外壳的 COM 诊断上下文**：`vox-audio-win` 里 `.ctx("…")` / `hr_err("…")` / `cable.rs` 下载提示 / `policy.rs`（约 60 条中文，Windows 才会走到，本机只能 `cargo check --target x86_64-pc-windows-gnu`）。
- **托盘菜单文字**（`app/src-tauri/src/tray.rs`：对外说话 / 听人说话 / 显示字幕 / 打开设置 / 退出）与**启动失败对话框**（`lib.rs` 的"Vox 启动失败"，此时前端和语言都还没有）：不是通知，需要另外按 `ui_language` 重建菜单，单独一张工单。
- `PipelineSnapshot.state_label` 仍是后端中文；前端已不显示它（`pipeline.ts` 顶部有注），可以在下一次快照形状变动时删掉。
- `vr_overlay.rs` 的 OpenVR 失败串（detail，中文）与 `vox-mcp` 面向 Agent 的错误文案不在本单。

