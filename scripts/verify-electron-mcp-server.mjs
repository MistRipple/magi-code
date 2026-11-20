// 打包 Electron 的原生 MCP 服务验收，使用独立目录与端口。
import { createServer } from "node:http";
import { execFile, spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { dirname, join } from "node:path";
import { networkInterfaces, tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { connectAppPage, createReporter, launchApp, pageTools, waitFor } from "./lib/electron-harness.mjs";

const execFileAsync = promisify(execFile);
const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const appExecutable = process.env.MAGI_ELECTRON_MCP_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const cdpPort = Number(process.env.MAGI_ELECTRON_MCP_CDP_PORT || 9324);
const daemonPort = Number(process.env.MAGI_ELECTRON_MCP_DAEMON_PORT || 38954);
const { check, summary, results } = createReporter();
const rendererErrors = [];

async function openSettingsTab(tab) {
  const nav = '[data-testid="settings-nav-mcpServer"]';
  const selected = `[data-mcp-tab="${tab}"]`;
  await waitFor(async () => {
    const visible = await page.evaluate(`Boolean(document.querySelector('[data-testid="settings-page"]')?.getClientRects().length)`);
    if (!visible) {
      await click('[data-testid="sidebar-settings"]');
      return false;
    }
    if (!await exists(nav + '[aria-current="page"]')) {
      await click(nav);
      return false;
    }
    if (!await exists(selected)) return false;
    if (!await exists(selected + '[aria-selected="true"]')) await click(selected);
    return exists(selected + '[aria-selected="true"]');
  }, `MCP 设置切到 ${tab}`);
}

async function setMcpEnabled(enabled) {
  const selector = '[data-mcp-server-enabled="1"]';
  await waitFor(() => exists(selector + ':enabled'), "MCP 服务开关可操作");
  if (await exists(selector + ':checked') !== enabled) await click(selector);
  return waitFor(async () => {
    const current = await status();
    return current.enabled === enabled && current.running === enabled && current;
  }, `daemon 提交 MCP 服务开关 ${enabled}`);
}

const workRoot = await mkdtemp(join(tmpdir(), "magi-electron-mcp-"));
const workspaceRoot = join(workRoot, "workspace");
await mkdir(workspaceRoot);

// 仅用来填充同一工作区中的普通会话，MCP 工具调用走真实外部客户端链路。
const provider = createServer((request, response) => {
  if (request.url === "/v1/models") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ object: "list", data: [{ id: "mcp-e2e-model", object: "model" }] }));
  } else if (request.url === "/v1/chat/completions") {
    request.resume();
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end('data: {"choices":[{"delta":{"content":"MCP_E2E_OK"},"finish_reason":null}]}\n\ndata: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
  } else {
    response.writeHead(404).end();
  }
});
await new Promise(resolve => provider.listen(0, "127.0.0.1", resolve));

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_DESKTOP_REUSE_DAEMON: "",
    MAGI_HOST: "127.0.0.1",
    MAGI_STATE_ROOT: join(workRoot, "state"),
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "mcp-e2e-key",
    MAGI_OPENAI_COMPAT_MODEL: "mcp-e2e-model",
  },
});
const electronLog = app.log;
let page, api, post, click, exists, text, failure;
const status = async () => JSON.parse((await api("/api/mcp-server/status")).body);
const documentBodyIncludes = (value) => page.evaluate(`document.body.innerText.includes(${JSON.stringify(value)})`);

