import { createWriteStream } from "node:fs";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";

/**
 * 真实 OpenAI-compatible Provider 的默认五场景时序采样；可通过
 * MAGI_PERF_SCENARIOS 选择 personal_short_history 等 correctness 切片。
 *
 * 该脚本只使用本地 daemon、独立状态根和用户已经配置的 Provider；不输出 API key，
 * 不接管已有 daemon 或 Electron 进程。输出 JSON 仅包含请求身份、阶段耗时和终态。
 * 前端 reducer/projection/DOM 阶段需要由 Electron DOM harness 另行采样，不能由本脚本
 * 把后端事件时间冒充浏览器绘制时间。
 */

const repositoryRoot = resolve(new URL("..", import.meta.url).pathname);
const daemonBinary = process.env.MAGI_PERF_DAEMON_BIN
  || join(repositoryRoot, "target/release/magi-daemon-app");
const providerBaseUrl = (process.env.MAGI_PERF_PROVIDER_URL || "http://127.0.0.1:8317/v1").replace(/\/$/u, "");
const providerModel = process.env.MAGI_PERF_MODEL || "gpt-5.6-luna";
const fixture = process.env.MAGI_PERF_FIXTURE || "real-provider-performance-v1";
const daemonPort = Number.parseInt(process.env.MAGI_PERF_PORT || "39237", 10);
const sampleCount = Number.parseInt(process.env.MAGI_PERF_SAMPLES || "20", 10);
const reasoningEffort = process.env.MAGI_PERF_REASONING_EFFORT || "medium";
const evidencePath = process.env.MAGI_PERF_EVIDENCE || join(tmpdir(), "magi-real-provider-performance.json");
const timeoutMs = Number.parseInt(process.env.MAGI_PERF_TURN_TIMEOUT_MS || "180000", 10);
const settlementObservationTimeoutMs = Number.parseInt(
  process.env.MAGI_PERF_SETTLEMENT_TIMEOUT_MS || "30000",
  10,
);
const providerIdleTimeoutMs = process.env.MAGI_PERF_PROVIDER_IDLE_TIMEOUT_MS
  ? Number.parseInt(process.env.MAGI_PERF_PROVIDER_IDLE_TIMEOUT_MS, 10)
  : null;
const contextWindowTokens = process.env.MAGI_PERF_CONTEXT_WINDOW_TOKENS
  ? Number.parseInt(process.env.MAGI_PERF_CONTEXT_WINDOW_TOKENS, 10)
  : null;
const longHistoryChars = process.env.MAGI_PERF_LONG_HISTORY_CHARS
  ? Number.parseInt(process.env.MAGI_PERF_LONG_HISTORY_CHARS, 10)
  : null;
const faultMode = process.env.MAGI_PERF_FAULT_MODE?.trim() || null;
const faultProxyHoldMs = Number.parseInt(process.env.MAGI_PERF_FAULT_HOLD_MS || "5000", 10);
const continueAfterSampleFailure = process.env.MAGI_PERF_CONTINUE_AFTER_SAMPLE_FAILURE === "1";
const keepStateOnFailure = process.env.MAGI_PERF_KEEP_STATE === "1";
const selectedScenarios = (process.env.MAGI_PERF_SCENARIOS || "new_personal_chat,personal_long_history,workspace_chat,workspace_tool,subagent_concurrency")
  .split(",")
  .map((value) => value.trim())
  .filter(Boolean);
const expectedOutcome = process.env.MAGI_PERF_EXPECTED_OUTCOME?.trim() || null;
const allScenarioNames = new Set([
  "new_personal_chat",
  "personal_short_history",
  "personal_long_history",
  "workspace_chat",
  "workspace_tool",
  "subagent_concurrency",
]);
if (selectedScenarios.some((scenario) => !allScenarioNames.has(scenario))) {
  throw new Error(`MAGI_PERF_SCENARIOS 包含未知场景：${selectedScenarios.join(",")}`);
}
if (expectedOutcome !== null && !new Set(["failed", "cancelled", "blocked"]).has(expectedOutcome)) {
  throw new Error(`MAGI_PERF_EXPECTED_OUTCOME 无效：${expectedOutcome}`);
}
if (faultMode !== null && !new Set(["empty_stream", "timeout"]).has(faultMode)) {
  throw new Error(`MAGI_PERF_FAULT_MODE 无效：${faultMode}`);
}
if (faultMode !== null && expectedOutcome !== "failed") {
  throw new Error("MAGI_PERF_FAULT_MODE 必须与 MAGI_PERF_EXPECTED_OUTCOME=failed 一起使用");
}
if (providerIdleTimeoutMs !== null
  && (!Number.isInteger(providerIdleTimeoutMs) || providerIdleTimeoutMs < 100)) {
  throw new Error("MAGI_PERF_PROVIDER_IDLE_TIMEOUT_MS 必须是至少 100 的整数");
}
if (!Number.isInteger(faultProxyHoldMs) || faultProxyHoldMs < 1) {
  throw new Error("MAGI_PERF_FAULT_HOLD_MS 必须是正整数");
}
if (contextWindowTokens !== null
  && (!Number.isInteger(contextWindowTokens) || contextWindowTokens < 16_000 || contextWindowTokens > 10_000_000)) {
  throw new Error("MAGI_PERF_CONTEXT_WINDOW_TOKENS 必须在 16000 到 10000000 之间");
}
if (longHistoryChars !== null && (!Number.isInteger(longHistoryChars) || longHistoryChars < 1_000)) {
  throw new Error("MAGI_PERF_LONG_HISTORY_CHARS 必须是至少 1000 的整数");
}
if (faultMode === "timeout"
  && (providerIdleTimeoutMs === null || faultProxyHoldMs <= providerIdleTimeoutMs)) {
  throw new Error(
    "Provider timeout 采样必须设置 MAGI_PERF_PROVIDER_IDLE_TIMEOUT_MS，且 fault hold 必须更长",
  );
}

