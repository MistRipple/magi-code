import { createServer } from "node:http";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connectAppPage, createReporter, launchApp, pageTools, waitFor } from "./lib/electron-harness.mjs";



const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = process.env.MAGI_LINKS_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const cdpPort = Number.parseInt(process.env.MAGI_LINKS_CDP_PORT || "9341", 10);
const daemonPort = Number.parseInt(process.env.MAGI_LINKS_DAEMON_PORT || "38951", 10);

const MODEL_ID = "right-pane-links-model";
const REPLY_TOKEN = "RIGHT_PANE_LINKS_OK";

const { check, summary } = createReporter();
const rendererErrors = [];

// 目标页面：真实可加载的本地网页，用来确认右栏 Browser Tab 真的把页面加载起来。
const pageServer = createServer((_request, response) => {
  response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  response.end("<!doctype html><meta charset=\"utf-8\"><title>Right Pane Link Target</title><h1 id=\"target\">RIGHT_PANE_LINK_TARGET</h1>");
});
await new Promise((resolve) => pageServer.listen(0, "127.0.0.1", resolve));
const pagePort = pageServer.address().port;
const firstUrl = `http://127.0.0.1:${pagePort}/first`;
const secondUrl = `http://127.0.0.1:${pagePort}/second`;
const thirdUrl = `http://127.0.0.1:${pagePort}/third`;

