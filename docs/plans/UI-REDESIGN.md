# UI 重设计（面向全球用户）

状态：Main 已拍板（2026-10-09）。本稿是施工依据；与代码冲突以代码为准并在报告里写明。

## 0. 为什么重做

现有界面是为中文开发者自用设计的，对全球用户有硬伤：

1. **字体**：全界面用打包的等宽字体（JetBrains Mono + Maple Mono CN 子集，只含 GB2312 汉字）。日文汉字、韩文、西里尔、阿拉伯、泰文等全部掉回系统字体，混排参差；等宽英文读起来像终端。中文还被加了字距，显得松散。
2. **导航只有图标**：靠悬停提示才知道是什么，首次使用的人找不到"模型服务商"在哪个钥匙图标后面。
3. **语言切换**是侧栏底部一个"中"字循环按钮，不知道能切、也看不到有哪些语言。
4. **窗口按钮**永远是 macOS 红黄绿，在 Windows / Linux 上不对。
5. **模型**：服务商页只有一个服务商下拉 + 静态能力表，没有"选哪个模型"，也没有"这个密钥能用哪些模型"。
6. **文案**里有开发术语（"这个构建里没编进去：构建时没开对应的 cargo feature"）。

## 1. 拍板总表

| # | 决定 |
|---|---|
| D1 | 界面字体改**系统无衬线栈**，按 `:lang()` 分语言给 CJK 字体；等宽只用于 ID / 密钥 / 数值 / 快捷键。删掉打包的 Maple Mono CN 与 JetBrains Mono 的界面用途（JetBrains Mono 拉丁子集可留作 `--font-mono`）。正文任何地方**不加字距**。 |
| D2 | 侧栏改**图标 + 文字**（宽 200px），窗口宽 < 860px 时收成纯图标（保留 tooltip）。分组见 §3。 |
| D3 | 界面语言改为**设置页里的语言下拉**（每项用该语言自己的名字：简体中文 / English / 日本語），首次启动按系统语言选，系统语言不支持就用 English。侧栏底部的"中"按钮删掉。i18n 缺键回退 English，不回退中文。 |
| D4 | 主题（跟随系统 / 浅 / 深）也进设置页；侧栏底部的月亮按钮删掉。 |
| D5 | 窗口按钮**按平台**：macOS 左上红黄绿；Windows / Linux 右上 最小化 / 最大化 / 关闭（线性图标）。平台用 Tauri `os` 信息或 `navigator.userAgent` 判，mock 模式下可用 `?platform=` 覆盖。 |
| D6 | "模型服务商"页重做为 **"模型"** 页（§5）：三家服务商（阿里云百炼 / OpenAI / Google Gemini）、各自的密钥与地区、**现场拉官方模型列表**。 |
| D7 | 首页每条管线卡片显示并可切换 **服务商 · 模型**（§4）。 |
| D8 | 用户可见文案去开发术语；构建缺功能统一说"此版本不包含该功能"。 |
| D9 | 不改视觉基调：保留现有配色 token、圆角、卡片、浅/深色。这是**结构与排版**的重做，不是换皮。 |

## 2. 字体与排版（D1）

`styles/tokens.css`：

```css
--font-ui: system-ui, -apple-system, "Segoe UI", "Noto Sans", Roboto, "Helvetica Neue", Arial, sans-serif;
--font-mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, monospace;
:lang(zh-CN) { --font-cjk: "PingFang SC", "Microsoft YaHei UI", "Noto Sans CJK SC", "Source Han Sans SC"; }
:lang(ja)    { --font-cjk: "Hiragino Sans", "Yu Gothic UI", "Meiryo UI", "Noto Sans CJK JP"; }
:lang(ko)    { --font-cjk: "Apple SD Gothic Neo", "Malgun Gothic", "Noto Sans CJK KR"; }
body { font-family: var(--font-ui-resolved); } /* = 拉丁 system-ui 在前，CJK 栈在中，sans-serif 兜底 */
```

- `<html lang>` 跟随界面语言（zh-CN / ja / en），这样 `:lang()` 生效、日文汉字用日文字形。
- 删 `fonts.css` 里 Maple Mono CN 的三个字重和 `tools/build-fonts.mjs` 对它的处理；JetBrains Mono 只保留 Regular 拉丁子集。安装包因此变小（约 5MB）。
- 全局去掉正文 / 标题 / 标签上的 `letter-spacing`（数值类负字距可留）。
- 页面标题字号 22px / 700，不再用强调色，用 `--text`。
- 文字容器不设固定宽度，标签列用 `minmax(…, max-content)`，保证德语 / 西语长文案不截断（为后续加语言铺路）。

## 3. 外框与导航（D2、D4、D5）

侧栏分组（组标题小号灰字，纯图标模式下组间用分隔线）：

| 组 | 项 |
|---|---|
| — | 首页 |
| 翻译 | 模型、字幕 |
| 连接 | VRChat、Agent 控制 |
| 系统 | 设置、用量、关于 |

- 侧栏底部只保留应用版本号（小号灰字）；有更新时显示一个"有更新"小徽章，点击跳关于页。
- 选中态：浅强调底 + 左侧 3px 强调条（沿用现有样式）。
- 键盘焦点系统（`lib/focus.ts`、`data-focus-item`）必须继续工作：方向键在侧栏内上下移动，Tab 进入内容区。

## 4. 首页（D7）

两张管线卡片（对外说话 / 听人说话）结构统一：

