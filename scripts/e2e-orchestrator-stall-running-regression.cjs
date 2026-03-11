#!/usr/bin/env node
/**
 * Orchestrator stalled 门禁（运行中不误停）回归
 *
 * 目标：
 * 1) required todo 仍有 running 时，不应触发 stalled
 * 2) required todo 无 running 且无 external_wait 时，可触发 stalled
 *
 * 运行：
 *   npm run -s compile
 *   node scripts/e2e-orchestrator-stall-running-regression.cjs
 */

const fs = require('fs');
const path = require('path');

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
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function makeSnapshot(overrides = {}) {
  return {
    snapshotId: 'snap-stall',
    planId: 'plan-stall',
    attemptSeq: overrides.attemptSeq ?? 5,
    progressVector: {
      terminalRequiredTodos: overrides.terminalRequiredTodos ?? 0,
      acceptedCriteria: overrides.acceptedCriteria ?? 0,
      criticalPathResolved: overrides.criticalPathResolved ?? 0,
      unresolvedBlockers: overrides.unresolvedBlockers ?? 0,
    },
    reviewState: { accepted: 0, total: 2 },
    blockerState: {
      open: overrides.blockerOpen ?? 0,
      score: overrides.blockerScore ?? 0,
      externalWaitOpen: overrides.externalWaitOpen ?? 0,
      maxExternalWaitAgeMs: overrides.maxExternalWaitAgeMs ?? 0,
    },
    budgetState: {
      elapsedMs: overrides.elapsedMs ?? 1000,
      tokenUsed: overrides.tokenUsed ?? 200,
      errorRate: overrides.errorRate ?? 0,
    },
    cpVersion: 1,
    requiredTotal: overrides.requiredTotal ?? 2,
    failedRequired: overrides.failedRequired ?? 0,
    runningOrPendingRequired: overrides.runningOrPendingRequired ?? 2,
    runningRequired: overrides.runningRequired ?? 0,
    sourceEventIds: [],
    computedAt: Date.now(),
  };
}

function main() {
  const { OrchestratorDecisionEngine } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-decision-engine.js'));

  const policy = {
    stalledWindowSize: 3,
    externalWaitSlaMs: 120000,
    upstreamModelErrorStreak: 3,
    errorRateMinSamples: 2,
    budgetBreachStreakThreshold: 2,
    externalWaitBreachStreakThreshold: 2,
    budgetHardLimitFactor: 2,
    externalWaitHardLimitFactor: 2,
  };
  const engine = new OrchestratorDecisionEngine(policy);
  const budget = { maxDurationMs: 120000, maxTokenUsage: 100000, maxErrorRate: 0.9 };

  const gateState = {
    noProgressStreak: policy.stalledWindowSize,
    consecutiveUpstreamModelErrors: 0,
    budgetBreachStreak: 0,
    externalWaitBreachStreak: 0,
  };

  const snapshotRunning = makeSnapshot({ runningRequired: 1, runningOrPendingRequired: 1 });
  const resRunning = engine.collectBudgetCandidates({
    snapshot: snapshotRunning,
    budget,
    gateState,
    createCandidate: (reason, label) => ({ reason, eventId: label, triggeredAt: Date.now() }),
  });
  assert(!resRunning.candidates.some(c => c.reason === 'stalled'), 'runningRequired>0 不应触发 stalled');

  const snapshotIdle = makeSnapshot({ runningRequired: 0, runningOrPendingRequired: 1 });
  const resIdle = engine.collectBudgetCandidates({
    snapshot: snapshotIdle,
    budget,
    gateState,
    createCandidate: (reason, label) => ({ reason, eventId: label, triggeredAt: Date.now() }),
  });
  assert(resIdle.candidates.some(c => c.reason === 'stalled'), '无 running 且无 external_wait 时应触发 stalled');

  console.log('\n=== orchestrator stalled running regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'running_required_no_stall',
      'idle_required_stall',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('orchestrator stalled running 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}
