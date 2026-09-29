/**
 * 能力位消费端自查（S0 §4.3-B）：位为假的入口**不渲染**，并说清"这台设备做不到"及 reason
 * （§2.6 R1/R9）；位为真的入口照旧（R5）。
 *
 * 覆盖：
 *   [1] Windows 桌面（默认）：位为真的入口照旧；`vr_captions` 位为假 ⇒ SteamVR 开关不渲染
 *   [2] `?on=vr_captions`：位为真 ⇒ 开关照旧（R5 的另一半）
 *   [3] Linux 桌面：虚拟麦位为真 ⇒ 只给设备名引导，不出现安装/卸载（与 check-cable 同结论）
 *   [4] Linux + `?off=virtual_mic:not_wired`：接线前的中间态 ⇒ 撤下"去选 Vox Virtual Mic"
 *   [5] Android：抓程序 / 虚拟麦的入口不渲染，各自给出 reason
 *   [6] 无屏（embedded）：字幕页与热键整块不渲染，各自给出 reason
 *   [7] `(位, reason)` 遍历：七种 reason 各按一遍全部宿主位，zh 与 en 都不许漏出 i18n key
 *
 * 自包含：和 a11y.mjs 一样自己起 vite preview（**端口由内核分配**，别人占着就失败；
 * 起好之后还要认一遍"端上跑的就是这份 `dist`"）——两条都在 `preview.mjs` 里，
 * 五个自查脚本共用同一道闸，这里不再自己实现一份。
 * 用法：npm run build && npm run check:capabilities
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

/** 内核 `UnavailableReason` 全集（`crates/vox-core/src/capability.rs`）。 */
const REASONS = [
  "unsupported",
  "not_installed",
  "permission",
  "not_built",
  "not_wired",
  "pending_reboot",
  "busy",
];

/** 每个 reason 在 zh / en 里必须出现的那句特征文案（句子在 `src/i18n/*`）。 */
const REASON_MARK = {
  unsupported: { zh: "这台设备做不到", en: "can't do this" },
  not_installed: { zh: "需要先装虚拟声卡驱动", en: "install the virtual audio driver" },
  permission: { zh: "需要先授权", en: "Needs permission first" },
  not_built: { zh: "这个构建里没编进去", en: "Not compiled into this build" },
  not_wired: { zh: "装配层还没把它接上", en: "hasn't wired it up yet" },
  pending_reboot: { zh: "需要重启系统才生效", en: "reboot is required" },
  busy: { zh: "暂时用不了", en: "Temporarily unavailable" },
};

/** 宿主位的全集（`src/capabilities.ts` 的 `HOST_CAPABILITIES`）。 */
const HOST_BITS = [
  "mic",
  "program_tap",
  "virtual_mic",
  "captions",
  "global_hotkey",
  "tray",
  "background_service",
  "vr_captions",
  "net_in",
  "net_out",
  "file_config",
];

/** Windows 档位上限里那 8 位：`?off=` 只能按这些（上限之外的位按不动，上限是硬的）。 */
const WINDOWS_CEILING = [
  "mic",
  "program_tap",
  "virtual_mic",
  "captions",
  "global_hotkey",
  "tray",
  "background_service",
  "vr_captions",
];

const failures = [];

function expect(condition, message) {
  if (!condition) failures.push(message);
}

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

/** 打开某个 URL 的某一页（点侧栏，等页面标题出现）。 */
async function openPage(page, url, pageId, label) {
  await page.goto(url, { waitUntil: "networkidle" });
  await page.click(`.sidebar .nav-item[data-page="${pageId}"]`);
  await page.getByRole("heading", { name: label, exact: true }).waitFor();
  await page.waitForTimeout(150);
}

/**
 * 当前这一页。**八个页面全在 DOM 里**（只靠 `.page.active` 切显），所以位相关的
 * 计数与取文案都必须限定在当前页，否则会数到别的页面上那份。
 */
function active(page) {
  return page.locator(".page.active");
}

/** 当前页里位为假时那句说明的文案（位为真、或不在当前页时是 `null`）。 */
async function noteText(page, bit) {
  const note = active(page).locator(`[data-capability-note="${bit}"]`);
  return (await note.count()) > 0 ? (await note.first().innerText()).trim() : null;
}

async function switchLanguage(page) {
  // 侧栏底部的语言按钮循环 zh-CN → ja-JP → en；点两次到英文。
  const button = page.locator(".sidebar-bottom .nav-item").last();
  await button.click();
  await page.waitForTimeout(200);
  await button.click();
  await page.waitForTimeout(300);
}

const preview = await startPreview("check:capabilities");
const BASE = `${preview.base}/?mock=1`;

