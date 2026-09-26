#!/usr/bin/env node

import { readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { basename, resolve } from "node:path";

const SCHEMA_VERSION = "magi.trajectory.v1";
const DERIVE_VERSION = "magi-trajectory-derive.v5";
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
  providerRequestCount,
  providerDispatch,
  providerBlockReason,
  trajectoryMode,
  expectedOutcome,
  inputHash,
  settlement,
  settlementRequired,
  outcome,
  payload,
  fixture,
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
    provider_request_count: providerRequestCount,
    payload_hash: hashJson(payload),
    source,
    outcome,
    input_file: basename(inputPath),
    input_status: input?.status || "unknown",
  };
  if (trajectoryMode) record.trajectory_mode = trajectoryMode;
  if (expectedOutcome) record.expected_outcome = expectedOutcome;
  if (inputHash) record.input_hash = inputHash;
  if (providerDispatch) record.provider_dispatch = providerDispatch;
  if (providerBlockReason) record.provider_block_reason = providerBlockReason;
  if (settlement) record.settlement = settlement;
  if (settlementRequired === true) record.settlement_required = true;
  record.fixture = fixture
    || input?.fixture
    || input?.daemon?.runtimeEpoch
    || basename(inputPath);
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
      providerRequestCount: Number.isInteger(sample.providerRequestCount)
        ? sample.providerRequestCount
        : null,
      providerDispatch: sample.providerDispatch || input.providerDispatch || null,
      providerBlockReason: sample.providerBlockReason || input.providerBlockReason || null,
      trajectoryMode: sample.trajectoryMode || input.trajectoryMode || null,
      expectedOutcome: sample.expectedOutcome || input.expectedOutcome || null,
      inputHash: sample.inputHash || null,
      settlementRequired: sample.settlementRequired === true || input.settlement_required === true,
      outcome: sample.outcome || sample.terminalStatus || sample.status || input.status || "unknown",
      fixture: sample.fixture || input.fixture || input.daemon?.runtimeEpoch || basename(inputPath),
    };
    const backend = sample.backendStages || {};
    addStage(records, { ...common, phase: "accepted", timestampMs: sample.acceptedMs, eventSequence: null, payload: { acceptedMs: sample.acceptedMs } });
    addStage(records, { ...common, phase: "provider_request", timestampMs: firstStageValue(stageFrom(sample, "provider_request_started")), eventSequence: firstSequence(stageFrom(sample, "provider_request_started")), providerRound: firstProviderRound(stageFrom(sample, "provider_request_started")), payload: backend.provider_request_started || null });
    addStage(records, { ...common, phase: "provider_raw_delta", timestampMs: sample.providerFirstRawDeltaMs ?? firstStageValue(stageFrom(sample, "provider_first_raw_delta")), eventSequence: firstSequence(stageFrom(sample, "provider_first_raw_delta")), providerRound: firstProviderRound(stageFrom(sample, "provider_first_raw_delta")), payload: backend.provider_first_raw_delta || { value: sample.providerFirstRawDeltaMs } });
    addStage(records, { ...common, phase: "provider_visible_delta", timestampMs: sample.providerFirstDeltaMs ?? firstStageValue(stageFrom(sample, "provider_first_delta")), eventSequence: firstSequence(stageFrom(sample, "provider_first_delta")), providerRound: firstProviderRound(stageFrom(sample, "provider_first_delta")), payload: backend.provider_first_delta || { value: sample.providerFirstDeltaMs } });
    addStage(records, { ...common, phase: "event_bus", timestampMs: sample.firstEventMs ?? firstStageValue(stageFrom(sample, "event_bus_first_event")), eventSequence: sample.firstEventSequence ?? firstSequence(stageFrom(sample, "event_bus_first_event")), payload: backend.event_bus_first_event || { value: sample.firstEventMs } });
    addStage(records, { ...common, phase: "canonical_terminal", timestampMs: sample.terminalMs ?? firstStageValue(stageFrom(sample, "canonical_terminal_published")), eventSequence: sample.terminalSequence ?? firstSequence(stageFrom(sample, "canonical_terminal_published")), settlement: sample.settlement || null, payload: backend.canonical_terminal_published || { value: sample.terminalMs, status: sample.terminalStatus || sample.status } });
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
      outcome: sample.outcome || sample.terminalStatus || sample.status || input.status || "unknown",
      trajectoryMode: sample.trajectoryMode || input.trajectoryMode || null,
      expectedOutcome: sample.expectedOutcome || input.expectedOutcome || null,
      inputHash: sample.inputHash || null,
      providerDispatch: sample.providerDispatch || input.providerDispatch || null,
      providerBlockReason: sample.providerBlockReason || input.providerBlockReason || null,
      settlement: sample.settlement || null,
      settlementRequired: sample.settlementRequired === true || input.settlement_required === true,
      fixture: sample.fixture || input.fixture || input.daemon?.runtimeEpoch || basename(inputPath),
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
        source: "electron_log",
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
    if (record.schema_version !== SCHEMA_VERSION) {
      errors.push(`schema_version 无效：${record.source}/${record.scenario}/${record.phase}`);
    }
    if (!record.fixture || !record.turn_id || !record.request_id || !record.session_id) {
      errors.push(`缺少身份：${record.source}/${record.scenario}/${record.phase}`);
    }
    if (!Number.isInteger(record.sample_index) || record.sample_index < 0) {
      errors.push(`sample_index 无效：${record.scenario}/${record.phase}`);
    }
    if (!Number.isFinite(record.timestamp_ms)) {
      errors.push(`timestamp_ms 无效：${record.fixture}/${record.turn_id}/${record.phase}`);
    }
    if (record.expected_outcome && record.expected_outcome !== record.outcome) {
      errors.push(`expected_outcome 与实际 outcome 不一致：${record.fixture}/${record.turn_id}`);
    }
    if (record.settlement_required === true && !record.input_hash) {
      errors.push(`settlement-required 样本缺少 input_hash：${record.fixture}/${record.turn_id}`);
    }
    if (record.event_sequence !== null
      && (!Number.isInteger(record.event_sequence) || record.event_sequence < 0)) {
      errors.push(`event_sequence 无效：${record.fixture}/${record.turn_id}/${record.phase}`);
    }
    const turnKey = `${record.fixture}\u0000${record.turn_id}`;
    if (!byTurn.has(turnKey)) byTurn.set(turnKey, []);
    byTurn.get(turnKey).push(record);
    if (["event_bus", "canonical_terminal"].includes(record.phase) && record.event_sequence === null) {
      errors.push(`缺少 event_sequence：${record.turn_id}/${record.phase}`);
    }
  }
  for (const [turnKey, turnRecords] of byTurn) {
    const phaseCounts = new Map();
    for (const record of turnRecords) {
      phaseCounts.set(record.phase, (phaseCounts.get(record.phase) || 0) + 1);
    }
    const outcome = turnRecords[0]?.outcome || "unknown";
    const abnormalOutcome = new Set(["failed", "cancelled", "blocked"]).has(outcome);
    const providerDispatch = new Set(
      turnRecords.map((record) => record.provider_dispatch).filter(Boolean),
    );
    const providerBlockReasons = new Set(
      turnRecords.map((record) => record.provider_block_reason).filter(Boolean),
    );
    const preDispatchBlocked = providerDispatch.has("not_dispatched");
    if (providerDispatch.size > 1) {
      errors.push(`同一 fixture/Turn 的 provider_dispatch 不一致：${turnKey}`);
    }
    if (providerBlockReasons.size > 1) {
      errors.push(`同一 fixture/Turn 的 provider_block_reason 不一致：${turnKey}`);
    }
    if (preDispatchBlocked) {
      if (!abnormalOutcome) {
        errors.push(`Provider 未 dispatch 的 Turn 必须是异常 outcome：${turnKey}`);
      }
      if (turnRecords[0]?.trajectory_mode !== "abnormal") {
        errors.push(`Provider 未 dispatch 的 Turn 必须声明 trajectory_mode=abnormal：${turnKey}`);
      }
      if (providerBlockReasons.size !== 1
        || typeof turnRecords[0]?.provider_block_reason !== "string"
        || turnRecords[0].provider_block_reason.trim().length === 0) {
        errors.push(`Provider 未 dispatch 的 Turn 缺少明确阻断原因：${turnKey}`);
      }
      if (!turnRecords.some((record) => record.settlement_required === true)) {
        errors.push(`Provider 未 dispatch 的 Turn 必须声明 settlement_required：${turnKey}`);
      }
      if (!turnRecords.some((record) => typeof record.input_hash === "string" && record.input_hash.length > 0)) {
        errors.push(`Provider 未 dispatch 的 Turn 缺少 input_hash：${turnKey}`);
      }
      if (turnRecords.some((record) => [
        "provider_request",
        "provider_raw_delta",
        "provider_visible_delta",
      ].includes(record.phase))) {
        errors.push(`Provider 未 dispatch 的 Turn 不得包含 Provider 阶段：${turnKey}`);
      }
    } else if (providerDispatch.size > 0 && !providerDispatch.has("dispatched")) {
      errors.push(`provider_dispatch 值无效：${turnKey}`);
    }
    const requiredPhases = preDispatchBlocked
      ? ["accepted", "canonical_terminal"]
      : abnormalOutcome
        ? ["accepted", "provider_request", "canonical_terminal"]
        : ["accepted", "provider_request", "event_bus", "canonical_terminal"];
    for (const phase of requiredPhases) {
      if (!phaseCounts.has(phase)) {
        errors.push(`Turn 缺少必需阶段：${turnKey}/${phase}`);
      }
    }
    for (const phase of ["accepted", "provider_request", "event_bus", "canonical_terminal"]) {
      if ((phaseCounts.get(phase) || 0) > 1) {
        errors.push(`Turn 存在重复阶段：${turnKey}/${phase}`);
      }
    }
    if (turnRecords.some((record) => record.source === "electron_renderer")) {
      for (const phase of [
        "renderer_received",
        "reducer_completed",
        "projection_completed",
        "dom_painted",
      ]) {
        if (!phaseCounts.has(phase)) {
          errors.push(`Electron Turn 缺少必需 Renderer 阶段：${turnKey}/${phase}`);
        }
        if ((phaseCounts.get(phase) || 0) > 1) {
          errors.push(`Electron Turn 存在重复 Renderer 阶段：${turnKey}/${phase}`);
        }
      }
    }
    const terminals = turnRecords.filter((record) => record.phase === "canonical_terminal");
    if (terminals.length > 1) errors.push(`同一 fixture/turn_id 存在多个 canonical terminal：${turnKey}`);
    const eventBus = turnRecords.find((record) => record.phase === "event_bus");
    const terminal = turnRecords.find((record) => record.phase === "canonical_terminal");
    const identityFields = [
      "scenario",
      "sample_index",
      "query_source",
      "session_id",
      "request_id",
      "outcome",
      "provider_dispatch",
      "provider_block_reason",
    ];
    for (const field of identityFields) {
      if (new Set(turnRecords.map((record) => record[field])).size > 1) {
        errors.push(`同一 fixture/Turn 的 ${field} 不一致：${turnKey}`);
      }
    }
    const eventBusTerminalSequenceIsInvalid = preDispatchBlocked
      ? terminal?.event_sequence < eventBus?.event_sequence
      : terminal?.event_sequence <= eventBus?.event_sequence;
    if (eventBus && terminal
      && eventBus.event_sequence !== null && terminal.event_sequence !== null
      && eventBusTerminalSequenceIsInvalid) {
      errors.push(`canonical terminal 序号未在 EventBus 之后：${turnKey}`);
    }
    if (terminal?.settlement_required === true) {
      const settlement = terminal.settlement;
      if (settlement?.observed !== true) {
        errors.push(`终态缺少 settlement 证据：${turnKey}`);
      } else if (Number.isFinite(settlement.sinceAcceptedMs)
        && settlement.sinceAcceptedMs < terminal.timestamp_ms
        && settlement.barrier !== "session_terminal_finalize_core_completed") {
        errors.push(`settlement 早于 canonical terminal：${turnKey}`);
      }
    }
    const rendererPhaseOrder = [
      "renderer_received",
      "reducer_completed",
      "projection_completed",
      "dom_painted",
    ];
    const rendererRecords = rendererPhaseOrder
      .map((phase) => turnRecords.find((record) => record.phase === phase))
      .filter(Boolean);
    for (let index = 1; index < rendererRecords.length; index += 1) {
      if (rendererRecords[index].timestamp_ms < rendererRecords[index - 1].timestamp_ms) {
        errors.push(`Renderer 阶段时间线倒退：${turnKey}`);
        break;
      }
    }
  }
  return errors;
}

