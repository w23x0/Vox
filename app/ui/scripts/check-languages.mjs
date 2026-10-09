/**
 * 语言登记表 / 语言名本地化 / 能力联动 自查（docs/plans/LANG.md）。
 *
 *  - 登记表：每个码都有 en 兜底名；三种界面语言下 `Intl.DisplayNames` 给不出（原样返回码）的码
 *    有哪些，打印出来供维护者看（兜底名在 Rust/前端都存在，不会漏）；
 *  - 语言名由 Intl 现算：中文界面里 `en` 叫「英语」、日文界面叫「英語」、英文界面叫「English」，
 *    目标语言下拉按界面语言排序，不再有手写的 zh/ja 名字；
 *  - 目标语言为「仅文字」（阿里云的粤语）时，听人说话的「播放译音」开关禁用并给原因，换回有语音的语言恢复；
 *  - 切换模型后当前语言不被新方言支持（阿里云粤语 → GPT）：回该方言缺省并发通知。
 *
 * 自包含：自己起 vite preview。用法：npm run build && npm run check:languages
 */
import { readFileSync } from "node:fs";

import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

const failures = [];
const expect = (cond, msg) => {
  if (!cond) failures.push(msg);
};

// ── [1] 登记表与 Intl ────────────────────────────────────────────────────────
{
  const registry = JSON.parse(readFileSync(new URL("../../../catalog/languages.json", import.meta.url), "utf8"));
  const seen = new Set();
  for (const lang of registry.languages) {
    expect(typeof lang.en === "string" && lang.en.length > 0, `登记表 ${lang.code} 缺 en 兜底名`);
    expect(!seen.has(lang.code), `登记表 ${lang.code} 重复`);
    seen.add(lang.code);
    expect(!("zh" in lang) && !("ja" in lang), `登记表 ${lang.code} 不该再手写 zh/ja 名字`);
  }
  for (const [alias, code] of Object.entries(registry.aliases)) {
    expect(seen.has(code), `别名 ${alias} 指向未登记的码 ${code}`);
  }
  for (const ui of ["zh-CN", "ja-JP", "en"]) {
    const names = new Intl.DisplayNames([ui], { type: "language" });
    const bare = registry.languages.filter((l) => (names.of(l.code) ?? l.code).toLowerCase() === l.code.toLowerCase());
    console.log(`Intl(${ui}) 给不出名字、走英文兜底的码：${bare.length === 0 ? "（无）" : bare.map((l) => l.code).join(" ")}`);
  }
}

const preview = await startPreview("check:languages");
const BASE = `${preview.base}/?mock=1`;

async function open(browser, locale) {
  const page = await browser.newPage({ locale, viewport: { width: 1180, height: 1000 } });
  page.on("pageerror", (e) => failures.push(`pageerror: ${e.message}`));
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.waitForSelector("[data-page]");
  await page.waitForTimeout(700);
  return page;
}

async function optionLabels(page, trigger) {
  await page.locator(trigger).click();
  const labels = await page.locator(".dropdown-options.show [role=option]").allInnerTexts();
  await page.keyboard.press("Escape");
  return labels.map((l) => l.trim());
}

let browser;
try {
  browser = await chromium.launch();

  // ── [2] 语言名由 Intl 按界面语言现算，并按界面语言排序 ─────────────────────
  for (const [locale, english, ui] of [
    ["zh-CN", "英语", "zh-CN"],
    ["ja-JP", "英語", "ja-JP"],
    ["en-US", "English", "en"],
  ]) {
    const page = await open(browser, locale);
    const labels = await optionLabels(page, "#dd-home-target-language");
    expect(labels.includes(english), `${locale}：目标语言里应有「${english}」，实际前几项 ${labels.slice(0, 6).join("/")}`);
    const collator = new Intl.Collator(ui);
    // "最近使用"置顶：当前值（日语）排最前，其余按 Collator 排序。
    const rest = labels.slice(1);
    const sorted = [...rest].sort((a, b) => collator.compare(a, b));
    expect(JSON.stringify(rest) === JSON.stringify(sorted), `${locale}：目标语言（除置顶项）应按 Intl.Collator 排序`);
    await page.close();
  }

  // ── [3] 仅文字目标语言：播放译音禁用 + 原因；换回恢复 ──────────────────────
  {
    const page = await open(browser, "zh-CN");
    const more = page.locator('.more-toggle[aria-controls="more-listen"]');
    if ((await more.getAttribute("aria-expanded")) !== "true") await more.click();
    const toggle = page.getByRole("switch", { name: "播放译音", exact: true });
    await page.locator("#dd-home-listen-target-language").click();
    await page.locator(".dropdown-options.show").getByRole("option", { name: "粤语", exact: true }).click();
    await page.waitForTimeout(300);
    expect(await toggle.isDisabled(), "目标语言「粤语」仅文字：播放译音应禁用");
    expect((await toggle.getAttribute("aria-checked")) === "false", "仅文字时播放译音应显示为关");
    const card = page.locator(".pipeline-card").nth(1);
    expect((await card.innerText()).includes("粤语 仅支持译文，没有译音可播"), "应给一行原因");
    await page.locator("#dd-home-listen-target-language").click();
    await page.locator(".dropdown-options.show").getByRole("option", { name: "英语", exact: true }).click();
    await page.waitForTimeout(300);
    expect(!(await toggle.isDisabled()), "换回有语音的语言，播放译音应恢复可用");
    expect((await toggle.getAttribute("aria-checked")) === "true", "恢复后应回到原来的开");
    await page.close();
  }

  // ── [4] 切模型：语言不被新方言支持 → 回缺省 + 通知 ─────────────────────────
  {
    const page = await open(browser, "zh-CN");
    await page.locator("#dd-home-target-language").click();
    await page.locator(".dropdown-options.show").getByRole("option", { name: "粤语", exact: true }).click();
    await page.waitForTimeout(300);
    await page.getByRole("button", { name: "更换对外说话的模型", exact: true }).click();
    const pop = page.locator(".model-pop");
    await pop.getByRole("button", { name: "OpenAI GPT", exact: true }).click();
    await pop.locator("button.pick-item", { hasText: "gpt-realtime-translate" }).first().click();
    await page.waitForTimeout(500);
    const value = (await page.locator("#dd-home-target-language").innerText()).trim();
    expect(value.includes("日语"), `切到 GPT 后粤语应回方言缺省「日语」，实际「${value}」`);
    const toasts = (await page.locator("#toast-container .toast-text").allInnerTexts()).join("|");
    expect(toasts.includes("不支持目标语言「粤语」") && toasts.includes("「日语」"), `应有回落通知，实际：${toasts}`);
    const gptTargets = await optionLabels(page, "#dd-home-target-language");
    expect(gptTargets.length === 13, `GPT 方言只有 13 个目标语言，实际 ${gptTargets.length}`);
    expect(!gptTargets.includes("粤语"), "GPT 的目标语言里不该有粤语");
    await page.close();
  }
} finally {
  await browser?.close();
  preview.stop();
}

if (failures.length > 0) {
  console.error(failures.map((f) => `  ✗ ${f}`).join("\n"));
  process.exitCode = 1;
} else {
  console.log("语言登记表、Intl 语言名与排序、仅文字联动、切模型回落：通过。");
}