// 本地 Provider：回复里带两个 http 链接，供点击验收使用。
const provider = createServer((request, response) => {
  if (request.url === "/v1/models" && request.method === "GET") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ object: "list", data: [{ id: MODEL_ID, object: "model", owned_by: "harness" }] }));
    return;
  }
  if (request.url !== "/v1/chat/completions" || request.method !== "POST") {
    response.writeHead(404).end();
    return;
  }
  request.resume();
  const content = [
    `${REPLY_TOKEN}`,
    "",
    `- [第一个链接](${firstUrl})`,
    `- [第二个链接](${secondUrl})`,
    `- [第三个链接](${thirdUrl})`,
  ].join("\n");
  response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  response.write(`data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`);
  response.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`);
  response.write("data: [DONE]\n\n");
  response.end();
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const providerPort = provider.address().port;

const workRoot = await mkdtemp(join(tmpdir(), "magi-electron-right-pane-links-"));
const stateRoot = join(workRoot, "state");
await mkdir(join(stateRoot, "desktop"), { recursive: true });

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_STATE_ROOT: stateRoot,
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "right-pane-links-key",
    MAGI_OPENAI_COMPAT_MODEL: MODEL_ID,
  },
});

let page = null;
let tools = null;
try {
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, daemonPort, rendererErrors).catch(() => null);
    if (!candidate) return null;
    const ready = await candidate.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`).catch(() => false);
    if (!ready) { candidate.close(); return null; }
    return candidate;
  }, "打包 Electron App Renderer 就绪", 60_000, 500);
  tools = pageTools(page);
  check("Electron 启动并渲染工作台", true);
  const retiredBundled = await tools.api('/api/plugins/bundled');
  check("MCP 服务不再是内置插件（内置插件目录入口已删除）", retiredBundled.status === 404 || retiredBundled.status === 405);
  await tools.click('[data-testid="sidebar-settings"]');
  await waitFor(() => tools.exists('[data-testid="settings-nav-mcpServer"]'), "MCP 设置入口可见");
  check("真实设置页保留 MCP 服务入口", true);
  await tools.click('[data-testid="settings-back"]');

  const rightPaneVisible = () => page.evaluate(
    `(() => { const el = document.querySelector('.desktop-right-pane-column'); if (!el) return false; return !el.hidden && el.getAttribute('aria-hidden') !== 'true' && el.getClientRects().length > 0; })()`,
  );
  const browserTabIds = () => page.evaluate(
    `[...document.querySelectorAll('.right-pane-tabbar [data-tab-id^="browser:"]')].map((el) => el.getAttribute('data-tab-id'))`,
  );
  const browserWebviewCount = () => page.evaluate(
    `document.querySelectorAll('.right-pane-browser-tab-host webview').length`,
  );

  // 前置：全新状态根下右栏从未挂载。这是本次修复针对的真实起点。
  await waitFor(async () => !(await rightPaneVisible()), "右栏保持未展开", 20_000, 300);
  check("全新状态下右栏未挂载（点击链接前）", !(await rightPaneVisible()));

  // 个人会话里发一条消息，让回复带上两个链接。
  // 发送按钮只在输入区有内容时渲染，因此必须先写入文本再等它出现。
  await waitFor(async () => page.evaluate(
    `(() => { const el = document.querySelector('[data-testid="input-textarea"]'); return Boolean(el) && el.getAttribute('contenteditable') === 'true'; })()`,
  ), "输入区可编辑", 45_000, 300);
  const prompt = "请只回复给定的两个链接。";
  await page.evaluate(`(() => {
    const element = document.querySelector('[data-testid="input-textarea"]');
    element.focus();
    element.textContent = ${JSON.stringify(prompt)};
    element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${JSON.stringify(prompt)} }));
  })()`);
  await waitFor(async () => page.evaluate(
    `(() => { const send = document.querySelector('[data-testid="input-send-button"]'); return Boolean(send) && !send.disabled; })()`,
  ), "发送按钮可用", 45_000, 300);
  await tools.click('[data-testid="input-send-button"]');
  await waitFor(async () => (await page.evaluate("document.body.innerText")).includes(REPLY_TOKEN), "助手回复带链接读回 Magi", 90_000, 500);
  check("助手回复（含两个超链接）读回 Magi", true);

  await waitFor(() => page.evaluate(`document.querySelectorAll('.md-link').length >= 3`), "消息渲染出三个超链接", 20_000, 300);
  // 右栏仍未挂载：链接点击必须由 Shell 处理，不能依赖右栏组件存在。
  check("点击前右栏仍保持未挂载", !(await rightPaneVisible()));

  // 1. 点击第一个链接：右栏自动展开并出现新的顶级浏览器 Tab。
  await page.evaluate(`document.querySelectorAll('.md-link')[0].click()`);
  await waitFor(async () => (await browserTabIds()).length >= 1, "右栏出现浏览器 Tab", 60_000, 400);
  await waitFor(rightPaneVisible, "右栏自动展开", 30_000, 300);
  check("点击链接后右栏自动展开并出现顶级浏览器 Tab", true);
  const firstTabs = await browserTabIds();
  check("浏览器 Tab 是右栏一级 Tab", firstTabs.length === 1, JSON.stringify(firstTabs));

  // 2. 目标页面真实加载进内容槽。
  await waitFor(async () => (await browserWebviewCount()) >= 1, "浏览器内容槽挂载 webview", 45_000, 400);
  const loadedTarget = await waitFor(async () => {
    const value = await page.evaluate(`(() => {
      const view = document.querySelector('.right-pane-browser-tab-host webview');
      if (!view) return null;
      try { return view.getURL?.() || ''; } catch { return null; }
    })()`);
    return typeof value === "string" && value.includes("/first") ? value : null;
  }, "webview 加载到目标地址", 60_000, 500).catch(() => null);
  check("浏览器 Tab 加载到链接目标地址", Boolean(loadedTarget), String(loadedTarget));

  // 3. 同一 URL 再点一次：复用同一个 Tab，不叠加新页面。
  await page.evaluate(`document.querySelectorAll('.md-link')[0].click()`);
  await sleep(2500);
  const afterRepeat = await browserTabIds();
  check("同一 URL 重复点击复用已有 Tab", afterRepeat.length === 1, JSON.stringify(afterRepeat));

  // 4. 不同 URL：新增第二个顶级 Tab。
  await page.evaluate(`document.querySelectorAll('.md-link')[1].click()`);
  await waitFor(async () => (await browserTabIds()).length >= 2, "第二个链接新建 Tab", 60_000, 400);
  const afterSecond = await browserTabIds();
  check("不同 URL 新建第二个顶级 Tab", afterSecond.length === 2, JSON.stringify(afterSecond));

  const sessionId = await page.evaluate(`new URL(location.href).searchParams.get('sessionId') || ''`);
  let browserSessionId = '';
  if (sessionId) {
    const query = new URLSearchParams({ scope: 'personal', sessionId }).toString();
    const current = await tools.api(`/api/browser/sessions/current?${query}`);
    const body = current.status === 200 ? JSON.parse(current.body) : null;
    const session = body?.session ?? null;
    browserSessionId = session?.browserSessionId || '';
    check(
      "浏览器 Tab 归属当前 Magi 会话",
      Boolean(session) && !String(session.browserSessionId).startsWith("browser-session-app-") && (session.tabs?.length ?? 0) === 2,
      JSON.stringify({ status: current.status, browserSessionId: session?.browserSessionId, tabs: session?.tabs?.length, body: current.body.slice(0, 200) }),
    );
  } else {
    check("浏览器 Tab 归属当前 Magi 会话", false, "未取到当前会话 id");
  }

  const activeAfterThird = await page.evaluate(`document.querySelector('.right-pane-tabbar .right-pane-tab.active')?.getAttribute('data-tab-id') || ''`);
  await tools.click(`[data-tab-id="${activeAfterThird}"] .right-pane-tab-close`);
  await waitFor(async () => !(await browserTabIds()).includes(activeAfterThird), "逻辑关闭事件移除真实右栏 Tab");
  check("关闭后的 Tab 从真实界面移除", true);
  const closeSession = await tools.post('/api/session/close', { sessionId });
  check("Magi 会话关闭完成", closeSession.status === 200, closeSession.body.slice(0, 300));
  const currentAfterClose = await tools.api('/api/browser/sessions/' + encodeURIComponent(browserSessionId));
  const closedSession = JSON.parse(currentAfterClose.body);
  check("会话关闭后 Browser 会话已关闭且没有活动 Tab", currentAfterClose.status === 200
    && closedSession.lifecycle === 'closed' && closedSession.activeTabId === null
    && closedSession.tabs.every(tab => tab.lifecycle === 'closed'), currentAfterClose.body.slice(0, 400));

  check("验收期间 App Renderer 无运行时异常", rendererErrors.length === 0, rendererErrors.slice(0, 3).join(" | "));
} finally {
  page?.close();
  await app.stop();
  pageServer.close();
  provider.close();
  await rm(workRoot, { recursive: true, force: true });
}

process.stdout.write(`右栏链接验收：${summary()}\n`);
