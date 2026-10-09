/**
 * 多语言自查：en / ja 下渲染出来的文本里不许残留中文，以及「首次启动按系统语言选界面语言」。
 *
 * - 用 Playwright 的 `locale` 模拟系统语言，走真实的首次启动检测（`main.tsx::LangBridge`），
 *   不是直接改设置：检查 `<html lang>`（`:lang()` 选 CJK 字体靠它）和侧栏首项文字；
 * - 扫所有页面的文本节点和 aria-label / title / placeholder（不看是否可见——隐藏页也扫），
 *   并把会折叠的东西展开（首页「更多设置」、更换模型弹层、模型行详情、高级折叠）；
 * - en：任何汉字都算残留（语言下拉里每项用该语言自己的名字，是有意的，豁免）；
 * - ja：日文也用汉字，所以只抓「简体中文专用字」——只出现在简体里的字形。
 *
 * 自包含：自己起 vite preview（见 preview.mjs）。用法：npm run build && npm run check:i18n
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

/** 简体中文里用、日文汉字里不用的字（挑的是不会和日文共用的字形）。 */
const SIMPLIFIED_ONLY =
  "说译这为开关设显语选个们时对话听运终启连钥级态户务书线载构缓过还进动据应确认请输问题录绪测试页么发现读项转换频备长单";

const cases = [
  { locale: "en-US", lang: "en", home: "Home", residue: (s) => /\p{Script=Han}/u.test(s) },
  {
    locale: "ja-JP",
    lang: "ja",
    home: "ホーム",
    residue: (s) => [...s].some((c) => SIMPLIFIED_ONLY.includes(c)),
  },
];

const failures = [];

/** 页内收集：文本节点 + 可见名属性。 */
const COLLECT = () => {
  const out = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = (node.textContent ?? "").trim();
    if (!text) continue;
    const el = node.parentElement;
    if (!el || el.closest("script, style")) continue;
    if (el.closest("#dd-ui-language")) continue;
    out.push({ text, where: el.className ? `.${String(el.className).split(" ")[0]}` : el.tagName });
  }
  for (const el of document.querySelectorAll("[aria-label], [title], [placeholder]")) {
    for (const attr of ["aria-label", "title", "placeholder"]) {
      const v = el.getAttribute(attr);
      if (v) out.push({ text: v, where: `[${attr}] ${el.tagName.toLowerCase()}` });
    }
  }
  return out;
};

const preview = await startPreview("check:i18n");
let browser;
try {
  browser = await chromium.launch();
  for (const c of cases) {
    const ctx = await browser.newContext({ viewport: { width: 1040, height: 900 }, locale: c.locale });
    const page = await ctx.newPage();
    page.on("pageerror", (e) => failures.push(`${c.lang} pageerror: ${e.message}`));
    await page.goto(`${preview.base}/?mock=1`, { waitUntil: "networkidle" });
    await page.waitForSelector("[data-page]");
    await page.waitForTimeout(1200);

    const htmlLang = await page.evaluate(() => document.documentElement.lang);
    if (htmlLang !== c.lang) failures.push(`${c.lang}：<html lang> 是「${htmlLang}」，应随界面语言`);
    const first = await page.locator("[data-page]").first().getAttribute("aria-label");
    if (first !== c.home) failures.push(`${c.lang}：系统语言 ${c.locale} 下首次启动侧栏首项是「${first}」，应为「${c.home}」`);

    // 把会折叠的东西全展开，再逐页收集。
    await page.locator('[data-page="home"]').click();
    for (const toggle of await page.locator(".more-toggle").all()) await toggle.click();
    await page.locator(".pipeline-card .model-row button").first().click();
    await page.waitForTimeout(300);
    const home = await page.evaluate(COLLECT);

    const pages = [];
    for (const id of ["models", "settings", "subtitle", "vrchat", "agent", "usage", "about"]) {
      await page.locator(`[data-page="${id}"]`).click();
      await page.waitForTimeout(250);
      if (id === "models") {
        for (const head of await page.locator(".model-item-head").all()) await head.click();
        await page.evaluate(() => document.querySelectorAll("details").forEach((d) => (d.open = true)));
      }
      pages.push({ id, items: await page.evaluate(COLLECT) });
    }

    const seen = new Set();
    for (const { id, items } of [{ id: "home", items: home }, ...pages]) {
      for (const { text, where } of items) {
        if (!c.residue(text)) continue;
        const key = `${id}|${text}`;
        if (seen.has(key)) continue;
        seen.add(key);
        failures.push(`${c.lang} ${id}：残留中文「${text.slice(0, 40)}」（${where}）`);
      }
    }
    await ctx.close();
  }
} finally {
  await browser?.close();
  preview.stop();
}

if (failures.length > 0) {
  console.error(failures.join("\n"));
  process.exitCode = 1;
} else {
  console.log("en / ja 渲染文本无中文残留；首次启动按系统语言选语言、<html lang> 跟随。");
}
