#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const originalModuleLoad = Module._load;
Module._load = function patchedModuleLoad(request, parent, isMain) {
  if (request === 'vscode') {
    return {};
  }
  return originalModuleLoad.call(this, request, parent, isMain);
};

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function testSourceGuardrails() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'), 'utf8');
  assert(source.includes('const finalRuntimeReason = finalRuntimeTermination?.reason;'), 'engine finally 未收敛到 finalRuntimeReason');
  assert(source.includes('reason: finalRuntimeReason,'), 'Attempt/metrics 未统一消费 finalRuntimeReason');
  assert(!source.includes("orchestratorRuntimeReason = orchestratorRuntimeReason || 'interrupted'"), '仍残留旧中断兜底分支');
  assert(!source.includes("planFinalStatus = 'failed';"), '仍残留 planFinalStatus 过程态直写');
  assert(!source.includes('orchestrator-finalized:${planFinalStatus}'), '仍残留旧 attempt reason 拼接逻辑');
}

function createEngineHarness(MissionDrivenEngine) {
  const engine = Object.create(MissionDrivenEngine.prototype);
  const metricsRecords = [];
  engine.terminationMetricsRepository = {
    append(record) {
      metricsRecords.push(record);
    },
  };
  engine.metricsRecords = metricsRecords;
  return engine;
}

function testCompletedDemotedToFailed(engine) {
  const resolved = engine.resolveOrchestratorRuntimeReason({
    runtimeReason: 'completed',
    runtimeSnapshot: {
      sourceEventIds: ['runtime:completed'],
    },
    additionalCandidates: [{
      reason: 'failed',
      eventId: 'engine:audit-intervention',
      triggeredAt: Date.now(),
    }],
    fallback: 'completed',
  });

  assert(resolved.reason === 'failed', `completed + failed candidate 应收敛为 failed，实际: ${resolved.reason}`);
  assert(
    Array.isArray(resolved.runtimeSnapshot?.sourceEventIds)
      && resolved.runtimeSnapshot.sourceEventIds.length === 1
      && resolved.runtimeSnapshot.sourceEventIds[0] === 'engine:audit-intervention',
    `failed evidence_ids 未切换到失败证据链: ${JSON.stringify(resolved.runtimeSnapshot?.sourceEventIds || [])}`,
  );

  const finalPlanStatus = engine.mapOrchestratorRuntimeReasonToPlanFinalStatus(resolved.reason);
  const attemptStatus = engine.mapOrchestratorRuntimeReasonToAttemptStatus(resolved.reason);
  assert(finalPlanStatus === 'failed', `failed 应压缩为 plan=failed，实际: ${finalPlanStatus}`);
  assert(attemptStatus === 'failed', `failed 应压缩为 attempt=failed，实际: ${attemptStatus}`);

  engine.persistTerminationMetrics({
    sessionId: 'session-engine-termination',
    planId: 'plan-engine-termination-failed',
    turnId: 'turn-engine-termination-failed',
    mode: 'deep',
    finalPlanStatus,
    runtimeReason: resolved.reason,
    runtimeRounds: 2,
    runtimeSnapshot: resolved.runtimeSnapshot,
    startedAt: Date.now() - 10,
  });

  const latest = engine.metricsRecords[engine.metricsRecords.length - 1];
  assert(latest.final_status === 'failed', `metrics final_status 应为 failed，实际: ${latest.final_status}`);
  assert(latest.reason === 'failed', `metrics reason 应为 failed，实际: ${latest.reason}`);
  assert(
    Array.isArray(latest.evidence_ids)
      && latest.evidence_ids.length === 1
      && latest.evidence_ids[0] === 'engine:audit-intervention',
    `metrics evidence_ids 异常: ${JSON.stringify(latest.evidence_ids)}`,
  );
}

function testExternalAbortCompression(engine) {
  const resolved = engine.resolveOrchestratorRuntimeReason({
    runtimeReason: 'external_abort',
    runtimeSnapshot: {
      sourceEventIds: ['engine:external-abort'],
    },
  });

  assert(resolved.reason === 'external_abort', `external_abort 不应被改写，实际: ${resolved.reason}`);
  const finalPlanStatus = engine.mapOrchestratorRuntimeReasonToPlanFinalStatus(resolved.reason);
  const attemptStatus = engine.mapOrchestratorRuntimeReasonToAttemptStatus(resolved.reason);
  assert(finalPlanStatus === 'cancelled', `external_abort 应压缩为 plan=cancelled，实际: ${finalPlanStatus}`);
  assert(attemptStatus === 'cancelled', `external_abort 应压缩为 attempt=cancelled，实际: ${attemptStatus}`);

  engine.persistTerminationMetrics({
    sessionId: 'session-engine-termination',
    planId: 'plan-engine-termination-cancelled',
    turnId: 'turn-engine-termination-cancelled',
    mode: 'deep',
    finalPlanStatus,
    runtimeReason: resolved.reason,
    runtimeRounds: 1,
    runtimeSnapshot: resolved.runtimeSnapshot,
    startedAt: Date.now() - 10,
  });

  const latest = engine.metricsRecords[engine.metricsRecords.length - 1];
  assert(latest.final_status === 'cancelled', `metrics final_status 应为 cancelled，实际: ${latest.final_status}`);
  assert(latest.reason === 'external_abort', `metrics reason 应为 external_abort，实际: ${latest.reason}`);
  assert(
    Array.isArray(latest.evidence_ids)
      && latest.evidence_ids.length === 1
      && latest.evidence_ids[0] === 'engine:external-abort',
    `external_abort metrics evidence_ids 异常: ${JSON.stringify(latest.evidence_ids)}`,
  );
}

function main() {
  testSourceGuardrails();
  const { MissionDrivenEngine } = loadCompiledModule(path.join('orchestrator', 'core', 'mission-driven-engine.js'));
  const engine = createEngineHarness(MissionDrivenEngine);

  testCompletedDemotedToFailed(engine);
  testExternalAbortCompression(engine);

  console.log('\n=== engine termination consistency regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'completed_demoted_to_failed',
      'failed_reason_plan_attempt_metrics_consistent',
      'external_abort_compresses_to_cancelled',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('\n=== engine termination consistency regression failed ===');
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}