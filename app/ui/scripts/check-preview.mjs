/**
 * `preview.mjs` 那道闸自己的自查：**端口被占**与**产物不是我们的**两种情形下，
 * 起预览都必须失败。要防的是假绿——第七轮 F3 那种"端口一通就开跑，断言跑在别人的旧构建上"。
 *
 * 三档：
 *
 * 1. **端口被占**：先让别人占住一个端口，再让 `startPreview` 去用那个端口 → 必须失败，
 *    绝不"将就用"那个陌生服务；
 * 2. **产物不是我们的**：陌生服务在端口上答一份"旧的 dist" → `expectOwnBuild` 必须认出
 *    不是同一份产物（五个自查脚本起服务时跑的都是这一个函数）；
 * 3. **反向对照**：`startPreview` 对着真 dist 必须起得来、跑得过自检、停得干净。
 *    没有这一条的话，把接口写成"永远报错"也能骗过前两档。
 *
 * 不拉浏览器，几秒钟跑完。
 * 用法：npm run build && npm run check:preview
 */
import { appendFileSync, cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { expectOwnBuild, portOpen, startPreview } from "./preview.mjs";

const UI_DIR = fileURLToPath(new URL("..", import.meta.url));

const failures = [];
const check = (condition, message) => {
  console.log(`  ${condition ? "✓" : "✗"} ${message}`);
  if (!condition) failures.push(message);
};

/** `dist/index.html` 里那个入口脚本的相对路径（`assets/index-XXXX.js`）。 */
function entryOf(dist) {
  const html = readFileSync(join(dist, "index.html"), "utf8");
  const entry = /<script[^>]+src="\.?\/?(assets\/[^"]+\.js)"/.exec(html)?.[1];
  if (!entry) throw new Error(`${dist}/index.html 里找不到入口脚本——先 npm run build。`);
  return entry;
}

/** 一个只会把 `root` 下的文件原样吐出去的服务：就当它是"别人的预览服务"（真 preview 也会这样对待 `/`）。 */
function serveFiles(root) {
  const server = createServer((request, response) => {
    const { pathname } = new URL(request.url, "http://127.0.0.1");
    // 目录请求给 index.html——真实的预览服务就是这么答 `/` 的。
    const target = decodeURIComponent(pathname);
    let body;
    try {
      // 先读、后写头：读失败时头还没发出去，才换得成 404。
      body = readFileSync(join(root, target.endsWith("/") ? `${target}index.html` : target));
    } catch {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200).end(body);
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({
        port,
        base: `http://127.0.0.1:${port}`,
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

/** 跑一段**必须**抛错的代码，把错误信息还回来（没抛就返回 `null`）。 */
async function mustFail(run) {
  try {
    await run();
    return null;
  } catch (error) {
    return error.message;
  }
}

/** 等端口真的空下来（SIGTERM 之后内核要收一下尾）。 */
async function waitClosed(port, tries = 20) {
  for (let i = 0; i < tries; i += 1) {
    if (!(await portOpen(port))) return true;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return false;
}

console.log("[1] 端口被占：起服务前就该失败");
{
  const squatter = await serveFiles(join(UI_DIR, "dist"));
  process.env.VOX_PREVIEW_PORT = String(squatter.port);
  const message = await mustFail(async () => {
    const preview = await startPreview("check:preview(占端口)");
    preview.stop(); // 走到这儿说明它"将就用"了陌生服务——正是要防的假绿。
  });
  delete process.env.VOX_PREVIEW_PORT;
  await squatter.close();
  check(
    message !== null && message.includes(String(squatter.port)),
    `端口 ${squatter.port} 被占时 startPreview 失败：${message ?? "居然起来了"}`,
  );
}

console.log("[2] 产物不是我们的：同一性自检要认出旧构建");
{
  const stale = mkdtempSync(join(tmpdir(), "vox-stale-dist-"));
  cpSync(join(UI_DIR, "dist"), stale, { recursive: true });
  // "旧构建"长这样：入口脚本还是那个名字，内容已经不一样了（vite 的产物名带内容哈希，
  // 所以真实的旧构建连名字都不一样；这里把名字故意留着，好让比对那一行真正被执行到）。
  const entry = entryOf(join(UI_DIR, "dist"));
  appendFileSync(join(stale, entry), "\n// 上一轮的产物\n");

  const stranger = await serveFiles(stale);
  const message = await mustFail(() => expectOwnBuild(stranger.base, UI_DIR));
  await stranger.close();
  rmSync(stale, { recursive: true, force: true });
  check(
    message !== null && message.includes(entry),
    `陌生服务答旧产物时自检失败：${message ?? "居然放行了"}`,
  );
}

console.log("[3] 反向对照：真 dist 上起得来、自检过、停得干净");
{
  const preview = await startPreview("check:preview");
  check(await portOpen(preview.port), `startPreview 起来了（端口 ${preview.port}）`);
  preview.stop();
  check(await waitClosed(preview.port), `stop() 之后端口 ${preview.port} 关掉了`);
}

if (failures.length > 0) {
  console.error(`\n${failures.length} 条不达标：`);
  console.error(failures.join("\n"));
  process.exitCode = 1;
} else {
  console.log("\n端口被占、旧产物两种情形都会失败；真 dist 正常起停。");
}