function sourceIdentityValue(source, ...keys) {
  for (const key of keys) {
    if (source && typeof source[key] === "string") return source[key];
  }
  return null;
}

function validateInputSource(input, inputPath) {
  const source = input?.source;
  if (!source || typeof source !== "object") return [];
  const errors = [];
  const before = source.before;
  const after = source.after;
  const stable = source.stable_during_sample ?? source.stableDuringSample ?? source.stable;
  if (stable !== true) {
    errors.push(`输入 evidence 未确认采样期间源码稳定：${inputPath}`);
  }
  const beforeCommit = sourceIdentityValue(before, "commit", "source_commit");
  const afterCommit = sourceIdentityValue(after, "commit", "source_commit");
  const beforeFingerprint = sourceIdentityValue(before, "fingerprintSha256", "worktree_fingerprint_sha256");
  const afterFingerprint = sourceIdentityValue(after, "fingerprintSha256", "worktree_fingerprint_sha256");
  if (beforeCommit && afterCommit && beforeCommit !== afterCommit) {
    errors.push(`输入 evidence 采样前后 commit 不一致：${inputPath}`);
  }
  if (beforeFingerprint && afterFingerprint && beforeFingerprint !== afterFingerprint) {
    errors.push(`输入 evidence 采样前后 worktree fingerprint 不一致：${inputPath}`);
  }
  return errors;
}

