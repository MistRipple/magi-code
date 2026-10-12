import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import WebSocket from "ws";

/**
 * 打包 Electron 验收脚本共用的最小驱动：断言记录、轮询等待、CDP 连接和应用启动。
 * 只做传输与进程管理，不包含任何产品断言，也不接管用户已有的 Electron/daemon 进程。
 */

export function createReporter() {
  const results = [];
  return {
    results,
    check(name, ok, detail = "") {
      results.push({ name, ok: Boolean(ok), detail });
      process.stdout.write(`${ok ? "✓" : "✗"} ${name}${ok || !detail ? "" : `\n    ${detail}`}\n`);
      if (!ok) throw new Error(`验收失败：${name}${detail ? `（${detail}）` : ""}`);
    },
    summary() {
      return `${results.filter((item) => item.ok).length}/${results.length} 项通过`;
    },
  };
}

export async function waitFor(probe, label, timeoutMs = 20_000, intervalMs = 150) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(intervalMs);
  }
  throw new Error(`等待超时：${label}${lastError ? `；最后错误：${lastError.message}` : ""}`);
}

/** 按目标过滤器连接一个 CDP 目标（页面、OOPIF、webview）。`errors` 收集该目标的运行时异常。 */
export async function connectTarget(cdpPort, matches, label, errors = []) {
  const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
  const target = targets.find(matches);
  if (!target?.webSocketDebuggerUrl) throw new Error(`未找到 ${label}`);
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let nextId = 1;
  socket.on("message", (raw) => {
    const message = JSON.parse(raw.toString());
    if (message.method === "Runtime.exceptionThrown") {
      errors.push(message.params.exceptionDetails?.exception?.description || message.params.exceptionDetails?.text || "exception");
    } else if (message.method === "Runtime.consoleAPICalled" && message.params.type === "error") {
      errors.push(message.params.args.map((arg) => arg.value ?? arg.description ?? "").join(" "));
    }
    const entry = pending.get(message.id);
    if (!entry) return;
    pending.delete(message.id);
    clearTimeout(entry.timer);
    if (message.error) entry.reject(new Error(message.error.message || "CDP 失败")); else entry.resolve(message.result);
  });
  await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => { if (pending.delete(id)) reject(new Error(`CDP 超时：${method}`)); }, 20_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  await call("Runtime.enable");
  await call("Page.enable").catch(() => {});
  const evaluate = async (expression) => {
    const result = await call("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result?.exceptionDetails) {
      throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text || "evaluate 失败");
    }
    return result?.result?.value;
  };
  return { call, evaluate, close: () => socket.close() };
}

export function connectAppPage(cdpPort, daemonPort, errors) {
  return connectTarget(
    cdpPort,
    (item) => item.type === "page" && item.url.includes(`http://127.0.0.1:${daemonPort}/web.html`),
    "App Renderer",
    errors,
  );
}

/** 页面内便捷操作，selector 与表达式都经 JSON 转义，避免拼接注入。 */
export function pageTools(page) {
  const exists = (selector) => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  return {
    exists,
    text: (selector) => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.innerText ?? null`),
    click: (selector) => page.evaluate(`(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) throw new Error('缺少元素 ' + ${JSON.stringify(selector)}); el.click(); return true; })()`),
    api: (path, init) => page.evaluate(`fetch(${JSON.stringify(path)}, ${JSON.stringify(init ?? {})}).then(async (r) => ({ status: r.status, body: await r.text() }))`),
    post: (path, body) => page.evaluate(`fetch(${JSON.stringify(path)}, ${JSON.stringify({ method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })}).then(async (r) => ({ status: r.status, body: await r.text() }))`),
    screenshot: async () => (await page.call("Page.captureScreenshot", { format: "png" })).data,
  };
}

/** 启动打包应用；返回进程、日志缓冲和关闭函数。 */
export function launchApp({ executable, cwd, cdpPort, env, disableGpu = true }) {
  const log = [];
  const child = spawn(executable, [`--remote-debugging-port=${cdpPort}`, ...(disableGpu ? ["--disable-gpu"] : [])], {
    cwd,
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (chunk) => log.push(chunk.toString()));
  child.stderr.on("data", (chunk) => log.push(chunk.toString()));
  return {
    child,
    log,
    async stop() {
      child.kill("SIGTERM");
      await Promise.race([new Promise((resolve) => child.once("exit", resolve)), sleep(15_000)]);
      if (child.exitCode === null) child.kill("SIGKILL");
    },
  };
}
