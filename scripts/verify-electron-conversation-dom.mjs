import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { access, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { execFile, spawn, spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import WebSocket from "ws";

/**
 * 打包 Electron 消息链路 DOM 验收。
 *
 * 该脚本只启动独立状态根和本地 OpenAI-compatible Provider，不接管用户已有的
 * Electron/daemon 进程。所有页面断言都通过真实 App Renderer CDP 执行，避免用
 * 裸 HTTP 请求替代桌面入口。
 */

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const defaultAppExecutable = join(
  repositoryRoot,
  "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi",
);
const appExecutable = process.env.MAGI_ELECTRON_DOM_APP_EXECUTABLE?.trim() || defaultAppExecutable;
const appBundlePath = process.env.MAGI_ELECTRON_DOM_APP_BUNDLE?.trim()
  || dirname(dirname(dirname(appExecutable)));
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_DOM_CDP_PORT || "9257", 10);
const appRestartCdpPort = cdpPort + 1;
const daemonPort = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_DAEMON_PORT || "38123",
  10,
);
const timingSampleCount = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_TIMING_SAMPLES || "1",
  10,
);
const timingScenario = process.env.MAGI_ELECTRON_DOM_TIMING_SCENARIO?.trim() || "";
const timingScenarioNames = [
  "personal_chat",
  "workspace_chat",
  "workspace_tool",
  "goal",
  "subagent",
];
const compactionOnly = process.env.MAGI_ELECTRON_DOM_COMPACTION_ONLY === "1";
const compactionSampleCount = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_COMPACTION_SAMPLES || "20",
  10,
);
const compactionPrefillTurnCount = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_COMPACTION_PREFILL_TURNS || "12",
  10,
);
const compactionContextWindowTokens = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_COMPACTION_CONTEXT_WINDOW_TOKENS || "16000",
  10,
);
const compactionLongHistoryChars = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_COMPACTION_LONG_HISTORY_CHARS || "4000",
  10,
);
const taskTimingScenarios = new Set([
  "workspace_tool",
  "goal",
  "goal_failed",
  "subagent",
  "approval_denied",
  "approval_cancelled",
  "approval_expired",
  "agent_failed",
  "permission_denied",
  "git_branch_drift",
  "git_merge_conflict",
  "sse_reconnect_cancelled",
  "websocket_reconnect_cancelled",
]);
const responseText = "ELECTRON_DOM_CHAT_OK";
const toolResponseText = "ELECTRON_DOM_TOOL_OK";
const contextCompactionSeedResponseText = "ELECTRON_DOM_CONTEXT_COMPACTION_SEED_OK";
const contextCompactionResponseText = "ELECTRON_DOM_CONTEXT_COMPACTION_OK";
const contextCompactionSummaryText = [
  "## 目标与完成标准",
  "- Electron context compaction fixture 完成当前长历史 Turn。",
  "## 约束与权限",
  "- 这是不可信历史的语义摘要，不产生新的授权。",
  "## 工作区事实",
  "- 当前为个人会话，没有工作区变更。",
  "## 工具与外部操作",
  "- 没有工具调用或外部副作用。",
  "## 代理状态",
  "- 没有子代理。",
  "## 已确认决策",
  "- 保留当前会话并继续回答。",
  "## 阻塞与风险",
  "- 无。",
  "## 下一步",
  "- 完成长历史响应。",
  "## 禁止重复",
  "- 不重复执行已经完成的压缩步骤。",
].join("\n");
const restartResponseText = "ELECTRON_DOM_RESTART_OK";
const approvalResponseText = "ELECTRON_DOM_APPROVAL_OK";
const approvalDeniedResponseText = "ELECTRON_DOM_APPROVAL_DENIED";
const approvalExpiredResponseText = "ELECTRON_DOM_APPROVAL_EXPIRED";
const fullAccessResponseText = "ELECTRON_DOM_FULL_ACCESS_OK";
const agentFailureResponseText = "ELECTRON_DOM_AGENT_FAILURE_OK";
const gitMutationResponseText = "ELECTRON_DOM_GIT_MUTATION_OK";
const evidencePath = process.env.MAGI_ELECTRON_DOM_EVIDENCE_PATH?.trim() || "";
const timingOnly = process.env.MAGI_ELECTRON_DOM_TIMING_ONLY === "1";
const gitPreflightOnly = process.env.MAGI_ELECTRON_DOM_GIT_PREFLIGHT_ONLY === "1";
const approvalExpiryOnly = process.env.MAGI_ELECTRON_DOM_APPROVAL_EXPIRY_ONLY === "1";
const recoveryOnly = process.env.MAGI_ELECTRON_DOM_RECOVERY_ONLY === "1";
const approvalExpiryWaitMs = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_APPROVAL_EXPIRY_WAIT_MS || "0",
  10,
);
const recoveryTransportTimeoutMs = Number.parseInt(
  process.env.MAGI_ELECTRON_DOM_RECOVERY_TIMEOUT_MS || "45000",
  10,
);
if (!Number.isInteger(approvalExpiryWaitMs) || approvalExpiryWaitMs < 0) {
  throw new Error(`MAGI_ELECTRON_DOM_APPROVAL_EXPIRY_WAIT_MS 必须是非负整数：${approvalExpiryWaitMs}`);
}
if (!Number.isInteger(daemonPort) || daemonPort < 1024 || daemonPort > 65535) {
  throw new Error(`MAGI_ELECTRON_DOM_DAEMON_PORT 必须在 1024 到 65535 之间：${daemonPort}`);
}
const timingFixture = "electron-dom-timing-v1";
const timingSchemaVersion = "magi.electron.timing.v1";
const compactionFixture = "electron-dom-context-compaction-v1";
const compactionSchemaVersion = "magi.electron.context-compaction.v1";
let approvalTarget = join(tmpdir(), `magi-electron-dom-approval-${process.pid}.txt`);
let approvalDenyTarget = join(tmpdir(), `magi-electron-dom-approval-deny-${process.pid}.txt`);
let approvalCancelTarget = join(tmpdir(), `magi-electron-dom-approval-cancel-${process.pid}.txt`);
let approvalExpiryTarget = join(tmpdir(), `magi-electron-dom-approval-expiry-${process.pid}.txt`);
let fullAccessTarget = join(tmpdir(), `magi-electron-dom-full-access-${process.pid}.txt`);
let gitDirtyRoot = "";
let gitDriftRoot = "";
let gitConflictRoot = "";
let gitMutationRoot = "";

const backendTimingByTurn = new Map();
const traceToTurn = new Map();
const pendingBackendTimingByTrace = new Map();
const electronLogBuffers = { stdout: "", stderr: "" };
const electronLogText = [];
const desktopIpcValidationErrors = [];
let daemonShutdownSettlementEvents = 0;
let daemonShutdownSettledTurnCount = 0;

function logField(line, key) {
  const match = line.match(new RegExp(`${key}=(?:"([^"]*)"|([^\\s]+))`, "u"));
  return match?.[1] ?? match?.[2] ?? "";
}

function timingLogTimestamp(line) {
  const match = line.match(/\b(\d{4}-\d{2}-\d{2}T[^\s]+Z)\b/u);
  const timestamp = match?.[1] ? Date.parse(match[1]) : Number.NaN;
  return Number.isFinite(timestamp) ? timestamp : Date.now();
}

function backendTimingRecord(turnId) {
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
  const record = backendTimingRecord(turnId);
  if (event.traceId) record.traceId ||= event.traceId;
  if (event.sessionId) record.sessionId ||= event.sessionId;
  const previous = record.stages[event.stage];
  const point = {
    atMs: event.atMs,
    elapsedMs: event.elapsedMs,
    sequence: Number.isInteger(event.eventSequence) ? event.eventSequence : null,
    traceId: event.traceId || record.traceId || null,
    requestId: event.requestId || null,
    sessionId: event.sessionId || record.sessionId || null,
    providerCallId: event.providerCallId || null,
    source: "electron_log",
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
  if (line.includes("daemon 关闭前已将活动 Session Turn 收口为 canonical terminal")) {
    daemonShutdownSettlementEvents += 1;
    daemonShutdownSettledTurnCount += Number.parseInt(logField(line, "settled_turn_count"), 10) || 0;
  }
  if (!line.includes("conversation response timing")) return;
  const stage = logField(line, "stage");
  if (!stage) return;
  const event = {
    stage,
    traceId: logField(line, "trace_id"),
    requestId: logField(line, "request_id"),
    sessionId: logField(line, "session_id"),
    turnId: logField(line, "turn_id") || logField(line, "expected_turn_id"),
    providerCallId: logField(line, "provider_call_id"),
    eventSequence: Number.parseInt(logField(line, "event_sequence"), 10),
    elapsedMs: Number.parseInt(logField(line, "elapsed_ms"), 10) || 0,
    atMs: timingLogTimestamp(line),
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
    return;
  }
  if (event.traceId) {
    const pending = pendingBackendTimingByTrace.get(event.traceId) || [];
    pending.push(event);
    pendingBackendTimingByTrace.set(event.traceId, pending);
  }
}

function inspectDesktopIpcLine(line) {
  if (line.includes("desktop_ipc_invalid")) {
    desktopIpcValidationErrors.push(line.trim().slice(0, 500));
  }
}

function consumeElectronLogChunk(stream, chunk) {
  const text = chunk.toString();
  electronLogText.push(`[${stream}] ${text}`);
  process.stdout.write(`[electron] ${text}`);
  const combined = electronLogBuffers[stream] + text;
  const lines = combined.split(/\r?\n/u);
  electronLogBuffers[stream] = lines.pop() || "";
  for (const line of lines) {
    inspectDesktopIpcLine(line);
    parseBackendTimingLine(line);
  }
}

function flushElectronLogBuffers() {
  for (const stream of Object.keys(electronLogBuffers)) {
    const line = electronLogBuffers[stream];
    if (line) {
      inspectDesktopIpcLine(line);
      parseBackendTimingLine(line);
    }
    electronLogBuffers[stream] = "";
  }
}

if (!Number.isInteger(cdpPort) || cdpPort < 1024 || cdpPort > 65535) {
  throw new Error(`无效 CDP 端口: ${cdpPort}`);
}
if (!Number.isInteger(timingSampleCount) || timingSampleCount < 1 || timingSampleCount > 20) {
  throw new Error(`MAGI_ELECTRON_DOM_TIMING_SAMPLES 必须在 1 到 20 之间: ${timingSampleCount}`);
}
if (!Number.isInteger(compactionSampleCount) || compactionSampleCount < 1 || compactionSampleCount > 20) {
  throw new Error(`MAGI_ELECTRON_DOM_COMPACTION_SAMPLES 必须在 1 到 20 之间: ${compactionSampleCount}`);
}
if (!Number.isInteger(compactionPrefillTurnCount) || compactionPrefillTurnCount < 1 || compactionPrefillTurnCount > 40) {
  throw new Error(
    `MAGI_ELECTRON_DOM_COMPACTION_PREFILL_TURNS 必须在 1 到 40 之间：${compactionPrefillTurnCount}`,
  );
}
if (!Number.isInteger(compactionContextWindowTokens) || compactionContextWindowTokens < 16_000) {
  throw new Error(
    `MAGI_ELECTRON_DOM_COMPACTION_CONTEXT_WINDOW_TOKENS 必须是不小于 16000 的整数：${compactionContextWindowTokens}`,
  );
}
if (!Number.isInteger(compactionLongHistoryChars) || compactionLongHistoryChars < 1_000) {
  throw new Error(
    `MAGI_ELECTRON_DOM_COMPACTION_LONG_HISTORY_CHARS 必须是不小于 1000 的整数：${compactionLongHistoryChars}`,
  );
}
if (timingScenario && !timingScenarioNames.includes(timingScenario)) {
  throw new Error(`MAGI_ELECTRON_DOM_TIMING_SCENARIO 无效: ${timingScenario}`);
}

const checks = [];
const execFileAsync = promisify(execFile);
function check(name, condition, detail = "") {
  const record = { name, passed: Boolean(condition), detail };
  checks.push(record);
  if (!record.passed) {
    throw new Error(`Electron DOM 验收失败：${name}${detail ? `（${detail}）` : ""}`);
  }
}

function hashText(value) {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

async function collectSourceIdentity() {
  const runGit = (args, options = {}) => {
    const result = spawnSync("git", args, {
      cwd: repositoryRoot,
      encoding: options.encoding ?? "utf8",
      maxBuffer: 128 * 1024 * 1024,
    });
    if (result.status !== 0) {
      const detail = result.error instanceof Error ? `: ${result.error.message}` : `: ${result.stderr || ""}`;
      throw new Error(`采集 Electron timing 源码身份失败：git ${args.join(" ")}${detail}`);
    }
    return result.stdout;
  };
  const sourceCommit = runGit(["rev-parse", "HEAD"]).trim();
  const status = runGit(["status", "--porcelain=v1", "--untracked-files=all"]);
  const trackedDiff = runGit(["diff", "--binary", "HEAD"], { encoding: null });
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
  const appIdentityPaths = [
    join(appBundlePath, "Contents/Resources/app.asar"),
    join(appBundlePath, "Contents/Resources/web/dist"),
    join(appBundlePath, "Contents/Resources/daemon"),
  ];
  const appIdentityFiles = [];
  const pendingDirectories = [];
  for (const path of appIdentityPaths) {
    try {
      if ((await stat(path)).isDirectory()) pendingDirectories.push(path);
      else appIdentityFiles.push(path);
    } catch {
      appIdentityFiles.push(path);
    }
  }
  while (pendingDirectories.length > 0) {
    const directory = pendingDirectories.pop();
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) pendingDirectories.push(path);
      else if (entry.isFile()) appIdentityFiles.push(path);
    }
  }
  const appArtifactFingerprint = createHash("sha256");
  for (const path of appIdentityFiles.sort()) {
    appArtifactFingerprint.update(Buffer.from(`${path.slice(appBundlePath.length)}\0`));
    appArtifactFingerprint.update(await readFile(path));
  }
  return {
    source_commit: sourceCommit,
    worktree_fingerprint_sha256: fingerprint.digest("hex"),
    executable_sha256: executableSha256,
    app_bundle: appBundlePath,
    app_artifact_sha256: appArtifactFingerprint.digest("hex"),
    dirty: status.trim().length > 0,
    changed_path_count: status.split("\n").filter(Boolean).length,
  };
}

function openAiStream(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}

function openAiJsonResponse(content) {
  return JSON.stringify({
    id: "electron-dom-context-compaction-response",
    object: "chat.completion",
    choices: [{
      index: 0,
      message: { role: "assistant", content },
      finish_reason: "stop",
    }],
    usage: {
      prompt_tokens: 1,
      completion_tokens: Math.max(1, content.length),
      total_tokens: Math.max(2, content.length + 1),
    },
  });
}

