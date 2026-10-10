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
 * 后端消息（`catalog/notices.json`，K5）另外三类检查：
 * - Rust 在 `crates/vox-core/src/notice.rs` 的 `declare_keys!` 里声明的每个 key，zh / en / ja 都有
 *   非空文案，三语占位符一致，文件里没有没人声明的 key；
 * - 缺 key 的回退顺序：当前语言 → English → key 本身（**不回退中文**），detail 原样接在后面、不插值；
 * - 浏览器里：开局塞一条已知 key（带中文原文 detail）和一条未知 key，en / ja 的 toast 里
 *   译文部分无中文残留、原文原样在末尾、未知 key 显示 key 本身。
 *
 * 自包含：自己起 vite preview（见 preview.mjs）。用法：npm run build && npm run check:i18n
 */
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright";
import { createServer } from "vite";

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

const HERE = dirname(fileURLToPath(import.meta.url));
const UI_ROOT = resolve(HERE, "..");
const REPO = resolve(UI_ROOT, "../..");

const placeholders = (text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(",");

/** K5：Rust 声明的 key ⇄ catalog/notices.json。 */
function checkDeclaredKeys() {
  const rust = readFileSync(resolve(REPO, "crates/vox-core/src/notice.rs"), "utf8");
  const block = rust.slice(rust.indexOf("declare_keys! {"), rust.indexOf("#[cfg(test)]"));
  const declared = [...block.matchAll(/^\s+[A-Z][A-Z0-9_]*\s*=\s*"([^"]+)",/gm)].map((m) => m[1]);
  if (declared.length === 0) failures.push("notices：没从 notice.rs 的 declare_keys! 里读出任何 key（脚本与宏写法对不上）");
  const catalog = JSON.parse(readFileSync(resolve(REPO, "catalog/notices.json"), "utf8"));
  for (const key of declared) {
    const entry = catalog.messages[key];
    if (!entry) {
      failures.push(`notices：Rust 声明了 ${key}，catalog/notices.json 里没有`);
      continue;
    }
    const shapes = new Set();
    for (const lang of ["zh", "en", "ja"]) {
      if (typeof entry[lang] !== "string" || entry[lang].trim() === "") {
        failures.push(`notices：${key} 缺 ${lang} 文案`);
      } else {
        shapes.add(placeholders(entry[lang]));
      }
    }
    if (shapes.size > 1) failures.push(`notices：${key} 三语占位符不一致`);
  }
  for (const key of Object.keys(catalog.messages)) {
    if (!declared.includes(key)) failures.push(`notices：${key} 在 notices.json 里，但 Rust 没声明`);
  }
  return declared.length;
}

/** 回退顺序与 detail 原样：用合成的表直接调渲染函数。 */
async function checkFallbackOrder() {
  const server = await createServer({
    root: UI_ROOT,
    appType: "custom",
    logLevel: "error",
    server: { middlewareMode: true },
  });
  try {
    const { renderMsgFrom } = await server.ssrLoadModule("/src/i18n/notice.ts");
    const tables = {
      "zh-CN": { "t.both": "中文 both", "t.zhOnly": "中文 zhOnly" },
      en: { "t.both": "EN both", "t.enOnly": "EN enOnly {name}" },
      "ja-JP": { "t.both": "日本語 both" },
    };
    const seps = { "zh-CN": "：", en: ": ", "ja-JP": "：" };
    const eq = (what, got, want) => {
      if (got !== want) failures.push(`notices 回退：${what}：得到「${got}」，应为「${want}」`);
    };
    eq("当前语言有就用当前语言", renderMsgFrom(tables, seps, "ja-JP", { key: "t.both" }), "日本語 both");
    eq("ja 缺 → English，不是中文", renderMsgFrom(tables, seps, "ja-JP", { key: "t.enOnly", params: { name: "x" } }), "EN enOnly x");
    eq("只有中文有 → 不回退中文，显示 key", renderMsgFrom(tables, seps, "en", { key: "t.zhOnly" }), "t.zhOnly");
    eq("ja 下只有中文有 → 显示 key", renderMsgFrom(tables, seps, "ja-JP", { key: "t.zhOnly" }), "t.zhOnly");
    eq("全无 → key 本身", renderMsgFrom(tables, seps, "en", { key: "t.nope" }), "t.nope");
    const original = "服务端原话 {name} <&> 100%";
    eq(
      "detail 原样接在后面，不插值、不改动",
      renderMsgFrom(tables, seps, "en", { key: "t.enOnly", params: { name: "x" }, detail: original }),
      `EN enOnly x: ${original}`,
    );
  } finally {
    await server.close();
  }
}

const declaredKeyCount = checkDeclaredKeys();
await checkFallbackOrder();

/**
 * 浏览器里的 toast：开局稍后发一条通知（mock 的 `?mockNotice=`），读 toast 文案。
 * 每次用**新的上下文**——首次启动按系统语言选界面语言的标记存在 localStorage 里，复用会停在中文。
 */
async function toastTexts(browser, locale, base, query) {
  const ctx = await browser.newContext({ viewport: { width: 1040, height: 900 }, locale });
  try {
    const page = await ctx.newPage();
    await page.goto(`${base}/?mock=1&${query}`, { waitUntil: "networkidle" });
    await page.waitForSelector(".toast-text", { timeout: 6000 }).catch(() => undefined);
    return await page.locator(".toast-text").allTextContents();
  } finally {
    await ctx.close();
  }
}

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

    // 后端通知的渲染：已知 key + 中文原文 detail、未知 key。
    {
      const detail = "服务端原话：free tier 已用完 {name}";
      const known = await toastTexts(
        browser,
        c.locale,
        preview.base,
        `mockNotice=notice.server.closeQuota&mockDetail=${encodeURIComponent(detail)}`,
      );
      const knownText = known.find((x) => x.endsWith(detail));
      if (!knownText) {
        failures.push(`${c.lang} toast：已知 key 的 toast 没有把 detail 原样放在末尾：${JSON.stringify(known)}`);
      } else if (c.residue(knownText.slice(0, knownText.length - detail.length))) {
        failures.push(`${c.lang} toast：译文部分有中文残留：${knownText}`);
      }
      const unknown = await toastTexts(browser, c.locale, preview.base, "mockNotice=notice.doesNotExist");
      if (!unknown.some((x) => x.includes("notice.doesNotExist"))) {
        failures.push(`${c.lang} toast：未知 key 应显示 key 本身：${JSON.stringify(unknown)}`);
      }
      if (unknown.some((x) => c.residue(x))) failures.push(`${c.lang} toast：未知 key 的 toast 里不该有中文：${JSON.stringify(unknown)}`);
    }

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
  console.log(
    `en / ja 渲染文本无中文残留；首次启动按系统语言选语言、<html lang> 跟随；` +
      `后端消息 ${declaredKeyCount} 个 key 三语齐全、回退 当前语言→English→key、detail 原样保留。`,
  );
}
