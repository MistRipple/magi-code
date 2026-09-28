#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-electron-renderer-comparison.mjs", import.meta.url));
const scenarios = ["personal_chat", "workspace_chat", "workspace_tool", "goal", "subagent"];

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
      source: "golden",
      sinceAcceptedMs,
    },
  };
}

function rendererStage(elapsedMs, count = 1) {
  return {
    count,
    first: { atMs: 100 + elapsedMs, elapsedMs },
    last: { atMs: 100 + elapsedMs, elapsedMs },
  };
}

function sample(scenario, index, role) {
  const sessionId = `session-${role}-${scenario}`;
  const requestId = `request-${role}-${scenario}-${index}`;
  const turnId = `turn-${role}-${scenario}-${index}`;
  return {
    fixture: "electron-dom-timing-v1",
    schema_version: "magi.electron.timing.v1",
    scenario,
    query_source: "main_turn",
    execution_profile: scenario === "personal_chat" || scenario === "workspace_chat"
      ? "conversation"
      : "task",
    outcome: "completed",
    turnId,
    sessionId,
    requestId,
    backend: {
      traceId: requestId,
      sessionId,
      stages: {
        accepted_response_sent: backendStage({ sessionId, requestId, sinceAcceptedMs: 0 }),
        provider_request_started: backendStage({ sessionId, requestId, sinceAcceptedMs: 10 }),
        provider_first_raw_delta: backendStage({ sessionId, requestId, sinceAcceptedMs: 20 }),
        provider_first_delta: backendStage({ sessionId, requestId, sinceAcceptedMs: 25 }),
        event_bus_first_event: backendStage({ sessionId, requestId, sinceAcceptedMs: 30, sequence: 10 }),
        canonical_terminal_published: backendStage({ sessionId, requestId, sinceAcceptedMs: 40, sequence: 11 }),
      },
    },
    stages: {
      frontend_event_received: rendererStage(0.1, 1),
      reducer_completed: rendererStage(0.3, 1),
      projection_completed: rendererStage(0.8, 1),
      dom_painted: rendererStage(10 + index / 10, 1),
    },
  };
}

function identity(role) {
  return {
    source_commit: "golden-source-commit",
    worktree_fingerprint_sha256: `golden-worktree-fingerprint-${role}`,
    executable_sha256: "golden-electron-launcher",
    app_bundle: `/tmp/magi-golden-${role}.app`,
    app_artifact_sha256: `golden-app-artifact-${role}`,
    dirty: false,
    changed_path_count: 0,
  };
}

function evidence(scenario, role) {
  const source = identity(role);
  return {
    type: "electron_conversation_renderer_timing",
    schema_version: "magi.electron.timing.v1",
    fixture: "electron-dom-timing-v1",
    sampleCount: 20,
    checks: Array.from({ length: 2 }, (_, index) => `check-${index}`),
    desktopIpcValidationErrors: [],
    source: { before: source, after: source, stable: true },
    providerRequests: 20,
    rendererTimingSamples: Array.from({ length: 20 }, (_, index) => sample(scenario, index, role)),
    status: "passed",
  };
}

function run(args) {
  const result = spawnSync(process.execPath, [deriveScript, ...args], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
}

const tempRoot = await mkdtemp(join(tmpdir(), "magi-electron-renderer-golden-"));
try {
  const inputFiles = { before: [], after: [] };
  for (const role of ["before", "after"]) {
    for (const scenario of scenarios) {
      const path = join(tempRoot, `${role}-${scenario}.json`);
      await writeFile(path, `${JSON.stringify(evidence(scenario, role), null, 2)}\n`, "utf8");
      inputFiles[role].push(path);
    }
  }
  const aggregated = {};
  for (const role of ["before", "after"]) {
    aggregated[role] = join(tempRoot, `${role}.json`);
    run([
      "--role", role,
      "--output", aggregated[role],
      ...inputFiles[role].flatMap((path) => ["--input", path]),
    ]);
    const output = JSON.parse(await readFile(aggregated[role], "utf8"));
    assert.equal(output.status, "passed");
    assert.equal(output.derive_version, "magi-electron-renderer-derive.v3");
    assert.equal(output.rendererTimingSamples.length, 100);
    assert.equal(output.input_file_sha256.length, 5);
    assert.equal(output.backendStats.subagent.accepted_to_terminal_ms.p95_ms, 40);
    assert.equal(output.clocks.additive, false);
  }
  const comparisonPath = join(tempRoot, "comparison.json");
  run(["--compare", aggregated.before, aggregated.after, "--output", comparisonPath]);
  const comparison = JSON.parse(await readFile(comparisonPath, "utf8"));
  assert.equal(comparison.status, "passed");
  assert.equal(comparison.derive_version, "magi-electron-renderer-derive.v3");
  assert.equal(comparison.validation.backend_and_renderer_same_turn, true);
  assert.equal(comparison.validation.same_worktree_fingerprint, false);
  assert.equal(comparison.validation.distinct_source_identity, true);
  assert.equal(comparison.validation.distinct_input_payload_hash, true);
  assert.equal(comparison.validation.separate_non_additive_clocks, true);
  assert.equal(comparison.backendDeltas.personal_chat.accepted_to_terminal_ms.p95_ms, 0);
  console.log(`electron renderer comparison golden passed (${basename(comparisonPath)})`);
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}
