import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFile, spawn, spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import WebSocket from "ws";

/**
 * 打包 Electron 中真实 Chromium Browser 工具的权限代表格。
 *
 * 每个 AccessProfile 都在真实 Turn 中完成 Browser 读取/写入及无效、跨
 * Workspace 拒绝；另以 FullAccess Turn 单独验证跨 Session Tab 拒绝。Provider
 * 终态和外部副作用都进入 artifact，脚本不直接写 canonical 或前端 read model。
 */

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = join(
  repositoryRoot,
  "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi",
);
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_BROWSER_PERMISSION_CDP_PORT || "10427", 10);
const daemonPort = 38123;
const evidencePath = process.env.MAGI_ELECTRON_BROWSER_PERMISSION_EVIDENCE_PATH?.trim()
  || "/tmp/magi-electron-browser-permission-matrix-20260922.json";
const profiles = [
  { name: "ReadOnly", wire: "read_only" },
  { name: "Restricted", wire: "restricted" },
  { name: "FullAccess", wire: "full_access" },
];
const execFileAsync = promisify(execFile);
const checks = [];
const backendTimingByTurn = new Map();
const pendingBackendTimingByTrace = new Map();
const traceToTurn = new Map();
const electronLogBuffers = { stdout: "", stderr: "" };

if (!Number.isInteger(cdpPort) || cdpPort < 1024 || cdpPort > 65535) {
  throw new Error(`无效 CDP 端口: ${cdpPort}`);
}

function check(name, condition, detail = "") {
  const record = { name, passed: Boolean(condition), detail };
  checks.push(record);
  if (!record.passed) {
    throw new Error(`Electron Browser 权限验收失败：${name}${detail ? `（${detail}）` : ""}`);
  }
}

function logField(line, key) {
  const match = line.match(new RegExp(`${key}=(?:"([^"]*)"|([^\\s]+))`, "u"));
  return match?.[1] ?? match?.[2] ?? "";
}

function timingLogTimestamp(line) {
  const match = line.match(/\b(\d{4}-\d{2}-\d{2}T[^\s]+Z)\b/u);
  const timestamp = match?.[1] ? Date.parse(match[1]) : Number.NaN;
  return Number.isFinite(timestamp) ? timestamp : Date.now();
}

function backendRecord(turnId) {
  let record = backendTimingByTurn.get(turnId);
  if (!record) {
    record = { turnId, traceId: "", sessionId: "", stages: {} };
    backendTimingByTurn.set(turnId, record);
  }
  return record;
}

function addBackendTimingEvent(event, turnIdOverride = "") {
  const turnId = turnIdOverride || event.turnId;
  if (!turnId) return;
  const record = backendRecord(turnId);
  if (event.traceId) record.traceId ||= event.traceId;
  if (event.sessionId) record.sessionId ||= event.sessionId;
  const previous = record.stages[event.stage];
  const point = {
    elapsedMs: event.elapsedMs,
    eventSequence: Number.isSafeInteger(event.eventSequence) ? event.eventSequence : null,
    requestId: event.requestId || null,
  };
  record.stages[event.stage] = {
    count: (previous?.count || 0) + 1,
    first: previous?.first || point,
    last: point,
  };
}

function mergePendingBackendTiming(traceId, turnId) {
  const pending = pendingBackendTimingByTrace.get(traceId);
  if (!pending) return;
  for (const event of pending) addBackendTimingEvent(event, turnId);
  pendingBackendTimingByTrace.delete(traceId);
}

function parseBackendTimingLine(line) {
  if (!line.includes("conversation response timing")) return;
  const stage = logField(line, "stage");
  if (!stage) return;
  const event = {
    stage,
    traceId: logField(line, "trace_id"),
    requestId: logField(line, "request_id"),
    sessionId: logField(line, "session_id"),
    turnId: logField(line, "turn_id") || logField(line, "expected_turn_id"),
    eventSequence: Number.parseInt(logField(line, "event_sequence"), 10),
    elapsedMs: Number.parseInt(logField(line, "elapsed_ms"), 10) || 0,
  };
  if (!event.turnId && event.traceId && traceToTurn.has(event.traceId)) {
    event.turnId = traceToTurn.get(event.traceId);
  }
  if (event.turnId) {
    addBackendTimingEvent(event);
    if (event.traceId) {
      traceToTurn.set(event.traceId, event.turnId);
      mergePendingBackendTiming(event.traceId, event.turnId);
    }
  } else if (event.traceId) {
    const pending = pendingBackendTimingByTrace.get(event.traceId) || [];
    pending.push(event);
    pendingBackendTimingByTrace.set(event.traceId, pending);
  }
}

function consumeElectronLogChunk(stream, chunk) {
  const text = chunk.toString();
  process.stdout.write(`[electron] ${text}`);
  const combined = electronLogBuffers[stream] + text;
  const lines = combined.split(/\r?\n/u);
  electronLogBuffers[stream] = lines.pop() || "";
  for (const line of lines) parseBackendTimingLine(line);
}

function flushElectronLogBuffers() {
  for (const stream of Object.keys(electronLogBuffers)) {
    if (electronLogBuffers[stream]) parseBackendTimingLine(electronLogBuffers[stream]);
    electronLogBuffers[stream] = "";
  }
}

function openAiStream(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}

function createProvider() {
  const requests = [];
  const pendingResponses = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      if (request.url === "/v1/models" && request.method === "GET") {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({
          object: "list",
          data: [{ id: "browser-permission-model", object: "model", owned_by: "harness" }],
        }));
        return;
      }
      if (request.url === "/release" && request.method === "POST") {
        const pending = pendingResponses.shift();
        if (!pending) {
          response.writeHead(409, { "content-type": "application/json" });
          response.end(JSON.stringify({ error: "no_pending_provider_request" }));
          return;
        }
        pending.response.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        });
        pending.response.end(openAiStream(pending.content));
        response.writeHead(204).end();
        return;
      }
      if (request.url !== "/v1/chat/completions" || request.method !== "POST") {
        response.writeHead(404).end();
        return;
      }
      let parsed;
      try {
        parsed = JSON.parse(body);
      } catch {
        response.writeHead(400).end();
        return;
      }
      requests.push(parsed);
      const userText = (Array.isArray(parsed.messages) ? parsed.messages : [])
        .filter((message) => message?.role === "user")
        .map((message) => typeof message.content === "string" ? message.content : "")
        .at(-1) || "";
      pendingResponses.push({
        response,
        content: `BROWSER_PERMISSION_${userText.match(/ReadOnly|Restricted|FullAccess/u)?.[0] || "UNKNOWN"}`,
      });
    });
  });
  return { server, requests, pendingResponses };
}

function providerRequestCountForUserText(provider, text) {
  return provider.requests.filter((request) => (
    Array.isArray(request.messages)
      && request.messages.some((message) => (
        message?.role === "user"
        && typeof message.content === "string"
        && message.content.includes(text)
      ))
  )).length;
}

function createFixture() {
  const sideEffects = [];
  const server = createServer((request, response) => {
    const url = new URL(request.url || "/", "http://127.0.0.1");
    if (url.pathname === "/fixture" && request.method === "GET") {
      const profile = url.searchParams.get("profile") || "unknown";
      const page = `<!doctype html><html><body>
        <main><h1>Browser permission fixture</h1>
        <button id="send" aria-label="Send ${profile}" onclick="fetch('/side-effect?profile=${profile}', {method: 'POST'}).then(() => { document.body.dataset.sideEffect = 'done'; })">Send ${profile}</button>
        <button id="noop" aria-label="Noop ${profile}">Noop ${profile}</button>
        </main></body></html>`;
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(page);
      return;
    }
    if (url.pathname === "/side-effect" && request.method === "POST") {
      sideEffects.push({ profile: url.searchParams.get("profile") || "", at: Date.now() });
      response.writeHead(204).end();
      return;
    }
    response.writeHead(404).end();
  });
  return { server, sideEffects };
}

