# Gemini Live Translation 协议

核对日期：2026-08-19。模型为 `gemini-3.5-live-translate-preview`，使用 Google AI Studio API Key。

## 连接与鉴权

```text
wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=<API_KEY>
```

Key 只进入连接 URL，不进入普通设置与日志。`ConnectRequest` 和 `vox-net` 的调试输出都会移除 `?key=` 后的内容。

## 首帧与音频

连接后第一帧是 `setup`，模型写成 `models/gemini-3.5-live-translate-preview`。`generationConfig.translationConfig.targetLanguageCode` 指定目标语言，`echoTargetLanguage` 为 `true`。

上行音频使用 `realtimeInput.audio`：PCM16LE、16 kHz、单声道、base64。下行音频从 `serverContent.modelTurn.parts[].inlineData.data` 读取：PCM16LE、24 kHz、单声道。

译文来自 `serverContent.outputTranscription.text`，源语言来自 `inputTranscription.languageCode`。一条消息可能同时包含音频、转写、`turnComplete` 与 `usageMetadata`，因此解码器会返回多个内部事件。

## 用量与限流

Gemini 的 `usageMetadata` 字段映射如下：

| Gemini | Vox |
| --- | --- |
| `promptTokenCount` | `input_tokens` |
| `responseTokenCount` | `output_tokens` |
| `totalTokenCount` | `total_tokens` |

官方限流按 Google Cloud 项目和模型计算，不按单个 API Key 隔离。RPM 是每分钟请求数，TPM 是每分钟输入 token，RPD 是每日请求数。Live API 使用长连接，20 RPM 如果是连接请求限制，不等于每个音频分块算一次请求。

`RESOURCE_EXHAUSTED`/429 按软错误处理并显示限流提示；连接断开后使用现有指数退避重连。无效 Key、模型无权限和明确的总额度耗尽仍按永久错误停止。

官方资料：

- https://ai.google.dev/gemini-api/docs/live-api/live-translate
- https://ai.google.dev/api/live
- https://ai.google.dev/gemini-api/docs/rate-limits
