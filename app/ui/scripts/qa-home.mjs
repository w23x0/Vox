/**
 * 首页全流程自查：密钥隔离、Gemini 的联动、跨页面布局与窄窗。
 *
 * 自包含：和 a11y.mjs 一样自己起 vite preview（端口由内核分配、产物同一性自检都在
 * `preview.mjs` 里），不再认"某个固定端口上有个服务"——那个约定让这条自查可能跑在
 * 别人的（旧）构建上（verifier 第七轮 F3）。
 * 用法：npm run build && npm run qa:home
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

const failures = [];
const browserErrors = [];
const secretSentinel = "VB_TEST_SECRET_9f41c0d2";

function watch(page, label) {
  page.on("console", (message) => {
    if (message.type() === "error" || message.type() === "warning") {
      browserErrors.push(`${label} console ${message.type()}: ${message.text()}`);
    }
  });
  page.on("pageerror", (error) => browserErrors.push(`${label} pageerror: ${error.message}`));
}

async function selectOption(page, trigger, option) {
  await page.locator(trigger).click();
  await page.locator(".dropdown-options.show").getByRole("option", { name: option, exact: true }).click();
}

/** 模型页左侧选一家服务商（项文字含服务商名）。 */
async function pickProvider(page, name) {
  await page.locator(".provider-item", { hasText: name }).click();
}

/** 首页某张卡里：更换模型 → 选服务商 → 选模型（弹层里按模型 ID 找）。 */
async function changeModel(page, pipelineName, providerName, modelId) {
  await page.getByRole("button", { name: `更换${pipelineName}的模型`, exact: true }).click();
  const pop = page.locator(".model-pop");
  await pop.getByRole("button", { name: providerName, exact: true }).click();
  await pop.locator(".pick-item", { hasText: modelId }).click();
}