let browser;
try {
  browser = await launch();
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });

  // ── [1] Windows 桌面：位为真的入口照旧，位为假的换成 reason ──────────────────
  await openPage(page, BASE, "home", "首页");
  expect(
    (await page.locator("#dd-home-listen-target").count()) === 1,
    "Windows：抓程序位为真，监听程序选择器必须在",
  );

  await openPage(page, BASE, "settings", "设置");
  const cablePanel = page.locator(".settings-item").filter({ hasText: "虚拟麦克风" });
  await cablePanel.getByText("已安装", { exact: true }).waitFor();
  expect(
    (await active(page).locator('[data-capability-note="virtual_mic"]').count()) === 0,
    "Windows 且已装 VB-CABLE：虚拟麦位为真，不该有降级说明",
  );
  expect(
    (await cablePanel.getByRole("button", { name: "卸载", exact: true }).count()) === 1,
    "Windows：安装器状态那一套管理动作照旧",
  );

  // 位跟着事实走：卸掉驱动 ⇒ 位翻假并给出"先装驱动"的出路；装回来 ⇒ 位翻真、说明撤下。
  // 这条同时验了刷新通道（`devices_changed` → 前端重取快照里的 capabilities）。
  await cablePanel.getByRole("button", { name: "卸载", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "卸载虚拟麦克风" });
  await dialog.getByRole("button", { name: "关闭应用并卸载" }).click();
  await cablePanel.getByText("未安装", { exact: true }).waitFor();
  const afterUninstall = await noteText(page, "virtual_mic");
  expect(
    !!afterUninstall && afterUninstall.includes(REASON_MARK.not_installed.zh),
    "卸掉 VB-CABLE：虚拟麦位该翻假，并说清「先装驱动」这条出路",
  );
  await cablePanel.getByRole("button", { name: "安装", exact: true }).click();
  await cablePanel.getByText("已安装", { exact: true }).waitFor();
  expect(
    (await noteText(page, "virtual_mic")) === null,
    "装回来：虚拟麦位该翻真，降级说明撤下",
  );

  await openPage(page, BASE, "vrchat", "VRChat");
  expect(
    (await page.getByRole("switch", { name: "SteamVR 字幕", exact: true }).count()) === 0,
    "vr_captions 位为假（这份构建没编进去）：SteamVR 开关不许渲染（R5）",
  );
  const vrNote = await noteText(page, "vr_captions");
  expect(!!vrNote && vrNote.includes(REASON_MARK.not_built.zh), "vr_captions 缺 not_built 说明");

  await openPage(page, BASE, "about", "关于");
  const chips = active(page).locator("[data-capability]");
  expect((await chips.count()) === HOST_BITS.length, `能力清单应有 ${HOST_BITS.length} 个位`);
  for (const bit of HOST_BITS) {
    expect(
      (await active(page).locator(`[data-capability="${bit}"]`).count()) === 1,
      `能力清单缺位：${bit}`,
    );
  }
  expect(
    (await active(page).locator('[data-capability="mic"]').getAttribute("data-enabled")) === "true",
    "Windows：mic 位应为真",
  );
  expect(
    (await active(page).locator('[data-capability="vr_captions"]').getAttribute("data-enabled")) ===
      "false",
    "Windows：vr_captions 位应为假（没编进去）",
  );
  expect(
    !(await active(page).innerText()).includes("capabilities."),
    "能力清单漏出了 i18n key（zh）",
  );

  // ── [2] `?on=vr_captions`：位为真 ⇒ 开关照旧 ────────────────────────────────
  await openPage(page, `${BASE}&on=vr_captions`, "vrchat", "VRChat");
  expect(
    (await page.getByRole("switch", { name: "SteamVR 字幕", exact: true }).count()) === 1,
    "vr_captions 位为真：SteamVR 开关照旧（R5）",
  );
  expect(
    (await active(page).locator('[data-capability-note="vr_captions"]').count()) === 0,
    "位为真时不该有降级说明",
  );

  // ── [3] Linux 桌面：位为真 ⇒ 只给设备名引导，没有装/卸 ──────────────────────
  await openPage(page, `${BASE}&host=linux`, "settings", "设置");
  const linuxPanel = page.locator(".settings-item").filter({ hasText: "虚拟麦克风" });
  await linuxPanel.getByText("由 PipeWire 提供", { exact: true }).waitFor();
  await linuxPanel.getByText(/Vox Virtual Mic/).waitFor();
  for (const label of ["安装", "卸载"]) {
    expect(
      (await linuxPanel.getByRole("button", { name: label, exact: true }).count()) === 0,
      `Linux 上不该出现「${label}」按钮`,
    );
  }
  expect(
    (await active(page).locator('[data-capability-note="virtual_mic"]').count()) === 0,
    "Linux 已接线：虚拟麦位为真，不该有降级说明",
  );

  // ── [4] Linux 中间态：`not_wired` ⇒ 撤下"去选设备"那句 ──────────────────────
  await openPage(page, `${BASE}&host=linux&off=virtual_mic:not_wired`, "settings", "设置");
  const notWired = await noteText(page, "virtual_mic");
  expect(
    !!notWired && notWired.includes(REASON_MARK.not_wired.zh),
    "not_wired：应给「装配层还没接上」的说明",
  );
  expect(
    (await active(page).innerText()).includes("Vox Virtual Mic") === false,
    "位为假时不许再让用户去选一个不存在的设备（§1.4 的老毛病）",
  );

  // ── [5] Android：抓程序 / 虚拟麦的入口不渲染 ────────────────────────────────
  await openPage(page, `${BASE}&host=android`, "home", "首页");
  expect(
    (await page.locator("#dd-home-listen-target").count()) === 0,
    "Android：抓程序位为假，选择器不许渲染",
  );
  const tapNote = await noteText(page, "program_tap");
  expect(
    !!tapNote && tapNote.includes(REASON_MARK.unsupported.zh),
    "Android：抓程序要给「这台设备做不到」的说明（R9）",
  );
  const listenCard = page.locator(".stat-card").filter({ hasText: "听人说话" });
  expect(
    !(await listenCard.innerText()).includes("运行中"),
    "Android：抓程序位为假，听人说话不该显示运行中（假后端也不许起这条腿）",
  );
  expect(
    await listenCard.getByRole("button", { name: "启动", exact: true }).isDisabled(),
    "Android：听人说话启动键应禁用（位为假，点下去也只会在装配时撞空）",
  );
  expect(
    (await page.getByText("运行中", { exact: true }).count()) >= 1,
    "Android：对外说话不受这一位影响，照旧在跑",
  );

  await openPage(page, `${BASE}&host=android`, "settings", "设置");
  const androidMic = await noteText(page, "virtual_mic");
  expect(
    !!androidMic && androidMic.includes(REASON_MARK.unsupported.zh),
    "Android：虚拟麦要给「这台设备做不到」的说明",
  );
  expect(
    (await page.getByRole("button", { name: "安装", exact: true }).count()) === 0,
    "Android：没有「装驱动」这一步，不该出现安装按钮",
  );

  // ── [6] 无屏：字幕页 / 热键整块不渲染 ───────────────────────────────────────
  await openPage(page, `${BASE}&host=embedded`, "subtitle", "字幕外观");
  expect(
    (await page.locator("#dd-subtitle-font").count()) === 0,
    "无屏档：字幕位为假，外观设置不许渲染",
  );
  const captionNote = await noteText(page, "captions");
  expect(
    !!captionNote && captionNote.includes(REASON_MARK.unsupported.zh),
    "无屏档：字幕要给「这台设备做不到」的说明",
  );

  await openPage(page, `${BASE}&host=embedded`, "settings", "设置");
  expect(
    (await page.locator("#dd-speak-key").count()) === 0,
    "无屏档：热键位为假，热键编辑器不许渲染",
  );
  const hotkeyNote = await noteText(page, "global_hotkey");
  expect(
    !!hotkeyNote && hotkeyNote.includes(REASON_MARK.unsupported.zh),
    "无屏档：热键要给「这台设备做不到」的说明",
  );

  // 同一个入口、换成"少了 input 组"这一档 reason：文案要给出路（R2）
  await openPage(page, `${BASE}&host=linux&off=global_hotkey:permission`, "settings", "设置");
  const hotkeyPermission = await noteText(page, "global_hotkey");
  expect(
    !!hotkeyPermission && hotkeyPermission.includes("input"),
    "无 input 组：热键说明要带「加进 input 组」这条出路（R2）",
  );

  // 托盘这一位：位为假时要说清"关窗只会最小化"以及怎么把托盘弄回来（R2）
  await openPage(page, `${BASE}&host=embedded`, "about", "关于");
  const trayNote = await noteText(page, "tray");
  expect(
    !!trayNote && trayNote.includes("AppIndicator") && trayNote.includes("最小化"),
    "无托盘宿主：说明要带「装扩展」的出路与「关窗只会最小化」的后果（R2）",
  );

  // ── [7] `(位, reason)` 遍历：七种 reason × 全部宿主位，zh / en 都不漏 key ────
  for (const reason of REASONS) {
    const off = WINDOWS_CEILING.map((bit) => `${bit}:${reason}`).join(",");
    await openPage(page, `${BASE}&off=${off}`, "about", "关于");
    const notes = await active(page).locator("[data-capability-note]").count();
    expect(notes === HOST_BITS.length, `${reason}：${HOST_BITS.length} 个位都该有说明，实际 ${notes}`);
    const zh = await page.locator(".page.active").innerText();
    expect(!zh.includes("capabilities."), `${reason}：zh 漏出 i18n key`);
    expect(zh.includes(REASON_MARK[reason].zh), `${reason}：zh 缺这句特征文案`);

    await switchLanguage(page);
    const en = await page.locator(".page.active").innerText();
    expect(!en.includes("capabilities."), `${reason}：en 漏出 i18n key`);
    expect(
      en.includes(REASON_MARK[reason].en),
      `${reason}：en 缺这句特征文案（英文包不许漏条目）`,
    );
  }

  if (failures.length > 0) {
    console.error(failures.join("\n"));
    process.exitCode = 1;
  } else {
    console.log("能力位消费端：位为假的入口不渲染、说明带 reason，zh / en 文案齐全。");
  }
} finally {
  await browser?.close();
  preview.stop();
}