async function waitFor(predicate, label, timeoutMs = 45_000, intervalMs = 100) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(intervalMs);
  }
  throw new Error(`等待超时：${label}${lastError ? `；最后错误：${lastError.message}` : ""}`);
}

async function collectSourceIdentity() {
  const runGit = (args, encoding = "utf8") => {
    const result = spawnSync("git", args, {
      cwd: repositoryRoot,
      encoding,
      // 工作区包含 Electron 图标和打包旁证时，完整 binary diff 可能超过
      // Node spawnSync 的默认缓冲区；身份采集不能因此把一次有效验收误报为失败。
      maxBuffer: 128 * 1024 * 1024,
    });
    if (result.status !== 0) {
      const detail = result.error instanceof Error ? `: ${result.error.message}` : "";
      throw new Error(`采集 Chromium 权限证据源码身份失败：git ${args.join(" ")}${detail}`);
    }
    return result.stdout;
  };
  const sourceCommit = runGit(["rev-parse", "HEAD"]).trim();
  const status = runGit(["status", "--porcelain=v1", "--untracked-files=all"]);
  const trackedDiff = runGit(["diff", "--binary", "HEAD"], null);
  const untrackedPaths = runGit(["ls-files", "--others", "--exclude-standard", "-z"])
    .split("\0")
    .filter(Boolean);
  const fingerprint = createHash("sha256").update(trackedDiff);
  for (const path of untrackedPaths) {
    fingerprint.update(Buffer.from(`\0${path}\0`));
    fingerprint.update(await readFile(join(repositoryRoot, path)));
  }
  const executableSha256 = createHash("sha256")
    .update(await readFile(appExecutable))
    .digest("hex");
  return {
    source_commit: sourceCommit,
    worktree_fingerprint_sha256: fingerprint.digest("hex"),
    executable_sha256: executableSha256,
    dirty: status.trim().length > 0,
    changed_path_count: status.split("\n").filter(Boolean).length,
  };
}

async function connectCdpPage() {
  const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
  const target = targets.find((candidate) => (
    candidate.type === "page" && candidate.url.includes(`http://127.0.0.1:${daemonPort}/web.html`)
  ));
  if (!target?.webSocketDebuggerUrl) throw new Error("未找到打包 Electron App Renderer");
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let nextId = 1;
  socket.on("message", (raw) => {
    const message = JSON.parse(raw.toString());
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(message.error.message || "CDP 请求失败"));
    else request.resolve(message);
  });
  await new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", reject);
  });
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => {
      if (pending.delete(id)) reject(new Error(`CDP 超时: ${method}`));
    }, 20_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  return {
    async evaluate(expression) {
      const result = await send("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
      });
      const remote = result.result?.result;
      if (remote?.subtype === "error" || result.result?.exceptionDetails) {
        throw new Error(remote?.description || result.result.exceptionDetails?.text || "Renderer evaluate 失败");
      }
      return remote?.value;
    },
    async navigate(url) {
      const result = await send("Page.navigate", { url });
      const errorText = result.result?.errorText;
      if (typeof errorText === "string" && errorText.trim()) {
        throw new Error(`CDP 页面导航失败: ${errorText}`);
      }
    },
    close() {
      for (const request of pending.values()) {
        clearTimeout(request.timer);
        request.reject(new Error("CDP 连接已关闭"));
      }
      pending.clear();
      socket.close();
    },
  };
}

async function rendererRequest(page, path, init = {}) {
  const result = await page.evaluate(`(${async function request(relativePath, requestInit) {
    const response = await fetch(relativePath, requestInit);
    return { status: response.status, body: await response.text() };
  }.toString()})(${JSON.stringify(path)}, ${JSON.stringify(init)})`);
  return { ...result, json: result.body ? JSON.parse(result.body) : null };
}

async function openBrowserPane(page) {
  await waitFor(async () => page.evaluate("document.readyState === 'complete'"), "App Renderer 页面完成");
  await waitFor(
    async () => page.evaluate("Boolean(document.querySelector('.header-right-pane-btn'))"),
    "App Renderer 主界面挂载",
  );
  await page.evaluate("document.querySelector('.header-right-pane-btn')?.click(); true");
  await waitFor(async () => page.evaluate("Boolean(document.querySelector('.right-pane-add-tab'))"), "右侧面板挂载");
  await page.evaluate("document.querySelector('.right-pane-add-tab')?.click(); true");
  await waitFor(async () => page.evaluate("[...document.querySelectorAll('.right-pane-add-menu-item')].some((item) => item.innerText.trim() === '浏览器')"), "浏览器新增菜单");
  await page.evaluate("[...document.querySelectorAll('.right-pane-add-menu-item')].find((item) => item.innerText.trim() === '浏览器')?.click(); true");
  return waitFor(async () => {
    const sessionId = await page.evaluate("new URL(location.href).searchParams.get('sessionId') || ''");
    if (!sessionId) return null;
    const current = await rendererRequest(page, `/api/browser/sessions/current?scope=personal&sessionId=${encodeURIComponent(sessionId)}`);
    const tab = current.json?.session?.tabs?.find((candidate) => candidate.lifecycle === "ready" && candidate.surfaceId);
    return tab ? { sessionId, tab } : null;
  }, "真实 Browser Tab Surface", 60_000);
}

function eventsForTurn(appServer, turnId) {
  const events = [];
  for (const notification of appServer?.notifications || []) {
    if (notification.method === "events/snapshot") {
      events.push(...(notification.params?.recentEvents || []));
    } else if (notification.method === "events/resyncRequired") {
      events.push(...(notification.params?.snapshot?.recent_events || []));
    } else if (notification.method?.startsWith("event/")) {
      if (notification.params?.event) events.push(notification.params.event);
    }
  }
  return events.filter((event) => (
    event.payload?.turn_id === turnId
    || event.payload?.turnId === turnId
    || event.payload?.canonical_turn?.turnId === turnId
    || event.payload?.canonical_turn?.turn_id === turnId
  ));
}

function summarizeTurnEvents(appServer, turnId) {
  return eventsForTurn(appServer, turnId).map((event) => ({
    event_type: event.event_type || null,
    sequence: Number.isSafeInteger(event.sequence) ? event.sequence : null,
    canonical_event_kind: event.payload?.canonical_event_kind || null,
    canonical_turn_status: event.payload?.canonical_turn?.status || null,
    canonical_item_kind: event.payload?.canonical_item?.kind || null,
    canonical_item_status: event.payload?.canonical_item?.status || null,
  }));
}

