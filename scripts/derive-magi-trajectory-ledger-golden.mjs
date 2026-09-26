#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-magi-trajectory-ledger.mjs", import.meta.url));
const tempRoot = await mkdtemp(join(tmpdir(), "magi-trajectory-ledger-golden-"));

function stage(sinceAcceptedMs, sequence = null) {
  return {
    count: 1,
    first: { sinceAcceptedMs, atMs: 1_000 + sinceAcceptedMs, sequence },
    last: { sinceAcceptedMs, atMs: 1_000 + sinceAcceptedMs, sequence },
  };
}

function sample(withProviderRequest = true) {
  const backendStages = {
    accepted_response_sent: stage(0),
    ...(withProviderRequest ? { provider_request_started: stage(10) } : {}),
    event_bus_first_event: stage(15, 8),
    session_terminal_finalize_core_completed: stage(20),
    canonical_terminal_published: stage(30, 9),
  };
  return {
    fixture: "trajectory-ledger-golden-v5",
    schema_version: "magi.electron.timing.v1",
    scenario: "approval_cancelled",
    query_source: "main_turn",
    execution_profile: "task",
    outcome: "cancelled",
    trajectoryMode: "abnormal",
    expectedOutcome: "cancelled",
    inputHash: "golden-input-hash",
    settlementRequired: true,
    turnId: "turn-golden-v5",
    sessionId: "session-golden-v5",
    requestId: "request-golden-v5",
    settlement: {
      observed: true,
      barrier: "session_terminal_finalize_core_completed",
      stage: "session_terminal_finalize_core_completed",
      sinceAcceptedMs: 20,
    },
    backend: {
      traceId: "request-golden-v5",
      sessionId: "session-golden-v5",
      stages: backendStages,
    },
    stages: {
      frontend_event_received: stage(31),
      reducer_completed: stage(32),
      projection_completed: stage(33),
      dom_painted: stage(34),
    },
  };
}

function preDispatchSample({ withReason = true } = {}) {
  return {
    ...sample(false),
    fixture: "trajectory-ledger-golden-pre-dispatch-v5",
    scenario: "git_branch_drift",
    providerDispatch: "not_dispatched",
    ...(withReason ? { providerBlockReason: "git_branch_drift" } : {}),
    turnId: "turn-golden-pre-dispatch-v5",
    sessionId: "session-golden-pre-dispatch-v5",
    requestId: "request-golden-pre-dispatch-v5",
    backend: {
      traceId: "request-golden-pre-dispatch-v5",
      sessionId: "session-golden-pre-dispatch-v5",
      stages: {
        accepted_response_sent: stage(0),
        event_bus_first_event: stage(15, 8),
        session_terminal_finalize_core_completed: stage(20),
        canonical_terminal_published: stage(30, 9),
      },
    },
  };
}

async function derive(name, rendererTimingSamples) {
  const inputPath = join(tempRoot, `${name}.input.json`);
  const outputPath = join(tempRoot, `${name}.ledger.json`);
  await writeFile(inputPath, `${JSON.stringify({ status: "passed", rendererTimingSamples }, null, 2)}\n`, "utf8");
  const result = spawnSync(
    process.execPath,
    [deriveScript, "--output", outputPath, inputPath],
    { encoding: "utf8" },
  );
  const ledger = JSON.parse(await readFile(outputPath, "utf8"));
  return { result, ledger };
}

try {
  const valid = await derive("valid-finalizer-before-terminal", [sample(true)]);
  assert.equal(valid.result.status, 0, `${valid.result.stderr}\n${valid.result.stdout}`);
  assert.equal(valid.ledger.status, "passed");
  assert.equal(valid.ledger.derive_version, "magi-trajectory-derive.v5");
  assert.equal(valid.ledger.record_count, 8);
  assert.deepEqual(valid.ledger.validation.errors, []);

  const missingProviderRequest = await derive("missing-provider-request", [sample(false)]);
  assert.equal(missingProviderRequest.result.status, 2, missingProviderRequest.result.stderr);
  assert.equal(missingProviderRequest.ledger.status, "incomplete");
  assert(
    missingProviderRequest.ledger.validation.errors.some((error) => error.includes("provider_request")),
    JSON.stringify(missingProviderRequest.ledger.validation.errors),
  );

  const preDispatch = await derive("valid-pre-dispatch", [preDispatchSample()]);
  assert.equal(preDispatch.result.status, 0, `${preDispatch.result.stderr}\n${preDispatch.result.stdout}`);
  assert.equal(preDispatch.ledger.status, "passed");
  assert.equal(preDispatch.ledger.record_count, 7);
  assert.equal(preDispatch.ledger.phase_counts.provider_request, undefined);
  assert.equal(preDispatch.ledger.records.some((record) => record.provider_dispatch === "not_dispatched"), true);
  assert.equal(preDispatch.ledger.records.some((record) => record.provider_block_reason === "git_branch_drift"), true);

  const missingBlockReason = await derive("missing-pre-dispatch-reason", [
    preDispatchSample({ withReason: false }),
  ]);
  assert.equal(missingBlockReason.result.status, 2, missingBlockReason.result.stderr);
  assert.equal(missingBlockReason.ledger.status, "incomplete");
  assert(
    missingBlockReason.ledger.validation.errors.some((error) => error.includes("明确阻断原因")),
    JSON.stringify(missingBlockReason.ledger.validation.errors),
  );
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}

console.log("magi trajectory ledger golden: passed");