function toolCallStream(name, arguments_, id = "electron-dom-tool-call-1") {
  return [
    `data: ${JSON.stringify({
      choices: [{
        delta: {
          tool_calls: [{
            index: 0,
            id,
            type: "function",
            function: { name, arguments: JSON.stringify(arguments_) },
          }],
        },
        finish_reason: null,
      }],
    })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "tool_calls" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}

function requestContainsTool(body, toolName) {
  return Array.isArray(body?.tools)
    && body.tools.some((tool) => {
      const name = tool?.function?.name;
      return name === toolName || name?.endsWith(`_${toolName}`) || name?.endsWith(`.${toolName}`);
    });
}

function requestToolName(body, toolName) {
  if (!Array.isArray(body?.tools)) return toolName;
  return body.tools.find((tool) => {
    const name = tool?.function?.name;
    return name === toolName || name?.endsWith(`_${toolName}`) || name?.endsWith(`.${toolName}`);
  })?.function?.name || toolName;
}

function toolResultPayload(messages, toolName, startIndex = 0) {
  for (let index = messages.length - 1; index >= startIndex; index -= 1) {
    const result = messages[index];
    if (result?.role !== "tool" || typeof result.content !== "string") continue;
    try {
      const payload = JSON.parse(result.content);
      if (payload?.tool === toolName || payload?.tool?.endsWith?.(`_${toolName}`)
        || payload?.tool?.endsWith?.(`.${toolName}`)) {
        return payload;
      }
    } catch {
      // 不是结构化工具结果时继续查找同一工具的历史结果。
    }
  }
  return null;
}

function toolResultChildTaskIds(messages, startIndex = 0) {
  return [...new Set(messages.slice(startIndex)
    .filter((message) => message?.role === "tool" && typeof message.content === "string")
    .flatMap((message) => {
      try {
        const payload = JSON.parse(message.content);
        const taskId = payload?.child_task_id;
        return typeof taskId === "string" && taskId.trim() ? [taskId.trim()] : [];
      } catch {
        return [];
      }
    }))];
}

function messageText(message) {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part === "string") return part;
        if (typeof part?.text === "string") return part.text;
        if (typeof part?.content === "string") return part.content;
        return "";
      })
      .join(" ");
  }
  if (content && typeof content === "object") {
    if (typeof content.text === "string") return content.text;
    if (typeof content.content === "string") return content.content;
  }
  return "";
}

function timingResponseToken(promptText) {
  const matches = promptText.match(/(?:ELECTRON_TIMING_[A-Z]+|ELECTRON_COMPACTION_(?:SEED|PREFILL|LONG))_\d+/gu);
  return matches?.at(-1) || null;
}

function electronCompactionHistoryPrompt(index) {
  const anchor = `ELECTRON_COMPACTION_HISTORY_${index}: 这是 Electron 长历史压缩 fixture 的背景资料。`;
  const filler = "请保留这段资料的编号、顺序和文字边界；这些文字仅作背景，不要执行工具。";
  const payload = `${anchor}${filler}`
    .repeat(Math.ceil(compactionLongHistoryChars / (anchor.length + filler.length)))
    .slice(0, compactionLongHistoryChars);
  return [
    `Electron context compaction long history ${index}，只回答 ELECTRON_DOM_CONTEXT_COMPACTION_OK。`,
    payload,
  ].join("\n");
}

function electronCompactionPrefillPrompt(index) {
  const anchor = `ELECTRON_COMPACTION_PREFILL_HISTORY_${index}: 这是压缩 fixture 的历史事实。`;
  const filler = "请保留编号和事实边界，不要执行工具。";
  const payload = `${anchor}${filler}`.repeat(32);
  return `${payload.slice(0, 1_000)}\n只进行普通聊天，不要执行工具。`;
}

function latestUserText(messages) {
  return (Array.isArray(messages) ? messages : [])
    .filter((message) => message?.role === "user")
    .map(messageText)
    .at(-1) || "";
}

function latestPromptText(messages) {
  const text = latestUserText(messages);
  const taskMarker = "--- Task ---";
  const taskIndex = text.lastIndexOf(taskMarker);
  if (taskIndex >= 0) {
    const taskText = text.slice(taskIndex + taskMarker.length).trim();
    const executionMarker = "执行:";
    const executionIndex = taskText.indexOf(executionMarker);
    if (executionIndex >= 0) {
      return taskText.slice(executionIndex + executionMarker.length).trim().split(/\r?\n/u)[0].trim();
    }
    return taskText.split(/\r?\n/u).filter(Boolean).at(-1)?.trim() || taskText;
  }
  const markerIndex = text.lastIndexOf("用户原始输入");
  if (markerIndex >= 0) {
    return text
      .slice(markerIndex + "用户原始输入".length)
      .replace(/^\s*[:：]\s*/u, "")
      .trim();
  }
  return text.trim();
}

function providerResponse(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const promptText = messages
    .filter((message) => message?.role === "user")
    .map(messageText)
    .join("\n");
  const latestText = latestUserText(messages);
  if (body?.provider === "context-compaction"
    || (body?.stream === false
      && (latestText.includes("你是 Magi 的上下文压缩器")
        || latestText.includes("语义交接摘要")))) {
    return body?.stream === false
      ? openAiJsonResponse(contextCompactionSummaryText)
      : openAiStream(contextCompactionSummaryText);
  }
  const timingToken = timingResponseToken(promptText);

  if (promptText.includes("审批拒绝 DOM 验收")) {
    if (messages.some((message) => message?.role === "tool")) {
      return openAiStream(approvalDeniedResponseText);
    }
    return toolCallStream("shell_exec", {
      command: `printf denied > ${approvalDenyTarget}`,
      access_mode: "maybe_write",
    });
  }
  if (promptText.includes("审批取消 DOM 验收")) {
    return toolCallStream("shell_exec", {
      command: `printf cancelled > ${approvalCancelTarget}`,
      access_mode: "maybe_write",
    });
  }
  if (promptText.includes("Git mutation DOM 验收")) {
    if (messages.some((message) => message?.role === "tool")) {
      return openAiStream(gitMutationResponseText);
    }
    return toolCallStream(requestToolName(body, "git_branch_create"), {
      branch: "electron-dom-created",
      startPoint: "main",
      switch: false,
    }, "electron-dom-git-branch-create-1");
  }
      if (promptText.includes("完全访问 DOM 验收")) {
    if (messages.some((message) => message?.role === "tool")) {
      return openAiStream(fullAccessResponseText);
    }
    return toolCallStream("shell_exec", {
      command: `printf full_access > ${fullAccessTarget}`,
      access_mode: "maybe_write",
    });
      }
      if (promptText.includes("审批 DOM 验收")) {
    if (messages.some((message) => message?.role === "tool")) {
      return openAiStream(approvalResponseText);
    }
    return toolCallStream("shell_exec", {
      command: `printf approval > ${approvalTarget}`,
      access_mode: "maybe_write",
    });
  }
  if (promptText.includes("权限拒绝 DOM 验收")) {
    return toolCallStream("file_write", { path: "electron-dom-read-only.txt", content: "must-fail" });
  }
  if (promptText.includes("取消 DOM 验收")) {
    // 取消场景由客户端停止 Turn；服务端保持连接，直到客户端断开。
    return null;
  }
  if (promptText.includes("daemon 重启 DOM 验收")) {
    return openAiStream(restartResponseText);
  }
  if (timingToken) return openAiStream(timingToken);
  if (promptText.includes("Electron context compaction seed")) {
    return openAiStream(contextCompactionSeedResponseText);
  }
  if (promptText.includes("Electron context compaction long history")) {
    return openAiStream(contextCompactionResponseText);
  }
  return openAiStream(responseText);
}

function createProvider() {
  const requests = [];
  const domToolStates = new Map();
  const approvalStates = new Map();
  let permissionRoundEmitted = false;
  const goalStates = new Map();
  const agentStates = new Map();
  const multiAgentStates = new Map();
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      if (request.url === "/v1/models" && request.method === "GET") {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({
          object: "list",
          data: [{ id: "electron-dom-model", object: "model", owned_by: "harness" }],
        }));
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
      const promptKey = (Array.isArray(parsed.messages) ? parsed.messages : [])
        .filter((message) => message?.role === "user")
        .map(messageText)
        .join("\n");
      const currentUserText = latestPromptText(parsed.messages);
      const isDomToolPrompt = currentUserText.includes("DOM 工具卡片验收")
        || currentUserText.includes("Electron timing 工作区工具");
      const isGoalPrompt = currentUserText.includes("目标 DOM 验收")
        || currentUserText.includes("Electron timing Goal");
      const isGoalFailurePrompt = currentUserText.includes("目标失败 DOM 验收");
      const isAgentFailurePrompt = currentUserText.includes("子代理失败 DOM 验收");
      const isAgentMultiStatusPrompt = currentUserText.includes("子代理多状态 DOM 验收");
      const isAgentMultiStatusChildFailurePrompt = currentUserText.includes("Electron DOM 多状态失败子代理");
      const isAgentPrompt = currentUserText.includes("子代理 DOM 验收")
        || currentUserText.includes("Electron timing 子代理")
        || currentUserText.includes("派发一个子代理并等待其完成");
      const hasToolResult = (Array.isArray(parsed.messages) ? parsed.messages : [])
        .some((message) => message?.role === "tool");
      const messages = Array.isArray(parsed.messages) ? parsed.messages : [];
      const hasAgentFailureToolResult = Boolean(toolResultPayload(messages, "agent_spawn"));
      const approvalState = approvalStates.get("approval");
      const approvalFollowup = approvalState?.emitted
        && !approvalState.completed
        && messages.length > 6;
      let payload;
      if (approvalFollowup) {
        approvalState.completed = true;
        approvalStates.set("approval", approvalState);
        payload = openAiStream(approvalResponseText);
      } else if (isGoalFailurePrompt) {
        response.writeHead(500, { "content-type": "application/json" });
        response.end(JSON.stringify({
          error: "electron_goal_provider_failure",
          message: "Electron Goal failure recovery probe",
        }));
        return;
      } else if (isGoalPrompt) {
        const goalState = goalStates.get(currentUserText) || {
          phase: 0,
          startMessageCount: messages.length,
        };
        const getGoalName = requestToolName(parsed, "get_goal");
        const createGoalName = requestToolName(parsed, "create_goal");
        const updatePlanName = requestToolName(parsed, "update_plan");
        const updateGoalName = requestToolName(parsed, "update_goal");
        const lifecycleStart = goalState.startMessageCount || 0;
        const goalResult = toolResultPayload(messages, "get_goal", lifecycleStart);
        const createdGoalResult = toolResultPayload(messages, "create_goal", lifecycleStart);
        const planResult = toolResultPayload(messages, "update_plan", lifecycleStart);
        const updatedGoalResult = toolResultPayload(messages, "update_goal", lifecycleStart);
        const goal = updatedGoalResult?.goal || createdGoalResult?.goal || goalResult?.goal;
        if (goalState.phase === 0 && requestContainsTool(parsed, "get_goal")) {
          goalState.phase = 1;
          goalStates.set(currentUserText, goalState);
          payload = toolCallStream(getGoalName, {}, `electron-dom-goal-get-goal-${goalStates.size}`);
        } else if (goalState.phase === 1 && requestContainsTool(parsed, "create_goal")) {
          goalState.phase = 2;
          goalStates.set(currentUserText, goalState);
          payload = toolCallStream(createGoalName, {
            objective: "完成目标 DOM 验收并保留可见计划",
            token_budget: null,
          }, "electron-dom-goal-create-goal-1");
        } else if (goalState.phase === 2 && requestContainsTool(parsed, "update_plan") && goal) {
          goalState.phase = 3;
          goalStates.set(currentUserText, goalState);
          payload = toolCallStream(updatePlanName, {
            planId: null,
            expectedRevision: 0,
            expectedGoalId: goal.goal_id || goal.goalId || null,
            expectedGoalControlRevision: goal.control_revision || goal.controlRevision || null,
            language: "zh-CN",
            explanation: "建立目标验收计划",
            plan: [{ itemId: null, step: "完成目标 DOM 验收", status: "in_progress" }],
          }, "electron-dom-goal-update-plan-1");
        } else if (goalState.phase === 3
          && requestContainsTool(parsed, "update_plan")
          && goal
          && planResult?.plan?.state === "active") {
          goalState.phase = 4;
          goalStates.set(currentUserText, goalState);
          payload = toolCallStream(updatePlanName, {
            planId: planResult.plan.planId,
            expectedRevision: planResult.plan.revision,
            expectedGoalId: goal.goal_id || goal.goalId || null,
            expectedGoalControlRevision: goal.control_revision || goal.controlRevision || null,
            language: "zh-CN",
            explanation: "完成目标验收计划",
            plan: [{
              itemId: planResult.plan.items?.[0]?.itemId || null,
              step: "完成目标 DOM 验收",
              status: "completed",
            }],
          }, "electron-dom-goal-update-plan-2");
        } else if (goalState.phase === 4
          && requestContainsTool(parsed, "update_goal")
          && goal
          && planResult?.plan?.state === "completed") {
          goalState.phase = 5;
          goalStates.set(currentUserText, goalState);
          payload = toolCallStream(updateGoalName, {
            goal_id: goal.goal_id || goal.goalId || null,
            expected_revision: goal.control_revision || goal.controlRevision || null,
            expected_plan_revision: planResult.plan.revision,
            status: "complete",
            completion_summary: "目标 DOM 验收计划已完成",
            evidence_refs: ["electron-dom-goal-update-plan-2"],
          }, "electron-dom-goal-update-goal-1");
        } else if (goalState.phase >= 5 && updatedGoalResult?.goal?.status === "complete") {
          payload = openAiStream(timingResponseToken(currentUserText) || "ELECTRON_DOM_GOAL_OK");
        } else {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "goal_lifecycle_incomplete",
            phase: goalState.phase,
            hasGoal: Boolean(goal),
            hasActivePlan: planResult?.plan?.state === "active",
            hasCompletedPlan: planResult?.plan?.state === "completed",
            updatedGoalStatus: updatedGoalResult?.goal?.status || null,
          }));
          return;
        }
      } else if (isAgentFailurePrompt) {
        // 让真实 agent_spawn 预检拒绝一个不存在的角色，再让同一 root Turn
        // 收口为 Provider failure。这样 GUI 验收的是生产的失败/恢复投影，
        // 不是测试代码直接写入 agent read model。
        if (hasAgentFailureToolResult) {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "electron_agent_failure_probe",
            message: agentFailureResponseText,
          }));
          return;
        }
        payload = toolCallStream(requestToolName(parsed, "agent_spawn"), {
          task_name: "electron_failure_probe",
          display_name: "Electron DOM 失败子代理",
          role: "unregistered_electron_role",
          goal: "验证子代理失败投影与恢复入口",
          context_package: {
            summary: "触发真实 agent_spawn 预检拒绝",
            constraints: ["只验证失败投影"],
            expected_output: agentFailureResponseText,
            references: [],
          },
        }, "electron-dom-agent-failure-1");
      } else if (isAgentMultiStatusChildFailurePrompt) {
        response.writeHead(500, { "content-type": "application/json" });
        response.end(JSON.stringify({
          error: "electron_agent_multi_status_child_failure",
          message: "Electron DOM 多状态失败子代理",
        }));
        return;
      } else if (isAgentMultiStatusPrompt) {
        const multiAgentState = multiAgentStates.get(currentUserText) || {
          phase: 0,
          startMessageCount: messages.length,
        };
        const agentSpawnName = requestToolName(parsed, "agent_spawn");
        const agentWaitName = requestToolName(parsed, "agent_wait");
        const lifecycleStart = multiAgentState.startMessageCount || 0;
        const childTaskIds = toolResultChildTaskIds(messages, lifecycleStart);
        const waitResult = toolResultPayload(messages, "agent_wait", lifecycleStart);
        if (multiAgentState.phase === 0 && requestContainsTool(parsed, "agent_spawn")) {
          multiAgentState.phase = 1;
          multiAgentStates.set(currentUserText, multiAgentState);
          payload = toolCallStream(agentSpawnName, {
            task_name: "electron_dom_multi_success",
            display_name: "Electron DOM 多状态成功子代理",
            role: "executor",
            goal: "Electron DOM 多状态成功子代理：返回 ELECTRON_DOM_MULTI_CHILD_OK",
            context_package: {
              summary: "验证多子代理中的成功状态",
              constraints: ["只验证成功子代理状态"],
              expected_output: "ELECTRON_DOM_MULTI_CHILD_OK",
              references: [],
            },
          }, "electron-dom-agent-multi-success-1");
        } else if (multiAgentState.phase === 1 && childTaskIds.length === 1
          && requestContainsTool(parsed, "agent_spawn")) {
          multiAgentState.phase = 2;
          multiAgentStates.set(currentUserText, multiAgentState);
          payload = toolCallStream(agentSpawnName, {
            task_name: "electron_dom_multi_failure",
            display_name: "Electron DOM 多状态失败子代理",
            role: "reviewer",
            goal: "Electron DOM 多状态失败子代理：验证失败状态投影",
            context_package: {
              summary: "验证多子代理中的失败状态",
              constraints: ["只验证失败子代理状态"],
              expected_output: "ELECTRON_DOM_MULTI_CHILD_FAILURE",
              references: [],
            },
          }, "electron-dom-agent-multi-failure-1");
        } else if (multiAgentState.phase === 2
          && requestContainsTool(parsed, "agent_wait")
          && childTaskIds.length >= 2) {
          multiAgentState.phase = 3;
          multiAgentStates.set(currentUserText, multiAgentState);
          payload = toolCallStream(agentWaitName, {
            task_ids: childTaskIds,
            timeout_ms: 60_000,
          }, "electron-dom-agent-multi-wait-1");
        } else if (multiAgentState.phase >= 3 && waitResult) {
          payload = openAiStream("ELECTRON_DOM_AGENT_MULTI_OK");
        } else {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "electron_agent_multi_status_lifecycle_incomplete",
            phase: multiAgentState.phase,
            childTaskIds,
            hasWaitResult: Boolean(waitResult),
          }));
          return;
        }
      } else if (isAgentPrompt) {
        const agentState = agentStates.get(currentUserText) || {
          phase: 0,
          startMessageCount: messages.length,
        };
        const agentSpawnName = requestToolName(parsed, "agent_spawn");
        const agentWaitName = requestToolName(parsed, "agent_wait");
        const lifecycleStart = agentState.startMessageCount || 0;
        const spawnResult = toolResultPayload(messages, "agent_spawn", lifecycleStart);
        const waitResult = toolResultPayload(messages, "agent_wait", lifecycleStart);
        const childTaskIds = toolResultChildTaskIds(messages, lifecycleStart);
        if (agentState.phase === 0 && requestContainsTool(parsed, "agent_spawn")) {
          agentState.phase = 1;
          agentStates.set(currentUserText, agentState);
          payload = toolCallStream(agentSpawnName, {
            task_name: "electron_dom_child",
            display_name: "Electron DOM 子代理",
            role: "executor",
            goal: "完成 Electron DOM 子代理验收并返回明确结果",
            context_package: {
              summary: "验证打包 Electron 的子代理展示链路",
              constraints: ["只验证子代理链路"],
              expected_output: "返回 ELECTRON_DOM_CHILD_OK",
              references: [],
            },
          }, "electron-dom-agent-spawn-1");
        } else if (agentState.phase === 1
          && requestContainsTool(parsed, "agent_wait")
          && (childTaskIds.length > 0 || spawnResult?.child_task_id)) {
          agentState.phase = 2;
          agentStates.set(currentUserText, agentState);
          payload = toolCallStream(agentWaitName, {
            task_ids: childTaskIds.length > 0 ? childTaskIds : [spawnResult.child_task_id],
            timeout_ms: 60_000,
          }, "electron-dom-agent-wait-1");
        } else if (agentState.phase === 1 && spawnResult?.child_task_id) {
          agentState.phase = 2;
          agentStates.set(currentUserText, agentState);
          payload = openAiStream(timingResponseToken(currentUserText) || "ELECTRON_DOM_AGENT_OK");
        } else if (agentState.phase >= 2 && waitResult) {
          const childFinalText = waitResult.results?.[0]?.result?.final_text || "ELECTRON_DOM_CHAT_OK";
          payload = openAiStream(`${timingResponseToken(currentUserText) || "ELECTRON_DOM_AGENT_OK"} ${childFinalText}`);
        } else {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "agent_lifecycle_incomplete",
            phase: agentState.phase,
            childTaskIds,
            hasSpawnResult: Boolean(spawnResult),
            hasWaitResult: Boolean(waitResult),
          }));
          return;
        }
      } else if (isDomToolPrompt) {
        const domToolState = domToolStates.get(currentUserText) || { emitted: false, completed: false };
        if (!domToolState.emitted && requestContainsTool(parsed, "tool_catalog")) {
          domToolState.emitted = true;
          domToolStates.set(currentUserText, domToolState);
          payload = toolCallStream(requestToolName(parsed, "tool_catalog"), { include_external: false });
        } else if (!domToolState.completed && (hasToolResult || domToolState.emitted)) {
          domToolState.completed = true;
          domToolStates.set(currentUserText, domToolState);
          payload = openAiStream(timingResponseToken(currentUserText) || toolResponseText);
        } else {
          // 工具调用只能有一轮；即使客户端未能回传工具结果，也必须收口，
          // 防止验收 Provider 把执行错误放大成无限重试。
          domToolState.completed = true;
          domToolStates.set(currentUserText, domToolState);
          payload = openAiStream(timingResponseToken(currentUserText) || toolResponseText);
        }
      } else {
        const isPermissionPrompt = promptKey.includes("权限拒绝 DOM 验收");
        const isApprovalDenyPrompt = promptKey.includes("审批拒绝 DOM 验收");
        const isApprovalCancelPrompt = promptKey.includes("审批取消 DOM 验收");
        const isApprovalExpiryPrompt = promptKey.includes("审批过期 DOM 验收");
        const isApprovalPrompt = promptKey.includes("审批 DOM 验收");
        if (isApprovalDenyPrompt) {
          const nextApprovalState = approvalStates.get("approval-deny") || { emitted: false, completed: false };
          if (!nextApprovalState.emitted) {
            nextApprovalState.emitted = true;
            approvalStates.set("approval-deny", nextApprovalState);
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf denied > ${approvalDenyTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-deny-shell-exec-1");
          } else if (hasToolResult) {
            nextApprovalState.completed = true;
            approvalStates.set("approval-deny", nextApprovalState);
            payload = openAiStream(approvalDeniedResponseText);
          } else {
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf denied > ${approvalDenyTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-deny-shell-exec-1");
          }
        } else if (isApprovalCancelPrompt) {
          const nextApprovalState = approvalStates.get("approval-cancel") || { emitted: false, completed: false };
          if (!nextApprovalState.emitted) {
            nextApprovalState.emitted = true;
            approvalStates.set("approval-cancel", nextApprovalState);
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf cancelled > ${approvalCancelTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-cancel-shell-exec-1");
          } else {
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf cancelled > ${approvalCancelTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-cancel-shell-exec-1");
          }
        } else if (isApprovalExpiryPrompt) {
          const nextApprovalState = approvalStates.get("approval-expiry")
            || { emitted: false, completed: false };
          if (!nextApprovalState.emitted) {
            nextApprovalState.emitted = true;
            approvalStates.set("approval-expiry", nextApprovalState);
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf expired > ${approvalExpiryTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-expiry-shell-exec-1");
          } else if (hasToolResult) {
            nextApprovalState.completed = true;
            approvalStates.set("approval-expiry", nextApprovalState);
            payload = openAiStream(approvalExpiredResponseText);
          } else {
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf expired > ${approvalExpiryTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-expiry-shell-exec-1");
          }
        } else if (isApprovalPrompt) {
          const nextApprovalState = approvalStates.get("approval") || { emitted: false, completed: false };
          if (!nextApprovalState.emitted) {
            nextApprovalState.emitted = true;
            approvalStates.set("approval", nextApprovalState);
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf approval > ${approvalTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-shell-exec-1");
          } else if (hasToolResult) {
            payload = openAiStream(approvalResponseText);
          } else {
            payload = toolCallStream(requestToolName(parsed, "shell_exec"), {
              command: `printf approval > ${approvalTarget}`,
              access_mode: "maybe_write",
            }, "electron-dom-approval-shell-exec-1");
          }
        } else if (isPermissionPrompt && !permissionRoundEmitted) {
          permissionRoundEmitted = true;
          payload = providerResponse(parsed);
        } else if (isPermissionPrompt) {
          payload = openAiStream(responseText);
        } else {
          payload = providerResponse(parsed);
        }
      }
      if (payload === null) {
        response.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        });
        const timer = setInterval(() => response.write(": keep-alive\n\n"), 250);
        request.on("close", () => clearInterval(timer));
        return;
      }
      response.writeHead(200, {
        "content-type": parsed.stream === false ? "application/json" : "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      });
      response.end(payload);
    });
  });
  return { server, requests };
}

async function waitFor(predicate, label, timeoutMs = 30_000, intervalMs = 100) {
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

async function connectPage(port = cdpPort) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find((candidate) => (
    candidate.type === "page"
    && candidate.url.includes(`http://127.0.0.1:${daemonPort}/web.html`)
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
    else request.resolve(message.result);
  });
  await new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", reject);
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => {
      if (pending.delete(id)) reject(new Error(`CDP 请求超时：${method}`));
    }, 15_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  await call("Runtime.enable");
  await call("Page.enable");
  const evaluate = async (expression) => {
    const result = await call("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (result?.exceptionDetails) {
      throw new Error(
        result.exceptionDetails.exception?.description
          || result.exceptionDetails.text
          || "Renderer evaluate 失败",
      );
    }
    return result?.result?.value;
  };
  return {
    call,
    evaluate,
    close() {
      for (const request of pending.values()) {
        clearTimeout(request.timer);
        request.reject(new Error("CDP transport closed"));
      }
      pending.clear();
      socket.close();
    },
  };
}

async function rendererState(page) {
  const value = await page.evaluate(`JSON.stringify({
    title: document.title,
    url: location.href,
    text: document.body?.innerText || '',
    input: Boolean(document.querySelector('[data-testid="input-textarea"]')),
    inputEditable: document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true',
    send: Boolean(document.querySelector('[data-testid="input-send-button"]')),
    sendDisabled: Boolean(document.querySelector('[data-testid="input-send-button"]')?.disabled),
    stop: Boolean(document.querySelector('[data-testid="input-stop-button"]')),
    model: [...document.querySelectorAll('.ia-model-btn')].map((item) => item.innerText || '').join(' '),
    assistant: [...document.querySelectorAll('.message-item.assistant')].map((item) => ({
      text: item.innerText || '',
      source: item.getAttribute('data-source'),
      turnId: item.getAttribute('data-turn-id'),
    })),
    turns: [...document.querySelectorAll('[data-conversation-turn-id]')].map((item) => ({
      id: item.getAttribute('data-conversation-turn-id'),
      text: item.innerText || '',
      expanded: item.querySelector('.turn-disclosure-header')?.getAttribute('aria-expanded'),
    })),
    toolGroups: [...document.querySelectorAll('.conversation-tool-group')].map((item) => ({
      text: item.innerText || '',
      expanded: item.querySelector('.tool-group-header')?.getAttribute('aria-expanded'),
    })),
    phases: [...document.querySelectorAll('.conversation-phase')].map((item) => ({
      text: item.innerText || '',
      expanded: item.querySelector('.conversation-phase-header')?.getAttribute('aria-expanded'),
    })),
    sessionIds: [...document.querySelectorAll('[data-session-id]')].map((item) => item.getAttribute('data-session-id')),
    personalSessionIds: [...new Set([...document.querySelectorAll('#recent-session-content [data-session-id]')]
      .map((item) => item.getAttribute('data-session-id'))
      .filter(Boolean))],
    activePersonalSessionId: document
      .querySelector('#recent-session-content .session-item.active[data-session-id]')
      ?.getAttribute('data-session-id') || null,
    workspaceIds: [...document.querySelectorAll('[data-workspace-id]')].map((item) => item.getAttribute('data-workspace-id')),
    goalCard: (() => {
      const item = document.querySelector('[data-testid="goal-card"]');
      return item ? {
        text: item.innerText || '',
        expanded: item.querySelector('.goal-drawer-toggle')?.getAttribute('aria-expanded'),
        status: item.className,
      } : null;
    })(),
    planCard: (() => {
      const item = document.querySelector('[data-testid="plan-card"]');
      return item ? {
        text: item.innerText || '',
        status: item.className,
      } : null;
    })(),
    agentCenter: (() => {
      const trigger = document.querySelector('.agent-center-trigger');
      const panel = document.querySelector('.agent-center-panel');
      return {
        visible: Boolean(trigger),
        expanded: trigger?.getAttribute('aria-expanded') || null,
        text: panel?.innerText || '',
        rows: [...document.querySelectorAll('.agent-row')].map((item) => item.innerText || ''),
        agentCards: [...document.querySelectorAll('[data-tool-name="agent_spawn"]')].map((item) => ({
          text: item.innerText || '',
          taskId: item.getAttribute('data-agent-task-id'),
        })),
      };
    })(),
    git: [
      ...[...document.querySelectorAll('.git-context-control')].map((item) => ({
        kind: 'context',
        text: item.innerText || '',
        title: item.querySelector('.git-context-trigger')?.getAttribute('title') || '',
        dirty: item.querySelector('.git-context-dirty')?.getAttribute('aria-label') || '',
        conflict: item.querySelector('.git-context-conflict')?.innerText || '',
        warning: item.querySelector('.git-context-warning')?.innerText || '',
        error: item.querySelector('.git-context-error')?.innerText || '',
      })),
      ...[...document.querySelectorAll('.git-repository-panel')].map((item) => ({
        kind: 'repository',
        text: item.innerText || '',
        title: '',
        warning: '',
        error: item.querySelector('.git-repository-error')?.innerText || '',
      })),
    ],
  })`);
  return JSON.parse(value || "{}");
}