/** 展开首页某张卡的「更多设置」（已展开就不动）。 */
async function openMore(page, pipeline) {
  const toggle = page.locator(`.more-toggle[aria-controls="more-${pipeline}"]`);
  if ((await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
}

async function expectText(locator, expected, message) {
  const actual = (await locator.innerText()).trim();
  if (actual !== expected) failures.push(`${message}：期望“${expected}”，实际“${actual}”`);
}

async function expectCount(locator, expected, message) {
  const actual = await locator.count();
  if (actual !== expected) failures.push(`${message}：期望 ${expected}，实际 ${actual}`);
}

async function inspectVisibleLayout(page, label) {
  const issues = await page.locator(".page.active").evaluate((activePage) => {
    const output = [];
    const root = document.documentElement;
    if (root.scrollWidth > root.clientWidth + 1) {
      output.push(`document 横向溢出 ${root.scrollWidth - root.clientWidth}px`);
    }
    const scroll = activePage.querySelector(".page-scroll");
    if (scroll && scroll.scrollWidth > scroll.clientWidth + 1) {
      output.push(`页面横向溢出 ${scroll.scrollWidth - scroll.clientWidth}px`);
    }
    const candidates = activePage.querySelectorAll("button, input, .badge, .chip");
    for (const element of candidates) {
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      if (style.display === "none" || style.visibility === "hidden" || rect.width === 0) continue;
      if (element.scrollWidth > element.clientWidth + 2 && style.overflowX === "visible") {
        output.push(`${element.tagName.toLowerCase()} 文本溢出：${element.textContent?.trim() ?? ""}`);
      }
    }
    return output;
  });
  for (const issue of issues) failures.push(`${label}：${issue}`);
}

async function navigate(page, label) {
  await page.getByRole("button", { name: label, exact: true }).click();
  await page.getByRole("heading", { name: label, exact: true }).waitFor();
}

const preview = await startPreview("qa:home");
const baseUrl = `${preview.base}/?mock=1`;

let browser;
try {
  browser = await chromium.launch();
  const cold = await browser.newPage({ locale: "zh-CN", viewport: { width: 1180, height: 820 } });
  watch(cold, "cold");
  await cold.goto(`${baseUrl}&cold=1`, { waitUntil: "networkidle" });
  await cold.waitForSelector(".stats-row.cols-2 > .stat-card");

  const coldCards = cold.locator(".stats-row.cols-2 > .stat-card");
  await expectCount(coldCards, 2, "首页应正好有两条流水线");
  await expectCount(
    cold.getByText("请先配置 阿里云百炼 API 密钥", { exact: true }),
    2,
    "无 Key 时两条流水线都应显示明确配置提示",
  );
  const coldStartButtons = cold.getByRole("button", { name: "启动", exact: true });
  if (
    (await coldStartButtons.count()) !== 2 ||
    !(await coldStartButtons.nth(0).isDisabled()) ||
    !(await coldStartButtons.nth(1).isDisabled())
  ) {
    failures.push("无 Key 时启动按钮必须禁用");
  }

  await navigate(cold, "模型");
  // 显式选服务商，不依赖"默认停在哪个"（那是界面的自由，不是契约）：这一档要测的正是
  // "保存的只是 Google Gemini 的密钥，别家的密钥状态一个字节都没动"。
  await pickProvider(cold, "Google Gemini");
  const keyInput = cold.locator("#f-api-key");
  if ((await keyInput.getAttribute("type")) !== "password") failures.push("API Key 输入框不是 password");
  if (!(await cold.getByRole("button", { name: "保存", exact: true }).isDisabled())) {
    failures.push("空 Key 时保存按钮没有禁用");
  }

  await keyInput.fill(secretSentinel);
  const leakedBeforeSave = await cold.evaluate((secret) => ({
    html: document.documentElement.outerHTML.includes(secret),
    text: document.body.innerText.includes(secret),
  }), secretSentinel);
  if (leakedBeforeSave.html || leakedBeforeSave.text) failures.push("Key 出现在 HTML 属性或页面文本中");

  await cold.getByRole("button", { name: "保存", exact: true }).dblclick();
  const keyBadge = cold.locator(".models-detail .card-head .badge").first();
  await keyBadge.filter({ hasText: "已配置" }).waitFor();
  await cold.waitForTimeout(100);
  await expectCount(
    cold.getByText("Google Gemini 密钥已保存", { exact: true }),
    1,
    "重复点击保存不应提交两次",
  );
  // 已配置：输入框收起，只显示掩码 + 末四位；完整密钥不在页面上
  await expectCount(cold.locator("#f-api-key"), 0, "保存后输入框应收起（已配置只显示掩码）");
  await expectText(cold.locator(".key-mask"), `••••${secretSentinel.slice(-4)}`, "已配置的密钥掩码");
  const leakedAfterSave = await cold.evaluate((secret) => ({
    html: document.documentElement.outerHTML.includes(secret),
    text: document.body.innerText.includes(secret),
  }), secretSentinel);
  if (leakedAfterSave.html || leakedAfterSave.text) failures.push("保存后 Key 仍出现在 HTML 或页面文本中");

  await pickProvider(cold, "阿里云百炼");
  await keyBadge.filter({ hasText: "未配置" }).waitFor();
  await pickProvider(cold, "Google Gemini");
  await keyBadge.filter({ hasText: "已配置" }).waitFor();

  await cold.getByRole("button", { name: "清除", exact: true }).click();
  await cold.getByRole("button", { name: "确认清除", exact: true }).click();
  await keyBadge.filter({ hasText: "未配置" }).waitFor();
  if (await cold.getByRole("button", { name: "清除", exact: true }).isVisible().catch(() => false)) {
    failures.push("清除 Gemini Key 后清除按钮仍然可见");
  }
  // 窄窗下保存按钮不许换行（清除后输入框 + 保存又出现了）
  await cold.setViewportSize({ width: 900, height: 620 });
  const saveButtonBox = await cold.getByRole("button", { name: "保存", exact: true }).boundingBox();
  if (!saveButtonBox || saveButtonBox.height > 44) failures.push("900px 窄窗的保存按钮发生换行");
  if (browserErrors.some((message) => message.includes(secretSentinel))) {
    failures.push("Key 出现在浏览器控制台中");
  }
  await cold.close();

  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1180, height: 820 } });
  watch(page, "warm");
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.waitForSelector(".stats-row.cols-2 > .stat-card");

  // 阿里云：「更多设置」里有音色下拉；换到 Gemini 后能力位说不能选音色，字段整个不渲染（不是灰掉）
  await openMore(page, "speak");
  await expectCount(page.locator("#dd-home-speak-voice"), 1, "阿里云对外说话应有音色下拉");
  await changeModel(page, "对外说话", "Google Gemini", "gemini-3.5-live-translate-preview");
  await expectCount(page.locator("#dd-home-speak-voice"), 0, "Gemini 对外说话不给选手动音色，字段不应渲染");
  await page.getByText(/重启「对外说话」后生效/).waitFor({ timeout: 1500 }).catch(() => {
    failures.push("运行中切换对外说话服务商后没有重启提示");
  });

  await openMore(page, "listen");
  await changeModel(page, "听人说话", "Google Gemini", "gemini-3.5-live-translate-preview");
  // 不支持指定源语言：下拉换成固定的「自动识别」（不是灰掉的下拉），并说明原因
  await expectCount(page.locator("#dd-home-listen-source"), 0, "Gemini 不支持源语言，下拉不应渲染");
  await expectText(page.locator("#lang-home-listen-source"), "自动识别", "Gemini 听人说话源语言");
  await expectCount(page.getByText("固定为自动识别", { exact: false }), 1, "固定源语言要说明原因");
  await expectCount(page.locator("#dd-home-listen-voice"), 0, "Gemini 听人说话不给选手动音色，字段不应渲染");
  await page.getByText(/重启「听人说话」后生效/).waitFor({ timeout: 1500 }).catch(() => {
    failures.push("运行中切换听人说话服务商后没有重启提示");
  });

  const restartCountBeforeLanguage = await page.locator("#toast-container .toast-text").count();
  await selectOption(page, "#dd-home-target-language", "英语");
  await page.waitForFunction(
    ({ selector, before }) => document.querySelectorAll(selector).length > before,
    { selector: "#toast-container .toast-text", before: restartCountBeforeLanguage },
    { timeout: 1500 },
  ).catch(() => {
    failures.push("Gemini 运行中切换目标语言后没有重启提示");
  });
  const listenAudioToggle = page.getByRole("switch", { name: "播放译音", exact: true });
  await listenAudioToggle.click();
  if ((await listenAudioToggle.getAttribute("aria-checked")) !== "false") failures.push("播放译音开关关闭后状态不清楚");
  await listenAudioToggle.click();
  if ((await listenAudioToggle.getAttribute("aria-checked")) !== "true") failures.push("播放译音开关开启后状态不清楚");

  await inspectVisibleLayout(page, "首页 1180x820 亮色");
  await navigate(page, "模型");
  // 同冷启动那档：先把服务商选到要测的那个，再断言它渲染出来的东西。
  await pickProvider(page, "Google Gemini");
  /*
   * Gemini 的默认模型。以前这里 `import("/src/catalog.ts")` 直接把源码模块拉进来问——
   * 那只在 dev server 上成立（preview 服务的是打包后的 dist，没有 `/src/*`）。
   * 打包之后这个值只有一个对外可见的出口：模型页「可用模型」里那行模型 ID。问界面，
   * 顺带也就验了"目录里的默认模型真的渲染出来了"。
   */
  await expectCount(page.getByText("gemini-3.5-live-translate-preview", { exact: true }), 1, "Gemini 默认模型");
  await inspectVisibleLayout(page, "模型 1180x820 亮色");
  await page.locator("#toast-container .toast-item").first().waitFor({ state: "detached", timeout: 5000 }).catch(() => {});
  await page.screenshot({ path: "../../target/ui-qa-gemini-provider-1180x820-light.png" });

  await navigate(page, "设置");
  await inspectVisibleLayout(page, "设置 1180x820 亮色");
  await navigate(page, "关于");
  await inspectVisibleLayout(page, "关于 1180x820 亮色");

  // 主题在设置页的下拉里切（跟随系统 / 浅色 / 深色）
  await navigate(page, "设置");
  await selectOption(page, "#dd-theme", "深色");
  // 切换走 View Transition，data-theme 在下一帧才落；等它，不等就是读到旧值。
  await page
    .waitForFunction(() => document.documentElement.getAttribute("data-theme") === "dark", null, { timeout: 2000 })
    .catch(() => failures.push("选「深色」后 data-theme 没变成 dark"));
  for (const label of ["首页", "模型", "设置", "关于"]) {
    await navigate(page, label);
    await inspectVisibleLayout(page, `${label} 1180x820 暗色`);
  }
  await page.screenshot({ path: "../../target/ui-qa-about-1180x820-dark.png" });

  await page.setViewportSize({ width: 900, height: 620 });
  for (const label of ["首页", "模型", "设置", "关于"]) {
    await navigate(page, label);
    await inspectVisibleLayout(page, `${label} 900x620 暗色`);
  }
  await navigate(page, "首页");
  await page.screenshot({ path: "../../target/ui-qa-gemini-home-900x620-dark.png" });
  await navigate(page, "模型");
  await page.screenshot({ path: "../../target/ui-qa-gemini-provider-900x620-dark.png" });
  await page.close();

  failures.push(...browserErrors);
  if (failures.length > 0) {
    console.error(failures.join("\n"));
    process.exitCode = 1;
  } else {
    console.log("Gemini UI、密钥隔离、跨页面布局与窄窗口检查通过。");
  }
} finally {
  await browser?.close();
  preview.stop();
}
