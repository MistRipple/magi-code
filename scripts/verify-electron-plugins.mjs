import { createServer } from "node:http";
import { execFile, spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join } from "node:path";
import { networkInterfaces, tmpdir } from "node:os";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import {
  connectAppPage,
  connectTarget,
  createReporter,
  launchApp,
  pageTools,
  waitFor,
} from "./lib/electron-harness.mjs";

/**
 * 打包 Electron 验收：工作台导航与 MCP 服务。
 *
 * 启动打包后的 Magi.app（独立状态根、独立端口、本地 OpenAI-compatible Provider），
 * 通过 App Renderer 的 CDP 驱动真实界面：工作区导航与 MCP 服务（令牌、授权、外部会话、审计）。
 * 不接管用户已有的 Electron/daemon 进程。插件（受信 Node 扩展）的发行包验收尚未接入此脚本。
 *
 * 环境变量：MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE（默认 target/electron-dist/mac-arm64/Magi.app/...）、
 * MAGI_ELECTRON_PLUGINS_CDP_PORT（9301）、MAGI_ELECTRON_PLUGINS_DAEMON_PORT（38931）。
 */

const execFileAsync = promisify(execFile);
const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = process.env.MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_CDP_PORT || "9301", 10);
const daemonPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_DAEMON_PORT || "38931", 10);

const { check, summary } = createReporter();

