#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const DAEMON_SCENARIOS = [
  "new_personal_chat",
  "personal_long_history",
  "workspace_chat",
  "workspace_tool",
  "subagent_concurrency",
];
const ELECTRON_SCENARIOS = [
  "personal_chat",
  "workspace_chat",
  "workspace_tool",
  "goal",
  "subagent",
];
const ENVELOPE_DERIVE_VERSION = "magi-performance-envelope-derive.v1";
const ELECTRON_DERIVE_VERSION = "magi-electron-renderer-derive.v2";

function usage() {
  console.error([
    "绑定 daemon 性能 sidecar 与 Electron 同 Turn comparison，保留分层 fixture/时钟，不计算总延迟：",
    "  node scripts/derive-magi-performance-envelope.mjs --daemon-before DAEMON_BEFORE --daemon-after DAEMON_AFTER",
    "    --electron-before ELECTRON_BEFORE --electron-after ELECTRON_AFTER",
    "    --electron-comparison ELECTRON_COMPARISON --output OUTPUT",
  ].join("\n"));
}

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (key === "--help" || key === "-h") {
      usage();
      process.exit(0);
    }
    if (!key.startsWith("--") || index + 1 >= argv.length) throw new Error(`参数无效：${key}`);
    values[key.slice(2).replaceAll("-", "_")] = resolve(argv[++index]);
  }
  const required = [
    "daemon_before",
    "daemon_after",
    "electron_before",
    "electron_after",
    "electron_comparison",
    "output",
  ];
  for (const key of required) if (!values[key]) throw new Error(`缺少 --${key.replaceAll("_", "-")}`);
  return values;
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function stableValue(value) {
  if (Array.isArray(value)) return value.map(stableValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stableValue(value[key])]));
  }
  return value;
}

