import { createServer } from "node:http";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connectAppPage, createReporter, launchApp, pageTools, waitFor } from "./lib/electron-harness.mjs";

/**
 * 「消息里的超链接点击 → 右栏展开顶级 Tab」的真实 Electron 验收。
 *
 * 这是本次改动的关键回归：创建入口原先挂在懒加载的右栏组件上，右栏未挂载
 * 时点击被静默丢弃。因此验收必须从**右栏从未挂载**的状态开始（全新状态根，
 * 且不打开 GPT Web 视图），再点击消息里的链接。
 *
 * 环境变量：MAGI_LINKS_APP_EXECUTABLE、MAGI_LINKS_CDP_PORT（9341）、
 * MAGI_LINKS_DAEMON_PORT（38951）、MAGI_LINKS_SCREENSHOTS。
 */

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
  check("打包 Electron 启动并渲染工作台", true);

  // 先装好并激活 GPT Web 插件（右栏此时仍未挂载：入口只是菜单项，应用级宿主要等用户
  // 点「新增 → GPT Web」才会创建），随后重载让渲染端投影出插件入口。
  const PLUGIN_ID = "openai.chatgpt-web";
  const installed = await tools.post(`/api/plugins/bundled/${PLUGIN_ID}/install`, {});
  check("第一方 GPT Web 插件安装成功", installed.status === 200, installed.body.slice(0, 200));
  const pluginManifest = JSON.parse(installed.body).plugins.find((item) => item.manifest.id === PLUGIN_ID).manifest;
  await tools.post(`/api/plugins/${PLUGIN_ID}/enable`, { scope: "application" });
  await tools.post(`/api/plugins/${PLUGIN_ID}/authorize`, { scope: "application", grants: pluginManifest.permissions.map((permission) => ({ ...permission })) });
  const activated = await tools.post(`/api/plugins/${PLUGIN_ID}/activate`, { scope: "application" });
  check("GPT Web 插件应用级激活成功", activated.status === 200, activated.body.slice(0, 200));

  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => tools.exists('[data-testid="input-textarea"]'), "重载后工作台", 30_000, 400);

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

  // 5. 浏览器 Tab 归属当前 Magi 会话，而不是 GPT Web 的应用级会话。
  const sessionId = await page.evaluate(`document.querySelector('[data-session-id]')?.getAttribute('data-session-id') || ''`);
  if (sessionId) {
    const query = new URLSearchParams({ scope: 'personal', sessionId }).toString();
    const current = await tools.api(`/api/browser/sessions/current?${query}`);
    const body = current.status === 200 ? JSON.parse(current.body) : null;
    const session = body?.session ?? null;
    check(
      "浏览器 Tab 归属当前 Magi 会话",
      Boolean(session) && !String(session.browserSessionId).startsWith("browser-session-app-") && (session.tabs?.length ?? 0) === 2,
      JSON.stringify({ status: current.status, browserSessionId: session?.browserSessionId, tabs: session?.tabs?.length, body: current.body.slice(0, 200) }),
    );
  } else {
    check("浏览器 Tab 归属当前 Magi 会话", false, "未取到当前会话 id");
  }

  // 6. GPT Web（应用级）视图处于激活态时点链接：新 Tab 必须成为内容槽当前项，
  //    而不是只进 Tab 条；GPT Web 宿主保持挂载（后台推理不中断）。
  // 打开 GPT Web 视图：沿用 verify-electron-gpt-web 已验证的真实入口（设置 → GPT Web → 连接）。
  await waitFor(async () => {
    const visible = await page.evaluate(`Boolean(document.querySelector('[data-testid="settings-page"]')?.getClientRects().length)`);
    if (!visible) await tools.click('[data-testid="sidebar-settings"]');
    await sleep(1200);
    await tools.click('[data-testid="settings-nav-webModel"]').catch(() => {});
    await sleep(600);
    return page.evaluate(`Boolean(document.getElementById('web-model-title'))`);
  }, "设置页出现独立的 GPT Web 分类", 30_000, 500);
  await page.evaluate(`document.querySelector('[data-web-model-connect]').click()`);
  await waitFor(() => tools.exists(".web-model-content webview"), "GPT Web webview 挂载", 45_000, 500);
  await tools.click('[data-testid="settings-back"]');
  // 连接只挂载宿主（标签默认隐藏）；与用户一致，经「新增 → GPT Web」把视图显示为当前 Tab。
  await waitFor(() => tools.exists(".right-pane-add-tab"), "右栏新增按钮可见", 30_000, 400);
  await page.evaluate(`document.querySelector('.right-pane-add-tab').click()`);
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('.right-pane-add-menu-item[data-kind="webSession"]:not([disabled])'))`), "新增菜单出现可用的 GPT Web 入口", 30_000, 400);
  await page.evaluate(`document.querySelector('.right-pane-add-menu-item[data-kind="webSession"]').click()`);
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('.right-pane-tabbar [data-tab-id="webSession:app"].active'))`), "GPT Web 视图成为当前 Tab", 45_000, 400);
  check("GPT Web 视图处于激活态", true);

  const activeTabId = () => page.evaluate(`document.querySelector('.right-pane-tabbar .right-pane-tab.active')?.getAttribute('data-tab-id') || ''`);
  await page.evaluate(`document.querySelectorAll('.md-link')[2].click()`);
  await waitFor(async () => (await browserTabIds()).length >= 3, "GPT Web 激活时点链接新建第三个 Tab", 60_000, 400);
  const activeAfterThird = await waitFor(async () => {
    const id = await activeTabId();
    return id.startsWith("browser:") ? id : null;
  }, "新建的浏览器 Tab 成为当前 Tab", 20_000, 300).catch(() => null);
  check("GPT Web 激活时点链接，新 Tab 成为当前 Tab（而不是留在 GPT Web）", Boolean(activeAfterThird), `active=${await activeTabId()}`);
  check("GPT Web 宿主仍保留在 Tab 条中（未被销毁）", await page.evaluate(`Boolean(document.querySelector('.right-pane-tabbar [data-tab-id="webSession:app"]'))`));

  check("验收期间 App Renderer 无运行时异常", rendererErrors.length === 0, rendererErrors.slice(0, 3).join(" | "));
} finally {
  page?.close();
  await app.stop();
  pageServer.close();
  provider.close();
  await rm(workRoot, { recursive: true, force: true });
}

process.stdout.write(`右栏链接验收：${summary()}\n`);
