> 状态：已完成（2026-09-21）｜ 第一批 T1–T9、D1、D2、R1、R2 已落地，仅 R3（拆 `App.tsx` 的 `document.keydown` 大闭包）从未开工、本文要求单独点头——见 `docs/architecture/DIRECTIONS.md` §6.2 / §7 ｜ 保留作留痕，别照它施工 ｜ **第二十轮状态注（2026-09-22）**：§验收 的 `npm run verify` 链**已扩展**，写稿当时 5 步、现为 9 步，**以 `app/ui/package.json` 的 `verify` 为准**（下文链枚举已按此改写；本文件其余内容仍按写稿当时时点阅读）。

# Vox 前端代码体检 · 执行方案

> 来源：2026-08-22 对 `app/ui/src/` 的代码体检。
> 本文件是**可执行的作业清单**——子代理按本文件干活，不需要再读体检报告原文。
> 用户新开对话评估本方案后再启动子代理。

## 0. 硬约束（子代理必须遵守，违反即算改坏）

1. **视觉来源是 GlassUI**（`C:\Users\Wang\Desktop\glassui`）。不要挑颜色、不要即兴改样式、不要新造设计令牌。改 CSS 只能**删除**死样式，不能改值或新增样式。
2. **运行期依赖极简**：`react` + `react-dom` + `@tauri-apps/api`（+ 已在用的 `plugin-updater`）。**禁止**引入 Tailwind / shadcn / Radix / lucide / 任何图标库 / 任何状态库。图标继续手写内联 SVG（`ui/icons.tsx`）。
3. **前端不持有状态**：一切状态从后端 `Snapshot` 来。本次作业**不碰** store 的事件归约与乐观更新逻辑。
4. **i18n 多语 label 住 catalog JSON 的 {zh,en,ja}**，不在前端字典里硬编码模型名/语言名。
5. **不动 Rust 侧**。`start_pipeline` / `stop_pipeline` / `open_dashscope_console` 这些命令在后端**保留**（成本极低、保持后端完整），只在**前端 `VoxApi` 接口里摘掉**。
6. **不碰**以下刻意设计：单通道事件 `vox://event`、`focus.ts` 列槽导航算法、`Latency.tsx` 的分段计算、catalog 单数据源 + 覆盖版机制。
7. **不改 i18n 三语字典的内容/结构**（除非某条 key 因删组件而彻底没人用，见下文「死 key 清理」——需独立裁决，默认不动字典）。

## 验收（子代理作业完成的硬门槛）

- 根目录 `app/ui` 下跑 `npm run verify`，必须**全绿**。
  - 它串了（**权威源**：`app/ui/package.json` 的 `verify`；写稿当时 5 步，现为 9 步）：`build`（`tsc --noEmit` + `vite build`）→ `check:classes`（类名白名单校验）→ `check:preview` → `a11y` → `qa:narrow` → `check:cable` → `check:capabilities` → `check:agent` → `qa:home`。
  - **重点**：`check:classes` 会校验用到的 CSS 类都在白名单内；删组件时要同步删对应 CSS，否则 `check:classes` 可能报「未使用类」或相反。
- 不允许出现新的 `any`、不允许 `@ts-ignore`/`@ts-expect-error`（除非原代码已有且本次未触及）。
- 不允许改 `types.ts` / `types.snapshot.ts` 的字段名（snake_case 照抄 Rust serde）。

## 怎么读本文件

- 每项作业标了 **【改动文件】、【动作】、【风险】、【验证点】**。
- 严重度沿用体检报告：🔴高 / 🟡中 / 🟢低。
- 按下面「建议执行顺序」分批做，每批跑一次 `npm run verify`。

---

## 第一批：死代码清理（低风险，先做）

> 这一批全部是「零引用残留」，删了之后 `tsc --noEmit` 能直接证明没删错（删错会报找不到符号）。做完跑 `npm run build` 即可确认。

