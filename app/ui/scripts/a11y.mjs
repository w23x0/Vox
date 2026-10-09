/**
 * 界面自查：不靠看截图，全部是程序化断言。
 *
 * 八页 × 亮暗两主题，每页查：
 *   1. 运行时干净        —— 控制台没有 error/warning，没有未捕获异常，没有 4xx/5xx
 *   2. 控件有可读名字    —— label[for] / aria-label / 自身文本
 *   3. settings-item 真的左右分栏 —— 文字在左、控件在右且同一行
 *   4. 不出现横向滚动    —— 宽窗窄窗都不许
 *   5. 禁用控件旁有原因  —— 不把「为什么不能点」藏进 hover
 *   6. 页面不超 2 屏     —— 防止滑回长滚动
 * 最后查一次键盘遍历、焦点环、自绘下拉的键盘可用性和点空白收起。
 *
 * 用法：npm run build && npm run a11y
 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

/** 和 src/nav.ts 的 NAV_GROUPS 保持一致（含顺序）。对不上就是导航表改了没同步过来。 */
const PAGES = [
  { id: "home", label: "首页" },
  { id: "models", label: "模型" },
  { id: "subtitle", label: "字幕" },
  { id: "vrchat", label: "VRChat" },
  { id: "agent", label: "Agent 控制" },
  { id: "settings", label: "设置" },
  { id: "usage", label: "用量" },
  { id: "about", label: "关于" },
];

const PAGE_BUTTONS = ".sidebar .nav-item[data-page]";

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

let bad = 0;
const fail = (msg) => {
  bad += 1;
  console.log(`  ✗ ${msg}`);
};
const ok = (msg) => console.log(`  ✓ ${msg}`);

/** 页内断言，全部在浏览器里算，返回纯数据。 */
const AUDIT = () => {
  const out = { unnamed: [], badRows: [], overflow: null, silentDisabled: [], size: null };
  const page = document.querySelector(".page.active");
  if (!page) return { ...out, overflow: "找不到 .page.active" };

  // 1) 控件必须有可读名字
  const ctlSel =
    "input, select, textarea, button, [role=switch], [role=meter], [role=slider], [role=option]";
  for (const el of page.querySelectorAll(ctlSel)) {
    if (el.closest("[aria-hidden=true]")) continue;
    const id = el.getAttribute("id");
    const named =
      (id && document.querySelector(`label[for="${CSS.escape(id)}"]`)) ||
      el.closest("label") ||
      el.getAttribute("aria-label") ||
      (el.getAttribute("aria-labelledby") &&
        document.getElementById(el.getAttribute("aria-labelledby"))) ||
      (el.textContent || "").trim().length > 0;
    if (!named) {
      out.unnamed.push(
        `${el.tagName.toLowerCase()}${id ? "#" + id : ""}${el.className ? "." + String(el.className).split(" ")[0] : ""}`,
      );
    }
  }

  // 2) settings-item：文字在左、控件在右、同一行
  for (const row of page.querySelectorAll(".settings-item")) {
    const left = row.querySelector(":scope > .si-text");
    const ctl = row.querySelector(":scope > .si-control");
    if (!left || !ctl) {
      out.badRows.push("settings-item 缺 si-text 或 si-control");
      continue;
    }
    const a = left.getBoundingClientRect();
    const b = ctl.getBoundingClientRect();
    if (a.width === 0 || b.width === 0) continue; // 隐藏行不算
    const label = (left.textContent || "").replace(/\s+/g, " ").trim().slice(0, 24);
    if (a.left >= b.left) {
      out.badRows.push(
        `「${label}」控件没在右侧（文字 ${Math.round(a.left)} / 控件 ${Math.round(b.left)}）`,
      );
    } else if (Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) <= 0) {
      out.badRows.push(`「${label}」控件掉到了下一行`);
    }
  }

  // 3) 不许有横向滚动。main 自己滚纵向，横向溢出就是宽度算错了
  const main = document.querySelector(".app-content");
  const doc = document.documentElement;
  if (doc.scrollWidth > doc.clientWidth + 1)
    out.overflow = `document ${doc.scrollWidth} > ${doc.clientWidth}`;
  else if (main && main.scrollWidth > main.clientWidth + 1)
    out.overflow = `main ${main.scrollWidth} > ${main.clientWidth}`;

  // 4) 禁用控件所在的行里必须能看到原因
  for (const el of page.querySelectorAll("[disabled], [aria-disabled=true]")) {
    if (el.closest("[aria-hidden=true]")) continue;
    // 整块关掉的区域：原因写在那块外面的总开关上，逐行再报一遍是噪音
    if (el.closest("[data-block-disabled]")) continue;
    const row = el.closest(".settings-item, .card, .panel");
    const txt = row ? row.textContent || "" : "";
    /*
     * 关键词判断本来就糙：文案换个说法就会误报。
     * 宁可放宽 —— 漏判一个不如天天喊狼来了，那样这条检查就没人看了。
     */
    if (!/不生效|用不了|先|没有|没在|已关闭|等待|暂无|需要|才能|当前|固定|没选|没扫到|不在|要先|粘贴/.test(txt)) {
      out.silentDisabled.push(
        `${el.tagName.toLowerCase()} —— 所在行：${txt.replace(/\s+/g, " ").trim().slice(0, 60) || "（找不到行）"}`,
      );
    }
  }

  // 5) 页面高度。改成切页就是为了别再出现长滚动，给个上限盯着
  if (main) out.size = { h: main.scrollHeight, view: main.clientHeight };

  return out;
};

