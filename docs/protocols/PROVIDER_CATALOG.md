# 服务商能力表维护说明

VoxBridge 只做实时语音翻译。当前启用三个服务商，各固定一个专用模型：

- 阿里云百炼：`qwen3.5-livetranslate-flash-realtime`
- Google Gemini：`gemini-3.5-live-translate-preview`
- OpenAI GPT：`gpt-realtime-translate`

模型、语言、音色、API 元数据的唯一维护入口是：

`catalog/aliyun.json` 和 `catalog/gemini.json`

前端直接导入两份 JSON；`vox-core` 的构建脚本会读取并校验同一份数据，再生成 Rust 常量。
不要在 TypeScript 或 Rust 文件里另抄一份表。

## 当前目录包含什么

- 官方推荐的稳定模型 ID 与当前稳定快照。
- WebSocket 旧公共地址和分地域业务空间地址模板。
- 60 种可识别/可翻译语言。
- 其中 29 种支持“音频+文本”输出，31 种仅支持文本。
- Qwen3.5 LiveTranslate 当前官方音色及中文说明。
- 已移除旧音色 ID，仅用于把老用户配置安全迁移到 Tina，不会出现在界面选项里。
- 官方文档地址与最后核对日期。
- Gemini Live Translation 的模型、WebSocket、音频规格和能力摘要。

## 更新流程

1. 先看对应服务商的模型上下架公告。
2. 核对实时翻译、客户端事件、服务端事件、音频格式和限流说明。
3. 修改对应的 `catalog/*.json`，同时更新 `verified_at`。
4. 运行 `cargo test --workspace` 和 `npm run verify`。
5. 用真实 API Key 做一次中文→外语、外语→中文的双线路冒烟测试。
6. 提升版本号后打 tag 推送，CI 自动构建并发版（见下节「软件更新与自动发版」）。

构建脚本会强制检查语言、语音输出语言和音色数组的数量必须与 JSON 中的 `expected_counts` 一致，默认语言和默认音色必须存在，并拒绝重复代码。官方数量变化时，数据和期望数量在同一个 JSON 里一起更新。

Gemini 的公开限流值可能随项目层级变化，不写进目录。实际 RPM/TPM/RPD 以 AI Studio 的项目配额页为准；同一 Google Cloud 项目下的多个 API Key 共享配额。

## 官方资料

- 实时翻译总览：https://help.aliyun.com/zh/model-studio/qwen3-5-livetranslate-flash-realtime
- 客户端事件：https://help.aliyun.com/zh/model-studio/live-translator-client-events
- 服务端事件：https://help.aliyun.com/zh/model-studio/live-translator-server-events
- 音色列表：https://help.aliyun.com/zh/model-studio/omni-voice-list
- 模型上下架与更新：https://help.aliyun.com/zh/model-studio/newly-released-models
- Gemini Live Translation：https://ai.google.dev/gemini-api/docs/live-api/live-translate
- Gemini 限流：https://ai.google.dev/gemini-api/docs/rate-limits

## 软件更新与自动发版

“关于 → 检查更新”已接入 Tauri Updater。端点是
`https://github.com/w23x0/Vox/releases/latest/download/latest.json`，
公钥配置在 `app/src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`。

发版是全自动的：推 `v*` tag 触发 `.github/workflows/release.yml`，
CI 自动构建 NSIS 安装包、生成签名与 `latest.json` 并创建 GitHub Release。
本地只需要改版本号（根 `Cargo.toml`、`app/src-tauri/tauri.conf.json`、
`app/ui/package.json` 三处，再刷新 `Cargo.lock`）、提交、打 tag、推送。

签名私钥在 `tools/signing/voxbridge_private.key`（已 gitignore，无密码）。
仓库 Secret `TAURI_SIGNING_PRIVATE_KEY` 的值必须与该文件原文完全一致——
就是那一整行 base64，不要解码后再传。

2026-08-22 密钥轮换过一次：≤0.1.3 的安装无法应用内升级到 0.1.4，
需手动安装一次；从 0.1.4 起自更新正常。
