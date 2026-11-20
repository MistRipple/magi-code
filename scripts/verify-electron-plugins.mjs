import { createServer } from "node:http";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  connectAppPage,
  createReporter,
  launchApp,
  pageTools,
  waitFor,
} from "./lib/electron-harness.mjs";

/**
 * 打包 Electron 工作区导航验收，使用独立状态根、端口与本地 Provider。
 *
 * 环境变量：MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE（默认 target/electron-dist/mac-arm64/Magi.app/...）、
 * MAGI_ELECTRON_PLUGINS_CDP_PORT（9301）、MAGI_ELECTRON_PLUGINS_DAEMON_PORT（38931）。
 */

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = process.env.MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_CDP_PORT || "9301", 10);
const daemonPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_DAEMON_PORT || "38931", 10);

const { check, summary } = createReporter();

// ---- 本地 Provider：为工作区导航创建有内容的普通会话 ----
function sse(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}
function textOf(message) {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((part) => part?.text ?? "").join(" ");
  return "";
}

const providerRequests = [];
const rendererErrors = [];
const provider = createServer((request, response) => {
  let body = "";
  request.on("data", (chunk) => { body += chunk; });
  request.on("end", () => {
    if (request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "plugin-e2e-model", object: "model" }] }));
      return;
    }
    if (request.url !== "/v1/chat/completions") return void response.writeHead(404).end();
    const parsed = JSON.parse(body);
    providerRequests.push(parsed);
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(sse("PLUGIN_E2E_OK"));
  });
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const providerPort = provider.address().port;

let tools = null;
const api = (_page, path, init) => tools.api(path, init);
const post = (_page, path, body) => tools.post(path, body);
const click = (_page, selector) => tools.click(selector);
const exists = (_page, selector) => tools.exists(selector);
const text = (_page, selector) => tools.text(selector);

// ---- 启动 ----
const workRoot = await mkdtemp(join(tmpdir(), "magi-electron-plugins-"));
const stateRoot = join(workRoot, "state");
const workspaceRoot = join(workRoot, "workspace");
await mkdir(workspaceRoot, { recursive: true });
await writeFile(join(workspaceRoot, "README.md"), "plugin e2e\n");

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_STATE_ROOT: stateRoot,
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "plugin-e2e-key",
    MAGI_OPENAI_COMPAT_MODEL: "plugin-e2e-model",
  },
});
const electronLog = app.log;