function browserAuthorization({
  browserAccess,
  requestedAccess = browserAccess,
  allowed,
  sessionId,
  workspaceId,
  browserSessionId,
  tabId,
  turnId,
  surfaceId = null,
  scopeKind = null,
  leaseEvent = null,
  requestedWorkspaceId = null,
  requestedSessionId = null,
  sourceSessionId = null,
  authorizationBasis = null,
  browserOrigin = "loopback_fixture",
}) {
  const lease = leaseEvent?.payload || null;
  const kind = scopeKind || (workspaceId ? "workspace_session" : "personal_session");
  const basis = authorizationBasis || (allowed
    ? requestedAccess === "write"
      ? "browser_tool_access+surface_control_lease"
      : "browser_tool_access+browser_session_tab_binding"
    : "browser_session_scope_or_tab_authority_rejection");
  const controlLease = lease ? {
    event_id: leaseEvent.event_id || null,
    event_sequence: Number.isSafeInteger(leaseEvent.sequence) ? leaseEvent.sequence : null,
    lease_id: lease.lease_id || lease.leaseId || null,
    workspace_id: leaseEvent.workspace_id || null,
    session_id: leaseEvent.session_id || null,
    turn_id: lease.turn_id || lease.turnId || null,
    tab_id: lease.tab_id || lease.tabId || null,
    surface_id: lease.surface_id || lease.surfaceId || null,
  } : null;
  return {
    authorization_basis: basis,
    authorization_decision: allowed ? "allow" : "deny",
    approval_requirement: "not_required",
    workspace_id: workspaceId,
    session_id: sessionId,
    browser_session_id: browserSessionId,
    browser_tab_id: tabId,
    browser_origin: browserOrigin,
    authorization_scope: {
      kind,
      workspace_id: workspaceId,
      session_id: sessionId,
      browser_session_id: browserSessionId,
      browser_tab_id: tabId,
      requested_workspace_id: requestedWorkspaceId,
      requested_session_id: requestedSessionId,
      source_session_id: sourceSessionId,
      turn_id: turnId,
      surface_id: surfaceId || lease?.surface_id || lease?.surfaceId || null,
      control_lease: controlLease,
    },
    authorization_evidence: {
      browser_tool_access: browserAccess,
      requested_access: requestedAccess,
      decision: allowed ? "allow" : "deny",
      basis,
      control_lease_event: controlLease,
    },
  };
}

async function createWorkspaceBrowserContext(page, workspaceRoot) {
  const registered = await rendererRequest(page, "/api/workspaces/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path: workspaceRoot }),
  });
  const workspaceId = registered.json?.workspaceId;
  check(
    "正向 Browser 场景注册真实 workspace",
    registered.status === 200 && typeof workspaceId === "string" && workspaceId.length > 0,
    JSON.stringify(registered),
  );
  const materialized = await rendererRequest(page, "/api/session/materialize", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ scope: "workspace", workspaceId }),
  });
  const sessionId = materialized.json?.sessionId;
  check(
    "正向 Browser 场景物化并持久化 workspace Session",
    materialized.status === 200
      && materialized.json?.workspaceId === workspaceId
      && typeof sessionId === "string"
      && sessionId.length > 0,
    JSON.stringify(materialized),
  );
  const createdSession = await rendererRequest(page, "/api/browser/sessions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      scope: "workspace",
      workspaceId,
      sessionId,
      clientPlatform: "desktop",
    }),
  });
  const browserSessionId = createdSession.json?.browserSessionId;
  check(
    "Browser Session 与真实 workspace/Session 双向绑定",
    [200, 201].includes(createdSession.status)
      && createdSession.json?.workspaceId === workspaceId
      && createdSession.json?.sessionId === sessionId
      && typeof browserSessionId === "string"
      && browserSessionId.length > 0,
    JSON.stringify(createdSession),
  );
  const createdTab = await rendererRequest(
    page,
    `/api/browser/sessions/${encodeURIComponent(browserSessionId)}/tabs`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ initialUrl: "about:blank", clientPlatform: "desktop" }),
    },
  );
  const tabId = createdTab.json?.tabId;
  check(
    "workspace-bound Browser Tab 由打包 Electron Host 创建",
    createdTab.status === 201
      && typeof tabId === "string"
      && createdTab.json?.browserSessionId === browserSessionId,
    JSON.stringify(createdTab),
  );
  return { workspaceId, sessionId, browserSessionId, tabId, workspaceRoot };
}

async function attachWorkspaceBrowserSurface(page, workspace) {
  const currentUrl = new URL(await page.evaluate("location.href"));
  currentUrl.searchParams.set("scope", "workspace");
  currentUrl.searchParams.set("workspaceId", workspace.workspaceId);
  currentUrl.searchParams.set("workspacePath", workspace.workspaceRoot);
  currentUrl.searchParams.set("sessionId", workspace.sessionId);
  await page.navigate(currentUrl.toString());
  await waitFor(async () => {
    const url = new URL(await page.evaluate("location.href"));
    return documentReady(await page.evaluate("document.readyState"))
      && url.searchParams.get("scope") === "workspace"
      && url.searchParams.get("workspaceId") === workspace.workspaceId
      && url.searchParams.get("sessionId") === workspace.sessionId;
  }, "App Renderer 切换到 workspace Session", 60_000);
  await waitFor(
    async () => page.evaluate("Boolean(document.querySelector('.header-right-pane-btn'))"),
    "workspace App Renderer 主界面挂载",
    60_000,
  );
  const rightPaneExpanded = await page.evaluate(
    "document.querySelector('.header-right-pane-btn')?.getAttribute('aria-expanded') === 'true'",
  );
  if (!rightPaneExpanded) {
    await page.evaluate("document.querySelector('.header-right-pane-btn')?.click(); true");
  }
  const query = new URLSearchParams({
    scope: "workspace",
    workspaceId: workspace.workspaceId,
    sessionId: workspace.sessionId,
  });
  const current = await waitFor(async () => {
    const response = await rendererRequest(page, `/api/browser/sessions/current?${query}`);
    const session = response.json?.session;
    const tab = session?.tabs?.find((candidate) => candidate.tabId === workspace.tabId);
    return response.status === 200
      && session?.workspaceId === workspace.workspaceId
      && session?.sessionId === workspace.sessionId
      && session?.browserSessionId === workspace.browserSessionId
      && tab?.lifecycle === "ready"
      && typeof tab.surfaceId === "string"
      ? { session, tab }
      : null;
  }, "workspace-bound Chromium Surface Ready", 60_000);
  check(
    "workspace BrowserAuthority 通过可信 Renderer guest 注册真实 Surface",
    current.tab.surfaceId.length > 0 && current.session.browserSessionId === workspace.browserSessionId,
    JSON.stringify({ session: current.session, tab: current.tab }),
  );
  return { ...workspace, session: current.session, tab: current.tab };
}

function documentReady(state) {
  return state === "complete" || state === "interactive";
}

