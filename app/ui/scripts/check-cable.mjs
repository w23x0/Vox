/** 虚拟麦克风管理区：安装状态、二次确认卸载、重新安装、16 声道隐藏与恢复。 */
import { chromium } from "playwright";

import { startPreview } from "./preview.mjs";

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

const preview = await startPreview("check:cable");
const origin = preview.base;

let browser;
try {
  browser = await launch();
  const page = await browser.newPage({ viewport: { width: 1040, height: 780 } });
  await page.goto(`${origin}/?mock=1`, { waitUntil: "networkidle" });
  await page.click('.sidebar .nav-item[data-page="settings"]');

  const panel = page.locator(".settings-item").filter({ hasText: "虚拟麦克风" });
  await panel.getByText("已安装", { exact: true }).waitFor();

  // 卸载走二次确认：假数据里 Discord 在占用，得先报出来再关
  await panel.getByRole("button", { name: "卸载", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "卸载虚拟麦克风" });
  await dialog.getByText("Discord", { exact: true }).waitFor();
  await dialog.getByRole("button", { name: "关闭应用并卸载" }).click();
  await panel.getByText("未安装", { exact: true }).waitFor();

  await panel.getByRole("button", { name: "安装", exact: true }).click();
  await panel.getByText("已安装", { exact: true }).waitFor();

  const channel = page.locator(".settings-item").filter({ hasText: "多声道设备" });
  await channel.getByText("已隐藏", { exact: true }).waitFor();
  await channel.getByRole("button", { name: "显示", exact: true }).click();
  await channel.getByText("已显示", { exact: true }).waitFor();
  await channel.getByRole("button", { name: "隐藏", exact: true }).click();
  await channel.getByText("已隐藏", { exact: true }).waitFor();
  console.log("虚拟麦克风管理区：安装、卸载、多声道隐藏与恢复全部通过。");

  // Linux（?host=linux）：没有装/卸/多声道那一套，只给状态 + 去哪选设备。
  await page.goto(`${origin}/?mock=1&host=linux`, { waitUntil: "networkidle" });
  await page.click('.sidebar .nav-item[data-page="settings"]');
  const linuxPanel = page.locator(".settings-item").filter({ hasText: "虚拟麦克风" });
  await linuxPanel.getByText("由 PipeWire 提供", { exact: true }).waitFor();
  await linuxPanel.getByText(/Vox Virtual Mic/).waitFor();
  for (const label of ["安装", "卸载"]) {
    const count = await linuxPanel.getByRole("button", { name: label, exact: true }).count();
    if (count > 0) throw new Error(`Linux 上不该出现「${label}」按钮`);
  }
  if ((await page.locator(".settings-item").filter({ hasText: "多声道设备" }).count()) > 0) {
    throw new Error("Linux 上不该出现多声道设备那一项");
  }
  console.log("Linux 虚拟麦克风：不出现安装/卸载/多声道，只给设备名引导。");
} finally {
  await browser?.close();
  preview.stop();
}
