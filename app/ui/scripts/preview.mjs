/**
 * 五个自查脚本（`a11y` / `qa:narrow` / `check:cable` / `check:capabilities` / `qa:home`）共用的
 * "起一个**自己的** vite preview"。
 *
 * 为什么不能像以前那样"固定端口 + 等到端口通就开跑"（verifier 第七轮 F3 复现过）：
 * 端口被别的进程占着时（上一次没清干净的 preview、别的项目、另一个自查脚本），那一等会
 * **立刻**成功，测试实际上跑在一个陌生服务上——它可能正好是旧构建，于是断言全过、假绿。
 *
 * 这里的规矩，五条都占：
 *
 * 1. 端口由内核分配（`listen(0)` 问出来的那个），不问到就失败；
 * 2. 起服务**之前**再确认这个端口是空的——那一刻通着就直接失败，绝不"将就用"；
 * 3. 直接跑 `node_modules/vite/bin/vite.js`（不经 `npx`：多一层壳就多一个杀不掉的子进程），
 *    `--strictPort` 让 vite 抢不到端口时自己退出；子进程若在就绪前退出，**失败**，并把它的
 *    stderr 带出来（"先 npm run build"这类原因都在这儿）；
 * 4. 就绪之后再做一次真 HTTP 取根路径，确认有个服务真在答（不是个只开着的端口）；
 * 5. 就绪之后**再**确认两件事，任一不满足即失败——这是"端口通了"到"能开跑"之间那条
 *    TOCTOU 缝的补丁：
 *    (a) **我们的**子进程还活着（`child.exitCode === null`）。端口通了不等于答话的是我们；
 *        从内核把端口交出去、到 vite 真的绑上，中间那一小段谁都可能插队，而子进程这时
 *        正带着 `--strictPort` 退出——看它的退出码比看端口诚实得多。产物自检完**再**看一次：
 *        自检期间同样可能死掉（那时端口上答话的可能已经是别人了）。
 *    (b) 服务的就是我们刚建出来的那份 `dist`（[`expectOwnBuild`] 逐字节比对入口脚本）。
 *        vite 的产物名带内容哈希，名字对得上、内容逐字节相同，才算同一份产物。
 *
 * 返回值：`{ port, base, stop() }`。`stop()` 幂等，脚本的 `finally` 里调它即可。
 */
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createConnection, createServer } from "node:net";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** 仓库里装好的 vite。直接跑它的入口，省掉 npx 那一层壳。 */
const VITE = fileURLToPath(new URL("../node_modules/vite/bin/vite.js", import.meta.url));

/** 默认工作目录：`vite preview` 就在这里跑，产物就是这里的 `dist/`。 */
const UI_DIR = fileURLToPath(new URL("..", import.meta.url));

/** 端口探活（800ms 超时）。 */
export function portOpen(port) {
  return new Promise((resolve) => {
    const socket = createConnection({ port, host: "127.0.0.1" });
    socket.on("connect", () => (socket.end(), resolve(true)));
    socket.on("error", () => resolve(false));
    setTimeout(() => (socket.destroy(), resolve(false)), 800);
  });
}

/** 向内核要一个此刻空闲的端口（`listen(0)` 问出来的那个）。 */
function freePort() {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

/**
 * 这次起在哪个端口上：默认问内核要一个（别人没法提前占住）。
 *
 * `VOX_PREVIEW_PORT` 能把端口钉住，**只给自查用**——`scripts/check-preview.mjs` 要能
 * 复现"端口被占"那一档。钉住的端口真被占着时 `startPreview` 会失败（规矩 2），
 * 这正是要演示的东西：它只会让自查**更早**失败，不会让它跑在别人的服务上。
 */
async function pickPort() {
  const pinned = process.env.VOX_PREVIEW_PORT;
  if (pinned === undefined || pinned === "") return freePort();
  const port = Number(pinned);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`VOX_PREVIEW_PORT 不是个端口：${pinned}`);
  }
  return port;
}

/** 子进程还活着？（两个字段都为空 = 没退出。`exitCode` 是规矩 5(a) 认的那个。） */
function childAlive(child) {
  return child.exitCode === null && child.signalCode === null;
}