async function runAppServerBrowserTurn(page, input) {
  const {
    sessionId,
    tabId,
    profile,
    turnIndex,
    workspaceId = null,
    scope = "personal",
    fixtureLabel = profile.name,
    prompt = `真实 Chromium 权限验收 ${profile.name}`,
  } = input;
  const browserTools = [
    {
      case: "read_snapshot",
      tool: "browser_snapshot",
      arguments: { tab_id: tabId },
    },
    {
      case: "allow_click",
      tool: "browser_click",
    },
    {
      case: "write_evaluate",
      tool: "browser_evaluate",
      arguments: {
        tab_id: tabId,
        expression: `fetch('/side-effect?profile=${fixtureLabel}', {method: 'POST'}).then(() => 'browser_permission_${fixtureLabel}_side_effect')`,
      },
    },
    {
      case: "mixed_read_console",
      tool: "browser_console",
      arguments: { tab_id: tabId, action: "list", page_size: 10 },
    },
    {
      case: "mixed_write_console",
      tool: "browser_console",
      arguments: { tab_id: tabId, action: "clear" },
    },
    {
      case: "reject_unknown_tab",
      tool: "browser_snapshot",
      arguments: { tab_id: `missing-browser-tab-${profile.name}` },
    },
    {
      case: "reject_workspace_mismatch",
      tool: "browser_snapshot",
      arguments: { tab_id: tabId },
      workspaceId: `workspace-browser-permission-mismatch-${profile.name}`,
    },
  ];
  const expression = `(${async function run(input) {
    const socketUrl = `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/app-server`;
    const socket = new WebSocket(socketUrl);
    const pending = new Map();
    const notifications = [];
    let nextRequestId = 1;
    const waitFor = (id) => new Promise((resolve, reject) => {
      const key = String(id);
      const timer = setTimeout(() => {
        pending.delete(key);
        reject(new Error(`App Server 请求超时: ${key}`));
      }, 60_000);
      pending.set(key, { resolve, reject, timer });
    });
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data);
      const pendingRequest = pending.get(String(message.id));
      if (!pendingRequest) {
        notifications.push(message);
        return;
      }
      pending.delete(String(message.id));
      clearTimeout(pendingRequest.timer);
      pendingRequest.resolve(message);
    };
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = reject;
    });
    const request = (method, params) => {
      const id = String(nextRequestId++);
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      return waitFor(id);
    };
    const initialize = await request("initialize", {
      clientInfo: { name: "magi-electron-browser-permission-harness", version: "1" },
      protocol: { major: 1, minor: 0 },
      capabilities: {
        desktopBrowserSurface: true,
        browserTools: true,
        approvals: true,
        streaming: true,
      },
    });
    socket.send(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
    const subscription = await request("events/subscribe", {
      sessionId: input.sessionId,
      ...(input.workspaceId ? { workspaceId: input.workspaceId } : {}),
      afterSequence: 0,
    });
    if (subscription.error) return { initialize, subscription, requestId: "", turn: null, browsers: [], notifications };
    const requestId = `electron-browser-permission-${input.turnIndex}`;
    const turn = await request("turn/start", {
      sessionId: input.sessionId,
      scope: input.scope,
      ...(input.workspaceId ? { workspaceId: input.workspaceId } : {}),
      text: input.prompt,
      accessProfile: input.profileWire,
      requestId,
    });
    if (turn.error) return { initialize, subscription, turn, requestId, browser: null, notifications };
    const browserExecutionId = turn.result?.turnId || "";
    const browsers = [];
    let latestSnapshot = null;
    for (const [index, browserTool] of input.browserTools.entries()) {
      const browserArguments = browserTool.case === "allow_click"
        ? {
            tab_id: input.tabId,
            element_ref: latestSnapshot?.snapshot?.elements?.find((element) => element.name?.startsWith("Noop "))?.element_ref,
          }
        : browserTool.arguments;
      browsers.push({
        case: browserTool.case,
        response: await request("browser/tool", {
          sessionId: input.sessionId,
          ...(input.workspaceId && !browserTool.workspaceId ? { workspaceId: input.workspaceId } : {}),
          tool: browserTool.tool,
          arguments: browserArguments,
          ...(browserTool.workspaceId ? { workspaceId: browserTool.workspaceId } : {}),
          callId: `electron-browser-permission-call-${input.turnIndex}-${index + 1}`,
          browserExecutionId,
        }),
      });
      if (browserTool.case === "read_snapshot") {
        latestSnapshot = browsers.at(-1).response.result?.payload;
      }
    }
    socket.close();
    return { initialize, subscription, turn, requestId, browserExecutionId, browsers, notifications };
  }.toString()})(${JSON.stringify({
    sessionId,
    tabId,
    turnIndex,
    workspaceId,
    scope,
    prompt,
    fixtureLabel,
    profile: profile.name,
    profileWire: profile.wire,
    browserTools,
  })})`;
  return page.evaluate(expression);
}

async function runCrossSessionTabAttempt(page, input) {
  const expression = `(${async function run(input) {
    const socketUrl = `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/app-server`;
    const socket = new WebSocket(socketUrl);
    const pending = new Map();
    let nextRequestId = 1;
    const waitFor = (id) => new Promise((resolve, reject) => {
      const key = String(id);
      const timer = setTimeout(() => {
        pending.delete(key);
        reject(new Error(`App Server 请求超时: ${key}`));
      }, 60_000);
      pending.set(key, { resolve, reject, timer });
    });
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data);
      const pendingRequest = pending.get(String(message.id));
      if (!pendingRequest) return;
      pending.delete(String(message.id));
      clearTimeout(pendingRequest.timer);
      pendingRequest.resolve(message);
    };
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = reject;
    });
    const request = (method, params) => {
      const id = String(nextRequestId++);
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      return waitFor(id);
    };
    const initialize = await request("initialize", {
      clientInfo: { name: "magi-electron-browser-cross-session-harness", version: "1" },
      protocol: { major: 1, minor: 0 },
      capabilities: { desktopBrowserSurface: true, browserTools: true, streaming: true },
    });
    socket.send(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
    const requestId = "electron-browser-cross-session-tab";
    const prompt = "真实 Chromium 跨 Session Tab 作用域验收";
    const turn = await request("turn/start", {
      sessionId: input.sessionId,
      scope: "personal",
      text: prompt,
      accessProfile: "full_access",
      requestId,
    });
    let browser = null;
    if (turn.result?.kind === "accepted") {
      browser = await request("browser/tool", {
        sessionId: input.sessionId,
        tool: "browser_snapshot",
        arguments: { tab_id: input.foreignTabId },
        callId: "electron-browser-cross-session-tab-call",
        browserExecutionId: turn.result.turnId,
      });
    }
    socket.close();
    return { initialize, requestId, prompt, turn, browser };
  }.toString()})(${JSON.stringify(input)})`;
  return page.evaluate(expression);
}

async function readCanonicalTurn(page, input) {
  const expression = `(${async function run(input) {
    const socketUrl = `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/app-server`;
    const socket = new WebSocket(socketUrl);
    const pending = new Map();
    let nextRequestId = 1;
    const waitFor = (id) => new Promise((resolve, reject) => {
      const key = String(id);
      const timer = setTimeout(() => {
        pending.delete(key);
        reject(new Error(`App Server session/read 请求超时: ${key}`));
      }, 20_000);
      pending.set(key, { resolve, reject, timer });
    });
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data);
      const pendingRequest = pending.get(String(message.id));
      if (!pendingRequest) return;
      pending.delete(String(message.id));
      clearTimeout(pendingRequest.timer);
      pendingRequest.resolve(message);
    };
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = reject;
    });
    const request = (method, params) => {
      const id = String(nextRequestId++);
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      return waitFor(id);
    };
    await request("initialize", {
      clientInfo: { name: "magi-electron-browser-permission-readback", version: "1" },
      protocol: { major: 1, minor: 0 },
      capabilities: { browserTools: true, streaming: true },
    });
    socket.send(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
    const read = await request("session/read", {
      sessionId: input.sessionId,
      includeTurns: true,
    });
    socket.close();
    const turns = Array.isArray(read.result?.turns) ? read.result.turns : [];
    return {
      read,
      turn: turns.find((candidate) => candidate?.turnId === input.turnId) || null,
    };
  }.toString()})(${JSON.stringify(input)})`;
  return page.evaluate(expression);
}

async function ownedDaemonPid(parentPid) {
  try {
    const { stdout } = await execFileAsync("pgrep", ["-P", String(parentPid)]);
    const candidates = stdout.trim().split(/\s+/u).filter(Boolean);
    for (const candidate of candidates) {
      const { stdout: command } = await execFileAsync("ps", ["-p", candidate, "-o", "command="]);
      if (command.includes("magi-daemon-app")) return Number.parseInt(candidate, 10);
    }
  } catch {
    return null;
  }
  return null;
}

