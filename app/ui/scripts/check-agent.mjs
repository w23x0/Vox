/**
 * Agent 控制面那一屏的自查（S1 §2.5.2 闸门①的界面侧）。
 *
 * 要防的是"界面假装在跑"：控制面起没起是**事实**（`snapshot.control`），
 * 这一屏必须按观察值说，且总开关一拨就要有反应（热切换的另一半在
 * `app/src-tauri/tests/mcp.rs` 的 `the_switch_hot_starts_and_stops_the_plane`）。
 *
 * 七件事：
 *   1. 默认档（开关关着）：说"没在监听"，**不**摆凭据文件那一行；
 *   2. 拨开 → 说"运行中" + 端口，凭据文件那一行出现；
 *   3. 端口落在内核保留段（<1024）→ 归一化成 0（与芯的 `Settings::normalize` 同一条）；
 *   4. 去抖间隔越界 → 夹进 50–5000；
 *   5. 关掉 → 回到"没在监听"，凭据行收回去；
 *   6. 四个授权位能拨、拨完留在那儿；
 *   7. `?control=busy`（端口被占那一档）：说"起不来" + 原因，**不许**说"运行中"。
 * 末尾再核一遍 en 文案与 ja 的回落（冻结包缺 `agent`，要落到基准中文，不许露出 key）。
 *
 * 自包含：和别的自查脚本一样自己起 vite preview（端口由内核分配、产物同一性自检在
 * `preview.mjs` 里），结束自动清理。
 * 用法：npm run build && npm run check:agent
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

const failures = [];
const expect = (condition, message) => {
  if (!condition) failures.push(message);
};

/** 和 a11y.mjs 一样挨个试内置 / Chrome / Edge。 */
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

/** 打开控制面那一页（点侧栏，等页面标题出现）。 */
async function openAgent(page, url, label = "Agent 控制") {
  await page.goto(url, { waitUntil: "networkidle" });
  await page.click('.sidebar .nav-item[data-page="agent"]');
  await page.getByRole("heading", { name: label, exact: true }).waitFor();
  await page.waitForTimeout(150);
}

/** 状态那一格（`data-control-status` 是它的 kind，文案是给人看的那句）。 */
async function status(page) {
  const badge = page.locator(".page.active [data-control-status]");
  return {
    kind: await badge.getAttribute("data-control-status"),
    text: (await badge.innerText()).trim(),
  };
}

/** 某个授权位的开关。 */
function toggle(page, name) {
  return page.getByRole("switch", { name, exact: true });
}

/** 数字设置项：填一个值再失焦（那一屏只在失焦 / 回车时才写回设置）。 */
async function typeNumber(page, id, text) {
  const input = page.locator(`.page.active #${id}`);
  await input.fill(text);
  await input.blur();
  await page.waitForTimeout(250);
  return input.inputValue();
}

const preview = await startPreview("check:agent");
const BASE = `${preview.base}/?mock=1`;

