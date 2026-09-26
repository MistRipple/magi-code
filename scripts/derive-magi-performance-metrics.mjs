#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const SCENARIOS = [
  "new_personal_chat",
  "personal_long_history",
  "workspace_chat",
  "workspace_tool",
  "subagent_concurrency",
];
const METRICS = [
  "accepted_to_provider_request_ms",
  "provider_request_to_raw_ms",
  "accepted_to_raw_ms",
  "accepted_to_visible_ms",
  "accepted_to_event_bus_ms",
  "accepted_to_terminal_ms",
  "magi_first_content_overhead_ms",
];

function usage() {
  console.error([
    "聚合 daemon 性能阶段：",
    "  node scripts/derive-magi-performance-metrics.mjs --role before|after --output OUTPUT --input EVIDENCE [--log LOG]",
    "比较 before/after：",
    "  node scripts/derive-magi-performance-metrics.mjs --compare BEFORE AFTER --output OUTPUT",
  ].join("\n"));
}

function parseArgs(argv) {
  let role = null;
  let output = null;
  let compare = null;
  const inputs = [];
  const logs = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--role") role = argv[++index];
    else if (value === "--output") output = resolve(argv[++index]);
    else if (value === "--input") inputs.push(resolve(argv[++index]));
    else if (value === "--log") logs.push(resolve(argv[++index]));
    else if (value === "--compare") compare = [resolve(argv[++index]), resolve(argv[++index])];
    else if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    } else throw new Error(`未知参数：${value}`);
  }
  if (!output) throw new Error("必须提供 --output");
  if (compare && (role || inputs.length > 0 || logs.length > 0)) {
    throw new Error("--compare 不能与 --role/--input/--log 同时使用");
  }
  if (compare) return { mode: "compare", output, compare };
  if (!role || !["before", "after"].includes(role) || inputs.length !== 1) {
    usage();
    throw new Error("聚合模式必须提供 role 和一个 --input");
  }
  return { mode: "aggregate", role, output, input: inputs[0], log: logs[0] || null };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function stableValue(value) {
  if (Array.isArray(value)) return value.map(stableValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value).sort().map((key) => [key, stableValue(value[key])]),
    );
  }
  return value;
}

function hashJson(value) {
  return sha256(JSON.stringify(stableValue(value)));
}

function field(line, ...names) {
  for (const name of names) {
    const quoted = line.match(new RegExp(`${name}="([^"]*)"`, "u"));
    if (quoted) return quoted[1];
    const raw = line.match(new RegExp(`${name}=([^\\s]+)`, "u"));
    if (raw) return raw[1];
  }
  return "";
}

function timestamp(line) {
  const match = line.match(/\b(\d{4}-\d{2}-\d{2}T[^\s]+Z)\b/u);
  const value = match ? Date.parse(match[1]) : Number.NaN;
  return Number.isFinite(value) ? value : null;
}

function identityKey(source) {
  return JSON.stringify({
    commit: source?.before?.commit || source?.before?.source_commit || null,
    fingerprint: source?.before?.fingerprintSha256 || source?.before?.worktree_fingerprint_sha256 || null,
    executable: source?.before?.executable_sha256 || null,
  });
}

function nearestRank(values, percentile) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(percentile * sorted.length) - 1)];
}

function stats(values) {
  const finiteValues = values.filter(Number.isFinite);
  if (finiteValues.length === 0) return { samples: 0, applicable: false };
  return {
    samples: finiteValues.length,
    applicable: true,
    p50_ms: nearestRank(finiteValues, 0.5),
    p95_ms: nearestRank(finiteValues, 0.95),
    max_ms: Math.max(...finiteValues),
  };
}

function parseLog(logText) {
  const byRequest = new Map();
  const byTurn = new Map();
  for (const line of logText.split(/\r?\n/u)) {
    if (!line.includes("conversation response timing")) continue;
    const stage = field(line, "stage");
    const requestId = field(line, "request_id", "requestId");
    const turnId = field(line, "turn_id", "turnId", "expected_turn_id");
    const atMs = timestamp(line);
    if (!stage || !atMs || (!requestId && !turnId)) continue;
    const event = {
      stage,
      atMs,
      sequence: Number.parseInt(field(line, "event_sequence", "eventSeq"), 10),
      providerCallId: field(line, "provider_call_id", "providerCallId") || null,
      requestId: requestId || null,
      turnId: turnId || null,
    };
    if (requestId) {
      const list = byRequest.get(requestId) || [];
      list.push(event);
      byRequest.set(requestId, list);
    }
    if (turnId) {
      const list = byTurn.get(turnId) || [];
      list.push(event);
      byTurn.set(turnId, list);
    }
  }
  return { byRequest, byTurn };
}

function firstStage(events, ...names) {
  const candidates = events.filter((event) => names.includes(event.stage));
  candidates.sort((left, right) => left.atMs - right.atMs);
  return candidates[0] || null;
}

function metricValues(events) {
  const accepted = firstStage(events, "accepted_response_sent");
  const providerRequest = firstStage(events, "provider_request_started");
  const raw = firstStage(events, "provider_first_raw_delta");
  const visible = firstStage(events, "provider_first_delta");
  const eventBus = firstStage(events, "event_bus_item_published");
  const terminal = firstStage(events, "canonical_terminal_published");
  assert(accepted && providerRequest && raw && visible && eventBus && terminal,
    `阶段不完整：${JSON.stringify({ accepted, providerRequest, raw, visible, eventBus, terminal })}`);
  const fromAccepted = (event) => event.atMs - accepted.atMs;
  return {
    accepted_to_provider_request_ms: fromAccepted(providerRequest),
    provider_request_to_raw_ms: raw.atMs - providerRequest.atMs,
    accepted_to_raw_ms: fromAccepted(raw),
    accepted_to_visible_ms: fromAccepted(visible),
    accepted_to_event_bus_ms: fromAccepted(eventBus),
    accepted_to_terminal_ms: fromAccepted(terminal),
    magi_first_content_overhead_ms: raw.providerCallId && raw.providerCallId === visible.providerCallId
      ? fromAccepted(visible) - (raw.atMs - providerRequest.atMs)
      : null,
  };
}