### T1 🟢 删 `LevelMeter` 组件 + 关联死 CSS
- **【改动文件】**
  - `app/ui/src/ui/controls.tsx`：删 `LevelMeter` 函数（当前约 **252–296 行**，从 `/* ---- 电平条 ---- */` 注释到 `}` 结束）。
  - `app/ui/src/styles/components.css`：
    - 删电平条三段：`.meter`（**1210**）、`.meter-tick`（**1214**），以及其上注释 `/* 电平条：progress-track 加一根门限刻线… */`（**1209**）。
    - ⚠️ **不能删** `.progress-track`（**764**）/`.progress-fill`（**773**） + dark 变体（**781**）——需先确认除 `LevelMeter` 外无人用。已验证：全仓 `progress-track`/`progress-fill` 的 className 仅出现在 `controls.tsx:270/284`（即 `LevelMeter` 内部）。删掉 `LevelMeter` 后这组进度条 CSS 变孤儿，**一并删 762–783 整块**（含 `/* ---- 进度条 ---- */` 注释）。
- **【动作】** 删除函数与 CSS，不补替代。
- **【风险】** 🟢 极低。`tsc` 会立刻报错若有暗引用；`check:classes` 会校验类名清理是否干净。
- **【验证点】** `grep -rn "LevelMeter\|progress-track\|progress-fill\|meter-tick" app/ui/src` 应只剩 0 处（CSS 里若 `.meter` 单独被别处用到——已确认没有）。

### T2 🟢 删 `HotkeyCombo`（只读热键展示）
- **【改动文件】** `app/ui/src/ui/hotkey.tsx`：删 `HotkeyCombo` 函数（**33–52 行**）。
- **【动作】** 删除。`hotkey.tsx` 里 `KEY_LABEL_TRANSLATIONS`、`keyDisplay` 仍被 `HotkeyEditor` 用着，**保留**。
- **【风险】** 🟢 已验证零引用。
- **【验证点】** `grep -rn "HotkeyCombo" app/ui/src` = 0。

### T3 🟢 删 `stateTone` / `StateTone`
- **【改动文件】** `app/ui/src/pipeline.ts`：删 `StateTone` 类型（**25 行**）与 `stateTone` 函数（**27–40 行**）。
- **【动作】** 删除。`STATE_LABEL`（store + mock 用）和 `isRunning`（mock 用）**保留**。
- **【风险】** 🟢 已验证零引用。
- **【验证点】** `grep -rn "stateTone\|StateTone" app/ui/src` = 0。

### T4 🟢 删 `PIPELINE_LABEL`
- **【改动文件】** `app/ui/src/pipeline.ts`：删 `PIPELINE_LABEL`（**42–45 行**）。这是 i18n 化前的中文硬编码残留，展示处全走 `t("pipeline.speak")`。
- **【动作】** 删除。
- **【风险】** 🟢 已验证零引用。
- **【验证点】** `grep -rn "PIPELINE_LABEL" app/ui/src` = 0。

### T5 🟢 从前端接口摘掉 `startPipeline` / `stopPipeline`（后端命令保留）
- **【改动文件】**
  - `app/ui/src/api.ts`：
    - 接口 `VoxApi` 删两行：`startPipeline(...)`（**42**）、`stopPipeline(...)`（**43**）。
    - `createTauriApi()` 删两行实现：`startPipeline:`（**90**）、`stopPipeline:`（**91**）。
  - `app/ui/src/mock/backend.ts`：
    - 返回对象里删 `startPipeline: start,`（**359**）、`stopPipeline: stop,`（**360**）。
    - **保留**局部函数 `start`/`stop`（**245–269**）——`togglePipeline`（**361**）内部还在调用它们。
  - **不动** `app/src-tauri/src/lib.rs` / `commands.rs` 的 Rust 命令。
- **【动作】** 接口层摘除，mock 内部实现保留。
- **【风险】** 🟢 UI 仅用 `togglePipeline`（`Home.tsx:509`），已验证。后端不受影响。
- **【验证点】** `grep -rn "startPipeline\|stopPipeline" app/ui/src` 仅剩 mock 内部 `start`/`stop` 局部函数（不再出现在 `VoxApi` 接口或对象字面量里）。

### T6 🟢 从前端接口摘掉 `openDashscopeConsole`（后端命令保留）
- **【改动文件】**
  - `app/ui/src/api.ts`：删接口行 `openDashscopeConsole(): Promise<void>;`（**54**）与实现 `openDashscopeConsole: ...`（**104**）。
  - `app/ui/src/mock/backend.ts`：删 `async openDashscopeConsole() {...}`（**415–417**）。
  - **不动** Rust 的 `open_dashscope_console` 命令（`commands.rs:490`）。
- **【动作】** 摘除。服务商页用通用的 `openProviderConsole`。
- **【风险】** 🟢 已验证 UI 无调用。
- **【验证点】** `grep -rn "openDashscopeConsole\|openDashscope" app/ui/src` = 0。

