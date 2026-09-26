#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const deriveScript = fileURLToPath(new URL("./derive-magi-performance-envelope.mjs", import.meta.url));
const daemonScenarios = [
  "new_personal_chat",
  "personal_long_history",
  "workspace_chat",
  "workspace_tool",
  "subagent_concurrency",
];
const electronScenarios = ["personal_chat", "workspace_chat", "workspace_tool", "goal", "subagent"];

function daemon(role) {
  return {
    status: "passed",
    schema_version: "magi.performance.v1",
    fixture: "real-provider-performance-v1",
    comparison_role: role,
    sample_count: 100,
    metrics: Array.from({ length: 100 }, () => ({ outcome: "completed" })),
    summary: Object.fromEntries(daemonScenarios.map((scenario) => [scenario, {}])),
    input_payload_hash: `daemon-${role}-input`,
    daemon_log_sha256: `daemon-${role}-log`,
    source: { stableDuringSample: true, before: { commit: "commit", fingerprintSha256: `daemon-${role}` } },
    provider: { model: "model" },
  };
}

function electron(role) {
  return {
    status: "passed",
    schema_version: "magi.electron.timing.v1",
    derive_version: "magi-electron-renderer-derive.v2",
    fixture: "electron-dom-timing-paired-v1",
    comparison_role: role,
    sampleCount: 20,
    scenarios: electronScenarios,
    rendererTimingSamples: Array.from({ length: 100 }, () => ({})),
    input_payload_hash: `electron-${role}-input`,
    input_file_sha256: Array.from({ length: 5 }, (_, index) => ({ file: `${index}.json`, sha256: `${role}-${index}` })),
    source: { stable: true, before: { source_commit: "commit", worktree_fingerprint_sha256: `electron-${role}` } },
    backendStats: {},
    rendererStats: {},
  };
}

function comparison() {
  return {
    status: "passed",
    schema_version: "magi.electron.timing.v1",
    derive_version: "magi-electron-renderer-derive.v2",
    fixture: "electron-dom-timing-paired-v1",
    before: { input_payload_hash: "electron-before-input", providerRequests: 100 },
    after: { input_payload_hash: "electron-after-input", providerRequests: 100 },
    validation: {
      backend_and_renderer_same_turn: true,
      separate_non_additive_clocks: true,
    },
  };
}

const tempRoot = await mkdtemp(join(tmpdir(), "magi-performance-envelope-golden-"));
try {
  const paths = {};
  for (const [name, value] of Object.entries({
    daemonBefore: daemon("before"),
    daemonAfter: daemon("after"),
    electronBefore: electron("before"),
    electronAfter: electron("after"),
    electronComparison: comparison(),
  })) {
    paths[name] = join(tempRoot, `${name}.json`);
    await writeFile(paths[name], `${JSON.stringify(value)}\n`, "utf8");
  }
  const output = join(tempRoot, "envelope.json");
  const result = spawnSync(process.execPath, [
    deriveScript,
    "--daemon-before", paths.daemonBefore,
    "--daemon-after", paths.daemonAfter,
    "--electron-before", paths.electronBefore,
    "--electron-after", paths.electronAfter,
    "--electron-comparison", paths.electronComparison,
    "--output", output,
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
  const envelope = JSON.parse(await readFile(output, "utf8"));
  assert.equal(envelope.status, "passed");
  assert.equal(envelope.derive_version, "magi-performance-envelope-derive.v1");
  assert.equal(envelope.validation.scenario_mapping_explicit, true);
  assert.equal(envelope.validation.full_scenario_equivalence, false);
  assert.equal(envelope.clock_contract.total_latency_defined, false);
  console.log("magi performance envelope golden passed");
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}