let page = null;
let failure = null;
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

  // 1. 工作区
  const registered = await post(page, "/api/workspaces/register", { path: workspaceRoot });
  const workspaceId = JSON.parse(registered.body).workspaceId;
  check("工作区注册成功", Boolean(workspaceId), registered.body);
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => exists(page, '[data-testid="input-textarea"]'), "重载后工作台就绪");
  await waitFor(() => exists(page, `[data-workspace-id="${workspaceId}"]`), "工作区进入侧栏");
  await page.evaluate(`(() => {
    const row = document.querySelector('[data-workspace-id="${workspaceId}"]');
    row.click();
    const create = row.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('缺少新建会话按钮');
    create.click();
  })()`);
  await waitFor(async () => exists(page, '[data-testid="input-textarea"]'), "工作区会话输入区");

  // 工作区标题只展开/收起；普通会话、草稿和工作区绑定只由明确导航改变。
  const otherRoot = join(workRoot, "other-workspace");
  await mkdir(otherRoot);
  const otherWorkspace = JSON.parse((await post(page, "/api/workspaces/register", { path: otherRoot })).body).workspaceId;
  const sidebarSessions = [];
  for (const [id, name] of [[workspaceId, "侧栏会话 A"], [otherWorkspace, "侧栏会话 B"]]) {
    const created = JSON.parse((await post(page, "/api/session/materialize", { scope: "workspace", workspaceId: id })).body);
    check("为工作区导航验收建立真实会话", Boolean(created.sessionId));
    // 目录按产品规则隐藏空白历史，因此先通过真实 Turn 写入可展示的会话内容。
    const turn = await post(page, "/api/session/turn", { scope: "workspace", workspaceId: id, sessionId: created.sessionId, text: `记录${name}`, requestId: `sidebar-${id}` });
    check("导航验收会话写入真实对话内容", turn.status === 200, turn.body.slice(0, 180));
    await waitFor(async () => (await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(id)}&sessionId=${encodeURIComponent(created.sessionId)}`)).body.includes("PLUGIN_E2E_OK"), "导航验收会话回复完成", 30_000);
    await post(page, "/api/session/rename", { sessionId: created.sessionId, workspaceId: id, name });
    sidebarSessions.push(created.sessionId);
  }
  const navigationUrl = new URL(await page.evaluate("location.href"));
  navigationUrl.searchParams.set("scope", "workspace");
  navigationUrl.searchParams.set("workspaceId", workspaceId);
  navigationUrl.searchParams.set("workspacePath", workspaceRoot);
  navigationUrl.searchParams.set("sessionId", sidebarSessions[0]);
  await page.call("Page.navigate", { url: navigationUrl.href });
  const activeSession = () => page.evaluate("document.querySelector('.session-item.active')?.dataset.sessionId ?? ''");
  await waitFor(async () => (await activeSession()) === sidebarSessions[0], "当前会话 A 已恢复");
  const otherHeader = `[data-workspace-id="${otherWorkspace}"]`;
  if (await page.evaluate(`document.querySelector(${JSON.stringify(otherHeader)}).getAttribute('aria-expanded') === 'true'`)) await click(page, otherHeader);
  await click(page, otherHeader);
  await waitFor(() => exists(page, `.session-item[data-session-id="${sidebarSessions[1]}"]`), "点击另一工作区后加载其会话列表");
  check("首次展开另一工作区不自动打开第一条会话", (await activeSession()) === sidebarSessions[0] && (await page.evaluate("new URL(location.href).searchParams.get('sessionId')")) === sidebarSessions[0]);
  await click(page, otherHeader);
  await click(page, otherHeader);
  check("收起和再次展开已加载工作区仍保留当前会话", (await activeSession()) === sidebarSessions[0]);
  await click(page, `.session-item[data-session-id="${sidebarSessions[1]}"]`);
  await waitFor(async () => (await activeSession()) === sidebarSessions[1], "明确点击会话 B 后切换成功");
  check("只有点击具体会话才切换到对应工作区会话", (await page.evaluate("new URL(location.href).searchParams.get('workspaceId')")) === otherWorkspace);
  await click(page, `[data-workspace-id="${workspaceId}"]`);
  check("点击原工作区标题不会从会话 B 跳回会话 A", (await activeSession()) === sidebarSessions[1]);
  await page.evaluate(`document.querySelector('[data-workspace-id="${workspaceId}"]').closest('.workspace-row').querySelector('button.row-action').click()`);
  await waitFor(async () => !(await activeSession()) && (await page.evaluate("new URL(location.href).searchParams.get('workspaceId')")) === workspaceId, "新建按钮进入工作区 A 的草稿");
  await page.evaluate(`(() => { const input = document.querySelector('[data-testid="input-textarea"]'); input.textContent = '未发送的工作区草稿'; input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: '未发送的工作区草稿' })); })()`);
  await click(page, otherHeader);
  check("浏览其他工作区也保留当前草稿和输入区", !(await activeSession()) && (await page.evaluate("new URL(location.href).searchParams.get('workspaceId')")) === workspaceId && (await text(page, '[data-testid="input-textarea"]')).includes("未发送的工作区草稿"));
  await page.evaluate(`(() => { const input = document.querySelector('[data-testid="input-textarea"]'); input.textContent = ''; input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward', data: null })); })()`);

  const logText = electronLog.join("");
  check("历史快照与实时事件无 canonical 事实冲突", !rendererErrors.some((error) => error.includes('[canonical-turn-store]')), rendererErrors.slice(-4).join('\n'));
  check("Electron 日志没有 Desktop IPC 协议拒绝", !/IPC.*(validation|拒绝)/iu.test(logText));
  check("插件 iframe 没有 sandbox 初始化异常或串入应用消息协议", !/sandboxed_renderer.bundle.js script failed|binding.startupData|未知消息类型: request/u.test([...rendererErrors, logText].join("\n")));
} catch (error) {
  failure = error;
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1500)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n`);
    process.stderr.write(`\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-8).join("\n---\n")}\n`);
    process.stderr.write(`\n[Provider 请求]\n${providerRequests.map((request, index) => `#${index} last=${textOf((request.messages ?? []).at(-1)).slice(0, 160).replace(/\n/g, " ")}`).join("\n")}\n`);
    const nav = await page.evaluate(`[...document.querySelectorAll('[data-testid^="settings-nav-"]')].map((el) => el.dataset.testid + '=' + el.getAttribute('aria-current')).join(', ')`).catch(() => "");
    process.stderr.write(`\n[设置导航]\n${nav}\n`);
  }
  process.stderr.write(`\n[Electron 日志尾部]\n${electronLog.join("").slice(-2500)}\n`);
} finally {
  if (failure || process.env.MAGI_ELECTRON_PLUGINS_DIAGNOSTICS_DIR) {
    const diagnostics = process.env.MAGI_ELECTRON_PLUGINS_DIAGNOSTICS_DIR || workRoot;
    await mkdir(diagnostics, { recursive: true });
    await writeFile(join(diagnostics, "diagnostics.json"), JSON.stringify({ failure: failure?.message, rendererErrors }, null, 2));
    await writeFile(join(diagnostics, "electron.log"), electronLog.join(""));
    if (page) {
      const screenshot = await page.call("Page.captureScreenshot", { format: "png" }).catch(() => null);
      if (screenshot) await writeFile(join(diagnostics, "screen.png"), Buffer.from(screenshot.data, "base64"));
    }
    process.stdout.write(`验收现场已保存: ${diagnostics}\n`);
  }
  page?.close();
  await app.stop();
  provider.closeAllConnections?.();
  provider.close();
  if (!failure) await rm(workRoot, { recursive: true, force: true }).catch(() => {});
}

process.stdout.write(`\n${summary()}\n`);
if (failure) {
  process.stderr.write(`${failure.message}\n`);
  process.exit(1);
}
