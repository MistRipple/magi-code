#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const EXPECTED_SCENARIOS = [
  "personal_chat",
  "workspace_chat",
  "workspace_tool",
  "goal",
  "subagent",
];
const RENDERER_STAGES = [
  "frontend_event_received",
  "reducer_completed",
  "projection_completed",
  "dom_painted",
];
const BACKEND_METRICS = [
  "accepted_to_provider_request_ms",
  "provider_request_to_raw_ms",
  "accepted_to_raw_ms",
  "accepted_to_visible_ms",
  "accepted_to_event_bus_ms",
  "accepted_to_terminal_ms",
];
const PAIRED_FIXTURE = "electron-dom-timing-paired-v1";
const DERIVE_VERSION = "magi-electron-renderer-derive.v3";

function usage() {
  console.error([
    "聚合 Renderer before/after evidence：",
    "  node scripts/derive-electron-renderer-comparison.mjs --role before|after --output OUTPUT --input INPUT...",
    "比较已聚合 evidence：",
    "  node scripts/derive-electron-renderer-comparison.mjs --compare BEFORE AFTER --output OUTPUT",
  ].join("\n"));
}

function parseArgs(argv) {
  let role = null;
  let output = null;
  let compare = null;
  const inputs = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--role") role = argv[++index];
    else if (value === "--output") output = resolve(argv[++index]);
    else if (value === "--input") inputs.push(resolve(argv[++index]));
    else if (value === "--compare") compare = [resolve(argv[++index]), resolve(argv[++index])];
    else if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    } else {
      throw new Error(`未知参数：${value}`);
    }
  }
  if (!output) throw new Error("必须提供 --output");
  if (compare && (role || inputs.length > 0)) {
    throw new Error("--compare 不能与 --role/--input 同时使用");
  }
  if (compare) return { mode: "compare", output, compare };
  if (!role || !["before", "after"].includes(role) || inputs.length !== EXPECTED_SCENARIOS.length) {
    usage();
    throw new Error(`聚合模式必须提供 role 和 ${EXPECTED_SCENARIOS.length} 个 --input`);
  }
  return { mode: "aggregate", role, output, inputs };
}

async function load(path) {
  return JSON.parse(await readFile(path, "utf8"));
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

async function inputIdentity(path) {
  const payload = await readFile(path);
  return { file: basename(path), sha256: sha256(payload) };
}

function identityKey(identity) {
  return JSON.stringify({
    source_commit: identity?.source_commit || null,
    worktree_fingerprint_sha256: identity?.worktree_fingerprint_sha256 || null,
    executable_sha256: identity?.executable_sha256 || null,
    app_artifact_sha256: identity?.app_artifact_sha256 || null,
    dirty: identity?.dirty ?? null,
    changed_path_count: identity?.changed_path_count ?? null,
  });
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function validateTimingEvidence(evidence, path, expectedScenario) {
  assert(evidence?.status === "passed", `${path} status 不是 passed`);
  assert(evidence?.schema_version === "magi.electron.timing.v1", `${path} schema_version 无效`);
  assert(evidence?.sampleCount === 20, `${path} sampleCount 必须为 20`);
  assert(evidence?.rendererTimingSamples?.length === 20, `${path} 必须有 20 条 Renderer sample`);
  assert(evidence.desktopIpcValidationErrors?.length === 0, `${path} 存在 Desktop IPC 协议错误`);
  assert(evidence.source?.stable === true, `${path} 未确认采样期间 source 稳定`);
  assert(evidence.source.before?.app_artifact_sha256, `${path} 缺少 app_artifact_sha256`);
  assert(evidence.source.after?.app_artifact_sha256, `${path} 缺少 after app_artifact_sha256`);
  assert(identityKey(evidence.source.before) === identityKey(evidence.source.after), `${path} source 身份不稳定`);
  const scenarios = new Set(evidence.rendererTimingSamples.map((sample) => sample.scenario));
  assert(scenarios.size === 1 && scenarios.has(expectedScenario), `${path} 场景不是 ${expectedScenario}`);
  for (const [index, sample] of evidence.rendererTimingSamples.entries()) {
    assert(sample.turnId && sample.sessionId && sample.requestId, `${path} 第 ${index} 条缺少 Turn 身份`);
    assert(sample.backend?.stages?.event_bus_first_event?.first?.sequence !== undefined,
      `${path} 第 ${index} 条缺少 EventBus 序号`);
    assert(sample.backend?.stages?.canonical_terminal_published?.first?.sequence !== undefined,
      `${path} 第 ${index} 条缺少终态序号`);
    for (const stage of RENDERER_STAGES) {
      assert(sample.stages?.[stage]?.count > 0, `${path} 第 ${index} 条缺少 Renderer 阶段 ${stage}`);
      assert(Number.isFinite(sample.stages[stage]?.first?.elapsedMs),
        `${path} 第 ${index} 条阶段 ${stage} 缺少 elapsedMs`);
    }
    assert(sample.stages.dom_painted.count === 1, `${path} 第 ${index} 条 dom_painted 不是唯一记录`);
  }
}

function nearestRank(values, percentile) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(percentile * sorted.length) - 1)];
}