```
┌───────────────────────────────────────────────┐
│ [图标] 对外说话          ● 运行中     [ 停止 ] │  ← 头部：名称、状态、主按钮（主按钮放这里，不放卡底）
│ 阿里云百炼 · Qwen3.8 LiveTranslate   [更换]   │  ← 服务商·模型（点"更换"弹出选择）
│                                               │
│   自动识别  ⇄  日语                            │  ← 语言对：源（可"自动识别"）→ 目标，大号下拉
│                                               │
│ 麦克风 [系统默认 ▾] ⟳                          │  ← 设备
│ ▸ 更多：译音音色 / 译音输出 / 显示译文 / 回听   │  ← 次要项折叠，展开状态记在 localStorage
└───────────────────────────────────────────────┘
```

- 模型选择器：先选服务商（只列已配置密钥的；未配置的灰显并附"去配置"链接到模型页），再选模型（只列 `adapted=true` 的，见 §6）。
- 服务商能力决定字段显隐：不支持选音色就不显示音色；不支持源语言就把源语言固定显示"自动识别"且禁用。沿用现有 `capabilities.ts`。
- 延迟卡片保留在两卡下方。
- 宽度 < 860px 时两卡上下排。

## 5. 模型页（D6）

左侧服务商列表（三项，每项显示状态点：已配置 / 未配置 / 出错），右侧该服务商详情：

1. **密钥**：已配置时显示"已配置 ••••末四位"+「更换」「清除」；未配置时输入框 +「保存」+「获取密钥」外链。
2. **地区 / 接入点**（仅阿里云）：`中国内地（北京）` / `国际（新加坡）`；「高级」折叠里放"自定义接入点"输入框（`wss://…`，空=按地区缺省），附一行说明：工作空间专属地址可在百炼控制台找到。
3. **可用模型**（核心）：
   - 标题行：「可用模型」+ 来源标签（`官方列表 · 3 分钟前` 或 `内置列表`）+「刷新」按钮。
   - 每行：展示名、模型 ID（等宽小字）、徽章：`推荐` / `已适配` / `未适配`（灰，不可选）/ `将于 2026-12-01 下线`（警告色）/ `快照`。
   - 行内展开显示能力：语言数（译音 N 种 / 仅译文 M 种）、可选音色、声音复刻、可指定源语言。这些来自内置的方言表（§6），不是官方列表。
   - 拉取失败时显示原因：密钥无效 →"密钥无效或无权限"；网络 →"无法连接 {host}。如果你所在地区无法直接访问该服务，需要先配置网络代理。"（不提具体国家）；然后退回内置列表并标注。
4. 原"模型能力"大表（语言 chip 列表）移到每个模型行的展开里，不再单独占一屏。

## 6. 前后端契约（UI 先按 mock 实现，Rust 侧在模型表工单里实现）

新 Tauri 命令 `list_models`：

```ts
// api.ts
listModels(provider: ProviderId, opts?: { refresh?: boolean }): Promise<ModelList>

interface ModelList {
  provider: ProviderId;              // "aliyun" | "gpt" | "gemini"
  source: "official" | "builtin";    // official = 本次/缓存里来自服务商官方列表接口
  fetched_at: number | null;         // ms epoch；builtin 时为 null
  error: null | { kind: "auth" | "network" | "other"; host: string; message: string };
  models: ModelEntry[];
}
interface ModelEntry {
  id: string;                        // 调用用的模型 ID
  label: string;                     // 展示名（官方给的，没有就用 id）
  adapted: boolean;                  // 有对应协议方言才 true；false 不可选
  recommended: boolean;
  snapshot_of: string | null;        // 是某稳定版的快照
  offline_at: string | null;         // YYYY-MM-DD，官方预计下线日
  capabilities: { voice_selection: boolean; voice_clone: boolean; source_language: boolean; hot_update_language: boolean };
  languages: { audio: string[]; text_only: string[] } | null;   // 语言代码
}
```

- 设置结构里每条管线已有 `provider` 与 `model_name`；首页更换模型就是写这两个字段（现有 `updateSettings` 路径）。
- mock（`src/mock/backend.ts`）要覆盖：三家各一份正常列表、阿里云含一个 `adapted=false` 与一个带 `offline_at` 的条目、OpenAI 返回 `network` 错误的分支（用 `?mockErr=gpt` 触发）。
- `types.snapshot.ts` 若是从 Rust 生成的快照，新增类型先写在 `types.ts`，注明"待 Rust 侧 list_models 落地后进快照"。

## 7. 设置页

顶部新增「通用」卡：界面语言（D3）、主题（D4）、开机自启、启动后最小化（从原卡移来）。其余卡片（虚拟麦克风、快捷键）保持，文案按 D8 清理。

## 8. 不做 / 后续

- 本轮不新增界面语言的翻译（只做 zh-CN / en / ja 三种的完整覆盖与回退机制）；韩语、西语等后续单独工单。
- 不做首次启动引导（后续工单，依赖本轮的语言与模型页）。
- 不动 Rust 侧（`list_models` 由模型表工单实现）；本轮 UI 走 mock 与现有命令。

## 9. 验收

- `npm run verify` 全绿（里面的 a11y / 窄屏 / 首页等检查脚本若因结构**有意**改变而失配，改脚本以匹配新结构，并在报告里逐条说明改了什么断言、为什么）。
- 截图：浅色 / 深色 × zh-CN / en / ja × 1040×780 与 820×640，覆盖首页、模型页、设置页；放 `/tmp/claude-1000/ui-after/`。
- `en` 与 `ja` 下无中文残留（脚本扫描渲染后的文本节点）。
- 打包字体删除后 `npm run build` 产物体积对比写进报告。
