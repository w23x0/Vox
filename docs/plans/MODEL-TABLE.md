# 模型表 + 协议方言 + 接入点走配置链 + 官方模型列表

状态：Main 已拍板（2026-10-09）。前端已按 `docs/plans/UI-REDESIGN.md` §6 的契约落地（mock），本稿是 Rust 侧施工依据。

## 0. 目标

新模型上线 = 改表，不改代码；模型列表从服务商**官方接口**现场拉，表里只放官方接口给不了的东西（协议方言、音色、语言）。

## 1. 真连核实的事实（2026-10-09，国内站密钥，`crates/vox-net/examples/aliyun_probe.rs`）

### 1.1 阿里云官方模型列表
- `GET https://dashscope.aliyuncs.com/api/v1/models?capabilities=Realtime-Audio-Translate&page_size=100`，`Authorization: Bearer {key}`，**旧国内域名可用，不需要 WorkspaceId**。
- 国际站 `https://dashscope-intl.aliyuncs.com`、香港 `https://cn-hongkong.dashscope.aliyuncs.com` 用国内站密钥返回 `401 InvalidApiKey`：**密钥按地区分开**，换地区必须换密钥。
- 返回 `output.models[]`，真实字段（逐字）：`model`、`name`、`description`、`provider`、`capabilities[]`、`published_time`（`"2026-09-11 18:02:53"`）、`inference_metadata.request_modality[]` / `response_modality[]`（单数！）、`equivalent_snapshot`（目前全 null）、`inference_offline_info`（null 或 `{"offline_time":"2026-10-10 00:00:00"}`）、`model_info{context_window,…}`、`prices[]`。
- 当日返回 6 个：`qwen3.8-livetranslate-flash-realtime`、`qwen3.5-livetranslate-flash-realtime`、`qwen3.5-livetranslate-flash-realtime-2026-05-19`、`qwen3-livetranslate-flash-realtime`、`qwen3-livetranslate-flash-realtime-2025-09-22`、`gummy-realtime-v1`。后三个 `offline_time = 2026-10-10`。
- 探针的列表解析字段名猜错了两处（`request_modalities`→应为 `request_modality`；offline 时间在 `inference_offline_info.offline_time`）。本工单顺手修正。

### 1.2 qwen3.8 会话
- `audio.output.voice` 与平铺 `voice` **两种形状都被接受**，`session.updated` 两处都回显所选音色。产品继续用现有（3.8）形状。
- 接受 `input_audio_transcription.language`（回显）。
- 服务端缺省 `input_audio_transcription.model = "gummy-realtime-v1"`——而 gummy 2026-10-10 下线。**待查**：显式发 `input_audio_transcription.model = "qwen3-asr-flash-realtime"`（官方文档为 3.5 推荐的源语言识别模型）3.8 是否接受；若接受，3.8 方言默认显式带上。
- 3.8 缺省 `output_audio_format = pcm24`（24k）。

### 1.3 qwen3.5 会话 —— **当前连不通**
- 收到 `session.created` 后，服务端立刻发一个**负载 > 125 字节的控制帧**（大概率是带长原因的 close 帧），tungstenite 报 `WebSocket protocol error: Control frame too big` 断开。文档形状（平铺 `voice` + `pcm`）和新形状都一样。
- 这同样会打到产品（同一个 `WsTransport`）。本工单要查清：拿到原始 close 负载（例如临时用不校验的读取路径、或抓 TLS 之上的帧）看服务端到底说了什么，再决定是协议形状问题还是要容忍超长控制帧。查不清就把 3.5 标 `adapted=false` 并在报告里写明，不许硬糊。

## 2. 拍板