### T7 🟢 删 `MockSettings` 假导出 + 收紧 `backend.ts` 的 import
- **【改动文件】** `app/ui/src/mock/backend.ts`：
  - 删 `export type MockSettings = Settings;`（**444–445**，含其上注释）。
  - 第 **10 行** `import type { PipelineName, PipelineState, Settings, Track } from "../types";` —— 删掉其中的 `Settings`（确认 `backend.ts` 函数体不再直接用 `Settings` 类型；`normalizeSettings` 的入参类型来自 `merge.ts` 自己的 `Settings` import，与本文件无关）。若 `tsc` 报 `Settings` 仍被用到，则保留——以编译器为准。
- **【动作】** 删除假类型；按 `tsc` 结果决定是否从 import 去掉 `Settings`。
- **【风险】** 🟢 编译器会兜底。
- **【验证点】** `npm run typecheck` 通过；`grep -rn "MockSettings" app/ui` = 0。

### T8 🟢 删 `Snapshot.has_api_key`（仅前端类型 + mock 填充，后端保留）
- **【改动文件】**
  - `app/ui/src/types.snapshot.ts`：删 `has_api_key: boolean;`（**117**）。
  - `app/ui/src/mock/backend.ts`：删 `has_api_key: ...`（**274**）。
  - **不动** Rust `dto.rs`（`has_api_key` 字段 + 序列化保留——后端发多余字段前端忽略即可，serde 宽松）。
- **【动作】** 前端类型与 mock 去掉该字段。
- **【风险】** 🟢 已验证前端零读取（UI 读 `api_keys[provider]`）。后端多发的字段前端类型不声明不影响反序列化。
- **【验证点】** `grep -rn "has_api_key" app/ui/src` = 0；`npm run typecheck` 通过。

### T9 🟢 收掉 `fmtNum` 的死参数 `lang`
- **【改动文件】** `app/ui/src/lib/format.ts`：
  - `fmtNum` 签名改成 `export function fmtNum(n: number): string`，函数体直接 `return NUM_CN.format(Math.round(n));`。
  - 删 `NUM_EN` 常量（**6 行**），保留 `NUM_CN`。
  - **不要**去改 6 处调用方（它们本来就没传第二参，签名收窄后调用不变）。
- **【动作】** 收窄签名。中英千分位都是逗号，行为不变。
- **【风险】** 🟢 已验证无调用方传 lang。
- **【验证点】** `npm run typecheck` 通过（若有暗调用传了第二参会立刻报错）。

---

## 第二批：去重（中等风险）

### D1 🟡 提取公共 `ConfirmButton`，替换 `Aliyun.tsx` 的手写两段式确认
- **【改动文件】**
  - 新建 `app/ui/src/ui/ConfirmButton.tsx`：把 `app/ui/src/sections/Usage.tsx:80–145` 的 `ConfirmButton` 原样搬过来（含 `useT`、`useEffect` 清 timer、armed + 5s 退回）。注意它是**纯展示控件**，不该 import `store`。
  - `app/ui/src/sections/Usage.tsx`：删本地的 `ConfirmButton`（**80–145**），改成 `import { ConfirmButton } from "../ui/ConfirmButton";`。两处用法（**242–251**、**286–297**）保持不变。
  - `app/ui/src/sections/Aliyun.tsx`：把清密钥那块手写的 `armed` state（**23 行** `const [armed, setArmed] = useState(false);`）+ `timer` ref（**26 行**）+ JSX 里的两段式（**173–213**）替换成 `<ConfirmButton>`。
    - 注意 `Aliyun.tsx` 的清密钥按钮当前带了 `<IconTrash>` 图标和 `disabled={busy !== null}`——`ConfirmButton` 的 props 要支持 `disabled`（已有）和 children 里放图标（已支持，children 是 ReactNode）。
    - 删掉 `Aliyun.tsx:38–42` 那个只用于清理手写 timer 的 `useEffect`（换成 `ConfirmButton` 后不再需要）。