// ---- 本地 Provider：记录请求；遇到插件工具时发起一次真实工具调用 ----
function sse(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}
function toolCall(name, args) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: "plugin-call-1", type: "function", function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "tool_calls" }] })}\n\n`,
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
    const messages = parsed.messages ?? [];
    const lastUser = [...messages].reverse().find((message) => message.role === "user");
    const prompt = textOf(lastUser);
    // 发给模型的工具名经 bridge 编码，不再保留贡献 ID；按清单里的描述识别只读的“读取看板”。
    const summaryTool = (parsed.tools ?? []).find((tool) => tool.function?.description?.includes("返回看板当前的条目"))?.function?.name;
    const toolResult = messages.find((message) => message.role === "tool");
    response.writeHead(200, { "content-type": "text/event-stream" });
    if (prompt.includes("PLUGIN_TOOL_E2E") && summaryTool && !toolResult) {
      response.end(toolCall(summaryTool, {}));
    } else if (toolResult) {
      response.end(sse(`PLUGIN_TOOL_RESULT:${textOf(toolResult)}`));
    } else {
      response.end(sse("PLUGIN_E2E_OK"));
    }
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
const documentBodyIncludes = (value) => page.evaluate(`document.body.innerText.includes(${JSON.stringify(value)})`);

/** 设置页在显示时会重置/聚焦当前分类；重试点击直到目标分类真正成为当前页。 */
async function openSettingsTab(page, tabId) {
  const nav = `[data-testid="settings-nav-${tabId}"]`;
  await waitFor(async () => {
    // 设置视图创建后常驻 DOM、只切换显示，因此必须检查可见性而不是存在性。
    const visible = await page.evaluate(`(() => { const el = document.querySelector('[data-testid="settings-page"]'); return Boolean(el && el.getClientRects().length > 0); })()`);
    if (!visible) {
      await click(page, '[data-testid="sidebar-settings"]');
      // 刚打开时设置视图仍在初始化，分类选择会被随后的初始化覆盖；等其稳定后再选。
      await sleep(1500);
    }
    if (!(await exists(page, nav))) return false;
    await click(page, nav);
    await sleep(700);
    return (await page.evaluate(`document.querySelector(${JSON.stringify(nav)}).getAttribute('aria-current')`)) === "page";
  }, `设置分类 ${tabId} 成为当前页`, 15_000, 300);
}

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

  // 10d. MCP 服务是原生功能：不依赖任何插件，管理面始终可用，监听只由自己的开关控制
  const before = await api(page, "/api/mcp-server/status");
  check("MCP 服务管理面可用", before.status === 200, String(before.status));
  check("设置导航始终有 MCP 服务分类", await exists(page, '[data-testid="settings-nav-mcpServer"]'));
  const mcpEnabled = JSON.parse((await post(page, "/api/mcp-server/enabled", { enabled: true })).body);
  check("可开启服务并监听本机端口", mcpEnabled.running === true && /^http:\/\/127\.0\.0\.1:\d+/.test(mcpEnabled.url ?? ""), JSON.stringify(mcpEnabled).slice(0, 200));
  const reachable = async (url) => fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }).then((response) => response.status).catch(() => null);
  check("监听端口要求令牌（无令牌 401）", (await reachable(mcpEnabled.url)) === 401);
  // MCP 客户端设置：工具目录固定完整开放，只选择逐次询问或开放全部权限。
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => exists(page, '[data-testid="input-textarea"]'), "MCP 激活后工作台重新加载");
  await openSettingsTab(page, "mcpServer");
  await click(page, '[data-mcp-tab="clients"]');
  await waitFor(() => exists(page, '[data-mcp-create-open="1"]'), "MCP 客户端设置出现");
  await click(page, '[data-mcp-create-open="1"]');
  await waitFor(() => exists(page, '[data-mcp-approval-mode="1"]'), "MCP 客户端授权方式选择器出现");
  check("创建 MCP 时展示授权源目录与固定绑定说明", await text(page, '[data-mcp-bound-folders="new"]').then((value) => value.includes(workspaceRoot))
    && await documentBodyIncludes("项目新增目录不会扩展此连接的权限"));
  check("新令牌默认完整工具目录并逐次询问", await page.evaluate(`(() => ({
    hasProfile: Boolean(document.querySelector('[data-mcp-profile="1"]')),
    approval: document.querySelector('[data-mcp-approval-mode="1"] select')?.value,
    hasDeny: [...(document.querySelector('[data-mcp-approval-mode="1"] select')?.options ?? [])].some((option) => option.value === 'deny'),
  }))()`).then((state) => !state.hasProfile && state.approval === 'ask' && !state.hasDeny));
  await page.evaluate(`(() => {
    const select = document.querySelector('[data-mcp-approval-mode="1"] select');
    select.value = 'always';
    select.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  check("开放权限无需第二个风险确认，网络开关仍独立可选", !(await exists(page, '[data-mcp-confirm-risk="1"]')) && await exists(page, '[data-mcp-allow-network="1"]'));
  await page.evaluate(`(() => [...document.querySelectorAll('form.create-form')].at(-1)?.querySelector('button[type="button"]')?.click())()`);
  const restoredUrl = mcpEnabled.url;
  // 10e. 外部会话：外部 agent 通过 MCP 调用本地工具的过程，是对应工作区下的一个真正的（只读）会话，
  // 调用写进它的 canonical 时间线，在主对话区里和内部对话同一套展示
  await writeFile(join(workspaceRoot, "external-read.txt"), "hello from an external agent");
  const autoIssued = JSON.parse((await post(page, "/api/mcp-server/tokens", {
    clientName: "e2e-auto-agent",
    workspaceId,
    approvalMode: "always",
    allowNetwork: true,
    ttlDays: 1,
  })).body);
  check("开放权限网络令牌创建成功", Boolean(autoIssued.token?.tokenId && autoIssued.secret));
  const autoCall = (name, args_) => fetch(restoredUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args_ } }),
  }).then((response) => response.json());
  const autoWrite = await autoCall("magi.fs.write", { path: "auto-mode.txt", content: "auto-approved\n" });
  const pendingAfterAutoWrite = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
  check("始终授权令牌的普通写入无需 Magi 弹窗", autoWrite.result?.isError !== true && pendingAfterAutoWrite.length === 0, JSON.stringify(autoWrite).slice(0, 200));
  await writeFile(join(workspaceRoot, "auto-delete.txt"), "remove without prompt");
  const autoRemove = await autoCall("magi.fs.remove", { path: "auto-delete.txt" });
  const removedFileStillExists = await access(join(workspaceRoot, "auto-delete.txt")).then(() => true).catch(() => false);
  check("开放权限也自动执行删除类 MCP 工具", autoRemove.result?.isError !== true && !removedFileStillExists, JSON.stringify(autoRemove).slice(0, 200));
  const autoExec = await autoCall("magi.shell.exec", { command: "printf open-mode" });
  check("开放权限也自动执行命令工具", autoExec.result?.isError !== true && JSON.stringify(autoExec).includes("open-mode"), JSON.stringify(autoExec).slice(0, 200));
  // 同一令牌共享 Magi 会话，标准 HTTP 传输会话隔离相同请求 ID 和取消。
  const transportRpc = (session, method, params = {}, id = 1) => fetch(restoredUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}`,
      ...(session ? { "mcp-session-id": session } : {}) },
    body: JSON.stringify({ jsonrpc: "2.0", ...(id === null ? {} : { id }), method, params }),
  });
  const transportIds = [];
  for (const name of ["example-test", "claude-test"]) {
    const initialized = await transportRpc(null, "initialize", { clientInfo: { name, version: "1" } });
    transportIds.push(initialized.headers.get("mcp-session-id"));
  }
  check("同一令牌两次 HTTP 初始化签发不同传输会话", transportIds.every(Boolean) && transportIds[0] !== transportIds[1]);
  const beforeHttpDelete = JSON.parse((await api(page, "/api/mcp-server/status")).body).httpSessions;
  check("HTTP 统计来自真实传输会话", beforeHttpDelete >= 2);
  await openSettingsTab(page, "mcpServer");
  await click(page, '[data-mcp-tab="connect"]');
  await waitFor(async () => (await text(page, '[data-mcp-connections="1"]'))?.includes(`HTTP 传输会话：${beforeHttpDelete}`), "桌面设置展示 daemon 的连接统计");
  await click(page, '[data-testid="settings-back"]');
  await Promise.all([0, 1].map((index) => mkdir(join(workspaceRoot, `http-${index}`))));
  const runningShells = transportIds.map((session, index) => transportRpc(session, "tools/call", {
    name: "magi.shell.exec", arguments: { cwd: `http-${index}`, command: `printf started > started.txt; sleep 30; printf finished > finished.txt` },
  }).then((response) => response.json()).catch((error) => ({ transportError: String(error) })));
  await waitFor(async () => (await Promise.all([0, 1].map((index) => access(join(workspaceRoot, `http-${index}`, "started.txt")).then(() => true, () => false)))).every(Boolean), "相同请求 ID 的两个 HTTP Shell 均已执行", 15_000);
  const conflicting = await transportRpc(transportIds[1], "tools/call", { name: "magi.shell.exec", arguments: { cwd: "http-0", command: "printf conflict > conflict.txt" } }, 3).then((response) => response.json());
  check("共享工作区中的同目录并发写入仍明确拒绝", conflicting.result?.isError === true && JSON.stringify(conflicting).includes("write_conflict"));
  const cancelled = await transportRpc(transportIds[0], "notifications/cancelled", { requestId: 1 }, null);
  const firstCancelled = await runningShells[0];
  const secondRead = await transportRpc(transportIds[1], "tools/call", { name: "magi.fs.read", arguments: { path: "http-1/started.txt" } }, 2).then((response) => response.json());
  check("取消第一个 HTTP 会话不会中断第二个会话", cancelled.status === 202 && firstCancelled.result?.isError === true && secondRead.result?.isError === false);
  const terminated = await fetch(restoredUrl, { method: "DELETE", headers: { authorization: `Bearer ${autoIssued.secret}`, "mcp-session-id": transportIds[1] } });
  const secondCancelled = await runningShells[1];
  check("终止 HTTP 传输会话取消其 Shell 且不落地迟到副作用", terminated.status === 204 && secondCancelled.result?.isError === true
    && (await Promise.all([0, 1].map((index) => access(join(workspaceRoot, `http-${index}`, "finished.txt")).then(() => false, () => true)))).every(Boolean));
  check("已终止传输会话返回 404", (await transportRpc(transportIds[1], "tools/list")).status === 404);
  check("终止后 HTTP 统计减少一个", JSON.parse((await api(page, "/api/mcp-server/status")).body).httpSessions === beforeHttpDelete - 1);
  const sharedSessions = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions.filter((entry) => entry.tokenId === autoIssued.token.tokenId);
  check("多个 HTTP 传输会话仍合并到同一 Magi 外部会话", sharedSessions.length === 1 && sharedSessions[0].workspaceId === workspaceId);
  // 使用设置页实际生成的配置，验证发行包自己的 stdio 中继，避免维护另一套命令与权限合同。
  const snippets = JSON.parse((await api(page, `/api/mcp-server/config-snippets?tokenId=${autoIssued.token.tokenId}`)).body);
  check("客户端配置服务名包含工作区和客户端", snippets.serverName === "magi-workspace-e2e-auto-agent");
  const relayConfig = Object.values(snippets.stdioJson.mcpServers)[0];
  const relay = spawn(relayConfig.command, relayConfig.args, {
    env: { ...process.env, ...relayConfig.env }, stdio: ["pipe", "pipe", "pipe"],
  });
  const replies = new Map();
  const lines = createInterface({ input: relay.stdout });
  let relayError = null;
  relay.on("error", (error) => { relayError = error; });
  relay.stderr.on("data", () => {});
  lines.on("line", (line) => { const reply = JSON.parse(line); replies.set(reply.id, reply); });
  let relayId = 0;
  const send = async (method, params) => {
    const id = ++relayId;
    relay.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return waitFor(() => {
      if (relayError) throw relayError;
      if (relay.exitCode !== null && !replies.has(id)) throw new Error("stdio 中继提前退出");
      return replies.get(id);
    }, `stdio ${method}`, 10_000);
  };
  try {
    const initialized = await send("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "plugin-e2e-stdio", version: "1" } });
    check("生成的客户端配置可通过发行包 stdio 中继握手", initialized.result?.serverInfo?.name === "magi");
    check("已认证 stdio 连接计入本地统计", JSON.parse((await api(page, "/api/mcp-server/status")).body).localConnections === 1);
    check("stdio 握手告知真实绑定工作区及相对路径基准", initialized.result?.instructions?.includes(workspaceRoot)
      && initialized.result.instructions.includes('"name":"workspace"')
      && initialized.result.instructions.includes("Resolve relative paths against this workspace root"));
    const listed = await send("tools/list", {});
    const names = (listed.result?.tools ?? []).map((tool) => tool.name);
    check("stdio 目录统一包含读取、写入、删除和命令", ["magi.fs.read", "magi.fs.write", "magi.fs.remove", "magi.shell.exec"].every((name) => names.includes(name)));
    const read = await send("tools/call", { name: "magi.fs.read", arguments: { path: "external-read.txt" } });
    check("stdio 读取真实工作区内容", read.result?.isError === false && JSON.stringify(read).includes("hello from an external agent"));
    const write = await send("tools/call", { name: "magi.fs.write", arguments: { path: "stdio-write.txt", content: "via packaged stdio" } });
    check("stdio 开放权限写入实际文件", write.result?.isError === false && (await readFile(join(workspaceRoot, "stdio-write.txt"), "utf8")) === "via packaged stdio");
    const escape = await send("tools/call", { name: "magi.fs.read", arguments: { path: "../outside.txt" } });
    check("stdio 越出工作区拒绝", escape.result?.isError === true);
    const cancelId = relayId + 1;
    const pendingShell = send("tools/call", { name: "magi.shell.exec", arguments: {
      command: "printf started > stdio-cancel-started.txt; sleep 30; printf finished > stdio-cancel-finished.txt",
    } });
    await waitFor(() => access(join(workspaceRoot, "stdio-cancel-started.txt")).then(() => true).catch(() => false), "stdio 命令实际开始");
    const concurrentRead = await send("tools/call", { name: "magi.fs.read", arguments: { path: "external-read.txt" } });
    relay.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: cancelId } })}\n`);
    const cancelledShell = await pendingShell;
    check("stdio 取消通知终止指定命令并保留另一条读取结果", cancelledShell.result?.isError === true
      && JSON.stringify(cancelledShell).includes("cancelled") && concurrentRead.result?.isError === false
      && !(await access(join(workspaceRoot, "stdio-cancel-finished.txt")).then(() => true).catch(() => false)), JSON.stringify(cancelledShell).slice(0, 250));
  } finally {
    lines.close();
    relay.stdin.end();
    relay.kill();
  }
  await waitFor(async () => JSON.parse((await api(page, "/api/mcp-server/status")).body).localConnections === 0, "stdio 断开后统计归零");
  const invalidRelay = await execFileAsync(relayConfig.command, relayConfig.args, {
    env: { ...process.env, ...relayConfig.env, MAGI_MCP_TOKEN: "magi_mcp_wrong" }, timeout: 5_000,
  }).then((output) => output, (error) => error);
  check("stdio 错误令牌被拒绝并断开", /unauthorized/u.test(invalidRelay.stdout ?? ""));
  const issued = JSON.parse((await post(page, "/api/mcp-server/tokens", { clientName: "e2e-agent", workspaceId, ttlDays: 1 })).body);
  const issuedInitialization = await fetch(restoredUrl, {
    method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${issued.secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 0, method: "initialize" }),
  });
  const issuedSessionId = issuedInitialization.headers.get("mcp-session-id");
  check("询问模式客户端初始化 HTTP 传输会话", Boolean(issuedSessionId));
  const mcpCall = (name, args_) => fetch(restoredUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${issued.secret}`, "mcp-session-id": issuedSessionId },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args_ } }),
  }).then((response) => response.json());
  const externalReadPending = mcpCall("magi.fs.read", { path: "external-read.txt" });
  const readApproval = await waitFor(async () => {
    const approvals = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下读取操作也请求确认");
  const readApprovalResult = await post(page, "/api/mcp-server/approvals/resolve", {
    approvalId: readApproval.approvalId,
    decision: "allow_once",
  });
  check("可单次允许 MCP 读取操作", readApprovalResult.status === 200, readApprovalResult.body.slice(0, 200));
  const externalRead = await externalReadPending;
  check("外部 agent 经 MCP 读取本地文件成功", externalRead.result?.isError !== true && JSON.stringify(externalRead).includes("hello from an external agent"), JSON.stringify(externalRead).slice(0, 200));
  const externalDenied = await mcpCall("magi.fs.read", { path: "../escape.txt" });
  check("越出工作区的调用被拒绝", externalDenied.result?.isError === true);
  const sessionsList = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions;
  const externalSession = sessionsList.find((entry) => entry.tokenId === issued.token.tokenId);
  check("daemon 为外部客户端建立了外部会话，归属令牌绑定的工作区", externalSession?.workspaceId === workspaceId && externalSession.sessionId.startsWith("external-tool-"), JSON.stringify(sessionsList).slice(0, 200));
  // 外部会话由 daemon 的 mcp.activity 事件驱动出现在侧栏，不依赖窗口焦点或插件注册表刷新。
  const sessionItem = `.session-item[data-session-id="${externalSession.sessionId}"]`;
  await page.evaluate(`(() => { const header = document.querySelector('[data-workspace-id="${workspaceId}"]'); if (header && header.getAttribute('aria-expanded') !== 'true') header.click(); })()`);
  await waitFor(() => exists(page, sessionItem), "工作区下出现该令牌的外部客户端会话", 30_000);
  check("外部会话进入工作区的统一会话列表", await exists(page, sessionItem));
  check("外部会话按最近活动时间排在普通会话中", await page.evaluate(`(() => {
    const item = document.querySelector('[data-testid="external-session-item"]');
    const list = item?.closest('.session-list');
    const times = list ? [...list.querySelectorAll('.session-item')].map((entry) => Number(entry.dataset.sessionUpdatedAt ?? 0)) : [];
    return Boolean(item && list && !item.closest('.external-list') && times.every((time, index) => index === 0 || times[index - 1] >= time));
  })()`));
  check("侧栏的外部会话以客户端名称标识并标注「外部」", await page.evaluate(`(() => { const item = document.querySelector('[data-testid="external-session-item"]'); return Boolean(item?.closest('.workspace-children')) && item.innerText.includes('e2e-agent') && item.innerText.includes('外部'); })()`));
  if (await exists(page, '[data-testid="settings-back"]')) await click(page, '[data-testid="settings-back"]').catch(() => {});
  // 停在「变更」标签时点外部会话，也要回到「对话」标签看到它。
  await page.evaluate(`document.querySelectorAll('.tab-bar--top .tab-item')[1]?.click()`);
  await sleep(300);
  await click(page, sessionItem);
  await waitFor(() => exists(page, '[data-testid="external-session-bar"]'), "打开外部会话后输入区换成只读提示", 30_000);
  check("点外部会话后顶部标签回到「对话」", await page.evaluate(`document.querySelector('.tab-bar--top .tab-item')?.classList.contains('active') === true`));
  check("外部会话在主对话区打开，没有对话输入框", !(await exists(page, '[data-testid="input-textarea"]')));
  check("打开外部会话后仍保留在统一会话列表", await exists(page, `.session-item[data-session-id="${externalSession.sessionId}"]`));
  for (const [locale, expected] of [
    ["en-US", "External calls from e2e-agent"],
    ["zh-CN", "来自 e2e-agent 的外部调用"],
  ]) {
    await post(page, "/api/settings/update", { key: "locale", value: locale });
    await page.call("Page.reload", { ignoreCache: true });
    await waitFor(() => exists(page, '[data-testid="external-session-bar"]'), "重载后恢复外部会话", 30_000);
    for (const mode of ["original", "summary"]) {
      await post(page, "/api/settings/update", { key: "conversationDisplayMode", value: mode });
      await page.call("Page.reload", { ignoreCache: true });
      await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes(expected), `${locale}/${mode} 外部活动提示本地化`, 30_000);
      check(`${locale}/${mode} 历史外部调用按界面语言显示`, true);
    }
  }
  await post(page, "/api/settings/update", { key: "conversationDisplayMode", value: "original" });
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => page.evaluate(`Boolean([...document.querySelectorAll('.tool-call')].find(card => card.textContent.includes('external-read.txt')))`), "时间线里出现外部调用的工具卡片", 30_000);
  check("外部调用在时间线里按工具卡片展示（读取的文件名）", true);
  // 写入需要在 Magi 中确认：确认出现在会话自己的授权托盘里（和内部对话同一个组件），允许一次后写入成功。
  // 窗口不在前台时，新的待确认发系统通知（验收里用桩捕获 Notification 调用）。
  await page.evaluate(`window.__notes = []; document.hasFocus = () => false; window.Notification = class { static permission = 'granted'; constructor(title, options) { window.__notes.push({ title, body: options?.body ?? '' }); } close() {} };`);
  const writing = mcpCall("magi.fs.write", { path: "ext-write.txt", content: "written by external\n" });
  writing.catch(() => {});
  await waitFor(() => exists(page, '[data-approval-id]'), "外部写入的待确认出现在会话的授权托盘里", 30_000);
  check("等待确认的写入出现在外部会话自己的授权托盘里", true);
  const notes = await waitFor(async () => { const list = JSON.parse(await page.evaluate(`JSON.stringify(window.__notes)`)); return list.length > 0 ? list : null; }, "窗口不在前台时发出系统通知", 20_000);
  check("待确认在窗口不在前台时发系统通知（带客户端名称与操作摘要）", notes[0].body.includes("e2e-agent") && notes[0].body.includes("ext-write.txt"), JSON.stringify(notes).slice(0, 200));
  await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少单次授权按钮'); button.click(); })()`);
  const written = await writing;
  check("在会话里批准后外部写入成功", written.result?.isError !== true, JSON.stringify(written).slice(0, 200));
  await waitFor(() => page.evaluate(`Boolean([...document.querySelectorAll('.tool-call')].find(card => card.textContent.includes('ext-write.txt')))`), "写入展示为带文件名的卡片", 30_000);
  check("外部写入在会话里展示为带文件名的卡片", true);
  const writingAgain = mcpCall("magi.fs.write", { path: "ext-write-again.txt", content: "written by external after another prompt\n" });
  const secondApprovalAppeared = await waitFor(() => exists(page, '[data-approval-id]'), "下一次写入再次出现单次确认", 30_000).then(() => true).catch(() => false);
  if (secondApprovalAppeared) {
    await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少第二次单次授权按钮'); button.click(); })()`);
  }
  const writtenAgain = await writingAgain;
  check("每次询问模式对下一次写入再次确认", secondApprovalAppeared && writtenAgain.result?.isError !== true, JSON.stringify(writtenAgain).slice(0, 200));
  const deniedWritePending = mcpCall("magi.fs.write", { path: "denied-write.txt", content: "must not be written" });
  const deniedApproval = await waitFor(async () => JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals.find((approval) => approval.tokenId === issued.token.tokenId), "拒绝写入的待确认");
  await post(page, "/api/mcp-server/approvals/resolve", { approvalId: deniedApproval.approvalId, decision: "deny" });
  const deniedWrite = await deniedWritePending;
  check("拒绝当前 MCP 写入不产生文件", deniedWrite.result?.isError === true && !(await access(join(workspaceRoot, "denied-write.txt")).then(() => true).catch(() => false)));
  const cancelledWritePending = mcpCall("magi.fs.write", { path: "cancelled-write.txt", content: "must not be written" });
  await waitFor(() => page.evaluate(`[...document.querySelectorAll('[data-approval-id]')].some((tray) => tray.innerText.includes('cancelled-write.txt'))`), "取消前出现对应写入的授权托盘");
  const cancelledNotification = await fetch(restoredUrl, {
    method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${issued.secret}`, "mcp-session-id": issuedSessionId },
    body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: 1 } }),
  });
  const cancelledWrite = await cancelledWritePending;
  check("HTTP 取消待审批调用不产生文件", cancelledNotification.status === 202 && cancelledWrite.result?.isError === true
    && JSON.stringify(cancelledWrite).includes("cancelled")
    && !(await access(join(workspaceRoot, "cancelled-write.txt")).then(() => true).catch(() => false)));
  await waitFor(async () => !(await exists(page, '[data-approval-id]')), "取消后授权托盘消失");
  const cancelledCard = await waitFor(() => page.evaluate(`(() => { const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('cancelled-write.txt') && card.querySelector('.status-cancelled')); return card ? card.innerText + ' ' + card.querySelector('.status-cancelled').getAttribute('aria-label') : ''; })()`), "取消写入的工具卡片显示已取消");
  check("取消后的界面没有遗留审批且工具卡片标记已取消", /已取消|Cancelled/iu.test(cancelledCard), cancelledCard.slice(0, 200));
  const changes = await autoCall("magi.changes.list", {});
  check("MCP 变更账本包含真实写入", changes.result?.isError === false && JSON.stringify(changes).includes("auto-mode.txt"));
  const audit = await api(page, "/api/mcp-server/audit?limit=20");
  check("MCP 审计保留记录且不回显令牌", JSON.parse(audit.body).entries.length > 0 && !audit.body.includes(issued.secret) && !audit.body.includes(autoIssued.secret));
  // 不保存调用内容：读到的内容不落库，动作与路径保留。
  const retention = await fetch(`http://127.0.0.1:${daemonPort}/api/mcp-server/external`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ saveContent: false, retentionDays: 30 }) });
  check("可关闭保存调用内容", retention.status === 200);
  await writeFile(join(workspaceRoot, "private.txt"), "very private body");
  const privateReadPending = mcpCall("magi.fs.read", { path: "private.txt" });
  const privateReadApproval = await waitFor(async () => {
    const approvals = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下的隐私读取也请求确认");
  check("每次询问模式下的隐私读取也请求确认", true);
  await post(page, "/api/mcp-server/approvals/resolve", {
    approvalId: privateReadApproval.approvalId,
    decision: "allow_once",
  });
  const privateRead = await privateReadPending;
  check("关闭保存后外部调用本身仍然成功", privateRead.result?.isError !== true);
  const turnsText = JSON.stringify(JSON.parse((await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body));
  check("关闭保存后读到的内容不落库、动作与路径保留", !turnsText.includes("very private body") && turnsText.includes("private.txt"), turnsText.slice(0, 200));
  await waitFor(async () => {
    const history = JSON.parse((await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body);
    return history.canonicalTurns.at(-1)?.status === 'completed';
  }, "外部调用空闲后 daemon 收口活动段", 45_000);
  await waitFor(() => page.evaluate(`!document.querySelector('.runtime-diagnostics-slot')?.innerText.includes('运行中')
    && !document.querySelector('.turn-runtime-indicator')`), "外部活动段完成后界面不再显示运行中");
  check("外部活动段自动收口且实时清理运行状态，无需刷新", true);
  await fetch(`http://127.0.0.1:${daemonPort}/api/mcp-server/external`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ saveContent: true, retentionDays: 30 }) });

  const revokedShellPending = autoCall("magi.shell.exec", { command: "printf started > revoke-shell-started.txt; sleep 2; printf finished > revoke-shell-finished.txt" });
  await waitFor(() => access(join(workspaceRoot, "revoke-shell-started.txt")).then(() => true).catch(() => false), "吊销前命令实际开始");
  await api(page, `/api/mcp-server/tokens/${autoIssued.token.tokenId}`, { method: "DELETE" });
  const revokedShell = await revokedShellPending;
  check("吊销令牌停止在途命令且不产生后续写入", revokedShell.result?.isError === true
    && JSON.parse(revokedShell.result.content[0].text).cancelled === true
    && !(await access(join(workspaceRoot, "revoke-shell-finished.txt")).then(() => true).catch(() => false)), JSON.stringify(revokedShell).slice(0, 200));
  const revoked = await fetch(restoredUrl, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}` }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping", params: {} }) });
  check("吊销客户端令牌后 HTTP 立即拒绝", revoked.status === 401);
  const revokedSession = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions.find((session) => session.tokenId === autoIssued.token.tokenId);
  await click(page, `.session-item[data-session-id="${revokedSession.sessionId}"]`);
  await waitFor(() => page.evaluate(`(() => {
    const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('revoke-shell-started.txt'));
    return Boolean(card?.querySelector('.status-cancelled'));
  })()`), "吊销命令的会话卡片显示已取消");
  check("吊销后会话卡片显示实际取消状态", true);

  // 外部工具结果已交付后，canonical 活动段仍需由 daemon 的空闲结算完成。
  // 项目移除守卫在此期间按合同返回 409；等待原活动段收口后再移除。
  const removedWorkspace = await waitFor(async () => {
    const response = await post(page, "/api/workspaces/remove", { workspaceId });
    if (response.status === 409) return null;
    if (response.status !== 200) throw new Error(`移除项目失败：${response.status} ${response.body}`);
    return response;
  }, "外部活动段结算后移除项目", 45_000, 1000);
  check("项目移除事务成功", JSON.parse(removedWorkspace.body).removed === true);
  const removedHistory = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions;
  check("移除工作区后保留历史并标记不可用", removedHistory.some((session) => session.sessionId === revokedSession.sessionId && session.workspaceAvailable === false));
  await click(page, '[data-testid="sidebar-refresh"]');
  await waitFor(() => exists(page, `[data-testid="unavailable-external-session"][data-session-id="${revokedSession.sessionId}"]`), "工作区移除后侧栏保留不可用历史提示");
  check("不可用历史没有绕过工作区校验的会话按钮", !(await exists(page, `[data-testid="unavailable-external-session"][data-session-id="${revokedSession.sessionId}"] button`)));
  await openSettingsTab(page, "mcpServer");
  await click(page, '[data-mcp-tab="clients"]');
  await click(page, '[data-mcp-toggle-inactive="1"]');
  await waitFor(async () => (await text(page, `[data-mcp-token="${issued.token.tokenId}"]`))?.includes("已吊销"), "令牌展示 daemon 的吊销状态");
  const restoredWorkspace = JSON.parse((await post(page, "/api/workspaces/register", { path: workspaceRoot })).body);
  check("重新添加原目录复用工作区身份", restoredWorkspace.workspaceId === workspaceId);
  const restoredHistory = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions;
  check("原工作区恢复后历史重新可用", restoredHistory.some((session) => session.sessionId === revokedSession.sessionId && session.workspaceAvailable === true));
  check("恢复工作区不复活已吊销令牌", JSON.parse((await api(page, "/api/mcp-server/status")).body).tokens.find((token) => token.tokenId === issued.token.tokenId)?.state === "revoked");
  await post(page, "/api/mcp-server/enabled", { enabled: false });
  const stoppedStatus = JSON.parse((await api(page, "/api/mcp-server/status")).body);
  check("服务关闭后两种连接统计归零", stoppedStatus.httpSessions === 0 && stoppedStatus.localConnections === 0);

  // 11. Renderer 无未处理异常
  const logText = electronLog.join("");
  check("移除工作区后 sidecar 维护没有归属错误", !/刷新 sidecar 失败[^\n]*未注册 workspace/u.test(logText));
  check("历史快照与实时事件无 canonical 事实冲突", !rendererErrors.some((error) => error.includes('[canonical-turn-store]')), rendererErrors.slice(-4).join('\n'));
  check("Electron 日志没有 Desktop IPC 协议拒绝", !/IPC.*(validation|拒绝)/iu.test(logText));
  check("插件 iframe 没有 sandbox 初始化异常或串入应用消息协议", !/sandboxed_renderer.bundle.js script failed|binding.startupData|未知消息类型: request/u.test([...rendererErrors, logText].join("\n")));
  // 最后验证非回环网卡直连，环境不可达时仍保留前面的 stdio、审批与吊销证据。
  const lanIp = Object.values(networkInterfaces()).flat().find((entry) => entry?.family === "IPv4" && !entry.internal)?.address;
  if (!lanIp) throw new Error("缺少可用于网络直连验收的非回环 IPv4 网卡");
  await post(page, "/api/mcp-server/enabled", { enabled: true });
  const networkIssued = JSON.parse((await post(page, "/api/mcp-server/tokens", { clientName: "lan-network", workspaceId, approvalMode: "always", allowNetwork: true, ttlDays: 1 })).body);
  const direct = await api(page, "/api/mcp-server/direct", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled: true, bindHost: "0.0.0.0", publicHosts: [lanIp], confirmRisk: true }) });
  check("无需 tunnel 即可监听真实网卡地址", direct.status === 200, direct.body.slice(0, 150));
  const lanUrl = JSON.parse(direct.body).direct.mcpUrl;
  const localOnly = JSON.parse((await post(page, "/api/mcp-server/tokens", { clientName: "lan-local-only", workspaceId, ttlDays: 1 })).body);
  await openSettingsTab(page, "mcpServer");
  await click(page, '[data-mcp-tab="remote"]');
  await waitFor(() => documentBodyIncludes("网络直连（内网／公网）"), "桌面端展示网络直连文案");
  check("网络设置明确区分直连与公网隧道", await documentBodyIncludes("公网隧道") && await documentBodyIncludes("客户端访问地址"));
  const lanList = (secret) => fetch(lanUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    signal: AbortSignal.timeout(10_000),
  }).catch((cause) => ({ status: null, error: `网络直连验收无法连接 ${lanUrl}：${cause.message}` }));
  const [networkReply, localReply] = await Promise.all([lanList(networkIssued.secret), lanList(localOnly.secret)]);
  check("内网直连接受网络令牌并拒绝本机令牌", networkReply.status === 200 && localReply.status === 401,
    JSON.stringify({ network: networkReply.status, local: localReply.status, error: networkReply.error ?? localReply.error }));
  await api(page, "/api/mcp-server/direct", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled: false }) });
  await click(page, '[data-testid="settings-back"]');
  await post(page, "/api/mcp-server/enabled", { enabled: false });
} catch (error) {
  failure = error;
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1500)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n`);
    process.stderr.write(`\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-8).join("\n---\n")}\n`);
    process.stderr.write(`\n[Provider 请求]\n${providerRequests.map((request, index) => `#${index} tools=${(request.tools ?? []).map((tool) => tool.function?.name).filter((name) => name?.includes("board")).join(",")} last=${textOf((request.messages ?? []).at(-1)).slice(0, 160).replace(/\n/g, " ")}`).join("\n")}\n`);
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
