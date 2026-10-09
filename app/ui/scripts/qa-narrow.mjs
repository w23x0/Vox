/**
 * 窄窗布局审计：所有页面 × 多档窄宽度，检查横向溢出与文本穿模。
 *
 * 页面的内容区宽 = 窗口宽 − 64(侧栏) − 32(左右边距) ≈ 窗口宽 − 96。
 * 所以要实测窗口宽从 640(最小) 到现身窗宽度(如 960/1040) 的渐变，
 * 尤其每档 media query 阈值(1024 / 768 / 640)附近，避免边界单列/多列
 * 跳变时的挤压穿模。
 *
 * 重点盯紧用量(Usage)这类字符密集页：它的 .input-grid-3 三卡横排 +
 * 30px 等宽大数字(stat-value)，窄窗最容易文字撑破。这套脚本把
 * Usage/Subtitle 也纳入窄窗巡检 —— 老 QA-home 恰恰漏了这两页。
 *
 * 自包含：和 a11y.mjs 一样自己起 vite preview（端口由内核分配、端上跑的得是这份 `dist`——
 * 两道闸都在 `preview.mjs` 里），结束自动清理。
 * 用法：npm run build && npm run qa:narrow
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

/* 页面 id; label 仅用于报错阅读 */
const ORDER = [
  ["home", "首页", "首页"],
  ["models", "模型", "模型"],
  ["subtitle", "字幕", "字幕"],
  ["vrchat", "VRChat", "VRChat"],
  ["agent", "Agent 控制", "Agent 控制"],
  ["settings", "设置", "设置"],
  ["usage", "用量", "用量"],
  ["about", "关于", "关于"],
];

async function launch() {
  for (const channel of ["chrome", "msedge", undefined]) {
    try {
      return await chromium.launch(channel ? { channel } : {});
    } catch {
      /* 换下一个 */
    }
  }
  throw new Error("没有可用浏览器。");
}

const failures = [];

async function checkPage(page, width, height, label) {
  const issues = await page.evaluate(() => {
    const out = [];
    const active = document.querySelector(".page.active");

    // 1) 整窗横向溢出
    const root = document.documentElement;
    if (root.scrollWidth > root.clientWidth + 1) {
      out.push(`整个文档横向溢出 ${root.scrollWidth - root.clientWidth}px`);
    }

    // 2) 页面滚动容器横向溢出
    const scroll = active?.querySelector(".page-scroll");
    if (scroll && scroll.scrollWidth > scroll.clientWidth + 1) {
      out.push(`页面容器横向溢出 ${scroll.scrollWidth - scroll.clientWidth}px`);
    }

    // 3) 按钮 / 输入 / 徽章 / chip / 标签 文本溢出（穿模）
    const candidates = active?.querySelectorAll(
      "button, input, select, .badge, .chip, .num, .stat-value, .stat-label, .si-title, .lat-item, .sub-row > span, .sub-card-head",
    ) ?? [];
    for (const el of candidates) {
      const style = getComputedStyle(el);
      const rect = el.getBoundingClientRect();
      if (style.display === "none" || style.visibility === "hidden" || rect.width === 0) continue;
      // 文本被压缩到换行但被迫单行不换，或明显超出自身
      if (el.scrollWidth > el.clientWidth + 3 && style.overflowX !== "hidden") {
        const txt = (el.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 40);
        out.push(`${el.tagName.toLowerCase()}[${el.className}] 文本溢出: "${txt}"（${el.scrollWidth} > ${el.clientWidth}）`);
      }
    }

    // 4) 元素被顶出视口左/右缘（横向穿出才是穿模；纵向可滚动不算）
    for (const el of active?.querySelectorAll("*") ?? []) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      const style = getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") continue;
      const horizontalEscape =
        r.left < -2 ||
        r.right > (window.innerWidth || document.documentElement.clientWidth) + 2;
      if (!horizontalEscape) continue;
      const txt = (el.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 30);
      out.push(`水平越界 ${JSON.stringify({ x: Math.round(r.left), w: Math.round(r.width) })} <${el.tagName.toLowerCase()}> "${txt}"`);
    }
    return [...new Set(out)];
  });

  for (const issue of issues) failures.push(`${label}@${width}x${height}:${issue}`);
}

const preview = await startPreview("qa:narrow");
const BASE = `${preview.base}/?mock=1`;

// 多档宽度：从初始窗到最小 640，卡 max-width 阈值(1024/768)两头。
const widths = [
  { width: 1040, height: 700 },
  { width: 1024, height: 660 },
  { width: 960, height: 640 },
  { width: 900, height: 600 },
  { width: 800, height: 580 },
  { width: 769, height: 560 },
  { width: 768, height: 560 },
  { width: 720, height: 560 },
  { width: 640, height: 560 },
];

try {
  const browser = await launch();

  for (const { width, height } of widths) {
    const page = await browser.newPage({ locale: "zh-CN", viewport: { width, height } });
    page.on("pageerror", (e) => failures.push(`${width}x${height} pageerror: ${e.message}`));
    await page.goto(BASE, { waitUntil: "networkidle" });
    await page.waitForTimeout(400);

    for (const [id, label] of ORDER) {
      await page
        .locator(`[data-page="${id}"]`)
        .click()
        .catch((e) => failures.push(`${id}/${label} 点击失败: ${e.message}`));
      await page.waitForTimeout(60);
      await checkPage(page, width, height, `${width}x${height} ${label}`);
    }
    await page.close();
  }
  await browser.close();
} catch (e) {
  console.error(e.message);
  process.exitCode = 1;
} finally {
  preview.stop();
}

if (failures.length > 0) {
  console.error("\n窄窗布局问题：");
  console.error(failures.join("\n"));
  process.exitCode = process.exitCode || 1;
} else {
  console.log(
    `所有页面在 ${widths[0].width}–${widths[widths.length - 1].width} 窄窗下均无横向溢出/文本穿模。`,
  );
}