function backendMetricValues(sample, path) {
  const stages = sample.backend?.stages;
  const stage = (name) => stages?.[name]?.first;
  const accepted = stage("accepted_response_sent");
  const providerRequest = stage("provider_request_started");
  const raw = stage("provider_first_raw_delta");
  const visible = stage("provider_first_delta");
  const eventBus = stage("event_bus_first_event");
  const terminal = stage("canonical_terminal_published");
  const required = { accepted, providerRequest, raw, visible, eventBus, terminal };
  assert(Object.values(required).every((value) => value), `${path} sample ${sample.turnId} 缺少跨层后端阶段`);
  assert(sample.backend.sessionId === sample.sessionId, `${path} sample ${sample.turnId} Session identity 不一致`);
  assert(sample.backend.traceId === sample.requestId, `${path} sample ${sample.turnId} request identity 不一致`);
  for (const [name, value] of Object.entries(required)) {
    assert(value.sessionId === sample.sessionId, `${path} sample ${sample.turnId} ${name} Session identity 不一致`);
    assert((value.requestId || value.traceId) === sample.requestId,
      `${path} sample ${sample.turnId} ${name} request identity 不一致`);
    assert(Number.isFinite(value.sinceAcceptedMs) && value.sinceAcceptedMs >= 0,
      `${path} sample ${sample.turnId} ${name} 缺少非负 sinceAcceptedMs`);
  }
  assert(Number.isFinite(eventBus.sequence) && Number.isFinite(terminal.sequence),
    `${path} sample ${sample.turnId} 缺少后端 durable sequence`);
  assert(eventBus.sequence <= terminal.sequence,
    `${path} sample ${sample.turnId} EventBus sequence 晚于 terminal sequence`);
  return {
    accepted_to_provider_request_ms: providerRequest.sinceAcceptedMs - accepted.sinceAcceptedMs,
    provider_request_to_raw_ms: raw.sinceAcceptedMs - providerRequest.sinceAcceptedMs,
    accepted_to_raw_ms: raw.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_visible_ms: visible.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_event_bus_ms: eventBus.sinceAcceptedMs - accepted.sinceAcceptedMs,
    accepted_to_terminal_ms: terminal.sinceAcceptedMs - accepted.sinceAcceptedMs,
  };
}

function backendStats(samples, path) {
  const result = {};
  for (const scenario of EXPECTED_SCENARIOS) {
    const rows = samples.filter((sample) => sample.scenario === scenario);
    assert(rows.length === 20, `${path} 场景 ${scenario} 必须有 20 条跨层样本，实际 ${rows.length}`);
    const values = rows.map((sample) => backendMetricValues(sample, path));
    result[scenario] = { count: rows.length };
    for (const metric of BACKEND_METRICS) {
      const metricValues = values.map((row) => row[metric]);
      assert(metricValues.every((value) => Number.isFinite(value) && value >= 0),
        `${path} 场景 ${scenario} 指标 ${metric} 含无效值`);
      result[scenario][metric] = {
        p50_ms: nearestRank(metricValues, 0.5),
        p95_ms: nearestRank(metricValues, 0.95),
        max_ms: Math.max(...metricValues),
      };
    }
  }
  return result;
}

