#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-electron-context-compaction-metrics.mjs", import.meta.url));
const stages = [
  "frontend_event_received",
  "reducer_completed",
  "projection_completed",
  "dom_painted",
];

function backendStage({ sessionId, requestId, sinceAcceptedMs, sequence = null }) {
  return {
    count: 1,
    first: {
      atMs: 1_000 + sinceAcceptedMs,
      elapsedMs: sinceAcceptedMs,
      sequence,
      traceId: requestId,
      requestId,
      sessionId,
      providerCallId: "provider-call-golden",
      sinceAcceptedMs,
      source: "golden",
    },
  };
}

function rendererStage(elapsedMs) {
  return { count: 1, first: { atMs: 100 + elapsedMs, elapsedMs } };
}

function sample(index, role, state = index === 19 ? "skipped" : "completed") {
  const sessionId = `session-${role}-${index}`;
  const requestId = `request-${role}-${index}`;
  const turnId = `turn-${role}-${index}`;
  return {
    fixture: "electron-dom-context-compaction-v1",
    schema_version: "magi.electron.context-compaction.v1",
    scenario: "personal_long_history",
    query_source: "main_turn",
    execution_profile: "conversation",
    outcome: "completed",
    turnId,
    sessionId,
    requestId,
    historySeedTurnId: `seed-${role}-${index}`,
    contextWindowTokens: 16_000,
    longHistoryChars: 4_000,
    compactionEvents: [
      { sequence: 9, state: "running", phase: "pre_turn" },
      { sequence: 10, state, phase: "pre_turn" },
    ],
    backend: {
      traceId: requestId,
      sessionId,
      stages: {
        accepted_response_sent: backendStage({ sessionId, requestId, sinceAcceptedMs: 0 }),
        provider_request_started: backendStage({ sessionId, requestId, sinceAcceptedMs: 10 }),
        provider_first_raw_delta: backendStage({ sessionId, requestId, sinceAcceptedMs: 20, providerCallId: "provider-call-golden" }),
        provider_first_delta: backendStage({ sessionId, requestId, sinceAcceptedMs: 20, providerCallId: "provider-call-golden" }),
        event_bus_first_event: backendStage({ sessionId, requestId, sinceAcceptedMs: 30, sequence: 11 }),
        terminal: backendStage({ sessionId, requestId, sinceAcceptedMs: 40, sequence: 12 }),
      },
    },
    stages: Object.fromEntries(stages.map((stage, stageIndex) => [
      stage,
      rendererStage(1 + stageIndex + index / 10),
    ])),
  };
}

function source(role) {
  return {
    source_commit: "golden-electron-source-commit",
    worktree_fingerprint_sha256: `golden-electron-worktree-${role}`,
    executable_sha256: `golden-launcher-${role}`,
    app_bundle: `/tmp/golden-${role}.app`,
    app_artifact_sha256: `golden-electron-artifact-${role}`,
    dirty: false,
    changed_path_count: 0,
  };
}

function evidence(role) {
  const sourceIdentity = source(role);
  return {
    type: "electron_conversation_context_compaction",
    schema_version: "magi.electron.context-compaction.v1",
    fixture: "electron-dom-context-compaction-v1",
    sampleCount: 20,
    scenarios: ["personal_long_history"],
    checks: [],
    desktopIpcValidationErrors: [],
    context: {
      contextWindowTokens: 16_000,
      longHistoryChars: 4_000,
      compactionRequired: true,
      observedCompactionRows: 19,
      skippedCompactionRows: 1,
    },
    source: { before: sourceIdentity, after: sourceIdentity, stable: true },
    providerRequests: 40,
    samples: Array.from({ length: 20 }, (_, index) => sample(index, role)),
    status: "passed",
  };
}

function run(args) {
  return spawnSync(process.execPath, [deriveScript, ...args], { encoding: "utf8" });
}

const root = await mkdtemp(join(tmpdir(), "magi-electron-context-compaction-golden-"));
try {
  const paths = {};
  for (const role of ["before", "after"]) {
    paths[role] = join(root, `${role}-evidence.json`);
    await writeFile(paths[role], `${JSON.stringify(evidence(role), null, 2)}\n`, "utf8");
  }
  const sidecars = {};
  for (const role of ["before", "after"]) {
    sidecars[role] = join(root, `${role}-sidecar.json`);
    const result = run(["--role", role, "--input", paths[role], "--output", sidecars[role]]);
    assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
    const sidecar = JSON.parse(await readFile(sidecars[role], "utf8"));
    assert.equal(sidecar.status, "passed");
    assert.equal(sidecar.metric_version, "magi-electron-context-compaction-metrics.v2");
    assert.equal(sidecar.sample_count, 20);
    assert.equal(sidecar.context.completed_samples, 19);
    assert.equal(sidecar.context.skipped_samples, 1);
    assert.equal(sidecar.summary.personal_long_history.backend.accepted_to_terminal_ms.p95_ms, 40);
  }
  const comparisonPath = join(root, "comparison.json");
  const comparisonResult = run([
    "--compare", sidecars.before, sidecars.after,
    "--output", comparisonPath,
  ]);
  assert.equal(comparisonResult.status, 0, `${comparisonResult.stderr}\n${comparisonResult.stdout}`);
  const comparison = JSON.parse(await readFile(comparisonPath, "utf8"));
  assert.equal(comparison.status, "passed");
  assert.equal(comparison.validation.separate_non_additive_clocks, true);
  assert.equal(comparison.validation.distinct_source_identity, true);
  assert.equal(comparison.validation.same_worktree_fingerprint, false);
  assert.equal(comparison.deltas["renderer.dom_painted"].p95_ms, 0);
  console.log("electron context compaction metrics golden passed");
} finally {
  await rm(root, { recursive: true, force: true });
}