if (!Number.isInteger(daemonPort) || daemonPort < 1024 || daemonPort > 65535) {
  throw new Error(`MAGI_PERF_PORT 无效：${daemonPort}`);
}
if (!Number.isInteger(sampleCount) || sampleCount < 1 || sampleCount > 100) {
  throw new Error(`MAGI_PERF_SAMPLES 必须在 1 到 100 之间：${sampleCount}`);
}
if (!Number.isInteger(settlementObservationTimeoutMs) || settlementObservationTimeoutMs < 1) {
  throw new Error(`MAGI_PERF_SETTLEMENT_TIMEOUT_MS 必须是正整数：${settlementObservationTimeoutMs}`);
}

const baseUrl = `http://127.0.0.1:${daemonPort}`;
const sleepMs = (milliseconds) => sleep(milliseconds);
const now = () => Date.now();

function percentile(values, percentileValue) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const rank = Math.max(0, Math.ceil(sorted.length * percentileValue / 100) - 1);
  return sorted[rank];
}

function metrics(values) {
  const present = values.filter((value) => Number.isFinite(value));
  return {
    samples: present.length,
    p50: percentile(present, 50),
    p95: percentile(present, 95),
    max: present.length > 0 ? Math.max(...present) : null,
  };
}

async function readProviderKey() {
  const explicit = process.env.MAGI_PERF_API_KEY?.trim();
  if (explicit) return explicit;
  const settingsPath = process.env.MAGI_PERF_SETTINGS_PATH || join(homedir(), ".magi", "settings.json");
  const settings = JSON.parse(await readFile(settingsPath, "utf8"));
  const key = settings?.orchestrator?.apiKey;
  if (typeof key !== "string" || key.trim() === "") {
    throw new Error(`未找到 Provider API key：${settingsPath}`);
  }
  return key.trim();
}

async function collectSourceIdentity() {
  const runGit = (args, options = {}) => {
    const result = spawnSync("git", args, {
      cwd: repositoryRoot,
      encoding: options.encoding || "utf8",
      // 工作区可能包含 Electron 图标等二进制修改；Node 默认的 1 MiB
      // spawnSync 缓冲区会把有效的源码身份采集误报为 ENOBUFS。
      maxBuffer: 128 * 1024 * 1024,
    });
    if (result.status !== 0) {
      throw new Error(`采集源码身份失败：git ${args.join(" ")}：${result.stderr || result.error || "unknown error"}`);
    }
    return result.stdout;
  };
  const commit = runGit(["rev-parse", "HEAD"]).trim();
  const status = runGit(["status", "--porcelain=v1", "--untracked-files=all"]);
  const trackedDiff = runGit(["diff", "--binary", "HEAD"], { encoding: "buffer" });
  const untrackedPaths = runGit(["ls-files", "--others", "--exclude-standard", "-z"])
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

async function sha256File(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

function readRequestBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => resolve(Buffer.concat(chunks)));
    request.on("error", reject);
  });
}