async function main() {
  const { output, inputs } = parseArgs(process.argv.slice(2));
  const allRecords = [];
  const inputMetadata = [];
  const inputErrors = [];
  for (const inputPath of inputs) {
    const raw = await readFile(inputPath, "utf8");
    const input = JSON.parse(raw);
    inputMetadata.push({
      path: inputPath,
      payload_hash: hashJson(input),
      fixture: input.fixture || input.daemon?.runtimeEpoch || basename(inputPath),
      status: input.status || "unknown",
      trajectory_mode: input.trajectoryMode || "normal",
      expected_outcome: input.expectedOutcome || null,
      settlement_required: input.settlement_required === true,
      provider_dispatch: input.providerDispatch || null,
      provider_block_reason: input.providerBlockReason || null,
      source: input.source || null,
      ...(input.samplingError ? { sampling_error: input.samplingError } : {}),
    });
    const recordsBefore = allRecords.length;
    // Electron 压缩 fixture 为了保留 compaction-specific sample 字段同时提供
    // `samples` 和 `rendererTimingSamples`；它仍只有一条 Electron 轨迹，不能
    // 被先按 real-provider 再按 Renderer 轨迹重复计入。
    if (Array.isArray(input.rendererTimingSamples)) {
      allRecords.push(...electronRecords(input, inputPath));
    } else if (Array.isArray(input.samples)) {
      allRecords.push(...realProviderRecords(input, inputPath));
    }
    inputErrors.push(...validateInputSource(input, inputPath));
    if (allRecords.length === recordsBefore) {
      inputErrors.push(`输入没有可识别的 samples 或 rendererTimingSamples：${inputPath}`);
    }
  }
  const errors = [...inputErrors, ...validateRecords(allRecords)];
  for (const input of inputMetadata) {
    if (input.status !== "passed") {
      errors.push(`输入 evidence 未通过：${input.path} status=${input.status}`);
    }
    if (input.sampling_error) {
      errors.push(`输入 evidence 含 samplingError：${input.path}`);
    }
  }
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