let browser;
try {
  browser = await launch();
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1180, height: 820 } });

  // ── [1] 默认档：开关关着，如实说"没在监听" ─────────────────────────────────
  await openAgent(page, BASE);
  const master = toggle(page, "总开关");
  expect((await master.getAttribute("aria-checked")) === "false", "默认档总开关该是关的");
  const off = await status(page);
  expect(off.kind === "off", `默认档状态该是 off，实际 ${off.kind}（${off.text}）`);
  expect(off.text.includes("没在监听"), `默认档要说清"没在监听"：${off.text}`);
  expect(
    (await page.locator(".page.active #agent-credentials").count()) === 0,
    "没在跑就不该摆出凭据文件那一行",
  );

  // ── [2] 拨开：说"运行中" + 真绑上的端口，凭据行出现 ─────────────────────────
  await master.click();
  await page.waitForTimeout(400);
  const on = await status(page);
  expect(on.kind === "running", `拨开后状态该是 running，实际 ${on.kind}（${on.text}）`);
  expect(on.text.includes("127.0.0.1:47123"), `要报真绑上的端口（假后端 = 47123）：${on.text}`);
  const credentials = page.locator(".page.active #agent-credentials");
  expect((await credentials.count()) === 1, "跑起来之后要给出凭据文件路径");
  expect(
    (await credentials.inputValue()).endsWith("control.json"),
    `凭据文件该是 control.json：${await credentials.inputValue()}`,
  );

  // ── [3] 端口：保留段（<1024）归一化成 0，状态照旧"运行中" ────────────────────
  expect((await typeNumber(page, "agent-port", "80")) === "0", "小于 1024 的端口该归 0");
  const afterPort = await status(page);
  expect(
    afterPort.kind === "running",
    `端口归 0 之后照旧在跑（0 = 系统分配）：${afterPort.kind}（${afterPort.text}）`,
  );

  // ── [4] 去抖间隔：越界夹回 50–5000 ─────────────────────────────────────────
  expect((await typeNumber(page, "agent-notify", "10")) === "50", "去抖下限是 50ms");
  expect((await typeNumber(page, "agent-notify", "99999")) === "5000", "去抖上限是 5000ms");

  // ── [5] 四个授权位：能拨，且拨完留在那儿 ─────────────────────────────────────
  for (const name of ["允许使用麦克风", "允许抓取程序声音", "允许放出声音", "允许修改配置"]) {
    const bit = toggle(page, name);
    expect((await bit.getAttribute("aria-checked")) === "false", `${name}：默认该是关的`);
    await bit.click();
    await page.waitForTimeout(250);
    expect((await bit.getAttribute("aria-checked")) === "true", `${name}：拨开之后该留着`);
  }

  // ── [6] 关掉总开关：回到"没在监听"，凭据行收回去 ─────────────────────────────
  await master.click();
  await page.waitForTimeout(400);
  const backOff = await status(page);
  expect(backOff.kind === "off", `关掉之后该是 off，实际 ${backOff.kind}（${backOff.text}）`);
  expect(
    (await page.locator(".page.active #agent-credentials").count()) === 0,
    "停了之后凭据文件那一行要收回去",
  );

  // ── [7] 起不来那一档（`?control=busy`）：说清原因，不许说"运行中" ────────────
  await openAgent(page, `${BASE}&control=busy`);
  await toggle(page, "总开关").click();
  await page.waitForTimeout(400);
  const failed = await status(page);
  expect(failed.kind === "failed", `起不来时状态该是 failed，实际 ${failed.kind}`);
  expect(failed.text.includes("起不来"), `要明说"起不来"：${failed.text}`);
  expect(!failed.text.includes("运行中"), `起不来时不许说"运行中"：${failed.text}`);
  expect(failed.text.includes("占用"), `要把后端给的原因带上：${failed.text}`);

  // ── [8] 三种界面语言：ja / en 各有自己的文案，页面名随语言变，不许露出 key ────────
  // 语言在设置页的下拉里选（项名是该语言自己的名字），选完回到控制面那一页。
  async function pickLanguage(nativeName) {
    await page.click('.sidebar .nav-item[data-page="settings"]');
    await page.locator("#dd-ui-language").click();
    await page
      .locator(".dropdown-options.show")
      .getByRole("option", { name: nativeName, exact: true })
      .click();
    await page.waitForTimeout(250);
    await page.click('.sidebar .nav-item[data-page="agent"]');
    await page.waitForTimeout(250);
  }
  await pickLanguage("日本語");
  const jaTitle = await page.locator(".page.active .page-head h1").innerText();
  expect(jaTitle.trim() === "エージェント制御", `ja 页面名该是「エージェント制御」，实际「${jaTitle}」`);
  const jaPage = await page.locator(".page.active").innerText();
  expect(jaPage.includes("マスタースイッチ"), `ja 要有总开关的日文文案：${jaPage.slice(0, 120)}`);
  expect(!jaPage.includes("agent."), `不许露出 i18n key：${jaPage.slice(0, 200)}`);
  await pickLanguage("English");
  const enTitle = await page.locator(".page.active .page-head h1").innerText();
  expect(enTitle.trim() === "Agent control", `en 页面名该是 Agent control，实际「${enTitle}」`);
  const enPage = await page.locator(".page.active").innerText();
  expect(enPage.includes("Master switch"), `en 要有总开关的英文文案：${enPage.slice(0, 120)}`);
  expect(!enPage.includes("agent."), `不许露出 i18n key：${enPage.slice(0, 200)}`);
} finally {
  await browser?.close();
  preview.stop();
}

if (failures.length > 0) {
  console.log(`\n✗ ${failures.length} 项要修：`);
  for (const failure of failures) console.log(`  - ${failure}`);
  process.exitCode = 1;
} else {
  console.log(
    "\nAgent 控制面：状态按事实报（关着 / 运行中 / 起不来）、端口与去抖归一化、" +
      "授权位可拨、en / ja 文案齐。",
  );
}
