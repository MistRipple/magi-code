#!/usr/bin/env node

import { readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { basename, resolve } from "node:path";

const SCHEMA_VERSION = "magi.trajectory.v1";
const DERIVE_VERSION = "magi-trajectory-derive.v1";
const PHASES = new Set([
  "accepted",
  "provider_request",
  "provider_raw_delta",
  "provider_visible_delta",
  "tool_call",
  "tool_result",
  "approval_requested",
  "approval_resolved",
  "event_bus",
  "canonical_terminal",
  "renderer_received",
  "reducer_completed",
  "projection_completed",
  "dom_painted",
]);

function usage() {
  console.error(
    "用法：node scripts/derive-magi-trajectory-ledger.mjs --output OUTPUT INPUT...",
  );
}

function parseArgs(argv) {
  let output = null;
  const inputs = [];
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--output") {
      output = argv[index + 1];
      index += 1;
    } else if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    } else {
      inputs.push(value);
    }
  }
  if (!output || inputs.length === 0) {
    usage();
    throw new Error("必须提供 --output 和至少一个 JSON 输入文件");
  }
  return { output: resolve(output), inputs: inputs.map((input) => resolve(input)) };
}

function stableValue(value) {
  if (Array.isArray(value)) return value.map(stableValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, stableValue(value[key])]),
    );
  }
  return value;
}

function hashJson(value) {
  return createHash("sha256")
    .update(JSON.stringify(stableValue(value)))
    .digest("hex");
}

function firstStageValue(stage) {
  if (stage === null || stage === undefined) return null;
  if (typeof stage === "number") return Number.isFinite(stage) ? stage : null;
  if (typeof stage !== "object") return null;
  const first = stage.first || stage;
  for (const key of ["sinceAcceptedMs", "elapsedMs", "timestampMs", "atMs"]) {
    if (Number.isFinite(first?.[key])) return first[key];
  }
  return null;
}

function firstSequence(stage, fallback = null) {
  if (stage && typeof stage === "object") {
    const first = stage.first || stage;
    if (Number.isInteger(first?.sequence)) return first.sequence;
  }
  return Number.isInteger(fallback) ? fallback : null;
}

function firstProviderRound(stage, fallback = null) {
  if (stage && typeof stage === "object") {
    const first = stage.first || stage;
    if (Number.isInteger(first?.providerRound)) return first.providerRound;
  }
  return Number.isInteger(fallback) ? fallback : null;
}

function makeRecord({
  input,
  inputPath,
  source,
  scenario,
  sampleIndex,
  querySource,
  sessionId,
  turnId,
  requestId,
  eventSequence,
  phase,
  timestampMs,
  providerRound,
  toolCallCount,
  outcome,
  payload,
}) {
  if (!PHASES.has(phase)) throw new Error(`未知 trajectory phase：${phase}`);
  const record = {
    schema_version: SCHEMA_VERSION,
    scenario,
    sample_index: sampleIndex,
    query_source: querySource,
    session_id: sessionId,
    turn_id: turnId,
    request_id: requestId,
    event_sequence: eventSequence,
    phase,
    timestamp_ms: timestampMs,
    provider_round: providerRound,
    tool_call_count: toolCallCount,
    payload_hash: hashJson(payload),
    source,
    outcome,
    input_file: basename(inputPath),
  };
  if (input?.fixture) record.fixture = input.fixture;
  return record;
}

function stageFrom(sample, name) {
  const backendStage = sample.backendStages?.[name];
  if (backendStage !== undefined) {
    return Array.isArray(backendStage) ? backendStage[0] ?? null : backendStage;
  }
  return sample.backend?.stages?.[name]
    ?? sample.stages?.[name]
    ?? null;
}

function addStage(records, options) {
  if (options.timestampMs === null || options.timestampMs === undefined) return;
  records.push(makeRecord(options));
}

