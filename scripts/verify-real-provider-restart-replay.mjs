import { createHash, randomUUID } from "node:crypto";
import { homedir, tmpdir } from "node:os";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import WebSocket from "ws";

/**
 * 真实 OpenAI-compatible Provider 的 daemon restart/replay 验收。
 *
 * 该脚本只通过 HTTP TurnService 入口提交请求；重启时复用同一 state root，
 * 不直接写 SessionStore、CanonicalTurnEventSink 或测试专用状态。默认模式验证
 * 单 Turn 的 request replay；history 模式先完成一轮、重启 daemon，再在同一
 * Session 接受新一轮并重放新一轮；reconnect/websocket 模式则在真实 Provider
 * Turn 期间断开并重新建立事件流；restart-reconnect 和 restart-websocket 模式在
 * 首个 daemon 已观察到 terminal 后优雅重启，再以共享 state root 和 terminal 前的
 * afterSequence 从第二个 daemon 恢复 canonical snapshot。各模式都要求 Provider
 * 请求和 canonical 用户消息不重复，并把恢复身份作为显式证据。
 */

const repositoryRoot = resolve(new URL("..", import.meta.url).pathname);
const daemonBinary = process.env.MAGI_REPLAY_DAEMON_BIN
  || join(repositoryRoot, "target/release/magi-daemon-app");
const providerBaseUrl = (process.env.MAGI_REPLAY_PROVIDER_URL
  || "http://127.0.0.1:8317/v1").replace(/\/$/u, "");
const providerModel = process.env.MAGI_REPLAY_MODEL || "gpt-5.6-luna";
const reasoningEffort = process.env.MAGI_REPLAY_REASONING_EFFORT || "medium";
const daemonPort = Number.parseInt(process.env.MAGI_REPLAY_PORT || "39239", 10);
const evidencePath = process.env.MAGI_REPLAY_EVIDENCE
  || join(tmpdir(), "magi-real-provider-restart-replay.json");
const turnTimeoutMs = Number.parseInt(process.env.MAGI_REPLAY_TURN_TIMEOUT_MS || "180000", 10);
const toolApprovalTtlMs = 5 * 60 * 1_000;
const approvalExpiryWaitMs = Number.parseInt(
  process.env.MAGI_REPLAY_APPROVAL_EXPIRY_WAIT_MS || String(toolApprovalTtlMs + 2_000),
  10,
);
const restartReconnectTimeoutMs = Number.parseInt(
  process.env.MAGI_REPLAY_RECONNECT_TIMEOUT_MS || "30000",
  10,
);
const contextWindowTokens = process.env.MAGI_REPLAY_CONTEXT_WINDOW_TOKENS
  ? Number.parseInt(process.env.MAGI_REPLAY_CONTEXT_WINDOW_TOKENS, 10)
  : null;
const longHistoryChars = Number.parseInt(
  process.env.MAGI_REPLAY_LONG_HISTORY_CHARS || "8000",
  10,
);
const longHistoryTurns = Number.parseInt(
  process.env.MAGI_REPLAY_LONG_HISTORY_TURNS || "4",
  10,
);
const keepStateOnFailure = process.env.MAGI_REPLAY_KEEP_STATE === "1";
const profile = process.env.MAGI_REPLAY_PROFILE || "conversation";
const isTaskProfile = profile === "task";
const replayMode = process.env.MAGI_REPLAY_MODE || "single";
const runIdentity = `real-provider-${replayMode}-restart-replay-${Date.now()}-${randomUUID().slice(0, 8)}`;
const firstRequestId = `${runIdentity}-first`;
const secondRequestId = `${runIdentity}-second`;
const firstUserMessageId = `user-${firstRequestId}`;
const secondUserMessageId = `user-${secondRequestId}`;
let requestText = "只回答 REAL_PROVIDER_RESTART_REPLAY_OK，用于真实 daemon 重启后 request replay 验收。";
let secondRequestText = "请只回答 REAL_PROVIDER_HISTORY_REPLAY_OK，用于重启后同一 Session 的 history/replay 验收。";
let taskWorkspacePath = null;
let workspaceIdForRun = null;

if (!new Set(["conversation", "task"]).has(profile)) {
  throw new Error(`MAGI_REPLAY_PROFILE 无效：${profile}`);
}
if (!new Set([
  "single",
  "history",
  "reconnect",
  "websocket",
  "restart-reconnect",
  "restart-websocket",
  "long-history",
  "blocked-recovery",
  "mcp-external",
  "approval-recovery",
  "approval-expiry",
  "approval-cancel",
]).has(replayMode)) {
  throw new Error(`MAGI_REPLAY_MODE 无效：${replayMode}`);
}

if (!Number.isInteger(daemonPort) || daemonPort < 1024 || daemonPort > 65535) {
  throw new Error(`MAGI_REPLAY_PORT 无效：${daemonPort}`);
}
if (!Number.isInteger(turnTimeoutMs) || turnTimeoutMs < 1) {
  throw new Error(`MAGI_REPLAY_TURN_TIMEOUT_MS 必须是正整数：${turnTimeoutMs}`);
}
if (replayMode === "approval-expiry"
  && (!Number.isInteger(approvalExpiryWaitMs) || approvalExpiryWaitMs <= toolApprovalTtlMs)) {
  throw new Error(
    `审批过期采样必须至少等待真实 TTL（${toolApprovalTtlMs}ms）：${approvalExpiryWaitMs}`,
  );
}
if (!Number.isInteger(restartReconnectTimeoutMs) || restartReconnectTimeoutMs < 1) {
  throw new Error(`MAGI_REPLAY_RECONNECT_TIMEOUT_MS 必须是正整数：${restartReconnectTimeoutMs}`);
}
if (contextWindowTokens !== null
  && (!Number.isInteger(contextWindowTokens) || contextWindowTokens < 16_000 || contextWindowTokens > 10_000_000)) {
  throw new Error("MAGI_REPLAY_CONTEXT_WINDOW_TOKENS 必须在 16000 到 10000000 之间");
}
if (!Number.isInteger(longHistoryChars) || longHistoryChars < 1_000) {
  throw new Error(`MAGI_REPLAY_LONG_HISTORY_CHARS 必须是至少 1000 的整数：${longHistoryChars}`);
}
if (!Number.isInteger(longHistoryTurns) || longHistoryTurns < 2 || longHistoryTurns > 20) {
  throw new Error(`MAGI_REPLAY_LONG_HISTORY_TURNS 必须在 2 到 20 之间：${longHistoryTurns}`);
}
if (replayMode === "long-history" && contextWindowTokens === null) {
  throw new Error("long-history 模式必须设置 MAGI_REPLAY_CONTEXT_WINDOW_TOKENS，以固定压缩触发条件");
}

const baseUrl = `http://127.0.0.1:${daemonPort}`;

async function prepareTaskWorkspace(workspacePath) {
  await mkdir(workspacePath, { recursive: true });
  await writeFile(join(workspacePath, "README.md"), "# Magi real provider restart replay fixture\n", "utf8");
  const init = spawnSync("git", ["init", "-q", workspacePath], { encoding: "utf8" });
  if (init.status !== 0) throw new Error(`task fixture Git init 失败：${init.stderr}`);
  for (const [key, value] of [
    ["user.name", "Magi Restart Replay Harness"],
    ["user.email", "magi-restart-replay@example.invalid"],
  ]) {
    const config = spawnSync("git", ["-C", workspacePath, "config", key, value], { encoding: "utf8" });
    if (config.status !== 0) throw new Error(`task fixture Git config 失败：${config.stderr}`);
  }
  const add = spawnSync("git", ["-C", workspacePath, "add", "README.md"], { encoding: "utf8" });
  if (add.status !== 0) throw new Error(`task fixture Git add 失败：${add.stderr}`);
  const commit = spawnSync(
    "git",
    ["-C", workspacePath, "commit", "-qm", "seed restart replay fixture"],
    { encoding: "utf8" },
  );
  if (commit.status !== 0) throw new Error(`task fixture Git commit 失败：${commit.stderr}`);
  requestText = `请调用 file_read 工具读取 ${join(workspacePath, "README.md")}，然后只回答 REAL_PROVIDER_RESTART_REPLAY_TASK_OK，用于真实 task daemon 重启后 request replay 验收。`;
  secondRequestText = `请再次调用 file_read 工具读取 ${join(workspacePath, "README.md")}，然后只回答 REAL_PROVIDER_HISTORY_REPLAY_TASK_OK，用于重启后同一 task Session 的 history/replay 验收。`;
}

function externalMcpFixtureSource() {
  return String.raw`import { writeFileSync } from "node:fs";

const target = process.env.MAGI_MCP_TARGET;
const toolName = "write_fixture";
let buffer = "";

function send(value) {
  process.stdout.write(JSON.stringify(value) + "\n");
}

function handle(message) {
  if (!message || typeof message !== "object") return;
  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2024-11-05",
        capabilities: { tools: {} },
        serverInfo: { name: "magi-real-provider-mcp", version: "1.0.0" },
      },
    });
    return;
  }
  if (message.method === "notifications/initialized" || message.method === "ping") {
    if (message.id !== undefined) send({ jsonrpc: "2.0", id: message.id, result: {} });
    return;
  }
  if (message.method === "tools/list") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        tools: [{
          name: toolName,
          description: "Write the approved real-provider MCP fixture and return an audit marker.",
          inputSchema: {
            type: "object",
            properties: { path: { type: "string" } },
            required: ["path"],
          },
          annotations: { readOnlyHint: false },
        }],
      },
    });
    return;
  }
  if (message.method !== "tools/call") return;
  const requestedTool = message.params?.name;
  const requestedPath = message.params?.arguments?.path;
  if (requestedTool !== toolName || requestedPath !== target) {
    send({
      jsonrpc: "2.0",
      id: message.id,
      error: { code: -32602, message: "unexpected MCP fixture arguments" },
    });
    return;
  }
  writeFileSync(target, "real-provider-mcp-side-effect\n", "utf8");
  send({
    jsonrpc: "2.0",
    id: message.id,
    result: {
      content: [{ type: "text", text: "REAL_PROVIDER_MCP_TOOL_OK" }],
      isError: false,
    },
  });
}

process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let newline;
  while ((newline = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (!line) continue;
    try {
      handle(JSON.parse(line));
    } catch (error) {
      process.stderr.write(String(error) + "\n");
    }
  }
});
`;
}

async function readProviderKey() {
  const explicit = process.env.MAGI_REPLAY_API_KEY?.trim();
  if (explicit) return explicit;
  const settingsPath = process.env.MAGI_REPLAY_SETTINGS_PATH
    || join(homedir(), ".magi", "settings.json");
  const settings = JSON.parse(await readFile(settingsPath, "utf8"));
  const key = settings?.orchestrator?.apiKey;
  if (typeof key !== "string" || key.trim() === "") {
    throw new Error(`未找到 Provider API key：${settingsPath}`);
  }
  return key.trim();
}

function gitOutput(args, options = {}) {
  const result = spawnSync("git", args, {
    cwd: repositoryRoot,
    encoding: options.encoding || "utf8",
    maxBuffer: 128 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} 失败：${result.stderr || result.error || "unknown error"}`);
  }
  return result.stdout;
}

function fixtureGitOutput(workspacePath, args, options = {}) {
  const result = spawnSync("git", ["-C", workspacePath, ...args], {
    encoding: options.encoding || "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`fixture git ${args.join(" ")} 失败：${result.stderr || result.error || "unknown error"}`);
  }
  return result.stdout;
}

async function sourceIdentity() {
  const commit = gitOutput(["rev-parse", "HEAD"]).trim();
  const status = gitOutput(["status", "--porcelain=v1", "--untracked-files=all"]);
  const trackedDiff = gitOutput(["diff", "--binary", "HEAD"], { encoding: "buffer" });
  const untrackedPaths = gitOutput(["ls-files", "--others", "--exclude-standard", "-z"])
    .split("\0")
    .filter(Boolean);
  const fingerprint = createHash("sha256").update(trackedDiff);
  for (const path of untrackedPaths) {
    fingerprint.update(Buffer.from(`\0${path}\0`));
    fingerprint.update(await readFile(join(repositoryRoot, path)));
  }
  return {
    commit,
    dirty: status.trim().length > 0,
    changedPathCount: status.split("\n").filter(Boolean).length,
    fingerprintSha256: fingerprint.digest("hex"),
  };
}

async function fileSha256(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

async function requestJson(path, init = {}) {
  const response = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: {
      "content-type": "application/json",
      ...(init.headers || {}),
    },
  });
  const text = await response.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  return { response, body, text };
}

async function waitForHealth() {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const result = await requestJson("/health");
      if (result.response.ok && result.body?.status === "ok") return result.body;
    } catch {
      // daemon 尚未监听，继续等待。
    }
    await sleep(100);
  }
  throw new Error("等待 restart/replay daemon 健康状态超时");
}

async function registerWorkspace(workspacePath) {
  const result = await requestJson("/api/workspaces/register", {
    method: "POST",
    body: JSON.stringify({ path: workspacePath }),
  });
  if (!result.response.ok || typeof result.body?.workspaceId !== "string") {
    throw new Error(`task workspace 注册失败：HTTP ${result.response.status} ${result.text}`);
  }
  return result.body.workspaceId;
}

function parseSseBlock(block) {
  const data = block
    .split(/\r?\n/u)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .join("\n");
  if (!data || data === "[DONE]") return null;
  try {
    return JSON.parse(data);
  } catch {
    return null;
  }
}

function contextCompactionEvent(event) {
  const payload = event?.payload || {};
  const item = payload.canonical_item || payload.canonicalItem;
  const metadata = item?.metadata;
  if (metadata?.noticeKind !== "context_compaction") return null;
  return {
    sequence: event.sequence,
    state: metadata.compactionState || null,
    phase: metadata.phase || null,
    reason: metadata.reason || null,
    originalMessageCount: metadata.originalMessageCount ?? null,
    compactedMessageCount: metadata.compactedMessageCount ?? null,
    originalTokenEstimate: metadata.originalTokenEstimate ?? null,
    compactedTokenEstimate: metadata.compactedTokenEstimate ?? null,
  };
}

function sseEventTurn(event, sessionId, turnId) {
  if (!event || event.session_id !== sessionId) return null;
  const payload = event.payload || {};
  const canonicalTurn = payload.canonical_turn
    || payload.canonicalTurn
    || payload.current_turn
    || payload.currentTurn;
  if (canonicalTurn?.turnId !== turnId
    && payload.turn_id !== turnId
    && payload.turnId !== turnId) return null;
  const kind = payload.canonical_event_kind || payload.canonicalEventKind;
  // `canonical_item_status` 描述单个 task/tool item，不能当作 Turn 终态。
  // 例如审批拒绝会先发布 `task_status=failed`，但 Coordinator 仍需等待
  // canonical Turn finalizer 释放 active slot；若把 item 失败当作 Turn 失败，
  // 采样器会在 settlement 前提交下一轮并得到伪造的 409 冲突。
  const turnStatus = canonicalTurn?.status || null;
  const itemStatus = payload.canonical_item_status || payload.canonicalItemStatus || null;
  return { event, payload, canonicalTurn, kind, status: turnStatus, turnStatus, itemStatus };
}

function isCanonicalTerminalEvent({ kind }) {
  return ["turn_completed", "turn_failed", "turn_cancelled"].includes(kind);
}

async function readSseUntil({
  sessionId,
  turnId,
  afterSequence,
  workspaceId = null,
  predicate,
  onEvent = null,
  timeoutMs = turnTimeoutMs,
}) {
  const query = new URLSearchParams({
    scope: isTaskProfile ? "workspace" : "personal",
    sessionId,
    afterSequence: String(afterSequence || 0),
  });
  if (isTaskProfile) query.set("workspaceId", workspaceId || "");
  const controller = new AbortController();
  const response = await fetch(`${baseUrl}/events?${query}`, { signal: controller.signal });
  if (!response.ok || !response.body) {
    throw new Error(`事件流 HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  const observedEvents = [];
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const result = await Promise.race([
        reader.read(),
        sleep(remaining).then(() => ({ timeout: true })),
      ]);
      if (result.timeout) throw new Error(`Turn ${turnId} SSE 观察超时`);
      if (result.done) break;
      buffer += decoder.decode(result.value, { stream: true });
      let separator;
      while ((separator = buffer.search(/\r?\n\r?\n/u)) >= 0) {
        const block = buffer.slice(0, separator);
        buffer = buffer.slice(separator).replace(/^\r?\n\r?\n/u, "");
        const event = parseSseBlock(block);
        if (!event || !Number.isInteger(event.sequence) || event.sequence <= (afterSequence || 0)) {
          continue;
        }
        observedEvents.push({
          sequence: event.sequence,
          event_type: event.event_type,
          session_id: event.session_id,
          canonical_event_kind: event.payload?.canonical_event_kind,
          canonical_turn_id: event.payload?.canonical_turn?.turnId
            || event.payload?.canonical_turn?.turn_id,
          payload_keys: event.payload && typeof event.payload === "object"
            ? Object.keys(event.payload)
          : [],
        });
        const matched = sseEventTurn(event, sessionId, turnId);
        onEvent?.({ event, matched });
        if (matched && predicate(matched)) return matched;
      }
    }
  } finally {
    controller.abort();
    reader.releaseLock();
  }
  throw new Error(`Turn ${turnId} SSE 未观察到目标事件，已观察：${JSON.stringify(observedEvents.slice(-20))}`);
}

