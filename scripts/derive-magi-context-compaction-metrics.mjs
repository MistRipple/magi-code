#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const SCENARIOS = ["new_personal_chat", "personal_long_history"];
const REQUIRED_STAGES = [
  "accepted_response_sent",
  "provider_request_started",
  "provider_first_raw_delta",
  "provider_first_delta",
  "event_bus_item_published",
  "canonical_terminal_published",
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
const SCHEMA_VERSION = "magi.performance.v1";
const DERIVE_VERSION = "magi-context-compaction-metrics.v1";

function usage() {
  console.error([
    "派生真实 Provider 上下文压缩专项性能 sidecar：",
    "  node scripts/derive-magi-context-compaction-metrics.mjs --role before|after --input EVIDENCE --output OUTPUT [--log LOG]",
    "比较两个已通过的专项 sidecar：",
    "  node scripts/derive-magi-context-compaction-metrics.mjs --compare BEFORE AFTER --output OUTPUT",
  ].join("\n"));
}

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    }
    if (value === "--role" || value === "--input" || value === "--output" || value === "--log") {
      if (index + 1 >= argv.length) throw new Error(`参数缺少值：${value}`);
      const argument = argv[++index];
      values[value.slice(2)] = value === "--role" ? argument : resolve(argument);
      continue;
    }
    if (value === "--compare") {
      if (index + 2 >= argv.length) throw new Error("--compare 必须提供两个输入文件");
      values.compare = [resolve(argv[++index]), resolve(argv[++index])];
      continue;
    }
    throw new Error(`未知参数：${value}`);
  }
  if (values.compare) {
    if (values.role || values.input || values.log || !values.output) {
      throw new Error("--compare 不能与 --role/--input/--log 混用，并且必须提供 --output");
    }
    return { mode: "compare", ...values };
  }
  if (!values.role || !["before", "after"].includes(values.role) || !values.input || !values.output) {
    usage();
    throw new Error("聚合模式必须提供 --role、--input 和 --output");
  }
  return { mode: "aggregate", ...values };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
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

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
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

function parseLog(logText) {
  const byRequest = new Map();
  const byTurn = new Map();
  for (const line of logText.split(/\r?\n/u)) {
    if (!line.includes("conversation response timing")) continue;
    const stage = field(line, "stage");
    const requestId = field(line, "request_id", "requestId");
    const turnId = field(line, "turn_id", "turnId", "expected_turn_id");
    const atMs = timestamp(line);
    if (!stage || atMs === null || (!requestId && !turnId)) continue;
    const event = {
      stage,
      atMs,
      providerCallId: field(line, "provider_call_id", "providerCallId") || null,
      requestId: requestId || null,
      turnId: turnId || null,
    };
    if (requestId) {
      const events = byRequest.get(requestId) || [];
      events.push(event);
      byRequest.set(requestId, events);
    }
    if (turnId) {
      const events = byTurn.get(turnId) || [];
      events.push(event);
      byTurn.set(turnId, events);
    }
  }
  return { byRequest, byTurn };
}

function firstStage(events, name) {
  const candidates = events.filter((event) => event.stage === name);
  candidates.sort((left, right) => left.atMs - right.atMs);
  return candidates[0] || null;
}

function metricValues(events, identity) {
  const stages = Object.fromEntries(REQUIRED_STAGES.map((name) => [name, firstStage(events, name)]));
  assert(
    REQUIRED_STAGES.every((name) => stages[name]),
    `样本 ${identity} 缺少性能阶段：${JSON.stringify(Object.keys(stages).filter((name) => !stages[name]))}`,
  );
  const accepted = stages.accepted_response_sent;
  const providerRequest = stages.provider_request_started;
  const raw = stages.provider_first_raw_delta;
  const visible = stages.provider_first_delta;
  const fromAccepted = (event) => event.atMs - accepted.atMs;
  return {
    accepted_to_provider_request_ms: fromAccepted(providerRequest),
    provider_request_to_raw_ms: raw.atMs - providerRequest.atMs,
    accepted_to_raw_ms: fromAccepted(raw),
    accepted_to_visible_ms: fromAccepted(visible),
    accepted_to_event_bus_ms: fromAccepted(stages.event_bus_item_published),
    accepted_to_terminal_ms: fromAccepted(stages.canonical_terminal_published),
    magi_first_content_overhead_ms: raw.providerCallId && raw.providerCallId === visible.providerCallId
      ? fromAccepted(visible) - (raw.atMs - providerRequest.atMs)
      : null,
  };
}

function nearestRank(values, percentile) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(percentile * sorted.length) - 1)];
}