function rendererStats(samples) {
  const result = {};
  for (const scenario of EXPECTED_SCENARIOS) {
    const rows = samples.filter((sample) => sample.scenario === scenario);
    assert(rows.length === 20, `场景 ${scenario} 必须有 20 条样本，实际 ${rows.length}`);
    result[scenario] = { count: rows.length };
    for (const stage of RENDERER_STAGES) {
      const values = rows.map((sample) => sample.stages[stage].first.elapsedMs);
      result[scenario][stage] = {
        p50_ms: nearestRank(values, 0.5),
        p95_ms: nearestRank(values, 0.95),
        max_ms: Math.max(...values),
      };
    }
  }
  return result;
}

async function aggregate({ role, output, inputs }) {
  const samples = [];
  const sourceIdentities = [];
  const inputIdentities = [];
  let providerRequests = 0;
  let checks = 0;
  for (const [index, path] of inputs.entries()) {
    const evidence = await load(path);
    validateTimingEvidence(evidence, path, EXPECTED_SCENARIOS[index]);
    sourceIdentities.push(evidence.source.before);
    inputIdentities.push(await inputIdentity(path));
    providerRequests += evidence.providerRequests || 0;
    checks += evidence.checks?.length || 0;
    for (const sample of evidence.rendererTimingSamples) {
      samples.push({
        ...sample,
        fixture: PAIRED_FIXTURE,
        comparison_role: role,
      });
    }
  }
  assert(new Set(sourceIdentities.map(identityKey)).size === 1, `${role} 五个场景 source 身份不一致`);
  const sourceIdentity = sourceIdentities[0];
  const artifact = sourceIdentity.app_artifact_sha256;
  const result = {
    type: "electron_conversation_renderer_timing_paired",
    schema_version: "magi.electron.timing.v1",
    derive_version: DERIVE_VERSION,
    fixture: PAIRED_FIXTURE,
    comparison_role: role,
    sampleCount: 20,
    scenarios: EXPECTED_SCENARIOS,
    input_files: inputs.map((path) => basename(path)),
    input_file_sha256: inputIdentities,
    input_payload_hash: hashJson(inputIdentities),
    checks,
    desktopIpcValidationErrors: [],
    source: {
      before: sourceIdentity,
      after: sourceIdentity,
      stable: true,
      app_artifact_sha256: artifact,
    },
    providerRequests,
    rendererTimingSamples: samples,
    backendStats: backendStats(samples, output),
    rendererStats: rendererStats(samples),
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

function validateAggregateEvidence(evidence, path) {
  assert(evidence?.status === "passed", `${path} status 不是 passed`);
  assert(evidence?.schema_version === "magi.electron.timing.v1", `${path} schema_version 无效`);
  assert(evidence?.derive_version === DERIVE_VERSION, `${path} derive_version 无效`);
  assert(evidence?.fixture === PAIRED_FIXTURE, `${path} fixture 无效`);
  assert(evidence?.sampleCount === 20, `${path} sampleCount 必须为 20`);
  assert(evidence?.rendererTimingSamples?.length === 100, `${path} 必须有 100 条跨层 sample`);
  assert(evidence?.source?.stable === true, `${path} source 未稳定`);
  assert(evidence?.input_file_sha256?.length === EXPECTED_SCENARIOS.length,
    `${path} 缺少五个原始输入 hash`);
  assert(evidence.input_payload_hash === hashJson(evidence.input_file_sha256),
    `${path} input_payload_hash 与输入 hash 列表不一致`);
  assert(evidence?.clocks?.additive === false, `${path} 未声明后端与 Renderer 时钟不可相加`);
  const scenarios = new Set(evidence.rendererTimingSamples.map((sample) => sample.scenario));
  assert(scenarios.size === EXPECTED_SCENARIOS.length
    && EXPECTED_SCENARIOS.every((scenario) => scenarios.has(scenario)),
  `${path} 场景集合不完整`);
  for (const sample of evidence.rendererTimingSamples) {
    assert(sample.turnId && sample.sessionId && sample.requestId,
      `${path} 缺少 Turn identity`);
    for (const stage of RENDERER_STAGES) {
      assert(sample.stages?.[stage]?.count > 0 && Number.isFinite(sample.stages[stage].first?.elapsedMs),
        `${path} ${sample.turnId} 缺少 Renderer 阶段 ${stage}`);
    }
    backendMetricValues(sample, path);
  }
  const expectedBackendStats = backendStats(evidence.rendererTimingSamples, path);
  assert(JSON.stringify(expectedBackendStats) === JSON.stringify(evidence.backendStats),
    `${path} backendStats 不是由当前 sample 重算结果`);
  const expectedRendererStats = rendererStats(evidence.rendererTimingSamples);
  assert(JSON.stringify(expectedRendererStats) === JSON.stringify(evidence.rendererStats),
    `${path} rendererStats 不是由当前 sample 重算结果`);
}

async function compare({ output, compare }) {
  const before = await load(compare[0]);
  const after = await load(compare[1]);
  validateAggregateEvidence(before, compare[0]);
  validateAggregateEvidence(after, compare[1]);
  assert(before.fixture === after.fixture, "before/after fixture 不一致");
  assert(JSON.stringify(before.scenarios) === JSON.stringify(after.scenarios), "before/after 场景集合不一致");
  assert(before.rendererTimingSamples.length === 100 && after.rendererTimingSamples.length === 100,
    "before/after 必须各有 100 条 Renderer sample");
  assert(before.source.stable === true && after.source.stable === true, "before/after source 未稳定");
  const beforeSource = before.source.before;
  const afterSource = after.source.before;
  const distinctSourceIdentity = identityKey(beforeSource) !== identityKey(afterSource);
  assert(distinctSourceIdentity, "before/after source 身份未区分");
  assert(before.source.before.app_artifact_sha256 !== after.source.before.app_artifact_sha256,
    "before/after app artifact 相同，不能证明是两个版本");
  assert(before.input_payload_hash !== after.input_payload_hash,
    "before/after input_payload_hash 相同，不能证明是两组独立原始输入");
  assert(before.providerRequests === after.providerRequests, "before/after Provider request 数不一致");
  const deltas = {};
  const backendDeltas = {};
  for (const scenario of EXPECTED_SCENARIOS) {
    deltas[scenario] = {};
    backendDeltas[scenario] = {};
    for (const stage of RENDERER_STAGES) {
      const beforeStats = before.rendererStats[scenario][stage];
      const afterStats = after.rendererStats[scenario][stage];
      deltas[scenario][stage] = {
        p50_ms: afterStats.p50_ms - beforeStats.p50_ms,
        p95_ms: afterStats.p95_ms - beforeStats.p95_ms,
        max_ms: afterStats.max_ms - beforeStats.max_ms,
      };
    }
    for (const metric of BACKEND_METRICS) {
      const beforeStats = before.backendStats[scenario][metric];
      const afterStats = after.backendStats[scenario][metric];
      backendDeltas[scenario][metric] = {
        p50_ms: afterStats.p50_ms - beforeStats.p50_ms,
        p95_ms: afterStats.p95_ms - beforeStats.p95_ms,
        max_ms: afterStats.max_ms - beforeStats.max_ms,
      };
    }
  }
  const result = {
    type: "electron_conversation_renderer_before_after",
    schema_version: "magi.electron.timing.v1",
    derive_version: DERIVE_VERSION,
    fixture: before.fixture,
    sampleCount: 20,
    scenarios: EXPECTED_SCENARIOS,
    before: {
      input_file: basename(compare[0]),
      input_payload_hash: before.input_payload_hash,
      source: before.source,
      providerRequests: before.providerRequests,
      backendStats: before.backendStats,
      rendererStats: before.rendererStats,
    },
    after: {
      input_file: basename(compare[1]),
      input_payload_hash: after.input_payload_hash,
      source: after.source,
      providerRequests: after.providerRequests,
      backendStats: after.backendStats,
      rendererStats: after.rendererStats,
    },
    deltas,
    backendDeltas,
    validation: {
      same_fixture: true,
      distinct_app_artifact: true,
      distinct_input_payload_hash: true,
      same_provider_request_count: true,
      sample_count_per_scenario: 20,
      nearest_rank: true,
      backend_stage_contract: true,
      backend_and_renderer_same_turn: true,
      same_source_commit: beforeSource.source_commit === afterSource.source_commit,
      same_worktree_fingerprint:
        beforeSource.worktree_fingerprint_sha256 === afterSource.worktree_fingerprint_sha256,
      distinct_source_identity: distinctSourceIdentity,
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
    sampleCount: result.sampleCount,
    scenarios: result.scenarios,
  }, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
}
