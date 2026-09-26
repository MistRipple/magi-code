#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const SCENARIO = "personal_long_history";
const BACKEND_METRICS = [
  "accepted_to_provider_request_ms",
  "provider_request_to_raw_ms",
  "accepted_to_raw_ms",
  "accepted_to_visible_ms",
  "accepted_to_event_bus_ms",
  "accepted_to_terminal_ms",
];
const RENDERER_STAGES = [
  "frontend_event_received",
  "reducer_completed",
  "projection_completed",
  "dom_painted",
];
const SCHEMA_VERSION = "magi.performance.v1";
const EVIDENCE_SCHEMA_VERSION = "magi.electron.context-compaction.v1";
const DERIVE_VERSION = "magi-electron-context-compaction-metrics.v1";
const FIXTURE = "electron-dom-context-compaction-v1";

function usage() {
  console.error([
    "派生 Electron 上下文压缩专项性能 sidecar：",
    "  node scripts/derive-electron-context-compaction-metrics.mjs --role before|after --input EVIDENCE --output OUTPUT",
    "比较两个已通过的专项 sidecar：",
    "  node scripts/derive-electron-context-compaction-metrics.mjs --compare BEFORE AFTER --output OUTPUT",
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
    if (value === "--role" || value === "--input" || value === "--output") {
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
    if (values.role || values.input || !values.output) {
      throw new Error("--compare 不能与 --role/--input 混用，并且必须提供 --output");
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

function nearestRank(values, percentile) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(percentile * sorted.length) - 1)];
}

function stats(values) {
  const finite = values.filter(Number.isFinite);
  assert(finite.length > 0, "指标没有可计算样本");
  return {
    samples: finite.length,
    p50_ms: nearestRank(finite, 0.5),
    p95_ms: nearestRank(finite, 0.95),
    max_ms: Math.max(...finite),
  };
}

function sourceIdentity(source) {
  return source?.before || null;
}

function sourceKey(source) {
  const identity = sourceIdentity(source);
  return JSON.stringify({
    source_commit: identity?.source_commit || null,
    worktree_fingerprint_sha256: identity?.worktree_fingerprint_sha256 || null,
    executable_sha256: identity?.executable_sha256 || null,
    app_artifact_sha256: identity?.app_artifact_sha256 || null,
  });
}

function backendStage(sample, name, path) {
  const stage = sample.backend?.stages?.[name];
  assert(stage?.count > 0 && stage.first, `${path} 缺少后端阶段 ${name}`);
  assert(Number.isFinite(stage.first.sinceAcceptedMs) && stage.first.sinceAcceptedMs >= 0,
    `${path} 后端阶段 ${name} 缺少 sinceAcceptedMs`);
  return stage.first;
}

function backendMetricValues(sample, path) {
  const accepted = backendStage(sample, "accepted_response_sent", path);
  const providerRequest = backendStage(sample, "provider_request_started", path);
  const raw = backendStage(sample, "provider_first_raw_delta", path);
  const visible = backendStage(sample, "provider_first_delta", path);
  const eventBus = backendStage(sample, "event_bus_first_event", path);
  const terminal = backendStage(sample, "terminal", path);
  assert(sample.backend.sessionId === sample.sessionId, `${path} Session identity 不一致`);
  assert(sample.backend.traceId === sample.requestId, `${path} request identity 不一致`);
  for (const [name, stage] of Object.entries({ accepted, providerRequest, raw, visible, eventBus, terminal })) {
    assert((stage.requestId || stage.traceId) === sample.requestId,
      `${path} ${name} request identity 不一致`);
    assert(stage.sessionId === sample.sessionId, `${path} ${name} Session identity 不一致`);
  }
  assert(Number.isSafeInteger(eventBus.sequence) && Number.isSafeInteger(terminal.sequence),
    `${path} 缺少 EventBus/canonical terminal durable sequence`);
  assert(eventBus.sequence < terminal.sequence, `${path} terminal sequence 未晚于 EventBus sequence`);
  return {
    accepted_to_provider_request_ms: providerRequest.sinceAcceptedMs - accepted.sinceAcceptedMs,
    provider_request_to_raw_ms: raw.sinceAcceptedMs - providerRequest.sinceAcceptedMs,
    accepted_to_raw_ms: raw.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_visible_ms: visible.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_event_bus_ms: eventBus.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_terminal_ms: terminal.sinceAcceptedMs - accepted.sinceAcceptedMs,
  };
}

function rendererMetricValues(sample, path) {
  const result = {};
  for (const stage of RENDERER_STAGES) {
    const value = sample.stages?.[stage]?.first?.elapsedMs;
    assert(sample.stages?.[stage]?.count > 0 && Number.isFinite(value) && value >= 0,
      `${path} 缺少 Renderer 阶段 ${stage}`);
    result[stage] = value;
  }
  assert(sample.stages.dom_painted.count === 1, `${path} dom_painted 不是唯一记录`);
  return result;
}

function compactionState(sample, path) {
  const events = sample.compactionEvents || [];
  assert(events.length > 0, `${path} 缺少 context_compaction 事件`);
  let previousSequence = 0;
  const states = [];
  for (const event of events) {
    assert(Number.isSafeInteger(event.sequence) && event.sequence > previousSequence,
      `${path} context_compaction event sequence 不连续或无效`);
    previousSequence = event.sequence;
    assert(["running", "completed", "skipped"].includes(event.state),
      `${path} 存在未知 context_compaction 状态：${event.state}`);
    states.push(event.state);
  }
  const terminalStates = states.filter((state) => ["completed", "skipped"].includes(state));
  assert(terminalStates.length > 0, `${path} 缺少 completed/skipped 压缩终态`);
  return terminalStates.at(-1);
}

function validateEvidence(evidence, path) {
  assert(evidence?.status === "passed", `${path} status 不是 passed`);
  assert(evidence?.schema_version === EVIDENCE_SCHEMA_VERSION, `${path} schema_version 无效`);
  assert(evidence?.fixture === FIXTURE, `${path} fixture 无效`);
  assert(evidence?.sampleCount === 20, `${path} sampleCount 必须为 20`);
  assert(evidence?.samples?.length === 20, `${path} 必须有 20 条 sample`);
  assert(JSON.stringify(evidence.scenarios) === JSON.stringify([SCENARIO]), `${path} 场景集合无效`);
  assert(evidence.source?.stable === true, `${path} source 未确认稳定`);
  assert(sourceIdentity(evidence.source)?.app_artifact_sha256, `${path} 缺少 app_artifact_sha256`);
  assert(evidence.context?.compactionRequired === true, `${path} 未声明 compactionRequired=true`);
  assert(Number.isInteger(evidence.context?.contextWindowTokens)
    && evidence.context.contextWindowTokens >= 16_000, `${path} context window 无效`);
  assert(Number.isInteger(evidence.context?.longHistoryChars)
    && evidence.context.longHistoryChars >= 1_000, `${path} long history 长度无效`);
  let completed = 0;
  let skipped = 0;
  for (const [index, sample] of evidence.samples.entries()) {
    const samplePath = `${path} 第 ${index} 条 sample`;
    assert(sample.schema_version === EVIDENCE_SCHEMA_VERSION, `${samplePath} schema_version 无效`);
    assert(sample.scenario === SCENARIO && sample.outcome === "completed", `${samplePath} outcome/scenario 无效`);
    assert(sample.sessionId && sample.turnId && sample.requestId && sample.historySeedTurnId,
      `${samplePath} 缺少 Session/Turn/request/history seed 身份`);
    assert(sample.historySeedTurnId !== sample.turnId, `${samplePath} seed Turn 与压缩 Turn 相同`);
    assert(sample.contextWindowTokens === evidence.context.contextWindowTokens,
      `${samplePath} context window 与 evidence 不一致`);
    assert(sample.longHistoryChars === evidence.context.longHistoryChars,
      `${samplePath} long history 长度与 evidence 不一致`);
    const terminal = compactionState(sample, samplePath);
    if (terminal === "completed") completed += 1;
    if (terminal === "skipped") skipped += 1;
    backendMetricValues(sample, samplePath);
    rendererMetricValues(sample, samplePath);
  }
  assert(completed > 0, `${path} 没有任何 context_compaction=completed sample`);
  assert(evidence.context.observedCompactionRows === completed,
    `${path} observedCompactionRows 与 samples 不一致`);
  assert(evidence.context.skippedCompactionRows === skipped,
    `${path} skippedCompactionRows 与 samples 不一致`);
  return { completed, skipped };
}

function buildSummary(metrics) {
  const backend = Object.fromEntries(BACKEND_METRICS.map((metric) => [
    metric,
    stats(metrics.map((sample) => sample.backendMetrics[metric])),
  ]));
  const renderer = Object.fromEntries(RENDERER_STAGES.map((stage) => [
    stage,
    stats(metrics.map((sample) => sample.rendererMetrics[stage])),
  ]));
  return { backend, renderer };
}

async function aggregate({ role, input, output }) {
  const inputText = await readFile(input, "utf8");
  const evidence = JSON.parse(inputText);
  const compaction = validateEvidence(evidence, input);
  const metrics = evidence.samples.map((sample, index) => ({
    fixture: evidence.fixture,
    comparison_role: role,
    scenario: sample.scenario,
    sample_index: index,
    session_id: sample.sessionId,
    turn_id: sample.turnId,
    request_id: sample.requestId,
    history_seed_turn_id: sample.historySeedTurnId,
    outcome: sample.outcome,
    context_compaction_state: compactionState(sample, `${input} 第 ${index} 条 sample`),
    backendMetrics: backendMetricValues(sample, `${input} 第 ${index} 条 sample`),
    rendererMetrics: rendererMetricValues(sample, `${input} 第 ${index} 条 sample`),
  }));
  const result = {
    type: "magi_performance_metrics",
    schema_version: SCHEMA_VERSION,
    metric_version: DERIVE_VERSION,
    fixture: evidence.fixture,
    comparison_role: role,
    input_file: basename(input),
    input_payload_sha256: sha256(inputText),
    input_payload_hash: hashJson(evidence),
    source: evidence.source,
    context: {
      context_window_tokens: evidence.context.contextWindowTokens,
      long_history_chars: evidence.context.longHistoryChars,
      compaction_required: evidence.context.compactionRequired,
      completed_samples: compaction.completed,
      skipped_samples: compaction.skipped,
    },
    scenarios: [SCENARIO],
    sample_count: metrics.length,
    sample_count_per_scenario: 20,
    summary: { [SCENARIO]: buildSummary(metrics) },
    metrics,
    metric_definitions: {
      accepted_to_provider_request_ms: "daemon sinceAcceptedMs：accepted_response_sent 到 provider_request_started",
      provider_request_to_raw_ms: "daemon sinceAcceptedMs：provider_request_started 到 provider_first_raw_delta",
      accepted_to_raw_ms: "daemon sinceAcceptedMs：accepted_response_sent 到 provider_first_raw_delta",
      accepted_to_visible_ms: "daemon sinceAcceptedMs：accepted_response_sent 到 provider_first_delta",
      accepted_to_event_bus_ms: "daemon sinceAcceptedMs：accepted_response_sent 到 event_bus_first_event",
      accepted_to_terminal_ms: "daemon sinceAcceptedMs：accepted_response_sent 到 canonical terminal",
      frontend_event_received: "Renderer timing registry elapsedMs",
      reducer_completed: "Renderer timing registry elapsedMs",
      projection_completed: "Renderer timing registry elapsedMs",
      dom_painted: "Renderer timing registry elapsedMs",
    },
    clocks: {
      backend: "daemon absolute timestamp expressed as sinceAcceptedMs",
      renderer: "page-local timing registry elapsedMs",
      additive: false,
    },
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

function metricDelta(before, after, path) {
  const left = before.summary[SCENARIO];
  const right = after.summary[SCENARIO];
  const delta = {};
  for (const metric of BACKEND_METRICS) {
    delta[`backend.${metric}`] = {
      p50_ms: right.backend[metric].p50_ms - left.backend[metric].p50_ms,
      p95_ms: right.backend[metric].p95_ms - left.backend[metric].p95_ms,
      max_ms: right.backend[metric].max_ms - left.backend[metric].max_ms,
    };
  }
  for (const stage of RENDERER_STAGES) {
    delta[`renderer.${stage}`] = {
      p50_ms: right.renderer[stage].p50_ms - left.renderer[stage].p50_ms,
      p95_ms: right.renderer[stage].p95_ms - left.renderer[stage].p95_ms,
      max_ms: right.renderer[stage].max_ms - left.renderer[stage].max_ms,
    };
  }
  assert(Object.keys(delta).length === BACKEND_METRICS.length + RENDERER_STAGES.length,
    `${path} comparison metric 数量错误`);
  return delta;
}

async function compare({ compare, output }) {
  const before = JSON.parse(await readFile(compare[0], "utf8"));
  const after = JSON.parse(await readFile(compare[1], "utf8"));
  assert(before.status === "passed" && after.status === "passed", "压缩 before/after sidecar 必须通过");
  assert(before.schema_version === SCHEMA_VERSION && after.schema_version === SCHEMA_VERSION,
    "sidecar schema_version 无效");
  assert(before.metric_version === DERIVE_VERSION && after.metric_version === DERIVE_VERSION,
    "sidecar derive_version 无效");
  assert(before.fixture === after.fixture && before.fixture === FIXTURE, "压缩 before/after fixture 不一致");
  assert(JSON.stringify(before.scenarios) === JSON.stringify([SCENARIO]), "before 场景集合无效");
  assert(JSON.stringify(after.scenarios) === JSON.stringify([SCENARIO]), "after 场景集合无效");
  assert(before.sample_count === 20 && after.sample_count === 20, "压缩 before/after 必须各有 20 条 metric");
  assert(before.context.context_window_tokens === after.context.context_window_tokens,
    "压缩 before/after context window 不一致");
  assert(before.context.long_history_chars === after.context.long_history_chars,
    "压缩 before/after long history 长度不一致");
  assert(sourceKey(before.source) !== sourceKey(after.source),
    "压缩 before/after source 身份未区分");
  const beforeIdentity = sourceIdentity(before.source);
  const afterIdentity = sourceIdentity(after.source);
  assert(beforeIdentity.source_commit === afterIdentity.source_commit,
    "压缩 before/after source commit 不一致");
  assert(beforeIdentity.worktree_fingerprint_sha256 === afterIdentity.worktree_fingerprint_sha256,
    "压缩 before/after worktree fingerprint 不一致");
  assert(before.input_payload_hash !== after.input_payload_hash,
    "压缩 before/after input payload hash 相同");
  const result = {
    type: "magi_electron_context_compaction_performance_before_after",
    schema_version: SCHEMA_VERSION,
    metric_version: DERIVE_VERSION,
    fixture: before.fixture,
    scenarios: [SCENARIO],
    before: {
      input_file: before.input_file,
      input_payload_hash: before.input_payload_hash,
      source: before.source,
      context: before.context,
      summary: before.summary,
    },
    after: {
      input_file: after.input_file,
      input_payload_hash: after.input_payload_hash,
      source: after.source,
      context: after.context,
      summary: after.summary,
    },
    deltas: metricDelta(before, after, "Electron context compaction comparison"),
    validation: {
      same_fixture: true,
      same_context_window: true,
      same_long_history_chars: true,
      same_sample_count_per_scenario: 20,
      same_source_commit: true,
      same_worktree_fingerprint: true,
      distinct_source_identity: true,
      distinct_input_payload_hash: true,
      separate_non_additive_clocks: true,
    },
    status: "passed",
  };
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  return result;
}

try {
  const args = parseArgs(process.argv.slice(2));
  const result = args.mode === "aggregate" ? await aggregate(args) : await compare(args);
  console.log(JSON.stringify({
    status: result.status,
    output: args.output,
    fixture: result.fixture,
    metric_version: result.metric_version,
  }, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
}
