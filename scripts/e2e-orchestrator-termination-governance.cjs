#!/usr/bin/env node
/**
 * Orchestrator 终止治理回归脚本
 *
 * 覆盖目标：
 * 1) 终止原因优先级裁决 deterministic
 * 2) 同优先级按触发时间裁决
 * 3) 进展向量比较规则（改进/退化/版本切换）
 */

const path = require('path');
const fs = require('fs');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function makeSnapshot(overrides = {}) {
  return {
    snapshotId: 'snap',
    planId: 'plan',
    attemptSeq: 1,
    progressVector: {
      terminalRequiredTodos: 1,
      acceptedCriteria: 1,
      criticalPathResolved: 0.3,
      unresolvedBlockers: 2,
    },
    reviewState: { accepted: 1, total: 3 },
    blockerState: {
      open: 2,
      score: 3,
      externalWaitOpen: 0,
      maxExternalWaitAgeMs: 0,
    },
    budgetState: {
      elapsedMs: 1000,
      tokenUsed: 200,
      errorRate: 0.2,
    },
    cpVersion: 1,
    requiredTotal: 3,
    failedRequired: 0,
    runningOrPendingRequired: 2,
    sourceEventIds: [],
    computedAt: Date.now(),
    ...overrides,
  };
}

function testReasonPriority(mod) {
  const now = Date.now();
  const result = mod.resolveTerminationReason([
    { reason: 'completed', eventId: 'e3', triggeredAt: now + 3 },
    { reason: 'stalled', eventId: 'e2', triggeredAt: now + 2 },
    { reason: 'budget_exceeded', eventId: 'e1', triggeredAt: now + 1 },
  ]);
  assert(result.reason === 'budget_exceeded', '优先级裁决失败：budget_exceeded 应高于 stalled/completed');
  assert(result.evidenceIds.length === 1 && result.evidenceIds[0] === 'e1', '证据链提取失败');
}

function testSamePriorityEarliest(mod) {
  const now = Date.now();
  const result = mod.resolveTerminationReason([
    { reason: 'failed', eventId: 'f2', triggeredAt: now + 5 },
    { reason: 'failed', eventId: 'f1', triggeredAt: now + 1 },
  ]);
  assert(result.reason === 'failed', '同优先级 reason 应保持');
  assert(result.evidenceIds.includes('f1'), '同优先级最早事件未入证据链');
}

function testProgressEvaluation(mod) {
  const prev = makeSnapshot();
  const currImproved = makeSnapshot({
    progressVector: {
      ...prev.progressVector,
      terminalRequiredTodos: prev.progressVector.terminalRequiredTodos + 1,
      unresolvedBlockers: prev.progressVector.unresolvedBlockers - 1,
    },
  });
  const improved = mod.evaluateProgress(prev, currImproved);
  assert(improved.progressed === true, '进展判定失败：应识别为进展');
  assert(improved.regressed === false, '进展判定失败：不应同时退化');

  const currRegressed = makeSnapshot({
    progressVector: {
      ...prev.progressVector,
      unresolvedBlockers: prev.progressVector.unresolvedBlockers + 1,
    },
  });
  const regressed = mod.evaluateProgress(prev, currRegressed);
  assert(regressed.progressed === false, '退化判定失败：不应识别为进展');
  assert(regressed.regressed === true, '退化判定失败：应识别为退化');

  const currRebased = makeSnapshot({
    cpVersion: prev.cpVersion + 1,
    progressVector: { ...prev.progressVector },
  });
  const rebased = mod.evaluateProgress(prev, currRebased);
  assert(rebased.progressed === true, '关键路径重基线后应重置为进展');
  assert(rebased.regressed === false, '关键路径重基线后不应标记退化');
}

function main() {
  const mod = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-termination.js'));

  testReasonPriority(mod);
  testSamePriorityEarliest(mod);
  testProgressEvaluation(mod);

  const report = {
    pass: true,
    checks: [
      'reason_priority',
      'reason_tie_break',
      'progress_vector_evaluation',
    ],
  };
  console.log('\n=== Orchestrator Termination Governance 回归结果 ===');
  console.log(JSON.stringify(report, null, 2));
}

try {
  main();
} catch (error) {
  console.error('\n=== Orchestrator Termination Governance 回归失败 ===');
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}
