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
  assert(source.includes("if (finalPlanStatus === 'completed')"), 'Mission completed 未统一消费 finalPlanStatus');
  assert(source.includes("} else if (finalPlanStatus === 'failed') {"), 'Mission failed 未统一消费 finalPlanStatus');
  assert(source.includes("} else if (finalPlanStatus === 'cancelled') {"), 'Mission cancelled 未统一消费 finalPlanStatus');
  assert(source.includes('private buildExecutionFailureMessages('), '缺少 formal runtime reason -> failureReason 收敛函数');
  assert(source.includes('runtimeReason: this.lastExecutionRuntimeReason,'), 'getLastExecutionStatus 未暴露 runtimeReason');
  assert(!source.includes('lastExecutionSuccess'), '仍残留旧 lastExecutionSuccess 第二裁决源');
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

function testMissionTerminalContracts(engine) {
  const cases = [
    { reason: 'completed', plan: 'completed', attempt: 'succeeded', success: true, message: undefined },
    { reason: 'cancelled', plan: 'cancelled', attempt: 'cancelled', success: false, message: undefined },
    { reason: 'external_abort', plan: 'cancelled', attempt: 'cancelled', success: false, message: undefined },
    { reason: 'interrupted', plan: 'cancelled', attempt: 'cancelled', success: false, message: undefined },
    { reason: 'failed', plan: 'failed', attempt: 'failed', success: false, message: '执行失败' },
    { reason: 'stalled', plan: 'failed', attempt: 'timeout', success: false, message: '执行停滞，未取得有效进展' },
    { reason: 'budget_exceeded', plan: 'failed', attempt: 'timeout', success: false, message: '执行达到预算上限' },
    { reason: 'external_wait_timeout', plan: 'failed', attempt: 'timeout', success: false, message: '执行等待外部条件超时' },
    { reason: 'upstream_model_error', plan: 'failed', attempt: 'failed', success: false, message: '执行遭遇上游模型错误' },
  ];

  for (const item of cases) {
    const finalPlanStatus = engine.mapOrchestratorRuntimeReasonToPlanFinalStatus(item.reason);
    const attemptStatus = engine.mapOrchestratorRuntimeReasonToAttemptStatus(item.reason);
    assert(finalPlanStatus === item.plan, `${item.reason} 应映射为 plan=${item.plan}，实际: ${finalPlanStatus}`);
    assert(attemptStatus === item.attempt, `${item.reason} 应映射为 attempt=${item.attempt}，实际: ${attemptStatus}`);

    const errors = item.plan === 'failed'
      ? engine.buildExecutionFailureMessages(item.reason, [])
      : [];
    if (item.message) {
      assert(errors.length > 0, `${item.reason} failed 时必须产生 failureReason`);
      assert(errors[0] === item.message, `${item.reason} failureReason 异常: ${errors[0]}`);
    }

    engine.lastExecutionRuntimeReason = item.reason;
    engine.lastExecutionErrors = errors;
    const status = engine.getLastExecutionStatus();
    assert(status.runtimeReason === item.reason, `getLastExecutionStatus runtimeReason 异常: ${status.runtimeReason}`);
    assert(status.success === item.success, `getLastExecutionStatus success 异常: ${item.reason} -> ${status.success}`);
    if (item.message) {
      assert(status.errors[0] === item.message, `getLastExecutionStatus errors 异常: ${status.errors[0]}`);
    }
  }
}

function main() {
  testSourceGuardrails();
  const { MissionDrivenEngine } = loadCompiledModule(path.join('orchestrator', 'core', 'mission-driven-engine.js'));
  const engine = createEngineHarness(MissionDrivenEngine);

  testCompletedDemotedToFailed(engine);
  testExternalAbortCompression(engine);
  testMissionTerminalContracts(engine);

  console.log('\n=== engine termination consistency regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'completed_demoted_to_failed',
      'failed_reason_plan_attempt_metrics_consistent',
      'external_abort_compresses_to_cancelled',
      'mission_terminal_contracts_follow_runtime_reason',
    ],
  }, null, 2));
  process.exit(0);
}

try {
  main();
} catch (error) {
  console.error('\n=== engine termination consistency regression failed ===');
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}