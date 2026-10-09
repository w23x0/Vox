import { chromium } from "playwright";
const browser = await chromium.launch();
const p = await browser.newPage({ viewport: { width: 1040, height: 780 } });
await p.goto("http://127.0.0.1:5183/?mock=1", { waitUntil: "networkidle" });
await p.waitForTimeout(600);

const who = () =>
  p.evaluate(() => {
    const a = document.activeElement;
    return a ? a.id || a.tagName + "." + String(a.className).split(" ")[0] : "null";
  });
async function go(id) {
  await p.evaluate((sel) => document.getElementById(sel)?.focus(), id);
  await p.waitForTimeout(30);
}
async function step(key) {
  await p.keyboard.press(key);
  await p.waitForTimeout(30);
}
const inListen = async () => (await who()).includes("listen");

console.log("=== A. 完整纵向遍历（speak 目标语言一路↓到底再一路↑回原点）===");
await go("dd-home-target-language");
const chain = [await who()];
for (let i = 0; i < 5; i++) { await step("ArrowDown"); chain.push(await who()); }
console.log("  下", chain.join(" → "));
const ups = [await who()];
for (let i = 0; i < 5; i++) { await step("ArrowUp"); ups.push(await who()); }
console.log("  上", ups.join(" → "));

console.log("\n=== B. ↓ 全程不跨卡（speak 卡内控件一路下行）===");
await go("dd-home-input-device");
let crossed = false;
for (let i = 0; i < 4; i++) { await step("ArrowDown"); if ((await who()).includes("listen")) { crossed = true; break; } }
console.log(crossed ? "  ❌ 跨卡" : "  ✅ 未跨卡");

console.log("\n=== C. ← 在最左列 → 回侧栏，→ 重进内容 ===");
await go("dd-home-target-language");
await p.keyboard.press("ArrowRight");
console.log("  → (行内右移): " + (await who()));

await browser.close();