try {
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, daemonPort, rendererErrors);
    if (await pageTools(candidate).exists('[data-testid="input-textarea"]').catch(() => false)) return candidate;
    candidate.close();
    return null;
  }, "打包 Electron 工作台就绪", 60_000);
  ({ api, post, click, exists, text } = pageTools(page));
  const registered = await post("/api/workspaces/register", { path: workspaceRoot });
  const workspaceId = JSON.parse(registered.body).workspaceId;
  check("MCP 验收工作区注册成功", registered.status === 200 && Boolean(workspaceId), registered.body);
  const { sessionId: ordinarySession } = JSON.parse((await post("/api/session/materialize", { scope: "workspace", workspaceId })).body);
  const ordinaryTurn = await post("/api/session/turn", { scope: "workspace", workspaceId, sessionId: ordinarySession,
    text: "记录普通会话", requestId: "mcp-workspace-session" });
  check("普通会话接纳了真实 Turn", ordinaryTurn.status === 200, ordinaryTurn.body);
  await waitFor(async () => (await api(`/api/messages?scope=workspace&workspaceId=${workspaceId}&sessionId=${ordinarySession}`)).body.includes("MCP_E2E_OK"), "普通会话完成模型回复");
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => exists(`[data-workspace-id="${workspaceId}"]`), "真实工作区出现在侧栏");

  // MCP 服务是原生功能：不依赖任何插件，管理面始终可用，监听只由自己的开关控制
  const before = await api("/api/mcp-server/status");
  check("MCP 服务管理面可用", before.status === 200, String(before.status));
  await openSettingsTab("clients");
  check("设置导航始终有 MCP 服务分类", await exists('[data-testid="settings-nav-mcpServer"]'));
  check("MCP 设置提供客户端、网络访问与活动标签", await page.evaluate(`[...document.querySelectorAll('[data-mcp-tabs] [role="tab"]')].map(tab => tab.dataset.mcpTab).join(',')`) === "clients,remote,activity");
  const mcpEnabled = await setMcpEnabled(true);
  check("可开启服务并监听本机端口", mcpEnabled.running === true && /^http:\/\/127\.0\.0\.1:\d+/.test(mcpEnabled.url ?? ""), JSON.stringify(mcpEnabled).slice(0, 200));
  const reachable = async (url) => fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }).then((response) => response.status).catch(() => null);
  check("监听端口要求令牌（无令牌 401）", (await reachable(mcpEnabled.url)) === 401);
  // MCP 客户端设置：工具目录固定完整开放，只选择逐次询问或开放全部权限。
  await waitFor(() => exists('[data-mcp-create-open="1"]'), "MCP 客户端设置出现");
  await click('[data-mcp-create-open="1"]');
  await waitFor(() => exists('[data-mcp-approval-mode="1"]'), "MCP 客户端授权方式选择器出现");
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
  check("开放权限无需第二个风险确认，网络开关仍独立可选", !(await exists('[data-mcp-confirm-risk="1"]')) && await exists('[data-mcp-allow-network="1"]'));
  await page.evaluate(`(() => [...document.querySelectorAll('form.create-form')].at(-1)?.querySelector('button[type="button"]')?.click())()`);
  const restoredUrl = mcpEnabled.url;
  // 外部会话：外部 agent 通过 MCP 调用本地工具的过程，是对应工作区下的一个真正的（只读）会话，
  // 调用写进它的 canonical 时间线，在主对话区里和内部对话同一套展示
  await writeFile(join(workspaceRoot, "external-read.txt"), "hello from an external agent");
  const autoIssued = JSON.parse((await post("/api/mcp-server/tokens", {
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
  const pendingAfterAutoWrite = JSON.parse((await api("/api/mcp-server/approvals")).body).approvals ?? [];
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
  const beforeHttpDelete = JSON.parse((await api("/api/mcp-server/status")).body).httpSessions;
  check("HTTP 统计来自真实传输会话", beforeHttpDelete >= 2);
  await openSettingsTab("clients");
  await click('[data-mcp-local-endpoints="1"]:not([open]) summary');
  const connectionCount = '[data-mcp-local-endpoints="1"][open] [data-mcp-connections="1"]';
  await waitFor(async () => (await text(connectionCount))?.includes(`HTTP 传输会话：${beforeHttpDelete}`), "桌面设置展示 daemon 的连接统计");
  check("客户端页展开后显示与 daemon 一致的 HTTP 连接数", true);
  await click('[data-testid="settings-back"]');
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
  check("终止后 HTTP 统计减少一个", JSON.parse((await api("/api/mcp-server/status")).body).httpSessions === beforeHttpDelete - 1);
  const sharedSessions = JSON.parse((await api("/api/mcp-server/sessions")).body).sessions.filter((entry) => entry.tokenId === autoIssued.token.tokenId);
  check("多个 HTTP 传输会话仍合并到同一 Magi 外部会话", sharedSessions.length === 1 && sharedSessions[0].workspaceId === workspaceId);
  // 使用设置页实际生成的配置，验证发行包自己的 stdio 中继，避免维护另一套命令与权限合同。
  const snippets = JSON.parse((await api(`/api/mcp-server/config-snippets?tokenId=${autoIssued.token.tokenId}`)).body);
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
    check("已认证 stdio 连接计入本地统计", JSON.parse((await api("/api/mcp-server/status")).body).localConnections === 1);
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
  await waitFor(async () => JSON.parse((await api("/api/mcp-server/status")).body).localConnections === 0, "stdio 断开后统计归零");
  const invalidRelay = await execFileAsync(relayConfig.command, relayConfig.args, {
    env: { ...process.env, ...relayConfig.env, MAGI_MCP_TOKEN: "magi_mcp_wrong" }, timeout: 5_000,
  }).then((output) => output, (error) => error);
  check("stdio 错误令牌被拒绝并断开", /unauthorized/u.test(invalidRelay.stdout ?? ""));
  const issued = JSON.parse((await post("/api/mcp-server/tokens", { clientName: "e2e-agent", workspaceId, ttlDays: 1 })).body);
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
    const approvals = JSON.parse((await api("/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下读取操作也请求确认");
  const readApprovalResult = await post("/api/mcp-server/approvals/resolve", {
    approvalId: readApproval.approvalId,
    decision: "allow_once",
  });
  check("可单次允许 MCP 读取操作", readApprovalResult.status === 200, readApprovalResult.body.slice(0, 200));
  const externalRead = await externalReadPending;
  check("外部 agent 经 MCP 读取本地文件成功", externalRead.result?.isError !== true && JSON.stringify(externalRead).includes("hello from an external agent"), JSON.stringify(externalRead).slice(0, 200));
  const externalDenied = await mcpCall("magi.fs.read", { path: "../escape.txt" });
  check("越出工作区的调用被拒绝", externalDenied.result?.isError === true);
  const sessionsList = JSON.parse((await api("/api/mcp-server/sessions")).body).sessions;
  const externalSession = sessionsList.find((entry) => entry.tokenId === issued.token.tokenId);
  check("daemon 为外部客户端建立了外部会话，归属令牌绑定的工作区", externalSession?.workspaceId === workspaceId && externalSession.sessionId.startsWith("external-tool-"), JSON.stringify(sessionsList).slice(0, 200));
  // 外部会话由 daemon 的 mcp.activity 事件驱动出现在侧栏，不依赖窗口焦点或插件注册表刷新。
  const sessionItem = `.session-item[data-session-id="${externalSession.sessionId}"]`;
  await page.evaluate(`(() => { const header = document.querySelector('[data-workspace-id="${workspaceId}"]'); if (header && header.getAttribute('aria-expanded') !== 'true') header.click(); })()`);
  await waitFor(() => exists(sessionItem), "工作区下出现该令牌的外部客户端会话", 30_000);
  check("外部会话进入工作区的统一会话列表", await exists(sessionItem));
  const externalItem = JSON.stringify(sessionItem);
  check("外部会话按最近活动时间排在普通会话中", await page.evaluate(`(() => {
    const item = document.querySelector(${externalItem});
    const list = item?.closest('.session-list');
    const times = list ? [...list.querySelectorAll('.session-item')].map((entry) => Number(entry.dataset.sessionUpdatedAt ?? 0)) : [];
    return Boolean(item && list?.querySelector('.session-item[data-session-id="${ordinarySession}"]') && !item.closest('.external-list') && times.every((time, index) => index === 0 || times[index - 1] >= time));
  })()`));
  check("侧栏的外部会话以客户端名称标识并标注「外部」", await page.evaluate(`(() => { const item = document.querySelector(${externalItem}); return Boolean(item?.closest('.workspace-children')) && item.innerText.includes('e2e-agent') && item.innerText.includes('外部'); })()`));
  if (await exists('[data-testid="settings-back"]')) await click('[data-testid="settings-back"]').catch(() => {});
  // 停在「变更」标签时点外部会话，也要回到「对话」标签看到它。
  await page.evaluate(`document.querySelectorAll('.tab-bar--top .tab-item')[1]?.click()`);
  await click(sessionItem);
  await waitFor(() => exists('[data-testid="external-session-bar"]'), "打开外部会话后输入区换成只读提示", 30_000);
  check("点外部会话后顶部标签回到「对话」", await page.evaluate(`document.querySelector('.tab-bar--top .tab-item')?.classList.contains('active') === true`));
  check("外部会话在主对话区打开，没有对话输入框", !(await exists('[data-testid="input-textarea"]')));
  check("打开外部会话后仍保留在统一会话列表", await exists(`.session-item[data-session-id="${externalSession.sessionId}"]`));
  for (const [locale, expected] of [
    ["en-US", "From e2e-agent · Read-only execution log"],
    ["zh-CN", "由 e2e-agent 发起 · 只读执行记录"],
  ]) {
    await post("/api/settings/update", { key: "locale", value: locale });
    for (const mode of ["original", "summary"]) {
      await post("/api/settings/update", { key: "conversationDisplayMode", value: mode });
      await page.call("Page.reload", { ignoreCache: true });
      await waitFor(async () => (await text('[data-testid="external-session-bar"]'))?.includes(expected), `${locale}/${mode} 只读来源提示本地化`, 30_000);
      check(`${locale}/${mode} 外部会话来源按界面语言显示且保持只读`, !(await exists('[data-testid="input-textarea"]')));
    }
  }
  await post("/api/settings/update", { key: "conversationDisplayMode", value: "original" });
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => page.evaluate(`Boolean([...document.querySelectorAll('.tool-call')].find(card => card.textContent.includes('external-read.txt')))`), "时间线里出现外部调用的工具卡片", 30_000);
  check("外部调用在时间线里按工具卡片展示（读取的文件名）", true);
  // 写入需要在 Magi 中确认：确认出现在会话自己的授权托盘里（和内部对话同一个组件），允许一次后写入成功。
  // 窗口不在前台时，新的待确认发系统通知（验收里用桩捕获 Notification 调用）。
  await page.evaluate(`window.__notes = []; document.hasFocus = () => false; window.Notification = class { static permission = 'granted'; constructor(title, options) { window.__notes.push({ title, body: options?.body ?? '' }); } close() {} };`);
  const writing = mcpCall("magi.fs.write", { path: "ext-write.txt", content: "written by external\n" });
  writing.catch(() => {});
  await waitFor(() => exists('[data-approval-id]'), "外部写入的待确认出现在会话的授权托盘里", 30_000);
  check("等待确认的写入出现在外部会话自己的授权托盘里", true);
  const notes = await waitFor(async () => { const list = JSON.parse(await page.evaluate(`JSON.stringify(window.__notes)`)); return list.length > 0 ? list : null; }, "窗口不在前台时发出系统通知", 20_000);
  check("待确认在窗口不在前台时发系统通知（带客户端名称与操作摘要）", notes[0].body.includes("e2e-agent") && notes[0].body.includes("ext-write.txt"), JSON.stringify(notes).slice(0, 200));
  await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少单次授权按钮'); button.click(); })()`);
  const written = await writing;
  check("在会话里批准后外部写入成功", written.result?.isError !== true, JSON.stringify(written).slice(0, 200));
  await waitFor(() => page.evaluate(`Boolean([...document.querySelectorAll('.tool-call')].find(card => card.textContent.includes('ext-write.txt')))`), "写入展示为带文件名的卡片", 30_000);
  check("外部写入在会话里展示为带文件名的卡片", true);
  const writingAgain = mcpCall("magi.fs.write", { path: "ext-write-again.txt", content: "written by external after another prompt\n" });
  const secondApprovalAppeared = await waitFor(() => exists('[data-approval-id]'), "下一次写入再次出现单次确认", 30_000).then(() => true).catch(() => false);
  if (secondApprovalAppeared) {
    await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少第二次单次授权按钮'); button.click(); })()`);
  }
  const writtenAgain = await writingAgain;
  check("每次询问模式对下一次写入再次确认", secondApprovalAppeared && writtenAgain.result?.isError !== true, JSON.stringify(writtenAgain).slice(0, 200));
  const deniedWritePending = mcpCall("magi.fs.write", { path: "denied-write.txt", content: "must not be written" });
  const deniedApproval = await waitFor(async () => JSON.parse((await api("/api/mcp-server/approvals")).body).approvals.find((approval) => approval.tokenId === issued.token.tokenId), "拒绝写入的待确认");
  await post("/api/mcp-server/approvals/resolve", { approvalId: deniedApproval.approvalId, decision: "deny" });
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
  await waitFor(async () => !(await exists('[data-approval-id]')), "取消后授权托盘消失");
  const cancelledCard = await waitFor(() => page.evaluate(`(() => { const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('cancelled-write.txt') && card.querySelector('.status-cancelled')); return card ? card.innerText + ' ' + card.querySelector('.status-cancelled').getAttribute('aria-label') : ''; })()`), "取消写入的工具卡片显示已取消");
  check("取消后的界面没有遗留审批且工具卡片标记已取消", /已取消|Cancelled/iu.test(cancelledCard), cancelledCard.slice(0, 200));
  const changes = await autoCall("magi.changes.list", {});
  check("MCP 变更账本包含真实写入", changes.result?.isError === false && JSON.stringify(changes).includes("auto-mode.txt"));
  const audit = await api("/api/mcp-server/audit?limit=20");
  check("MCP 审计保留记录且不回显令牌", JSON.parse(audit.body).entries.length > 0 && !audit.body.includes(issued.secret) && !audit.body.includes(autoIssued.secret));
  // 不保存调用内容：读到的内容不落库，动作与路径保留。
  await openSettingsTab("activity");
  await waitFor(async () => (await text('[data-mcp-audit-total="1"]'))?.match(/[1-9]\d*/), "活动页显示真实审计记录");
  check("活动页显示审计与保存策略", await exists('[data-mcp-server-audit="1"]') && await exists('[data-mcp-external-retention="1"]'));
  await waitFor(() => exists('[data-mcp-external-save-content="1"]:checked:enabled'), "活动页内容留存开关就绪");
  await click('[data-mcp-external-save-content="1"]');
  await waitFor(async () => (await status()).external.saveContent === false, "活动页关闭保存调用内容");
  check("可关闭保存调用内容", await exists('[data-mcp-external-save-content="1"]:not(:checked)'));
  await click('[data-testid="settings-back"]');
  await writeFile(join(workspaceRoot, "private.txt"), "very private body");
  const privateReadPending = mcpCall("magi.fs.read", { path: "private.txt" });
  const privateReadApproval = await waitFor(async () => {
    const approvals = JSON.parse((await api("/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下的隐私读取也请求确认");
  check("每次询问模式下的隐私读取也请求确认", true);
  await post("/api/mcp-server/approvals/resolve", {
    approvalId: privateReadApproval.approvalId,
    decision: "allow_once",
  });
  const privateRead = await privateReadPending;
  check("关闭保存后外部调用本身仍然成功", privateRead.result?.isError !== true);
  const turnsText = JSON.stringify(JSON.parse((await api(`/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body));
  check("关闭保存后读到的内容不落库、动作与路径保留", !turnsText.includes("very private body") && turnsText.includes("private.txt"), turnsText.slice(0, 200));
  await waitFor(async () => {
    const history = JSON.parse((await api(`/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body);
    return history.canonicalTurns.at(-1)?.status === 'completed';
  }, "外部调用空闲后 daemon 收口活动段", 45_000);
  await waitFor(() => page.evaluate(`!document.querySelector('.turn-runtime-indicator')`), "外部活动段完成后界面不再显示运行中");
  check("外部活动段自动收口且实时清理运行状态，无需刷新", true);

  const revokedShellPending = autoCall("magi.shell.exec", { command: "printf started > revoke-shell-started.txt; sleep 2; printf finished > revoke-shell-finished.txt" });
  await waitFor(() => access(join(workspaceRoot, "revoke-shell-started.txt")).then(() => true).catch(() => false), "吊销前命令实际开始");
  await api(`/api/mcp-server/tokens/${autoIssued.token.tokenId}`, { method: "DELETE" });
  const revokedShell = await revokedShellPending;
  check("吊销令牌停止在途命令且不产生后续写入", revokedShell.result?.isError === true
    && JSON.parse(revokedShell.result.content[0].text).cancelled === true
    && !(await access(join(workspaceRoot, "revoke-shell-finished.txt")).then(() => true).catch(() => false)), JSON.stringify(revokedShell).slice(0, 200));
  const revoked = await fetch(restoredUrl, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}` }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping", params: {} }) });
  check("吊销客户端令牌后 HTTP 立即拒绝", revoked.status === 401);
  const revokedSession = JSON.parse((await api("/api/mcp-server/sessions")).body).sessions.find((session) => session.tokenId === autoIssued.token.tokenId);
  const revokedSessionItem = `.session-item[data-session-id="${revokedSession.sessionId}"]`;
  await waitFor(() => exists(revokedSessionItem), "吊销后保留原外部会话入口");
  await click(revokedSessionItem);
  await waitFor(() => page.evaluate(`(() => {
    const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('revoke-shell-started.txt'));
    return Boolean(card?.querySelector('.status-cancelled'));
  })()`), "吊销命令的会话卡片显示已取消");
  check("吊销后会话卡片显示实际取消状态", true);

  // 外部工具结果已交付后，canonical 活动段仍需由 daemon 的空闲结算完成。
  await waitFor(async () => {
    const response = await api(`/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(revokedSession.sessionId)}`);
    return JSON.parse(response.body).canonicalTurns?.at(-1)?.status === "completed";
  }, "吊销后的外部活动段结算", 45_000);
  await click('[data-testid="sidebar-new-session"]');
  await waitFor(() => exists('[data-testid="input-textarea"]'), "退出外部会话后可打开新会话");
  const removedWorkspace = await post("/api/workspaces/remove", { workspaceId });
  check("项目移除事务成功", removedWorkspace.status === 200 && JSON.parse(removedWorkspace.body).removed === true, removedWorkspace.body);
  const removedHistory = JSON.parse((await api("/api/mcp-server/sessions")).body).sessions;
  check("移除工作区后保留历史并标记不可用", removedHistory.some((session) => session.sessionId === revokedSession.sessionId && session.workspaceAvailable === false));
  await click('[data-testid="sidebar-refresh"]');
  await waitFor(() => exists(`[data-testid="unavailable-external-session"][data-session-id="${revokedSession.sessionId}"]`), "工作区移除后侧栏保留不可用历史提示");
  check("不可用历史没有绕过工作区校验的会话按钮", !(await exists(`[data-testid="unavailable-external-session"][data-session-id="${revokedSession.sessionId}"] button`)));
  await openSettingsTab("clients");
  const revokedToken = `[data-mcp-token="${autoIssued.token.tokenId}"]`;
  await waitFor(() => exists('[data-mcp-toggle-inactive="1"]'), "客户端页可展开失效令牌");
  if (!await exists(revokedToken)) await click('[data-mcp-toggle-inactive="1"]');
  await waitFor(async () => (await text(revokedToken))?.includes("已吊销"), "令牌展示 daemon 的吊销状态");
  check("客户端页显示原令牌已吊销", true);
  const restoredWorkspace = JSON.parse((await post("/api/workspaces/register", { path: workspaceRoot })).body);
  check("重新添加原目录复用工作区身份", restoredWorkspace.workspaceId === workspaceId);
  const restoredHistory = JSON.parse((await api("/api/mcp-server/sessions")).body).sessions;
  check("原工作区恢复后历史重新可用", restoredHistory.some((session) => session.sessionId === revokedSession.sessionId && session.workspaceAvailable === true));
  check("恢复工作区不复活已吊销令牌", (await status()).tokens.find((token) => token.tokenId === autoIssued.token.tokenId)?.state === "revoked");
  const stoppedStatus = await setMcpEnabled(false);
  check("服务关闭后两种连接统计归零", stoppedStatus.httpSessions === 0 && stoppedStatus.localConnections === 0);

  // 最后验证非回环网卡直连，环境不可达时仍保留前面的 stdio、审批与吊销证据。
  const lanIp = Object.values(networkInterfaces()).flat().find((entry) => entry?.family === "IPv4" && !entry.internal)?.address;
  if (!lanIp) throw new Error("缺少可用于网络直连验收的非回环 IPv4 网卡");
  await setMcpEnabled(true);
  const networkIssued = JSON.parse((await post("/api/mcp-server/tokens", { clientName: "lan-network", workspaceId, allowNetwork: true, ttlDays: 1 })).body);
  const localOnly = JSON.parse((await post("/api/mcp-server/tokens", { clientName: "lan-local-only", workspaceId, ttlDays: 1 })).body);
  await openSettingsTab("remote");
  await click('[data-mcp-server-direct="1"] details:not([open]) summary');
  check("网络设置明确区分直连与公网隧道", await documentBodyIncludes("公网隧道") && await documentBodyIncludes("客户端访问地址"));
  await page.evaluate(`(() => {
    for (const selector of ['[data-mcp-direct-hosts="1"]', '[data-mcp-direct-bind="1"]']) {
      const field = document.querySelector(selector);
      field.value = ${JSON.stringify(lanIp)};
      field.dispatchEvent(new Event('input', { bubbles: true }));
    }
  })()`);
  await waitFor(() => exists('[data-mcp-direct-start="1"]:enabled'), "网络访问页可启动网卡直连");
  await click('[data-mcp-direct-start="1"]');
  await waitFor(() => exists('[role="dialog"] .btn--error'), "网络直连风险确认");
  await click('[role="dialog"] .btn--error');
  const direct = await waitFor(async () => {
    const current = await status();
    return current.direct.listening && current.direct;
  }, "daemon 开启直连监听");
  const lanUrl = direct.mcpUrl;
  await waitFor(async () => (await text('[data-mcp-direct-url="1"]')) === lanUrl, "网络访问页展示直连地址");
  check("无需 tunnel 即可监听真实网卡地址", direct.bindHost === lanIp && new URL(lanUrl).hostname === lanIp);
  const lanList = (secret) => fetch(lanUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    signal: AbortSignal.timeout(10_000),
  }).catch((cause) => ({ status: null, error: `网络直连验收无法连接 ${lanUrl}：${cause.message}` }));
  const [networkReply, localReply] = await Promise.all([lanList(networkIssued.secret), lanList(localOnly.secret)]);
  check("内网直连接受网络令牌并拒绝本机令牌", networkReply.status === 200 && localReply.status === 401,
    JSON.stringify({ network: networkReply.status, local: localReply.status, error: networkReply.error ?? localReply.error }));
  await click('[data-mcp-direct-stop="1"]');
  await waitFor(async () => !(await status()).direct.listening, "停止网络直连");
  await openSettingsTab("clients");
  await setMcpEnabled(false);
  const logText = electronLog.join("");
  check("移除工作区后 sidecar 维护没有归属错误", !/刷新 sidecar 失败[^\n]*未注册 workspace/u.test(logText));
  check("历史快照与实时事件无 canonical 事实冲突", !rendererErrors.some((error) => error.includes('[canonical-turn-store]')), rendererErrors.slice(-4).join('\n'));
  check("Electron 日志没有 Desktop IPC 协议拒绝", !/IPC.*(validation|拒绝)/iu.test(logText));
} catch (error) {
  failure = error;
  if (!results.some(({ ok }) => !ok)) results.push({ name: "MCP 验收流程完成", ok: false, detail: error.message });
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1500)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n`);
    process.stderr.write(`\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-8).join("\n---\n")}\n`);
    const nav = await page.evaluate(`[...document.querySelectorAll('[data-testid^="settings-nav-"]')].map((el) => el.dataset.testid + '=' + el.getAttribute('aria-current')).join(', ')`).catch(() => "");
    process.stderr.write(`\n[设置导航]\n${nav}\n`);
  }
  process.stderr.write(`\n[Electron 日志尾部]\n${electronLog.join("").slice(-2500)}\n`);
} finally {
  if (failure || process.env.MAGI_ELECTRON_MCP_DIAGNOSTICS_DIR) {
    const diagnostics = process.env.MAGI_ELECTRON_MCP_DIAGNOSTICS_DIR || workRoot;
    await mkdir(diagnostics, { recursive: true });
    await writeFile(join(diagnostics, "diagnostics.json"), JSON.stringify({ completed: !failure, failure: failure?.message, checks: results, rendererErrors }, null, 2));
    await writeFile(join(diagnostics, "electron.log"), electronLog.join(""));
    if (page) {
      const screenshot = await page.call("Page.captureScreenshot", { format: "png" }).catch(() => null);
      if (screenshot) await writeFile(join(diagnostics, "screen.png"), Buffer.from(screenshot.data, "base64"));
    }
    process.stdout.write(`验收现场已保存: ${diagnostics}\n`);
  }
  page?.close();
  await app.stop();
  provider.closeAllConnections();
  provider.close();
  if (!failure) await rm(workRoot, { recursive: true, force: true }).catch(() => {});
}

process.stdout.write(`\n${summary()}\n`);
if (failure) {
  process.stderr.write(`${failure.message}\n`);
  process.exit(1);
}
