> 状态：已完成（2026-09-21）｜ 正文“下一步”已落地：`app/ui/src/lib/focus.ts` 已是物理列槽模型（`pickBestSlotNearest` / `bestSlot`），见 `docs/architecture/DIRECTIONS.md` §7；整份交接单作废 ｜ 保留作留痕，别照它施工。

# 交接 / 转交提示词 · Vox 首页布局 + 焦点聚焦

> 本文件是交给"续接上下文"的交接单。当前会话已近上下文上限，可能被压缩/重开。
> 从此文件接着干，不要依赖本会话前面的对话。

## 一、这次要解决的诉求（用户原话，大白话）

用户要的是**首页 speak/listen 两张卡里控件的"二维网格焦点"像文本编辑光标一样**：

- 网格里每个控件有 (行, 列)，行 . 列是真实坐标。
- `←→` 同一行内左右移一列。
- `↓`/`↑` 在上下行间移动时**保持列**（从 `12` 下到 `22` 再 `↑` 必须回 `12`，不能漂）。
- **要求"列记忆"**：即使目标行没有那么多列，也要尽量保持当前那一列/物理位置，而不是随便靠边。
- **用户强烈反对"跨卡"**：从 speak 卡片里一个控件按 ↓，跳到 listen 卡片另一半的开关，是 bug。但注意，用户自己说的是"像编辑文本的光标"，**不要引入"卡"概念**，就是二维格子 + 列记忆。

## 二、当前代码状态（重要：不要逆）
- 工作目录：`C:\Users\Wang\Desktop\Vox`
- 分支 `main`，当前是**脏工作区**（很多文件从一开始就是未提交/修改，不只是我改的）。**只动这两个：
  - `app/ui/src/lib/focus.ts`（我改过，见下）
  - `app/ui/src/sections/Home.tsx`（我改过，见下）

### focus.ts —— 真实情况
`git diff` 显示 **`stepUpDown` 的函数体逻辑几乎没有变**，我最后只改注释。**真正的问题不在 stepUpDown，而在 `buildGrid` 的"行/列划分"** 导致列号对上错，让用户看到"13→21 漂移"。

`buildGrid`（focus.ts）现状：
- 把 `[data-focus-item]` 全部平铺，按 `top` 8px 容差分"行"，行内按 `left` 排 = 列号。
- **缺陷**：
  1. 两张卡左右并排时，同 top 的 speak+listen 控件被并进**同一行**（实测行0 = speak壮/listen提供者等 4 列）。这本身不算错，但会：
  2. **卡片内的"开关/附属控件"被 leave 进"单独一行"而不是嵌在它所属控件下方**——用户的心智是"开关在语言控件正下方"，但 grid 把 toggle 排成"下一行"，导致 `12(listen服务商)` 的 ↓ 落点奇怪。
  3. 列号是"行内 index"而非"竖直物理列"，所以 `13(col2)`↓到只有2列的行会 clamped 落到第2格，看起来像"跳到21"。
- **用户真正要的列记忆/光标**：必须按"同一竖直列(x 对齐 / 行内 index 稳定)"来移动，且上下跨行时**保持列**，而不是靠 getBoundingClientRect 的最近中心。

### Home.tsx —— 我的改动（对着 diff）
1. 把"显示译文/播放译音" Toggle **从 voice 行的 `.pipeline-config-head` 里移到语言(Dropdown)格正下方**，用一个 `.pipeline-inline-toggle`（加了 inline marginTop:8,width:100%,justifyContent:space-between）。
2. 移走了 voice 行里原来的 `.pipeline-config-head`（含 inline-toggle），voice 行现在只有 voice label + voice Dropdown。
3. i18n 里我之前加的 `思 outputNeedsPlay` 已删干净（zh/en/ja 都回到 HEAD）。
4. **输出设备块**：之前改成"始终渲染+始终可用"，但**这处最终的 diff 是否保留要看 git**（上面 diff head 只显示第一段，可能有更多输出块改动）。

**请继续仔细读 Home.tsx 当前完整内容**，我上面指出可能还有"输出设备始终渲染"残留（那块我改过，但这段 diff 被截断）。用 `git diff app/ui/src/sections/Home.tsx` 看全量。