async function aggregate({ role, output, input, log }) {
  const evidenceText = await readFile(input, "utf8");
  const evidence = JSON.parse(evidenceText);
  const logPath = log || evidence.logPath;
  assert(evidence.status === "passed", `${input} status 不是 passed`);
  assert(evidence.fixture === "real-provider-performance-v1", `${input} fixture 不一致`);
  assert(evidence.samples?.length === 100, `${input} 必须包含 100 条 sample`);
  assert(evidence.source?.stableDuringSample === true, `${input} 未确认 source 稳定`);
  assert(logPath, `${input} 缺少 daemon log`);
  const logText = await readFile(logPath, "utf8");
  const parsed = parseLog(logText);
  const metrics = [];
  for (const [index, sample] of evidence.samples.entries()) {
    const events = [
      ...(parsed.byRequest.get(sample.requestId) || []),
      ...(parsed.byTurn.get(sample.turnId) || []),
    ];
    const values = metricValues(events);
    metrics.push({
      fixture: evidence.fixture,
      comparison_role: role,
      scenario: sample.scenario,
      sample_index: Number.isInteger(sample.sampleIndex) ? sample.sampleIndex : index,
      session_id: sample.sessionId,
      turn_id: sample.turnId,
      request_id: sample.requestId,
      outcome: sample.status,
      ...values,
    });
  }
  const summary = {};
  for (const scenario of SCENARIOS) {
    const rows = metrics.filter((metric) => metric.scenario === scenario);
    assert(rows.length === 20, `${scenario} 必须有 20 条 metric`);
    summary[scenario] = Object.fromEntries(METRICS.map((metric) => [
      metric,
      stats(rows.map((row) => row[metric])),
    ]));
  }
  const result = {
    type: "magi_performance_metrics",
    schema_version: "magi.performance.v1",
    metric_version: "magi-performance-metrics.v1",
    fixture: evidence.fixture,
    comparison_role: role,
    input_file: basename(input),
    log_file: basename(logPath),
    input_payload_sha256: sha256(evidenceText),
    input_payload_hash: hashJson(evidence),
    daemon_log_sha256: sha256(logText),
    source: evidence.source,
    provider: evidence.provider,
    metric_definitions: {
      accepted_to_provider_request_ms: "accepted_response_sent 到 provider_request_started 的 daemon 绝对时间差",
      provider_request_to_raw_ms: "provider_request_started 到 provider_first_raw_delta 的 daemon 绝对时间差",
      accepted_to_raw_ms: "accepted_response_sent 到 provider_first_raw_delta 的 daemon 绝对时间差",
      accepted_to_visible_ms: "accepted_response_sent 到 provider_first_delta 的 daemon 绝对时间差",
      accepted_to_event_bus_ms: "accepted_response_sent 到首个 event_bus_item_published 的 daemon 绝对时间差",
      accepted_to_terminal_ms: "accepted_response_sent 到首个 canonical_terminal_published 的 daemon 绝对时间差",
      magi_first_content_overhead_ms: "仅当首 raw 与首 visible 属于同一 provider_call 时，accepted_response_sent 到首 visible 减去 Provider TTFT",
    },
    sample_count: metrics.length,
    summary,
    metrics,
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

async function compare({ output, compare }) {
  const before = JSON.parse(await readFile(compare[0], "utf8"));
  const after = JSON.parse(await readFile(compare[1], "utf8"));
  assert(before.status === "passed" && after.status === "passed", "before/after metrics 必须通过");
  assert(before.fixture === after.fixture, "before/after fixture 不一致");
  assert(before.sample_count === 100 && after.sample_count === 100, "before/after 必须各有 100 条 metrics");
  assert(identityKey(before.source) !== identityKey(after.source), "before/after source 身份未区分");
  assert(before.provider?.model === after.provider?.model, "before/after model 不一致");
  const deltas = {};
  for (const scenario of SCENARIOS) {
    deltas[scenario] = {};
    for (const metric of METRICS) {
      const beforeStats = before.summary[scenario][metric];
      const afterStats = after.summary[scenario][metric];
      deltas[scenario][metric] = beforeStats.applicable && afterStats.applicable
        ? {
          p50_ms: afterStats.p50_ms - beforeStats.p50_ms,
          p95_ms: afterStats.p95_ms - beforeStats.p95_ms,
          max_ms: afterStats.max_ms - beforeStats.max_ms,
        }
        : { applicable: false };
    }
  }
  const result = {
    type: "magi_performance_before_after",
    schema_version: "magi.performance.v1",
    metric_version: "magi-performance-metrics.v1",
    fixture: before.fixture,
    before: { input_file: basename(compare[0]), source: before.source, summary: before.summary },
    after: { input_file: basename(compare[1]), source: after.source, summary: after.summary },
    deltas,
    validation: {
      same_fixture: true,
      same_model: true,
      same_sample_count_per_scenario: 20,
      absolute_daemon_clock: true,
      nearest_rank: true,
    },
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

try {
  const args = parseArgs(process.argv.slice(2));
  const result = args.mode === "aggregate" ? await aggregate(args) : await compare(args);
  console.log(JSON.stringify({ status: result.status, output: args.output, fixture: result.fixture }, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
}