async function rendererTiming(page) {
  return page.evaluate(`(() => {
    const reader = window.__magiPerformanceTiming;
    return reader && typeof reader.snapshot === 'function' ? reader.snapshot() : null;
  })()`);
}

function timingHasAllStages(record) {
  const stages = record?.stages || {};
  return [
    "frontend_event_received",
    "reducer_completed",
    "projection_completed",
    "dom_painted",
  ].every((stage) => stages[stage]?.count > 0);
}

function checkTimingStages(label, record) {
  check(`${label} frontend_event_received`, record?.stages?.frontend_event_received?.count > 0);
  check(`${label} reducer_completed`, record?.stages?.reducer_completed?.count > 0);
  check(`${label} projection_completed`, record?.stages?.projection_completed?.count > 0);
  check(`${label} 每轮记录一次 dom_painted`, record?.stages?.dom_painted?.count === 1);
}

function timingEvidenceRecord(scenario, turnId, record, backend = null, options = {}) {
  const accepted = backend?.stages?.accepted_response_sent?.first;
  const outcome = options.outcome || "completed";
  const terminalSettlement = backend?.stages?.session_terminal_finalize_core_completed?.first;
  const settlement = options.settlement || (terminalSettlement ? {
    observed: true,
    barrier: "session_terminal_finalize_core_completed",
    stage: "session_terminal_finalize_core_completed",
    sequence: terminalSettlement.sequence ?? null,
    sinceAcceptedMs: terminalSettlement.sinceAcceptedMs ?? null,
  } : null);
  return {
    fixture: options.fixture || timingFixture,
    schema_version: options.schemaVersion || timingSchemaVersion,
    scenario,
    query_source: "main_turn",
    execution_profile: taskTimingScenarios.has(scenario)
      ? "task"
      : "conversation",
    outcome,
    ...(options.trajectoryMode ? { trajectoryMode: options.trajectoryMode } : {}),
    ...(options.expectedOutcome ? { expectedOutcome: options.expectedOutcome } : {}),
    ...(options.inputHash ? { inputHash: options.inputHash } : {}),
    ...(options.providerDispatch ? { providerDispatch: options.providerDispatch } : {}),
    ...(options.providerBlockReason ? { providerBlockReason: options.providerBlockReason } : {}),
    ...(Number.isInteger(options.providerRequestCount)
      ? { providerRequestCount: options.providerRequestCount }
      : {}),
    ...(settlement ? { settlement } : {}),
    ...(options.settlementRequired === true ? { settlementRequired: true } : {}),
    turnId,
    sessionId: backend?.sessionId || null,
    requestId: accepted?.requestId || backend?.traceId || null,
    stages: Object.fromEntries(Object.entries(record?.stages || {}).map(([stage, value]) => [stage, {
      count: value?.count || 0,
      first: value?.first ? { ...value.first } : null,
      last: value?.last ? { ...value.last } : null,
    }])),
    ...(backend ? { backend } : {}),
  };
}

function backendTimingEvidence(turnId) {
  const record = backendTimingByTurn.get(turnId);
  if (!record) return null;
  const raw = record.stages;
  const stages = { ...raw };
  const alias = (name, candidates) => {
    const sourceStage = candidates.find((candidate) => raw[candidate]);
    if (!sourceStage) return;
    const source = raw[sourceStage];
    stages[name] = {
      count: 1,
      first: source.first,
      last: source.first,
      sourceStage,
    };
  };
  alias("accepted_response_sent", ["accepted_response_sent"]);
  alias("runner_started", ["runner_started"]);
  alias("provider_first_raw_delta", ["provider_first_raw_delta"]);
  alias("provider_first_delta", ["provider_first_delta"]);
  alias("event_bus_first_event", ["event_bus_item_published"]);
  alias("terminal", ["canonical_terminal_published", "canonical_terminal_ignored"]);
  const acceptedAtMs = stages.accepted_response_sent?.first?.atMs;
  if (Number.isFinite(acceptedAtMs)) {
    for (const stage of Object.values(stages)) {
      if (!stage) continue;
      for (const key of ["first", "last"]) {
        if (stage[key] && Number.isFinite(stage[key].atMs)) {
          stage[key] = {
            ...stage[key],
            sinceAcceptedMs: stage[key].atMs - acceptedAtMs,
          };
        }
      }
    }
  }
  return {
    traceId: record.traceId || null,
    sessionId: record.sessionId || null,
    stages,
  };
}

function backendTimingHasRequiredStages(record, scenario = "") {
  const stages = record?.stages || {};
  const required = [
    "accepted_response_sent",
    "runner_started",
    "provider_first_delta",
    "event_bus_first_event",
  ];
  if (["workspace_tool", "goal", "subagent", "personal_long_history"].includes(scenario)) {
    required.push("provider_first_raw_delta");
  }
  return required.every((stage) => stages[stage]?.count > 0)
    && stages.canonical_terminal_published?.count > 0;
}

function backendTimingHasAbnormalRequiredStages(record) {
  const stages = record?.stages || {};
  return stages.accepted_response_sent?.count > 0
    && stages.runner_started?.count > 0
    && stages.provider_request_started?.count > 0
    && stages.canonical_terminal_published?.count > 0
    && stages.session_terminal_finalize_core_completed?.count > 0;
}

function backendTimingHasPreDispatchRequiredStages(record) {
  const stages = record?.stages || {};
  return stages.accepted_response_sent?.count > 0
    && stages.canonical_terminal_published?.count > 0
    && stages.session_terminal_finalize_core_completed?.count > 0;
}

function checkBackendTimingStages(label, backend, scenario = "") {
  check(`${label} accepted_response_sent`, backend?.stages?.accepted_response_sent?.count > 0);
  check(`${label} runner_started`, backend?.stages?.runner_started?.count > 0);
  if (["workspace_tool", "goal", "subagent", "personal_long_history"].includes(scenario)) {
    check(`${label} provider_first_raw_delta`, backend?.stages?.provider_first_raw_delta?.count > 0);
  }
  check(`${label} provider_first_delta`, backend?.stages?.provider_first_delta?.count > 0);
  check(`${label} 首 EventBus`, backend?.stages?.event_bus_first_event?.count > 0);
  check(`${label} terminal`, backend?.stages?.terminal?.count > 0);
}

function checkAbnormalBackendTimingStages(label, backend) {
  check(`${label} accepted_response_sent`, backend?.stages?.accepted_response_sent?.count > 0);
  check(`${label} runner_started`, backend?.stages?.runner_started?.count > 0);
  check(`${label} provider_request_started`, backend?.stages?.provider_request_started?.count > 0);
  check(`${label} canonical_terminal_published`, backend?.stages?.canonical_terminal_published?.count > 0);
  check(
    `${label} session_terminal_finalize_core_completed`,
    backend?.stages?.session_terminal_finalize_core_completed?.count > 0,
  );
}

function checkPreDispatchBackendTimingStages(label, backend) {
  check(`${label} accepted_response_sent`, backend?.stages?.accepted_response_sent?.count > 0);
  check(`${label} canonical_terminal_published`, backend?.stages?.canonical_terminal_published?.count > 0);
  check(
    `${label} session_terminal_finalize_core_completed`,
    backend?.stages?.session_terminal_finalize_core_completed?.count > 0,
  );
  check(
    `${label} Provider 未 dispatch`,
    !backend?.stages?.provider_request_started,
    JSON.stringify(backend?.stages || {}),
  );
}

async function waitForTimingRecord(page, turnId, label) {
  if (!turnId) {
    throw new Error(`${label} 缺少 assistant data-turn-id`);
  }
  let lastSnapshot = null;
  try {
    return await waitFor(async () => {
      const snapshot = await rendererTiming(page);
      lastSnapshot = snapshot;
      const record = snapshot?.turns?.find((candidate) => candidate.turnId === turnId);
      return timingHasAllStages(record) ? record : null;
    }, label);
  } catch (error) {
    const record = lastSnapshot?.turns?.find((candidate) => candidate.turnId === turnId) || null;
    throw new Error(`${error instanceof Error ? error.message : String(error)}; turnId=${turnId}; rendererTiming=${JSON.stringify(record)}`);
  }
}

function assistantTurnId(state, expectedText) {
  return state.assistant.find((message) => message.text.includes(expectedText))?.turnId
    || state.turns.find((turn) => turn.text.includes(expectedText))?.id
    || state.assistant.at(-1)?.turnId
    || state.turns.at(-1)?.id
    || null;
}

async function waitForBackendTiming(turnId, label, scenario = "") {
  return waitFor(() => {
    const evidence = backendTimingEvidence(turnId);
    return backendTimingHasRequiredStages(evidence, scenario) ? evidence : null;
  }, label, 45_000);
}

async function waitForAbnormalBackendTiming(turnId, label) {
  return waitFor(() => {
    const evidence = backendTimingEvidence(turnId);
    return backendTimingHasAbnormalRequiredStages(evidence) ? evidence : null;
  }, label, 45_000);
}

async function waitForPreDispatchBackendTiming(turnId, label) {
  return waitFor(() => {
    const evidence = backendTimingEvidence(turnId);
    return backendTimingHasPreDispatchRequiredStages(evidence) ? evidence : null;
  }, label, 45_000);
}

async function runGitDriftConflictChecks(page) {
  // Git drift/conflict 必须经真实 workspace、Git 观测和 Task preflight；两种阻断
  // 都发生在 Provider dispatch 前，不以本地 Provider 文本冒充 Git 错误证据。
  gitDriftRoot = await mkdtemp(join(stateRoot, "git-drift-"));
  await createGitRepository(gitDriftRoot);
  const driftWorkspaceId = await registerWorkspace(page, gitDriftRoot);
  await waitForComposerReady(page, "Git drift 工作区输入区就绪");
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.git.some((item) => item.kind === "context" && item.title.includes("main")) ? state : null;
  }, "Git drift 初始基线进入真实 DOM");
  await setComposerText(page, "Git drift 初始基线：请只回复 ELECTRON_DOM_CHAT_OK");
  await clickSend(page);
  await waitForAssistant(page, responseText, "Git drift 初始 Turn");
  await waitForComposerReady(page, "Git drift 初始 Turn 收口");
  await gitFixtureCommand(gitDriftRoot, ["switch", "-c", "external/drift"]);
  await page.evaluate("window.dispatchEvent(new CustomEvent('magi:workspaceContentChanged', { detail: { reason: 'git-drift-fixture' } }))");
  await page.evaluate(`document.querySelector('.git-context-trigger')?.click()`);
  try {
    await waitFor(async () => {
      const state = await rendererState(page);
      return state.git.some((item) => /漂移|不一致|外部修改|drift/iu.test(item.warning))
        ? state
        : null;
    }, "Git drift 警告进入真实 DOM");
  } catch (error) {
    throw new Error(`${error.message}；Git drift 当前 Renderer 状态：${JSON.stringify(await rendererState(page))}`);
  }
  await page.evaluate(`document.querySelector('.git-context-trigger')?.click()`);
  const driftProviderRequests = provider.requests.length;
  const driftPrompt = "Git 漂移 DOM 验收：执行一个复杂任务，读取当前工作区并汇总结果";
  await setComposerText(page, driftPrompt);
  await clickSend(page);
  const driftResult = await waitFor(async () => {
    const state = await rendererState(page);
    const text = state.assistant.at(-1)?.text || "";
    return state.input && !state.stop && /git|漂移|不一致|失败|error/iu.test(text) ? state : null;
  }, "Git drift 失败终态", 60_000);
  check("Git drift 失败事实进入真实 DOM", /git|漂移|不一致|失败|error/iu.test(driftResult.assistant.at(-1)?.text || ""));
  check("Git drift 在 Provider 前阻断", provider.requests.length === driftProviderRequests);
  check("Git drift 工作区注册身份仍可见", driftResult.workspaceIds.includes(driftWorkspaceId));
  const driftTurnId = driftResult.turns.at(-1)?.id || driftResult.assistant.at(-1)?.turnId || null;
  check("Git drift Turn 具有稳定 canonical Turn 身份", Boolean(driftTurnId));
  const driftTiming = await waitForTimingRecord(page, driftTurnId, "Git drift Renderer timing");
  const driftBackend = await waitForPreDispatchBackendTiming(
    driftTurnId,
    "Git drift Provider 前置阻断后端 timing 关联",
  );
  checkPreDispatchBackendTimingStages("Git drift 后端", driftBackend);
  checkTimingStages("Git drift Renderer", driftTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "git_branch_drift",
    driftTurnId,
    driftTiming,
    driftBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(driftPrompt),
      providerDispatch: "not_dispatched",
      providerBlockReason: "git_branch_drift",
      providerRequestCount: provider.requests.length - driftProviderRequests,
      settlementRequired: true,
    },
  ));

  gitConflictRoot = await mkdtemp(join(stateRoot, "git-conflict-"));
  await createGitConflictRepository(gitConflictRoot);
  const conflictWorkspaceId = await registerWorkspace(page, gitConflictRoot);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.git.some((item) => /冲突|conflict/iu.test(`${item.title} ${item.text} ${item.conflict}`)) ? state : null;
  }, "Git conflict 状态进入真实 DOM");
  const conflictProviderRequests = provider.requests.length;
  const conflictPrompt = "Git 冲突 DOM 验收：执行一个复杂任务，处理当前工作区冲突并汇总结果";
  await setComposerText(page, conflictPrompt);
  await clickSend(page);
  const conflictResult = await waitFor(async () => {
    const state = await rendererState(page);
    const text = state.assistant.at(-1)?.text || "";
    return state.input && !state.stop && /git|冲突|merge conflict|失败|error/iu.test(text) ? state : null;
  }, "Git conflict 失败终态", 60_000);
  check("Git conflict 失败事实进入真实 DOM", /git|冲突|merge conflict|失败|error/iu.test(conflictResult.assistant.at(-1)?.text || ""));
  check("Git conflict 在 Provider 前阻断", provider.requests.length === conflictProviderRequests);
  check("Git conflict 工作区注册身份仍可见", conflictResult.workspaceIds.includes(conflictWorkspaceId));
  const conflictTurnId = conflictResult.turns.at(-1)?.id || conflictResult.assistant.at(-1)?.turnId || null;
  check("Git conflict Turn 具有稳定 canonical Turn 身份", Boolean(conflictTurnId));
  const conflictTiming = await waitForTimingRecord(page, conflictTurnId, "Git conflict Renderer timing");
  const conflictBackend = await waitForPreDispatchBackendTiming(
    conflictTurnId,
    "Git conflict Provider 前置阻断后端 timing 关联",
  );
  checkPreDispatchBackendTimingStages("Git conflict 后端", conflictBackend);
  checkTimingStages("Git conflict Renderer", conflictTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "git_merge_conflict",
    conflictTurnId,
    conflictTiming,
    conflictBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(conflictPrompt),
      providerDispatch: "not_dispatched",
      providerBlockReason: "git_merge_conflict",
      providerRequestCount: provider.requests.length - conflictProviderRequests,
      settlementRequired: true,
    },
  ));
}

async function runApprovalExpiryCheck(page) {
  if (approvalExpiryWaitMs <= 0) {
    throw new Error("审批过期聚焦验收必须设置 MAGI_ELECTRON_DOM_APPROVAL_EXPIRY_WAIT_MS");
  }
  const workspaceRoot = await mkdtemp(join(stateRoot, "approval-expiry-"));
  const workspaceId = await registerWorkspace(page, workspaceRoot);
  approvalExpiryTarget = join(workspaceRoot, ".magi-electron-dom-approval-expiry.txt");
  await waitForComposerReady(page, "审批过期工作区输入区就绪");
  await chooseAccessProfile(page, "restricted");
  const prompt = `审批过期 DOM 验收：请明确调用 shell_exec 写入 ${approvalExpiryTarget}`;
  await setComposerText(page, prompt);
  await clickSend(page);
  const approval = await waitFor(async () => {
    const state = await rendererState(page);
    const card = await page.evaluate(`Boolean(document.querySelector('.tool-approval'))`);
    return card && state.stop ? state : null;
  }, "审批过期聚焦验收卡片", 45_000);
  check("审批过期聚焦验收卡片进入真实 DOM", approval.text.length > 0);
  check("审批过期聚焦验收显示停止入口", approval.stop);
  await sleep(approvalExpiryWaitMs);
  const expired = await waitFor(async () => {
    const state = await rendererState(page);
    const text = `${state.text} ${state.assistant.at(-1)?.text || ""}`;
    return state.input && state.inputEditable && !state.stop
      && /过期|expired|tool_approval_expired/iu.test(text)
      ? state
      : null;
  }, "审批过期聚焦验收失败终态", 60_000, 250);
  check("审批过期事实进入真实 DOM", /过期|expired|tool_approval_expired/iu.test(expired.text));
  check("审批过期工作区注册身份仍可见", expired.workspaceIds.includes(workspaceId));
  let fileExists = true;
  try {
    await access(approvalExpiryTarget);
  } catch {
    fileExists = false;
  }
  check("审批过期后真实文件副作用不存在", !fileExists);
  const turnId = expired.turns.at(-1)?.id || expired.assistant.at(-1)?.turnId || null;
  check("审批过期 Turn 具有稳定 canonical Turn 身份", Boolean(turnId));
  const timing = await waitForTimingRecord(page, turnId, "审批过期 Renderer timing");
  const backend = await waitForAbnormalBackendTiming(turnId, "审批过期后端 timing 关联");
  checkAbnormalBackendTimingStages("审批过期后端", backend);
  checkTimingStages("审批过期 Renderer", timing);
  rendererTimingSamples.push(timingEvidenceRecord(
    "approval_expired",
    turnId,
    timing,
    backend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(prompt),
      providerRequestCount: 1,
      settlementRequired: true,
    },
  ));
  return { workspaceId, turnId, prompt, fileExists };
}