- **【动作】** 提控件 + 换两处调用。
- **【风险】** 🟡 `Aliyun.tsx` 清密钥的「确认态文案」要对应到 `ConfirmButton` 的 `confirmText`（现在是 `t("providersPage.confirmClear")`），行为必须等价（点一下变确认、5 秒退回、第二下执行）。
- **【验证点】**
  - `npm run a11y` 通过（两段式确认的无障碍属性要保留：确认按钮仍带 `data-focus-item`）。
  - 手动/mock 验证：`?mock=1` 下进服务商页，点「清空密钥」→ 变「确认清空」→ 5 秒自动退回；点第二次→执行 `clearKey`。
  - `grep -rn "setTimeout(() => setArmed(false), 5000)" app/ui/src` 只剩 `ConfirmButton.tsx` 一处。

### D2 🟢（可选，建议）合并 `speakVoiceOptions` / `listenVoiceOptions`
- **【改动文件】**
  - `app/ui/src/voices.ts`：在 `orderedVoices` 旁边加一个 `voiceOptions(uiLang, voice, recent, t)`，把「`orderedVoices(...)` + `map` 加 `defaultVoiceSuffix`」打包（`Home.tsx:92–109` 两段逐字相同的 map）。
  - `app/ui/src/sections/Home.tsx`：`speakVoiceOptions`/`listenVoiceOptions` 改调 `voiceOptions`。
- **【动作】** 小幅去重。
- **【风险】** 🟢 纯逻辑搬运，输出必须字节级一致。
- **【验证点】** `npm run build` 通过；下拉选项数量/顺序/文案在 mock 下肉眼或 `qa:narrow` 不变。
- **【备注】** 此项依赖第一批的 T 项做完后 `Home.tsx` 的状态。如果第三批要拆 `HomePage`，**建议把 D2 并进第三批一起做**，避免改两遍。

---

## 第三批：巨型组件拆分（高风险，单独裁决）

> ⚠️ 这一批**改动面大**，强烈建议**单独开一轮评估**确认拆分边界后再动手。
> 子代理执行时若用户只批了第一批 + D1，**跳过第三批**。

### R1 🔴 拆 `HomePage` 单卡 `map`（`Home.tsx` 549 行）
- **【现状】** `CARDS.map` 的回调体（**155–516**，约 360 行）里，speak/listen 的全部差异靠贯穿 JSX 的 `card.id === "speak"` 三元表达：目标语言 vs 源语言（**236–307**）、音色选项分支（**320–329**）、输出设备（**353–386**）、speak 的输入设备块（**397–451**）vs listen 的监听目标块（**453–501**）、textOnlyHint（**390–394**）。
- **【改动文件】** `app/ui/src/sections/Home.tsx`（可能新增同目录子文件）。
- **【动作】** 建议拆分（子代理按评估后确认的边界执行，以下是建议草图）：
  1. 抽 `<PipelineCard pipeline={"speak"|"listen"}>`：承载两张卡**共有**的部分（图标/状态徽标/blocked 提示/启停按钮/provider 下拉）。
  2. speak 与 listen 的差异分别抽 `<SpeakCardExtras>` / `<ListenCardExtras>`（目标语言 vs 源语言 + 对应 Toggle、输入设备 vs 监听目标、子进程开关、textOnlyHint）。
  3. 公共纯逻辑下沉：`deviceOptions`（**28–44**，已是模块函数，保留）、`pickApp`（**127–139**）、`blockedBy`（**142–150**）、`byExe` 去重（**112–117**）——这些可挪到一个 `home-logic.ts` 或留在 `Home.tsx` 顶部作模块函数。
  4. `HomePage` 的 `return` 只剩 `CARDS.map(card => <PipelineCard .../>)` + 底部 `Latency` 面板。
- **【绝对保留】**（拆分时不能丢的行为）：
  - `data-focus-item` 必须保留在所有可聚焦控件上（焦点网格依赖，见 `focus.ts`）。
  - 每个 `Dropdown` 的 `id` / `label` / `aria-*` 必须原样保留（a11y + `qa:narrow` 会查）。
  - `patch({...})` 的字段路径与 `voice_by_language` 合并语义（**246–261**、**334–349**）一字不改。
  - `SYSTEM_DEFAULT` 设备哨兵（**26 行**）逻辑不变。
- **【风险】** 🔴 高。这是本次最大的改动，回归面广（首页是主交互页）。
- **【验证点】**
  - `npm run verify` 全绿。
  - `npm run a11y` + `qa:narrow` 重点看首页。
  - mock 下逐项点测：切 provider（模型名跟随）、切目标语言（voice 跟随 + voice_by_language 累积）、切源语言、音色选项、输出/输入设备、监听目标、子进程开关、启停按钮的 disabled 联动（`blockedBy`）。