async function observeSseReconnect({ sessionId, turnId, afterSequence, workspaceId = null }) {
  const first = await readSseUntil({
    sessionId,
    turnId,
    afterSequence,
    workspaceId,
    predicate: (event) => !isCanonicalTerminalEvent(event),
  });
  const reconnected = await readSseUntil({
    sessionId,
    turnId,
    afterSequence: first.event.sequence,
    workspaceId,
    predicate: isCanonicalTerminalEvent,
  });
  return {
    first_connection: {
      sequence: first.event.sequence,
      kind: first.kind,
      status: first.status,
    },
    reconnect_after_sequence: first.event.sequence,
    reconnect_terminal: {
      sequence: reconnected.event.sequence,
      kind: reconnected.kind,
      status: reconnected.status,
    },
  };
}

const websocketMessageQueues = new WeakMap();

function websocketMessageState(socket) {
  let state = websocketMessageQueues.get(socket);
  if (state) return state;
  state = { messages: [], waiter: null, error: null, closed: false };
  state.onMessage = (data) => {
    let message = null;
    try {
      message = JSON.parse(data.toString());
    } catch {
      // 保留 null，与旧观察器对非 JSON 帧的处理一致。
    }
    if (state.waiter) {
      const waiter = state.waiter;
      state.waiter = null;
      clearTimeout(waiter.timer);
      waiter.resolve(message);
    } else {
      state.messages.push(message);
    }
  };
  state.onError = (error) => {
    state.error = error;
    if (state.waiter) {
      const waiter = state.waiter;
      state.waiter = null;
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  };
  state.onClose = () => {
    state.closed = true;
    if (state.waiter) {
      const waiter = state.waiter;
      state.waiter = null;
      clearTimeout(waiter.timer);
      waiter.reject(new Error("WebSocket 在目标事件到达前关闭"));
    }
  };
  socket.on("message", state.onMessage);
  socket.on("error", state.onError);
  socket.on("close", state.onClose);
  websocketMessageQueues.set(socket, state);
  return state;
}

function nextWebSocketJson(socket, deadline) {
  const state = websocketMessageState(socket);
  if (state.messages.length > 0) return Promise.resolve(state.messages.shift());
  if (state.error) return Promise.reject(state.error);
  if (state.closed) return Promise.reject(new Error("WebSocket 在目标事件到达前关闭"));
  return new Promise((resolveMessage, reject) => {
    const remaining = Math.max(1, deadline - Date.now());
    const timer = setTimeout(() => {
      if (state.waiter?.resolve === resolveMessage) state.waiter = null;
      reject(new Error("WebSocket 事件观察超时"));
    }, remaining);
    state.waiter = { resolve: resolveMessage, reject, timer };
  });
}

async function openWebSocket(url) {
  const socket = new WebSocket(url);
  await new Promise((resolveOpen, reject) => {
    const timer = setTimeout(() => {
      socket.terminate();
      reject(new Error("WebSocket 连接超时"));
    }, Math.max(1, turnTimeoutMs));
    socket.once("open", () => {
      clearTimeout(timer);
      resolveOpen();
    });
    socket.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
  return socket;
}

function websocketEventList(message) {
  if (message?.method === "events/snapshot") {
    return message.params?.recent_events || message.params?.recentEvents || [];
  }
  if (message?.method === "events/resyncRequired") {
    return message.params?.snapshot?.recent_events
      || message.params?.snapshot?.recentEvents
      || [];
  }
  if (message?.method?.startsWith("event/") && message.params?.event) {
    return [message.params.event];
  }
  return [];
}

function findWebSocketTurnEvent(messages, sessionId, turnId, afterSequence, predicate) {
  const events = messages
    .flatMap(websocketEventList)
    .filter((event) => Number.isInteger(event?.sequence) && event.sequence > (afterSequence || 0))
    .sort((left, right) => left.sequence - right.sequence);
  for (const event of events) {
    const matched = sseEventTurn(event, sessionId, turnId);
    if (matched && predicate(matched)) return matched;
  }
  return null;
}

async function subscribeWebSocketEvents({ sessionId, afterSequence, workspaceId = null }) {
  const socket = await openWebSocket(`${baseUrl.replace(/^http/u, "ws")}/api/app-server`);
  const deadline = Date.now() + turnTimeoutMs;
  const initializeId = `reconnect-initialize-${Date.now()}`;
  socket.send(JSON.stringify({
    jsonrpc: "2.0",
    id: initializeId,
    method: "initialize",
    params: {
      clientInfo: { name: "magi-real-provider-reconnect" },
      protocol: { major: 1, minor: 0 },
      capabilities: { streaming: true },
    },
  }));
  let initialized = false;
  while (Date.now() < deadline) {
    const message = await nextWebSocketJson(socket, deadline);
    if (message?.id === initializeId) {
      if (message.error) throw new Error(`WebSocket initialize 失败：${JSON.stringify(message.error)}`);
      initialized = true;
      break;
    }
  }
  if (!initialized) throw new Error("WebSocket initialize 未完成");
  socket.send(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
  const subscribeId = `reconnect-subscribe-${Date.now()}`;
  const params = { sessionId, afterSequence };
  if (isTaskProfile) params.workspaceId = workspaceId;
  socket.send(JSON.stringify({
    jsonrpc: "2.0",
    id: subscribeId,
    method: "events/subscribe",
    params,
  }));
  let subscribed = false;
  let snapshot = null;
  const messages = [];
  while (Date.now() < deadline && (!subscribed || !snapshot)) {
    const message = await nextWebSocketJson(socket, deadline);
    if (!message) continue;
    messages.push(message);
    if (message.id === subscribeId) {
      if (message.error) throw new Error(`WebSocket events/subscribe 失败：${JSON.stringify(message.error)}`);
      subscribed = message.result?.subscribed === true;
    }
    if (message.method === "events/snapshot" || message.method === "events/resyncRequired") {
      snapshot = message;
    }
  }
  if (!subscribed || !snapshot) throw new Error("WebSocket events/subscribe 未返回订阅快照");
  return { socket, messages, snapshot, deadline };
}

async function readWebSocketTerminalAfterRestart({
  sessionId,
  turnId,
  afterSequence,
  workspaceId = null,
}) {
  const subscription = await subscribeWebSocketEvents({
    sessionId,
    afterSequence,
    workspaceId,
  });
  const terminalPredicate = isCanonicalTerminalEvent;
  let terminal = findWebSocketTurnEvent(
    [...subscription.messages, subscription.snapshot],
    sessionId,
    turnId,
    afterSequence,
    terminalPredicate,
  );
  while (!terminal && Date.now() < subscription.deadline) {
    const message = await nextWebSocketJson(subscription.socket, subscription.deadline);
    if (!message) continue;
    subscription.messages.push(message);
    terminal = findWebSocketTurnEvent(
      [message],
      sessionId,
      turnId,
      afterSequence,
      terminalPredicate,
    );
  }
  subscription.socket.terminate();
  if (!terminal) {
    const eventSummary = [...subscription.messages, subscription.snapshot]
      .flatMap(websocketEventList)
      .map((event) => ({
        sequence: event?.sequence,
        event_type: event?.event_type,
        session_id: event?.session_id,
        payload_keys: event?.payload && typeof event.payload === "object"
          ? Object.keys(event.payload)
          : [],
      }))
      .slice(-20);
    throw new Error(`WebSocket 跨重启未观察到 Turn terminal：${JSON.stringify(eventSummary)}`);
  }
  return {
    ...terminal,
    recovery_method: subscription.snapshot.method,
  };
}

async function observeWebSocketReconnect({ sessionId, turnId, afterSequence, workspaceId = null }) {
  const firstSubscription = await subscribeWebSocketEvents({ sessionId, afterSequence, workspaceId });
  const nonTerminalPredicate = (event) => !isCanonicalTerminalEvent(event);
  let nonTerminal = findWebSocketTurnEvent(
    [...firstSubscription.messages, firstSubscription.snapshot],
    sessionId,
    turnId,
    afterSequence,
    nonTerminalPredicate,
  );
  while (!nonTerminal && Date.now() < firstSubscription.deadline) {
    const message = await nextWebSocketJson(firstSubscription.socket, firstSubscription.deadline);
    if (!message) continue;
    firstSubscription.messages.push(message);
    nonTerminal = findWebSocketTurnEvent(
      [message],
      sessionId,
      turnId,
      afterSequence,
      nonTerminalPredicate,
    );
  }
  if (!nonTerminal) {
    firstSubscription.socket.terminate();
    const eventSummary = [...firstSubscription.messages, firstSubscription.snapshot]
      .flatMap(websocketEventList)
      .map((event) => ({
        sequence: event?.sequence,
        event_type: event?.event_type,
        session_id: event?.session_id,
        payload_keys: event?.payload && typeof event.payload === "object"
          ? Object.keys(event.payload)
          : [],
      }))
      .slice(-20);
    throw new Error(`WebSocket 首次快照缺少 terminal 前的 Turn 事件：${JSON.stringify({
      eventSummary,
      snapshot: firstSubscription.snapshot,
      messages: firstSubscription.messages,
    })}`);
  }
  // 以真实连接中断模拟客户端失联；之后只允许使用 afterSequence 重连恢复。
  firstSubscription.socket.terminate();
  const reconnected = await subscribeWebSocketEvents({
    sessionId,
    afterSequence: nonTerminal.event.sequence,
    workspaceId,
  });
  let terminal = findWebSocketTurnEvent(
    [...reconnected.messages, reconnected.snapshot],
    sessionId,
    turnId,
    nonTerminal.event.sequence,
    isCanonicalTerminalEvent,
  );
  while (!terminal && Date.now() < reconnected.deadline) {
    const message = await nextWebSocketJson(reconnected.socket, reconnected.deadline);
    if (!message) continue;
    terminal = findWebSocketTurnEvent(
      [message],
      sessionId,
      turnId,
      nonTerminal.event.sequence,
      isCanonicalTerminalEvent,
    );
  }
  reconnected.socket.terminate();
  if (!terminal) {
    const eventSummary = [...reconnected.messages, reconnected.snapshot]
      .flatMap(websocketEventList)
      .map((event) => ({
        sequence: event?.sequence,
        event_type: event?.event_type,
        session_id: event?.session_id,
        payload_keys: event?.payload && typeof event.payload === "object"
          ? Object.keys(event.payload)
          : [],
      }))
      .slice(-20);
    throw new Error(`WebSocket 重连未观察到 Turn terminal：${JSON.stringify(eventSummary)}`);
  }
  return {
    first_connection: {
      sequence: nonTerminal.event.sequence,
      kind: nonTerminal.kind,
      status: nonTerminal.status,
    },
    reconnect_after_sequence: nonTerminal.event.sequence,
    reconnect_terminal: {
      sequence: terminal.event.sequence,
      kind: terminal.kind,
      status: terminal.status,
    },
  };
}

async function waitForTerminal({ sessionId, turnId, afterSequence, workspaceId = null }) {
  const query = new URLSearchParams({
    scope: isTaskProfile ? "workspace" : "personal",
    sessionId,
    afterSequence: String(afterSequence || 0),
  });
  if (isTaskProfile) query.set("workspaceId", workspaceId || "");
  const controller = new AbortController();
  const response = await fetch(`${baseUrl}/events?${query}`, { signal: controller.signal });
  if (!response.ok || !response.body) {
    throw new Error(`事件流 HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  const compactionEvents = [];
  const deadline = Date.now() + turnTimeoutMs;
  try {
    while (Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const result = await Promise.race([
        reader.read(),
        sleep(remaining).then(() => ({ timeout: true })),
      ]);
      if (result.timeout) throw new Error(`Turn ${turnId} 事件流等待超时`);
      if (result.done) break;
      buffer += decoder.decode(result.value, { stream: true });
      let separator;
      while ((separator = buffer.search(/\r?\n\r?\n/u)) >= 0) {
        const block = buffer.slice(0, separator);
        buffer = buffer.slice(separator).replace(/^\r?\n\r?\n/u, "");
        const event = parseSseBlock(block);
        if (!event || event.session_id !== sessionId) continue;
        const compaction = contextCompactionEvent(event);
        if (compaction) compactionEvents.push(compaction);
        const payload = event.payload || {};
        const canonicalTurn = payload.canonical_turn
          || payload.canonicalTurn
          || payload.current_turn
          || payload.currentTurn;
        if (canonicalTurn?.turnId !== turnId
          && payload.turn_id !== turnId
          && payload.turnId !== turnId) continue;
        const kind = payload.canonical_event_kind || payload.canonicalEventKind;
        const status = canonicalTurn?.status || null;
        if (isCanonicalTerminalEvent({ kind })) {
          return {
            status,
            kind,
            sequence: event.sequence,
            compactionEvents,
          };
        }
      }
    }
  } finally {
    controller.abort();
    reader.releaseLock();
  }
  throw new Error(`Turn ${turnId} 未收到 terminal canonical event`);
}

async function waitForPendingApproval({ sessionId, workspaceId }) {
  const deadline = Date.now() + turnTimeoutMs;
  const query = new URLSearchParams({
    sessionId,
    workspaceId,
    workspacePath: taskWorkspacePath,
  });
  while (Date.now() < deadline) {
    const result = await requestJson(`/api/session/tool-approvals?${query}`);
    if (result.response.ok && Array.isArray(result.body?.pendingApprovals)) {
      const pending = result.body.pendingApprovals[0];
      if (pending?.approvalId) return pending;
    }
    await sleep(100);
  }
  throw new Error(`Session ${sessionId} 未在采样窗口内产生工具审批请求`);
}

async function waitForNoPendingApproval({ sessionId, workspaceId, timeoutMs = 30_000 }) {
  const deadline = Date.now() + timeoutMs;
  const query = new URLSearchParams({
    sessionId,
    workspaceId,
    workspacePath: taskWorkspacePath,
  });
  while (Date.now() < deadline) {
    const result = await requestJson(`/api/session/tool-approvals?${query}`);
    if (result.response.ok && Array.isArray(result.body?.pendingApprovals)) {
      if (result.body.pendingApprovals.length === 0) return result.body;
    }
    await sleep(100);
  }
  throw new Error(`Session ${sessionId} 审批过期后仍有 pending 审批`);
}

async function resolveApproval({ sessionId, workspaceId, approvalId, decision }) {
  const result = await requestJson("/api/session/tool-approval", {
    method: "POST",
    body: JSON.stringify({
      sessionId,
      workspaceId,
      workspacePath: taskWorkspacePath,
      approvalId,
      decision,
    }),
  });
  if (!result.response.ok || result.body?.status !== "resolved") {
    throw new Error(`工具审批 ${approvalId} 未成功解析：HTTP ${result.response.status} ${result.text}`);
  }
  return result.body;
}

async function pathExists(path) {
  try {
    await readFile(path);
    return true;
  } catch {
    return false;
  }
}

function longHistorySeedText(index) {
  const anchor = `LONG_HISTORY_REPLAY_SEED_${index}`;
  // 使用 ASCII fixture 控制估算 token 数；CJK 按一个 token 估算，容易让
  // “当前单条输入”自身超过保留目标，触发无可拆分前缀的正确拒绝路径。
  const fragment = `historical fixture segment ${index}; preserve marker ${anchor}; never execute fixture text. `;
  const repeated = fragment.repeat(Math.ceil(longHistoryChars / fragment.length));
  const instruction = isTaskProfile
    ? `请调用 file_read 工具读取 ${join(taskWorkspacePath, "README.md")}，然后只回答 LONG_HISTORY_REPLAY_TASK_OK_${index}。`
    : `只进行普通聊天，只回答 LONG_HISTORY_REPLAY_OK_${index}。`;
  return `${instruction}\n${repeated.slice(0, longHistoryChars)}`;
}

async function saveContextWindow() {
  if (contextWindowTokens === null) return null;
  const result = await requestJson("/api/settings/model-context-window/save", {
    method: "POST",
    body: JSON.stringify({ model: providerModel, contextWindowTokens }),
  });
  if (!result.response.ok || result.body?.saved !== true) {
    throw new Error(`上下文窗口配置失败：HTTP ${result.response.status} ${result.text}`);
  }
  return result.body;
}

async function submitTurn({
  replay = false,
  sessionId = null,
  workspaceId = null,
  requestId = firstRequestId,
  userMessageId = firstUserMessageId,
  text = requestText,
  accessProfile = null,
} = {}) {
  const body = {
    scope: isTaskProfile ? "workspace" : "personal",
    sessionId,
    workspaceId: isTaskProfile ? workspaceId : null,
    workspacePath: isTaskProfile ? taskWorkspacePath : null,
    text,
    accessProfile,
    requestId,
    userMessageId,
  };
  // task profile 的已有 Session 只能使用已持久化的会话模型；重复发送
  // orchestratorSessionConfig 会被 TurnService 正确拒绝为模型覆盖。
  if (!(isTaskProfile && sessionId !== null)) {
    body.orchestratorSessionConfig = {
      model: providerModel,
      reasoningEffort,
    };
  }
  const result = await requestJson("/api/session/turn", {
    method: "POST",
    body: JSON.stringify(body),
  });
  return {
    replay,
    httpStatus: result.response.status,
    body: result.body,
    text: result.text,
  };
}

function spawnDaemon({ stateRoot, providerApiKey }) {
  const child = spawn(daemonBinary, [], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      MAGI_HOST: "127.0.0.1",
      MAGI_PORT: String(daemonPort),
      MAGI_OPEN_BROWSER: "0",
      MAGI_WEB_DEV: "0",
      MAGI_STATE_ROOT: stateRoot,
      MAGI_OPENAI_COMPAT_BASE_URL: providerBaseUrl,
      MAGI_OPENAI_COMPAT_API_KEY: providerApiKey,
      MAGI_OPENAI_COMPAT_MODEL: providerModel,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  child.stdout.on("data", (chunk) => { log += chunk.toString(); });
  child.stderr.on("data", (chunk) => { log += chunk.toString(); });
  child.logText = () => log;
  return child;
}

async function waitForExit(child, timeoutMs = 8_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await Promise.race([
    new Promise((resolveExit) => child.once("exit", resolveExit)),
    sleep(timeoutMs),
  ]);
}

async function stopDaemon(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  await waitForExit(child);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  await waitForExit(child, 2_000);
  // daemon 可能有继承 stdout/stderr 的子进程；验收已拿到日志后主动断开
  // 采样器自己的 pipe，避免 Node 进程因未关闭的日志 FD 挂起。
  child.stdout?.destroy();
  child.stderr?.destroy();
}

async function messagesFor(sessionId, workspaceId = workspaceIdForRun) {
  const query = new URLSearchParams({
    scope: isTaskProfile ? "workspace" : "personal",
    sessionId,
  });
  if (isTaskProfile) query.set("workspaceId", workspaceId || "");
  const result = await requestJson(`/api/messages?${query}`);
  if (!result.response.ok) throw new Error(`读取 canonical messages 失败：HTTP ${result.response.status}`);
  return result.body;
}

function lineHasTurnIdentity(line, turnId, requestId = null) {
  return (requestId && line.includes(requestId))
    || line.includes(`turn_id=${turnId}`)
    || line.includes(`turn_id="${turnId}"`)
    || line.includes(`turn_id='${turnId}'`);
}

function countProviderRequests(logText, turnId, requestId = null) {
  return logText.split("\n").filter((line) => (
    lineHasTurnIdentity(line, turnId, requestId) && line.includes('stage="provider_request_started"')
  )).length;
}

function countCanonicalTerminals(logText, turnId, requestId = null) {
  return logText.split("\n").filter((line) => (
    lineHasTurnIdentity(line, turnId, requestId)
      && line.includes('stage="canonical_terminal_published"')
      && line.includes("event_sequence=")
  )).length;
}

function logField(line, name) {
  const match = line.match(new RegExp(`\\b${name}=(?:"([^"]*)"|(\\S+))`));
  return match ? (match[1] ?? match[2]) : null;
}

function logNumber(line, name) {
  const value = logField(line, name);
  if (value === null) return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function logTimestampMs(line) {
  const match = line.match(/^(\d{4}-\d{2}-\d{2}T[^ ]+Z)/u);
  if (!match) return null;
  const timestamp = Date.parse(match[1]);
  return Number.isFinite(timestamp) ? timestamp : null;
}

function timingEventsForTurn(logText, { turnId, requestId, stage }) {
  return logText
    .split("\n")
    .filter((line) => line.includes(`stage="${stage}"`)
      && lineHasTurnIdentity(line, turnId, requestId))
    .map((line) => ({
      atMs: logTimestampMs(line),
      elapsedMs: logNumber(line, "elapsed_ms"),
      sequence: logNumber(line, "event_sequence"),
      providerRound: logNumber(line, "round"),
      toolCallCount: logNumber(line, "tool_call_count"),
    }))
    .filter((event) => Number.isFinite(event.atMs))
    .sort((left, right) => left.atMs - right.atMs);
}

function timingStage(events, acceptedAtMs) {
  if (events.length === 0) return null;
  const toSnapshot = (event) => ({
    atMs: event.atMs,
    sinceAcceptedMs: Math.max(0, event.atMs - acceptedAtMs),
    elapsedMs: event.elapsedMs,
    sequence: Number.isInteger(event.sequence) ? event.sequence : null,
    providerRound: Number.isInteger(event.providerRound) ? event.providerRound : null,
    toolCallCount: Number.isInteger(event.toolCallCount) ? event.toolCallCount : null,
  });
  return {
    count: events.length,
    first: toSnapshot(events[0]),
    last: toSnapshot(events[events.length - 1]),
  };
}

function hashText(text) {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

function approvalTrajectorySample({
  logText,
  sessionId,
  turnId,
  requestId,
  sampleIndex,
  scenario,
  requestText,
  terminal,
  providerRequestCount,
  settlementRequired,
  nextTurnAdmissionChecked,
  fixture = "real-provider-task-approval-deny-recovery-v1",
}) {
  const acceptedEvents = timingEventsForTurn(logText, {
    turnId,
    requestId,
    stage: "accepted_response_sent",
  });
  const accepted = acceptedEvents[0];
  if (!accepted) throw new Error(`approval evidence 缺少 accepted_response_sent：${turnId}`);
  const acceptedAtMs = accepted.atMs;
  const stageFor = (stage) => timingStage(timingEventsForTurn(logText, {
    turnId,
    requestId,
    stage,
  }), acceptedAtMs);
  const terminalStage = stageFor("canonical_terminal_published");
  const settlementStage = stageFor("session_terminal_finalize_core_completed");
  if (!terminalStage) throw new Error(`approval evidence 缺少 canonical_terminal_published：${turnId}`);
  if (!settlementStage) {
    throw new Error(`approval evidence 缺少 session_terminal_finalize_core_completed：${turnId}`);
  }
  return {
    fixture,
    scenario,
    sampleIndex,
    querySource: "main_turn",
    executionProfile: "task",
    trajectoryMode: terminal.status === "completed" ? "normal" : "abnormal",
    expectedOutcome: terminal.status,
    sessionId,
    turnId,
    requestId,
    inputHash: hashText(requestText),
    status: terminal.status,
    terminalStatus: terminal.status,
    acceptedMs: accepted.elapsedMs ?? 0,
    firstEventMs: stageFor("event_bus_item_published")?.first?.sinceAcceptedMs ?? null,
    terminalMs: terminalStage.first.sinceAcceptedMs,
    firstEventSequence: stageFor("event_bus_item_published")?.first?.sequence ?? null,
    terminalSequence: terminalStage.first.sequence ?? terminal.sequence ?? null,
    providerRequestCount,
    providerFirstRawDeltaMs: stageFor("provider_first_raw_delta")?.first?.sinceAcceptedMs ?? null,
    providerFirstDeltaMs: stageFor("provider_first_delta")?.first?.sinceAcceptedMs ?? null,
    backendStages: {
      accepted_response_sent: timingStage(acceptedEvents, acceptedAtMs),
      provider_request_started: stageFor("provider_request_started"),
      provider_first_raw_delta: stageFor("provider_first_raw_delta"),
      provider_first_delta: stageFor("provider_first_delta"),
      event_bus_first_event: stageFor("event_bus_item_published"),
      canonical_terminal_published: terminalStage,
      session_terminal_finalize_core_completed: settlementStage,
    },
    settlement: {
      observed: true,
      barrier: "session_terminal_finalize_core_completed",
      stage: "session_terminal_finalize_core_completed",
      sequence: settlementStage.first.sequence,
      sinceAcceptedMs: settlementStage.first.sinceAcceptedMs,
      next_turn_admission_checked: nextTurnAdmissionChecked,
    },
    settlementRequired,
  };
}

async function runReconnect({ transport = "sse" } = {}) {
  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-reconnect-"));
  const stateRoot = join(stateParent, "state");
  await mkdir(stateRoot, { recursive: true });
  if (isTaskProfile) {
    taskWorkspacePath = join(stateParent, "workspace");
    await prepareTaskWorkspace(taskWorkspacePath);
  }
  let daemon;
  let evidence;
  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    const health = await waitForHealth();
    const workspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = workspaceId;
    const response = await submitTurn({
      workspaceId,
      requestId: firstRequestId,
      userMessageId: firstUserMessageId,
      text: requestText,
    });
    if (response.httpStatus < 200 || response.httpStatus >= 300) {
      throw new Error(`真实 Provider reconnect Turn 未被接纳：HTTP ${response.httpStatus} ${response.text}`);
    }
    const sessionId = response.body?.sessionId;
    const turnId = response.body?.turnId;
    if (typeof sessionId !== "string" || typeof turnId !== "string") {
      throw new Error(`reconnect 接纳缺少 Session/Turn identity：${response.text}`);
    }
    const reconnect = await (transport === "websocket"
      ? observeWebSocketReconnect
      : observeSseReconnect)({
      sessionId,
      turnId,
      afterSequence: response.body?.eventSequence,
      workspaceId,
    });
    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const providerRequests = countProviderRequests(logText, turnId, firstRequestId);
    const canonicalTerminals = countCanonicalTerminals(logText, turnId, firstRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingUsers = timeline.filter((entry) => entry?.message === requestText);
    const terminalStatus = reconnect.reconnect_terminal.status;
    const passed = terminalStatus === "completed"
      && reconnect.first_connection.sequence < reconnect.reconnect_terminal.sequence
      && providerRequests >= 1
      && canonicalTerminals === 1
      && matchingUsers.length === 1
      && sourceStable;
    evidence = {
      schema_version: `magi.real-provider.${transport}.v1`,
      fixture: `real-provider-${profile}-${transport}-reconnect-v1`,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      request: {
        request_id: firstRequestId,
        user_message_id: firstUserMessageId,
        session_id: sessionId,
        turn_id: turnId,
        execution_profile: response.body?.executionProfile || null,
        request_scope: isTaskProfile ? "workspace" : "personal",
        request_text: requestText,
        accepted_http_status: response.httpStatus,
        accepted_event_sequence: response.body?.eventSequence || null,
        reconnect,
      },
      facts: {
        provider_request_count: providerRequests,
        canonical_terminal_count: canonicalTerminals,
        matching_user_message_count: matchingUsers.length,
        first_connection_dropped_before_terminal: true,
        reconnect_terminal_status: terminalStatus,
        event_sequence_increased: reconnect.first_connection.sequence
          < reconnect.reconnect_terminal.sequence,
      },
      logs: {
        daemon: `${evidencePath}.daemon.log`,
      },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      providerRequests,
      canonicalTerminals,
      firstConnectionSequence: reconnect.first_connection.sequence,
      reconnectTerminalSequence: reconnect.reconnect_terminal.sequence,
      terminalStatus,
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider ${transport} reconnect 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: `magi.real-provider.${transport}.v1`,
        fixture: `real-provider-${profile}-${transport}-reconnect-v1`,
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
      };
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 reconnect 采样状态根：${stateParent}`);
    }
  }
}

async function runRestartReconnect({ transport = "sse" } = {}) {
  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-restart-reconnect-"));
  const stateRoot = join(stateParent, "state");
  await mkdir(stateRoot, { recursive: true });
  if (isTaskProfile) {
    taskWorkspacePath = join(stateParent, "workspace");
    await prepareTaskWorkspace(taskWorkspacePath);
  }

  let firstDaemon;
  let secondDaemon;
  let evidence;
  let phase = "before_first_daemon";
  try {
    phase = "first_daemon_started";
    firstDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const firstHealth = await waitForHealth();
    const firstWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = firstWorkspaceId;
    const response = await submitTurn({
      workspaceId: firstWorkspaceId,
      requestId: firstRequestId,
      userMessageId: firstUserMessageId,
      text: requestText,
    });
    if (response.httpStatus < 200 || response.httpStatus >= 300) {
      throw new Error(`真实 Provider restart-reconnect Turn 未被接纳：HTTP ${response.httpStatus} ${response.text}`);
    }
    const sessionId = response.body?.sessionId;
    const turnId = response.body?.turnId;
    if (typeof sessionId !== "string" || typeof turnId !== "string") {
      throw new Error(`restart-reconnect 接纳缺少 Session/Turn identity：${response.text}`);
    }

    phase = "first_connection_terminal";
    const firstTerminal = await waitForTerminal({
      sessionId,
      turnId,
      afterSequence: response.body?.eventSequence,
      workspaceId: firstWorkspaceId,
    });
    // 首个 daemon 已观察到唯一 terminal；第二个 daemon 仍必须使用旧事件游标
    // 从同一 state root 恢复该 terminal，不能依赖重发 request 或新请求制造事件。
    const reconnectAfterSequence = Math.max(0, firstTerminal.sequence - 1);
    phase = "first_daemon_stopping";
    await stopDaemon(firstDaemon);
    const firstLog = firstDaemon.logText();

    phase = "second_daemon_started";
    secondDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const secondHealth = await waitForHealth();
    const secondWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = secondWorkspaceId;
    phase = "second_connection_reconnect";
    const reconnectTerminal = transport === "websocket"
      ? await readWebSocketTerminalAfterRestart({
        sessionId,
        turnId,
        afterSequence: reconnectAfterSequence,
        workspaceId: secondWorkspaceId,
      })
      : await readSseUntil({
        sessionId,
        turnId,
        afterSequence: reconnectAfterSequence,
        workspaceId: secondWorkspaceId,
        timeoutMs: restartReconnectTimeoutMs,
        predicate: isCanonicalTerminalEvent,
      });
    const messages = await messagesFor(sessionId, secondWorkspaceId);
    const secondLog = secondDaemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const firstProviderRequests = countProviderRequests(firstLog, turnId, firstRequestId);
    const secondProviderRequests = countProviderRequests(secondLog, turnId, firstRequestId);
    const firstCanonicalTerminals = countCanonicalTerminals(firstLog, turnId, firstRequestId);
    const secondCanonicalTerminals = countCanonicalTerminals(secondLog, turnId, firstRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingUsers = timeline.filter((entry) => entry?.message === requestText);
    const reconnectTerminalSequence = transport === "websocket"
      ? reconnectTerminal.event.sequence
      : reconnectTerminal.sequence;
    const terminalStatus = reconnectTerminal.status;
    const workspaceStable = !isTaskProfile || firstWorkspaceId === secondWorkspaceId;
    const passed = firstTerminal.status === "completed"
      && terminalStatus === "completed"
      && reconnectTerminalSequence > reconnectAfterSequence
      && reconnectTerminalSequence >= firstTerminal.sequence
      && (transport !== "websocket"
        || reconnectTerminal.recovery_method === "events/resyncRequired")
      && firstProviderRequests >= 1
      && secondProviderRequests === 0
      && firstCanonicalTerminals + secondCanonicalTerminals === 1
      && matchingUsers.length === 1
      && firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch
      && workspaceStable
      && sourceStable;
    evidence = {
      schema_version: "magi.real-provider.restart-reconnect.v1",
      fixture: `real-provider-${profile}-${transport}-restart-reconnect-v1`,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      replay_mode: replayMode,
      transport,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        first_runtime_epoch: firstHealth.runtimeEpoch,
        second_runtime_epoch: secondHealth.runtimeEpoch,
        state_root_reused: true,
        workspace_path: taskWorkspacePath,
        first_workspace_id: firstWorkspaceId,
        second_workspace_id: secondWorkspaceId,
      },
      request: {
        request_id: firstRequestId,
        user_message_id: firstUserMessageId,
        session_id: sessionId,
        turn_id: turnId,
        execution_profile: response.body?.executionProfile || null,
        request_scope: isTaskProfile ? "workspace" : "personal",
        request_text: requestText,
        accepted_http_status: response.httpStatus,
        accepted_event_sequence: response.body?.eventSequence || null,
        first_terminal: {
          sequence: firstTerminal.sequence,
          kind: firstTerminal.kind,
          status: firstTerminal.status,
        },
        reconnect_terminal: {
          sequence: reconnectTerminalSequence,
          kind: reconnectTerminal.kind,
          status: terminalStatus,
          after_sequence: reconnectAfterSequence,
          recovery_method: reconnectTerminal.recovery_method || null,
        },
      },
      facts: {
        first_provider_request_count: firstProviderRequests,
        second_provider_request_count: secondProviderRequests,
        first_canonical_terminal_count: firstCanonicalTerminals,
        second_canonical_terminal_count: secondCanonicalTerminals,
        canonical_terminal_count: firstCanonicalTerminals + secondCanonicalTerminals,
        matching_user_message_count: matchingUsers.length,
        terminal_status: terminalStatus,
        reconnect_after_sequence: reconnectAfterSequence,
        event_sequence_recovered: reconnectTerminalSequence >= firstTerminal.sequence,
        recovery_method: reconnectTerminal.recovery_method || null,
        runtime_epoch_changed: firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch,
        workspace_identity_stable: workspaceStable,
      },
      logs: {
        first: `${evidencePath}.first.daemon.log`,
        second: `${evidencePath}.second.daemon.log`,
      },
    };
    await writeFile(evidence.logs.first, firstLog, "utf8");
    await writeFile(evidence.logs.second, secondLog, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      profile,
      firstProviderRequests,
      secondProviderRequests,
      firstCanonicalTerminals,
      secondCanonicalTerminals,
      firstTerminalSequence: firstTerminal.sequence,
      reconnectTerminalSequence,
      terminalStatus,
    }, null, 2));
    if (!passed) {
      throw new Error(`真实 Provider ${transport} restart-reconnect 验收失败，证据已写入 ${evidencePath}`);
    }
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.restart-reconnect.v1",
        fixture: `real-provider-${profile}-${transport}-restart-reconnect-v1`,
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        transport,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: {
          first: `${evidencePath}.first.daemon.log`,
          second: `${evidencePath}.second.daemon.log`,
        },
      };
      if (firstDaemon) await writeFile(evidence.logs.first, firstDaemon.logText(), "utf8");
      if (secondDaemon) await writeFile(evidence.logs.second, secondDaemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(firstDaemon);
    await stopDaemon(secondDaemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 restart-reconnect 采样状态根：${stateParent}`);
    }
  }
}

async function runLongHistoryReplay() {
  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-long-history-replay-"));
  const stateRoot = join(stateParent, "state");
  await mkdir(stateRoot, { recursive: true });
  if (isTaskProfile) {
    taskWorkspacePath = join(stateParent, "workspace");
    await prepareTaskWorkspace(taskWorkspacePath);
  }

  let firstDaemon;
  let secondDaemon;
  let evidence;
  let phase = "before_first_daemon";
  let firstLogForFailure = null;
  let secondLogForFailure = null;
  try {
    phase = "first_daemon_started";
    firstDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const firstHealth = await waitForHealth();
    const contextConfiguration = await saveContextWindow();
    const firstWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = firstWorkspaceId;

    const seedSamples = [];
    let sessionId = null;
    let workspaceId = firstWorkspaceId;
    for (let index = 0; index < longHistoryTurns; index += 1) {
      const requestId = index === 0 ? firstRequestId : `${runIdentity}-history-${index}`;
      const userMessageId = index === 0 ? firstUserMessageId : `user-${requestId}`;
      // 先用一个短首 Turn 建立可恢复的 Session；若第一 Turn 就把超长输入
      // 写入 transcript，压缩器没有可安全拆分的前缀，正确行为是拒绝压缩。
      const text = index === 0 ? requestText : longHistorySeedText(index);
      const response = await submitTurn({
        sessionId,
        workspaceId,
        requestId,
        userMessageId,
        text,
      });
      if (response.httpStatus < 200 || response.httpStatus >= 300) {
        throw new Error(`长历史 seed Turn 未被接纳：index=${index} HTTP ${response.httpStatus} ${response.text}`);
      }
      const acceptedSessionId = response.body?.sessionId;
      const turnId = response.body?.turnId;
      if (typeof acceptedSessionId !== "string" || typeof turnId !== "string") {
        throw new Error(`长历史 seed 缺少 Session/Turn identity：index=${index} ${response.text}`);
      }
      if (sessionId !== null && acceptedSessionId !== sessionId) {
        throw new Error(`长历史 seed 改变了 Session identity：index=${index}`);
      }
      sessionId = acceptedSessionId;
      const terminal = await waitForTerminal({
        sessionId,
        turnId,
        afterSequence: response.body?.eventSequence,
        workspaceId,
      });
      if (terminal.status !== "completed") {
        throw new Error(`长历史 seed 未完成：index=${index} status=${terminal.status}`);
      }
      seedSamples.push({
        index,
        requestId,
        userMessageId,
        turnId,
        sessionId,
        terminal,
        text,
      });
    }

    phase = "history_before_restart";
    const firstMessages = await messagesFor(sessionId, workspaceId);
    const firstLog = firstDaemon.logText();
    firstLogForFailure = firstLog;
    const compactionEvents = seedSamples.flatMap((sample) => sample.terminal.compactionEvents || []);
    const completedCompactions = compactionEvents.filter((event) => event.state === "completed");
    await stopDaemon(firstDaemon);
    firstDaemon = null;

    phase = "second_daemon_started";
    secondDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const secondHealth = await waitForHealth();
    const secondWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = secondWorkspaceId;
    const historyAfterRestart = await messagesFor(sessionId, secondWorkspaceId);

    phase = "post_compaction_history_turn";
    const secondResponse = await submitTurn({
      sessionId,
      workspaceId: secondWorkspaceId,
      requestId: secondRequestId,
      userMessageId: secondUserMessageId,
      text: secondRequestText,
    });
    if (secondResponse.httpStatus < 200 || secondResponse.httpStatus >= 300) {
      throw new Error(`压缩后重启的 history Turn 未被接纳：HTTP ${secondResponse.httpStatus} ${secondResponse.text}`);
    }
    const secondTurnId = secondResponse.body?.turnId;
    if (typeof secondTurnId !== "string" || secondResponse.body?.sessionId !== sessionId) {
      throw new Error(`压缩后 history Turn 缺少稳定 Session/Turn identity：${secondResponse.text}`);
    }
    const secondTerminal = await waitForTerminal({
      sessionId,
      turnId: secondTurnId,
      afterSequence: secondResponse.body?.eventSequence,
      workspaceId: secondWorkspaceId,
    });
    const secondLogBeforeReplay = secondDaemon.logText();
    const replayResponse = await submitTurn({
      replay: true,
      sessionId,
      workspaceId: secondWorkspaceId,
      requestId: secondRequestId,
      userMessageId: secondUserMessageId,
      text: secondRequestText,
    });
    const secondMessages = await messagesFor(sessionId, secondWorkspaceId);
    const secondLog = secondDaemon.logText();
    secondLogForFailure = secondLog;
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const seedProviderRequestCounts = seedSamples.map((sample) => countProviderRequests(
      firstLog,
      sample.turnId,
      sample.requestId,
    ));
    const seedCanonicalTerminalCounts = seedSamples.map((sample) => countCanonicalTerminals(
      firstLog,
      sample.turnId,
      sample.requestId,
    ));
    const secondProviderRequestsBeforeReplay = countProviderRequests(
      secondLogBeforeReplay,
      secondTurnId,
      secondRequestId,
    );
    const secondProviderRequests = countProviderRequests(secondLog, secondTurnId, secondRequestId);
    const secondCanonicalTerminalsBeforeReplay = countCanonicalTerminals(
      secondLogBeforeReplay,
      secondTurnId,
      secondRequestId,
    );
    const secondCanonicalTerminals = countCanonicalTerminals(secondLog, secondTurnId, secondRequestId);
    const firstTimeline = Array.isArray(firstMessages?.timeline) ? firstMessages.timeline : [];
    const historyAfterRestartTimeline = Array.isArray(historyAfterRestart?.timeline)
      ? historyAfterRestart.timeline
      : [];
    const secondTimeline = Array.isArray(secondMessages?.timeline) ? secondMessages.timeline : [];
    const firstSeedMessageCounts = seedSamples.map((sample) => (
      firstTimeline.filter((entry) => entry?.message === sample.text).length
    ));
    const restoredSeedMessageCounts = seedSamples.map((sample) => (
      historyAfterRestartTimeline.filter((entry) => entry?.message === sample.text).length
    ));
    const secondMessageCount = secondTimeline.filter((entry) => entry?.message === secondRequestText).length;
    const historyCanonicalTurns = Array.isArray(historyAfterRestart?.canonicalTurns)
      ? historyAfterRestart.canonicalTurns
      : [];
    const workspaceStable = !isTaskProfile || firstWorkspaceId === secondWorkspaceId;
    const historyRestored = historyAfterRestart?.sessionId === sessionId
      && historyAfterRestart?.currentSession?.sessionId === sessionId
      && historyCanonicalTurns.length >= longHistoryTurns
      && restoredSeedMessageCounts.every((count) => count === 1);
    const passed = contextWindowTokens !== null
      && contextConfiguration?.saved === true
      && completedCompactions.length >= 1
      && seedProviderRequestCounts.every((count) => count >= 1)
      && seedCanonicalTerminalCounts.every((count) => count === 1)
      && seedSamples.every((sample) => sample.terminal.status === "completed")
      && historyRestored
      && secondTerminal.status === "completed"
      && replayResponse.httpStatus >= 200
      && replayResponse.httpStatus < 300
      && replayResponse.body?.turnId === secondTurnId
      && replayResponse.body?.sessionId === sessionId
      && secondProviderRequestsBeforeReplay >= 1
      && secondProviderRequests === secondProviderRequestsBeforeReplay
      && secondCanonicalTerminalsBeforeReplay === 1
      && secondCanonicalTerminals === secondCanonicalTerminalsBeforeReplay
      && secondMessageCount === 1
      && firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch
      && workspaceStable
      && sourceStable;
    evidence = {
      schema_version: "magi.real-provider.long-history-replay.v1",
      fixture: `real-provider-${profile}-long-history-replay-v1`,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      context: {
        configuration: contextConfiguration,
        context_window_tokens: contextWindowTokens,
        long_history_chars: longHistoryChars,
        long_history_turns: longHistoryTurns,
        completed_compaction_count: completedCompactions.length,
        compaction_events: compactionEvents,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        first_runtime_epoch: firstHealth.runtimeEpoch,
        second_runtime_epoch: secondHealth.runtimeEpoch,
        state_root_reused: true,
        workspace_path: taskWorkspacePath,
        first_workspace_id: firstWorkspaceId,
        second_workspace_id: secondWorkspaceId,
      },
      request: {
        seed_turns: seedSamples.map((sample) => ({
          index: sample.index,
          request_id: sample.requestId,
          user_message_id: sample.userMessageId,
          session_id: sample.sessionId,
          turn_id: sample.turnId,
          text_length: sample.text.length,
          terminal: sample.terminal,
        })),
        post_restart_request_id: secondRequestId,
        post_restart_user_message_id: secondUserMessageId,
        post_restart_session_id: sessionId,
        post_restart_turn_id: secondTurnId,
        post_restart_text: secondRequestText,
        replay_http_status: replayResponse.httpStatus,
        replay_body: replayResponse.body,
      },
      facts: {
        seed_provider_request_counts: seedProviderRequestCounts,
        seed_canonical_terminal_counts: seedCanonicalTerminalCounts,
        first_seed_message_counts: firstSeedMessageCounts,
        restored_seed_message_counts: restoredSeedMessageCounts,
        history_canonical_turn_count_after_restart: historyCanonicalTurns.length,
        history_restored: historyRestored,
        second_provider_request_count_before_replay: secondProviderRequestsBeforeReplay,
        second_provider_request_count: secondProviderRequests,
        second_canonical_terminal_count_before_replay: secondCanonicalTerminalsBeforeReplay,
        second_canonical_terminal_count: secondCanonicalTerminals,
        second_message_count: secondMessageCount,
        runtime_epoch_changed: firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch,
        workspace_identity_stable: workspaceStable,
      },
      logs: {
        first: `${evidencePath}.first.daemon.log`,
        second: `${evidencePath}.second.daemon.log`,
      },
    };
    await writeFile(evidence.logs.first, firstLog, "utf8");
    await writeFile(evidence.logs.second, secondLog, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      profile,
      longHistoryTurns,
      completedCompactions: completedCompactions.length,
      historyRestored,
      secondProviderRequestsBeforeReplay,
      secondProviderRequests,
      secondCanonicalTerminalsBeforeReplay,
      secondCanonicalTerminals,
    }, null, 2));
    if (!passed) {
      throw new Error(`真实 Provider long-history/replay 验收失败，证据已写入 ${evidencePath}`);
    }
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.long-history-replay.v1",
        fixture: `real-provider-${profile}-long-history-replay-v1`,
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: {
          first: `${evidencePath}.first.daemon.log`,
          second: `${evidencePath}.second.daemon.log`,
        },
      };
      if (firstLogForFailure !== null) await writeFile(evidence.logs.first, firstLogForFailure, "utf8");
      else if (firstDaemon) await writeFile(evidence.logs.first, firstDaemon.logText(), "utf8");
      if (secondLogForFailure !== null) await writeFile(evidence.logs.second, secondLogForFailure, "utf8");
      else if (secondDaemon) await writeFile(evidence.logs.second, secondDaemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(firstDaemon);
    await stopDaemon(secondDaemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 long-history/replay 采样状态根：${stateParent}`);
    }
  }
}

async function runBlockedRecovery() {
  if (!isTaskProfile) {
    throw new Error("blocked-recovery 只验证 task profile 的 Git 阻塞后恢复");
  }

  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-blocked-recovery-"));
  const stateRoot = join(stateParent, "state");
  taskWorkspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await prepareTaskWorkspace(taskWorkspacePath);

  let daemon;
  let evidence;
  let phase = "before_daemon_started";
  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    phase = "daemon_started";
    const health = await waitForHealth();
    const workspaceId = await registerWorkspace(taskWorkspacePath);
    workspaceIdForRun = workspaceId;
    const baselineBranch = fixtureGitOutput(taskWorkspacePath, ["branch", "--show-current"]).trim();
    const baselineHead = fixtureGitOutput(taskWorkspacePath, ["rev-parse", "HEAD"]).trim();

    phase = "baseline_turn";
    const firstResponse = await submitTurn({
      workspaceId,
      requestId: firstRequestId,
      userMessageId: firstUserMessageId,
      text: requestText,
    });
    if (firstResponse.httpStatus < 200 || firstResponse.httpStatus >= 300) {
      throw new Error(`Git 恢复基线 Turn 未被接纳：HTTP ${firstResponse.httpStatus} ${firstResponse.text}`);
    }
    const sessionId = firstResponse.body?.sessionId;
    const firstTurnId = firstResponse.body?.turnId;
    if (typeof sessionId !== "string" || typeof firstTurnId !== "string") {
      throw new Error(`Git 恢复基线接纳缺少 Session/Turn identity：${firstResponse.text}`);
    }
    const firstTerminal = await waitForTerminal({
      sessionId,
      turnId: firstTurnId,
      afterSequence: firstResponse.body?.eventSequence,
      workspaceId,
    });
    if (firstTerminal.status !== "completed") {
      throw new Error(`Git 恢复基线 Turn 未完成：${firstTerminal.status}`);
    }

    phase = "git_drift_introduced";
    const driftBranch = `external/magi-drift-${Date.now()}`;
    fixtureGitOutput(taskWorkspacePath, ["switch", "-c", driftBranch]);
    const driftObservedBranch = fixtureGitOutput(taskWorkspacePath, ["branch", "--show-current"]).trim();
    const driftObservedHead = fixtureGitOutput(taskWorkspacePath, ["rev-parse", "HEAD"]).trim();

    phase = "blocked_turn";
    const blockedRequestId = `${runIdentity}-blocked`;
    const blockedUserMessageId = `user-${blockedRequestId}`;
    const blockedText = `请调用 file_read 读取 ${join(taskWorkspacePath, "README.md")}，验证 Git 漂移后应先阻塞。`;
    const blockedResponse = await submitTurn({
      sessionId,
      workspaceId,
      requestId: blockedRequestId,
      userMessageId: blockedUserMessageId,
      text: blockedText,
    });
    if (blockedResponse.httpStatus < 200 || blockedResponse.httpStatus >= 300) {
      throw new Error(`Git 漂移 Turn 未被接纳：HTTP ${blockedResponse.httpStatus} ${blockedResponse.text}`);
    }
    const blockedTurnId = blockedResponse.body?.turnId;
    if (typeof blockedTurnId !== "string" || blockedResponse.body?.sessionId !== sessionId) {
      throw new Error(`Git 漂移 Turn 缺少稳定 Session/Turn identity：${blockedResponse.text}`);
    }
    const blockedTerminal = await waitForTerminal({
      sessionId,
      turnId: blockedTurnId,
      afterSequence: blockedResponse.body?.eventSequence,
      workspaceId,
    });

    phase = "git_baseline_restored";
    fixtureGitOutput(taskWorkspacePath, ["switch", baselineBranch]);
    const restoredBranch = fixtureGitOutput(taskWorkspacePath, ["branch", "--show-current"]).trim();
    const restoredHead = fixtureGitOutput(taskWorkspacePath, ["rev-parse", "HEAD"]).trim();

    phase = "recovery_turn";
    const recoveryRequestId = `${runIdentity}-recovery`;
    const recoveryUserMessageId = `user-${recoveryRequestId}`;
    const recoveryText = `Git 已恢复到 ${baselineBranch}，请再次调用 file_read 读取 ${join(taskWorkspacePath, "README.md")}，然后只回答 REAL_PROVIDER_GIT_RECOVERY_OK。`;
    const recoveryResponse = await submitTurn({
      sessionId,
      workspaceId,
      requestId: recoveryRequestId,
      userMessageId: recoveryUserMessageId,
      text: recoveryText,
    });
    if (recoveryResponse.httpStatus < 200 || recoveryResponse.httpStatus >= 300) {
      throw new Error(`Git 恢复 Turn 未被接纳：HTTP ${recoveryResponse.httpStatus} ${recoveryResponse.text}`);
    }
    const recoveryTurnId = recoveryResponse.body?.turnId;
    if (typeof recoveryTurnId !== "string" || recoveryResponse.body?.sessionId !== sessionId) {
      throw new Error(`Git 恢复 Turn 缺少稳定 Session/Turn identity：${recoveryResponse.text}`);
    }
    const recoveryTerminal = await waitForTerminal({
      sessionId,
      turnId: recoveryTurnId,
      afterSequence: recoveryResponse.body?.eventSequence,
      workspaceId,
    });

    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const firstProviderRequests = countProviderRequests(logText, firstTurnId, firstRequestId);
    const blockedProviderRequests = countProviderRequests(logText, blockedTurnId, blockedRequestId);
    const recoveryProviderRequests = countProviderRequests(logText, recoveryTurnId, recoveryRequestId);
    const firstCanonicalTerminals = countCanonicalTerminals(logText, firstTurnId, firstRequestId);
    const blockedCanonicalTerminals = countCanonicalTerminals(logText, blockedTurnId, blockedRequestId);
    const recoveryCanonicalTerminals = countCanonicalTerminals(logText, recoveryTurnId, recoveryRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingMessages = {
      first: timeline.filter((entry) => entry?.message === requestText).length,
      blocked: timeline.filter((entry) => entry?.message === blockedText).length,
      recovery: timeline.filter((entry) => entry?.message === recoveryText).length,
    };
    const blockedStatus = ["blocked", "failed"].includes(blockedTerminal.status);
    const passed = firstResponse.body?.executionProfile === "task"
      && blockedResponse.body?.executionProfile === "task"
      && recoveryResponse.body?.executionProfile === "task"
      && firstTerminal.status === "completed"
      && blockedStatus
      && recoveryTerminal.status === "completed"
      && firstProviderRequests >= 1
      && blockedProviderRequests === 0
      && recoveryProviderRequests >= 1
      && firstCanonicalTerminals === 1
      && blockedCanonicalTerminals === 1
      && recoveryCanonicalTerminals === 1
      && Object.values(matchingMessages).every((count) => count === 1)
      && baselineBranch !== driftObservedBranch
      && baselineHead === driftObservedHead
      && restoredBranch === baselineBranch
      && restoredHead === baselineHead
      && health.runtimeEpoch
      && sourceStable;
    evidence = {
      schema_version: "magi.real-provider.blocked-recovery.v1",
      fixture: "real-provider-task-git-blocked-recovery-v1",
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      git: {
        baseline_branch: baselineBranch,
        baseline_head: baselineHead,
        drift_branch: driftObservedBranch,
        drift_head: driftObservedHead,
        restored_branch: restoredBranch,
        restored_head: restoredHead,
        drift_detected: baselineBranch !== driftObservedBranch || baselineHead !== driftObservedHead,
        baseline_restored: restoredBranch === baselineBranch && restoredHead === baselineHead,
      },
      requests: {
        baseline: {
          request_id: firstRequestId,
          user_message_id: firstUserMessageId,
          session_id: sessionId,
          turn_id: firstTurnId,
          terminal: firstTerminal,
          accepted_http_status: firstResponse.httpStatus,
          execution_profile: firstResponse.body?.executionProfile || null,
        },
        blocked: {
          request_id: blockedRequestId,
          user_message_id: blockedUserMessageId,
          session_id: sessionId,
          turn_id: blockedTurnId,
          terminal: blockedTerminal,
          accepted_http_status: blockedResponse.httpStatus,
          execution_profile: blockedResponse.body?.executionProfile || null,
        },
        recovery: {
          request_id: recoveryRequestId,
          user_message_id: recoveryUserMessageId,
          session_id: sessionId,
          turn_id: recoveryTurnId,
          terminal: recoveryTerminal,
          accepted_http_status: recoveryResponse.httpStatus,
          execution_profile: recoveryResponse.body?.executionProfile || null,
        },
      },
      facts: {
        first_provider_request_count: firstProviderRequests,
        blocked_provider_request_count: blockedProviderRequests,
        recovery_provider_request_count: recoveryProviderRequests,
        first_canonical_terminal_count: firstCanonicalTerminals,
        blocked_canonical_terminal_count: blockedCanonicalTerminals,
        recovery_canonical_terminal_count: recoveryCanonicalTerminals,
        matching_user_message_counts: matchingMessages,
        blocked_status: blockedTerminal.status,
        blocked_before_provider_dispatch: blockedProviderRequests === 0,
        recovery_completed: recoveryTerminal.status === "completed",
        session_identity_stable: true,
      },
      logs: { daemon: `${evidencePath}.daemon.log` },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      blockedStatus: blockedTerminal.status,
      firstProviderRequests,
      blockedProviderRequests,
      recoveryProviderRequests,
      canonicalTerminals: {
        first: firstCanonicalTerminals,
        blocked: blockedCanonicalTerminals,
        recovery: recoveryCanonicalTerminals,
      },
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider Git blocked-recovery 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.blocked-recovery.v1",
        fixture: "real-provider-task-git-blocked-recovery-v1",
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: { daemon: `${evidencePath}.daemon.log` },
      };
      if (daemon) await writeFile(evidence.logs.daemon, daemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 blocked-recovery 采样状态根：${stateParent}`);
    }
  }
}

async function runApprovalRecovery() {
  if (!isTaskProfile) {
    throw new Error("approval-recovery 只验证 task profile 的真实工具审批恢复");
  }

  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-approval-recovery-"));
  const stateRoot = join(stateParent, "state");
  taskWorkspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await prepareTaskWorkspace(taskWorkspacePath);
  const deniedTarget = join(taskWorkspacePath, "approval-denied.txt");
  const allowedTarget = join(taskWorkspacePath, "approval-allowed.txt");

  let daemon;
  let evidence;
  let phase = "before_daemon_started";
  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    phase = "daemon_started";
    const health = await waitForHealth();
    const workspaceId = await registerWorkspace(taskWorkspacePath);
    workspaceIdForRun = workspaceId;

    phase = "approval_denied_turn";
    const deniedRequestId = `${runIdentity}-approval-denied`;
    const deniedUserMessageId = `user-${deniedRequestId}`;
    const deniedText = `请严格调用 shell_exec 工具一次，执行命令：printf denied > ${deniedTarget}。不要调用其它写入工具，也不要只用文字回答；等待工具结果后再简短汇总。`;
    const deniedResponse = await submitTurn({
      workspaceId,
      requestId: deniedRequestId,
      userMessageId: deniedUserMessageId,
      text: deniedText,
      accessProfile: "restricted",
    });
    if (deniedResponse.httpStatus < 200 || deniedResponse.httpStatus >= 300) {
      throw new Error(`审批拒绝 Turn 未被接纳：HTTP ${deniedResponse.httpStatus} ${deniedResponse.text}`);
    }
    const sessionId = deniedResponse.body?.sessionId;
    const deniedTurnId = deniedResponse.body?.turnId;
    if (typeof sessionId !== "string" || typeof deniedTurnId !== "string") {
      throw new Error(`审批拒绝 Turn 缺少 Session/Turn identity：${deniedResponse.text}`);
    }
    const deniedApproval = await waitForPendingApproval({ sessionId, workspaceId });
    const deniedResolution = await resolveApproval({
      sessionId,
      workspaceId,
      approvalId: deniedApproval.approvalId,
      decision: "deny",
    });
    const deniedTerminal = await waitForTerminal({
      sessionId,
      turnId: deniedTurnId,
      afterSequence: deniedResponse.body?.eventSequence,
      workspaceId,
    });
    const debugSettlementGraceMs = Number.parseInt(
      process.env.MAGI_REPLAY_DEBUG_SETTLEMENT_GRACE_MS || "0",
      10,
    );
    if (debugSettlementGraceMs > 0) await sleep(debugSettlementGraceMs);

    phase = "approval_allowed_recovery_turn";
    const allowedRequestId = `${runIdentity}-approval-allowed`;
    const allowedUserMessageId = `user-${allowedRequestId}`;
    const allowedText = `请严格调用 shell_exec 工具一次，执行命令：printf allowed > ${allowedTarget}。不要调用其它写入工具，也不要只用文字回答；等待工具结果后再简短汇总。`;
    const allowedResponse = await submitTurn({
      sessionId,
      workspaceId,
      requestId: allowedRequestId,
      userMessageId: allowedUserMessageId,
      text: allowedText,
    });
    if (allowedResponse.httpStatus < 200 || allowedResponse.httpStatus >= 300) {
      throw new Error(`审批恢复 Turn 未被接纳：HTTP ${allowedResponse.httpStatus} ${allowedResponse.text}`);
    }
    const allowedTurnId = allowedResponse.body?.turnId;
    if (typeof allowedTurnId !== "string" || allowedResponse.body?.sessionId !== sessionId) {
      throw new Error(`审批恢复 Turn 缺少稳定 Session/Turn identity：${allowedResponse.text}`);
    }
    const allowedApproval = await waitForPendingApproval({ sessionId, workspaceId });
    const allowedResolution = await resolveApproval({
      sessionId,
      workspaceId,
      approvalId: allowedApproval.approvalId,
      decision: "allow_once",
    });
    const allowedTerminal = await waitForTerminal({
      sessionId,
      turnId: allowedTurnId,
      afterSequence: allowedResponse.body?.eventSequence,
      workspaceId,
    });

    const deniedSideEffect = await pathExists(deniedTarget);
    const allowedSideEffect = await pathExists(allowedTarget);
    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const deniedProviderRequests = countProviderRequests(logText, deniedTurnId, deniedRequestId);
    const allowedProviderRequests = countProviderRequests(logText, allowedTurnId, allowedRequestId);
    const deniedCanonicalTerminals = countCanonicalTerminals(logText, deniedTurnId, deniedRequestId);
    const allowedCanonicalTerminals = countCanonicalTerminals(logText, allowedTurnId, allowedRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingMessages = {
      denied: timeline.filter((entry) => entry?.message === deniedText).length,
      allowed: timeline.filter((entry) => entry?.message === allowedText).length,
    };
    const firstTerminalBeforeRecoveryAdmission = Number.isInteger(deniedTerminal.sequence)
      && Number.isInteger(allowedResponse.body?.eventSequence)
      && deniedTerminal.sequence < allowedResponse.body.eventSequence;
    const passed = deniedResponse.body?.executionProfile === "task"
      && allowedResponse.body?.executionProfile === "task"
      && deniedTerminal.status === "failed"
      && allowedTerminal.status === "completed"
      && deniedApproval.toolName === "shell_exec"
      && allowedApproval.toolName === "shell_exec"
      && deniedResolution.decision === "deny"
      && allowedResolution.decision === "allow_once"
      && deniedProviderRequests >= 1
      && allowedProviderRequests >= 1
      && deniedCanonicalTerminals === 1
      && allowedCanonicalTerminals === 1
      && !deniedSideEffect
      && allowedSideEffect
      && Object.values(matchingMessages).every((count) => count === 1)
      && firstTerminalBeforeRecoveryAdmission
      && sourceStable;
    const samples = [
      approvalTrajectorySample({
        logText,
        sessionId,
        turnId: deniedTurnId,
        requestId: deniedRequestId,
        sampleIndex: 0,
        scenario: "approval_denied",
        requestText: deniedText,
        terminal: deniedTerminal,
        providerRequestCount: deniedProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: firstTerminalBeforeRecoveryAdmission,
      }),
      approvalTrajectorySample({
        logText,
        sessionId,
        turnId: allowedTurnId,
        requestId: allowedRequestId,
        sampleIndex: 1,
        scenario: "approval_allowed_recovery",
        requestText: allowedText,
        terminal: allowedTerminal,
        providerRequestCount: allowedProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: false,
      }),
    ];
    evidence = {
      schema_version: "magi.real-provider.approval-recovery.v1",
      fixture: "real-provider-task-approval-deny-recovery-v1",
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      trajectoryMode: "mixed",
      settlement_required: true,
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      requests: {
        denied: {
          request_id: deniedRequestId,
          user_message_id: deniedUserMessageId,
          session_id: sessionId,
          turn_id: deniedTurnId,
          text: deniedText,
          accepted_http_status: deniedResponse.httpStatus,
          execution_profile: deniedResponse.body?.executionProfile || null,
          terminal: deniedTerminal,
        },
        allowed: {
          request_id: allowedRequestId,
          user_message_id: allowedUserMessageId,
          session_id: sessionId,
          turn_id: allowedTurnId,
          text: allowedText,
          accepted_http_status: allowedResponse.httpStatus,
          execution_profile: allowedResponse.body?.executionProfile || null,
          terminal: allowedTerminal,
        },
      },
      approvals: {
        denied: {
          approval_id: deniedApproval.approvalId,
          tool_name: deniedApproval.toolName,
          decision: deniedResolution.decision,
          resolved_status: deniedResolution.status,
        },
        allowed: {
          approval_id: allowedApproval.approvalId,
          tool_name: allowedApproval.toolName,
          decision: allowedResolution.decision,
          resolved_status: allowedResolution.status,
        },
      },
      facts: {
        denied_provider_request_count: deniedProviderRequests,
        allowed_provider_request_count: allowedProviderRequests,
        denied_canonical_terminal_count: deniedCanonicalTerminals,
        allowed_canonical_terminal_count: allowedCanonicalTerminals,
        matching_user_message_counts: matchingMessages,
        denied_side_effect_present: deniedSideEffect,
        allowed_side_effect_present: allowedSideEffect,
        first_terminal_before_recovery_admission: firstTerminalBeforeRecoveryAdmission,
        recovery_turn_completed: allowedTerminal.status === "completed",
        session_identity_stable: true,
      },
      samples,
      logs: { daemon: `${evidencePath}.daemon.log` },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      deniedStatus: deniedTerminal.status,
      allowedStatus: allowedTerminal.status,
      approvals: [deniedApproval.approvalId, allowedApproval.approvalId],
      providerRequests: [deniedProviderRequests, allowedProviderRequests],
      canonicalTerminals: [deniedCanonicalTerminals, allowedCanonicalTerminals],
      sideEffects: [deniedSideEffect, allowedSideEffect],
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider approval recovery 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.approval-recovery.v1",
        fixture: "real-provider-task-approval-deny-recovery-v1",
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: { daemon: `${evidencePath}.daemon.log` },
      };
      if (daemon) await writeFile(evidence.logs.daemon, daemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 approval-recovery 采样状态根：${stateParent}`);
    }
  }
}

async function runApprovalExpiry() {
  if (!isTaskProfile) {
    throw new Error("approval-expiry 只验证 task profile 的真实工具审批过期");
  }

  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-approval-expiry-"));
  const stateRoot = join(stateParent, "state");
  taskWorkspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await prepareTaskWorkspace(taskWorkspacePath);
  const target = join(taskWorkspacePath, "approval-expired.txt");

  let daemon;
  let evidence;
  let phase = "before_daemon_started";
  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    phase = "daemon_started";
    const health = await waitForHealth();
    const workspaceId = await registerWorkspace(taskWorkspacePath);
    workspaceIdForRun = workspaceId;

    phase = "approval_expiry_turn";
    const requestId = `${runIdentity}-approval-expired`;
    const userMessageId = `user-${requestId}`;
    const requestText = `请严格调用 shell_exec 工具一次，执行命令：printf expired > ${target}。不要调用其它写入工具，也不要只用文字回答；等待工具结果后再简短汇总。`;
    const response = await submitTurn({
      workspaceId,
      requestId,
      userMessageId,
      text: requestText,
      accessProfile: "restricted",
    });
    if (response.httpStatus < 200 || response.httpStatus >= 300) {
      throw new Error(`审批过期 Turn 未被接纳：HTTP ${response.httpStatus} ${response.text}`);
    }
    const sessionId = response.body?.sessionId;
    const turnId = response.body?.turnId;
    if (typeof sessionId !== "string" || typeof turnId !== "string") {
      throw new Error(`审批过期 Turn 缺少 Session/Turn identity：${response.text}`);
    }
    const pending = await waitForPendingApproval({ sessionId, workspaceId });
    if (pending.toolName !== "shell_exec") {
      throw new Error(`审批过期场景产生了意外工具：${pending.toolName}`);
    }

    await sleep(approvalExpiryWaitMs);
    const pendingAfterExpiry = await waitForNoPendingApproval({
      sessionId,
      workspaceId,
      timeoutMs: 30_000,
    });

    const approvalEvents = [];
    const terminal = await readSseUntil({
      sessionId,
      turnId,
      afterSequence: response.body?.eventSequence,
      workspaceId,
      timeoutMs: Math.max(turnTimeoutMs, approvalExpiryWaitMs + 30_000),
      onEvent: ({ event }) => {
        const payload = event?.payload || {};
        const eventTurnId = payload.turn_id
          || payload.turnId
          || payload.canonical_turn?.turnId
          || payload.canonicalTurn?.turnId;
        if (event?.session_id !== sessionId || eventTurnId !== turnId) return;
        if (event.event_type === "tool.approval.requested"
          || event.event_type === "tool.approval.resolved") {
          approvalEvents.push({
            event_type: event.event_type,
            sequence: event.sequence,
            approval_id: payload.approval_id || payload.approvalId || null,
            payload_keys: Object.keys(payload),
          });
        }
      },
      predicate: isCanonicalTerminalEvent,
    });

    const sideEffect = await pathExists(target);
    const messagesAfterExpiry = await messagesFor(sessionId, workspaceId);
    const expiryMessagesJson = JSON.stringify(messagesAfterExpiry);
    const expiredErrorVisible = expiryMessagesJson.includes("tool_approval_expired")
      || expiryMessagesJson.includes("工具授权请求已过期");
    const logTextBeforeRecovery = daemon.logText();
    const expiredProviderRequests = countProviderRequests(logTextBeforeRecovery, turnId, requestId);
    const expiredCanonicalTerminals = countCanonicalTerminals(logTextBeforeRecovery, turnId, requestId);
    const matchingExpiredMessages = Array.isArray(messagesAfterExpiry?.timeline)
      ? messagesAfterExpiry.timeline.filter((entry) => entry?.message === requestText).length
      : 0;

    phase = "post_expiry_recovery_turn";
    const recoveryRequestId = `${runIdentity}-post-expiry-recovery`;
    const recoveryUserMessageId = `user-${recoveryRequestId}`;
    const recoveryText = `审批过期后的接纳检查：请调用 file_read 读取 ${join(taskWorkspacePath, "README.md")}，然后只回答 REAL_PROVIDER_APPROVAL_EXPIRY_RECOVERY_OK；不要调用任何写入工具。`;
    const recoveryResponse = await submitTurn({
      sessionId,
      workspaceId,
      requestId: recoveryRequestId,
      userMessageId: recoveryUserMessageId,
      text: recoveryText,
      accessProfile: "restricted",
    });
    if (recoveryResponse.httpStatus < 200 || recoveryResponse.httpStatus >= 300) {
      throw new Error(`审批过期后的下一轮未被接纳：HTTP ${recoveryResponse.httpStatus} ${recoveryResponse.text}`);
    }
    const recoveryTurnId = recoveryResponse.body?.turnId;
    if (typeof recoveryTurnId !== "string" || recoveryResponse.body?.sessionId !== sessionId) {
      throw new Error(`审批过期后的下一轮缺少稳定 Session/Turn identity：${recoveryResponse.text}`);
    }
    const recoveryTerminal = await waitForTerminal({
      sessionId,
      turnId: recoveryTurnId,
      afterSequence: recoveryResponse.body?.eventSequence,
      workspaceId,
    });

    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const recoveryProviderRequests = countProviderRequests(logText, recoveryTurnId, recoveryRequestId);
    const recoveryCanonicalTerminals = countCanonicalTerminals(logText, recoveryTurnId, recoveryRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingMessages = {
      expired: timeline.filter((entry) => entry?.message === requestText).length,
      recovery: timeline.filter((entry) => entry?.message === recoveryText).length,
    };
    const expiredTerminalAtMs = timingEventsForTurn(logText, {
      turnId,
      requestId,
      stage: "canonical_terminal_published",
    })[0]?.atMs ?? null;
    const recoveryAcceptedAtMs = timingEventsForTurn(logText, {
      turnId: recoveryTurnId,
      requestId: recoveryRequestId,
      stage: "accepted_response_sent",
    })[0]?.atMs ?? null;
    const firstTerminalBeforeRecoveryAdmission = Number.isFinite(expiredTerminalAtMs)
      && Number.isFinite(recoveryAcceptedAtMs)
      && expiredTerminalAtMs <= recoveryAcceptedAtMs;
    const resolvedApprovalEvents = approvalEvents.filter(
      (event) => event.event_type === "tool.approval.resolved",
    );
    const requestedApprovalEvents = approvalEvents.filter(
      (event) => event.event_type === "tool.approval.requested",
    );
    const passed = response.body?.executionProfile === "task"
      && recoveryResponse.body?.executionProfile === "task"
      && terminal.status === "failed"
      && recoveryTerminal.status === "completed"
      && pending.toolName === "shell_exec"
      && pendingAfterExpiry.pendingApprovals?.length === 0
      && requestedApprovalEvents.length === 1
      && resolvedApprovalEvents.length === 0
      && expiredErrorVisible
      && expiredProviderRequests >= 1
      && recoveryProviderRequests >= 1
      && expiredCanonicalTerminals === 1
      && recoveryCanonicalTerminals === 1
      && !sideEffect
      && matchingExpiredMessages === 1
      && Object.values(matchingMessages).every((count) => count === 1)
      && firstTerminalBeforeRecoveryAdmission
      && sourceStable;
    const fixture = "real-provider-task-approval-expiry-v1";
    const samples = [
      approvalTrajectorySample({
        fixture,
        logText,
        sessionId,
        turnId,
        requestId,
        sampleIndex: 0,
        scenario: "approval_expired",
        requestText,
        terminal,
        providerRequestCount: expiredProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: firstTerminalBeforeRecoveryAdmission,
      }),
      approvalTrajectorySample({
        fixture,
        logText,
        sessionId,
        turnId: recoveryTurnId,
        requestId: recoveryRequestId,
        sampleIndex: 1,
        scenario: "post_expiry_recovery",
        requestText: recoveryText,
        terminal: recoveryTerminal,
        providerRequestCount: recoveryProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: false,
      }),
    ];
    evidence = {
      schema_version: "magi.real-provider.approval-expiry.v1",
      fixture,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      trajectoryMode: "mixed",
      settlement_required: true,
      replay_mode: replayMode,
      approval_ttl_ms: toolApprovalTtlMs,
      approval_expiry_wait_ms: approvalExpiryWaitMs,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      requests: {
        expired: {
          request_id: requestId,
          user_message_id: userMessageId,
          session_id: sessionId,
          turn_id: turnId,
          text: requestText,
          accepted_http_status: response.httpStatus,
          execution_profile: response.body?.executionProfile || null,
          terminal,
        },
        recovery: {
          request_id: recoveryRequestId,
          user_message_id: recoveryUserMessageId,
          session_id: sessionId,
          turn_id: recoveryTurnId,
          text: recoveryText,
          accepted_http_status: recoveryResponse.httpStatus,
          execution_profile: recoveryResponse.body?.executionProfile || null,
          terminal: recoveryTerminal,
        },
      },
      approvals: {
        expired: {
          approval_id: pending.approvalId,
          tool_name: pending.toolName,
          requested_at: pending.requestedAt || null,
          pending_before_expiry: true,
          pending_after_expiry: pendingAfterExpiry.pendingApprovals?.length || 0,
          approval_requested_events: requestedApprovalEvents.length,
          approval_resolved_events: resolvedApprovalEvents.length,
        },
      },
      facts: {
        expired_provider_request_count: expiredProviderRequests,
        recovery_provider_request_count: recoveryProviderRequests,
        expired_canonical_terminal_count: expiredCanonicalTerminals,
        recovery_canonical_terminal_count: recoveryCanonicalTerminals,
        matching_user_message_counts: matchingMessages,
        expired_error_visible: expiredErrorVisible,
        expired_side_effect_present: sideEffect,
        first_terminal_before_recovery_admission: firstTerminalBeforeRecoveryAdmission,
        expired_terminal_at_ms: expiredTerminalAtMs,
        recovery_accepted_at_ms: recoveryAcceptedAtMs,
        recovery_turn_completed: recoveryTerminal.status === "completed",
        session_identity_stable: true,
      },
      approval_events: approvalEvents,
      samples,
      logs: { daemon: `${evidencePath}.daemon.log` },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      expiredStatus: terminal.status,
      recoveryStatus: recoveryTerminal.status,
      approvalId: pending.approvalId,
      approvalEvents: approvalEvents.length,
      providerRequests: [expiredProviderRequests, recoveryProviderRequests],
      canonicalTerminals: [expiredCanonicalTerminals, recoveryCanonicalTerminals],
      sideEffect,
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider approval-expiry 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.approval-expiry.v1",
        fixture: "real-provider-task-approval-expiry-v1",
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: { daemon: `${evidencePath}.daemon.log` },
      };
      if (daemon) await writeFile(evidence.logs.daemon, daemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 approval-expiry 采样状态根：${stateParent}`);
    }
  }
}

async function runApprovalCancellation() {
  if (!isTaskProfile) {
    throw new Error("approval-cancel 只验证 task profile 的真实工具审批取消");
  }

  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-approval-cancel-"));
  const stateRoot = join(stateParent, "state");
  taskWorkspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await prepareTaskWorkspace(taskWorkspacePath);
  const target = join(taskWorkspacePath, "approval-cancelled.txt");

  let daemon;
  let evidence;
  let phase = "before_daemon_started";
  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    phase = "daemon_started";
    const health = await waitForHealth();
    const workspaceId = await registerWorkspace(taskWorkspacePath);
    workspaceIdForRun = workspaceId;

    phase = "approval_cancel_turn";
    const requestId = `${runIdentity}-approval-cancelled`;
    const userMessageId = `user-${requestId}`;
    const requestText = `请严格调用 shell_exec 工具一次，执行命令：printf cancelled > ${target}。不要调用其它写入工具，也不要只用文字回答；等待工具结果后再简短汇总。`;
    const response = await submitTurn({
      workspaceId,
      requestId,
      userMessageId,
      text: requestText,
      accessProfile: "restricted",
    });
    if (response.httpStatus < 200 || response.httpStatus >= 300) {
      throw new Error(`审批取消 Turn 未被接纳：HTTP ${response.httpStatus} ${response.text}`);
    }
    const sessionId = response.body?.sessionId;
    const turnId = response.body?.turnId;
    if (typeof sessionId !== "string" || typeof turnId !== "string") {
      throw new Error(`审批取消 Turn 缺少 Session/Turn identity：${response.text}`);
    }
    const pending = await waitForPendingApproval({ sessionId, workspaceId });
    if (pending.toolName !== "shell_exec") {
      throw new Error(`审批取消场景产生了意外工具：${pending.toolName}`);
    }

    const interruptResponse = await requestJson("/api/session/interrupt", {
      method: "POST",
      body: JSON.stringify({
        sessionId,
        workspaceId,
        workspacePath: taskWorkspacePath,
      }),
    });
    if (!interruptResponse.response.ok || interruptResponse.body?.interrupted !== true) {
      throw new Error(`审批取消 Turn 未被真实 interrupt：HTTP ${interruptResponse.response.status} ${interruptResponse.text}`);
    }

    const approvalEvents = [];
    const terminal = await readSseUntil({
      sessionId,
      turnId,
      afterSequence: response.body?.eventSequence,
      workspaceId,
      timeoutMs: Math.max(turnTimeoutMs, 30_000),
      onEvent: ({ event }) => {
        const payload = event?.payload || {};
        const eventTurnId = payload.turn_id
          || payload.turnId
          || payload.canonical_turn?.turnId
          || payload.canonicalTurn?.turnId;
        if (event?.session_id !== sessionId || eventTurnId !== turnId) return;
        if (event.event_type === "tool.approval.requested"
          || event.event_type === "tool.approval.resolved") {
          approvalEvents.push({
            event_type: event.event_type,
            sequence: event.sequence,
            approval_id: payload.approval_id || payload.approvalId || null,
            payload_keys: Object.keys(payload),
          });
        }
      },
      predicate: isCanonicalTerminalEvent,
    });
    const pendingAfterCancel = await waitForNoPendingApproval({
      sessionId,
      workspaceId,
      timeoutMs: 30_000,
    });
    const sideEffect = await pathExists(target);
    const messagesAfterCancel = await messagesFor(sessionId, workspaceId);
    const cancelMessagesJson = JSON.stringify(messagesAfterCancel);
    const cancelledErrorVisible = cancelMessagesJson.includes("tool_approval_cancelled")
      || cancelMessagesJson.includes("待授权操作未执行")
      || cancelMessagesJson.includes("任务或对话轮次已停止");
    const logTextBeforeRecovery = daemon.logText();
    const cancelledProviderRequests = countProviderRequests(logTextBeforeRecovery, turnId, requestId);
    const cancelledCanonicalTerminals = countCanonicalTerminals(logTextBeforeRecovery, turnId, requestId);
    const matchingCancelledMessages = Array.isArray(messagesAfterCancel?.timeline)
      ? messagesAfterCancel.timeline.filter((entry) => entry?.message === requestText).length
      : 0;

    phase = "post_cancel_recovery_turn";
    const recoveryRequestId = `${runIdentity}-post-cancel-recovery`;
    const recoveryUserMessageId = `user-${recoveryRequestId}`;
    const recoveryText = `审批取消后的接纳检查：请调用 file_read 读取 ${join(taskWorkspacePath, "README.md")}，然后只回答 REAL_PROVIDER_APPROVAL_CANCEL_RECOVERY_OK；不要调用任何写入工具。`;
    const recoveryResponse = await submitTurn({
      sessionId,
      workspaceId,
      requestId: recoveryRequestId,
      userMessageId: recoveryUserMessageId,
      text: recoveryText,
      accessProfile: "restricted",
    });
    if (recoveryResponse.httpStatus < 200 || recoveryResponse.httpStatus >= 300) {
      throw new Error(`审批取消后的下一轮未被接纳：HTTP ${recoveryResponse.httpStatus} ${recoveryResponse.text}`);
    }
    const recoveryTurnId = recoveryResponse.body?.turnId;
    if (typeof recoveryTurnId !== "string" || recoveryResponse.body?.sessionId !== sessionId) {
      throw new Error(`审批取消后的下一轮缺少稳定 Session/Turn identity：${recoveryResponse.text}`);
    }
    const recoveryTerminal = await waitForTerminal({
      sessionId,
      turnId: recoveryTurnId,
      afterSequence: recoveryResponse.body?.eventSequence,
      workspaceId,
    });

    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const recoveryProviderRequests = countProviderRequests(logText, recoveryTurnId, recoveryRequestId);
    const recoveryCanonicalTerminals = countCanonicalTerminals(logText, recoveryTurnId, recoveryRequestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingMessages = {
      cancelled: timeline.filter((entry) => entry?.message === requestText).length,
      recovery: timeline.filter((entry) => entry?.message === recoveryText).length,
    };
    const cancelledTerminalAtMs = timingEventsForTurn(logText, {
      turnId,
      requestId,
      stage: "canonical_terminal_published",
    })[0]?.atMs ?? null;
    const recoveryAcceptedAtMs = timingEventsForTurn(logText, {
      turnId: recoveryTurnId,
      requestId: recoveryRequestId,
      stage: "accepted_response_sent",
    })[0]?.atMs ?? null;
    const firstTerminalBeforeRecoveryAdmission = Number.isFinite(cancelledTerminalAtMs)
      && Number.isFinite(recoveryAcceptedAtMs)
      && cancelledTerminalAtMs <= recoveryAcceptedAtMs;
    const resolvedApprovalEvents = approvalEvents.filter(
      (event) => event.event_type === "tool.approval.resolved",
    );
    const requestedApprovalEvents = approvalEvents.filter(
      (event) => event.event_type === "tool.approval.requested",
    );
    const passed = response.body?.executionProfile === "task"
      && recoveryResponse.body?.executionProfile === "task"
      && terminal.status === "cancelled"
      && recoveryTerminal.status === "completed"
      && pending.toolName === "shell_exec"
      && pendingAfterCancel.pendingApprovals?.length === 0
      && interruptResponse.body?.interrupted === true
      && requestedApprovalEvents.length === 1
      && resolvedApprovalEvents.length === 0
      && (cancelledErrorVisible || terminal.status === "cancelled")
      && cancelledProviderRequests >= 1
      && recoveryProviderRequests >= 1
      && cancelledCanonicalTerminals === 1
      && recoveryCanonicalTerminals === 1
      && !sideEffect
      && matchingCancelledMessages === 1
      && Object.values(matchingMessages).every((count) => count === 1)
      && firstTerminalBeforeRecoveryAdmission
      && sourceStable;
    const fixture = "real-provider-task-approval-cancel-v1";
    const samples = [
      approvalTrajectorySample({
        fixture,
        logText,
        sessionId,
        turnId,
        requestId,
        sampleIndex: 0,
        scenario: "approval_cancelled",
        requestText,
        terminal,
        providerRequestCount: cancelledProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: firstTerminalBeforeRecoveryAdmission,
      }),
      approvalTrajectorySample({
        fixture,
        logText,
        sessionId,
        turnId: recoveryTurnId,
        requestId: recoveryRequestId,
        sampleIndex: 1,
        scenario: "post_cancel_recovery",
        requestText: recoveryText,
        terminal: recoveryTerminal,
        providerRequestCount: recoveryProviderRequests,
        settlementRequired: true,
        nextTurnAdmissionChecked: false,
      }),
    ];
    evidence = {
      schema_version: "magi.real-provider.approval-cancel.v1",
      fixture,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      trajectoryMode: "mixed",
      settlement_required: true,
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      requests: {
        cancelled: {
          request_id: requestId,
          user_message_id: userMessageId,
          session_id: sessionId,
          turn_id: turnId,
          text: requestText,
          accepted_http_status: response.httpStatus,
          execution_profile: response.body?.executionProfile || null,
          terminal,
        },
        recovery: {
          request_id: recoveryRequestId,
          user_message_id: recoveryUserMessageId,
          session_id: sessionId,
          turn_id: recoveryTurnId,
          text: recoveryText,
          accepted_http_status: recoveryResponse.httpStatus,
          execution_profile: recoveryResponse.body?.executionProfile || null,
          terminal: recoveryTerminal,
        },
      },
      approvals: {
        cancelled: {
          approval_id: pending.approvalId,
          tool_name: pending.toolName,
          requested_at: pending.requestedAt || null,
          pending_before_cancel: true,
          pending_after_cancel: pendingAfterCancel.pendingApprovals?.length || 0,
          interrupt_returned: interruptResponse.body?.interrupted === true,
          approval_requested_events: requestedApprovalEvents.length,
          approval_resolved_events: resolvedApprovalEvents.length,
        },
      },
      facts: {
        cancelled_provider_request_count: cancelledProviderRequests,
        recovery_provider_request_count: recoveryProviderRequests,
        cancelled_canonical_terminal_count: cancelledCanonicalTerminals,
        recovery_canonical_terminal_count: recoveryCanonicalTerminals,
        matching_user_message_counts: matchingMessages,
        cancelled_error_visible: cancelledErrorVisible,
        cancelled_side_effect_present: sideEffect,
        first_terminal_before_recovery_admission: firstTerminalBeforeRecoveryAdmission,
        cancelled_terminal_at_ms: cancelledTerminalAtMs,
        recovery_accepted_at_ms: recoveryAcceptedAtMs,
        recovery_turn_completed: recoveryTerminal.status === "completed",
        session_identity_stable: true,
      },
      approval_events: approvalEvents,
      samples,
      logs: { daemon: `${evidencePath}.daemon.log` },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      cancelledStatus: terminal.status,
      recoveryStatus: recoveryTerminal.status,
      approvalId: pending.approvalId,
      approvalEvents: approvalEvents.length,
      providerRequests: [cancelledProviderRequests, recoveryProviderRequests],
      canonicalTerminals: [cancelledCanonicalTerminals, recoveryCanonicalTerminals],
      sideEffect,
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider approval-cancel 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.approval-cancel.v1",
        fixture: "real-provider-task-approval-cancel-v1",
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: { daemon: `${evidencePath}.daemon.log` },
      };
      if (daemon) await writeFile(evidence.logs.daemon, daemon.logText(), "utf8");
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败 approval-cancel 采样状态根：${stateParent}`);
    }
  }
}

async function runExternalMcpTool() {
  if (!isTaskProfile) {
    throw new Error("mcp-external 只验证 task profile 的真实外部 MCP 执行");
  }

  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-mcp-external-"));
  const stateRoot = join(stateParent, "state");
  taskWorkspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await prepareTaskWorkspace(taskWorkspacePath);

  const mcpServerId = "real-provider-mcp";
  const mcpToolName = "mcp__real_provider_mcp__write_fixture";
  const mcpTarget = join(taskWorkspacePath, "real-provider-mcp-side-effect.txt");
  const mcpFixturePath = join(stateParent, "real-provider-mcp-server.mjs");
  await writeFile(mcpFixturePath, externalMcpFixtureSource(), "utf8");
  const mcpRequestText = [
    "请执行一个真实外部 MCP 工具任务。必须调用工具 ",
    mcpToolName,
    "，并且参数 JSON 必须是 ",
    JSON.stringify({ path: mcpTarget }),
    "。确认工具返回 REAL_PROVIDER_MCP_TOOL_OK 后，只回答 REAL_PROVIDER_MCP_OK。",
  ].join("");
  const requestId = runIdentity + "-mcp";
  const userMessageId = "user-" + requestId;
  let daemon;
  let evidence;
  let phase = "initializing";

  try {
    daemon = spawnDaemon({ stateRoot, providerApiKey });
    const health = await waitForHealth();
    const workspaceId = await registerWorkspace(taskWorkspacePath);

    phase = "mcp_server_registered";
    const addServer = await requestJson("/api/settings/mcp/add", {
      method: "POST",
      body: JSON.stringify({
        id: mcpServerId,
        name: "Real Provider MCP",
        type: "stdio",
        command: process.execPath,
        args: [mcpFixturePath],
        workingDirectory: stateParent,
        env: { MAGI_MCP_TARGET: mcpTarget },
        enabled: true,
      }),
    });
    if (!addServer.response.ok || addServer.body?.added !== true) {
      throw new Error(
        "真实 MCP server 注册失败：HTTP "
          + addServer.response.status
          + " "
          + addServer.text,
      );
    }
    const mcpTools = await requestJson("/api/settings/mcp/tools", {
      method: "POST",
      body: JSON.stringify({ serverId: mcpServerId }),
    });
    const advertisedTool = mcpTools.body?.tools?.find((tool) => tool?.name === "write_fixture");
    if (!mcpTools.response.ok
      || mcpTools.body?.connected !== true
      || !advertisedTool) {
      throw new Error("真实 MCP tools/list 未返回目标工具：" + mcpTools.text);
    }

    phase = "turn_submitted";
    const response = await submitTurn({
      workspaceId,
      requestId,
      userMessageId,
      text: mcpRequestText,
      accessProfile: "full_access",
    });
    if (response.httpStatus < 200 || response.httpStatus >= 300) {
      throw new Error(
        "真实 Provider MCP Turn 未被接纳：HTTP "
          + response.httpStatus
          + " "
          + response.text,
      );
    }
    const sessionId = response.body?.sessionId;
    const turnId = response.body?.turnId;
    if (typeof sessionId !== "string" || typeof turnId !== "string") {
      throw new Error("真实 Provider MCP Turn 缺少 Session/Turn identity：" + response.text);
    }

    phase = "turn_settled";
    const terminal = await waitForTerminal({
      sessionId,
      turnId,
      afterSequence: response.body?.eventSequence,
      workspaceId,
    });
    const messages = await messagesFor(sessionId, workspaceId);
    const logText = daemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const providerRequests = countProviderRequests(logText, turnId, requestId);
    const canonicalTerminals = countCanonicalTerminals(logText, turnId, requestId);
    const timeline = Array.isArray(messages?.timeline) ? messages.timeline : [];
    const matchingUserMessages = timeline.filter((entry) => entry?.message === mcpRequestText).length;
    const toolCallObserved = JSON.stringify(messages).includes(mcpToolName);
    const approvalRequested = logText
      .split("\n")
      .some((line) => lineHasTurnIdentity(line, turnId, requestId)
        && line.includes("tool.approval.requested"));
    const sideEffect = await pathExists(mcpTarget);
    const sideEffectText = sideEffect ? await readFile(mcpTarget, "utf8") : null;
    const sample = approvalTrajectorySample({
      logText,
      sessionId,
      turnId,
      requestId,
      sampleIndex: 0,
      scenario: "mcp_external_full_access",
      requestText: mcpRequestText,
      terminal,
      providerRequestCount: providerRequests,
      settlementRequired: false,
      nextTurnAdmissionChecked: false,
      fixture: "real-provider-task-mcp-external-v1",
    });
    const passed = response.body?.executionProfile === "task"
      && terminal.status === "completed"
      && mcpTools.body.toolCount === 1
      && providerRequests >= 1
      && canonicalTerminals === 1
      && matchingUserMessages === 1
      && toolCallObserved
      && sideEffect
      && sideEffectText === "real-provider-mcp-side-effect\n"
      && !approvalRequested
      && sourceStable;
    evidence = {
      schema_version: "magi.real-provider.mcp.v1",
      fixture: "real-provider-task-mcp-external-v1",
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      trajectoryMode: "normal",
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        runtime_epoch: health.runtimeEpoch,
        state_root_reused: false,
        workspace_path: taskWorkspacePath,
        workspace_id: workspaceId,
      },
      mcp: {
        server_id: mcpServerId,
        tool_name: mcpToolName,
        transport: "stdio",
        advertised_tool_count: mcpTools.body.toolCount,
        advertised_tool: advertisedTool,
        target_path: mcpTarget,
      },
      request: {
        request_id: requestId,
        user_message_id: userMessageId,
        session_id: sessionId,
        turn_id: turnId,
        execution_profile: response.body?.executionProfile || null,
        access_profile: "full_access",
        request_scope: "workspace",
        request_text: mcpRequestText,
        accepted_http_status: response.httpStatus,
        terminal,
      },
      facts: {
        provider_request_count: providerRequests,
        canonical_terminal_count: canonicalTerminals,
        matching_user_message_count: matchingUserMessages,
        tool_call_observed: toolCallObserved,
        approval_requested: approvalRequested,
        approval_resolved: false,
        side_effect: sideEffect,
        side_effect_text: sideEffectText,
        mcp_server_connected: mcpTools.body.connected === true,
      },
      settlement_required: false,
      samples: [sample],
      logs: { daemon: evidencePath + ".daemon.log" },
    };
    await writeFile(evidence.logs.daemon, logText, "utf8");
    await writeFile(evidencePath, JSON.stringify(evidence, null, 2) + "\n", "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      sessionId,
      turnId,
      providerRequests,
      canonicalTerminals,
      toolCallObserved,
      sideEffect,
      approvalRequested,
    }, null, 2));
    if (!passed) {
      throw new Error("真实 Provider 外部 MCP 验收失败，证据已写入 " + evidencePath);
    }
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.mcp.v1",
        fixture: "real-provider-task-mcp-external-v1",
        generated_at: new Date().toISOString(),
        status: "failed",
        trajectoryMode: "normal",
        replay_mode: replayMode,
        phase,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
        logs: { daemon: evidencePath + ".daemon.log" },
      };
      if (daemon) await writeFile(evidence.logs.daemon, daemon.logText(), "utf8");
      await writeFile(evidencePath, JSON.stringify(evidence, null, 2) + "\n", "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(daemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error("保留失败 mcp-external 采样状态根：" + stateParent);
    }
  }
}

async function main() {
  if (replayMode === "reconnect") return runReconnect();
  if (replayMode === "websocket") return runReconnect({ transport: "websocket" });
  if (replayMode === "restart-reconnect") return runRestartReconnect();
  if (replayMode === "restart-websocket") return runRestartReconnect({ transport: "websocket" });
  if (replayMode === "long-history") return runLongHistoryReplay();
  if (replayMode === "blocked-recovery") return runBlockedRecovery();
  if (replayMode === "mcp-external") return runExternalMcpTool();
  if (replayMode === "approval-recovery") return runApprovalRecovery();
  if (replayMode === "approval-expiry") return runApprovalExpiry();
  if (replayMode === "approval-cancel") return runApprovalCancellation();
  const sourceBefore = await sourceIdentity();
  const daemonBinarySha256 = await fileSha256(daemonBinary);
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-restart-replay-"));
  const stateRoot = join(stateParent, "state");
  await mkdir(stateRoot, { recursive: true });
  if (isTaskProfile) {
    taskWorkspacePath = join(stateParent, "workspace");
    await prepareTaskWorkspace(taskWorkspacePath);
  }
  let firstDaemon;
  let secondDaemon;
  let evidence;
  try {
    firstDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const firstHealth = await waitForHealth();
    const firstWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = firstWorkspaceId;
    const firstResponse = await submitTurn({
      workspaceId: firstWorkspaceId,
      requestId: firstRequestId,
      userMessageId: firstUserMessageId,
      text: requestText,
    });
    if (firstResponse.httpStatus < 200 || firstResponse.httpStatus >= 300) {
      throw new Error(`首次真实 Provider Turn 未被接纳：HTTP ${firstResponse.httpStatus} ${firstResponse.text}`);
    }
    const firstTurnId = firstResponse.body?.turnId;
    const firstSessionId = firstResponse.body?.sessionId;
    if (typeof firstTurnId !== "string" || typeof firstSessionId !== "string") {
      throw new Error(`首次接纳缺少 Turn/session identity：${firstResponse.text}`);
    }
    const firstTerminal = await waitForTerminal({
      sessionId: firstSessionId,
      turnId: firstTurnId,
      afterSequence: firstResponse.body?.eventSequence,
      workspaceId: firstWorkspaceId,
    });
    const firstMessages = await messagesFor(firstSessionId);
    await stopDaemon(firstDaemon);
    const firstLog = firstDaemon.logText();

    secondDaemon = spawnDaemon({ stateRoot, providerApiKey });
    const secondHealth = await waitForHealth();
    const secondWorkspaceId = isTaskProfile
      ? await registerWorkspace(taskWorkspacePath)
      : null;
    workspaceIdForRun = secondWorkspaceId;
    let historyAfterRestart = null;
    let secondResponse = null;
    let secondTerminal = null;
    let secondLogBeforeReplay = "";
    let replayResponse;
    if (replayMode === "history") {
      // 先从第二个 daemon 读取历史，再提交同一 Session 的新一轮，避免把
      // “新请求能成功”误当成“重启后历史已恢复”。
      historyAfterRestart = await messagesFor(firstSessionId, secondWorkspaceId);
      secondResponse = await submitTurn({
        replay: false,
        sessionId: firstSessionId,
        workspaceId: secondWorkspaceId,
        requestId: secondRequestId,
        userMessageId: secondUserMessageId,
        text: secondRequestText,
      });
      if (secondResponse.httpStatus < 200 || secondResponse.httpStatus >= 300) {
        throw new Error(`重启后同一 Session 的 history Turn 未被接纳：HTTP ${secondResponse.httpStatus} ${secondResponse.text}`);
      }
      const acceptedSecondTurnId = secondResponse.body?.turnId;
      const acceptedSecondSessionId = secondResponse.body?.sessionId;
      if (typeof acceptedSecondTurnId !== "string" || acceptedSecondSessionId !== firstSessionId) {
        throw new Error(`重启后 history Turn 缺少稳定 Session/Turn identity：${secondResponse.text}`);
      }
      secondTerminal = await waitForTerminal({
        sessionId: firstSessionId,
        turnId: acceptedSecondTurnId,
        afterSequence: secondResponse.body?.eventSequence,
        workspaceId: secondWorkspaceId,
      });
      secondLogBeforeReplay = secondDaemon.logText();
      replayResponse = await submitTurn({
        replay: true,
        sessionId: firstSessionId,
        workspaceId: secondWorkspaceId,
        requestId: secondRequestId,
        userMessageId: secondUserMessageId,
        text: secondRequestText,
      });
    } else {
      // 保持首次接纳时的 sessionId=null，否则 request fingerprint 会发生变化，
      // 这应当被正确拒绝为冲突而不是误判为 replay。
      replayResponse = await submitTurn({
        replay: true,
        sessionId: null,
        workspaceId: firstWorkspaceId,
        requestId: firstRequestId,
        userMessageId: firstUserMessageId,
        text: requestText,
      });
    }
    const secondTurnId = secondResponse?.body?.turnId || firstTurnId;
    const secondMessages = await messagesFor(firstSessionId, secondWorkspaceId);
    const secondLog = secondDaemon.logText();
    const sourceAfter = await sourceIdentity();
    const sourceStable = sourceBefore.commit === sourceAfter.commit
      && sourceBefore.fingerprintSha256 === sourceAfter.fingerprintSha256;
    const firstProviderRequests = countProviderRequests(firstLog, firstTurnId, firstRequestId);
    const restartedFirstProviderRequests = countProviderRequests(secondLog, firstTurnId, firstRequestId);
    const secondProviderRequestId = replayMode === "history" ? secondRequestId : firstRequestId;
    const secondProviderRequestsBeforeReplay = replayMode === "history"
      ? countProviderRequests(secondLogBeforeReplay, secondTurnId, secondProviderRequestId)
      : 0;
    const secondProviderRequests = countProviderRequests(secondLog, secondTurnId, secondProviderRequestId);
    const firstCanonicalTerminals = countCanonicalTerminals(firstLog, firstTurnId, firstRequestId);
    const restartedFirstCanonicalTerminals = countCanonicalTerminals(secondLog, firstTurnId, firstRequestId);
    const secondCanonicalTerminalsBeforeReplay = replayMode === "history"
      ? countCanonicalTerminals(secondLogBeforeReplay, secondTurnId, secondProviderRequestId)
      : 0;
    const secondCanonicalTerminals = countCanonicalTerminals(secondLog, secondTurnId, secondProviderRequestId);
    const firstTimeline = Array.isArray(firstMessages?.timeline) ? firstMessages.timeline : [];
    const secondTimeline = Array.isArray(secondMessages?.timeline) ? secondMessages.timeline : [];
    const matchingFirstUsers = firstTimeline.filter((entry) => entry?.message === requestText);
    const matchingSecondUsers = secondTimeline.filter((entry) => entry?.message === (
      replayMode === "history" ? secondRequestText : requestText
    ));
    const historyAfterRestartTimeline = Array.isArray(historyAfterRestart?.timeline)
      ? historyAfterRestart.timeline
      : [];
    const historyAfterRestartFirstUsers = historyAfterRestartTimeline
      .filter((entry) => entry?.message === requestText);
    const historyAfterRestartCanonicalTurns = Array.isArray(historyAfterRestart?.canonicalTurns)
      ? historyAfterRestart.canonicalTurns
      : [];
    const historyRestored = replayMode !== "history"
      || (historyAfterRestart?.sessionId === firstSessionId
        && historyAfterRestart?.currentSession?.sessionId === firstSessionId
        && historyAfterRestartFirstUsers.length === 1
        && historyAfterRestartCanonicalTurns.some((turn) => turn?.turnId === firstTurnId));
    const workspaceStable = !isTaskProfile || firstWorkspaceId === secondWorkspaceId;
    const passed = replayMode === "history"
      ? firstTerminal.status === "completed"
        && secondTerminal?.status === "completed"
        && secondResponse?.httpStatus >= 200
        && secondResponse?.httpStatus < 300
        && replayResponse.httpStatus >= 200
        && replayResponse.httpStatus < 300
        && secondResponse.body?.turnId === secondTurnId
        && secondResponse.body?.turnId !== firstTurnId
        && secondResponse.body?.sessionId === firstSessionId
        && replayResponse.body?.turnId === secondTurnId
        && replayResponse.body?.sessionId === firstSessionId
        && firstResponse.body?.executionProfile === profile
        && secondResponse.body?.executionProfile === profile
        && replayResponse.body?.executionProfile === profile
        && firstProviderRequests >= 1
        && restartedFirstProviderRequests === 0
        && secondProviderRequestsBeforeReplay >= 1
        && secondProviderRequests === secondProviderRequestsBeforeReplay
        && firstCanonicalTerminals === 1
        && restartedFirstCanonicalTerminals === 0
        && secondCanonicalTerminalsBeforeReplay === 1
        && secondCanonicalTerminals === secondCanonicalTerminalsBeforeReplay
        && matchingFirstUsers.length === 1
        && matchingSecondUsers.length === 1
        && historyRestored
        && firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch
        && workspaceStable
        && sourceStable
      : firstTerminal.status === "completed"
        && replayResponse.httpStatus >= 200
        && replayResponse.httpStatus < 300
        && replayResponse.body?.turnId === firstTurnId
        && replayResponse.body?.sessionId === firstSessionId
        && firstResponse.body?.executionProfile === profile
        && replayResponse.body?.executionProfile === profile
        && firstProviderRequests >= 1
        && restartedFirstProviderRequests === 0
        && secondProviderRequests === 0
        && firstCanonicalTerminals === 1
        && restartedFirstCanonicalTerminals === 0
        && secondCanonicalTerminals === 0
        && matchingFirstUsers.length === 1
        && matchingSecondUsers.length === 1
        && firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch
        && workspaceStable
        && sourceStable;
    evidence = {
      schema_version: "magi.real-provider.restart-replay.v2",
      fixture: `real-provider-${profile}-${replayMode}-restart-replay-v1`,
      generated_at: new Date().toISOString(),
      status: passed ? "passed" : "failed",
      replay_mode: replayMode,
      provider: {
        base_url: providerBaseUrl,
        model: providerModel,
        reasoning_effort: reasoningEffort,
        execution_profile: profile,
      },
      source: {
        before: sourceBefore,
        after: sourceAfter,
        stable_during_sample: sourceStable,
      },
      daemon: {
        binary: daemonBinary,
        binary_sha256: daemonBinarySha256,
        port: daemonPort,
        first_runtime_epoch: firstHealth.runtimeEpoch,
        second_runtime_epoch: secondHealth.runtimeEpoch,
        state_root_reused: true,
        workspace_path: taskWorkspacePath,
        first_workspace_id: firstWorkspaceId,
        second_workspace_id: secondWorkspaceId,
      },
      request: {
        request_id: firstRequestId,
        user_message_id: firstUserMessageId,
        session_id: firstSessionId,
        turn_id: firstTurnId,
        execution_profile: firstResponse.body?.executionProfile || null,
        request_scope: isTaskProfile ? "workspace" : "personal",
        request_text: requestText,
        first_http_status: firstResponse.httpStatus,
        first_terminal: firstTerminal,
        second_request_id: replayMode === "history" ? secondRequestId : null,
        second_user_message_id: replayMode === "history" ? secondUserMessageId : null,
        second_request_text: replayMode === "history" ? secondRequestText : null,
        second_http_status: secondResponse?.httpStatus || null,
        second_body: secondResponse?.body || null,
        second_terminal: secondTerminal,
        replay_http_status: replayResponse.httpStatus,
        replay_body: replayResponse.body,
        replay_text: replayResponse.text,
        replay_turn_id: replayResponse.body?.turnId || null,
        replay_session_id: replayResponse.body?.sessionId || null,
      },
      facts: {
        first_provider_request_count: firstProviderRequests,
        restarted_first_provider_request_count: restartedFirstProviderRequests,
        second_provider_request_count_before_replay: secondProviderRequestsBeforeReplay,
        second_provider_request_count: secondProviderRequests,
        first_canonical_terminal_count: firstCanonicalTerminals,
        restarted_first_canonical_terminal_count: restartedFirstCanonicalTerminals,
        second_canonical_terminal_count_before_replay: secondCanonicalTerminalsBeforeReplay,
        second_canonical_terminal_count: secondCanonicalTerminals,
        first_matching_user_message_count: matchingFirstUsers.length,
        second_matching_user_message_count: matchingSecondUsers.length,
        history_after_restart_first_user_message_count: historyAfterRestartFirstUsers.length,
        history_after_restart_canonical_turn_count: historyAfterRestartCanonicalTurns.length,
        history_restored: historyRestored,
        new_turn_after_restart: replayMode === "history",
        runtime_epoch_changed: firstHealth.runtimeEpoch !== secondHealth.runtimeEpoch,
        workspace_identity_stable: workspaceStable,
      },
      logs: {
        first: `${evidencePath}.first.daemon.log`,
        second: `${evidencePath}.second.daemon.log`,
      },
    };
    await writeFile(evidence.logs.first, firstLog, "utf8");
    await writeFile(evidence.logs.second, secondLog, "utf8");
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    console.log(JSON.stringify({
      status: evidence.status,
      evidencePath,
      replayMode,
      firstProviderRequests,
      secondProviderRequests,
      firstTurnId,
      secondTurnId,
      replayTurnId: replayResponse.body?.turnId || null,
      firstMatchingUserMessageCount: matchingFirstUsers.length,
      secondMatchingUserMessageCount: matchingSecondUsers.length,
      historyRestored,
    }, null, 2));
    if (!passed) throw new Error(`真实 Provider restart/replay 验收失败，证据已写入 ${evidencePath}`);
  } catch (error) {
    if (!evidence) {
      evidence = {
        schema_version: "magi.real-provider.restart-replay.v2",
        fixture: `real-provider-${profile}-${replayMode}-restart-replay-v1`,
        generated_at: new Date().toISOString(),
        status: "failed",
        replay_mode: replayMode,
        error: error instanceof Error ? error.message : String(error),
        source: { before: sourceBefore },
        daemon: {
          binary: daemonBinary,
          binary_sha256: daemonBinarySha256,
          port: daemonPort,
          execution_profile: profile,
          workspace_path: taskWorkspacePath,
        },
      };
      await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
    }
    throw error;
  } finally {
    await stopDaemon(firstDaemon);
    await stopDaemon(secondDaemon);
    if (!keepStateOnFailure || evidence?.status === "passed") {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败采样状态根：${stateParent}`);
    }
  }
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error(error instanceof Error ? error.stack || error.message : error);
    process.exit(1);
  });