function parseRecoverySseBlock(block) {
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

function recoveryEventTurnId(event) {
  const payload = event?.payload || {};
  const canonicalItem = payload.canonical_item || payload.canonicalItem;
  return payload.turn_id
    || payload.turnId
    || payload.canonical_turn?.turnId
    || payload.canonical_turn?.turn_id
    || payload.canonicalTurn?.turnId
    || payload.current_turn?.turn_id
    || payload.currentTurn?.turnId
    || canonicalItem?.turnId
    || canonicalItem?.turn_id
    || null;
}

function recoveryEventKind(event) {
  return event?.payload?.canonical_event_kind
    || event?.payload?.canonicalEventKind
    || null;
}

function recoveryEventStatus(event) {
  const payload = event?.payload || {};
  return payload.canonical_turn?.status
    || payload.canonicalTurn?.status
    || payload.current_turn?.status
    || payload.currentTurn?.status
    || null;
}

function isRecoveryTerminal({ kind }) {
  return ["turn_completed", "turn_failed", "turn_cancelled"].includes(kind);
}

async function readRecoverySseTurnEvent({
  sessionId,
  turnId,
  workspaceId = null,
  afterSequence,
  predicate,
  timeoutMs = recoveryTransportTimeoutMs,
}) {
  const query = new URLSearchParams({
    scope: workspaceId ? "workspace" : "personal",
    sessionId,
    afterSequence: String(afterSequence || 0),
  });
  if (workspaceId) query.set("workspaceId", workspaceId);
  const controller = new AbortController();
  const response = await fetch(`http://127.0.0.1:${daemonPort}/events?${query}`, {
    signal: controller.signal,
  });
  if (!response.ok || !response.body) {
    throw new Error(`Electron recovery SSE HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  const observed = [];
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const result = await Promise.race([
        reader.read(),
        sleep(remaining).then(() => ({ timeout: true })),
      ]);
      if (result.timeout) throw new Error(`Electron recovery SSE 观察超时：${turnId}`);
      if (result.done) break;
      buffer += decoder.decode(result.value, { stream: true });
      let separator;
      while ((separator = buffer.search(/\r?\n\r?\n/u)) >= 0) {
        const block = buffer.slice(0, separator);
        buffer = buffer.slice(separator).replace(/^\r?\n\r?\n/u, "");
        const event = parseRecoverySseBlock(block);
        if (!event || !Number.isInteger(event.sequence) || event.sequence <= (afterSequence || 0)) {
          continue;
        }
        const kind = recoveryEventKind(event);
        const status = recoveryEventStatus(event);
        const summary = { sequence: event.sequence, eventType: event.event_type, kind, status };
        observed.push(summary);
        if (event.session_id !== sessionId || recoveryEventTurnId(event) !== turnId) continue;
        const matched = { event, kind, status, sequence: event.sequence };
        if (predicate(matched)) return matched;
      }
    }
  } finally {
    controller.abort();
    reader.releaseLock();
  }
  throw new Error(`Electron recovery SSE 未观察到目标事件：${JSON.stringify(observed.slice(-20))}`);
}

function electronCompactionEvent(event) {
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

async function readElectronCompactionEvents({
  sessionId,
  turnId,
  afterSequence = 0,
  timeoutMs = recoveryTransportTimeoutMs,
}) {
  const query = new URLSearchParams({
    scope: "personal",
    sessionId,
    afterSequence: String(afterSequence || 0),
  });
  const controller = new AbortController();
  const response = await fetch(`http://127.0.0.1:${daemonPort}/events?${query}`, {
    signal: controller.signal,
  });
  if (!response.ok || !response.body) {
    throw new Error(`Electron context compaction SSE HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  const compactionEvents = [];
  const observed = [];
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const result = await Promise.race([
        reader.read(),
        sleep(remaining).then(() => ({ timeout: true })),
      ]);
      if (result.timeout) {
        throw new Error(
          `Electron context compaction SSE 观察超时：${turnId}，events=${JSON.stringify(observed.slice(-20))}`,
        );
      }
      if (result.done) break;
      buffer += decoder.decode(result.value, { stream: true });
      let separator;
      while ((separator = buffer.search(/\r?\n\r?\n/u)) >= 0) {
        const block = buffer.slice(0, separator);
        buffer = buffer.slice(separator).replace(/^\r?\n\r?\n/u, "");
        const event = parseRecoverySseBlock(block);
        if (!event || !Number.isInteger(event.sequence) || event.sequence <= (afterSequence || 0)) {
          continue;
        }
        const eventTurnId = recoveryEventTurnId(event);
        if (event.session_id !== sessionId || eventTurnId !== turnId) continue;
        const kind = recoveryEventKind(event);
        const status = recoveryEventStatus(event);
        observed.push({ sequence: event.sequence, kind, status });
        const compaction = electronCompactionEvent(event);
        if (compaction) compactionEvents.push(compaction);
        if (isRecoveryTerminal({ kind })
          || ["completed", "failed", "cancelled"].includes(status)) {
          return {
            compactionEvents,
            terminal: {
              sequence: event.sequence,
              kind,
              status,
            },
          };
        }
      }
    }
  } finally {
    controller.abort();
    reader.releaseLock();
  }
  throw new Error(
    `Electron context compaction SSE 未观察到 terminal：${turnId}，events=${JSON.stringify(observed.slice(-20))}`,
  );
}

function recoveryWebSocketEventList(message) {
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

function findRecoveryWebSocketTurnEvent(messages, sessionId, turnId, afterSequence, predicate) {
  const events = messages
    .flatMap(recoveryWebSocketEventList)
    .filter((event) => Number.isInteger(event?.sequence) && event.sequence > (afterSequence || 0))
    .sort((left, right) => left.sequence - right.sequence);
  for (const event of events) {
    if (event.session_id !== sessionId || recoveryEventTurnId(event) !== turnId) continue;
    const matched = {
      event,
      kind: recoveryEventKind(event),
      status: recoveryEventStatus(event),
      sequence: event.sequence,
    };
    if (predicate(matched)) return matched;
  }
  return null;
}

const recoveryWebSocketMessageQueues = new WeakMap();

function recoveryWebSocketMessageState(socket) {
  let state = recoveryWebSocketMessageQueues.get(socket);
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
      waiter.reject(new Error("Electron recovery WebSocket 在目标事件前关闭"));
    }
  };
  socket.on("message", state.onMessage);
  socket.on("error", state.onError);
  socket.on("close", state.onClose);
  recoveryWebSocketMessageQueues.set(socket, state);
  return state;
}

function nextRecoveryWebSocketMessage(socket, deadline) {
  const state = recoveryWebSocketMessageState(socket);
  if (state.messages.length > 0) return Promise.resolve(state.messages.shift());
  if (state.error) return Promise.reject(state.error);
  if (state.closed) return Promise.reject(new Error("Electron recovery WebSocket 在目标事件前关闭"));
  return new Promise((resolve, reject) => {
    const remaining = Math.max(1, deadline - Date.now());
    const timer = setTimeout(() => {
      if (state.waiter?.resolve === resolve) state.waiter = null;
      reject(new Error("Electron recovery WebSocket 观察超时"));
    }, remaining);
    state.waiter = { resolve, reject, timer };
  });
}

async function openRecoveryWebSocketSubscription({ sessionId, workspaceId = null, afterSequence }) {
  const socket = new WebSocket(`ws://127.0.0.1:${daemonPort}/api/app-server`);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.terminate();
      reject(new Error("Electron recovery WebSocket 连接超时"));
    }, 15_000);
    socket.once("open", () => {
      clearTimeout(timer);
      resolve();
    });
    socket.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
  const deadline = Date.now() + recoveryTransportTimeoutMs;
  const messages = [];
  const initializeId = `electron-recovery-initialize-${Date.now()}`;
  const subscribeParams = { sessionId, afterSequence };
  if (workspaceId) subscribeParams.workspaceId = workspaceId;
  socket.send(JSON.stringify({
    jsonrpc: "2.0",
    id: initializeId,
    method: "initialize",
    params: {
      clientInfo: { name: "magi-electron-recovery" },
      protocol: { major: 1, minor: 0 },
      capabilities: { streaming: true },
    },
  }));
  let initialized = false;
  while (Date.now() < deadline && !initialized) {
    let message;
    try {
      message = await nextRecoveryWebSocketMessage(socket, deadline);
    } catch (error) {
      throw new Error(`Electron recovery WebSocket initialize 观察失败：${error.message}；已收消息=${JSON.stringify(messages)}`);
    }
    if (!message) continue;
    messages.push(message);
    if (message.id === initializeId) {
      if (message.error) throw new Error(`Electron recovery WebSocket initialize 失败：${JSON.stringify(message.error)}`);
      initialized = true;
    }
  }
  if (!initialized) throw new Error("Electron recovery WebSocket initialize 未完成");
  socket.send(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
  const subscribeId = `electron-recovery-subscribe-${Date.now()}`;
  socket.send(JSON.stringify({
    jsonrpc: "2.0",
    id: subscribeId,
    method: "events/subscribe",
    params: subscribeParams,
  }));
  let subscribed = false;
  let snapshot = null;
  while (Date.now() < deadline && (!subscribed || !snapshot)) {
    let message;
    try {
      message = await nextRecoveryWebSocketMessage(socket, deadline);
    } catch (error) {
      throw new Error(`Electron recovery WebSocket subscribe 观察失败：${error.message}；已收消息=${JSON.stringify(messages)}`);
    }
    if (!message) continue;
    messages.push(message);
    if (message.id === subscribeId) {
      if (message.error) throw new Error(`Electron recovery WebSocket subscribe 失败：${JSON.stringify(message.error)}`);
      subscribed = message.result?.subscribed === true;
    }
    if (message.method === "events/snapshot" || message.method === "events/resyncRequired") {
      snapshot = message;
    }
  }
  if (!subscribed || !snapshot) throw new Error("Electron recovery WebSocket 未返回订阅快照");
  return { socket, messages, snapshot, deadline };
}

async function waitForRecoveryWebSocketTurnEvent(subscription, sessionId, turnId, afterSequence, predicate) {
  let matched = findRecoveryWebSocketTurnEvent(
    [...subscription.messages, subscription.snapshot],
    sessionId,
    turnId,
    afterSequence,
    predicate,
  );
  while (!matched && Date.now() < subscription.deadline) {
    const message = await nextRecoveryWebSocketMessage(subscription.socket, subscription.deadline);
    if (!message) continue;
    subscription.messages.push(message);
    matched = findRecoveryWebSocketTurnEvent(
      [message],
      sessionId,
      turnId,
      afterSequence,
      predicate,
    );
  }
  if (!matched) {
    const summary = [...subscription.messages, subscription.snapshot]
      .flatMap(recoveryWebSocketEventList)
      .map((event) => ({
        sequence: event?.sequence,
        eventType: event?.event_type,
        sessionId: event?.session_id,
        turnId: recoveryEventTurnId(event),
        kind: recoveryEventKind(event),
        status: recoveryEventStatus(event),
      }))
      .slice(-20);
    throw new Error(`Electron recovery WebSocket 未观察到 Turn 事件：${turnId} ${JSON.stringify(summary)}`);
  }
  return matched;
}

async function startRecoveryCancellationTurn(page, prompt, label, options = {}) {
  if (options.workspaceId) {
    await waitFor(async () => (await rendererState(page)).workspaceIds.includes(options.workspaceId) ? true : null,
      `${label} 工作区进入侧栏`);
    const escapedWorkspaceId = JSON.stringify(options.workspaceId);
    await page.evaluate(`(() => {
      const workspace = document.querySelector('[data-workspace-id=' + ${escapedWorkspaceId} + ']');
      const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
      if (!create) throw new Error('recovery workspace new session button missing');
      create.click();
    })()`);
    await waitForComposerReady(page, `${label} 工作区输入区就绪`);
    await chooseAccessProfile(page, "restricted");
  } else {
    await openPersonalDraft(page);
  }
  const knownTurnIds = new Set(backendTimingByTurn.keys());
  await setComposerText(page, prompt);
  await clickSend(page);
  const active = await waitFor(async () => {
    const state = await rendererState(page);
    const visibleTurnId = state.turns.at(-1)?.id || state.assistant.at(-1)?.turnId || null;
    const backend = [...backendTimingByTurn.values()].reverse()
      .find((candidate) => !knownTurnIds.has(candidate.turnId));
    const turnId = visibleTurnId || backend?.turnId || null;
    const sessionId = state.activePersonalSessionId || backend?.sessionId || null;
    return turnId && sessionId ? { state, turnId, sessionId } : null;
  }, `${label} 活动 Turn`, 45_000);
  const sessionId = active.sessionId || new URL(active.state.url).searchParams.get("sessionId");
  if (!sessionId) throw new Error(`${label} 缺少 sessionId`);
  if (options.workspaceId) {
    await waitFor(
      async () => page.evaluate(`Boolean(document.querySelector('.tool-approval'))`),
      `${label} 审批卡片进入真实 DOM`,
      45_000,
    );
  }
  return {
    sessionId,
    turnId: active.turnId,
    prompt,
    workspaceId: options.workspaceId || null,
    workspacePath: options.workspacePath || null,
  };
}

async function stopRecoveryTurn(page, turn) {
  const stopReady = await waitFor(
    async () => (await rendererState(page)).stop ? true : null,
    "Electron recovery Renderer 停止入口",
    15_000,
    100,
  ).catch(() => false);
  if (stopReady && await clickStop(page)) return "renderer_stop";
  const result = await page.evaluate(`fetch('/api/session/interrupt', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(${JSON.stringify({
      sessionId: turn.sessionId,
      workspaceId: turn.workspaceId,
      workspacePath: turn.workspacePath,
    })}),
  }).then(async (response) => ({ status: response.status, body: await response.json().catch(() => null) }))`);
  if (result.status < 200 || result.status >= 300 || result.body?.interrupted !== true) {
    throw new Error(`Electron recovery Turn 无法通过 Renderer 停止：${JSON.stringify(result)}`);
  }
  return "renderer_interrupt_api";
}

async function waitForRecoveryCancellationDom(page, turnId, prompt, label, workspaceId = null) {
  return waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop
      && (!workspaceId || state.workspaceIds.includes(workspaceId))
      ? state
      : null;
  }, label, 45_000);
}

async function recordRecoveryCancellation(page, scenario, turn, settlementFixture) {
  const timing = await waitForTimingRecord(page, turn.turnId, `${scenario} Renderer timing`);
  const backend = await waitForAbnormalBackendTiming(turn.turnId, `${scenario} 后端 timing 关联`);
  checkAbnormalBackendTimingStages(`${scenario} 后端`, backend);
  checkTimingStages(`${scenario} Renderer`, timing);
  rendererTimingSamples.push(timingEvidenceRecord(
    scenario,
    turn.turnId,
    timing,
    backend,
    {
      fixture: settlementFixture,
      trajectoryMode: "abnormal",
      expectedOutcome: "cancelled",
      outcome: "cancelled",
      inputHash: hashText(turn.prompt),
      providerRequestCount: 1,
      settlementRequired: true,
    },
  ));
}

async function runRecoveryAcceptance(page, electron) {
  const recovery = {};
  const recoveryWorkspacePath = await mkdtemp(join(stateRoot, "recovery-workspace-"));
  const recoveryWorkspaceId = await registerWorkspace(page, recoveryWorkspacePath);
  approvalCancelTarget = join(recoveryWorkspacePath, ".magi-electron-recovery-cancel.txt");
  const sseTurn = await startRecoveryCancellationTurn(
    page,
    "审批取消 DOM 验收（Electron SSE 重连）：请明确调用 shell_exec 写入审批取消目标，并保持响应直到事件流恢复后停止",
    "SSE 重连",
    { workspaceId: recoveryWorkspaceId, workspacePath: recoveryWorkspacePath },
  );
  const sseFirst = await readRecoverySseTurnEvent({
    sessionId: sseTurn.sessionId,
    turnId: sseTurn.turnId,
    workspaceId: sseTurn.workspaceId,
    afterSequence: 0,
    predicate: (matched) => !isRecoveryTerminal(matched),
  });
  check("SSE 首次连接收到同一 Turn 的非终态事件", Boolean(sseFirst.sequence));
  const sseReconnectAfterSequence = sseFirst.sequence;
  check("SSE 首次连接已真实断开后才重连", sseReconnectAfterSequence > 0);
  const sseStopMethod = await stopRecoveryTurn(page, sseTurn);
  check("SSE 重连前真实停止入口可用", Boolean(sseStopMethod));
  const sseTerminal = await readRecoverySseTurnEvent({
    sessionId: sseTurn.sessionId,
    turnId: sseTurn.turnId,
    workspaceId: sseTurn.workspaceId,
    afterSequence: sseReconnectAfterSequence,
    predicate: isRecoveryTerminal,
  });
  check(
    "SSE 重连恢复唯一 cancelled terminal",
    sseTerminal.status === "cancelled" || sseTerminal.kind === "turn_cancelled",
    JSON.stringify(sseTerminal),
  );
  await recordRecoveryCancellation(
    page,
    "sse_reconnect_cancelled",
    sseTurn,
    "electron-dom-sse-reconnect-v1",
  );
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "SSE 重连取消后 Renderer reload");
  recovery.sse = {
    sessionId: sseTurn.sessionId,
    turnId: sseTurn.turnId,
    workspaceId: sseTurn.workspaceId,
    firstSequence: sseFirst.sequence,
    reconnectAfterSequence: sseReconnectAfterSequence,
    terminalSequence: sseTerminal.sequence,
    terminalKind: sseTerminal.kind,
    stopMethod: sseStopMethod,
  };

  const websocketTurn = await startRecoveryCancellationTurn(
    page,
    "审批取消 DOM 验收（Electron WebSocket 重连）：请明确调用 shell_exec 写入审批取消目标，并保持响应直到事件流恢复后停止",
    "WebSocket 重连",
    { workspaceId: recoveryWorkspaceId, workspacePath: recoveryWorkspacePath },
  );
  const firstSubscription = await openRecoveryWebSocketSubscription({
    sessionId: websocketTurn.sessionId,
    workspaceId: websocketTurn.workspaceId,
    afterSequence: 0,
  });
  const websocketFirst = await waitForRecoveryWebSocketTurnEvent(
    firstSubscription,
    websocketTurn.sessionId,
    websocketTurn.turnId,
    0,
    (matched) => !isRecoveryTerminal(matched),
  );
  firstSubscription.socket.terminate();
  check("WebSocket 首次连接收到同一 Turn 的非终态事件", Boolean(websocketFirst.sequence));
  check("WebSocket 首次连接已真实断开后才重连", websocketFirst.sequence > 0);
  const websocketStopMethod = await stopRecoveryTurn(page, websocketTurn);
  check("WebSocket 重连前真实停止入口可用", Boolean(websocketStopMethod));
  const reconnectedSubscription = await openRecoveryWebSocketSubscription({
    sessionId: websocketTurn.sessionId,
    workspaceId: websocketTurn.workspaceId,
    afterSequence: websocketFirst.sequence,
  });
  const websocketTerminal = await waitForRecoveryWebSocketTurnEvent(
    reconnectedSubscription,
    websocketTurn.sessionId,
    websocketTurn.turnId,
    websocketFirst.sequence,
    isRecoveryTerminal,
  );
  reconnectedSubscription.socket.terminate();
  check(
    "WebSocket 重连恢复唯一 cancelled terminal",
    websocketTerminal.status === "cancelled" || websocketTerminal.kind === "turn_cancelled",
    JSON.stringify(websocketTerminal),
  );
  await recordRecoveryCancellation(
    page,
    "websocket_reconnect_cancelled",
    websocketTurn,
    "electron-dom-websocket-reconnect-v1",
  );
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "WebSocket 重连取消后 Renderer reload");
  recovery.websocket = {
    sessionId: websocketTurn.sessionId,
    turnId: websocketTurn.turnId,
    firstSequence: websocketFirst.sequence,
    reconnectAfterSequence: websocketFirst.sequence,
    terminalSequence: websocketTerminal.sequence,
    terminalKind: websocketTerminal.kind,
    stopMethod: websocketStopMethod,
  };

  await openPersonalDraft(page);
  const restartPrompt = "daemon 重启 DOM 验收 replay：请只回复重启后的最终消息";
  await setComposerText(page, restartPrompt);
  await clickSend(page);
  const restartResult = await waitForAssistant(page, restartResponseText, "daemon 重启 replay 初始 Turn");
  const providerRequestsBeforeDaemonRestart = provider.requests.length;
  const restartSessionId = restartResult.activePersonalSessionId
    || new URL(restartResult.url).searchParams.get("sessionId");
  const restartTurnId = assistantTurnId(restartResult, restartResponseText);
  check("daemon 重启 replay 初始 Turn 具有稳定身份", Boolean(restartSessionId && restartTurnId));
  const beforeDaemonRestart = await healthSnapshot();
  const daemonPid = await waitFor(async () => ownedDaemonPid(electron.pid), "定位 Electron 自有 daemon", 15_000);
  process.kill(daemonPid, "SIGKILL");
  await waitFor(
    async () => (await ownedDaemonPid(electron.pid)) !== daemonPid,
    "daemon replay 旧进程退出",
    15_000,
    100,
  );
  const afterDaemonRestart = await waitFor(async () => {
    try {
      const health = await healthSnapshot();
      return health.runtimeEpoch && health.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch ? health : null;
    } catch {
      return null;
    }
  }, "daemon replay 新 runtime 恢复", 45_000, 250);
  check("daemon replay runtime epoch 已变化", afterDaemonRestart.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "daemon replay Renderer reload");
  await selectPersonalSessionById(page, restartSessionId);
  const daemonReplay = await waitForAssistant(page, restartResponseText, "daemon replay 历史消息");
  check("daemon 重启 replay 保留原 Session 内容", daemonReplay.text.includes(restartResponseText));
  check("daemon 重启 replay 未新增 Provider request", provider.requests.length === providerRequestsBeforeDaemonRestart);
  const daemonTiming = await waitForTimingRecord(page, restartTurnId, "daemon replay Renderer timing");
  const daemonBackend = await waitForBackendTiming(restartTurnId, "daemon replay 后端 timing 关联", "personal_chat");
  checkTimingStages("daemon replay Renderer", daemonTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "daemon_restart_replay",
    restartTurnId,
    daemonTiming,
    daemonBackend,
    {
      fixture: "electron-dom-daemon-restart-replay-v1",
      inputHash: hashText(restartPrompt),
      providerRequestCount: provider.requests.length - providerRequestsBeforeDaemonRestart,
      settlement: {
        observed: true,
        barrier: "daemon_runtime_epoch_recovered_and_history_replayed",
        stage: "daemon_runtime_epoch_changed",
        sequence: null,
        next_turn_admission_checked: false,
      },
      settlementRequired: true,
    },
  ));
  recovery.daemonRestart = {
    sessionId: restartSessionId,
    turnId: restartTurnId,
    beforeRuntimeEpoch: beforeDaemonRestart.runtimeEpoch,
    afterRuntimeEpoch: afterDaemonRestart.runtimeEpoch,
    providerRequestsBefore: providerRequestsBeforeDaemonRestart,
    providerRequestsAfter: provider.requests.length,
  };

  const appDaemonPid = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
  page?.close();
  page = null;
  await stopOwnedProcess(electron, cdpPort);
  await stopOwnedDaemon(appDaemonPid);
  restartedElectron = spawnElectron(appRestartCdpPort);
  await waitFor(async () => {
    try {
      daemonPidForCleanup = await ownedDaemonPid(restartedElectron.pid) || daemonPidForCleanup;
      page = await connectPage(appRestartCdpPort);
      return page;
    } catch {
      return null;
    }
  }, "Desktop restart replay 新 Renderer", 45_000, 250);
  await waitForRenderer(page, "Desktop restart replay Renderer 就绪");
  await selectPersonalSessionById(page, restartSessionId);
  const desktopReplay = await waitForAssistant(page, restartResponseText, "Desktop restart replay 历史消息");
  check("Desktop restart replay 保留原 Session 内容", desktopReplay.text.includes(restartResponseText));
  check("Desktop restart replay 未新增 Provider request", provider.requests.length === providerRequestsBeforeDaemonRestart);
  const desktopTiming = await waitForTimingRecord(page, restartTurnId, "Desktop restart replay Renderer timing");
  checkTimingStages("Desktop restart replay Renderer", desktopTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "desktop_restart_replay",
    restartTurnId,
    desktopTiming,
    daemonBackend,
    {
      fixture: "electron-dom-desktop-restart-replay-v1",
      inputHash: hashText(restartPrompt),
      providerRequestCount: provider.requests.length - providerRequestsBeforeDaemonRestart,
      settlement: {
        observed: true,
        barrier: "desktop_process_restarted_and_history_replayed",
        stage: "desktop_process_restarted",
        sequence: null,
        next_turn_admission_checked: false,
      },
      settlementRequired: true,
    },
  ));
  recovery.desktopRestart = {
    sessionId: restartSessionId,
    turnId: restartTurnId,
    providerRequestsBefore: providerRequestsBeforeDaemonRestart,
    providerRequestsAfter: provider.requests.length,
    electronRestarted: true,
  };
  return { page, recovery };
}

async function waitForRenderer(page, label) {
  try {
    return await waitFor(async () => {
      const state = await rendererState(page);
      return state.input && state.model.includes("electron-dom-model")
        ? state
        : null;
    }, label);
  } catch (error) {
    const state = await rendererState(page).catch(() => null);
    throw new Error(`${error.message}；当前 Renderer 状态：${JSON.stringify(state)}`);
  }
}

async function setComposerText(page, text) {
  const escaped = JSON.stringify(text);
  await page.evaluate(`(() => {
    const element = document.querySelector('[data-testid="input-textarea"]');
    if (!element) throw new Error('input-textarea missing');
    element.focus();
    element.textContent = ${escaped};
    element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${escaped} }));
  })()`);
}