## 正确的下一步（给续接者）
1. 先把 `git diff app/ui/src/sections/Home.tsx` 和 `git diff app/ui/src/lib/focus.ts` 完整读一遍，确认当前工作区到底变成什么样。
2. 不要凭空重写 grid。**先起 dev + Playwright 实测首页焦点路径**（脚本见下方），记录当前 `buildGrid` 每行每列到底怎么划分，确认"12↓→21"的复现路径。
3. 用户心魔模型是**纯二维网格 + 列记忆 + 不引入卡**。要达成它，**不是改 stepUpDown**，而是改 `buildGrid` 的"列"定义：让"列"代表"竖直对齐的物理列槽位"（按每行所有控件的 x 分类成条），并且 ↓↑ 保持"列槽位"而不是"行内 index"。列记忆 = 记住上次的行内物理列位置（x），在下一行找 x 最接近的（这是原实现"center 最近"？但用户说那不是他要的）。

   ⚠ 用户在对话里明确：旧的"物理中心最近"这版造成 `13→21`，他觉得坏。他一再用"列记忆 12→22→12"举例，说明他要的是**稳定列号(clamping)** + **下↕回原列**，而不是物理最近。
   判断依据 —— 分别先实测这两个算法在 `12 13 14 / 21 22` 上的表现再定：
   - A：`col = min(cur.col, targetRow.length-1)`（当前实现，column index 夹紧）
   - B：物理 x 中心最近（我上一个版本，用户说造成 13→21）
4. **不要引入"卡"概念**，不要在 focus.ts 用 `.stat-card` 特判。格子在边缘就不存在垂直牵连一致性处理——除非用户确认要加.
5. 开工前 `npm run verify` 通过（包含 a11y 键盘遍历断言、强制 select. 窄窗 qa:narrow、check:cable）。改动别破坏 a11y：`silentDisabled` 禁用的必须有可见原因或 `data-block-disabled`。

## 四、推荐实测脚本（playwright，需在 app/ui 下跑，playwright 在 node_modules）
```js
// 在 app/ui 下 node focus-test.mjs
import { chromium } from "playwright";
const browser = await chromium.launch();
const p = await browser.newPage({viewport:{width:1040,height:780}});
await p.goto("http://127.0.0.1:PORT/?mock=1",{waitUntil:"networkidle"});
await p.waitForTimeout(600);
const who=()=>p.evaluate(()=>{const a=document.activeElement;return a?(a.id||a.tagName+"."+String(a.className).split(" ")[0]):"null";});
// 打印每行列归属
const grid=await p.evaluate(()=>{
  const items=[...document.querySelectorAll("[data-focus-item]")].filter(el=>!el.closest('[data-focus-zone="sidebar"]')&&el.offsetParent!==null);
  const byTop=new Map();
  for(const i of items){const t=Math.round(i.getBoundingClientRect().top);if(!byTop.has(t))byTop.set(t,[]);byTop.get(t).push({id:i.id,left:Math.round(i.getBoundingClientRect().left)});}
  return [...byTop.entries()].sort((a,b)=>a[0]-b[0]).map(([t,a])=>({top:t,row:a.sort((x,y)=>x.left-y.left)}));
});
grid.forEach((r,i)=>console.log(`行${i}:`,r.row.map(x=>x.id).join(" | ")));
```
然后 `await p.focus(...); keyboard.press("ArrowDown")...` 逐步验证 `12↓→22→↑→12`。

## 已知坑
- Windows 上 `npx tauri build` 在 app 目录跑会拉错 npm 包；**正确入口 `/app/ui` 下 `npm run tauri:build`**（记忆 `vox-tauri-build-from-ui`）。
- 每次编译前 Vox 旧 exe 若在运行，target 锁定导致失败 → 先结束进程。
- 编译产物在 `target/release/vox.exe` + `target/release/bundle/nsis/*-setup.exe`。
- GlassUI skill 在 `C:\Users\Wang\Desktop\glassui`，用户想让人把"焦点不跨卡 + 附属控件贴在上方控件正下方"写进 skill；我此前派过一个**方向错了的子代理**把"视觉 title"写进 `skills/glassui/SKILL.md`（有一条 bullet），以及 `references/design-system.md` 的 `## Control Alignment (attached controls)`。**这个方向可能该纠正成"焦点列记忆"**——但先确认，不要乱改 /无关它。

## 本会话已在做的事（避免重复）
我已经完成：
- Home.tsx：Toggle 移到语言下方（见 diff）。
- focus.ts：注释改动员 + stepUpDown 其实逻辑没大改（见 `git diff` 确认）。
- 多次"修改 focus → 实测 → verify → 编译"循环，最近一次编译失败是**因为旧 exe 被占用**（已结束进程后可重编）。
- 最后一步是在 focus.ts 改回"用 col index 的保持列记忆"版本并实测：`dd-home-target-language ↓ → (它的 toggle) → ↑ 回 target-language` —— **这看起来对**，但用户在他真机（非 mock）看到的"13→21 无记忆"需要你自己核实。

## 交接给下一位时请记得
- 用中文回复用户。
- 动手前先 `git diff` 看准现状，别用本会话记忆里的旧代码。
- 一次只动一处、每步实测，不要反复推翻。用户在等你把"方向键光标网格 + 列记忆 + 不跨卡"做对，很在意「所见即所得」，别再用"卡"概念去解释。