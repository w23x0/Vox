> 状态：已完成（2026-09-21）｜ P0 / P1 全部落地，更新模块路径 B（模型目录在线更新）已实现（`app/src-tauri/src/catalog_updater.rs`）、路径 A（整程序自动更新）未做——见本文自述与 `docs/architecture/DIRECTIONS.md` §7 ｜ 保留作留痕，别照它施工。

# Vox 三项问题 · 交办给 agent 的修改任务

> 调研结论（2026-08-21）。下面每个问题给出「根因 → 目标 → 改动清单」。方向键是最紧急、改动最小的，全提为 P0；i18n 分两步；更新模块两条路径。

> **状态（2026-08-22，落地后更新）**：本报告除「更新模块」外，其余均已通过 commit `0328077`（P0 方向键 + 状态徽标 + Aliyun 重复 useT）、`07409c1`（i18n 模型/语言/服务商/音色多语）、`a8c52a8`（新增 GPT 服务商）完成。「更新模块」路径 B 已于本次修改落地，改动概览：
> - 后端：新增 `app/src-tauri/src/catalog_updater.rs`；`commands.rs` 加三个命令（`read_catalog_override` / `check_catalog_update` / `apply_catalog_update`）；`Cargo.toml` 加 `reqwest`（复用 rustls-ring 栈）。
> - 前端：`catalog.ts` 改为 runtime 可替换 registry（`ensureCatalogLoaded` 启动载覆盖版，`reload_catalog` 应用后刷新）；`About.tsx` 加「模型目录」检查/应用区；`api.ts` / `mock/backend.ts` 接线。
> - **未改 CSP**：本方案用 `reqwest` 在 **Rust 进程内**拉取（报告第 4 步的 "CSP 放行" 只适用于 `tauri-plugin-http` 那条 '或' 选项），故无需放行 `connect-src`。
> - 分支定 `main`（远程 `w23x0/Vox`）。

---

## P0-A · 方向键控件 Bug（首页无法用 ↓ 导航）

**目标设计**（对齐 `C:\Users\Wang\Desktop\glassui` 的 glassui 规范，`design-system.md:73`）：
> 方向键 = 移动焦点；Enter / Space = 激活/打开/确认；Esc = 关闭。
> 控件**未打开**时绝不拦截方向键；只有「已展开」的控件才吃方向键，且必须 `stopPropagation`。

### Root cause
`app/ui/src/ui/controls.tsx` 的 `Dropdown.onKey`（:107-130，绑定在触发按钮 `:144`）把**未展开**时按 `ArrowDown` 当成「打开」并 `preventDefault()`：

```ts
if (!open) {
  if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    setOpen(true);
  }
  return;
}
```
结果：焦点落在未展开的下拉上时，↓ 永远只是「打开它 / 在选项里走」，**不会移到下一个控件**；且 `App.tsx:154-155` 的 `isDropdownOpenAt` 守卫在打开后把方向键全交给下拉自身 → 首页方向键导航锁死。home 首页大部分控件就是 Dropdown（provider / 语言 / 音色 / 设备），所以损失最明显。

### 改动（对齐 glassui `Select.tsx`）
1. `controls.tsx` 打开条件改成只认 Enter/Space，**删掉 ArrowDown**（ArrowUp 不开，习惯保持不开）：
   ```ts
   if (!open) {
     if (e.key === "Enter" || e.key === " ") {
       e.preventDefault();
       setOpen(true);
     }
     return;
   }
   ```
   → 未展开时按 ↓ 就会冒泡到 `document`，由 `lib/focus.ts` 的 `stepUpDown` 正常移到下一控件。**Bug 本体删除。**
2. 展开后的分支（`controls.tsx` 约 :116-129）加 `e.stopPropagation()`，避免与 `App.tsx` 的 document keydown 争抢（对齐 glassui `Select.tsx :144`）。
3. 关闭下拉时把焦点显式还给触发器按钮（对齐 glassui `closePopup`），保证选中后方向键能立刻继续走网格。

### 已对齐、无需改
- `lib/focus.ts`（网格算法与 glassui `focusGrid.ts` 同源）
- `lib/shortcuts.ts`（只管修饰键组合，不碰裸方向键）
- `sections/Home.tsx`（没有任何自己的键盘处理）
- `App.tsx:154-155` 的 `isDropdownOpenAt` 守卫 **保留**（它是「展开时」才吃方向键的正确实现，改完 1 后它只有在下拉确实展开时生效）。

---

## P0（顺手修）i18n 两个独立 bug

### 2a. 运行状态徽标后端写死中文
- **现状**：后端 `crates/vox-core/src/event.rs:60-85` 写死中文 `state_label`，经 `app/src-tauri/src/dto.rs` 发给前端，`Home.tsx:53`（`{state?.state_label ?? "读取中"}`）原样渲染 → 切英文/日文时仍是中文。
- **改法**：`Home.tsx` 不再信后端 `state_label`，改用字典里已有的 `t("pipeline.state." + state?.state)`（`zh.ts:42-48` 已齐）。后端 `event.rs`/`dto.rs` 的 `label` 字段可保留作内部日志。

### 2b. Aliyun.tsx 重复声明 `useT()` 会导致崩溃
- `app/ui/src/sections/Aliyun.tsx:69` 在 `clearKey` 函数体内部又 `const t = useT();` —— React Hook 不能在回调里调用，运行时会崩，且遮蔽整体顶层 :17 的 `t`。
- **改法**：删掉 :69 那行（组件顶层已拿 `t`）。

---

## P1 · i18n：模型名 / 语言选择 / 服务商名 / 音色名国际化