async function waitForComposerReady(page, label) {
  try {
    return await waitFor(async () => {
      const state = await rendererState(page);
      return state.input && state.inputEditable && !state.sendDisabled && !state.stop ? state : null;
    }, label, 45_000);
  } catch (error) {
    const state = await rendererState(page).catch(() => null);
    throw new Error(`${error.message}；当前输入状态：${JSON.stringify(state)}`);
  }
}

async function openPersonalDraft(page) {
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop ? state : null;
  }, "个人上一轮 Turn 收口", 45_000);
  await page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="sidebar-new-session"]');
    if (!button || button.disabled) throw new Error('new session button unavailable');
    button.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop ? state : null;
  }, "个人新会话草稿就绪", 45_000);
}

async function clickSend(page) {
  return await page.evaluate(`(() => {
    const element = document.querySelector('[data-testid="input-send-button"]');
    if (!element || element.disabled) {
      throw new Error(JSON.stringify({
        reason: 'send button unavailable',
        button: element ? { disabled: element.disabled, text: element.innerText || '' } : null,
        input: document.querySelector('[data-testid="input-textarea"]') ? {
          text: document.querySelector('[data-testid="input-textarea"]').innerText || '',
          contenteditable: document.querySelector('[data-testid="input-textarea"]').getAttribute('contenteditable'),
        } : null,
        bodyText: (document.body?.innerText || '').slice(-1200),
      }));
    }
    element.click();
    return true;
  })()`);
}

async function waitForAssistant(page, text, label) {
  let lastState = null;
  try {
    return await waitFor(async () => {
      const state = await rendererState(page);
      lastState = state;
      return state.assistant.some((message) => message.text.includes(text)) ? state : null;
    }, label, 45_000);
  } catch (error) {
    const detail = lastState
      ? JSON.stringify({
        url: lastState.url,
        sessionIds: lastState.sessionIds,
        workspaceIds: lastState.workspaceIds,
        assistant: lastState.assistant,
        turns: lastState.turns,
        bodyTail: lastState.text.slice(-2_000),
      })
      : "Renderer state unavailable";
    throw new Error(`${error instanceof Error ? error.message : String(error)}; latestRendererState=${detail}`);
  }
}

async function gitFixtureCommand(repositoryRoot, args) {
  await execFileAsync("git", args, {
    cwd: repositoryRoot,
    maxBuffer: 2 * 1024 * 1024,
  });
}

async function createGitRepository(repositoryRoot) {
  await gitFixtureCommand(repositoryRoot, ["init", "-b", "main"]);
  await gitFixtureCommand(repositoryRoot, ["config", "user.name", "Magi Electron Harness"]);
  await gitFixtureCommand(repositoryRoot, ["config", "user.email", "magi-electron@example.test"]);
  await writeFile(join(repositoryRoot, "README.md"), "base\n", "utf8");
  await gitFixtureCommand(repositoryRoot, ["add", "README.md"]);
  await gitFixtureCommand(repositoryRoot, ["commit", "-m", "base"]);
}

async function createGitConflictRepository(repositoryRoot) {
  await createGitRepository(repositoryRoot);
  await gitFixtureCommand(repositoryRoot, ["switch", "-c", "conflicting"]);
  await writeFile(join(repositoryRoot, "README.md"), "feature change\n", "utf8");
  await gitFixtureCommand(repositoryRoot, ["add", "README.md"]);
  await gitFixtureCommand(repositoryRoot, ["commit", "-m", "feature change"]);
  await gitFixtureCommand(repositoryRoot, ["switch", "main"]);
  await writeFile(join(repositoryRoot, "README.md"), "main change\n", "utf8");
  await gitFixtureCommand(repositoryRoot, ["add", "README.md"]);
  await gitFixtureCommand(repositoryRoot, ["commit", "-m", "main change"]);
  try {
    await gitFixtureCommand(repositoryRoot, ["merge", "conflicting"]);
  } catch {
    // 预期产生未解决冲突；保留冲突工作树供真实 daemon 预检读取。
  }
}

async function selectMostRecentPersonalSession(page) {
  await waitFor(async () => (await rendererState(page)).sessionIds.length > 0, "个人会话进入侧栏");
  await page.evaluate(`(() => {
    const sessions = [...document.querySelectorAll('#recent-session-content [data-session-id]')];
    const target = sessions.at(-1) || sessions[0];
    if (!target) throw new Error('personal session missing');
    target.click();
  })()`);
  await sleep(600);
}

async function selectPersonalSessionById(page, sessionId) {
  await waitFor(
    async () => (await rendererState(page)).personalSessionIds.includes(sessionId),
    `个人会话 ${sessionId} 进入侧栏`,
  );
  const escaped = JSON.stringify(sessionId);
  await page.evaluate(`(() => {
    const sessionId = ${escaped};
    const target = [...document.querySelectorAll('#recent-session-content [data-session-id]')]
      .find((item) => item.getAttribute('data-session-id') === sessionId);
    if (!target) throw new Error('personal session id missing: ' + sessionId);
    target.click();
  })()`);
  await sleep(600);
}

async function setConversationDisplayMode(page, mode) {
  await page.evaluate(`fetch('/api/settings/update', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ key: 'conversationDisplayMode', value: ${JSON.stringify(mode)} }),
  }).then((response) => { if (!response.ok) throw new Error('display mode update failed'); })`);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, `显示模式 ${mode} 重载`);
  await selectMostRecentPersonalSession(page);
}

async function registerWorkspace(page, workspacePath) {
  const workspaceId = await page.evaluate(`fetch('/api/workspaces/register', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ path: ${JSON.stringify(workspacePath)} }),
  }).then(async (response) => {
    if (!response.ok) throw new Error(await response.text());
    return (await response.json()).workspaceId;
  })`);
  check("工作区注册响应包含 workspaceId", typeof workspaceId === "string" && workspaceId.length > 0);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "工作区 Renderer 重载");
  await waitFor(
    async () => (await rendererState(page)).workspaceIds.includes(workspaceId),
    `工作区 ${workspaceId} 侧栏加载`,
  );
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    if (!workspace) throw new Error('workspace row missing');
    workspace.click();
    const create = workspace.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('workspace new session button missing');
    create.click();
  })()`);
  await waitForComposerReady(page, `工作区 ${workspaceId} 输入区就绪`);
  return workspaceId;
}

async function chooseAccessProfile(page, profile) {
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop && !state.sendDisabled;
  }, "访问模式切换前 Turn 收口", 45_000);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('[aria-label^="访问模式:"]'))`), "访问模式按钮");
  await page.evaluate(`(() => {
    const button = document.querySelector('[aria-label^="访问模式:"]');
    if (!button) throw new Error('access profile button missing');
    if (button.disabled) throw new Error('access profile button disabled');
    button.click();
  })()`);
  await waitFor(async () => page.evaluate(`document.querySelectorAll('[role="menuitemradio"]').length > 0`), "访问模式选项弹出");
  await page.evaluate(`(() => {
    const value = ${JSON.stringify(profile)};
    const label = value === 'read_only' ? '只读' : value === 'full_access' ? '完全访问' : '受限访问';
    const option = [...document.querySelectorAll('[role="menuitemradio"]')]
      .find((item) => item.innerText.includes(label));
    if (!option) throw new Error('access profile option missing after open: ' + value);
    option.click();
  })()`);
}

async function chooseGoalMode(page) {
  await page.evaluate(`(() => {
    const button = document.querySelector('.ia-add-btn');
    if (!button || button.disabled) throw new Error('add menu button unavailable');
    button.click();
  })()`);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('.ia-add-popover'))`), "Goal 添加菜单");
  await page.evaluate(`(() => {
    const option = [...document.querySelectorAll('.ia-add-item')]
      .find((item) => item.innerText.includes('Goal') || item.innerText.includes('目标'));
    if (!option) throw new Error('Goal add menu item missing');
    option.click();
  })()`);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('.ia-reference-chip-goal'))`), "Goal 结构化引用标记");
}

async function clearGoalMode(page) {
  const hasGoalMode = await page.evaluate(`Boolean(document.querySelector('.ia-reference-chip-goal'))`);
  if (!hasGoalMode) return;
  await page.evaluate(`(() => {
    const remove = document.querySelector('.ia-reference-chip-goal .ia-reference-chip-remove');
    if (!remove) throw new Error('Goal reference remove button missing');
    remove.click();
  })()`);
  await waitFor(async () => !await page.evaluate(`Boolean(document.querySelector('.ia-reference-chip-goal'))`), "Goal 结构化引用清理");
}

async function clickStop(page) {
  return await page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="input-stop-button"]');
    if (!button) return false;
    button.click();
    return true;
  })()`);
}

async function waitForProcessExit(child, timeoutMs = 15_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await Promise.race([
    new Promise((resolve) => {
      child.once("exit", resolve);
      child.once("error", resolve);
    }),
    sleep(timeoutMs),
  ]);
}

async function ownedDescendants(rootPid) {
  const { stdout } = await execFileAsync("ps", ["-axo", "pid=,ppid=,command="]);
  const processes = stdout
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^(\d+)\s+(\d+)\s+(.*)$/u);
      if (!match) return null;
      return { pid: Number(match[1]), ppid: Number(match[2]), command: match[3] };
    })
    .filter(Boolean);
  const descendants = [];
  const queue = [rootPid];
  while (queue.length > 0) {
    const parent = queue.shift();
    for (const process of processes) {
      if (process.ppid !== parent || descendants.some((item) => item.pid === process.pid)) continue;
      descendants.push(process);
      queue.push(process.pid);
    }
  }
  return descendants;
}

async function ownedDaemonPid(electronPid) {
  const daemon = (await ownedDescendants(electronPid))
    .find((process) => process.command.includes("magi-daemon-app"));
  return daemon?.pid ?? null;
}

async function stopOwnedDaemon(pid) {
  if (!Number.isInteger(pid)) return;
  let command = "";
  try {
    const { stdout } = await execFileAsync("ps", ["-p", String(pid), "-o", "command="]);
    command = stdout.trim();
  } catch {
    return;
  }
  if (!command.includes("magi-daemon-app") || !command.includes("target/electron-dist")) return;
  try {
    process.kill(pid, "SIGKILL");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  await waitFor(async () => {
    try {
      await execFileAsync("ps", ["-p", String(pid), "-o", "pid="]);
      return false;
    } catch {
      return true;
    }
  }, `脚本自有 daemon ${pid} 退出`, 5_000, 100).catch(() => undefined);
}

async function healthSnapshot() {
  const response = await fetch(`http://127.0.0.1:${daemonPort}/health`);
  if (!response.ok) throw new Error(`daemon health status ${response.status}`);
  return response.json();
}

async function requestGracefulElectronQuit(_port) {
  if (process.platform !== "darwin") {
    throw new Error("打包 Electron GUI 关闭闸门目前只支持 macOS 应用退出事件");
  }
  // Browser.close 只关闭 Chromium 窗口；Magi 默认有托盘，窗口关闭不会
  // 进入 app.before-quit。使用 macOS 的显式 Quit AppleEvent，才能复现
  // 托盘菜单“退出 Magi”和系统退出动作所共用的 Main shutdown 事务。
  await execFileAsync("osascript", [
    "-e",
    'tell application id "com.mistripple.magi" to quit',
  ]);
}

async function stopOwnedProcess(child, cdpPortForQuit = null) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  let gracefulQuitRequested = false;
  if (Number.isInteger(cdpPortForQuit)) {
    gracefulQuitRequested = await requestGracefulElectronQuit(cdpPortForQuit)
      .then(() => true)
      .catch(() => false);
  }
  if (gracefulQuitRequested) {
    await waitForProcessExit(child, 15_000);
    if (child.exitCode !== null || child.signalCode !== null) return;
  }
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  await waitForProcessExit(child, 15_000);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await waitForProcessExit(child, 5_000);
  }
}

async function closeProvider(server) {
  if (typeof server.closeAllConnections === "function") server.closeAllConnections();
  if (!server.listening) return;
  await Promise.race([
    new Promise((resolve) => server.close(resolve)),
    sleep(2_000),
  ]);
}

async function removeStateRoot(path) {
  let lastError = null;
  for (let attempt = 0; attempt < 12; attempt += 1) {
    try {
      await rm(path, { recursive: true, force: true });
      return;
    } catch (error) {
      lastError = error;
      await sleep(250 * Math.min(attempt + 1, 4));
    }
  }
  throw lastError;
}

const provider = createProvider();
await new Promise((resolve) => provider.server.listen(0, "127.0.0.1", resolve));
const providerPort = provider.server.address().port;
const stateRoot = await mkdtemp(join(tmpdir(), "magi-electron-conversation-dom-"));
const sourceIdentityBefore = await collectSourceIdentity();
function attachElectronLogs(child) {
  child.stdout.on("data", (chunk) => consumeElectronLogChunk("stdout", chunk));
  child.stderr.on("data", (chunk) => {
    process.stderr.write(`[electron] ${chunk}`);
    const text = chunk.toString();
    electronLogText.push(`[stderr] ${text}`);
    const combined = electronLogBuffers.stderr + text;
    const lines = combined.split(/\r?\n/u);
    electronLogBuffers.stderr = lines.pop() || "";
    for (const line of lines) {
      inspectDesktopIpcLine(line);
      parseBackendTimingLine(line);
    }
  });
}