function realProviderRecords(input, inputPath) {
  const records = [];
  for (const [index, sample] of (input.samples || []).entries()) {
    const sessionId = sample.sessionId || null;
    const turnId = sample.turnId || null;
    const requestId = sample.requestId || null;
    const common = {
      input,
      inputPath,
      source: "real_provider",
      scenario: sample.scenario || "unknown",
      sampleIndex: Number.isInteger(sample.sampleIndex) ? sample.sampleIndex : index,
      querySource: sample.querySource || "main_turn",
      sessionId,
      turnId,
      requestId,
      toolCallCount: Number.isInteger(sample.toolCallCount) ? sample.toolCallCount : null,
      outcome: sample.status || input.status || "unknown",
    };
    const backend = sample.backendStages || {};
    addStage(records, { ...common, phase: "accepted", timestampMs: sample.acceptedMs, eventSequence: null, payload: { acceptedMs: sample.acceptedMs } });
    addStage(records, { ...common, phase: "provider_request", timestampMs: firstStageValue(stageFrom(sample, "provider_request_started")), eventSequence: firstSequence(stageFrom(sample, "provider_request_started")), providerRound: firstProviderRound(stageFrom(sample, "provider_request_started")), payload: backend.provider_request_started || null });
    addStage(records, { ...common, phase: "provider_raw_delta", timestampMs: sample.providerFirstRawDeltaMs ?? firstStageValue(stageFrom(sample, "provider_first_raw_delta")), eventSequence: firstSequence(stageFrom(sample, "provider_first_raw_delta")), providerRound: firstProviderRound(stageFrom(sample, "provider_first_raw_delta")), payload: backend.provider_first_raw_delta || { value: sample.providerFirstRawDeltaMs } });
    addStage(records, { ...common, phase: "provider_visible_delta", timestampMs: sample.providerFirstDeltaMs ?? firstStageValue(stageFrom(sample, "provider_first_delta")), eventSequence: firstSequence(stageFrom(sample, "provider_first_delta")), providerRound: firstProviderRound(stageFrom(sample, "provider_first_delta")), payload: backend.provider_first_delta || { value: sample.providerFirstDeltaMs } });
    addStage(records, { ...common, phase: "event_bus", timestampMs: sample.firstEventMs ?? firstStageValue(stageFrom(sample, "event_bus_first_event")), eventSequence: sample.firstEventSequence ?? firstSequence(stageFrom(sample, "event_bus_first_event")), payload: backend.event_bus_first_event || { value: sample.firstEventMs } });
    addStage(records, { ...common, phase: "canonical_terminal", timestampMs: sample.terminalMs ?? firstStageValue(stageFrom(sample, "canonical_terminal_published")), eventSequence: sample.terminalSequence ?? firstSequence(stageFrom(sample, "canonical_terminal_published")), payload: backend.canonical_terminal_published || { value: sample.terminalMs, status: sample.status } });
  }
  return records;
}

function electronRecords(input, inputPath) {
  const records = [];
  for (const [index, sample] of (input.rendererTimingSamples || []).entries()) {
    const backend = sample.backend || {};
    const backendStages = backend.stages || {};
    const sessionId = sample.sessionId || backend.sessionId || null;
    const turnId = sample.turnId || null;
    const requestId = sample.requestId || backend.traceId || null;
    const common = {
      input,
      inputPath,
      source: "electron_renderer",
      scenario: sample.scenario || "unknown",
      sampleIndex: Number.isInteger(sample.sampleIndex) ? sample.sampleIndex : index,
      querySource: sample.querySource || "main_turn",
      sessionId,
      turnId,
      requestId,
      toolCallCount: Number.isInteger(sample.toolCallCount) ? sample.toolCallCount : null,
      outcome: sample.outcome || input.status || "unknown",
    };
    const backendPhaseMap = [
      ["accepted_response_sent", "accepted"],
      ["provider_request_started", "provider_request"],
      ["provider_first_raw_delta", "provider_raw_delta"],
      ["provider_first_delta", "provider_visible_delta"],
      ["event_bus_first_event", "event_bus"],
      ["canonical_terminal_published", "canonical_terminal"],
    ];
    for (const [stageName, phase] of backendPhaseMap) {
      const stage = backendStages[stageName];
      addStage(records, {
        ...common,
        phase,
        timestampMs: firstStageValue(stage),
        eventSequence: firstSequence(stage),
        providerRound: firstProviderRound(stage),
        payload: stage || null,
      });
    }
    const rendererPhaseMap = [
      ["frontend_event_received", "renderer_received"],
      ["reducer_completed", "reducer_completed"],
      ["projection_completed", "projection_completed"],
      ["dom_painted", "dom_painted"],
    ];
    for (const [stageName, phase] of rendererPhaseMap) {
      const stage = sample.stages?.[stageName];
      addStage(records, {
        ...common,
        phase,
        timestampMs: firstStageValue(stage),
        eventSequence: firstSequence(stage),
        payload: stage || null,
      });
    }
  }
  return records;
}

