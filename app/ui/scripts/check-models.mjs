/**
 * 模型页 / 首页「服务商 · 模型」/ 窗口按钮 / 侧栏键盘 自查（UI 重设计的新结构）。
 *
 * 全是 mock 后端的行为（`list_models` 契约见 docs/plans/UI-REDESIGN.md §6）：
 *  - 模型页：官方列表来源标签与刷新、未适配 / 推荐 / 将下线徽章、能力展开、
 *    拉取失败的三种原因（网络 / 密钥 / 命令调不通）与退回内置列表、自定义接入点校验；
 *  - 首页：「更换」弹层只列已配置密钥的服务商（没配的灰显 + 去配置）、只列已适配模型、
 *    选定后写回两条管线；「更多设置」展开状态跨刷新保留；
 *  - 窗口按钮按平台（?platform=）；侧栏方向键仍能在页面间走。
 *
 * 自包含：自己起 vite preview。用法：npm run build && npm run check:models
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

const failures = [];
const expect = (cond, msg) => {
  if (!cond) failures.push(msg);
};

const preview = await startPreview("check:models");
const BASE = `${preview.base}/?mock=1`;

async function open(browser, query = "", size = { width: 1180, height: 860 }) {
  const page = await browser.newPage({ locale: "zh-CN", viewport: size });
  page.on("pageerror", (e) => failures.push(`pageerror: ${e.message}`));
  await page.goto(`${BASE}${query}`, { waitUntil: "networkidle" });
  await page.waitForSelector("[data-page]");
  await page.waitForTimeout(700);
  return page;
}

const go = async (page, id) => {
  await page.click(`.sidebar .nav-item[data-page="${id}"]`);
  await page.waitForTimeout(200);
};
const tab = (page, name) => page.locator(".provider-item", { hasText: name });
const modelCard = (page) => page.locator('section[aria-labelledby^="models-title-"]');

let browser;
try {
  browser = await chromium.launch();

  // ── [1] 模型页：官方列表、徽章、能力展开、刷新 ───────────────────────────────
  {
    const page = await open(browser);
    await go(page, "models");
    const card = modelCard(page);
    expect((await card.locator(".source-tag").innerText()).includes("官方列表 · 3 分钟前"), "阿里云：来源标签应是「官方列表 · 3 分钟前」");
    expect((await card.locator(".model-item").count()) === 4, "阿里云应有 4 个模型条目");
    expect((await card.locator(".model-item.dim").count()) === 1, "未适配的那条要灰显");
    expect((await card.getByText("未适配", { exact: true }).count()) === 1, "要有且仅有一个「未适配」徽章");
    expect((await card.getByText("推荐", { exact: true }).count()) === 1, "要有且仅有一个「推荐」徽章");
    expect((await card.getByText("将于 2026-12-01 下线").count()) === 1, "要有「将于 2026-12-01 下线」警告徽章");
    expect((await card.getByText("快照", { exact: true }).count()) === 1, "要有「快照」徽章");
    expect(await card.locator(".badge-warn").count() === 1, "下线徽章用警告色");

    // 能力在行内展开，不再占一整屏
    expect((await card.getByText("译音 29 种", { exact: false }).count()) === 0, "展开前不显示语言数");
    await card.locator(".model-item-head").first().click();
    const detail = await card.locator(".mi-detail").first().innerText();
    expect(/译音 29 种 · 仅译文 31 种/.test(detail), `展开后应有语言数：${detail.replace(/\s+/g, " ")}`);
    expect(/可选 47 种/.test(detail) && /声音复刻/.test(detail) && /指定对方语言/.test(detail), `展开后应有音色 / 复刻 / 源语言：${detail}`);

    await card.getByRole("button", { name: "刷新", exact: true }).click();
    await page.waitForTimeout(700);
    expect((await card.locator(".source-tag").innerText()).includes("官方列表 · 刚刚"), "刷新后来源标签应变成「刚刚」");

    // 地区 + 高级里的自定义接入点
    expect((await page.locator("#dd-aliyun-region").innerText()).includes("中国内地（北京）"), "地区默认中国内地（北京）");
    await page.locator(".adv-details summary").click();
    const endpoint = page.locator("#f-aliyun-endpoint");
    await endpoint.fill("http://example.com");
    await endpoint.blur();
    expect((await endpoint.getAttribute("aria-invalid")) === "true", "非 ws(s):// 地址要标无效");
    expect((await page.locator(".adv-details .hint-danger").innerText()).includes("wss://"), "无效时要说明需要 wss://");
    await endpoint.fill("ws://example.com/realtime");
    await endpoint.blur();
    expect((await endpoint.getAttribute("aria-invalid")) === "true", "远端 ws:// 明文地址要标无效");
    await endpoint.fill("ws://localhost:8080/realtime");
    await endpoint.blur();
    expect((await endpoint.getAttribute("aria-invalid")) === "false", "回环 ws:// 允许");
    expect((await page.locator(".adv-details").innerText()).includes("重新启动翻译后生效"), "要说明重新启动翻译后生效");
    const good = "wss://ws1.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime";
    await endpoint.fill(good);
    await endpoint.blur();
    await page.waitForTimeout(300);
    expect((await endpoint.getAttribute("aria-invalid")) === "false", "合法地址不标无效");
    await go(page, "home");
    await go(page, "models");
    expect((await page.locator("#f-aliyun-endpoint").inputValue()) === good, "合法接入点要保存下来");
    // 非阿里云没有地区 / 接入点卡
    await tab(page, "Google Gemini").click();
    expect((await page.locator("#dd-aliyun-region").count()) === 0, "Gemini 不该有阿里云的地区设置");
    await page.close();
  }

  // ── [2] 拉取失败：网络 / 密钥 / 命令调不通 ───────────────────────────────────
  {
    const page = await open(browser, "&mockErr=gpt");
    await go(page, "models");
    await tab(page, "OpenAI GPT").click();
    await page.waitForTimeout(300);
    const card = modelCard(page);
    const text = await card.innerText();
    expect(text.includes("无法连接 api.openai.com") && text.includes("网络代理"), `网络错误要说明主机与代理：${text.slice(0, 160)}`);
    expect(!/国|地区无法/.test(text.replace("如果你所在地区无法直接访问该服务", "")), "提示里不提具体国家");
    expect((await card.locator(".source-tag").innerText()).trim() === "内置列表", "失败后要标「内置列表」");
    expect((await card.locator(".model-item").count()) === 1, "失败后退回内置列表（1 条）");
    expect((await tab(page, "OpenAI GPT").locator(".provider-dot.error").count()) === 1, "出错的服务商要显示出错状态点");
    await page.close();

    const auth = await open(browser, "&mockErr=gpt:auth");
    await go(auth, "models");
    await tab(auth, "OpenAI GPT").click();
    await auth.waitForTimeout(300);
    expect((await modelCard(auth).innerText()).includes("密钥无效或无权限"), "密钥错误要说「密钥无效或无权限」");
    await auth.close();

    const missing = await open(browser, "&mockErr=gpt:missing");
    await go(missing, "models");
    await tab(missing, "OpenAI GPT").click();
    await missing.waitForTimeout(300);
    const mCard = modelCard(missing);
    expect((await mCard.locator(".source-tag").innerText()).trim() === "内置列表", "命令调不通：标「内置列表」");
    expect((await mCard.locator(".model-error.hint-danger").count()) === 0, "命令调不通不显示错误条（只是退回内置信息）");
    expect((await mCard.locator(".model-item").count()) === 1, "命令调不通：用 catalog 的内置那一条");
    await missing.close();
  }

  // ── [3] 首页：更换模型弹层 ────────────────────────────────────────────────────
  {
    const page = await open(browser, "&cold=1");
    await page.getByRole("button", { name: "更换对外说话的模型", exact: true }).click();
    const pop = page.locator(".model-pop");
    expect((await pop.locator(".pick-row .pick-item:disabled").count()) === 3, "没配密钥时三家都灰显不可选");
    expect((await pop.getByRole("button", { name: "去配置", exact: true }).count()) === 3, "灰显的每家都有「去配置」");
    await pop.getByRole("button", { name: "去配置", exact: true }).first().click();
    await page.waitForTimeout(250);
    expect((await page.locator(".page.active h1").innerText()) === "模型", "「去配置」应跳到模型页");
    await page.close();
  }
  {
    const page = await open(browser);
    const speakCard = page.locator(".pipeline-card").first();
    await page.getByRole("button", { name: "更换对外说话的模型", exact: true }).click();
    const pop = page.locator(".model-pop");
    expect((await pop.locator(".pick-row .pick-item:disabled").count()) === 0, "三家都配了密钥时都可选");
    await pop.getByRole("button", { name: "OpenAI GPT", exact: true }).click();
    await page.waitForTimeout(500);
    const models = await pop.locator("button.pick-item", { hasText: "gpt-realtime" }).count();
    expect(models === 2, `OpenAI 只列已适配的模型（2 个，未适配的 gpt-realtime-2 不列），实际 ${models}`);
    expect((await pop.getByText("gpt-realtime-2", { exact: true }).count()) === 0, "未适配的模型不可出现在选择里");
    await pop.locator("button.pick-item", { hasText: "gpt-realtime-translate-2026-08-15" }).click();
    await page.waitForTimeout(400);
    expect((await page.locator(".model-pop").count()) === 0, "选定模型后弹层应关闭");
    const name = await speakCard.locator(".model-name").innerText();
    expect(name.includes("OpenAI GPT") && name.includes("GPT Realtime Translate (2026-08-15)"), `首页应显示新的服务商 · 模型：${name}`);
    // 听人说话那张卡不受影响
    const listenName = await page.locator(".pipeline-card").nth(1).locator(".model-name").innerText();
    expect(listenName.includes("阿里云百炼"), `另一张卡不该被改：${listenName}`);

    // 弹层：Esc 收起、点外面收起
    await page.getByRole("button", { name: "更换对外说话的模型", exact: true }).click();
    await page.keyboard.press("Escape");
    expect((await page.locator(".model-pop").count()) === 0, "Esc 应收起弹层");
    await page.getByRole("button", { name: "更换对外说话的模型", exact: true }).click();
    await page.click(".page.active .page-head h1");
    expect((await page.locator(".model-pop").count()) === 0, "点弹层外面应收起");

    // 「更多设置」折叠，展开状态跨刷新保留
    const more = page.locator('.more-toggle[aria-controls="more-listen"]');
    expect((await more.getAttribute("aria-expanded")) === "false", "「更多设置」默认折叠");
    expect((await page.locator("#more-listen").count()) === 0, "折叠时不渲染更多里的控件");
    await more.click();
    expect((await page.locator("#more-listen").count()) === 1, "展开后渲染更多里的控件");
    await page.reload({ waitUntil: "networkidle" });
    await page.waitForSelector("[data-page]");
    expect(
      (await page.locator('.more-toggle[aria-controls="more-listen"]').getAttribute("aria-expanded")) === "true",
      "「更多设置」的展开状态应记在 localStorage、刷新后保留",
    );
    expect(
      (await page.locator('.more-toggle[aria-controls="more-speak"]').getAttribute("aria-expanded")) === "false",
      "另一张卡的折叠状态各记各的",
    );
    await page.close();
  }

  // ── [4] 窗口按钮按平台 ────────────────────────────────────────────────────────
  for (const [platform, side] of [["mac", "left"], ["windows", "right"], ["linux", "right"]]) {
    const page = await open(browser, `&platform=${platform}`, { width: 1000, height: 700 });
    const info = await page.evaluate(() => {
      const box = document.querySelector(".window-traffic");
      const rect = box?.getBoundingClientRect();
      return {
        controls: !!box?.classList.contains("window-controls"),
        buttons: box ? box.querySelectorAll("button").length : 0,
        left: rect?.left ?? -1,
        right: rect ? window.innerWidth - rect.right : -1,
        names: [...(box?.querySelectorAll("button") ?? [])].map((b) => b.getAttribute("aria-label")),
      };
    });
    expect(info.buttons === 3, `${platform}：应有 3 个窗口按钮，实际 ${info.buttons}`);
    expect(info.names.every(Boolean), `${platform}：窗口按钮要有可访问名`);
    if (side === "left") {
      expect(!info.controls && info.left < 40, `mac：红黄绿应在左上（left=${info.left}）`);
    } else {
      expect(info.controls && info.right < 20, `${platform}：最小化/最大化/关闭应在右上（right=${info.right}）`);
      expect(info.names[2] === "关闭", `${platform}：关闭应排在最右：${info.names}`);
    }
    await page.close();
  }

  // ── [5] 侧栏键盘：方向键在页面间走，→ 进内容区 ────────────────────────────────
  {
    const page = await open(browser);
    await page.locator('.sidebar .nav-item[data-page="home"]').focus();
    await page.keyboard.press("ArrowDown");
    await page.waitForTimeout(250);
    expect((await page.locator(".sidebar .nav-item.active").getAttribute("data-page")) === "models", "侧栏 ↓ 应走到下一页（模型）");
    expect((await page.evaluate(() => document.activeElement?.getAttribute("data-page"))) === "models", "焦点应跟到新的侧栏项");
    await page.keyboard.press("ArrowRight");
    await page.waitForTimeout(150);
    expect(await page.evaluate(() => !!document.activeElement?.closest(".page.active")), "侧栏 → 应进入内容区");
    await page.keyboard.press("ArrowLeft");
    await page.waitForTimeout(150);
    expect(await page.evaluate(() => !!document.activeElement?.closest("[data-focus-zone='sidebar']")), "内容区最左 ← 应回侧栏");
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
  console.log("模型页、首页更换模型、窗口按钮平台差异、侧栏键盘：通过。");
}
