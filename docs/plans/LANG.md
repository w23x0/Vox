# LANG：统一语言登记表 + 语言名本地化显示

状态：Main 已拍板（2026-10-09），本稿为施工与验收依据。接 `MODEL-TABLE.md` M1（方言表）。

## 1. 决定（L1–L5）

| # | 决定 |
|---|---|
| L1 | 内部一律用 BCP-47 规范码（`zh` `en` `ja` `ko` `yue` `es` `nb` `fil`…；要区分书写系统才用 `zh-Hant` 这类）。设置里存规范码；旧设置值迁移到规范码。 |
| L2 | 每条方言声明：源语言集合、目标语言集合、每个目标是"语音+文字"还是"仅文字"、规范码 → 线上码映射。数据在 `catalog/*.json`，`build.rs` 生成。 |
| L3 | 语言名不再手写 zh/ja：前端 `Intl.DisplayNames(uiLang)` 现算，只为 Intl 给不出的码留英文兜底名；排序用 `Intl.Collator(uiLang)`，"最近使用"置顶保留。 |
| L4 | 目标语言"仅文字"⇒ 首页禁用"播放译音"并给一行原因；换模型后当前语言不被新方言支持 ⇒ 回该方言缺省并发通知。 |
| L5 | `SessionParams` 持规范码，发报文时经方言映射成线上码；`vox-core` 内部不再比较服务商线上码。 |

## 2. 数据形状

**`catalog/languages.json`（新，唯一的语言登记表）**

```json
{ "schema_version": 1,
  "languages": [ { "code": "zh", "en": "Chinese" }, … ],   // 60 项；en = Intl 给不出时的兜底名
  "aliases":   { "no": "nb", "tl": "fil", "iw": "he", … } }  // 仅迁移用：旧写法(小写,'-') → 规范码
```

**`catalog/<provider>.json` 的 `dialects.<id>.languages`**（`schema_version` 3 → 4；`languages[]` 与
`expected_counts.languages/audio_output_languages`、`defaults.target_language/listen_target_language` 从服务商级删除）

```json
"languages": {
  "source":            [ … ] | "all_registered",
  "target_audio":      [ … ] | "all_registered",   // 语音+文字
  "target_text_only":  [ … ],                      // 仅文字
  "wire":              { "<规范码>": "<线上码>" }, // 只写不同名的；今天三家都空
  "default_speak_target": "ja", "default_listen_target": "zh",
  "unverified": [ "[未核实] …" ]                   // 只给人看
}
```

`"all_registered"` = 登记表全集，**构建期展开**；只给官方没有逐码清单的情形用（GPT 源语言、Gemini 全部）。
`build.rs` 校验：码都在登记表、集合内不重复、语音/仅文字不相交、缺省目标在目标集合里、wire 键是本方言能说的码、
别名指向已登记的码；阿里云两条方言的 29/31 数量钉在 `expected_counts`。

Rust：`DialectInfo.languages: DialectLanguages { source, target_audio, target_text_only, wire, default_* }`；
`catalog::{canonical_language, normalize_speak_target, normalize_listen_target, normalize_source}`；
`SessionParams::{wire_target, wire_source}`，`audio_enabled` 看自己模型的方言。
前端：`catalog.ts` 的 `languageName / targetLanguageOptions / sourceLanguageOptions / targetMode / resolveTarget / resolveSource`。

覆盖版目录（在线更新落盘的 `app_config_dir/catalog/*.json`）只认 `schema_version >= 4`，旧版没有方言语言表，读进来界面就没语言可选。

## 3. 迁移

`SETTINGS_VERSION` 2 → 3。`Settings::migrate`（`version < 3`）把 `speak.target_language`、`listen.target_language`、
`listen.source_language`、`speak.voice_by_language` 的键用 `canonical_language` 收成规范码：去空白、转小写、`_`→`-`；
整串是登记码就留；是别名就换；否则取主语言子标签（`ja-JP`→`ja`、`zh-TW`→`zh`）。书写系统/区域差异登记表没有单独登记，归主语言。
认不出的值不猜，留给 `normalize` 回落到方言缺省。`voice_by_language` 新旧写法撞键时规范码那条优先。
**现存的 60 个取值本来就是规范码**（阿里云表的码，逐个核过），迁移对它们是恒等。
`normalize` 之后只认规范码原样；`canonical_language` 只给迁移用。
`normalize` 另外按**所选模型的方言**检查：目标语言不被支持回该方言缺省；源语言不被支持回自动识别。

## 4. 测试

- Rust：迁移（登记表 60 码恒等 + 区域/旧 ISO/大小写/撞键/认不出）；规范码→线上码映射机制；
  方言目标语言形态（阿里云 `yue` 仅文字、GPT 无 `sv`）；`audio_enabled` 随方言；切模型回落；`wire_source` 方言不支持则不发。
- 前端 `npm run check:languages`：登记表无 zh/ja 手写名；三种界面语言下 Intl 语言名与 `Collator` 排序；
  仅文字目标禁用播放译音并给原因、换回恢复；切到 GPT 后粤语回落到日语并有通知，GPT 只有 13 个目标语言。
- 变异验证：迁移跳过目标语言、映射忽略、`audio_enabled` 不看方言、不回落、开关不禁用、前端不回落——各自改坏应红。

## 5. `[未核实]`

见各 `catalog/*.json` 的 `unverified`：GPT 13 个目标语言与"70+"源语言、Gemini 的全部语言集合与 `targetLanguageCode` 写法、
qwen3.8 与 qwen3.5 语言表是否相同。