function hashJson(value) {
  return sha256(JSON.stringify(stableValue(value)));
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function assertScenarioSet(value, expected, label) {
  assert(JSON.stringify(value) === JSON.stringify(expected), `${label} 场景集合不一致`);
}

function validateDaemon(metrics, role) {
  assert(metrics.status === "passed", `daemon ${role} status 不是 passed`);
  assert(metrics.schema_version === "magi.performance.v1", `daemon ${role} schema_version 无效`);
  assert(metrics.fixture === "real-provider-performance-v1", `daemon ${role} fixture 无效`);
  assert(metrics.comparison_role === role, `daemon ${role} comparison_role 无效`);
  assert(metrics.sample_count === 100 && metrics.metrics?.length === 100,
    `daemon ${role} 必须有 100 条 metric`);
  assert(metrics.source?.stableDuringSample === true, `daemon ${role} source 未稳定`);
  for (const scenario of DAEMON_SCENARIOS) {
    assert(metrics.summary?.[scenario], `daemon ${role} 缺少场景 ${scenario}`);
  }
}

function validateElectronAggregate(aggregate, role) {
  assert(aggregate.status === "passed", `Electron ${role} status 不是 passed`);
  assert(aggregate.schema_version === "magi.electron.timing.v1", `Electron ${role} schema_version 无效`);
  assert(aggregate.derive_version === ELECTRON_DERIVE_VERSION, `Electron ${role} derive_version 无效`);
  assert(aggregate.fixture === "electron-dom-timing-paired-v1", `Electron ${role} fixture 无效`);
  assert(aggregate.comparison_role === role, `Electron ${role} comparison_role 无效`);
  assert(aggregate.sampleCount === 20 && aggregate.rendererTimingSamples?.length === 100,
    `Electron ${role} 必须有 100 条 sample`);
  assert(aggregate.input_payload_hash && aggregate.input_file_sha256?.length === 5,
    `Electron ${role} 缺少原始输入 hash`);
  assert(aggregate.source?.stable === true, `Electron ${role} source 未稳定`);
  assertScenarioSet(aggregate.scenarios, ELECTRON_SCENARIOS, `Electron ${role}`);
}

function validateComparison(comparison, before, after) {
  assert(comparison.status === "passed", "Electron comparison status 不是 passed");
  assert(comparison.schema_version === "magi.electron.timing.v1", "Electron comparison schema_version 无效");
  assert(comparison.derive_version === ELECTRON_DERIVE_VERSION, "Electron comparison derive_version 无效");
  assert(comparison.fixture === "electron-dom-timing-paired-v1", "Electron comparison fixture 无效");
  assert(comparison.validation?.backend_and_renderer_same_turn === true,
    "Electron comparison 缺少同 Turn 后端/Renderer 校验");
  assert(comparison.validation?.separate_non_additive_clocks === true,
    "Electron comparison 缺少非加和时钟声明");
  assert(comparison.before?.input_payload_hash === before.input_payload_hash,
    "Electron before input hash 未与 comparison 绑定");
  assert(comparison.after?.input_payload_hash === after.input_payload_hash,
    "Electron after input hash 未与 comparison 绑定");
  assert(comparison.before?.providerRequests === comparison.after?.providerRequests,
    "Electron before/after Provider request 数不一致");
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const daemonBefore = await readJson(args.daemon_before);
  const daemonAfter = await readJson(args.daemon_after);
  const electronBefore = await readJson(args.electron_before);
  const electronAfter = await readJson(args.electron_after);
  const electronComparison = await readJson(args.electron_comparison);

  validateDaemon(daemonBefore, "before");
  validateDaemon(daemonAfter, "after");
  validateElectronAggregate(electronBefore, "before");
  validateElectronAggregate(electronAfter, "after");
  validateComparison(electronComparison, electronBefore, electronAfter);
  assert(daemonBefore.provider?.model === daemonAfter.provider?.model, "daemon before/after model 不一致");
  assertScenarioSet(Object.keys(daemonBefore.summary), DAEMON_SCENARIOS, "daemon");

  const componentInputs = {
    daemon_before: {
      file: basename(args.daemon_before),
      input_payload_hash: daemonBefore.input_payload_hash,
      daemon_log_sha256: daemonBefore.daemon_log_sha256,
      source: daemonBefore.source,
    },
    daemon_after: {
      file: basename(args.daemon_after),
      input_payload_hash: daemonAfter.input_payload_hash,
      daemon_log_sha256: daemonAfter.daemon_log_sha256,
      source: daemonAfter.source,
    },
    electron_before: {
      file: basename(args.electron_before),
      input_payload_hash: electronBefore.input_payload_hash,
      source: electronBefore.source,
    },
    electron_after: {
      file: basename(args.electron_after),
      input_payload_hash: electronAfter.input_payload_hash,
      source: electronAfter.source,
    },
  };
  const result = {
    type: "magi_performance_envelope",
    schema_version: "magi.performance-envelope.v1",
    derive_version: ENVELOPE_DERIVE_VERSION,
    scenario_sets: {
      daemon: DAEMON_SCENARIOS,
      electron: ELECTRON_SCENARIOS,
    },
    scenario_mapping: [
      { daemon: "new_personal_chat", electron: "personal_chat", relation: "personal_chat_representative" },
      { daemon: "workspace_chat", electron: "workspace_chat", relation: "same_label" },
      { daemon: "workspace_tool", electron: "workspace_tool", relation: "same_label" },
      { daemon: "subagent_concurrency", electron: "subagent", relation: "subagent_representative" },
      { daemon: "personal_long_history", electron: null, relation: "daemon_only" },
      { daemon: null, electron: "goal", relation: "electron_only" },
    ],
    sample_count_per_scenario: 20,
    component_inputs: componentInputs,
    layers: {
      daemon: {
        fixture: daemonBefore.fixture,
        provider: daemonAfter.provider,
        before: { summary: daemonBefore.summary, source: daemonBefore.source },
        after: { summary: daemonAfter.summary, source: daemonAfter.source },
      },
      electron: {
        fixture: electronBefore.fixture,
        before: {
          input_payload_hash: electronBefore.input_payload_hash,
          backend_stats: electronBefore.backendStats,
          renderer_stats: electronBefore.rendererStats,
          source: electronBefore.source,
        },
        after: {
          input_payload_hash: electronAfter.input_payload_hash,
          backend_stats: electronAfter.backendStats,
          renderer_stats: electronAfter.rendererStats,
          source: electronAfter.source,
        },
      },
    },
    clock_contract: {
      daemon_backend: "absolute daemon timestamp",
      electron_backend: "sinceAcceptedMs derived from daemon timestamp",
      electron_renderer: "page-local elapsedMs",
      additive: false,
      total_latency_defined: false,
    },
    validation: {
      daemon_scenario_set_valid: true,
      electron_scenario_set_valid: true,
      scenario_mapping_explicit: true,
      full_scenario_equivalence: false,
      sample_count_per_scenario: 20,
      daemon_before_after_independently_recomputable: true,
      electron_before_after_independently_recomputable: true,
      electron_backend_and_renderer_same_turn: true,
      separate_fixture_streams_explicit: true,
      separate_non_additive_clocks: true,
      no_total_latency_claim: true,
    },
    component_input_hash: hashJson(componentInputs),
    status: "passed",
  };
  await writeFile(args.output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  console.log(JSON.stringify({
    status: result.status,
    output: args.output,
    derive_version: result.derive_version,
    component_input_hash: result.component_input_hash,
  }, null, 2));
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