function validateRecords(records) {
  const errors = [];
  const byTurn = new Map();
  for (const record of records) {
    if (!record.turn_id || !record.request_id || !record.session_id) {
      errors.push(`缺少身份：${record.source}/${record.scenario}/${record.phase}`);
    }
    if (!Number.isInteger(record.sample_index) || record.sample_index < 0) {
      errors.push(`sample_index 无效：${record.scenario}/${record.phase}`);
    }
    if (!byTurn.has(record.turn_id)) byTurn.set(record.turn_id, []);
    byTurn.get(record.turn_id).push(record);
    if (["event_bus", "canonical_terminal"].includes(record.phase) && record.event_sequence === null) {
      errors.push(`缺少 event_sequence：${record.turn_id}/${record.phase}`);
    }
  }
  for (const [turnId, turnRecords] of byTurn) {
    const terminals = turnRecords.filter((record) => record.phase === "canonical_terminal");
    if (terminals.length > 1) errors.push(`同一 turn_id 存在多个 canonical terminal：${turnId}`);
    const timed = turnRecords
      .filter((record) => Number.isFinite(record.timestamp_ms))
      .sort((left, right) => left.timestamp_ms - right.timestamp_ms);
    for (let index = 1; index < timed.length; index += 1) {
      if (timed[index].timestamp_ms < timed[index - 1].timestamp_ms) {
        errors.push(`阶段时间线倒退：${turnId}`);
        break;
      }
    }
  }
  return errors;
}

async function main() {
  const { output, inputs } = parseArgs(process.argv.slice(2));
  const allRecords = [];
  const inputMetadata = [];
  for (const inputPath of inputs) {
    const raw = await readFile(inputPath, "utf8");
    const input = JSON.parse(raw);
    inputMetadata.push({
      path: inputPath,
      payload_hash: hashJson(input),
      status: input.status || "unknown",
    });
    if (Array.isArray(input.samples)) allRecords.push(...realProviderRecords(input, inputPath));
    if (Array.isArray(input.rendererTimingSamples)) allRecords.push(...electronRecords(input, inputPath));
  }
  const errors = validateRecords(allRecords);
  const phaseCounts = Object.fromEntries(
    [...new Set(allRecords.map((record) => record.phase))]
      .sort()
      .map((phase) => [phase, allRecords.filter((record) => record.phase === phase).length]),
  );
  const ledger = {
    schema_version: SCHEMA_VERSION,
    derive_version: DERIVE_VERSION,
    generated_at: new Date().toISOString(),
    status: errors.length === 0 ? "passed" : "incomplete",
    inputs: inputMetadata,
    record_count: allRecords.length,
    phase_counts: phaseCounts,
    validation: { errors },
    records: allRecords,
  };
  await writeFile(output, `${JSON.stringify(ledger, null, 2)}\n`, "utf8");
  console.log(JSON.stringify({
    status: ledger.status,
    output,
    recordCount: ledger.record_count,
    phaseCounts,
    validationErrors: errors.length,
  }, null, 2));
  if (errors.length > 0) process.exitCode = 2;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