### R2 🟡 拆 `SettingsPage` 的 Cable 管理 + 卸载对话框
- **【现状】** `Settings.tsx`（371 行）一个组件管 cableBusy 5 态、uninstallDialog、3 个异步动作、3 个徽标计算、整段卸载对话框 JSX；`cableStatusLabel` 是 4 层嵌套三元（**40–48**）。
- **【改动文件】** `app/ui/src/sections/Settings.tsx`（可能新增 `<CableManager>` 子组件，同目录或 `Settings/` 子目录）。
- **【动作】** 建议拆分（同上，按评估确认的边界执行）：
  1. 抽 `<CableManager>`：吃 `snapshot`/`api`/`toast`/`applyCableChannelStatus`，封装虚拟麦克风 + 多声道 + 卸载对话框（**129–219** + **304–368**）。
  2. `cableStatusLabel` / `cableBadgeClass` / `channelBadgeClass` 的嵌套三元改 `Record<状态, 值>` 查表。
  3. 激活方式 + 热键（**246–302**）留在 `SettingsPage` 或抽 `<ActivationHotkeys>`。
- **【绝对保留】**：`applyCableChannelStatus` 的「后端回报即权威、不等 devices_changed」语义（**106–109** 注释）；卸载对话框的 `role="dialog" aria-modal` 与 `data-focus-item`。
- **【风险】** 🟡 中。Cable 状态机分支多但相对内聚。
- **【验证点】** `npm run verify` 全绿；mock 下走一遍 install/uninstall/blockers/16 声道显隐的徽标与 toast。

### R3 🟡 拆 `App.tsx` 的 `document.keydown` 超大闭包（可选）
- **【现状】** `App.tsx:99–242` 的 `onKeyDown` 约 145 行，串了快捷键/Tab/Ctrl+Enter/方向网格/PageUp-Down/Home-End/文本编辑态。
- **【改动文件】** `app/ui/src/App.tsx` + 可能新增 `lib/nav-route.ts`。
- **【动作】**（可选，优先级低于 R1/R2）把按键路由抽成纯函数 `routeNavigationKey(e, ctx): boolean`，`App` 只挂监听 + 提供 `ctx`（当前页、`selectPage`、focus 工具）。
- **【风险】** 🟡 中（键盘导航是焦点网格的入口，错了很难肉眼发现）。
- **【验证点】** `npm run verify` 全绿；mock 下逐键验证方向网格/Tab 红绿灯/Ctrl+Tab 切页/Ctrl+, 进设置/Esc 退编辑。

---

## 建议执行顺序

1. **第一批（T1–T9）一次性做完** → `npm run verify`。全绿后这批就是独立可交付的死代码清理。
2. **D1** → `npm run verify`。
3. **D2** 视情况：若要做 R1，则 D2 并入 R1 一起做；若不做 R1，D2 单独做。
4. **第三批（R1/R2/R3）单独评估**：每一项都需要用户在新对话里单独点头才动手，因为风险高、回归面广。

## 不在本方案范围内（体检提到但刻意不动）

- 内联 `style={{ marginTop }}` 散落（维度一-3）：要对 GlassUI 间距令牌才能动，本次不碰。
- `Sidebar` 的 `theme` 镜像状态（维度二-4）：架构一致性问题，不在本次清理目标内。
- `Slider` 只有字幕页一个调用方（维度四-1）：不算缺陷，保留。
- `toggleFullscreen`/`hasCmd` 的 Mac 分支（维度四-3）：跨平台延后项（见 roadmap），符合现状，不动。
- i18n 三语字典的死 key（删组件后可能残留的 key）：默认不动字典；若用户想顺带清，单独列清单人工裁决。

---

## 子代理作业须知

- 每完成一批（第一批整体视为一批；D1 一批；R 每项各一批），跑一次 `npm run verify`，**贴出完整输出**到回复里。
- 任何一项 `tsc`/`check:classes` 报错，**停下来报告**，不要绕过（不用 `@ts-ignore`、不改字段名硬塞）。
- 删 CSS 前用 `grep -rn "类名" app/ui/src` 确认无 HTML/JSX 引用（已验证的结论写在本文件对应项里，但执行时子代理应复核一次）。
- 遵守用户的交流偏好：**用中文回复**（代码/注释可英文）。
- 前端验证**不看截图**（用 `npm run verify` 的程序化断言）。