async function stopProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  await sleep(2_000);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

async function main() {
  const provider = createProvider();
  const fixture = createFixture();
  await new Promise((resolve) => provider.server.listen(0, "127.0.0.1", resolve));
  await new Promise((resolve) => fixture.server.listen(0, "127.0.0.1", resolve));
  const providerPort = provider.server.address().port;
  const fixturePort = fixture.server.address().port;
  const stateRoot = await mkdtemp(join(tmpdir(), "magi-electron-browser-permission-"));
  const sourceIdentityBefore = await collectSourceIdentity();
  const electron = spawn(appExecutable, [`--remote-debugging-port=${cdpPort}`, "--disable-gpu"], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      MAGI_STATE_ROOT: join(stateRoot, "state"),
      MAGI_OPEN_BROWSER: "0",
      MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      MAGI_OPENAI_COMPAT_API_KEY: "electron-browser-permission-key",
      MAGI_OPENAI_COMPAT_MODEL: "browser-permission-model",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  electron.stdout.on("data", (chunk) => consumeElectronLogChunk("stdout", chunk));
  electron.stderr.on("data", (chunk) => consumeElectronLogChunk("stderr", chunk));
  let page = null;
  try {
    await waitFor(async () => {
      try {
        page = await connectCdpPage();
        return page;
      } catch {
        return null;
      }
    }, "打包 Electron App Renderer", 60_000, 250);
    const browser = await openBrowserPane(page);
    check("打包 Electron 创建真实 Browser Tab", Boolean(browser.tab.surfaceId));
    check("真实 Browser Tab 导航到 about:blank 初始页面", browser.tab.url === "about:blank");
    const rows = [];
    for (const [index, profile] of profiles.entries()) {
      const navigation = await rendererRequest(
        page,
        `/api/browser/tabs/${encodeURIComponent(browser.tab.tabId)}/navigation`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            action: "url",
            url: `http://127.0.0.1:${fixturePort}/fixture?profile=${profile.name}`,
            clientPlatform: "desktop",
          }),
        },
      );
      check(`${profile.name} 真实 Chromium 导航成功`, navigation.status === 200, `HTTP ${navigation.status}`);
      const tab = navigation.json;
      const beforeSideEffects = fixture.sideEffects.length;
      const appServer = await runAppServerBrowserTurn(page, {
        sessionId: browser.sessionId,
        tabId: tab.tabId,
        profile,
        turnIndex: index + 1,
      });
      check(`${profile.name} App Server Desktop 浏览器能力已授权`, appServer.initialize?.result?.capabilities?.browserTools === true);
      check(`${profile.name} Turn 已接纳`, appServer.turn?.result?.kind === "accepted", JSON.stringify(appServer.turn));
      check(
        `${profile.name} request_id 与 Turn receipt 一致`,
        appServer.requestId === appServer.turn?.result?.requestId,
        JSON.stringify({ requestId: appServer.requestId, receipt: appServer.turn?.result?.requestId }),
      );
      check(
        `${profile.name} Browser execution identity 绑定 canonical Turn`,
        appServer.browserExecutionId === appServer.turn?.result?.turnId,
        JSON.stringify({ browserExecutionId: appServer.browserExecutionId, turnId: appServer.turn?.result?.turnId }),
      );
      check(
        `${profile.name} Turn 固化为 conversation profile`,
        appServer.turn?.result?.executionProfile === "conversation",
        JSON.stringify(appServer.turn),
      );
      const snapshot = appServer.browsers?.find((browserTool) => browserTool.case === "read_snapshot")?.response;
      const click = appServer.browsers?.find((browserTool) => browserTool.case === "allow_click")?.response;
      const evaluate = appServer.browsers?.find((browserTool) => browserTool.case === "write_evaluate")?.response;
      const mixedConsoleRead = appServer.browsers?.find((browserTool) => browserTool.case === "mixed_read_console")?.response;
      const mixedConsoleWrite = appServer.browsers?.find((browserTool) => browserTool.case === "mixed_write_console")?.response;
      const rejected = appServer.browsers?.find((browserTool) => browserTool.case === "reject_unknown_tab")?.response;
      const workspaceMismatch = appServer.browsers?.find((browserTool) => browserTool.case === "reject_workspace_mismatch")?.response;
      check(
        `${profile.name} Browser snapshot 读取成功`,
        snapshot?.result?.status === "completed"
          && snapshot.result.payload?.tool === "browser_snapshot",
        JSON.stringify(snapshot),
      );
      check(
        `${profile.name} Browser click 交互写操作成功`,
        click?.result?.status === "completed"
          && click.result.payload?.tool === "browser_click",
        JSON.stringify(click),
      );
      check(
        `${profile.name} Browser evaluate 写操作成功`,
        evaluate?.result?.status === "completed"
          && evaluate.result.payload?.tool === "browser_evaluate",
        JSON.stringify(evaluate),
      );
      check(
        `${profile.name} Browser Console Mixed 工具允许真实读操作`,
        mixedConsoleRead?.result?.status === "completed"
          && mixedConsoleRead.result.payload?.tool === "browser_console",
        JSON.stringify(mixedConsoleRead),
      );
      check(
        `${profile.name} Browser Console Mixed 工具允许真实写操作`,
        mixedConsoleWrite?.result?.status === "completed"
          && mixedConsoleWrite.result.payload?.tool === "browser_console",
        JSON.stringify(mixedConsoleWrite),
      );
      check(
        `${profile.name} 无效 Browser Tab 被拒绝`,
        rejected?.result?.status === "failed"
          && rejected.result.payload?.error_code === "browser_tab_not_found",
        JSON.stringify(rejected),
      );
      check(
        `${profile.name} 跨 Workspace 浏览器操作被 fail-closed 拒绝`,
        workspaceMismatch?.result?.status === "failed"
          && workspaceMismatch.result.payload?.error_code === "browser_workspace_scope_mismatch",
        JSON.stringify(workspaceMismatch),
      );
      await waitFor(() => provider.pendingResponses.length === 1, `${profile.name} Provider 请求已到达`);
      const released = await fetch(`http://127.0.0.1:${providerPort}/release`, { method: "POST" });
      check(`${profile.name} Provider 释放成功`, released.status === 204, `HTTP ${released.status}`);
      await waitFor(() => fixture.sideEffects.length > beforeSideEffects, `${profile.name} Chromium 外部 POST 副作用`, 20_000);
      const sideEffects = fixture.sideEffects.slice(beforeSideEffects);
      check(`${profile.name} 真实 Chromium 只产生一次外部副作用`, sideEffects.length === 1);
      check(`${profile.name} 外部副作用归属当前 AccessProfile`, sideEffects[0]?.profile === profile.name);
      const turnId = appServer.turn?.result?.turnId;
      const terminal = await waitFor(() => {
        flushElectronLogBuffers();
        const record = turnId ? backendTimingByTurn.get(turnId) : null;
        return record?.stages?.canonical_terminal_published?.count > 0 ? record : null;
      }, `${profile.name} canonical terminal`, 45_000);
      const canonical = await waitFor(async () => {
        const candidate = await readCanonicalTurn(page, { sessionId: browser.sessionId, turnId });
        return candidate.turn?.status === "completed" ? candidate : null;
      }, `${profile.name} session/read canonical completed`, 20_000);
      check(`${profile.name} session/read 返回同一 Turn`, canonical.turn?.turnId === turnId, JSON.stringify(canonical.read));
      check(`${profile.name} canonical Turn 终态为 completed`, canonical.turn?.status === "completed", JSON.stringify(canonical.turn));
      check(
        `${profile.name} canonical Turn profile 为 conversation`,
        canonical.turn?.metadata?.executionProfile === "conversation",
        JSON.stringify(canonical.turn?.metadata),
      );
      const providerRequestCount = providerRequestCountForUserText(
        provider,
        `真实 Chromium 权限验收 ${profile.name}`,
      );
      check(`${profile.name} 同轮 Provider 请求数为 1`, providerRequestCount === 1, String(providerRequestCount));
      check(`${profile.name} terminal 事件序号存在`, Number.isSafeInteger(terminal.stages.canonical_terminal_published.first.eventSequence));
      const turnEvents = eventsForTurn(appServer, turnId);
      const leaseEvent = turnEvents.find((event) => event.event_type === "browser.lease.acquired") || null;
      const lease = leaseEvent?.payload || null;
      check(
        `${profile.name} 写操作获得真实 Surface control lease`,
        Boolean(lease)
          && typeof lease.lease_id === "string"
          && lease.turn_id === turnId
          && lease.tab_id === tab.tabId
          && lease.surface_id === browser.tab.surfaceId
          && leaseEvent.session_id === browser.sessionId
          && (leaseEvent.workspace_id ?? null) === null,
        JSON.stringify(leaseEvent),
      );
      const authFor = (browserAccess, requestedAccess, allowed, options = {}) => browserAuthorization({
        browserAccess,
        requestedAccess,
        allowed,
        sessionId: browser.sessionId,
        workspaceId: null,
        browserSessionId: browser.tab.browserSessionId,
        tabId: options.tabId || tab.tabId,
        turnId,
        surfaceId: Object.hasOwn(options, "surfaceId") ? options.surfaceId : browser.tab.surfaceId,
        scopeKind: "personal_session",
        leaseEvent: requestedAccess === "write" && allowed ? leaseEvent : null,
        requestedWorkspaceId: options.requestedWorkspaceId ?? null,
        authorizationBasis: options.authorizationBasis || null,
      });
      rows.push({
        ...authFor("read", "read", true),
        fixture_id: `electron-browser-permission:${profile.name}:browser_snapshot`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_read_snapshot`,
        tool: "browser_snapshot",
        browser_tool_access: "read",
        browser_requested_access: "read",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "allow_read",
        side_effect: "none",
        executor_called: true,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: snapshot.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("write", "write", true),
        fixture_id: `electron-browser-permission:${profile.name}:browser_click`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_allow_click`,
        tool: "browser_click",
        browser_tool_access: "write",
        browser_requested_access: "write",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "allow_write",
        side_effect: "none",
        executor_called: true,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: click.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("write", "write", true),
        fixture_id: `electron-browser-permission:${profile.name}:browser_evaluate`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_write_evaluate`,
        tool: "browser_evaluate",
        browser_tool_access: "write",
        browser_requested_access: "write",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "allow_write",
        side_effect: "http_post_received",
        executor_called: true,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: evaluate.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("mixed", "read", true),
        fixture_id: `electron-browser-permission:${profile.name}:browser_console_list`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_mixed_console_read`,
        tool: "browser_console",
        browser_tool_access: "mixed",
        browser_requested_access: "read",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "allow_read",
        side_effect: "none",
        executor_called: true,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: mixedConsoleRead.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("mixed", "write", true),
        fixture_id: `electron-browser-permission:${profile.name}:browser_console_clear`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_mixed_console_write`,
        tool: "browser_console",
        browser_tool_access: "mixed",
        browser_requested_access: "write",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "allow_write",
        side_effect: "browser_runtime_state_cleared",
        executor_called: true,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: mixedConsoleWrite.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("read", "read", false, {
          tabId: `missing-browser-tab-${profile.name}`,
          surfaceId: null,
          authorizationBasis: "browser_tab_authority_rejection",
        }),
        fixture_id: `electron-browser-permission:${profile.name}:browser_snapshot_unknown_tab`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_reject_unknown_tab`,
        tool: "browser_snapshot",
        browser_tool_access: "read",
        browser_requested_access: "read",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "external_origin",
        lifecycle: "reject_invalid_surface",
        side_effect: "none",
        executor_called: false,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: rejected.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
      rows.push({
        ...authFor("read", "read", false, {
          requestedWorkspaceId: `workspace-browser-permission-mismatch-${profile.name}`,
          authorizationBasis: "workspace_session_binding_rejection",
        }),
        fixture_id: `electron-browser-permission:${profile.name}:browser_workspace_scope_mismatch`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_${profile.name}_reject_workspace_scope_mismatch`,
        tool: "browser_snapshot",
        browser_tool_access: "read",
        browser_requested_access: "read",
        surface: "browser_chromium",
        access_profile: profile.name,
        scope: "cross_workspace_scope",
        lifecycle: "reject_scope_mismatch",
        side_effect: "none",
        executor_called: false,
        approval_events: [],
        provider_requests: providerRequestCount,
        request_id: appServer.requestId,
        turn_status: canonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: workspaceMismatch.result.status,
        turn_id: turnId,
        execution_profile: canonical.turn.metadata?.executionProfile || null,
        canonical_item_count: canonical.turn.items?.length || 0,
        terminal_event_sequence: terminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: terminal.traceId || null,
      });
    }

    const workspaceRoot = await mkdtemp(join(stateRoot, "workspace-"));
    const workspaceContext = await createWorkspaceBrowserContext(page, workspaceRoot);
    const workspace = await attachWorkspaceBrowserSurface(page, workspaceContext);
    const workspaceNavigation = await rendererRequest(
      page,
      `/api/browser/tabs/${encodeURIComponent(workspace.tab.tabId)}/navigation`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          action: "url",
          url: `http://127.0.0.1:${fixturePort}/fixture?profile=WorkspaceFullAccess`,
          clientPlatform: "desktop",
        }),
      },
    );
    check("workspace-bound Chromium 导航到本地副作用 fixture", workspaceNavigation.status === 200, JSON.stringify(workspaceNavigation));
    const workspaceTab = workspaceNavigation.json;
    const workspaceBeforeSideEffects = fixture.sideEffects.length;
    const workspaceAppServer = await runAppServerBrowserTurn(page, {
      sessionId: workspace.sessionId,
      workspaceId: workspace.workspaceId,
      scope: "workspace",
      tabId: workspaceTab.tabId,
      profile: { name: "FullAccess", wire: "full_access" },
      turnIndex: profiles.length + 1,
      fixtureLabel: "WorkspaceFullAccess",
      prompt: "真实 Chromium 权限验收 WorkspaceFullAccess",
    });
    check("workspace-bound FullAccess Turn 已接纳", workspaceAppServer.turn?.result?.kind === "accepted", JSON.stringify(workspaceAppServer.turn));
    check(
      "workspace-bound Turn receipt 保留 workspace/session 身份",
      workspaceAppServer.turn?.result?.sessionId === workspace.sessionId
        && workspaceAppServer.turn?.result?.executionProfile === "conversation",
      JSON.stringify(workspaceAppServer.turn),
    );
    const workspaceResponses = new Map(workspaceAppServer.browsers.map((entry) => [entry.case, entry.response]));
    for (const name of ["read_snapshot", "allow_click", "write_evaluate", "mixed_read_console", "mixed_write_console"]) {
      const response = workspaceResponses.get(name);
      check(`workspace-bound Chromium ${name} 获准执行`, response?.result?.status === "completed", JSON.stringify(response));
    }
    const workspaceProviderText = "真实 Chromium 权限验收 WorkspaceFullAccess";
    await waitFor(() => provider.pendingResponses.length === 1, "workspace-bound Provider 请求已到达");
    const workspaceReleased = await fetch(`http://127.0.0.1:${providerPort}/release`, { method: "POST" });
    check("workspace-bound Provider 释放成功", workspaceReleased.status === 204, `HTTP ${workspaceReleased.status}`);
    await waitFor(
      () => fixture.sideEffects.length > workspaceBeforeSideEffects,
      "workspace-bound Chromium 外部 POST 副作用",
      20_000,
    );
    const workspaceSideEffects = fixture.sideEffects.slice(workspaceBeforeSideEffects);
    check("workspace-bound Chromium 只产生一次外部 POST", workspaceSideEffects.length === 1, JSON.stringify(workspaceSideEffects));
    check(
      "workspace-bound 外部副作用归属本次 workspace fixture",
      workspaceSideEffects[0]?.profile === "WorkspaceFullAccess",
      JSON.stringify(workspaceSideEffects),
    );
    const workspaceTurnId = workspaceAppServer.turn?.result?.turnId;
    const workspaceEventTrace = summarizeTurnEvents(workspaceAppServer, workspaceTurnId);
    console.log("workspace canonical event trace", JSON.stringify(workspaceEventTrace));
    const workspaceTerminal = await waitFor(() => {
      flushElectronLogBuffers();
      const record = workspaceTurnId ? backendTimingByTurn.get(workspaceTurnId) : null;
      return record?.stages?.canonical_terminal_published?.count > 0 ? record : null;
    }, "workspace-bound canonical terminal", 45_000);
    const workspaceCanonical = await waitFor(async () => {
      const candidate = await readCanonicalTurn(page, { sessionId: workspace.sessionId, turnId: workspaceTurnId });
      return candidate.turn?.status === "completed" ? candidate : null;
    }, "workspace-bound session/read canonical completed", 20_000);
    check(
      "workspace-bound canonical Turn identity/profile/terminal 一致",
      workspaceCanonical.turn?.turnId === workspaceTurnId
        && workspaceCanonical.turn?.metadata?.executionProfile === "conversation"
        && workspaceTerminal.sessionId === workspace.sessionId
        && Number.isSafeInteger(workspaceTerminal.stages.canonical_terminal_published.first.eventSequence),
      JSON.stringify({ turn: workspaceCanonical.turn, terminal: workspaceTerminal }),
    );
    const workspaceProviderRequestCount = providerRequestCountForUserText(provider, workspaceProviderText);
    check("workspace-bound 同轮 Provider 请求数为 1", workspaceProviderRequestCount === 1, String(workspaceProviderRequestCount));
    const workspaceTurnEvents = eventsForTurn(workspaceAppServer, workspaceTurnId);
    const workspaceLeaseEvent = workspaceTurnEvents.find((event) => event.event_type === "browser.lease.acquired") || null;
    const workspaceLease = workspaceLeaseEvent?.payload || null;
    check(
      "workspace-bound Browser 写操作持有当前 Turn/Session/Workspace/Tab/Surface control lease",
      Boolean(workspaceLease)
        && typeof workspaceLease.lease_id === "string"
        && workspaceLease.turn_id === workspaceTurnId
        && workspaceLease.tab_id === workspaceTab.tabId
        && workspaceLease.surface_id === workspace.tab.surfaceId
        && workspaceLeaseEvent.session_id === workspace.sessionId
        && workspaceLeaseEvent.workspace_id === workspace.workspaceId,
      JSON.stringify(workspaceLeaseEvent),
    );
    const workspaceCases = [
      { name: "read_snapshot", tool: "browser_snapshot", catalogAccess: "read", requestedAccess: "read", sideEffect: "none" },
      { name: "allow_click", tool: "browser_click", catalogAccess: "write", requestedAccess: "write", sideEffect: "none" },
      { name: "write_evaluate", tool: "browser_evaluate", catalogAccess: "write", requestedAccess: "write", sideEffect: "http_post_received" },
      { name: "mixed_read_console", tool: "browser_console", catalogAccess: "mixed", requestedAccess: "read", sideEffect: "none" },
      { name: "mixed_write_console", tool: "browser_console", catalogAccess: "mixed", requestedAccess: "write", sideEffect: "browser_runtime_state_cleared" },
    ];
    for (const [index, operation] of workspaceCases.entries()) {
      const response = workspaceResponses.get(operation.name);
      const callId = `electron-browser-permission-call-${profiles.length + 1}-${index + 1}`;
      const authorization = browserAuthorization({
        browserAccess: operation.catalogAccess,
        requestedAccess: operation.requestedAccess,
        allowed: response?.result?.status === "completed",
        sessionId: workspace.sessionId,
        workspaceId: workspace.workspaceId,
        browserSessionId: workspace.tab.browserSessionId,
        tabId: workspaceTab.tabId,
        turnId: workspaceTurnId,
        surfaceId: workspace.tab.surfaceId,
        scopeKind: "workspace_session",
        leaseEvent: operation.requestedAccess === "write" ? workspaceLeaseEvent : null,
      });
      authorization.authorization_evidence.operation_status = response?.result?.status || null;
      authorization.authorization_evidence.operation_error_code = response?.result?.payload?.error_code || null;
      authorization.authorization_evidence.call_id = callId;
      rows.push({
        ...authorization,
        fixture_id: `electron-browser-permission:workspace:FullAccess:${operation.name}`,
        schema_version: "magi.permission.fixture.v1",
        case: `electron_browser_chromium_workspace_FullAccess_${operation.name}`,
        tool: operation.tool,
        browser_tool_access: operation.catalogAccess,
        browser_requested_access: operation.requestedAccess,
        surface: "browser_chromium",
        access_profile: "FullAccess",
        scope: "workspace_external_origin",
        lifecycle: operation.requestedAccess === "read" ? "allow_read" : "allow_write",
        side_effect: operation.sideEffect,
        executor_called: true,
        approval_events: [],
        provider_requests: workspaceProviderRequestCount,
        request_id: workspaceAppServer.requestId,
        turn_status: workspaceCanonical.turn.status,
        task_status: "not_applicable",
        terminal_source: "canonical_turn_coordinator",
        status: response?.result?.status || "missing",
        turn_id: workspaceTurnId,
        execution_profile: workspaceCanonical.turn.metadata?.executionProfile || null,
        canonical_item_count: workspaceCanonical.turn.items?.length || 0,
        terminal_event_sequence: workspaceTerminal.stages.canonical_terminal_published.first.eventSequence,
        terminal_trace_id: workspaceTerminal.traceId || null,
      });
    }

    const foreignSessionMaterialized = await rendererRequest(page, "/api/session/materialize", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ scope: "personal" }),
    });
    const foreignSessionId = foreignSessionMaterialized.json?.sessionId;
    check(
      "跨 Session Browser 场景拥有独立且已持久化的 Magi Session",
      foreignSessionMaterialized.status === 200
        && typeof foreignSessionId === "string"
        && foreignSessionId.length > 0
        && foreignSessionId !== browser.sessionId,
      JSON.stringify(foreignSessionMaterialized),
    );
    const crossSessionAttempt = await runCrossSessionTabAttempt(page, {
      sessionId: foreignSessionId,
      foreignTabId: browser.tab.tabId,
    });
    check(
      "FullAccess 跨 Session Turn 独立接纳",
      crossSessionAttempt.turn?.result?.kind === "accepted"
        && crossSessionAttempt.turn.result.sessionId === foreignSessionId,
      JSON.stringify(crossSessionAttempt.turn),
    );
    check(
      "FullAccess 复用另一 Session 的真实 Browser Tab 被 fail-closed 拒绝",
      crossSessionAttempt.browser?.result?.status === "failed"
        && crossSessionAttempt.browser.result.payload?.error_code === "browser_tab_scope_mismatch",
      JSON.stringify(crossSessionAttempt.browser),
    );
    await waitFor(
      () => provider.pendingResponses.length === 1,
      "跨 Session Browser 拒绝场景的 Provider 请求已到达",
    );
    const crossSessionProviderRelease = await fetch(
      `http://127.0.0.1:${providerPort}/release`,
      { method: "POST" },
    );
    check(
      "跨 Session Browser 场景 Provider 正常释放",
      crossSessionProviderRelease.status === 204,
      `HTTP ${crossSessionProviderRelease.status}`,
    );
    const crossSessionTurnId = crossSessionAttempt.turn.result.turnId;
    const crossSessionTerminal = await waitFor(() => {
      flushElectronLogBuffers();
      const record = backendTimingByTurn.get(crossSessionTurnId);
      return record?.stages?.canonical_terminal_published?.count > 0 ? record : null;
    }, "跨 Session Browser Turn canonical terminal", 45_000);
    const crossSessionCanonical = await waitFor(async () => {
      const candidate = await readCanonicalTurn(page, {
        sessionId: foreignSessionId,
        turnId: crossSessionTurnId,
      });
      return candidate.turn?.status === "completed" ? candidate : null;
    }, "跨 Session Browser Turn canonical completed", 20_000);
    const crossSessionProviderRequestCount = providerRequestCountForUserText(
      provider,
      crossSessionAttempt.prompt,
    );
    check(
      "跨 Session Browser Turn Provider 请求数为 1",
      crossSessionProviderRequestCount === 1,
      String(crossSessionProviderRequestCount),
    );
    const foreignBrowserSession = await rendererRequest(
      page,
      `/api/browser/sessions/current?scope=personal&sessionId=${encodeURIComponent(foreignSessionId)}`,
    );
    check(
      "跨 Session 拒绝没有把来源 Tab 添加到目标 Browser Session",
      foreignBrowserSession.status === 200
        && Array.isArray(foreignBrowserSession.json?.session?.tabs)
        && foreignBrowserSession.json.session.tabs.length === 0,
      JSON.stringify(foreignBrowserSession.json?.session),
    );
    check(
      "跨 Session 失败 Turn 正常收敛到 canonical completed",
      crossSessionCanonical.turn?.turnId === crossSessionTurnId
        && crossSessionCanonical.turn?.status === "completed"
        && crossSessionTerminal.sessionId === foreignSessionId,
      JSON.stringify({ turn: crossSessionCanonical.turn, terminal: crossSessionTerminal }),
    );
    rows.push({
      authorization_basis: "browser_session_tab_binding_rejection",
      authorization_decision: "deny",
      approval_requirement: "not_required",
      authorization_scope: {
        kind: "cross_session_tab",
        workspace_id: null,
        session_id: foreignSessionId,
        browser_session_id: browser.tab.browserSessionId,
        browser_tab_id: browser.tab.tabId,
        requested_workspace_id: null,
        requested_session_id: browser.sessionId,
        source_session_id: browser.sessionId,
        turn_id: crossSessionTurnId,
        surface_id: browser.tab.surfaceId,
        control_lease: null,
      },
      authorization_evidence: {
        browser_tool_access: "read",
        requested_access: "read",
        decision: "deny",
        basis: "browser_session_tab_binding_rejection",
        control_lease_event: null,
        operation_status: crossSessionAttempt.browser?.result?.status || null,
        operation_error_code: crossSessionAttempt.browser?.result?.payload?.error_code || null,
        call_id: "electron-browser-cross-session-tab-call",
      },
      fixture_id: "electron-browser-permission:FullAccess:browser_cross_session_tab",
      schema_version: "magi.permission.fixture.v1",
      case: "electron_browser_chromium_FullAccess_reject_cross_session_tab",
      tool: "browser_snapshot",
      browser_tool_access: "read",
      browser_requested_access: "read",
      surface: "browser_chromium",
      access_profile: "FullAccess",
      scope: "cross_session_tab",
      lifecycle: "reject_scope_mismatch",
      side_effect: "none",
      executor_called: false,
      approval_events: [],
      provider_requests: crossSessionProviderRequestCount,
      request_id: crossSessionAttempt.requestId,
      turn_status: crossSessionCanonical.turn.status,
      task_status: "not_applicable",
      terminal_source: "canonical_turn_coordinator",
      status: crossSessionAttempt.browser.result.status,
      turn_id: crossSessionTurnId,
      execution_profile: crossSessionCanonical.turn.metadata?.executionProfile || null,
      canonical_item_count: crossSessionCanonical.turn.items?.length || 0,
      terminal_event_sequence: crossSessionTerminal.stages.canonical_terminal_published.first.eventSequence,
      terminal_trace_id: crossSessionTerminal.traceId || null,
    });
    check(
      "所有 Browser 权限行均绑定 request/turn/profile 和 terminal identity",
      rows.length > 0 && rows.every((row) =>
        typeof row.request_id === "string"
        && row.request_id.length > 0
        && typeof row.turn_id === "string"
        && row.turn_id.length > 0
        && typeof row.execution_profile === "string"
        && row.execution_profile.length > 0
        && Number.isSafeInteger(row.terminal_event_sequence)
        && typeof row.terminal_trace_id === "string"
        && row.terminal_trace_id.length > 0),
    );
    check(
      "Browser 权限行同时记录工具目录分类和请求方向",
      rows.every((row) =>
        ["read", "write", "mixed"].includes(row.browser_tool_access)
        && ["read", "write"].includes(row.browser_requested_access)),
    );
    check(
      "真实 Browser Turn 覆盖 Read/Write/Mixed 工具分类与 Read/Write 请求",
      ["read", "write", "mixed"].every((access) =>
        rows.some((row) => row.browser_tool_access === access))
        && ["read", "write"].every((access) =>
          rows.some((row) => row.browser_requested_access === access)),
    );
    check(
      "三个 AccessProfile 与 workspace-bound Chromium 各产生一次真实副作用",
      fixture.sideEffects.length === profiles.length + 1
        && fixture.sideEffects.filter((effect) => effect.profile === "WorkspaceFullAccess").length === 1,
      JSON.stringify(fixture.sideEffects),
    );
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256;
    check(
      "采样期间源码 HEAD、工作区指纹与打包 Electron 可执行文件保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    const evidence = {
      type: "magi_electron_browser_permission_matrix",
      schema_version: "magi.permission.fixture.v1",
      app: appExecutable,
      cdp_port: cdpPort,
      fixture_port: fixturePort,
      provider_port: providerPort,
      checks,
      rows,
      provider_requests: provider.requests.length,
      side_effects: fixture.sideEffects,
      source_identity: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      status: "passed",
    };
    const { writeFile } = await import("node:fs/promises");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({ ...evidence, evidencePath }, null, 2));
  } finally {
    page?.close();
    await stopProcess(electron);
    const daemonPid = await ownedDaemonPid(electron.pid);
    if (daemonPid) {
      try { process.kill(daemonPid, "SIGTERM"); } catch { /* 进程已退出 */ }
    }
    if (provider.server.closeAllConnections) provider.server.closeAllConnections();
    if (fixture.server.closeAllConnections) fixture.server.closeAllConnections();
    await new Promise((resolve) => provider.server.close(resolve));
    await new Promise((resolve) => fixture.server.close(resolve));
    await rm(stateRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