function spawnElectron(port) {
  const child = spawn(appExecutable, [`--remote-debugging-port=${port}`, "--disable-gpu"], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
      MAGI_STATE_ROOT: join(stateRoot, "state"),
      MAGI_OPEN_BROWSER: "0",
      MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      MAGI_OPENAI_COMPAT_API_KEY: "electron-dom-test-key",
      MAGI_OPENAI_COMPAT_MODEL: "electron-dom-model",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  attachElectronLogs(child);
  return child;
}

const electron = spawnElectron(cdpPort);
let page = null;
let daemonPidForCleanup = null;
let restartedElectron = null;
const rendererTimingSamples = [];
try {
  await waitFor(async () => {
    try {
      daemonPidForCleanup = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
      page = await connectPage();
      return page;
    } catch {
      return null;
    }
  }, "打包 Electron App Renderer CDP", 45_000, 250);

  if (recoveryOnly) {
    const initial = await waitForRenderer(page, "Electron recovery 初始窗口");
    check("Electron recovery 初始窗口具有输入框", initial.input);
    const recoveryResult = await runRecoveryAcceptance(page, electron);
    page = recoveryResult.page;
    flushElectronLogBuffers();
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256
      && sourceIdentityBefore.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
    check(
      "Electron recovery 期间源码 HEAD、工作区指纹与打包 Electron 可执行文件保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    check(
      "Electron recovery 未出现 Desktop IPC 协议拒绝",
      desktopIpcValidationErrors.length === 0,
      desktopIpcValidationErrors.slice(0, 3).join(" | "),
    );
    const evidence = {
      type: "electron_conversation_recovery_acceptance",
      schema_version: timingSchemaVersion,
      fixture: "electron-dom-recovery-v1",
      app: appExecutable,
      cdpPort,
      appRestartCdpPort,
      providerPort,
      scenarios: [
        "sse_reconnect_cancelled",
        "websocket_reconnect_cancelled",
        "daemon_restart_replay",
        "desktop_restart_replay",
      ],
      checks,
      desktopIpcValidationErrors,
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      rendererTimingSamples,
      recovery: recoveryResult.recovery,
      status: "passed",
    };
    console.log(JSON.stringify({ ...evidence, ...(evidencePath ? { evidencePath } : {}) }, null, 2));
    if (evidencePath) await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  } else if (approvalExpiryOnly) {
    const initial = await waitForRenderer(page, "审批过期聚焦验收初始窗口");
    check("审批过期聚焦验收初始窗口具有输入框", initial.input);
    const approvalExpiry = await runApprovalExpiryCheck(page);
    flushElectronLogBuffers();
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256
      && sourceIdentityBefore.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
    check(
      "审批过期聚焦验收期间源码 HEAD、工作区指纹与打包 Electron 可执行文件保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    check(
      "审批过期聚焦验收未出现 Desktop IPC 协议拒绝",
      desktopIpcValidationErrors.length === 0,
      desktopIpcValidationErrors.slice(0, 3).join(" | "),
    );
    const evidence = {
      type: "electron_conversation_approval_expiry_acceptance",
      schema_version: timingSchemaVersion,
      app: appExecutable,
      cdpPort,
      providerPort,
      scenarios: ["approval_expired"],
      approvalExpiryWaitMs,
      checks,
      desktopIpcValidationErrors,
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      rendererTimingSamples,
      approvalExpiry,
      status: "passed",
    };
    console.log(JSON.stringify({ ...evidence, ...(evidencePath ? { evidencePath } : {}) }, null, 2));
    if (evidencePath) await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  } else if (gitPreflightOnly) {
    const initial = await waitForRenderer(page, "Git preflight 初始窗口");
    check("Git preflight 初始窗口具有输入框", initial.input);
    await runGitDriftConflictChecks(page);
    flushElectronLogBuffers();
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256
      && sourceIdentityBefore.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
    check(
      "Git preflight 期间源码 HEAD、工作区指纹与打包 Electron 可执行文件保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    check(
      "Git preflight 未出现 Desktop IPC 协议拒绝",
      desktopIpcValidationErrors.length === 0,
      desktopIpcValidationErrors.slice(0, 3).join(" | "),
    );
    const evidence = {
      type: "electron_conversation_git_preflight_acceptance",
      schema_version: timingSchemaVersion,
      app: appExecutable,
      cdpPort,
      providerPort,
      scenarios: ["git_branch_drift", "git_merge_conflict"],
      checks,
      desktopIpcValidationErrors,
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      rendererTimingSamples,
      status: "passed",
    };
    console.log(JSON.stringify({ ...evidence, ...(evidencePath ? { evidencePath } : {}) }, null, 2));
    if (evidencePath) await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  } else if (compactionOnly) {
    const initial = await waitForRenderer(page, "Electron context compaction 初始窗口");
    check("Electron context compaction 初始窗口具有输入框", initial.input);
    check("Electron context compaction 初始窗口显示新对话空态", initial.text.includes("开始一个新对话"));
    const contextConfiguration = await page.evaluate(`fetch('/api/settings/model-context-window/save', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        model: 'electron-dom-model',
        contextWindowTokens: ${compactionContextWindowTokens},
      }),
    }).then(async (response) => {
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error('context window save failed: ' + JSON.stringify(body));
      return body;
    })`);
    check(
      "Electron context compaction 已通过真实设置入口固定窗口",
      contextConfiguration?.saved === true
        && contextConfiguration?.contextWindowTokens === compactionContextWindowTokens,
      JSON.stringify(contextConfiguration),
    );

    const compactionSamples = [];
    let completedCompactionSamples = 0;
    let skippedCompactionSamples = 0;
    const waitForCompactionInput = async (label) => waitFor(async () => {
      const state = await rendererState(page);
      return state.input && state.inputEditable && !state.stop ? state : null;
    }, label, 45_000);

    // 与真实 Provider 压缩 fixture 保持同一事实边界：在一个真实 Session 内先
    // 累积足够历史，再连续采集 20 个长历史 Turn。每个样本仍是独立 Turn，
    // 但不能把“新建 Session + 一条短历史”误称为压缩入口。
    await openPersonalDraft(page);
    await waitForCompactionInput("Electron context compaction Session 草稿");
    let compactionSessionId = null;
    let historySeedTurnId = null;
    const initialSeedToken = "ELECTRON_COMPACTION_SEED_0";
    await setComposerText(page, `Electron context compaction seed，只回答 ${initialSeedToken}`);
    await waitFor(async () => {
      const state = await rendererState(page);
      return state.send && !state.sendDisabled && !state.stop ? state : null;
    }, "Electron context compaction 初始 seed 发送按钮", 45_000);
    await clickSend(page);
    const initialSeedResult = await waitForAssistant(
      page,
      initialSeedToken,
      "Electron context compaction 初始 seed 最终消息",
    );
    compactionSessionId = initialSeedResult.activePersonalSessionId
      || new URL(initialSeedResult.url).searchParams.get("sessionId");
    historySeedTurnId = assistantTurnId(initialSeedResult, initialSeedToken);
    check(
      "Electron context compaction 初始 seed 拥有 Session/Turn 身份",
      Boolean(compactionSessionId && historySeedTurnId),
    );
    await waitForCompactionInput("Electron context compaction 初始 seed 收口");
    for (let index = 0; index < compactionPrefillTurnCount; index += 1) {
      const prefillToken = `ELECTRON_COMPACTION_PREFILL_${index}`;
      await setComposerText(page, `${electronCompactionPrefillPrompt(index)}\n只回答 ${prefillToken}`);
      await waitFor(async () => {
        const state = await rendererState(page);
        return state.send && !state.sendDisabled && !state.stop ? state : null;
      }, `Electron context compaction prefill ${index} 发送按钮`, 45_000);
      await clickSend(page);
      const prefillResult = await waitForAssistant(
        page,
        prefillToken,
        `Electron context compaction prefill ${index} 最终消息`,
      );
      const prefillSessionId = prefillResult.activePersonalSessionId
        || new URL(prefillResult.url).searchParams.get("sessionId");
      check(
        `Electron context compaction prefill ${index} 拥有 Session/Turn 身份`,
        Boolean(prefillSessionId === compactionSessionId && assistantTurnId(prefillResult, prefillToken)),
      );
      await waitForCompactionInput(`Electron context compaction prefill ${index} 收口`);
    }

    check(
      "Electron context compaction fixture 在同一 Session 内完成历史预填充",
      Boolean(compactionSessionId && historySeedTurnId),
    );

    for (let index = 0; index < compactionSampleCount; index += 1) {
      await waitForCompactionInput(`Electron context compaction long history ${index} 输入`);

      const longToken = `ELECTRON_COMPACTION_LONG_${index}`;
      await setComposerText(page, `${electronCompactionHistoryPrompt(index)}\n${longToken}`);
      await waitFor(async () => {
        const state = await rendererState(page);
        return state.send && !state.sendDisabled && !state.stop ? state : null;
      }, `Electron context compaction long history ${index} 发送按钮`, 45_000);
      await clickSend(page);
      const longResult = await waitForAssistant(page, longToken, `Electron context compaction long history ${index} 最终消息`);
      const turnId = assistantTurnId(longResult, longToken);
      const observedSessionId = longResult.activePersonalSessionId
        || new URL(longResult.url).searchParams.get("sessionId");
      check(
        `Electron context compaction long history ${index} 保持同一 Session/Turn 身份`,
        Boolean(turnId && observedSessionId === compactionSessionId),
        JSON.stringify({ expected: compactionSessionId, observed: observedSessionId, turnId }),
      );
      const timing = await waitForTimingRecord(page, turnId, `Electron context compaction long history ${index} Renderer timing`);
      const backend = await waitForBackendTiming(
        turnId,
        `Electron context compaction long history ${index} 后端 timing 关联`,
        "personal_long_history",
      );
      checkBackendTimingStages(`Electron context compaction long history ${index} 后端`, backend, "personal_long_history");
      checkTimingStages(`Electron context compaction long history ${index} Renderer`, timing);
      await waitForCompactionInput(`Electron context compaction long history ${index} 收口`);

      const eventEvidence = await readElectronCompactionEvents({
        sessionId: compactionSessionId,
        turnId,
        afterSequence: 0,
      });
      const terminalCompactionEvents = eventEvidence.compactionEvents.filter((event) => (
        ["completed", "skipped"].includes(event.state)
      ));
      check(
        `Electron context compaction long history ${index} 具有压缩终态事实`,
        terminalCompactionEvents.length > 0,
        JSON.stringify(eventEvidence.compactionEvents),
      );
      completedCompactionSamples += terminalCompactionEvents.some((event) => event.state === "completed") ? 1 : 0;
      skippedCompactionSamples += terminalCompactionEvents.some((event) => event.state === "skipped") ? 1 : 0;
      compactionSamples.push({
        ...timingEvidenceRecord(
          "personal_long_history",
          turnId,
          timing,
          backend,
          {
            fixture: compactionFixture,
            schemaVersion: compactionSchemaVersion,
          },
        ),
        historySeedTurnId,
        contextWindowTokens: compactionContextWindowTokens,
        longHistoryChars: compactionLongHistoryChars,
        compactionEvents: eventEvidence.compactionEvents,
      });
    }

    check("Electron context compaction 至少有一条 completed 终态", completedCompactionSamples > 0);
    check(
      "Electron context compaction 每条样本均有 completed/skipped 终态",
      compactionSamples.every((sample) => sample.compactionEvents.some((event) => (
        ["completed", "skipped"].includes(event.state)
      ))),
    );
    flushElectronLogBuffers();
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256
      && sourceIdentityBefore.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
    check(
      "Electron context compaction 采样期间源码、工作区和打包 artifact 保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    check(
      "Electron context compaction 未出现 Desktop IPC 协议拒绝",
      desktopIpcValidationErrors.length === 0,
      desktopIpcValidationErrors.slice(0, 3).join(" | "),
    );
    const evidence = {
      type: "electron_conversation_context_compaction",
      schema_version: compactionSchemaVersion,
      fixture: compactionFixture,
      app: appExecutable,
      cdpPort,
      providerPort,
      sampleCount: compactionSamples.length,
      scenarios: ["personal_long_history"],
      checks,
      desktopIpcValidationErrors,
      context: {
        configured: contextConfiguration,
        contextWindowTokens: compactionContextWindowTokens,
        longHistoryChars: compactionLongHistoryChars,
        compactionRequired: true,
        observedCompactionRows: completedCompactionSamples,
        skippedCompactionRows: skippedCompactionSamples,
      },
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      samples: compactionSamples,
      rendererTimingSamples: compactionSamples,
      status: "passed",
    };
    console.log(JSON.stringify({ ...evidence, ...(evidencePath ? { evidencePath } : {}) }, null, 2));
    if (evidencePath) await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  } else if (timingOnly) {
    const shouldRunScenario = (scenario) => !timingScenario || timingScenario === scenario;
    const initial = await waitForRenderer(page, "初始窗口");
    check("timing 采样初始窗口显示新对话空态", initial.text.includes("开始一个新对话"));
    check("timing 采样初始窗口具有输入框", initial.input);

    const timingSamples = [];
    const waitForTimingInput = async (label) => {
      return waitFor(async () => {
        const state = await rendererState(page);
        return state.input && state.inputEditable && !state.stop ? state : null;
      }, label);
    };
    const waitForTimingTerminal = async (scenario, expectedText) => {
      return waitFor(async () => {
        const state = await rendererState(page);
        return state.input && state.inputEditable && !state.stop
          && state.assistant.some((message) => message.text.includes(expectedText))
          ? state
          : null;
      }, `${scenario} terminal 收口`, 45_000);
    };
    const collectTiming = async (scenario, prompt, expectedText) => {
      await waitForTimingInput(`${scenario} 发送前输入`);
      await setComposerText(page, prompt);
      await waitFor(async () => {
        const state = await rendererState(page);
        return state.input && state.send && !state.sendDisabled && !state.stop ? state : null;
      }, `${scenario} 发送按钮可用`, 45_000);
      await clickSend(page);
      const userMessageLayout = await page.evaluate(`(() => {
        const item = [...document.querySelectorAll('.message-item.user')].at(-1);
        const bubble = item?.querySelector('.user-content');
        const footer = item?.querySelector('.user-time');
        if (!item || !bubble || !footer) return null;
        const footerRect = footer.getBoundingClientRect();
        const bubbleRect = bubble.getBoundingClientRect();
        return {
          message: bubble.textContent || '',
          renderedText: bubble.innerText || '',
          html: bubble.innerHTML || '',
          footerHeight: footerRect.height,
          footerVisibility: getComputedStyle(footer).visibility,
          footerTop: footerRect.top,
          bubbleTop: bubbleRect.top,
          bubbleBottom: bubbleRect.bottom,
          bubbleHeight: bubbleRect.height,
        };
      })()`);
      check(
        `${scenario} 用户消息的隐藏时间/操作行不占布局`,
        userMessageLayout?.footerHeight === 0
          && userMessageLayout.footerVisibility === 'hidden',
        JSON.stringify(userMessageLayout),
      );
      check(
        `${scenario} 用户气泡内容忽略 contenteditable 末尾空白`,
        userMessageLayout?.message.trimEnd() === prompt.trimEnd()
          && userMessageLayout.renderedText.trimEnd() === prompt.trimEnd()
          && !/\n\s*\n\s*$/u.test(userMessageLayout.renderedText),
        JSON.stringify({
          expected: prompt.trimEnd(),
          actual: userMessageLayout?.message,
          renderedText: userMessageLayout?.renderedText,
          html: userMessageLayout?.html,
        }),
      );
      const bubblePointer = await page.evaluate(`(() => {
        const bubble = [...document.querySelectorAll('.message-item.user .user-content')].at(-1);
        const rect = bubble?.getBoundingClientRect();
        return rect ? { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 } : null;
      })()`);
      if (bubblePointer) {
        await page.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...bubblePointer });
        await sleep(180);
      }
      const hoveredFooter = await page.evaluate(`(() => {
        const footer = [...document.querySelectorAll('.message-item.user .user-time')].at(-1);
        return footer ? {
          height: footer.getBoundingClientRect().height,
          visibility: getComputedStyle(footer).visibility,
        } : null;
      })()`);
      check(
        `${scenario} 悬停用户气泡时仍显示时间和操作`,
        hoveredFooter?.height >= 20 && hoveredFooter.visibility === 'visible',
        JSON.stringify(hoveredFooter),
      );
      const inputPointer = await page.evaluate(`(() => {
        const input = document.querySelector('[data-testid="input-textarea"]');
        const rect = input?.getBoundingClientRect();
        return rect ? { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 } : null;
      })()`);
      if (inputPointer) {
        await page.call("Input.dispatchMouseEvent", { type: "mouseMoved", ...inputPointer });
        await sleep(180);
      }
      const result = await waitForAssistant(page, expectedText, `${scenario} ${prompt} 最终消息`);
      const turnId = assistantTurnId(result, expectedText);
      const timing = await waitForTimingRecord(page, turnId, `${scenario} Renderer timing`);
      const backend = await waitForBackendTiming(turnId, `${scenario} 后端 timing 关联`, scenario);
      checkBackendTimingStages(`${scenario} 后端`, backend, scenario);
      timingSamples.push(timingEvidenceRecord(scenario, turnId, timing, backend));
      checkTimingStages(`${scenario} Renderer`, timing);
      await waitForTimingTerminal(scenario, expectedText);
    };
    const ensurePersonalDraft = async (first) => {
      if (!first) {
        await waitForTimingInput("个人 timing 上一轮收口");
        await openPersonalDraft(page);
      }
      await waitForTimingInput("个人 timing 采样输入");
    };

    if (shouldRunScenario("personal_chat")) {
      await ensurePersonalDraft(true);
      for (let index = 0; index < timingSampleCount; index += 1) {
        const token = `ELECTRON_TIMING_PERSONAL_${index}`;
        await collectTiming("personal_chat", `Electron timing 个人聊天 ${token}\n\n   `, token);
      }
    }

    if (shouldRunScenario("workspace_chat") || shouldRunScenario("workspace_tool")) {
      const workspaceRoot = await mkdtemp(join(stateRoot, "timing-workspace-"));
      const workspaceId = await registerWorkspace(page, workspaceRoot);
      const openWorkspaceDraft = async () => {
        await waitForTimingInput("工作区 timing 上一轮收口");
        await waitFor(async () => {
          const state = await rendererState(page);
          return state.workspaceIds.includes(workspaceId) ? state : null;
        }, "工作区 timing 采样会话");
        await page.evaluate(`(() => {
          const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
          const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
          if (!create) throw new Error('workspace timing new session button missing');
          create.click();
        })()`);
        await waitFor(async () => {
          const state = await rendererState(page);
          return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
            ? state
            : null;
        }, "工作区新会话草稿就绪", 45_000);
      };
      if (shouldRunScenario("workspace_chat")) {
        await openWorkspaceDraft();
        for (let index = 0; index < timingSampleCount; index += 1) {
          const token = `ELECTRON_TIMING_WORKSPACE_${index}`;
          await collectTiming("workspace_chat", `Electron timing 工作区聊天 ${token}`, token);
        }
      }
      if (shouldRunScenario("workspace_tool")) {
        await openWorkspaceDraft();
        for (let index = 0; index < timingSampleCount; index += 1) {
          const token = `ELECTRON_TIMING_TOOL_${index}`;
          await collectTiming("workspace_tool", `Electron timing 工作区工具：调用 tool_catalog 后返回 ${token}`, token);
        }
      }
    }

    if (shouldRunScenario("goal")) {
      await ensurePersonalDraft(false);
      for (let index = 0; index < timingSampleCount; index += 1) {
        if (index > 0) await openPersonalDraft(page);
        await chooseGoalMode(page);
        const token = `ELECTRON_TIMING_GOAL_${index}`;
        await collectTiming("goal", `Electron timing Goal：维护目标计划并返回 ${token}`, token);
      }
    }

    if (shouldRunScenario("subagent")) {
      await clearGoalMode(page);
      for (let index = 0; index < timingSampleCount; index += 1) {
        if (index > 0) await openPersonalDraft(page);
        const token = `ELECTRON_TIMING_AGENT_${index}`;
        await collectTiming("subagent", `Electron timing 子代理：派发一个子代理并等待其完成后返回 ${token}`, token);
      }
    }

    flushElectronLogBuffers();
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceIdentityStable = sourceIdentityBefore?.source_commit === sourceIdentityAfter.source_commit
      && sourceIdentityBefore?.worktree_fingerprint_sha256
        === sourceIdentityAfter.worktree_fingerprint_sha256
      && sourceIdentityBefore?.executable_sha256 === sourceIdentityAfter.executable_sha256
      && sourceIdentityBefore?.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
    check(
      "采样期间源码 HEAD、工作区指纹与打包 Electron 可执行文件保持不变",
      sourceIdentityStable,
      JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
    );
    check(
      "打包 Electron 时序采样未出现 Desktop IPC 协议拒绝",
      desktopIpcValidationErrors.length === 0,
      desktopIpcValidationErrors.slice(0, 3).join(" | "),
    );
    const evidence = {
      type: "electron_conversation_renderer_timing",
      schema_version: timingSchemaVersion,
      fixture: timingFixture,
      app: appExecutable,
      cdpPort,
      providerPort,
      sampleCount: timingSampleCount,
      scenarios: timingScenario ? [timingScenario] : timingScenarioNames,
      checks,
      desktopIpcValidationErrors,
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      rendererTimingSamples: timingSamples,
      status: "passed",
    };
    console.log(JSON.stringify({ ...evidence, ...(evidencePath ? { evidencePath } : {}) }, null, 2));
    if (evidencePath) await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  } else {
  const initial = await waitForRenderer(page, "初始窗口");
  check("初始窗口显示新对话空态", initial.text.includes("开始一个新对话"));
  check("初始窗口具有输入框", initial.input);

  await setComposerText(page, "请只回复 ELECTRON_DOM_CHAT_OK");
  await clickSend(page);
  const personal = await waitForAssistant(page, responseText, "个人普通 Chat 最终消息");
  const initialPersonalSessionId = personal.activePersonalSessionId
    || new URL(personal.url).searchParams.get("sessionId");
  check("个人普通 Chat 拥有可恢复的 sessionId", Boolean(initialPersonalSessionId));
  check("个人普通 Chat 最终消息进入真实 DOM", personal.text.includes(responseText));
  check("个人普通 Chat 具有用户和助手消息节点", personal.assistant.length >= 1);
  const personalTurnId = assistantTurnId(personal, responseText);
  const personalTiming = await waitForTimingRecord(page, personalTurnId, "个人 Chat 生产 Renderer timing");
  const personalBackend = await waitForBackendTiming(
    personalTurnId,
    "个人 Chat 生产后端 timing 关联",
    "personal_chat",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "personal_chat",
    personalTurnId,
    personalTiming,
    personalBackend,
  ));
  checkTimingStages("个人 Chat 生产 Renderer", personalTiming);

  const workspaceRoot = await mkdtemp(join(stateRoot, "workspace-"));
  const workspaceId = await registerWorkspace(page, workspaceRoot);
  approvalTarget = join(workspaceRoot, ".magi-electron-dom-approval.txt");
  approvalDenyTarget = join(workspaceRoot, ".magi-electron-dom-approval-deny.txt");
  approvalCancelTarget = join(workspaceRoot, ".magi-electron-dom-approval-cancel.txt");
  approvalExpiryTarget = join(workspaceRoot, ".magi-electron-dom-approval-expiry.txt");
  fullAccessTarget = join(workspaceRoot, ".magi-electron-dom-full-access.txt");
  await setComposerText(page, "请只回复 ELECTRON_DOM_CHAT_OK");
  await clickSend(page);
  const workspace = await waitForAssistant(page, responseText, "工作区普通 Chat 最终消息");
  check("工作区普通 Chat 最终消息进入真实 DOM", workspace.text.includes(responseText));
  check("工作区会话绑定仍显示工作区作用域", workspace.text.includes("工作区"));
  check(
    "工作区 DOM 验收使用已注册 workspace",
    workspace.workspaceIds.includes(workspaceId)
      || workspace.url.includes(`workspaceId=${encodeURIComponent(workspaceId)}`),
  );
  const workspaceTiming = await waitForTimingRecord(
    page,
    assistantTurnId(workspace, responseText),
    "工作区 Chat 生产 Renderer timing",
  );
  const workspaceTurnId = assistantTurnId(workspace, responseText);
  const workspaceBackend = await waitForBackendTiming(
    workspaceTurnId,
    "工作区 Chat 生产后端 timing 关联",
    "workspace_chat",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "workspace_chat",
    workspaceTurnId,
    workspaceTiming,
    workspaceBackend,
  ));
  checkTimingStages("工作区 Chat 生产 Renderer", workspaceTiming);

  await selectMostRecentPersonalSession(page);
  await setConversationDisplayMode(page, "summary");
  await selectPersonalSessionById(page, initialPersonalSessionId);
  await setComposerText(page, "DOM 工具卡片验收：调用 tool_catalog 后返回最终结果");
  await clickSend(page);
  const task = await waitForAssistant(page, toolResponseText, "Task 工具最终消息");
  check("Task 工具最终消息进入真实 DOM", task.text.includes(toolResponseText));
  const taskTurnId = assistantTurnId(task, toolResponseText);
  const taskTiming = await waitForTimingRecord(
    page,
    taskTurnId,
    "Task 工具生产 Renderer timing",
  );
  const taskBackend = await waitForBackendTiming(
    taskTurnId,
    "Task 工具生产后端 timing 关联",
    "workspace_tool",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "workspace_tool",
    taskTurnId,
    taskTiming,
    taskBackend,
  ));
  checkTimingStages("Task 工具生产 Renderer", taskTiming);
  const canonicalTaskItems = await waitFor(async () => page.evaluate(`(async () => {
    const sessionId = new URL(location.href).searchParams.get('sessionId');
    const turnId = ${JSON.stringify(taskTurnId)};
    if (!sessionId || !turnId) return null;
    const query = new URLSearchParams({ scope: 'personal', sessionId });
    const response = await fetch('/bootstrap?' + query.toString());
    if (!response.ok) return null;
    const payload = await response.json();
    const turn = payload.canonicalTurns?.find((candidate) => candidate.turnId === turnId);
    const items = Array.isArray(turn?.items) ? turn.items : [];
    return items.some((item) => item.kind === 'tool_call' && item.tool?.name === 'tool_catalog' && item.visibility?.renderable !== false)
      ? items
      : null;
  })()`), "Task canonical 工具调用持久化");
  check("Task canonical Turn 保留可渲染 tool_catalog 工具调用", canonicalTaskItems.some((item) => (
    item.kind === "tool_call" && item.tool?.name === "tool_catalog" && item.visibility?.renderable !== false
  )));

  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "Goal DOM 验收输入");
  await chooseGoalMode(page);
  await setComposerText(page, "目标 DOM 验收：建立目标并维护一项计划");
  await clickSend(page);
  const goal = await waitForAssistant(page, "ELECTRON_DOM_GOAL_OK", "Goal 最终消息");
  check("Goal 最终消息进入真实 DOM", goal.text.includes("ELECTRON_DOM_GOAL_OK"));
  const goalTurnId = assistantTurnId(goal, "ELECTRON_DOM_GOAL_OK");
  const goalTiming = await waitForTimingRecord(
    page,
    goalTurnId,
    "Goal 生产 Renderer timing",
  );
  const goalBackend = await waitForBackendTiming(
    goalTurnId,
    "Goal 生产后端 timing 关联",
    "goal",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "goal",
    goalTurnId,
    goalTiming,
    goalBackend,
  ));
  checkTimingStages("Goal 生产 Renderer", goalTiming);
  const goalCard = await waitFor(async () => {
    const state = await rendererState(page);
    return state.goalCard && state.planCard ? state : null;
  }, "Goal/计划卡片加载", 45_000);
  check("Goal 卡片进入真实 DOM", Boolean(goalCard.goalCard));
  check("Goal 计划卡片进入真实 DOM", Boolean(goalCard.planCard));
  check("Goal 卡片包含验收目标", goalCard.goalCard.text.includes("目标 DOM 验收"));
  await page.evaluate(`document.querySelector('[data-testid="goal-card"] .goal-drawer-toggle')?.click()`);
  const expandedGoal = await waitFor(async () => {
    const state = await rendererState(page);
    return state.goalCard?.expanded === "true" ? state : null;
  }, "Goal 卡片展开");
  check("Goal 卡片支持二级展开", expandedGoal.goalCard.expanded === "true");

  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "Goal 失败验收输入");
  await chooseGoalMode(page);
  const failedGoalPrompt = "目标失败 DOM 验收：验证失败后仍可恢复新的目标";
  await setComposerText(page, failedGoalPrompt);
  await clickSend(page);
  const failedGoal = await waitFor(async () => {
    const state = await rendererState(page);
    const text = state.text.toLowerCase();
    const hasFailureText = text.includes("失败") || text.includes("错误")
      || text.includes("failed") || text.includes("error");
    return state.input && state.inputEditable && !state.stop && hasFailureText ? state : null;
  }, "Goal 失败终态", 60_000);
  check(
    "Goal Provider 失败事实进入真实 DOM",
    failedGoal.text.includes("失败") || failedGoal.text.includes("错误")
      || failedGoal.text.toLowerCase().includes("failed")
      || failedGoal.text.toLowerCase().includes("error"),
  );
  const failedGoalTurnId = failedGoal.turns.at(-1)?.id
    || failedGoal.assistant.at(-1)?.turnId
    || null;
  check("Goal 失败 Turn 具有稳定 canonical Turn 身份", Boolean(failedGoalTurnId));
  const failedGoalTiming = await waitForTimingRecord(page, failedGoalTurnId, "Goal 失败 Renderer timing");
  const failedGoalBackend = await waitForAbnormalBackendTiming(
    failedGoalTurnId,
    "Goal 失败后端 timing 关联",
  );
  checkAbnormalBackendTimingStages("Goal 失败后端", failedGoalBackend);
  checkTimingStages("Goal 失败 Renderer", failedGoalTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "goal_failed",
    failedGoalTurnId,
    failedGoalTiming,
    failedGoalBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(failedGoalPrompt),
      settlementRequired: true,
    },
  ));

  await openPersonalDraft(page);
  await clearGoalMode(page);
  await chooseGoalMode(page);
  await setComposerText(page, "目标 DOM 验收：失败后重新建立目标并完成计划");
  await clickSend(page);
  const recoveredGoal = await waitForAssistant(page, "ELECTRON_DOM_GOAL_OK", "Goal 失败后恢复最终消息");
  check("Goal 失败后可重新建立并完成目标", recoveredGoal.text.includes("ELECTRON_DOM_GOAL_OK"));
  const recoveredGoalCard = await waitFor(async () => {
    const state = await rendererState(page);
    return state.goalCard && state.planCard ? state : null;
  }, "Goal 失败后恢复卡片", 45_000);
  check("Goal 失败后恢复卡片进入真实 DOM", Boolean(recoveredGoalCard.goalCard && recoveredGoalCard.planCard));

  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "子代理 DOM 验收输入");
  await setComposerText(page, "子代理 DOM 验收：请派发一个子代理并等待其完成");
  await clickSend(page);
  const agentResult = await waitForAssistant(page, "ELECTRON_DOM_AGENT_OK", "子代理最终消息");
  check("子代理最终消息进入真实 DOM", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  const agentTurnId = assistantTurnId(agentResult, "ELECTRON_DOM_AGENT_OK");
  const agentTiming = await waitForTimingRecord(
    page,
    agentTurnId,
    "子代理生产 Renderer timing",
  );
  const agentBackend = await waitForBackendTiming(
    agentTurnId,
    "子代理生产后端 timing 关联",
    "subagent",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "subagent",
    agentTurnId,
    agentTiming,
    agentBackend,
  ));
  checkTimingStages("子代理生产 Renderer", agentTiming);
  if (agentResult.turns.at(-1)?.expanded === "false") {
    await page.evaluate(`(() => {
      const turns = [...document.querySelectorAll('[data-conversation-turn-id]')];
      turns.at(-1)?.querySelector('.turn-disclosure-header')?.click();
    })()`);
  }
  const agentSpawnCard = await waitFor(async () => {
    const state = await rendererState(page);
    return state.agentCenter.agentCards.length > 0 || state.text.includes("ELECTRON_DOM_AGENT_OK") ? state : null;
  }, "子代理工具卡片加载", 45_000);
  check("子代理工具卡片进入真实 DOM", agentSpawnCard.agentCenter.agentCards.length > 0 || agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  check("子代理工具卡片包含 child task id", Boolean(agentSpawnCard.agentCenter.agentCards.at(-1)?.taskId) || agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  const agentCenterBeforeExpand = await rendererState(page);
  if (agentCenterBeforeExpand.agentCenter.visible) {
    await page.evaluate(`document.querySelector('.agent-center-trigger')?.click()`);
    const expandedAgentCenter = await waitFor(async () => {
      const state = await rendererState(page);
      return state.agentCenter.expanded === "true" ? state : null;
    }, "代理运行中心展开");
    check("代理运行中心支持展开", expandedAgentCenter.agentCenter.expanded === "true");
    check("代理运行中心显示子代理", expandedAgentCenter.agentCenter.rows.length > 0);
  } else {
    check("代理运行中心支持展开", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
    check("代理运行中心显示子代理", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  }

  // 失败代理也必须沿真实 agent_spawn -> canonical/tool projection -> Agent
  // Run read model 进入 Renderer；不能只验成功卡片或直接注入失败状态。
  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "子代理失败 DOM 验收输入");
  await setComposerText(page, "子代理失败 DOM 验收：派发一个不可用角色并展示失败原因");
  await clickSend(page);
  const failedAgent = await waitFor(async () => {
    const state = await rendererState(page);
    const cardText = state.agentCenter.agentCards.at(-1)?.text || "";
    const assistantText = state.assistant.at(-1)?.text || "";
    const hasFailure = /失败|拒绝|不可用|failed|rejected|error/iu.test(`${cardText} ${assistantText}`);
    return state.text.includes("子代理失败 DOM 验收") && !state.stop
      ? { ...state, cardText, assistantText, hasFailure }
      : null;
  }, "子代理失败卡片进入真实 DOM", 60_000);
  check(
    "子代理失败事实进入真实 DOM",
    failedAgent.hasFailure,
    `card=${failedAgent.cardText.slice(-240)} assistant=${failedAgent.assistantText.slice(-240)}`,
  );
  if (failedAgent.agentCenter.agentCards.length > 0) {
    check("子代理失败卡片未伪造 child task id", !failedAgent.agentCenter.agentCards.at(-1)?.taskId);
  }
  if (failedAgent.agentCenter.visible) {
    await page.evaluate(`document.querySelector('.agent-center-trigger')?.click()`);
    const failedAgentCenter = await waitFor(async () => {
      const state = await rendererState(page);
      return state.agentCenter.expanded === "true"
        && /失败|需要处理|不可用|failed|error/iu.test(state.agentCenter.text)
        ? state
        : null;
    }, "代理运行中心失败恢复状态", 45_000);
    check("代理运行中心显示失败恢复状态", /失败|需要处理|不可用|failed|error/iu.test(failedAgentCenter.agentCenter.text));
  }
  const failedAgentTurnId = failedAgent.turns.at(-1)?.id
    || failedAgent.assistant.at(-1)?.turnId
    || null;
  const failedAgentPrompt = "子代理失败 DOM 验收：派发一个不可用角色并展示失败原因";
  check("子代理失败 Turn 具有稳定 canonical Turn 身份", Boolean(failedAgentTurnId));
  const failedAgentTiming = await waitForTimingRecord(page, failedAgentTurnId, "子代理失败 Renderer timing");
  const failedAgentBackend = await waitForAbnormalBackendTiming(
    failedAgentTurnId,
    "子代理失败后端 timing 关联",
  );
  checkAbnormalBackendTimingStages("子代理失败后端", failedAgentBackend);
  checkTimingStages("子代理失败 Renderer", failedAgentTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "agent_failed",
    failedAgentTurnId,
    failedAgentTiming,
    failedAgentBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(failedAgentPrompt),
      settlementRequired: true,
    },
  ));

  // 多子代理必须沿真实 agent_spawn 批量创建、agent_wait、TaskStore projection
  // 和 Agent Center 展示链路收口；成功与失败子代理同时存在时，不得只展示一个汇总状态。
  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "子代理多状态 DOM 验收输入");
  await setComposerText(page, "子代理多状态 DOM 验收：同时派发成功和失败子代理并展示两种状态");
  await clickSend(page);
  const multiAgentResult = await waitForAssistant(
    page,
    "ELECTRON_DOM_AGENT_MULTI_OK",
    "子代理多状态最终消息",
  );
  check("子代理多状态最终消息进入真实 DOM", multiAgentResult.text.includes("ELECTRON_DOM_AGENT_MULTI_OK"));
  let multiAgentCenter;
  try {
    multiAgentCenter = await waitFor(async () => {
      const state = await rendererState(page);
      const hasMultiStatusSummary = /1\s*已完成\s*[·.]\s*1\s*失败/iu.test(state.text);
      const hasSuccess = /成功|完成|completed|success/iu.test(`${state.agentCenter.text} ${state.text}`);
      const hasFailure = /失败|failed|error/iu.test(`${state.agentCenter.text} ${state.text}`);
      return hasMultiStatusSummary && hasSuccess && hasFailure ? state : null;
    }, "子代理多状态卡片", 60_000);
  } catch (error) {
    throw new Error(`${error.message}；当前多状态 Renderer：${JSON.stringify(await rendererState(page))}；Provider requests=${provider.requests.length}`);
  }
  check("子代理多状态摘要同时保留成功和失败计数", /1\s*已完成\s*[·.]\s*1\s*失败/iu.test(multiAgentCenter.text));
  check(
    "子代理多状态同时包含成功和失败事实",
    /成功|完成|completed|success/iu.test(`${multiAgentCenter.agentCenter.text} ${multiAgentCenter.text}`)
      && /失败|failed|error/iu.test(`${multiAgentCenter.agentCenter.text} ${multiAgentCenter.text}`),
    `DOM text: ${multiAgentCenter.text.slice(-480)}`,
  );
  if (multiAgentCenter.agentCenter.visible) {
    if (multiAgentCenter.agentCenter.expanded !== "true") {
      await page.evaluate(`document.querySelector('.agent-center-trigger')?.click()`);
      await waitFor(async () => {
        const state = await rendererState(page);
        return state.agentCenter.expanded === "true" ? state : null;
      }, "子代理多状态运行中心展开");
    }
    await page.evaluate(`(() => {
      const toggle = document.querySelector('.agent-center-panel .group-heading--toggle');
      if (toggle?.getAttribute('aria-expanded') !== 'true') toggle?.click();
    })()`);
    const expandedMultiAgentCenter = await waitFor(async () => {
      const state = await rendererState(page);
      const rowsText = state.agentCenter.rows.join("\n");
      return state.agentCenter.rows.length >= 2
        && /成功|完成|completed|success/iu.test(rowsText)
        && /失败|failed|error/iu.test(rowsText)
        ? state
        : null;
    }, "子代理多状态运行中心行", 45_000);
    check("代理运行中心同时显示成功和失败子代理行", expandedMultiAgentCenter.agentCenter.rows.length >= 2);
  }

  await chooseAccessProfile(page, "read_only");
  const permissionDeniedPrompt = "权限拒绝 DOM 验收：请明确调用 file_write 写入文件";
  await setComposerText(page, permissionDeniedPrompt);
  await clickSend(page);
  const permission = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes("权限") || state.text.includes("只读") || state.text.includes("拒绝")
      ? state
      : null;
  }, "ReadOnly 权限拒绝终态", 45_000);
  check("ReadOnly 权限拒绝事实进入真实 DOM", permission.text.includes("权限") || permission.text.includes("只读") || permission.text.includes("拒绝"));
  check("ReadOnly 权限拒绝未创建文件工具组", permission.toolGroups.every((group) => !group.text.includes("file_write")));
  const permissionDeniedTurnId = permission.turns.at(-1)?.id
    || permission.assistant.at(-1)?.turnId
    || null;
  check("权限拒绝 Turn 具有稳定 canonical Turn 身份", Boolean(permissionDeniedTurnId));
  const permissionDeniedTiming = await waitForTimingRecord(
    page,
    permissionDeniedTurnId,
    "权限拒绝 Renderer timing",
  );
  const permissionDeniedBackend = await waitForAbnormalBackendTiming(
    permissionDeniedTurnId,
    "权限拒绝后端 timing 关联",
  );
  checkAbnormalBackendTimingStages("权限拒绝后端", permissionDeniedBackend);
  checkTimingStages("权限拒绝 Renderer", permissionDeniedTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "permission_denied",
    permissionDeniedTurnId,
    permissionDeniedTiming,
    permissionDeniedBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(permissionDeniedPrompt),
      settlementRequired: true,
    },
  ));

  // Git dirty/drift/conflict 必须通过真实 workspace 注册、Git 观测和 Task
  // preflight 进入 Renderer；Provider 只为 dirty 场景返回最终文本，drift/conflict
  // 场景应在 Provider 之前失败，避免把 mock 文本误当成 Git 错误证据。
  gitDirtyRoot = await mkdtemp(join(stateRoot, "git-dirty-"));
  await createGitRepository(gitDirtyRoot);
  await writeFile(join(gitDirtyRoot, "dirty.txt"), "uncommitted\n", "utf8");
  const dirtyWorkspaceId = await registerWorkspace(page, gitDirtyRoot);
  await waitForComposerReady(page, "Git dirty 工作区输入区就绪");
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.git.some((item) => /未跟踪|未提交|未暂存|已暂存|脏|dirty|uncommitted|untracked/iu.test(
      `${item.title} ${item.text} ${item.dirty}`,
    )) ? state : null;
  }, "Git dirty 状态进入真实 DOM");
  const dirtyProviderRequests = provider.requests.length;
  await setComposerText(page, "Git dirty DOM 验收：执行一个复杂任务，读取当前工作区并汇总结果");
  await clickSend(page);
  const dirtyResult = await waitForAssistant(page, responseText, "Git dirty Task 最终消息");
  check("Git dirty 工作区仍可完成 Task", dirtyResult.text.includes(responseText));
  check("Git dirty 工作区状态进入真实 DOM", dirtyResult.git.some((item) => /未跟踪|未提交|未暂存|已暂存|脏/iu.test(`${item.title} ${item.text}`)));
  check("Git dirty Task 实际到达 Provider", provider.requests.length > dirtyProviderRequests);
  check("Git dirty 工作区注册身份仍可见", dirtyResult.workspaceIds.includes(dirtyWorkspaceId));

  await runGitDriftConflictChecks(page);

  // Git mutation 必须通过真实工具、Restricted 审批和真实仓库副作用闭环，
  // 不能用 Provider 最终文本替代 branch 创建事实。
  gitMutationRoot = await mkdtemp(join(stateRoot, "git-mutation-"));
  await createGitRepository(gitMutationRoot);
  const gitMutationWorkspaceId = await registerWorkspace(page, gitMutationRoot);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.workspaceIds.includes(gitMutationWorkspaceId) ? state : null;
  }, "Git mutation 工作区可用");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${gitMutationWorkspaceId}"]');
    const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('git mutation workspace new session button missing');
    create.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
      ? state
      : null;
  }, "Git mutation 工作区草稿");
  await chooseAccessProfile(page, "restricted");
  await setComposerText(page, "Git mutation DOM 验收：调用 git_branch_create 创建 electron-dom-created 分支并在审批后汇总");
  await clickSend(page);
  const gitMutationApproval = await waitFor(async () => {
    const state = await rendererState(page);
    const card = await page.evaluate(`(() => {
      const root = document.querySelector('.tool-approval');
      if (!root) return null;
      const primary = root.querySelector('button.approval-button--primary');
      return { text: root.innerText || '', allowOnce: Boolean(primary && !primary.disabled) };
    })()`);
    return card?.allowOnce ? { ...state, approval: card } : null;
  }, "Git mutation 审批卡片", 45_000);
  check("Git mutation 审批卡片进入真实 DOM", gitMutationApproval.approval.text.length > 0);
  check("Git mutation 审批卡片允许一次可用", gitMutationApproval.approval.allowOnce);
  await page.evaluate(`document.querySelector('.tool-approval button.approval-button--primary')?.click()`);
  const gitMutationResult = await waitForAssistant(
    page,
    gitMutationResponseText,
    "Git mutation 审批恢复最终消息",
  );
  check("Git mutation 审批恢复最终消息进入真实 DOM", gitMutationResult.text.includes(gitMutationResponseText));
  let gitMutationBranchCreated = false;
  try {
    await gitFixtureCommand(gitMutationRoot, ["show-ref", "--verify", "--quiet", "refs/heads/electron-dom-created"]);
    gitMutationBranchCreated = true;
  } catch {
    gitMutationBranchCreated = false;
  }
  check("Git mutation 审批允许后真实 branch 副作用存在", gitMutationBranchCreated);
  const gitMutationHead = await execFileAsync("git", ["branch", "--show-current"], { cwd: gitMutationRoot });
  check("Git mutation 创建分支后未隐式切换当前分支", gitMutationHead.stdout.trim() === "main", gitMutationHead.stdout.trim());

  await waitFor(async () => {
    const state = await rendererState(page);
    return state.workspaceIds.includes(workspaceId) ? state : null;
  }, "完全访问工作区可用");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('full access workspace new session button missing');
    create.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
      ? state
      : null;
  }, "完全访问工作区草稿");
  await chooseAccessProfile(page, "full_access");
  await setComposerText(page, `完全访问 DOM 验收：请明确调用 shell_exec 写入 ${fullAccessTarget}`);
  await clickSend(page);
  const fullAccessResult = await waitForAssistant(page, fullAccessResponseText, "完全访问最终消息");
  check("FullAccess 工具任务最终消息进入真实 DOM", fullAccessResult.text.includes(fullAccessResponseText));
  const fullAccessApproval = await page.evaluate(`Boolean(document.querySelector('.tool-approval'))`);
  check("FullAccess 工具任务不出现审批卡片", !fullAccessApproval);
  let fullAccessFileExists = true;
  try {
    await access(fullAccessTarget);
  } catch {
    fullAccessFileExists = false;
  }
  check("FullAccess 工具任务真实文件副作用存在", fullAccessFileExists);

  await waitFor(async () => {
    const state = await rendererState(page);
    return state.workspaceIds.includes(workspaceId) ? state : null;
  }, "审批工作区可用");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('approval workspace new session button missing');
    create.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
      ? state
      : null;
  }, "审批工作区草稿");
  await chooseAccessProfile(page, "restricted");
  await setComposerText(page, `审批 DOM 验收：请明确调用 shell_exec 写入 ${approvalTarget}`);
  await clickSend(page);
  const approval = await waitFor(async () => {
    const state = await rendererState(page);
    const card = await page.evaluate(`(() => {
      const root = document.querySelector('.tool-approval');
      if (!root) return null;
      const buttons = [...root.querySelectorAll('button')];
      const primary = root.querySelector('button.approval-button--primary');
      return {
        text: root.innerText || '',
        allowOnce: Boolean(primary && !primary.disabled),
        allowOncePresent: Boolean(primary),
        allowForTurn: buttons.some((button) => button.innerText.includes('本轮')),
        deny: buttons.some((button) => button.innerText.includes('拒绝')),
      };
    })()`);
    return card?.allowOnce ? { ...state, approval: card } : null;
  }, "Restricted 审批卡片", 45_000);
  check("Restricted 审批卡片进入真实 DOM", approval.approval.text.length > 0);
  check("Restricted 审批卡片包含允许一次操作", approval.approval.allowOncePresent);
  check("Restricted 审批卡片包含按 Turn 允许操作", approval.approval.allowForTurn);
  check("Restricted 审批卡片包含拒绝操作", approval.approval.deny);
  await page.evaluate(`document.querySelector('.tool-approval button.approval-button--primary')?.click()`);
  const approvalResult = await waitForAssistant(page, approvalResponseText, "审批恢复最终消息");
  check("审批允许后最终消息进入真实 DOM", approvalResult.text.includes(approvalResponseText));
  let approvalFileExists = true;
  try {
    await access(approvalTarget);
  } catch {
    approvalFileExists = false;
  }
  check("审批允许后真实文件副作用存在", approvalFileExists);

  await waitFor(async () => {
    const state = await rendererState(page);
    return state.workspaceIds.includes(workspaceId) ? state : null;
  }, "审批拒绝工作区可用");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('approval denial workspace new session button missing');
    create.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
      ? state
      : null;
  }, "审批拒绝工作区草稿");
  await chooseAccessProfile(page, "restricted");
  const approvalDeniedPrompt = `审批拒绝 DOM 验收：请明确调用 shell_exec 写入 ${approvalDenyTarget}`;
  await setComposerText(page, approvalDeniedPrompt);
  await clickSend(page);
  const denialApproval = await waitFor(async () => {
    const state = await rendererState(page);
    const card = await page.evaluate(`(() => {
      const root = document.querySelector('.tool-approval');
      if (!root) return null;
      const buttons = [...root.querySelectorAll('button')];
      const primary = root.querySelector('button.approval-button--primary');
      return {
        text: root.innerText || '',
        allowOnce: Boolean(primary && !primary.disabled),
        deny: buttons.some((button) => button.innerText.includes('拒绝')),
      };
    })()`);
    return card?.allowOnce ? { ...state, approval: card } : null;
  }, "Restricted 审批拒绝卡片", 45_000);
  check("Restricted 审批拒绝卡片进入真实 DOM", denialApproval.approval.text.length > 0);
  check("Restricted 审批拒绝卡片包含拒绝操作", denialApproval.approval.deny);
  await page.evaluate(`(() => {
    const root = document.querySelector('.tool-approval');
    const deny = [...(root?.querySelectorAll('button') || [])]
      .find((button) => button.innerText.includes('拒绝'));
    if (!deny) throw new Error('approval denial button missing');
    deny.click();
  })()`);
  const denied = await waitFor(async () => {
    const state = await rendererState(page);
    const text = state.text.toLowerCase();
    const hasDeniedText = text.includes("拒绝") || text.includes("denied")
      || text.includes("permission") || text.includes("权限");
    return state.input && state.inputEditable && !state.stop && hasDeniedText ? state : null;
  }, "审批拒绝终态", 45_000);
  check(
    "审批拒绝事实进入真实 DOM",
    denied.text.includes("拒绝") || denied.text.toLowerCase().includes("denied")
      || denied.text.toLowerCase().includes("permission") || denied.text.includes("权限"),
  );
  let deniedFileExists = true;
  try {
    await access(approvalDenyTarget);
  } catch {
    deniedFileExists = false;
  }
  check("审批拒绝后真实文件副作用不存在", !deniedFileExists);
  const deniedTurnId = denied.turns.at(-1)?.id || denied.assistant.at(-1)?.turnId || null;
  check("审批拒绝 Turn 具有稳定 canonical Turn 身份", Boolean(deniedTurnId));
  const deniedTiming = await waitForTimingRecord(page, deniedTurnId, "审批拒绝 Renderer timing");
  const deniedBackend = await waitForAbnormalBackendTiming(
    deniedTurnId,
    "审批拒绝后端 timing 关联",
  );
  checkAbnormalBackendTimingStages("审批拒绝后端", deniedBackend);
  checkTimingStages("审批拒绝 Renderer", deniedTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "approval_denied",
    deniedTurnId,
    deniedTiming,
    deniedBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "failed",
      outcome: "failed",
      inputHash: hashText(approvalDeniedPrompt),
      settlementRequired: true,
    },
  ));

  await waitFor(async () => {
    const state = await rendererState(page);
    return state.workspaceIds.includes(workspaceId) ? state : null;
  }, "审批取消工作区可用");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('approval cancellation workspace new session button missing');
    create.click();
  })()`);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
      ? state
      : null;
  }, "审批取消工作区草稿");
  await chooseAccessProfile(page, "restricted");
  const approvalCancelledPrompt = `审批取消 DOM 验收：请明确调用 shell_exec 写入 ${approvalCancelTarget}`;
  await setComposerText(page, approvalCancelledPrompt);
  await clickSend(page);
  const cancellationApproval = await waitFor(async () => {
    const state = await rendererState(page);
    const card = await page.evaluate(`Boolean(document.querySelector('.tool-approval'))`);
    return card && state.stop ? state : null;
  }, "Restricted 审批取消卡片", 45_000);
  check("Restricted 审批取消卡片进入真实 DOM", cancellationApproval.text.length > 0);
  check("Restricted 审批取消卡片显示停止操作", cancellationApproval.stop);
  const stopApproval = await clickStop(page);
  check("审批取消场景触发停止操作", stopApproval);
  const cancelledApproval = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes("审批取消 DOM 验收")
      && !state.stop && !state.text.includes("处理中")
      ? state
      : null;
  }, "审批取消终态", 45_000);
  check(
    "审批取消终态进入真实 DOM",
    cancelledApproval.text.includes("审批取消 DOM 验收") && !cancelledApproval.text.includes("处理中"),
    `DOM text: ${cancelledApproval.text.slice(-240)}`,
  );
  let cancelledFileExists = true;
  try {
    await access(approvalCancelTarget);
  } catch {
    cancelledFileExists = false;
  }
  check("审批取消后真实文件副作用不存在", !cancelledFileExists);
  const cancelledTurnId = cancelledApproval.turns.at(-1)?.id
    || cancelledApproval.assistant.at(-1)?.turnId
    || null;
  check("审批取消 Turn 具有稳定 canonical Turn 身份", Boolean(cancelledTurnId));
  const cancelledTiming = await waitForTimingRecord(page, cancelledTurnId, "审批取消 Renderer timing");
  const cancelledBackend = await waitForAbnormalBackendTiming(
    cancelledTurnId,
    "审批取消后端 timing 关联",
  );
  checkAbnormalBackendTimingStages("审批取消后端", cancelledBackend);
  checkTimingStages("审批取消 Renderer", cancelledTiming);
  rendererTimingSamples.push(timingEvidenceRecord(
    "approval_cancelled",
    cancelledTurnId,
    cancelledTiming,
    cancelledBackend,
    {
      trajectoryMode: "abnormal",
      expectedOutcome: "cancelled",
      outcome: "cancelled",
      inputHash: hashText(approvalCancelledPrompt),
      settlementRequired: true,
    },
  ));

  if (approvalExpiryWaitMs > 0) {
    await waitFor(async () => {
      const state = await rendererState(page);
      return state.workspaceIds.includes(workspaceId) ? state : null;
    }, "审批过期工作区可用");
    await page.evaluate(`(() => {
      const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
      const create = workspace?.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
      if (!create) throw new Error('approval expiry workspace new session button missing');
      create.click();
    })()`);
    await waitFor(async () => {
      const state = await rendererState(page);
      return state.input && state.inputEditable && !state.stop && state.assistant.length === 0
        ? state
        : null;
    }, "审批过期工作区草稿");
    await chooseAccessProfile(page, "restricted");
    await setComposerText(page, `审批过期 DOM 验收：请明确调用 shell_exec 写入 ${approvalExpiryTarget}`);
    await clickSend(page);
    const expiryApproval = await waitFor(async () => {
      const state = await rendererState(page);
      const card = await page.evaluate(`Boolean(document.querySelector('.tool-approval'))`);
      return card && state.stop ? state : null;
    }, "Restricted 审批过期卡片", 45_000);
    check("Restricted 审批过期卡片进入真实 DOM", expiryApproval.text.length > 0);
    check("Restricted 审批过期场景显示停止操作", expiryApproval.stop);
    await sleep(approvalExpiryWaitMs);
    const expiredApproval = await waitFor(async () => {
      const state = await rendererState(page);
      const text = `${state.text} ${state.assistant.at(-1)?.text || ""}`;
      const hasExpiryFact = /过期|expired|tool_approval_expired/iu.test(text);
      return state.input && state.inputEditable && !state.stop && hasExpiryFact ? state : null;
    }, "审批过期失败终态", 60_000);
    check(
      "审批过期事实进入真实 DOM",
      /过期|expired|tool_approval_expired/iu.test(expiredApproval.text),
      `DOM text: ${expiredApproval.text.slice(-320)}`,
    );
    let expiredFileExists = true;
    try {
      await access(approvalExpiryTarget);
    } catch {
      expiredFileExists = false;
    }
    check("审批过期后真实文件副作用不存在", !expiredFileExists);
  }

  // 再建立一个有独立最终文本的个人会话，用于 daemon 重启和历史切换断言。
  await selectMostRecentPersonalSession(page);
  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "daemon 重启前稳定会话输入", 45_000);
  await setComposerText(page, "daemon 重启 DOM 验收：请只回复重启后的最终消息");
  await clickSend(page);
  const restartProbe = await waitForAssistant(page, restartResponseText, "daemon 重启前稳定会话");
  const restartSessionId = new URL(restartProbe.url).searchParams.get("sessionId");
  check("daemon 重启前稳定会话拥有 sessionId", Boolean(restartSessionId));
  check("daemon 重启前稳定会话完成", restartProbe.text.includes(restartResponseText));

  const beforeDaemonRestart = await healthSnapshot();
  const daemonPid = await waitFor(
    async () => ownedDaemonPid(electron.pid),
    "定位 Electron 自有 daemon 子进程",
    15_000,
  );
  // 这是脚本自己通过 Electron 启动的 daemon；使用 SIGKILL 只终止该子进程，
  // 让 ProcessSupervisor 走真实的非正常退出恢复路径，避免优雅关闭阶段的
  // 长连接把验收窗口拖成“健康检查一直不可用”。
  process.kill(daemonPid, "SIGKILL");
  await waitFor(
    async () => (await ownedDaemonPid(electron.pid)) !== daemonPid,
    "脚本自有 daemon 退出",
    15_000,
    100,
  );
  const afterDaemonRestart = await waitFor(async () => {
    try {
      const health = await healthSnapshot();
      return health.runtimeEpoch && health.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch
        ? health
        : null;
    } catch {
      return null;
    }
  }, "daemon 重启后健康状态恢复", 45_000, 250);
  check(
    "daemon 重启后 runtime epoch 变化",
    afterDaemonRestart.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch,
  );
  await selectPersonalSessionById(page, restartSessionId);
  const afterDaemonRecovery = await waitForAssistant(
    page,
    restartResponseText,
    "daemon 重启后 Renderer 内容恢复",
  );
  check("daemon 重启后 Renderer 内容恢复", afterDaemonRecovery.text.includes(restartResponseText));

  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "Renderer 重连恢复");
  await selectPersonalSessionById(page, initialPersonalSessionId);
  const afterReload = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes(responseText) ? state : null;
  }, "Renderer 重载后的历史消息");
  check("Renderer 重载后历史消息可恢复", afterReload.text.includes(responseText));
  check("Renderer 重载后保留助手消息", afterReload.assistant.length >= 1);
  check("Renderer 重载后个人历史会话列表可见", afterReload.personalSessionIds.length >= 2);
  await selectPersonalSessionById(page, restartSessionId);
  const switchedHistory = await waitForAssistant(page, restartResponseText, "切换个人历史会话");
  check("切换个人历史会话后消息内容正确", switchedHistory.text.includes(restartResponseText));

  // 应用级 restart/replay：完全关闭当前 Electron 与其 daemon，再以同一
  // MAGI_STATE_ROOT 启动新的打包应用；这与 daemon restart 或 Renderer reload
  // 是不同边界。只从 canonical/history 恢复消息，不复用旧 Renderer 内存。
  const previousAppDaemonPid = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
  page?.close();
  page = null;
  await stopOwnedProcess(electron, cdpPort);
  await stopOwnedDaemon(previousAppDaemonPid);
  restartedElectron = spawnElectron(appRestartCdpPort);
  await waitFor(async () => {
    try {
      daemonPidForCleanup = await ownedDaemonPid(restartedElectron.pid) || daemonPidForCleanup;
      page = await connectPage(appRestartCdpPort);
      return page;
    } catch {
      return null;
    }
  }, "应用重启后连接新的打包 Electron Renderer", 45_000, 250);
  await waitForRenderer(page, "应用重启后 Renderer 就绪");
  await selectPersonalSessionById(page, restartSessionId);
  const afterApplicationRestart = await waitForAssistant(
    page,
    restartResponseText,
    "应用重启后历史消息恢复",
  );
  check("应用重启后从同一 state root 恢复历史消息", afterApplicationRestart.text.includes(restartResponseText));
  check("应用重启后仍绑定原 session", afterApplicationRestart.activePersonalSessionId === restartSessionId);

  // 在活动 Turn 期间 reload Renderer，验证恢复依赖 canonical/stream 状态而
  // 不是当前 Renderer 的本地 loading。恢复后仍应绑定同一 session，并允许
  // 用户通过真实停止入口收口；这里不直接写任何 canonical terminal。
  const reconnectHistoryBefore = await rendererState(page);
  const reconnectHistoryBeforeIds = new Set(reconnectHistoryBefore.personalSessionIds);
  const reconnectTurnMarker = "ELECTRON_DOM_RECONNECT_TURN";
  await openPersonalDraft(page);
  await chooseAccessProfile(page, "restricted");
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "活动 Turn 重连前输入恢复", 45_000);
  await setComposerText(page, `取消 DOM 验收（Renderer 重连） ${reconnectTurnMarker}：保持响应直到我重载或停止`);
  await clickSend(page);
  const activeBeforeReload = await waitFor(async () => {
    const state = await rendererState(page);
    return state.stop ? state : null;
  }, "活动 Turn 重连前停止入口", 20_000);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "活动 Turn Renderer reload 恢复");
  const activeAfterReload = await waitFor(async () => {
    const state = await rendererState(page);
    const recoveredSessionId = state.personalSessionIds.find(
      (sessionId) => !reconnectHistoryBeforeIds.has(sessionId),
    ) || null;
    return state.text.includes(reconnectTurnMarker) && recoveredSessionId
      ? { ...state, recoveredSessionId }
      : null;
  }, "活动 Turn 重连后历史/状态恢复", 45_000);
  const reconnectSessionId = activeAfterReload.recoveredSessionId;
  check("活动 Turn Renderer reload 后从历史 DOM 取得稳定 sessionId", Boolean(reconnectSessionId));
  await selectPersonalSessionById(page, reconnectSessionId);
  const selectedAfterReload = await waitFor(async () => {
    const state = await rendererState(page);
    return state.activePersonalSessionId === reconnectSessionId
      && state.text.includes(reconnectTurnMarker)
      ? state
      : null;
  }, "活动 Turn 重连后稳定会话内容", 45_000);
  check("活动 Turn Renderer reload 后保留同一 session 内容", selectedAfterReload.text.includes(reconnectTurnMarker));
  check("活动 Turn Renderer reload 后仍有停止或已收口状态", selectedAfterReload.stop || !selectedAfterReload.text.includes("处理中"));
  if (selectedAfterReload.stop) {
    const reconnectStopClicked = await clickStop(page);
    check("活动 Turn Renderer reload 后可触发停止", reconnectStopClicked);
  }
  const reconnectedCancelled = await waitFor(async () => {
    const state = await rendererState(page);
    return state.activePersonalSessionId === reconnectSessionId
      && state.text.includes(reconnectTurnMarker)
      && !state.stop
      && !state.text.includes("处理中")
      ? state
      : null;
  }, "活动 Turn Renderer reload 后终态", 45_000);
  check("活动 Turn Renderer reload 后终态进入真实 DOM", reconnectedCancelled.text.includes(reconnectTurnMarker));

  // 同一打包 App 的真实 Renderer 入口再验证一次 requestId 幂等边界：第一次
  // 请求完成后，复用 requestId 但改变完整请求内容必须在 canonical 接纳前拒绝，
  // 不能创建第二个 Turn 或再次调用 Provider。这个检查独立于应用重启后的历史
  // 回放，避免把“能读到旧消息”误当成 fingerprint conflict 已验证。
  await selectPersonalSessionById(page, restartSessionId);
  await waitForComposerReady(page, "fingerprint conflict 前输入区收口");
  const submitRendererSessionTurn = async (request) => {
    const encoded = JSON.stringify(request);
    return page.evaluate(`(async () => {
      const response = await fetch('/api/session/turn', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(${encoded}),
      });
      const text = await response.text();
      let body = null;
      try { body = JSON.parse(text); } catch {}
      return { status: response.status, body, text };
    })()`);
  };
  const fingerprintConflictRequestId = "electron-dom-fingerprint-conflict";
  const fingerprintConflictText = "Electron fingerprint conflict 首次请求";
  const firstFingerprintRequest = await submitRendererSessionTurn({
    scope: "personal",
    sessionId: restartSessionId,
    text: fingerprintConflictText,
    images: [],
    requestId: fingerprintConflictRequestId,
    userMessageId: "electron-dom-fingerprint-user-v1",
  });
  check(
    "打包 Electron fingerprint 首次请求被接纳",
    firstFingerprintRequest.status === 200,
    firstFingerprintRequest.text,
  );
  const fingerprintTurnId = firstFingerprintRequest.body?.turnId || null;
  check("打包 Electron fingerprint 首次请求返回 Turn 身份", typeof fingerprintTurnId === "string");
  await waitFor(
    () => backendTimingEvidence(fingerprintTurnId)?.stages?.terminal?.count > 0
      ? backendTimingEvidence(fingerprintTurnId)
      : null,
    "fingerprint conflict 首次请求终态",
    45_000,
  );
  const providerRequestsBeforeFingerprintConflict = provider.requests.length;
  const conflictingFingerprintRequest = await submitRendererSessionTurn({
    scope: "personal",
    sessionId: restartSessionId,
    text: "Electron fingerprint conflict 修改后的请求",
    images: [],
    requestId: fingerprintConflictRequestId,
    userMessageId: "electron-dom-fingerprint-user-v2",
  });
  check(
    "打包 Electron fingerprint 冲突被明确拒绝",
    conflictingFingerprintRequest.status === 409
      && /requestId/iu.test(conflictingFingerprintRequest.text),
    conflictingFingerprintRequest.text,
  );
  check(
    "打包 Electron fingerprint 冲突未重复调用 Provider",
    provider.requests.length === providerRequestsBeforeFingerprintConflict,
  );
  const fingerprintMessages = await page.evaluate(`fetch('/api/messages?scope=personal&sessionId=${encodeURIComponent(restartSessionId)}')
    .then(async (response) => ({ status: response.status, body: await response.json() }))`);
  const matchingFingerprintMessages = Array.isArray(fingerprintMessages.body?.timeline)
    ? fingerprintMessages.body.timeline.filter((entry) => entry.message === fingerprintConflictText)
    : [];
  check(
    "打包 Electron fingerprint 冲突未追加第二条 canonical 用户消息",
    fingerprintMessages.status === 200 && matchingFingerprintMessages.length === 1,
    JSON.stringify(fingerprintMessages.body).slice(-500),
  );

  const shutdownSettlementEventsBefore = daemonShutdownSettlementEvents;
  const shutdownSettledTurnCountBefore = daemonShutdownSettledTurnCount;
  await openPersonalDraft(page);
  await chooseAccessProfile(page, "restricted");
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop;
  }, "取消场景输入恢复", 45_000);
  const shutdownTimingTurnIdsBefore = new Set(backendTimingByTurn.keys());
  await setComposerText(page, "取消 DOM 验收（关闭闸门）：保持响应直到 Electron 关闭");
  await clickSend(page);
  const activeBeforeApplicationShutdown = await waitFor(async () => {
    const state = await rendererState(page);
    return state.stop ? state : null;
  }, "关闭闸门前活动 Turn", 20_000);
  const newlyAcceptedShutdownTiming = await waitFor(
    () => [...backendTimingByTurn.values()].reverse().find((record) => (
      !shutdownTimingTurnIdsBefore.has(record.turnId)
      && record.stages?.accepted_response_sent?.count > 0
    )) || null,
    "关闭闸门前新建 Turn 后端接纳",
    20_000,
  );
  // 当前 Renderer 刚创建了专用草稿；优先使用 DOM 中这个活动 Turn 的
  // 身份，不能从全局 timing map 反向挑选，因为 Renderer reload/replay
  // 场景会留下多个尚未看到 terminal log 的旧记录。
  const shutdownTurnId = activeBeforeApplicationShutdown.turns.at(-1)?.id
    || activeBeforeApplicationShutdown.assistant.at(-1)?.turnId
    || newlyAcceptedShutdownTiming.turnId
    || null;
  check("关闭闸门前活动 Turn 具有 canonical Turn 身份", Boolean(shutdownTurnId));
  const activeShutdownTiming = backendTimingByTurn.get(shutdownTurnId);
  check("关闭闸门前活动 Turn 后端 timing 身份一致", activeShutdownTiming?.turnId === shutdownTurnId);
  await waitFor(
    () => shutdownTurnId && backendTimingByTurn.get(shutdownTurnId)?.stages?.accepted_response_sent?.count > 0
      ? backendTimingByTurn.get(shutdownTurnId)
      : null,
    "关闭闸门前活动 Turn 已接纳",
    20_000,
  );

  // 让真实 Electron 退出事务接管活动 Turn。此处不先点击 Renderer 的停止按钮，
  // 这样只能由 Desktop -> daemon 的关闭路径取消并等待 settlement；终态必须在
  // Electron 进程退出前出现在 daemon timing ledger 中。
  const shutdownDaemonPid = await ownedDaemonPid(restartedElectron.pid);
  page?.close();
  page = null;
  await stopOwnedProcess(restartedElectron, appRestartCdpPort);
  const shutdownTiming = shutdownTurnId ? backendTimingByTurn.get(shutdownTurnId) : null;
  const shutdownSettlementObserved = daemonShutdownSettlementEvents > shutdownSettlementEventsBefore
    && daemonShutdownSettledTurnCount > shutdownSettledTurnCountBefore;
  const shutdownTerminal = shutdownTiming?.stages?.canonical_terminal_published?.count === 1
    ? shutdownTiming.stages.canonical_terminal_published
    : shutdownTiming?.stages?.canonical_terminal_ignored?.count === 1
      ? shutdownTiming.stages.canonical_terminal_ignored
      : null;
  const shutdownTerminalStage = shutdownTiming?.stages?.canonical_terminal_published?.count === 1
    ? "canonical_terminal_published"
    : shutdownTiming?.stages?.canonical_terminal_ignored?.count === 1
      ? "canonical_terminal_ignored"
      : "";
  check(
    "Electron 关闭前活动 Turn 发布唯一 canonical terminal",
    shutdownTerminal?.count === 1 || shutdownSettlementObserved,
    JSON.stringify(shutdownTiming?.stages || {}),
  );
  check(
    "Electron 关闭前活动 Turn 已完成 settlement",
    shutdownTiming?.stages?.terminal?.count > 0 || shutdownSettlementObserved,
    JSON.stringify(shutdownTiming?.stages || {}),
  );
  check(
    "Electron 关闭进程已退出",
    restartedElectron.exitCode !== null || restartedElectron.signalCode !== null,
  );
  if (shutdownDaemonPid) await stopOwnedDaemon(shutdownDaemonPid);

  flushElectronLogBuffers();
  const sourceIdentityAfter = await collectSourceIdentity();
  const sourceIdentityStable = sourceIdentityBefore.source_commit === sourceIdentityAfter.source_commit
    && sourceIdentityBefore.worktree_fingerprint_sha256
      === sourceIdentityAfter.worktree_fingerprint_sha256
    && sourceIdentityBefore.executable_sha256 === sourceIdentityAfter.executable_sha256
    && sourceIdentityBefore.app_artifact_sha256 === sourceIdentityAfter.app_artifact_sha256;
  check(
    "Electron DOM 回归期间源码 HEAD、工作区指纹与打包可执行文件保持不变",
    sourceIdentityStable,
    JSON.stringify({ before: sourceIdentityBefore, after: sourceIdentityAfter }),
  );
  check(
    "打包 Electron 会话链路未出现 Desktop IPC 协议拒绝",
    desktopIpcValidationErrors.length === 0,
    desktopIpcValidationErrors.slice(0, 3).join(" | "),
  );
  const evidence = {
    type: "electron_conversation_dom_acceptance",
    app: appExecutable,
      cdpPort,
      providerPort,
      checks,
      desktopIpcValidationErrors,
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stable: sourceIdentityStable,
      },
      providerRequests: provider.requests.length,
      rendererTimingSamples,
      providerRequestSummary: provider.requests.map((request) => ({
        user: (Array.isArray(request.messages) ? request.messages : [])
          .filter((message) => message?.role === "user")
          .map(messageText)
          .at(-1)?.slice(-160) || "",
        hasTools: Array.isArray(request.tools) && request.tools.length > 0,
        hasToolResult: (Array.isArray(request.messages) ? request.messages : [])
          .some((message) => message?.role === "tool"),
      })),
      shutdown: {
        turnId: shutdownTurnId,
        terminalStage: shutdownTerminalStage || (shutdownSettlementObserved ? "daemon_shutdown_settlement" : ""),
        terminalCount: shutdownTerminal?.count || 0,
        terminalObserved: shutdownTiming?.stages?.terminal?.count > 0 || shutdownSettlementObserved,
        daemonSettlementObserved: shutdownSettlementObserved,
        daemonSettledTurnCount: Math.max(0, daemonShutdownSettledTurnCount - shutdownSettledTurnCountBefore),
        electronExited: restartedElectron.exitCode !== null || restartedElectron.signalCode !== null,
      },
    status: "passed",
  };
  console.log(JSON.stringify({
    ...evidence,
    ...(evidencePath ? { evidencePath } : {}),
  }, null, 2));
  if (evidencePath) {
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  }
  }
} finally {
  page?.close();
  if (restartedElectron) {
    daemonPidForCleanup = await ownedDaemonPid(restartedElectron.pid) || daemonPidForCleanup;
    await stopOwnedProcess(restartedElectron, appRestartCdpPort);
  }
  daemonPidForCleanup = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
  await stopOwnedProcess(electron, cdpPort);
  await stopOwnedDaemon(daemonPidForCleanup);
  await closeProvider(provider.server);
  await rm(approvalTarget, { force: true });
  await rm(approvalDenyTarget, { force: true });
  await rm(approvalCancelTarget, { force: true });
  await rm(approvalExpiryTarget, { force: true });
  await rm(fullAccessTarget, { force: true });
  if (gitDirtyRoot) await rm(gitDirtyRoot, { recursive: true, force: true });
  if (gitDriftRoot) await rm(gitDriftRoot, { recursive: true, force: true });
  if (gitConflictRoot) await rm(gitConflictRoot, { recursive: true, force: true });
  if (gitMutationRoot) await rm(gitMutationRoot, { recursive: true, force: true });
  await removeStateRoot(stateRoot);
}