function upstreamEndpointFor(pathname) {
  const upstream = new URL(providerBaseUrl);
  const endpointSuffix = pathname
    .replace(/^\/+/u, "")
    .replace(/^v1\//u, "");
  const basePath = upstream.pathname.replace(/\/+$/u, "");
  const endpointPrefix = basePath.endsWith("/v1")
    ? basePath
    : `${basePath}/v1`.replace(/^\/v1\/v1$/u, "/v1");
  upstream.pathname = `${endpointPrefix}/${endpointSuffix}`.replace(/\/+/gu, "/");
  upstream.search = "";
  return upstream;
}

async function startFaultProxy() {
  const state = {
    mode: faultMode,
    requests: 0,
    upstreamStatuses: [],
    emptyResponses: 0,
    timeoutResponses: 0,
    heldMs: [],
  };
  const server = createServer(async (request, response) => {
    if (request.method !== "POST") {
      response.writeHead(404);
      response.end();
      return;
    }
    const upstreamController = new AbortController();
    request.on("aborted", () => upstreamController.abort());
    try {
      const body = await readRequestBody(request);
      const forwardedHeaders = Object.fromEntries(
        Object.entries(request.headers)
          .filter(([name]) => !new Set(["host", "content-length", "connection", "accept-encoding"]).has(name)),
      );
      forwardedHeaders.accept = "text/event-stream";
      const upstreamRequest = fetch(upstreamEndpointFor(request.url || "/v1/chat/completions"), {
        method: "POST",
        headers: forwardedHeaders,
        body,
        signal: upstreamController.signal,
      }).then(async (upstream) => {
        state.upstreamStatuses.push(upstream.status);
        await upstream.body?.cancel();
      }).catch((error) => {
        if (!upstreamController.signal.aborted) {
          state.upstreamErrors = (state.upstreamErrors || 0) + 1;
        }
        return null;
      });
      state.requests += 1;
      if (faultMode === "empty_stream") {
        state.emptyResponses += 1;
        response.writeHead(200, {
          "cache-control": "no-cache",
          "connection": "close",
          "content-type": "text/event-stream",
        });
        response.end("data: [DONE]\n\n");
        upstreamController.abort();
        void upstreamRequest;
        return;
      }

      state.timeoutResponses += 1;
      response.writeHead(200, {
        "cache-control": "no-cache",
        "connection": "keep-alive",
        "content-type": "text/event-stream",
      });
      response.flushHeaders();
      const holdStartedAt = now();
      await new Promise((resolve) => {
        let settled = false;
        const finish = () => {
          if (settled) return;
          settled = true;
          state.heldMs.push(now() - holdStartedAt);
          clearTimeout(timer);
          upstreamController.abort();
          resolve();
        };
        const timer = setTimeout(() => {
          response.end();
          finish();
        }, faultProxyHoldMs);
        request.on("aborted", finish);
        response.on("close", finish);
      });
    } catch (error) {
      if (!response.headersSent) {
        response.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
        response.end("fault proxy request failed");
      } else {
        response.destroy();
      }
      if (!upstreamController.signal.aborted) {
        console.error(`fault proxy request failed: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("fault proxy 未取得监听端口");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}`,
    state,
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}

async function ensurePortAvailable() {
  try {
    const response = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(400) });
    throw new Error(`端口 ${daemonPort} 已有 HTTP 服务响应 ${response.status}，为避免接管其他进程而终止。`);
  } catch (error) {
    if (error instanceof Error && error.message.includes("已有 HTTP 服务")) throw error;
  }
}

async function waitForDaemonPort() {
  const deadline = now() + 30_000;
  while (now() < deadline) {
    try {
      const health = await requestJson("/health");
      if (health?.status === "ok") return health;
    } catch {
      // daemon 尚未绑定端口，继续等待。
    }
    await sleepMs(100);
  }
  throw new Error("等待性能采样 daemon 健康状态超时");
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
  if (!response.ok) {
    throw new Error(`${init.method || "GET"} ${path} HTTP ${response.status}: ${typeof body === "string" ? body : JSON.stringify(body)}`);
  }
  return body;
}

function lineContainsIdentity(line, { traceId, turnId }) {
  if (traceId && line.includes(traceId)) return true;
  if (!turnId) return false;
  return line.includes(`turn_id=${turnId}`)
    || line.includes(`turn_id="${turnId}"`)
    || line.includes(`turn_id='${turnId}'`);
}

function extractStage(line, identity) {
  if (!lineContainsIdentity(line, identity)) return null;
  const stage = line.match(/stage="?([A-Za-z0-9_]+)"?/u)?.[1];
  const elapsed = line.match(/elapsed_ms=(\d+)/u)?.[1];
  if (!stage || elapsed === undefined) return null;
  return { stage, elapsedMs: Number(elapsed), line };
}

async function waitForChildExit(child, timeout = 8_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    sleepMs(timeout),
  ]);
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

function longHistoryPrompt(index) {
  if (longHistoryChars === null) {
    return `只回答 REAL_PROVIDER_PERF_OK，个人长历史样本 ${index}。`;
  }
  const anchor = `LONG_CONTEXT_FIXTURE_${index}: 这是普通对话中的背景资料。`;
  const filler = "请保留这段资料的编号、顺序和文字边界；这些文字仅作背景。";
  const payload = `${anchor}${filler}`.repeat(
    Math.ceil(longHistoryChars / (anchor.length + filler.length)),
  ).slice(0, longHistoryChars);
  return `只进行普通聊天，不要执行工具，只回答 REAL_PROVIDER_CONTEXT_OK，背景资料编号 ${index}。\n${payload}`;
}

function contextCompactionEventFromPayload(payload, sequence) {
  const canonicalItem = payload.canonical_item || payload.canonicalItem;
  const metadata = canonicalItem?.metadata;
  if (metadata?.noticeKind !== "context_compaction") return null;
  return {
    sequence,
    state: metadata.compactionState || null,
    phase: metadata.phase || null,
    reason: metadata.reason || null,
    originalMessageCount: metadata.originalMessageCount ?? null,
    compactedMessageCount: metadata.compactedMessageCount ?? null,
    originalTokenEstimate: metadata.originalTokenEstimate ?? null,
    compactedTokenEstimate: metadata.compactedTokenEstimate ?? null,
  };
}

async function observeTurnEvents({
  sessionId,
  scope,
  workspaceId,
  afterSequence,
  turnId,
  startedAt,
  waitTimeoutMs = timeoutMs,
}) {
  const query = new URLSearchParams({
    scope,
    afterSequence: String(afterSequence || 0),
    sessionId,
  });
  if (scope === "workspace") query.set("workspaceId", workspaceId);
  const controller = new AbortController();
  const response = await fetch(`${baseUrl}/events?${query}`, { signal: controller.signal });
  if (!response.ok || !response.body) {
    throw new Error(`事件流 HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let firstEventMs = null;
  let firstEventSequence = null;
  let terminal = null;
  const compactionEvents = [];
  const deadline = startedAt + waitTimeoutMs;
  try {
    while (now() < deadline) {
      const remaining = Math.max(1, deadline - now());
      const readResult = await Promise.race([
        reader.read(),
        sleepMs(remaining).then(() => ({ timeout: true })),
      ]);
      if (readResult.timeout) throw new Error(`Turn ${turnId} 事件流等待超时`);
      if (readResult.done) break;
      buffer += decoder.decode(readResult.value, { stream: true });
      let separator;
      while ((separator = buffer.search(/\r?\n\r?\n/u)) >= 0) {
        const block = buffer.slice(0, separator);
        buffer = buffer.slice(separator).replace(/^\r?\n\r?\n/u, "");
        const event = parseSseBlock(block);
        if (!event || event.session_id !== sessionId) continue;
        const payload = event.payload || {};
        const canonicalTurn = payload.canonical_turn || payload.canonicalTurn || payload.current_turn || payload.currentTurn;
        if (canonicalTurn?.turnId !== turnId && payload.turn_id !== turnId && payload.turnId !== turnId) continue;
        const kind = payload.canonical_event_kind || payload.canonicalEventKind;
        const status = canonicalTurn?.status || payload.canonical_item_status || payload.canonicalItemStatus;
        const compactionEvent = contextCompactionEventFromPayload(payload, event.sequence);
        if (compactionEvent) compactionEvents.push(compactionEvent);
        const isTerminal = ["turn_completed", "turn_failed", "turn_cancelled"].includes(kind)
          || ["completed", "failed", "cancelled"].includes(status);
        if (!isTerminal && firstEventMs === null) {
          firstEventMs = Math.max(0, now() - startedAt);
          firstEventSequence = event.sequence;
        }
        if (isTerminal) {
          terminal = { status, kind, sequence: event.sequence, observedMs: Math.max(0, now() - startedAt) };
          return { firstEventMs, firstEventSequence, terminal, compactionEvents };
        }
      }
    }
  } finally {
    controller.abort();
    reader.releaseLock();
  }
  throw new Error(`Turn ${turnId} 未收到 terminal canonical event`);
}

async function submitTurn({ scenario, scope = "personal", workspaceId = null, workspacePath = null, sessionId = null, text, accessProfile = null }) {
  const requestId = `real-provider-${scenario}-${Date.now()}-${randomUUID().slice(0, 8)}`;
  const request = {
    scope,
    sessionId,
    workspaceId,
    workspacePath,
    text,
    accessProfile,
    requestId,
    userMessageId: `user-${requestId}`,
  };
  // Session model selection is immutable through /api/session/turn after the first
  // canonical user item. Reused-session scenarios must keep the model chosen by
  // their seed turn instead of resubmitting an override on every request.
  if (!sessionId) {
    request.orchestratorSessionConfig = { model: providerModel, reasoningEffort };
  }
  const startedAt = now();
  let accepted = null;
  let acceptedAt = null;
  try {
    accepted = await requestJson("/api/session/turn", {
      method: "POST",
      body: JSON.stringify(request),
    });
    acceptedAt = now();
    const turnId = accepted.turnId;
    const observed = await observeTurnEvents({
      sessionId: accepted.sessionId,
      scope,
      workspaceId,
      afterSequence: accepted.eventSequence,
      turnId,
      startedAt,
    });
    return {
      scenario,
      requestId,
      sessionId: accepted.sessionId,
      turnId,
      route: accepted.route,
      executionProfile: accepted.executionProfile,
      status: observed.terminal.status,
      acceptedMs: acceptedAt - startedAt,
      firstEventMs: observed.firstEventMs,
      terminalMs: observed.terminal.observedMs,
      firstEventSequence: observed.firstEventSequence,
      terminalSequence: observed.terminal.sequence,
      compactionEvents: observed.compactionEvents,
    };
  } catch (error) {
    let cleanup = { status: "not_started" };
    if (accepted?.sessionId && accepted?.turnId) {
      let interrupt;
      try {
        const interruptRequest = { sessionId: accepted.sessionId };
        if (scope === "workspace") {
          interruptRequest.workspaceId = workspaceId;
          interruptRequest.workspacePath = workspacePath;
        }
        interrupt = await requestJson("/api/session/interrupt", {
          method: "POST",
          body: JSON.stringify(interruptRequest),
        });
      } catch (cleanupError) {
        cleanup = {
          status: "interrupt_request_failed",
          interruptReturned: false,
          settlementBarrier: "not_confirmed",
          error: cleanupError instanceof Error ? cleanupError.message : String(cleanupError),
        };
      }
      if (interrupt) {
        const settlementBarrier = interrupt.interrupted === true
          ? "runner_and_root_dispatcher_quiescence_waited_by_interrupt_handler"
          : "not_confirmed";
        const settlementStartedAt = now();
        try {
          const settled = await observeTurnEvents({
            sessionId: accepted.sessionId,
            scope,
            workspaceId,
            afterSequence: accepted.eventSequence,
            turnId: accepted.turnId,
            startedAt: settlementStartedAt,
            waitTimeoutMs: settlementObservationTimeoutMs,
          });
          cleanup = {
            status: interrupt.interrupted === true
              ? "interrupt_returned_terminal_observed"
              : "terminal_observed_without_interrupt",
            interruptReturned: true,
            interrupted: interrupt.interrupted,
            settlementBarrier,
            terminalStatus: settled.terminal.status,
            terminalSequence: settled.terminal.sequence,
            firstEventMs: settled.firstEventMs === null
              ? null
              : Math.max(0, settlementStartedAt - startedAt + settled.firstEventMs),
            firstEventSequence: settled.firstEventSequence,
            terminalMs: Math.max(0, settlementStartedAt - startedAt + settled.terminal.observedMs),
            compactionEvents: settled.compactionEvents,
          };
        } catch (cleanupError) {
          cleanup = {
            status: "interrupt_returned_terminal_unobserved",
            interruptReturned: true,
            interrupted: interrupt.interrupted,
            settlementBarrier,
            error: cleanupError instanceof Error ? cleanupError.message : String(cleanupError),
          };
        }
      }
    }
    return {
      scenario,
      requestId,
      sessionId: accepted?.sessionId || sessionId,
      turnId: accepted?.turnId || null,
      route: accepted?.route || null,
      executionProfile: accepted?.executionProfile || null,
      status: cleanup.terminalStatus || "failed",
      error: error instanceof Error ? error.message : String(error),
      cleanup,
      acceptedMs: acceptedAt === null ? null : acceptedAt - startedAt,
      firstEventMs: cleanup.firstEventMs ?? null,
      terminalMs: cleanup.terminalMs ?? null,
      firstEventSequence: cleanup.firstEventSequence ?? null,
      terminalSequence: [
        "interrupt_returned_terminal_observed",
        "terminal_observed_without_interrupt",
      ].includes(cleanup.status)
        ? cleanup.terminalSequence
        : null,
      terminalStatus: [
        "interrupt_returned_terminal_observed",
        "terminal_observed_without_interrupt",
      ].includes(cleanup.status)
        ? cleanup.terminalStatus
        : null,
      compactionEvents: cleanup.compactionEvents || [],
    };
  }
}

async function appendCompletedSample(rows, options) {
  const row = await submitTurn(options);
  rows.push(row);
  const acceptedOutcome = expectedOutcome === null
    ? row.status === "completed"
    : row.status === expectedOutcome;
  if (!acceptedOutcome && !continueAfterSampleFailure) {
    const cleanupStatus = row.cleanup?.status || "not_required";
    throw new Error(
      `真实 Provider 样本未达到预期 outcome，停止后续采样：scenario=${row.scenario} `
      + `turn=${row.turnId || "unknown"} status=${row.status} cleanup=${cleanupStatus}`,
    );
  }
  return row;
}

function summarize(rows, timingByRequest) {
  const scenarios = [...new Set(rows.map((row) => row.scenario))];
  return Object.fromEntries(scenarios.map((scenario) => {
    const samples = rows.filter((row) => row.scenario === scenario);
    const backendStageValues = (stage) => samples.map((row) => {
      const values = timingByRequest[row.requestId]?.[stage];
      return Array.isArray(values) ? values[0] : null;
    });
    return [scenario, {
      sampleCount: samples.length,
      acceptedMs: metrics(samples.map((row) => row.acceptedMs)),
      firstEventMs: metrics(samples.map((row) => row.firstEventMs)),
      terminalMs: metrics(samples.map((row) => row.terminalMs)),
      providerFirstRawDeltaMs: metrics(backendStageValues("provider_first_raw_delta")),
      providerFirstDeltaMs: metrics(backendStageValues("provider_first_delta")),
      statusCounts: Object.fromEntries([...new Set(samples.map((row) => row.status))].map((status) => [
        status,
        samples.filter((row) => row.status === status).length,
      ])),
      routes: [...new Set(samples.map((row) => row.route))],
      executionProfiles: [...new Set(samples.map((row) => row.executionProfile))],
      sequenceMonotonic: samples.every((row) => Number.isInteger(row.firstEventSequence)
        && Number.isInteger(row.terminalSequence)
        && row.firstEventSequence < row.terminalSequence),
    }];
  }));
}

async function main() {
  await ensurePortAvailable();
  const providerApiKey = await readProviderKey();
  const stateParent = await mkdtemp(join(tmpdir(), "magi-real-provider-performance-"));
  const stateRoot = join(stateParent, "state");
  const workspacePath = join(stateParent, "workspace");
  await mkdir(stateRoot, { recursive: true });
  await mkdir(workspacePath, { recursive: true });
  await writeFile(join(workspacePath, "README.md"), "# Magi real provider performance fixture\n");
  const gitInit = spawnSync("git", ["init", "-q", workspacePath], { encoding: "utf8" });
  if (gitInit.status !== 0) throw new Error(`性能 fixture Git 初始化失败：${gitInit.stderr}`);
  for (const [key, value] of [["user.name", "Magi Performance Harness"], ["user.email", "magi-performance@example.invalid"]]) {
    const result = spawnSync("git", ["-C", workspacePath, "config", key, value], { encoding: "utf8" });
    if (result.status !== 0) throw new Error(`性能 fixture Git 配置失败：${result.stderr}`);
  }
  const gitCommit = spawnSync("git", ["-C", workspacePath, "add", "README.md"], { encoding: "utf8" });
  if (gitCommit.status !== 0) throw new Error(`性能 fixture Git add 失败：${gitCommit.stderr}`);
  const commit = spawnSync("git", ["-C", workspacePath, "commit", "-qm", "seed performance fixture"], { encoding: "utf8" });
  if (commit.status !== 0) throw new Error(`性能 fixture Git commit 失败：${commit.stderr}`);
  const logPath = join(stateParent, "daemon.log");
  const sourceIdentityBefore = await collectSourceIdentity();
  const daemonBinarySha256 = await sha256File(daemonBinary);
  let faultProxy = null;
  if (faultMode !== null) {
    faultProxy = await startFaultProxy();
  }
  const daemonProviderBaseUrl = faultProxy?.baseUrl || providerBaseUrl;
  const logStream = createWriteStream(logPath, { flags: "a" });
  const daemon = spawn(daemonBinary, [], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      MAGI_HOST: "127.0.0.1",
      MAGI_PORT: String(daemonPort),
      MAGI_OPEN_BROWSER: "0",
      MAGI_WEB_DEV: "0",
      MAGI_STATE_ROOT: stateRoot,
      MAGI_OPENAI_COMPAT_BASE_URL: daemonProviderBaseUrl,
      MAGI_OPENAI_COMPAT_API_KEY: providerApiKey,
      MAGI_OPENAI_COMPAT_MODEL: providerModel,
      ...(providerIdleTimeoutMs === null
        ? {}
        : { MAGI_OPENAI_COMPAT_STREAM_IDLE_TIMEOUT_MS: String(providerIdleTimeoutMs) }),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  daemon.stdout.pipe(logStream);
  daemon.stderr.pipe(logStream);
  let rows = [];
  let completed = false;
  let samplingError = null;
  try {
    const health = await waitForDaemonPort();
    let contextConfiguration = null;
    if (contextWindowTokens !== null) {
      contextConfiguration = await requestJson("/api/settings/model-context-window/save", {
        method: "POST",
        body: JSON.stringify({
          model: providerModel,
          contextWindowTokens,
        }),
      });
    }
    const workspace = await requestJson("/api/workspaces/register", {
      method: "POST",
      body: JSON.stringify({ path: workspacePath }),
    });
    const workspaceId = workspace.workspaceId;
    const personal = [];
    try {
      if (selectedScenarios.includes("new_personal_chat")) {
        for (let index = 0; index < sampleCount; index += 1) {
          const row = await appendCompletedSample(rows, {
            scenario: "new_personal_chat",
            text: `只回答 REAL_PROVIDER_PERF_OK，新建个人会话样本 ${index}。`,
          });
          personal.push(row);
        }
      }
      if (selectedScenarios.includes("personal_short_history")) {
        // 用独立 seedRows 保存前一轮事实，但不把 seed Turn 混入短历史样本
        // 的统计；输出的 historySeedTurnId 让 ledger 仍能复核这是有一轮
        // 已完成历史的真实 Session，而不是把 new_personal_chat 改名。
        for (let index = 0; index < sampleCount; index += 1) {
          const seedRows = [];
          const seed = await appendCompletedSample(seedRows, {
            scenario: "short_history_seed",
            text: `只回答 REAL_PROVIDER_SHORT_HISTORY_SEED_OK，短历史 seed ${index}。`,
          });
          const row = await appendCompletedSample(rows, {
            scenario: "personal_short_history",
            sessionId: seed.sessionId,
            text: `只回答 REAL_PROVIDER_SHORT_HISTORY_OK，已有一轮历史的短历史样本 ${index}。`,
          });
          row.historySeedTurnId = seed.turnId;
        }
      }
      if (selectedScenarios.includes("personal_long_history")) {
        const longHistorySessionId = personal.find((row) => row.sessionId)?.sessionId;
        if (!longHistorySessionId) throw new Error("个人长历史场景需要先完成至少一轮新建个人会话");
        for (let index = 0; index < sampleCount; index += 1) {
          await appendCompletedSample(rows, {
            scenario: "personal_long_history",
            sessionId: longHistorySessionId,
            text: longHistoryPrompt(index),
          });
        }
      }
      if (selectedScenarios.includes("workspace_chat")) {
        for (let index = 0; index < sampleCount; index += 1) {
          await appendCompletedSample(rows, {
            scenario: "workspace_chat",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `只回答 REAL_PROVIDER_PERF_OK，工作区普通聊天样本 ${index}。`,
          });
        }
      }
      if (selectedScenarios.includes("workspace_tool")) {
        for (let index = 0; index < sampleCount; index += 1) {
          await appendCompletedSample(rows, {
            scenario: "workspace_tool",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `请明确调用 file_read 工具读取路径 ${join(workspacePath, "README.md")}，然后只回答 REAL_PROVIDER_PERF_OK，工作区工具样本 ${index}。`,
          });
        }
      }
      if (selectedScenarios.includes("subagent_concurrency")) {
        for (let index = 0; index < sampleCount; index += 1) {
          await appendCompletedSample(rows, {
            scenario: "subagent_concurrency",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `请创建两个子代理并行检查 ${join(workspacePath, "README.md")}，等待它们完成后只回答 REAL_PROVIDER_PERF_OK，主代理与子代理并发样本 ${index}。`,
          });
        }
      }
    } catch (error) {
      samplingError = error instanceof Error ? error.message : String(error);
    }
    logStream.end();
    await new Promise((resolve) => logStream.once("close", resolve));
    const logText = await readFile(logPath, "utf8");
    const timingByRequest = {};
    for (const row of rows) {
      const stages = logText
        .split("\n")
        .map((line) => extractStage(line, {
          traceId: row.requestId,
          turnId: row.turnId,
        }))
        .filter(Boolean);
      timingByRequest[row.requestId] = stages.reduce((result, { stage, elapsedMs }) => {
        (result[stage] ||= []).push(elapsedMs);
        return result;
      }, {});
    }
    const expectedSamples = selectedScenarios.length * sampleCount;
    const sourceIdentityAfter = await collectSourceIdentity();
    const sourceStableDuringSample = sourceIdentityBefore.fingerprintSha256
      === sourceIdentityAfter.fingerprintSha256
      && sourceIdentityBefore.commit === sourceIdentityAfter.commit;
    const expectedRowStatus = expectedOutcome || "completed";
    const sampleErrors = rows
      .filter((row) => row.error || row.status !== expectedRowStatus)
      .map((row) => ({
        scenario: row.scenario,
        requestId: row.requestId,
        turnId: row.turnId,
        error: row.error || `canonical terminal status: ${row.status}`,
        expectedOutcome,
        cleanup: row.cleanup || null,
      }));
    const unexpectedRows = rows.filter((row) => (
      row.status !== expectedRowStatus
      || !Number.isFinite(row.terminalMs)
      || !Number.isInteger(row.terminalSequence)
      || !timingByRequest[row.requestId]?.provider_request_started?.length
      || (expectedOutcome === "cancelled"
        && row.cleanup?.settlementBarrier !== "runner_and_root_dispatcher_quiescence_waited_by_interrupt_handler")
    ));
    if (samplingError === null && unexpectedRows.length > 0) {
      const firstUnexpected = unexpectedRows[0];
      samplingError = `${unexpectedRows.length} 个样本未达到预期 outcome；首个失败：${firstUnexpected.error || firstUnexpected.status}`;
    }
    const compactionRows = rows.filter((row) => row.scenario === "personal_long_history");
    const compactionRequired = contextWindowTokens !== null && compactionRows.length > 0;
    // contextWindowTokens 表示该 fixture 允许触发压缩，不表示每个连续 Turn
    // 都必须压缩。历史已经在前一个 Turn 被压缩时，后续 Turn 合法地产生
    // context_compaction=skipped；只有没有任何终态事实（completed/skipped）
    // 才是证据缺失。整个 fixture 仍至少要有一条 completed，才能证明真实
    // Provider 入口确实走过压缩链路，而不是全程只观察到 skipped。
    const compactionCompletedRows = compactionRows.filter((row) => (
      row.compactionEvents?.some((event) => event.state === "completed")
    ));
    const compactionSkippedRows = compactionRows.filter((row) => (
      row.compactionEvents?.some((event) => event.state === "skipped")
    ));
    const missingCompactionRows = compactionRequired
      ? compactionRows.filter((row) => (
        !row.compactionEvents?.some((event) => event.state === "completed" || event.state === "skipped")
      ))
      : [];
    if (compactionRequired && samplingError === null && compactionCompletedRows.length === 0) {
      samplingError = "长历史 fixture 没有任何样本观察到 canonical context_compaction completed notice";
    } else if (compactionRequired && samplingError === null && missingCompactionRows.length > 0) {
      samplingError = `${missingCompactionRows.length} 个长历史样本没有观察到 canonical context_compaction completed/skipped notice`;
    }
    if (!sourceStableDuringSample && samplingError === null) {
      samplingError = "采样期间源码 HEAD 或工作区内容发生变化，当前 artifact 不具备稳定的代码身份";
    }
    // 轨迹合同要求每个正常 Provider Turn 都区分 raw 与 visible delta；
    // tool-call-only 的首 chunk 也必须由 raw 阶段承载。异常/空流样本另行采集，
    // 不能用“只对 task 场景要求 raw”掩盖 conversation 入口缺少埋点。
    const rawDeltaRequiredScenarios = expectedOutcome === null
      ? new Set(selectedScenarios)
      : new Set();
    const missingRawDeltaRows = rows.filter((row) => {
      if (!rawDeltaRequiredScenarios.has(row.scenario)) return false;
      return !timingByRequest[row.requestId]?.provider_first_raw_delta?.length;
    });
    const allSuccessful = samplingError === null
      && rows.length === expectedSamples
      && rows.every((row) => row.status === expectedRowStatus)
      && missingRawDeltaRows.length === 0
      && missingCompactionRows.length === 0
      && sourceStableDuringSample;
    const preservedLogPath = `${evidencePath}.daemon.log`;
    await writeFile(preservedLogPath, logText);
    const evidence = {
      status: allSuccessful ? "passed" : "failed",
      trajectoryMode: expectedOutcome === null ? "normal" : "abnormal",
      expectedOutcome,
      fixture,
      generatedAt: new Date().toISOString(),
      provider: {
        baseUrl: providerBaseUrl,
        requestBaseUrl: daemonProviderBaseUrl,
        model: providerModel,
        reasoningEffort,
        ...(providerIdleTimeoutMs === null
          ? {}
          : { streamIdleTimeoutMs: providerIdleTimeoutMs }),
        ...(faultProxy === null
          ? {}
          : {
            faultInjection: {
              mode: faultMode,
              proxyBaseUrl: faultProxy.baseUrl,
              holdMs: faultProxyHoldMs,
              ...faultProxy.state,
            },
          }),
      },
      context: {
        configured: contextConfiguration,
        contextWindowTokens,
        longHistoryChars,
        compactionRequired,
        observedCompactionRows: compactionCompletedRows.length,
        skippedCompactionRows: compactionSkippedRows.length,
      },
      source: {
        before: sourceIdentityBefore,
        after: sourceIdentityAfter,
        stableDuringSample: sourceStableDuringSample,
      },
      daemon: {
        binary: daemonBinary,
        binarySha256: daemonBinarySha256,
        port: daemonPort,
        runtimeEpoch: health.runtimeEpoch,
      },
      sampleCount,
      timeouts: {
        turnObservationMs: timeoutMs,
        settlementObservationMs: settlementObservationTimeoutMs,
      },
      samplingPolicy: {
        continueAfterSampleFailure,
      },
      metricDefinitions: {
        acceptedMs: "脚本发出 POST 到收到 accepted 响应",
        firstEventMs: "脚本发出 POST 到收到该 Turn 首个 session.turn.item 事件",
        terminalMs: "脚本发出 POST 到收到该 Turn terminal canonical event",
        backendStages: "daemon magi.performance 日志中按 requestId 关联的阶段；不代表浏览器 DOM 绘制",
        providerFirstRawDeltaMs: "daemon 日志中该 requestId 的首个 raw Provider delta；工具/子代理场景必须存在",
        providerFirstDeltaMs: "daemon 日志中该 requestId 的首个可见 content/thinking delta",
      },
      summary: summarize(rows, timingByRequest),
      ...(samplingError === null ? {} : { samplingError }),
      ...(sampleErrors.length > 0 ? { sampleErrors } : {}),
      samples: rows.map((row) => ({
        ...row,
        providerFirstRawDeltaMs: timingByRequest[row.requestId]?.provider_first_raw_delta?.[0] ?? null,
        providerFirstDeltaMs: timingByRequest[row.requestId]?.provider_first_delta?.[0] ?? null,
        backendStages: timingByRequest[row.requestId] || {},
      })),
      logPath: preservedLogPath,
      ...(missingRawDeltaRows.length > 0
        ? { missingRawDeltaRequestIds: missingRawDeltaRows.map((row) => row.requestId) }
        : {}),
      ...(missingCompactionRows.length > 0
        ? { missingCompactionRequestIds: missingCompactionRows.map((row) => row.requestId) }
        : {}),
    };
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    completed = allSuccessful;
    console.log(JSON.stringify({ status: evidence.status, evidencePath, summary: evidence.summary }, null, 2));
    if (!allSuccessful) {
      throw new Error(
        samplingError
          ? `真实 Provider 轨迹采样中断：${samplingError}；已保存部分 JSON 证据。`
          : "真实 Provider 轨迹采样未满足预期 outcome；已保存完整 JSON 证据。",
      );
    }
  } finally {
    if (daemon.exitCode === null && daemon.signalCode === null) daemon.kill("SIGTERM");
    await waitForChildExit(daemon);
    if (daemon.exitCode === null && daemon.signalCode === null) daemon.kill("SIGKILL");
    await waitForChildExit(daemon, 2_000);
    logStream.destroy();
    if (faultProxy !== null) {
      await faultProxy.close().catch(() => {});
    }
    if (!keepStateOnFailure || completed) {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败采样状态根：${stateParent}`);
    }
  }
}

// Node's fetch/undici pool may keep idle sockets after the daemon and provider
// have been stopped. The evidence and cleanup work is complete at this point,
// so terminate explicitly instead of leaving a finished verification hanging.
main().then(
  () => process.exit(0),
  (error) => {
    console.error(error instanceof Error ? error.stack || error.message : error);
    process.exit(1);
  },
);