function stats(values) {
  const finite = values.filter(Number.isFinite);
  if (finite.length === 0) return { samples: 0, applicable: false };
  return {
    samples: finite.length,
    applicable: true,
    p50_ms: nearestRank(finite, 0.5),
    p95_ms: nearestRank(finite, 0.95),
    max_ms: Math.max(...finite),
  };
}

function sourceKey(source) {
  return JSON.stringify({
    commit: source?.before?.commit || null,
    fingerprint: source?.before?.fingerprintSha256 || null,
    executable: source?.before?.executableSha256 || source?.before?.executable_sha256 || null,
  });
}

function compactionStates(sample) {
  return new Set((sample.compactionEvents || []).map((event) => event.state).filter(Boolean));
}

function validateEvidence(evidence, inputPath) {
  assert(evidence.status === "passed", `${inputPath} status 不是 passed`);
  assert(
    typeof evidence.fixture === "string" && evidence.fixture.startsWith("real-provider-context-compaction-"),
    `${inputPath} fixture 不是上下文压缩专项 fixture`,
  );
  assert(evidence.source?.stableDuringSample === true, `${inputPath} 未确认 source 稳定`);
  assert(evidence.context?.compactionRequired === true, `${inputPath} 未声明 compactionRequired=true`);
  assert(evidence.samples?.length === 40, `${inputPath} 必须包含 40 条 sample（两个场景各 20 条）`);
  for (const scenario of SCENARIOS) {
    const rows = evidence.samples.filter((sample) => sample.scenario === scenario);
    assert(rows.length === 20, `${inputPath} 场景 ${scenario} 必须有 20 条 sample，实际 ${rows.length}`);
    assert(rows.every((sample) => sample.status === "completed"), `${inputPath} 场景 ${scenario} 存在非 completed Turn`);
  }
  const longHistoryRows = evidence.samples.filter((sample) => sample.scenario === "personal_long_history");
  const terminalStates = longHistoryRows.map((sample) => {
    const states = compactionStates(sample);
    assert(
      [...states].every((state) => ["running", "completed", "skipped"].includes(state)),
      `${inputPath} 存在未知压缩状态：${JSON.stringify([...states])}`,
    );
    const terminal = [...states].filter((state) => ["completed", "skipped"].includes(state));
    assert(terminal.length > 0, `${inputPath} 长历史样本缺少 completed/skipped 压缩终态：${sample.turnId}`);
    return terminal.at(-1);
  });
  assert(terminalStates.includes("completed"), `${inputPath} 没有任何 context_compaction=completed 样本`);
  const completed = terminalStates.filter((state) => state === "completed").length;
  const skipped = terminalStates.filter((state) => state === "skipped").length;
  assert(evidence.context.observedCompactionRows === completed,
    `${inputPath} context.observedCompactionRows 与样本不一致`);
  assert(evidence.context.skippedCompactionRows === skipped,
    `${inputPath} context.skippedCompactionRows 与样本不一致`);
  return { longHistoryRows, completed, skipped };
}