const preview = await startPreview("a11y");
const BASE = preview.base;

try {
  const browser = await launch();
  // 按真窗口默认尺寸：tauri.conf.json 里 main 窗口是 1040×780
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1040, height: 780 } });

  // 运行时噪音全接住
  const noise = [];
  page.on("console", (m) => {
    if (m.type() !== "error" && m.type() !== "warning") return;
    if (m.text().startsWith("Failed to load resource")) return; // 不带 URL，交给 response 钩子
    noise.push(`[${m.type()}] ${m.text()}`);
  });
  page.on("pageerror", (e) => noise.push(`[pageerror] ${e.message}`));
  page.on("response", (r) => {
    if (r.status() < 400) return;
    if (/favicon/i.test(r.url())) return; // Tauri 窗口没标签页，不配图标是故意的
    noise.push(`[${r.status()}] ${r.url()}`);
  });

  await page.goto(`${BASE}/?mock=1`, { waitUntil: "networkidle" });
  await page.waitForSelector(PAGE_BUTTONS);
  await page.waitForTimeout(1200);

  const navCount = await page.$$eval(PAGE_BUTTONS, (n) => n.length);
  console.log(`[0] 外壳：侧栏 ${navCount} 项（应为 ${PAGES.length}）`);
  if (navCount !== PAGES.length) fail(`侧栏项数不对：${navCount}`);
  else ok("侧栏项数对得上");

  const sidebarGeometry = await page.evaluate(() => {
    const sidebar = document.querySelector(".sidebar");
    if (!sidebar) return null;
    const rect = sidebar.getBoundingClientRect();
    return {
      top: rect.top,
      bottom: window.innerHeight - rect.bottom,
      appHeightVar: document.documentElement.style.getPropertyValue("--app-h"),
    };
  });
  if (!sidebarGeometry) fail("找不到侧栏，无法检查上下结构");
  else if (Math.abs(sidebarGeometry.top - 44) > 0.5)
    fail(`侧栏顶部应留 44px，实际 ${sidebarGeometry.top}px`);
  else if (Math.abs(sidebarGeometry.bottom - 16) > 0.5)
    fail(`侧栏底部应留 16px，实际 ${sidebarGeometry.bottom}px`);
  else if (sidebarGeometry.appHeightVar)
    fail(`侧栏仍依赖 --app-h：${sidebarGeometry.appHeightVar}`);
  else ok("侧栏上下结构为 top 44px / bottom 16px，且不依赖 JS 高度变量");

  // 亮暗两遍都要过。规格的 token 是两套值，只测一套等于只测一半
  for (const theme of ["light", "dark"]) {
    console.log(`\n===== ${theme === "light" ? "亮色" : "暗色"}主题 =====`);
    await page.evaluate((t) => {
      document.documentElement.setAttribute("data-theme", t);
    }, theme);
    await page.waitForTimeout(500);

    for (const [i, p] of PAGES.entries()) {
      const navs = await page.$$(PAGE_BUTTONS);
      await navs[i].click();
      await page.waitForTimeout(350);

      // 点了之后必须真的切过去
      const state = await page.evaluate(
        (id) => {
          const sec = document.getElementById(`page-${id}`);
          const nav = document.querySelector(".sidebar .nav-item[data-page].active");
          return {
            active: sec?.classList.contains("active") ?? false,
            navLabel: nav?.getAttribute("aria-label") ?? null,
            title: document.querySelector(".page.active .page-head h1")?.textContent ?? "",
          };
        },
        p.id,
      );
      if (!state.active) fail(`${p.label}：点了侧栏但页面没切过去`);
      if (state.navLabel !== p.label) fail(`${p.label}：侧栏选中态是「${state.navLabel}」`);
      if (!state.title.includes(p.label)) fail(`${p.label}：页面标题是「${state.title}」`);

      const r = await page.evaluate(AUDIT);
      r.unnamed.forEach((u) => fail(`${p.label}：没有可读名字 ${u}`));
      r.badRows.forEach((b) => fail(`${p.label}：行没左右分栏 ${b}`));
      if (r.overflow) fail(`${p.label}：横向溢出 ${r.overflow}`);
      r.silentDisabled.forEach((s) => fail(`${p.label}：禁用但看不到原因 ${s}`));
      if (r.size && r.size.view > 0 && r.size.h > r.size.view * 2)
        fail(`${p.label}：内容 ${r.size.h}px 超过 2 屏（视口 ${r.size.view}px）`);

      const rows = await page.$$eval(".page.active .settings-item", (n) => n.length);
      if (
        r.unnamed.length === 0 &&
        r.badRows.length === 0 &&
        !r.overflow &&
        r.silentDisabled.length === 0
      ) {
        ok(
          `${p.label} 干净（设置行 ${rows} 条，内容 ${r.size?.h ?? "?"}px / ${r.size?.view ?? "?"}px）`,
        );
      }
    }
  }

  // 窄窗：侧栏是 fixed 的，main 靠 left/right 定位，窄了不该出横向滚动
  console.log("\n[9] 窄窗 900px（真窗口的 minWidth）");
  await page.setViewportSize({ width: 900, height: 700 });
  await page.waitForTimeout(400);
  const badBefore = bad;
  for (const [i, p] of PAGES.entries()) {
    const navs = await page.$$(PAGE_BUTTONS);
    await navs[i].click();
    await page.waitForTimeout(250);
    const of = await page.evaluate(() => {
      const d = document.documentElement;
      const m = document.querySelector(".app-content");
      if (d.scrollWidth > d.clientWidth + 1) return `document ${d.scrollWidth}>${d.clientWidth}`;
      if (m && m.scrollWidth > m.clientWidth + 1) return `main ${m.scrollWidth}>${m.clientWidth}`;
      return null;
    });
    if (of) fail(`${p.label} 窄窗横向溢出：${of}`);
  }
  if (bad === badBefore) ok(`${PAGES.length} 页在 900px 都没有横向溢出`);
  await page.setViewportSize({ width: 1040, height: 780 });
  await page.waitForTimeout(300);

  // 自绘下拉：原生 select 白送的东西，自绘之后得自己接
  console.log("\n[10] 自绘下拉");
  await page.click('.sidebar-nav .nav-item[aria-label="首页"]');
  await page.waitForTimeout(350);
  /*
   * 必须用 Playwright 的 click 再 await，不能在 page.evaluate 里同步 el.click()
   * 然后当场查 DOM —— React 的 setState 是批量异步的，那一刻还没重渲染，
   * 查出来永远是「没展开」（这里踩过一次）。
   */
  await page.click(".page.active .dropdown-selected");
  await page.waitForTimeout(250);
  const ddOpened = await page.evaluate(() => {
    const t = document.querySelector(".page.active .dropdown-selected");
    return {
      open: !!document.querySelector(".page.active .dropdown-options.show"),
      expanded: t?.getAttribute("aria-expanded"),
      role: document.querySelector(".page.active .dropdown-options")?.getAttribute("role"),
      options: document.querySelectorAll(".page.active .dropdown-options .dropdown-option").length,
    };
  });
  if (!ddOpened.open) fail("点触发器没展开");
  else if (ddOpened.expanded !== "true") fail("展开了但 aria-expanded 不是 true");
  else if (ddOpened.role !== "listbox") fail(`菜单 role 是 ${ddOpened.role}，应为 listbox`);
  else if (ddOpened.options === 0) fail("展开了但一个选项都没有");
  else ok(`点开展开、aria 正确（${ddOpened.options} 个选项）`);

  // 点页面空白处要收起。点页面大标题 —— 它一定存在、没有 click 处理，
  // 也不像 .app-titlebar-text 那样被 pointer-events:none 挡住。
  await page.click(".page.active .page-head h1");
  await page.waitForTimeout(250);
  const closed = await page.evaluate(() => !document.querySelector(".dropdown-selected.active"));
  if (!closed) fail("点空白处没有收起");
  else ok("点空白处收起");

  // 键盘：Esc 收起、方向键能走
  const kb = await page.evaluate(() => {
    const t = document.querySelector(".page.active .dropdown-selected");
    t?.focus();
    return document.activeElement === t;
  });
  if (!kb) fail("下拉触发器聚焦不上");
  else {
    await page.keyboard.press("Enter");
    await page.waitForTimeout(200);
    const openedByKey = await page.evaluate(() => !!document.querySelector(".dropdown-selected.active"));
    await page.keyboard.press("Escape");
    await page.waitForTimeout(200);
    const closedByEsc = await page.evaluate(() => !document.querySelector(".dropdown-selected.active"));
    if (!openedByKey) fail("回车打不开下拉");
    else if (!closedByEsc) fail("Esc 关不掉下拉");
    else ok("回车展开、Esc 收起");
  }

  // 键盘遍历：Tab 只循环窗口红绿灯（GlassUI 规范），内容区导航走方向键。
  // 这里验证 Tab 能在 3 个红绿灯之间循环到，并且重复 Tab 不会卡死空转。
  console.log("\n[11] 键盘遍历");
  const traffic = await page.evaluate(() => {
    const btns = [...document.querySelectorAll(".window-traffic button")];
    btns.forEach((el, i) => el.setAttribute("data-a11y-i", String(i)));
    return btns.length;
  });
  const seen = new Set();
  for (let i = 0; i < traffic + 20; i += 1) {
    await page.keyboard.press("Tab");
    const who = await page.evaluate(() => {
      const a = document.activeElement;
      if (!a || a === document.body) return null;
      const n = a.getAttribute?.("data-a11y-i");
      return n !== null && n !== undefined ? `#${n}` : `${a.tagName}:${a.className || ""}`;
    });
    if (who) seen.add(who);
  }
  console.log(`  红绿灯 ${traffic} 个，Tab 走到 ${seen.size} 个`);
  if (traffic > 0 && seen.size < Math.min(traffic, 3)) fail(`Tab 只走到 ${seen.size} 个，可能有键盘陷阱`);
  else ok(`Tab 循环红绿灯不卡死`);

  // 焦点环
  console.log("\n[12] 焦点环");
  const ring = await page.evaluate(() => {
    const el = document.querySelector(".page.active button:not([disabled])");
    if (!el) return null;
    el.focus();
    const cs = getComputedStyle(el);
    return {
      outlineWidth: cs.outlineWidth,
      outlineStyle: cs.outlineStyle,
      color: cs.outlineColor,
      boxShadow: cs.boxShadow,
    };
  });
  console.log(`  ${JSON.stringify(ring)}`);
  const hasOutline = ring && ring.outlineStyle !== "none" && ring.outlineWidth !== "0px";
  const hasShadow = ring && ring.boxShadow !== "none";
  if (!hasOutline && !hasShadow) fail("焦点环不可见");
  else ok("焦点有可见指示");

  // 主题切换：规格第 5 节要求三处 token 都生效，切换要双向
  console.log("\n[13] 主题切换");
  // 页面遍历最后停在设置页；设置组是透明布局容器，不适合拿来比较卡片 token。
  // 固定回首页量实体状态卡，避免页面顺序影响主题断言。
  await page.click(`${PAGE_BUTTONS}[aria-label="首页"]`);
  await page.waitForTimeout(250);
  const themed = await page.evaluate(() => {
    /*
     * --bg 是渐变，所以底色落在 background-image 上，background-color 是透明的 ——
     * 读 backgroundColor 会永远得到 rgba(0,0,0,0)（这里踩过一次）。
     * 顺带量一个卡片的文字色，确认不是只有 body 换了。
     */
    const read = () => {
      // 濒在 body 上会被 canvas 背景传播铺到四角（黑角根因）。背板已迁到 #root，
      // 主题断言跟随它取，不再从 body 读。
      const bgNav = document.querySelector("#root") ?? document.body;
      const card = document.querySelector(".page.active .stat-card, .page.active .card");
      const bg = getComputedStyle(bgNav);
      return {
        bg: bg.backgroundImage || bg.backgroundColor,
        text: getComputedStyle(document.body).color,
        cardBg: card ? getComputedStyle(card).backgroundColor : null,
      };
    };
    const root = document.documentElement;
    root.setAttribute("data-theme", "light");
    const light = read();
    root.setAttribute("data-theme", "dark");
    const dark = read();
    return { light, dark };
  });
  const shortBg = (s) => (s || "").replace(/linear-gradient\(/, "").slice(0, 46);
  if (themed.light.bg === themed.dark.bg) fail(`亮暗底色一样 —— token 没生效`);
  else if (themed.light.text === themed.dark.text) fail("亮暗文字色一样");
  else if (themed.light.cardBg === themed.dark.cardBg) fail("亮暗卡片底色一样");
  else {
    ok(`底色 亮[${shortBg(themed.light.bg)}] / 暗[${shortBg(themed.dark.bg)}]`);
    ok(`文字 亮 ${themed.light.text} / 暗 ${themed.dark.text}`);
    ok(`卡片 亮 ${themed.light.cardBg} / 暗 ${themed.dark.cardBg}`);
  }

  /*
   * 风格红线（UI 重设计后）：
   *   1. 界面字体是系统无衬线栈，不再打包 CJK 字体；等宽只给数值 / ID / 密钥（.num / .mono）；
   *      正文、标题、标签不加字距；页面标题 22px / 700 / 正文色（不用强调色）
   *   2. 浮空面板**无装饰性边框**（--border 恒为 transparent，轮廓靠填充明度 + 极轻垂直阴影）
   *   3. 没有顶部工具条 —— 标题栏只放固定应用名
   *   4. 侧栏：图标 + 文字、三个分组标题、竖条指示器、底部只有版本号（语言 / 主题在设置页）
   */
  console.log("\n[14] 风格红线");
  const style = await page.evaluate(() => {
    const body = getComputedStyle(document.body);
    const panel = document.querySelector(".page.active .panel, .page.active .settings-group, .page.active .stat-card");
    const bar = document.querySelector(".app-titlebar");
    // 标题栏里除了那行文字，不许有任何可交互元素
    const barCtl = bar
      ? bar.querySelectorAll("button, input, select, a[href], [tabindex='0']").length
      : -1;
    const h1 = document.querySelector(".page.active .page-head h1");
    const h1s = h1 ? getComputedStyle(h1) : null;
    const labels = [...document.querySelectorAll(".sidebar .nav-item[data-page] .nav-label")];
    return {
      font: body.fontFamily,
      bodySpacing: body.letterSpacing,
      h1: h1s && {
        size: h1s.fontSize,
        weight: h1s.fontWeight,
        spacing: h1s.letterSpacing,
        color: h1s.color,
        bodyColor: body.color,
      },
      monoVar: getComputedStyle(document.documentElement).getPropertyValue("--font-mono"),
      panelClass: panel?.className ?? null,
      panelBorder: panel ? getComputedStyle(panel).borderTopColor : null,
      barCtl,
      navDots: document.querySelectorAll(".sidebar .nav-item[data-page] .nav-dot").length,
      groupTitles: [...document.querySelectorAll(".sidebar .nav-group-title")].filter(
        (e) => e.getBoundingClientRect().width > 0,
      ).length,
      labelsVisible: labels.filter((e) => e.getBoundingClientRect().width > 0).length,
      legacyFooterItems: document.querySelectorAll(".sidebar-bottom").length,
      version: document.querySelector(".sidebar-foot .sidebar-version")?.textContent ?? "",
      sidebarWidth: document.querySelector(".sidebar")?.getBoundingClientRect().width ?? 0,
    };
  });
  if (/Maple|GlassUI/i.test(style.font) || !/system-ui/.test(style.font))
    fail(`正文字体应是系统无衬线栈（system-ui 在前，不含 Maple / GlassUI）：${style.font.slice(0, 80)}`);
  else ok("正文字体是系统无衬线栈");
  if (!/JetBrains Mono/.test(style.monoVar)) fail(`--font-mono 应以 JetBrains Mono 打头：${style.monoVar}`);
  else ok("--font-mono = JetBrains Mono（只给数值 / ID / 密钥）");
  if (style.bodySpacing !== "normal") fail(`正文不应加字距：${style.bodySpacing}`);
  else if (!style.h1 || style.h1.spacing !== "normal") fail(`页面标题不应加字距：${style.h1?.spacing}`);
  else ok("正文与标题没有字距");
  if (!style.h1 || style.h1.size !== "22px" || style.h1.weight !== "700") {
    fail(`页面标题应是 22px / 700：${JSON.stringify(style.h1)}`);
  } else if (style.h1.color !== style.h1.bodyColor) {
    fail(`页面标题应用正文色而非强调色：${style.h1.color} vs ${style.h1.bodyColor}`);
  } else ok("页面标题 22px / 700 / 正文色");

  // 无装饰性边框：面板 borderTopColor 必须是透明的（alpha=0）。
  // 旧规格的「亮边」红线在 GlassUI 无边化后反了过来 —— 描边恢复了才叫破功。
  const alpha = (style.panelBorder || "").match(/[\d.]+/g);
  const hasVisibleBorder =
    style.panelBorder !== "transparent" && !(alpha && Number(alpha[3]) === 0);
  if (hasVisibleBorder)
    fail(`面板不应有装饰性边框：${style.panelBorder}（${style.panelClass}）`);
  else ok(`面板无边 ${style.panelBorder}`);

  if (style.barCtl !== 0) fail(`标题栏里有 ${style.barCtl} 个交互元素，应为 0`);
  else ok("标题栏只有固定应用名，没有工具条");

  if (style.labelsVisible !== PAGES.length)
    fail(`宽侧栏下每项都应有可见文字：${style.labelsVisible}/${PAGES.length}`);
  else if (style.groupTitles !== 3) fail(`侧栏应有 3 个组标题（翻译 / 连接 / 系统）：${style.groupTitles}`);
  else if (style.navDots !== PAGES.length)
    fail(`每个页面项都应有竖条指示器：${style.navDots}/${PAGES.length}`);
  else if (style.legacyFooterItems !== 0) fail("侧栏底部不应再有主题 / 语言按钮区");
  else if (!/^v\d+\.\d+\.\d+/.test(style.version)) fail(`侧栏底部应显示版本号，实际「${style.version}」`);
  else if (Math.round(style.sidebarWidth) !== 200) fail(`宽侧栏应为 200px，实际 ${style.sidebarWidth}`);
  else ok(`侧栏 200px：${PAGES.length} 项带文字、3 个组标题、竖条指示器、底部只有版本号 ${style.version}`);

  // 窄窗（< 860px）：收成纯图标，文字隐藏但可访问名还在（tooltip 靠它）
  await page.setViewportSize({ width: 820, height: 700 });
  await page.waitForTimeout(300);
  const narrow = await page.evaluate(() => ({
    width: document.querySelector(".sidebar")?.getBoundingClientRect().width ?? 0,
    labels: [...document.querySelectorAll(".sidebar .nav-label")].filter(
      (e) => e.getBoundingClientRect().width > 0,
    ).length,
    titles: [...document.querySelectorAll(".sidebar .nav-group-title")].filter(
      (e) => e.getBoundingClientRect().width > 0,
    ).length,
    named: [...document.querySelectorAll(".sidebar .nav-item[data-page]")].every((e) =>
      e.getAttribute("aria-label"),
    ),
  }));
  if (Math.round(narrow.width) !== 64) fail(`窄窗侧栏应收成 64px，实际 ${narrow.width}`);
  else if (narrow.labels !== 0 || narrow.titles !== 0) fail("窄窗侧栏应只剩图标（文字与组标题隐藏）");
  else if (!narrow.named) fail("窄窗纯图标侧栏的每项仍须有可访问名");
  else ok("820px：侧栏收成 64px 纯图标，可访问名保留");
  await page.setViewportSize({ width: 1040, height: 780 });
  await page.waitForTimeout(300);

  // 运行时噪音：前面所有页面的都攒在这儿
  console.log("\n[15] 运行时噪音");
  const uniq = [...new Set(noise)];
  if (uniq.length) uniq.slice(0, 12).forEach((n) => fail(n));
  else ok("控制台干净，没有未捕获异常");

  await browser.close();
  console.log(bad === 0 ? "\n全部通过。" : `\n${bad} 项要修。`);
  process.exitCode = bad === 0 ? 0 : 1;
} finally {
  preview.stop();
}
