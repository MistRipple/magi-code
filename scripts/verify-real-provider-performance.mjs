import { createWriteStream } from "node:fs";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";

/**
 * 真实 OpenAI-compatible Provider 的五场景时序采样。
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
const daemonPort = Number.parseInt(process.env.MAGI_PERF_PORT || "39237", 10);
const sampleCount = Number.parseInt(process.env.MAGI_PERF_SAMPLES || "20", 10);
const reasoningEffort = process.env.MAGI_PERF_REASONING_EFFORT || "medium";
const evidencePath = process.env.MAGI_PERF_EVIDENCE || join(tmpdir(), "magi-real-provider-performance.json");
const timeoutMs = Number.parseInt(process.env.MAGI_PERF_TURN_TIMEOUT_MS || "180000", 10);
const keepStateOnFailure = process.env.MAGI_PERF_KEEP_STATE === "1";
const selectedScenarios = (process.env.MAGI_PERF_SCENARIOS || "new_personal_chat,personal_long_history,workspace_chat,workspace_tool,subagent_concurrency")
  .split(",")
  .map((value) => value.trim())
  .filter(Boolean);
const allScenarioNames = new Set([
  "new_personal_chat",
  "personal_long_history",
  "workspace_chat",
  "workspace_tool",
  "subagent_concurrency",
]);
if (selectedScenarios.some((scenario) => !allScenarioNames.has(scenario))) {
  throw new Error(`MAGI_PERF_SCENARIOS 包含未知场景：${selectedScenarios.join(",")}`);
}

if (!Number.isInteger(daemonPort) || daemonPort < 1024 || daemonPort > 65535) {
  throw new Error(`MAGI_PERF_PORT 无效：${daemonPort}`);
}
if (!Number.isInteger(sampleCount) || sampleCount < 1 || sampleCount > 100) {
  throw new Error(`MAGI_PERF_SAMPLES 必须在 1 到 100 之间：${sampleCount}`);
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

async function observeTurnEvents({ sessionId, scope, workspaceId, afterSequence, turnId, startedAt }) {
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
  const deadline = startedAt + timeoutMs;
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
        if (event.event_type === "session.turn.item"
          && firstEventMs === null
          && (payload.delta !== undefined || payload.canonical_item !== undefined || payload.canonicalItem !== undefined)) {
          firstEventMs = Math.max(0, now() - startedAt);
          firstEventSequence = event.sequence;
        }
        const kind = payload.canonical_event_kind || payload.canonicalEventKind;
        const status = canonicalTurn?.status || payload.canonical_item_status || payload.canonicalItemStatus;
        if (["turn_completed", "turn_failed", "turn_cancelled"].includes(kind)
          || ["completed", "failed", "cancelled"].includes(status)) {
          terminal = { status, kind, sequence: event.sequence, observedMs: Math.max(0, now() - startedAt) };
          return { firstEventMs, firstEventSequence, terminal };
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
    orchestratorSessionConfig: { model: providerModel, reasoningEffort },
  };
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
    };
  } catch (error) {
    return {
      scenario,
      requestId,
      sessionId: accepted?.sessionId || sessionId,
      turnId: accepted?.turnId || null,
      route: accepted?.route || null,
      executionProfile: accepted?.executionProfile || null,
      status: "failed",
      error: error instanceof Error ? error.message : String(error),
      acceptedMs: acceptedAt === null ? null : acceptedAt - startedAt,
      firstEventMs: null,
      terminalMs: null,
      firstEventSequence: null,
      terminalSequence: null,
    };
  }
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
      sequenceMonotonic: samples.every((row) => row.firstEventSequence < row.terminalSequence),
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
      MAGI_OPENAI_COMPAT_BASE_URL: providerBaseUrl,
      MAGI_OPENAI_COMPAT_API_KEY: providerApiKey,
      MAGI_OPENAI_COMPAT_MODEL: providerModel,
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
    const workspace = await requestJson("/api/workspaces/register", {
      method: "POST",
      body: JSON.stringify({ path: workspacePath }),
    });
    const workspaceId = workspace.workspaceId;
    const personal = [];
    try {
      if (selectedScenarios.includes("new_personal_chat")) {
        for (let index = 0; index < sampleCount; index += 1) {
          const row = await submitTurn({
            scenario: "new_personal_chat",
            text: `只回答 REAL_PROVIDER_PERF_OK，新建个人会话样本 ${index}。`,
          });
          personal.push(row);
          rows.push(row);
        }
      }
      if (selectedScenarios.includes("personal_long_history")) {
        const longHistorySessionId = personal.find((row) => row.sessionId)?.sessionId;
        if (!longHistorySessionId) throw new Error("个人长历史场景需要先完成至少一轮新建个人会话");
        for (let index = 0; index < sampleCount; index += 1) {
          rows.push(await submitTurn({
            scenario: "personal_long_history",
            sessionId: longHistorySessionId,
            text: `只回答 REAL_PROVIDER_PERF_OK，个人长历史样本 ${index}。`,
          }));
        }
      }
      if (selectedScenarios.includes("workspace_chat")) {
        for (let index = 0; index < sampleCount; index += 1) {
          rows.push(await submitTurn({
            scenario: "workspace_chat",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `只回答 REAL_PROVIDER_PERF_OK，工作区普通聊天样本 ${index}。`,
          }));
        }
      }
      if (selectedScenarios.includes("workspace_tool")) {
        for (let index = 0; index < sampleCount; index += 1) {
          rows.push(await submitTurn({
            scenario: "workspace_tool",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `请明确调用 file_read 工具读取路径 ${join(workspacePath, "README.md")}，然后只回答 REAL_PROVIDER_PERF_OK，工作区工具样本 ${index}。`,
          }));
        }
      }
      if (selectedScenarios.includes("subagent_concurrency")) {
        for (let index = 0; index < sampleCount; index += 1) {
          rows.push(await submitTurn({
            scenario: "subagent_concurrency",
            scope: "workspace",
            workspaceId,
            workspacePath,
            text: `请创建两个子代理并行检查 ${join(workspacePath, "README.md")}，等待它们完成后只回答 REAL_PROVIDER_PERF_OK，主代理与子代理并发样本 ${index}。`,
          }));
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
    const sampleErrors = rows
      .filter((row) => row.error)
      .map((row) => ({
        scenario: row.scenario,
        requestId: row.requestId,
        turnId: row.turnId,
        error: row.error,
      }));
    if (samplingError === null && sampleErrors.length > 0) {
      samplingError = `${sampleErrors.length} 个样本未完成；首个失败：${sampleErrors[0].error}`;
    }
    const rawDeltaRequiredScenarios = new Set(["workspace_tool", "subagent_concurrency"]);
    const missingRawDeltaRows = rows.filter((row) => {
      if (!rawDeltaRequiredScenarios.has(row.scenario)) return false;
      return !timingByRequest[row.requestId]?.provider_first_raw_delta?.length;
    });
    const allSuccessful = samplingError === null
      && rows.length === expectedSamples
      && rows.every((row) => row.status === "completed")
      && missingRawDeltaRows.length === 0;
    const preservedLogPath = `${evidencePath}.daemon.log`;
    await writeFile(preservedLogPath, logText);
    const evidence = {
      status: allSuccessful ? "passed" : "failed",
      generatedAt: new Date().toISOString(),
      provider: { baseUrl: providerBaseUrl, model: providerModel, reasoningEffort },
      daemon: { binary: daemonBinary, port: daemonPort, runtimeEpoch: health.runtimeEpoch },
      sampleCount,
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
    };
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    completed = allSuccessful;
    console.log(JSON.stringify({ status: evidence.status, evidencePath, summary: evidence.summary }, null, 2));
    if (!allSuccessful) {
      throw new Error(
        samplingError
          ? `真实 Provider 性能采样中断：${samplingError}；已保存部分 JSON 证据。`
          : "真实 Provider 性能采样包含失败 Turn；已保存完整 JSON 证据，不能把失败样本标记为通过。",
      );
    }
  } finally {
    if (daemon.exitCode === null && daemon.signalCode === null) daemon.kill("SIGTERM");
    await waitForChildExit(daemon);
    if (daemon.exitCode === null && daemon.signalCode === null) daemon.kill("SIGKILL");
    await waitForChildExit(daemon, 2_000);
    logStream.destroy();
    if (!keepStateOnFailure || completed) {
      await rm(stateParent, { recursive: true, force: true }).catch(() => {});
    } else {
      console.error(`保留失败采样状态根：${stateParent}`);
    }
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