| # | 决定 |
|---|---|
| M1 | **方言表**：`catalog/aliyun.json` 重构为多模型：顶层 `dialects{}`（`qwen3.8`、`qwen3.5`，各自的会话形状要点、语言表、音色表、能力位）+ `models[]`（`id`、`dialect`、`recommended`、`label`）。模型→方言靠显式 `id` 或 `id_prefix` 匹配（快照 `-YYYY-MM-DD` 后缀归到同方言）。`gpt.json` / `gemini.json` 同构（各一个方言）。表里**不放**官方列表能给的东西（发布时间、下线时间、价格）。 |
| M2 | **协议方言**：`crates/vox-core/src/cloud/protocol.rs` 的 session.update 构造与事件解码拆成按方言的适配器（trait 或 enum 分派均可，以简单为准），`SessionParams` 带方言。GPT / Gemini 已在各自文件，接到同一选择点。 |
| M3 | **接入点走配置链**：删掉进程级 `INTL_REGION` 与 `set_region`。`Settings` 增加 `aliyun_endpoint: String`（空 = 按地区缺省），连同 `aliyun_region` 经 SessionConfig 传到建连处。`VOX_ALIYUN_ENDPOINT` 环境变量保留为最高优先级的调试覆盖。前端已在 `types.ts` 用 `aliyun_endpoint` 这个名字。 |
| M4 | **放宽模型归一**：`normalize_model_for` 不再把一切归到缺省；只要模型 ID 能匹配到方言就保留，匹配不到才回缺省。旧设置里 qwen3.5 / 3.8 的值都要保留。 |
| M5 | **`list_models` 命令**（Tauri + `vox-host` 共享实现，无屏档也能调）：按 `UI-REDESIGN.md` §6 的 `ModelList` 契约返回。阿里云按 §1.1 拉并翻页；OpenAI `GET https://api.openai.com/v1/models`（Bearer）按 id 含 `realtime-translate` 过滤；Gemini `GET https://generativelanguage.googleapis.com/v1beta/models?key=…&pageSize=1000` 按 name 含 `live-translate` 且 `supportedGenerationMethods` 含 `bidiGenerateContent` 过滤。`adapted` = 匹配到方言；`offline_at` 取 offline_time 的日期部分；能力与语言来自方言表。错误归类 `auth`（401/403）/ `network`（连接/超时/DNS）/ `other`，带 `host`。内存缓存 10 分钟，`refresh=true` 强制重拉；拉失败返回内置列表（方言表里的 `models[]`）+ error。HTTP 客户端用 workspace 已有的；**不许让 `cargo tree -p vox-headless` 多出 tauri/gtk/keyring/zbus 类依赖**（加一个纯 Rust HTTPS 客户端可以，报告写依赖树对比）。 |
| M6 | **`api_key_tail`**：设置快照给每个服务商的密钥末 4 位（无密钥为 null），前端去掉 localStorage 的临时做法改读快照。 |
| M7 | 地区：`AliyunRegion` 增加 `Hk`（香港，`cn-hongkong.dashscope.aliyuncs.com`）仅当 §1.1/§1.2 能证明香港有实时翻译 WebSocket 地址；证明不了就不加，报告写明。前端地区下拉的说明文案改为"密钥只在创建它的地区有效"。 |
| M8 | gummy / qwen3 不进方言表（明天下线）；旧设置里若是这些值，归一到缺省模型。 |

## 3. 不做
- 首次启动"用户是否显式选过语言"标记、后端 notices 改 i18n key：单独工单。
- 不改 UI 结构；前端只做 M6 的读取改动与 `types.ts`/快照同步。

## 4. 验收
- `bash ~/.config/vox-orchestrator/gate.sh <worktree> <target>` 最后一行 `GATE PASS`；`cd app/ui && npm run verify` 全绿。
- 新测试覆盖：方言匹配（含快照后缀、未知 ID）、M4 归一、三家列表解析（用 §1.1 的真实字段名写样例 JSON）、错误归类、M3 地址优先级（env > 自定义 > 地区缺省）。关键用例做变异验证。
- 报告附：3.5 断连的根因（或"未查清"及证据）、3.8 显式 ASR 模型的探针结果（若能无密钥验证就做，不能就给出 Main 要跑的命令）。