**结论**：全部来自仓库根 `catalog/*.json`，完全没走 i18n；前端字典 `zh.ts`( :337-347) 已预留几个兜底 key（`catalog.autoDetect`、`catalog.legacyModelSuffix`、`catalog.customVoiceSuffix`…）但代码没用。Rust 侧 compile 期读同一份 JSON（`crates/vox-core/build.rs:90-99`），但只用来校验/归一化，**不把 label 发给前端**，所以改造主要落在前端，不用动 Rust（除 schema 变更场景）。

### 具体改法（分级）
**A. 接线已有 key（零新增字典，先做）**
- `catalog.ts:36` `"自动识别"` → `t("catalog.autoDetect")`
- `catalog.ts:67` `（历史模型）` → `t("catalog.legacyModelSuffix")`
- `voices.ts:40` `（自定义）` → `t("catalog.customVoiceSuffix")`
- 这些是顶层常量/函数拿不到 `t`（hook），需要改成在组件里用 `t()` 二次包装，或让 label 返回 code 由调用方翻译。参考现有 `hotkey.tsx:21-30` 的 `KEY_LABEL_TRANSLATIONS` 模式。

**B. 语言名 / 服务商名 / 模型商品名 / 音色名真正多语种（需新增翻译数据）**
- `catalog/aliyun.json`、`catalog/gemini.json`：给每个 `label` 扩成多语（`{zh,en,ja}`），或保持单 label、在前端字典新增按 code 索引的翻译表。
- `app/ui/src/i18n/zh.ts` / `en.ts` / `ja.ts`：新增成组 key（60 语言 × 3 语、`providers.aliyun`/`gemini`、`models.qwen`/`gemini`、`voices.*`）。因为 `en.ts`/`ja.ts` 用 `satisfies DictShape` 强约束，**三文件必须同步加**。
- `catalog.ts`：`LANGUAGES`/`PROVIDERS`/`MODELS` 的 `label` 改为「按 UI 语言查字典」的函数（不能在模块顶层定死），`languageLabel`/`providerLabel`/`modelLabel` 同步改造。
- `voices.ts`：`LABELS` 改按 UI 语言取；调用方 `Home.tsx`、`Aliyun.tsx`。
- 渲染点全部核对：`Home.tsx`(85-97、227、255-261、310-319、384)、`Aliyun.tsx`(86-91、228-232、262、269、281、292)、`Usage.tsx`(234、246)。

**C. 若选「把翻译放 catalog JSON 的 `{zh,en,ja}`」方案**，则 `crates/vox-core/build.rs` 的数据结构与 schemaVersion 校验也要跟着改（只影响后端校验，不影响展示）。

---

## P1 · 更新模块：先做「只更新模型数据」路径 B

**现状**：更新**完全没做**。
- `About.tsx` 只 `getVersion()` 读本地静态 `v0.1.0`，不联网。
- `app/src-tauri/Cargo.toml` 无 `tauri-plugin-updater`；`tauri.conf.json` 无 `bundle.updater`，CSP `connect-src` 只允许 `'self' ipc:` 挡外网。
- 无 `.github/workflows`、无签名密钥、仅 1 个 tag（`v0.1.0`）。
- 关键：`catalog/*.json` 在前端（`catalog.ts:8-9` `import`）和 Rust（`crates/vox-core/build.rs` 编译期读）都**编译打包进二进制**，在线更新必须改架构。

### 路径 B（推荐，满足你「主要更新模型厂商模型」，改动最小）
1. `app/src-tauri/Cargo.toml`：加 `reqwest`（复用 rustls 栈）或 `tauri-plugin-http`。
2. `app/src-tauri/src/commands.rs` 新增 `check_catalog_update` / `apply_catalog_update`：
   - 拉远程 catalog（GitHub raw：`https://raw.githubusercontent.com/<owner>/Vox/<branch>/catalog/aliyun.json`）。
   - 校验 `schema_version` 与 `verified_at`（复用 `crates/vox/core/build.rs:121-167` 的断言逻辑抽成普通函数）。
   - 落盘 `app_data_dir()/catalog/`（用户可写目录，不写安装目录）。
3. 前端 `catalog.ts` 改为「运行时经 Tauri command 拿：优先读 app_data 覆盖版，回落内置默认版」。
4. `tauri.conf.json` CSP `connect-src` 放行 `https://raw.githubusercontent.com`。
5. `About.tsx` 加「检查模型更新」按钮，展示 `verified_at` 对比。

### 路径 A（程序整体自动更新，以后需要时再做）
需要 `tauri-plugin-updater` + 签名密钥对 + 新建 `.github/workflows/release.yml`（`tauri-apps/tauri-action`）+ CSP 放行 + 前端 `@tauri-apps/plugin-updater`。前置全缺，工作量与路径 B 不可比。

**核心判断：先做路径 B 满足「更新模型厂商模型」的核心诉求；后续要发二进制修复时再叠加 A，两者不冲突。**

---

## 关键路径索引
- 方向键：`app/ui/src/ui/controls.tsx`（:107-130/:144）、`app/ui/src/lib/focus.ts`（骨架对齐）、`app/ui/src/App.tsx`（:154-155 守卫）
- i18n：`app/ui/src/catalog.ts`、`app/ui/src/voices.ts`、`app/ui/src/i18n/{zh,en,ja}.ts`、`app/ui/src/sections/{Home,Aliyun,Usage}.tsx`
- 更新：`app/ui/src/sections/About.tsx`、`app/ui/src/catalog.ts`、`app/src-tauri/{Cargo.toml,tauri.conf.json,src/commands.rs}`、`crates/vox-core/build.rs`
- 数据源：`catalog/aliyun.json`、`catalog/gemini.json`（唯一，前后端共读）