async function aggregate({ role, input, output, log }) {
  const evidenceText = await readFile(input, "utf8");
  const evidence = JSON.parse(evidenceText);
  const compaction = validateEvidence(evidence, input);
  const logPath = log || evidence.logPath;
  assert(logPath, `${input} 缺少 daemon log`);
  const logText = await readFile(logPath, "utf8");
  const parsed = parseLog(logText);
  const metrics = evidence.samples.map((sample, index) => {
    const events = [
      ...(parsed.byRequest.get(sample.requestId) || []),
      ...(parsed.byTurn.get(sample.turnId) || []),
    ];
    const values = metricValues(events, sample.turnId || sample.requestId);
    const states = compactionStates(sample);
    return {
      fixture: evidence.fixture,
      comparison_role: role,
      scenario: sample.scenario,
      sample_index: Number.isInteger(sample.sampleIndex) ? sample.sampleIndex : index,
      session_id: sample.sessionId,
      turn_id: sample.turnId,
      request_id: sample.requestId,
      outcome: sample.status,
      context_compaction_state: sample.scenario === "personal_long_history"
        ? [...states].filter((state) => ["completed", "skipped"].includes(state)).at(-1)
        : null,
      ...values,
    };
  });
  const summary = Object.fromEntries(SCENARIOS.map((scenario) => {
    const rows = metrics.filter((metric) => metric.scenario === scenario);
    return [scenario, Object.fromEntries(METRICS.map((metric) => [metric, stats(rows.map((row) => row[metric]))]))];
  }));
  const result = {
    type: "magi_performance_metrics",
    schema_version: SCHEMA_VERSION,
    metric_version: DERIVE_VERSION,
    fixture: evidence.fixture,
    comparison_role: role,
    input_file: basename(input),
    log_file: basename(logPath),
    input_payload_sha256: sha256(evidenceText),
    input_payload_hash: hashJson(evidence),
    daemon_log_sha256: sha256(logText),
    source: evidence.source,
    provider: evidence.provider,
    context: {
      context_window_tokens: evidence.context.contextWindowTokens,
      long_history_chars: evidence.context.longHistoryChars,
      compaction_required: evidence.context.compactionRequired,
      completed_samples: compaction.completed,
      skipped_samples: compaction.skipped,
    },
    scenarios: SCENARIOS,
    sample_count: metrics.length,
    sample_count_per_scenario: 20,
    summary,
    metrics,
    metric_definitions: {
      accepted_to_provider_request_ms: "accepted_response_sent 到 provider_request_started 的 daemon 绝对时间差",
      provider_request_to_raw_ms: "provider_request_started 到 provider_first_raw_delta 的 daemon 绝对时间差",
      accepted_to_raw_ms: "accepted_response_sent 到 provider_first_raw_delta 的 daemon 绝对时间差",
      accepted_to_visible_ms: "accepted_response_sent 到 provider_first_delta 的 daemon 绝对时间差",
      accepted_to_event_bus_ms: "accepted_response_sent 到首个 event_bus_item_published 的 daemon 绝对时间差",
      accepted_to_terminal_ms: "accepted_response_sent 到首个 canonical_terminal_published 的 daemon 绝对时间差",
      magi_first_content_overhead_ms: "仅当首 raw 与首 visible 属于同一 provider_call 时，accepted_response_sent 到首 visible 减去 Provider TTFT",
    },
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

async function compare({ compare, output }) {
  const before = JSON.parse(await readFile(compare[0], "utf8"));
  const after = JSON.parse(await readFile(compare[1], "utf8"));
  assert(before.status === "passed" && after.status === "passed", "压缩 before/after sidecar 必须通过");
  assert(before.schema_version === SCHEMA_VERSION && after.schema_version === SCHEMA_VERSION, "sidecar schema_version 无效");
  assert(before.metric_version === DERIVE_VERSION && after.metric_version === DERIVE_VERSION, "sidecar derive_version 无效");
  assert(before.fixture === after.fixture, "压缩 before/after fixture 不一致");
  assert(JSON.stringify(before.scenarios) === JSON.stringify(SCENARIOS), "before 场景集合无效");
  assert(JSON.stringify(after.scenarios) === JSON.stringify(SCENARIOS), "after 场景集合无效");
  assert(before.sample_count === 40 && after.sample_count === 40, "压缩 before/after 必须各有 40 条 metric");
  assert(before.context?.context_window_tokens === after.context?.context_window_tokens, "context window 不一致");
  assert(sourceKey(before.source) !== sourceKey(after.source), "压缩 before/after source 身份未区分");
  const deltas = Object.fromEntries(SCENARIOS.map((scenario) => [
    scenario,
    Object.fromEntries(METRICS.map((metric) => {
      const beforeStats = before.summary[scenario][metric];
      const afterStats = after.summary[scenario][metric];
      return [metric, beforeStats.applicable && afterStats.applicable
        ? {
          p50_ms: afterStats.p50_ms - beforeStats.p50_ms,
          p95_ms: afterStats.p95_ms - beforeStats.p95_ms,
          max_ms: afterStats.max_ms - beforeStats.max_ms,
        }
        : { applicable: false }];
    })),
  ]));
  const result = {
    type: "magi_context_compaction_performance_before_after",
    schema_version: SCHEMA_VERSION,
    metric_version: DERIVE_VERSION,
    fixture: before.fixture,
    before: {
      input_file: basename(compare[0]),
      input_payload_hash: before.input_payload_hash,
      source: before.source,
      context: before.context,
      summary: before.summary,
    },
    after: {
      input_file: basename(compare[1]),
      input_payload_hash: after.input_payload_hash,
      source: after.source,
      context: after.context,
      summary: after.summary,
    },
    deltas,
    validation: {
      same_fixture: true,
      same_context_window: true,
      same_sample_count_per_scenario: 20,
      absolute_daemon_clock: true,
      distinct_source_identity: true,
    },
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

try {
  const args = parseArgs(process.argv.slice(2));
  const result = args.mode === "aggregate"
    ? await aggregate(args)
    : await compare(args);
  console.log(JSON.stringify({
    status: result.status,
    output: args.output,
    metric_version: result.metric_version,
  }, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
}