/**
 * 同一性自检：`origin` 上答话的服务端，跑的必须就是**我们刚建出来的那份 `dist`**。
 *
 * 以前固定端口那套出过假绿（verifier 第七轮 F3）——端口被一个更旧的服务占着时，断言会在
 * **别人的构建**上全过。第四轮起 `check:capabilities` 单独带了这条自检，现在提到这里：
 * 五个脚本一个都不能漏。
 *
 * 入口脚本逐字节相同是最便宜的同一性证明：vite 的产物名带内容哈希
 * （`assets/index-<hash>.js`），内容一变名字就变，名字一样内容还逐字节相同，
 * 那就是同一份构建。
 *
 * 返回入口脚本的路径（`assets/index-XXXX.js`），顺带打一行给人看。
 *
 * @param {string} origin 预览服务的地址，如 `http://127.0.0.1:5184`。
 * @param {string} [cwd] `dist/` 所在的工作目录，默认 `app/ui`。
 */
export async function expectOwnBuild(origin, cwd = UI_DIR) {
  const dist = join(cwd, "dist");
  const html = readFileSync(join(dist, "index.html"), "utf8");
  const entry = /<script[^>]+src="\.?\/?(assets\/[^"]+\.js)"/.exec(html)?.[1];
  if (!entry) throw new Error("dist/index.html 里找不到入口脚本——先 npm run build。");

  const response = await fetch(`${origin}/${entry}`);
  if (!response.ok) throw new Error(`${origin}/${entry} 回 ${response.status}`);
  const served = await response.text();
  const built = readFileSync(join(dist, entry), "utf8");
  if (served !== built) {
    throw new Error(
      `${origin}/${entry} 的内容与 dist 里那份不一致——测试跑在别的（旧）服务上，结果不算数。`,
    );
  }
  console.log(`自检：预览服务的产物 = 刚建出来的 dist（${entry}，逐字节相同）。`);
}

/**
 * 起一个属于自己的预览服务。
 *
 * @param {string} label 出错信息里的名字（哪个自查脚本）。
 * @param {string} [cwd] `vite preview` 的工作目录，默认 `app/ui`。
 */
export async function startPreview(label, cwd = UI_DIR) {
  const port = await pickPort();
  if (await portOpen(port)) {
    throw new Error(`${label}：端口 ${port} 在起服务前就被占了（再跑一次）`);
  }

  const child = spawn(
    process.execPath,
    [VITE, "preview", "--port", String(port), "--strictPort", "--host", "127.0.0.1"],
    { cwd, stdio: ["ignore", "ignore", "pipe"] },
  );

  // 子进程的 stderr 留着报错用（上限 8 KiB，防着一路刷屏撑爆内存）。
  let log = "";
  child.stderr.on("data", (chunk) => {
    if (log.length < 8192) log += chunk.toString();
  });
  let exited = null;
  child.on("exit", (code, signal) => {
    exited = signal ?? code ?? 0;
  });

  // 60 × 250ms = 15s。子进程一退出就不再等——那是失败，不是"慢"。
  let up = false;
  for (let i = 0; i < 60 && exited === null; i += 1) {
    if (await portOpen(port)) {
      up = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  if (exited !== null) {
    throw new Error(`${label}：vite preview 没起来就退出了（${exited}）\n${log.trim()}`);
  }
  if (!up) {
    child.kill("SIGTERM");
    throw new Error(`${label}：vite preview 15 秒内没就绪\n${log.trim()}`);
  }

  const base = `http://127.0.0.1:${port}`;
  try {
    const root = await fetch(`${base}/`, { redirect: "manual" });
    if (!root.ok) {
      throw new Error(
        `${label}：${base}/ 回 ${root.status}（先 npm run build——preview 服务的是 dist）\n${log.trim()}`,
      );
    }

    // 规矩 5(a) 的第一眼：端口通了，但答话的必须还是我们那个子进程。
    expectAlive(label, child, log);

    // 规矩 5(b)：端上跑的确实是这份产物。
    await expectOwnBuild(base, cwd);

    // 规矩 5(a) 的第二眼：自检这一小会儿它也可能死了——那时上面那份"逐字节相同"
    // 可能是别人答的，不算数。收工前再确认一次。
    expectAlive(label, child, log);
  } catch (error) {
    child.kill("SIGTERM");
    throw error;
  }

  let stopped = false;
  return {
    port,
    base,
    stop() {
      if (stopped) return;
      stopped = true;
      if (childAlive(child)) child.kill("SIGTERM");
    },
  };
}

/** 就绪判定的一半（规矩 5(a)）：**我们的**子进程还活着吗？死了就不是"服务起来了"。 */
function expectAlive(label, child, log) {
  if (childAlive(child)) return;
  throw new Error(
    `${label}：vite preview 在就绪后立刻退出了（${child.signalCode ?? child.exitCode}）` +
      `——端口上答话的可能不是我们\n${log.trim()}`,
  );
}
