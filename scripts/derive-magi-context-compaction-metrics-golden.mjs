#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-magi-context-compaction-metrics.mjs", import.meta.url));
const root = await mkdtemp(join(tmpdir(), "magi-context-compaction-metrics-golden-"));
const logPath = join(root, "daemon.log");
const evidencePath = join(root, "evidence.json");
const beforeEvidencePath = join(root, "before-evidence.json");
const afterOutputPath = join(root, "after.json");
const beforeOutputPath = join(root, "before.json");
const comparisonPath = join(root, "comparison.json");
const invalidEvidencePath = join(root, "invalid-evidence.json");
const invalidOutputPath = join(root, "invalid.json");

function iso(timestampMs) {
  return new Date(timestampMs).toISOString();
}

function timingLine({ timestampMs, requestId, sessionId, turnId, stage, providerCallId = "" }) {
  return `${iso(timestampMs)} INFO conversation response timing trace_id="${requestId}" request_id="${requestId}" session_id="${sessionId}" turn_id="${turnId}" provider_call_id="${providerCallId}" stage="${stage}"`;
}

function sample(scenario, index, compactionState = null) {
  const requestId = `request-golden-${scenario}-${index}`;
  const sessionId = `session-golden-${scenario}-${index}`;
  const turnId = `turn-golden-${scenario}-${index}`;
  return {
    scenario,
    sampleIndex: index,
    requestId,
    sessionId,
    turnId,
    status: "completed",
    acceptedMs: 1,
    firstEventMs: 31,
    firstEventSequence: 10,
    terminalMs: 41,
    terminalSequence: 11,
    providerFirstRawDeltaMs: 21,
    providerFirstDeltaMs: 21,
    compactionEvents: compactionState
      ? [{ sequence: 9, state: compactionState, phase: "pre_turn" }]
      : [],
  };
}

function makeEvidence(sourceFingerprint, { invalid = false } = {}) {
  const samples = [];
  const logLines = [];
  let timestampMs = Date.parse("2026-09-26T12:00:00.000Z");
  for (const scenario of ["new_personal_chat", "personal_long_history"]) {
    for (let index = 0; index < 20; index += 1) {
      const state = scenario === "personal_long_history"
        ? (invalid ? "running" : index === 19 ? "skipped" : "completed")
        : null;
      const row = sample(scenario, index, state);
      samples.push(row);
      const base = timestampMs;
      const stageOffsets = [
        ["accepted_response_sent", 10],
        ["provider_request_started", 20],
        ["provider_first_raw_delta", 30],
        ["provider_first_delta", 30],
        ["event_bus_item_published", 40],
        ["canonical_terminal_published", 50],
      ];
      for (const [stage, offset] of stageOffsets) {
        logLines.push(timingLine({
          timestampMs: base + offset,
          requestId: row.requestId,
          sessionId: row.sessionId,
          turnId: row.turnId,
          stage,
          providerCallId: ["provider_request_started", "provider_first_raw_delta", "provider_first_delta"].includes(stage)
            ? `provider-call-${row.requestId}`
            : "",
        }));
      }
      timestampMs += 1000;
    }
  }
  return {
    evidence: {
      status: "passed",
      fixture: "real-provider-context-compaction-golden-v1",
      provider: { model: "golden-model", reasoningEffort: "medium" },
      context: {
        contextWindowTokens: 16_000,
        longHistoryChars: 4_000,
        compactionRequired: true,
        observedCompactionRows: invalid ? 0 : 19,
        skippedCompactionRows: invalid ? 0 : 1,
      },
      source: {
        before: {
          commit: "golden-commit",
          dirty: false,
          fingerprintSha256: sourceFingerprint,
        },
        after: {
          commit: "golden-commit",
          dirty: false,
          fingerprintSha256: sourceFingerprint,
        },
        stableDuringSample: true,
      },
      samples,
      logPath,
    },
    log: `${logLines.join("\n")}\n`,
  };
}

function run(args) {
  return spawnSync(process.execPath, [deriveScript, ...args], { encoding: "utf8" });
}

try {
  const afterFixture = makeEvidence("after-fingerprint");
  await writeFile(evidencePath, `${JSON.stringify(afterFixture.evidence, null, 2)}\n`, "utf8");
  await writeFile(logPath, afterFixture.log, "utf8");
  const afterResult = run([
    "--role", "after",
    "--input", evidencePath,
    "--output", afterOutputPath,
  ]);
  assert.equal(afterResult.status, 0, afterResult.stderr);
  const after = JSON.parse(await readFile(afterOutputPath, "utf8"));
  assert.equal(after.status, "passed");
  assert.equal(after.metric_version, "magi-context-compaction-metrics.v1");
  assert.equal(after.sample_count, 40);
  assert.equal(after.summary.personal_short_history, undefined);
  assert.equal(after.summary.personal_long_history.accepted_to_terminal_ms.samples, 20);
  assert.equal(after.context.completed_samples, 19);
  assert.equal(after.context.skipped_samples, 1);

  const beforeFixture = makeEvidence("before-fingerprint");
  await writeFile(beforeEvidencePath, `${JSON.stringify(beforeFixture.evidence, null, 2)}\n`, "utf8");
  const beforeResult = run([
    "--role", "before",
    "--input", beforeEvidencePath,
    "--output", beforeOutputPath,
  ]);
  assert.equal(beforeResult.status, 0, beforeResult.stderr);
  const comparisonResult = run([
    "--compare", beforeOutputPath, afterOutputPath,
    "--output", comparisonPath,
  ]);
  assert.equal(comparisonResult.status, 0, comparisonResult.stderr);
  const comparison = JSON.parse(await readFile(comparisonPath, "utf8"));
  assert.equal(comparison.status, "passed");
  assert.equal(comparison.validation.distinct_source_identity, true);

  const invalidFixture = makeEvidence("invalid-fingerprint", { invalid: true });
  await writeFile(invalidEvidencePath, `${JSON.stringify(invalidFixture.evidence, null, 2)}\n`, "utf8");
  const invalidResult = run([
    "--role", "after",
    "--input", invalidEvidencePath,
    "--output", invalidOutputPath,
  ]);
  assert.notEqual(invalidResult.status, 0);
  console.log("magi context compaction metrics golden: passed");
} finally {
  await rm(root, { recursive: true, force: true });
}
