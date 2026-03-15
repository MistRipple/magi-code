#!/usr/bin/env node
/**
 * 恢复决策内核回归脚本
 *
 * 覆盖目标：
 * 1) Replan 信号必须联动 budget/scope/acceptance/blocker/progress
 * 2) 恢复动作裁决必须统一且可预测（auto_repair/auto_resume/ask/auto_followup/pause）
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

function testDeriveSignals(mod) {
  const signals = mod.deriveReplanGateSignals({
    runtimeReason: 'failed',
    runtimeSnapshot: {
      requiredTotal: 4,
      failedRequired: 2,
      runningOrPendingRequired: 2,
      reviewState: { accepted: 1, total: 4 },
      blockerState: { open: 1, externalWaitOpen: 1 },
      budgetState: { errorRate: 0.6 },
    },
    auditOutcome: {
      issues: [{ dimension: 'scope', detail: '跨模块影响扩大' }],
    },
  });

  assert(signals.budgetPressure === true, 'signals.budgetPressure 期望为 true');
  assert(signals.scopeExpansion === true, 'signals.scopeExpansion 期望为 true');
  assert(signals.acceptanceFailure === true, 'signals.acceptanceFailure 期望为 true');
  assert(signals.blockerPressure === true, 'signals.blockerPressure 期望为 true');
  assert(signals.progressStalled === true, 'signals.progressStalled 期望为 true');
}

function testRecoveryDecisions(mod) {
  const baseSignals = {
    budgetPressure: false,
    scopeExpansion: false,
    scopeIssues: [],
    acceptanceFailure: false,
    blockerPressure: false,
    progressStalled: false,
    pendingRequiredTodos: 0,
    failedRequiredTodos: 0,
    unresolvedBlockers: 0,
    externalWaitOpen: 0,
  };

  const autoRepair = mod.decideRecoveryAction({
    currentPlanMode: 'deep',
    interactionMode: 'auto',
    isGovernancePaused: false,
    governanceReason: 'failed',
    governanceRecoveryAttempt: 0,
    governanceRecoveryMaxRounds: 2,
    deliveryFailed: true,
    continuationPolicy: 'auto',
    canAutoRepairByRounds: true,
    autoRepairStalled: false,
    hasFollowUpPending: false,
    followUpSignatureChanged: false,
    followUpStallStreak: 0,
    blockedFollowUpOnly: false,
    signals: baseSignals,
  });
  assert(autoRepair.action === 'auto_repair', `autoRepair.action 异常: ${autoRepair.action}`);

  const autoResume = mod.decideRecoveryAction({
    currentPlanMode: 'deep',
    interactionMode: 'auto',
    isGovernancePaused: true,
    governanceReason: 'external_wait_timeout',
    governanceRecoveryAttempt: 0,
    governanceRecoveryMaxRounds: 2,
    deliveryFailed: false,
    continuationPolicy: 'stop',
    canAutoRepairByRounds: false,
    autoRepairStalled: false,
    hasFollowUpPending: false,
    followUpSignatureChanged: false,
    followUpStallStreak: 0,
    blockedFollowUpOnly: false,
    signals: baseSignals,
  });
  assert(autoResume.action === 'auto_governance_resume', `autoResume.action 异常: ${autoResume.action}`);

  const askDecision = mod.decideRecoveryAction({
    currentPlanMode: 'deep',
    interactionMode: 'ask',
    isGovernancePaused: false,
    governanceReason: 'failed',
    governanceRecoveryAttempt: 0,
    governanceRecoveryMaxRounds: 2,
    deliveryFailed: false,
    continuationPolicy: 'stop',
    canAutoRepairByRounds: false,
    autoRepairStalled: false,
    hasFollowUpPending: false,
    followUpSignatureChanged: false,
    followUpStallStreak: 0,
    blockedFollowUpOnly: false,
    signals: {
      ...baseSignals,
      acceptanceFailure: true,
      failedRequiredTodos: 1,
    },
  });
  assert(askDecision.action === 'ask_followup_confirmation', `askDecision.action 异常: ${askDecision.action}`);
  assert(askDecision.replanSource === 'acceptance_failure', `askDecision.replanSource 异常: ${askDecision.replanSource}`);

  const autoFollowup = mod.decideRecoveryAction({
    currentPlanMode: 'deep',
    interactionMode: 'auto',
    isGovernancePaused: false,
    governanceReason: 'completed',
    governanceRecoveryAttempt: 0,
    governanceRecoveryMaxRounds: 2,
    deliveryFailed: false,
    continuationPolicy: 'stop',
    canAutoRepairByRounds: false,
    autoRepairStalled: false,
    hasFollowUpPending: true,
    followUpSignatureChanged: true,
    followUpStallStreak: 0,
    blockedFollowUpOnly: false,
    signals: {
      ...baseSignals,
      pendingRequiredTodos: 2,
    },
  });
  assert(autoFollowup.action === 'auto_followup', `autoFollowup.action 异常: ${autoFollowup.action}`);

  const pauseDecision = mod.decideRecoveryAction({
    currentPlanMode: 'deep',
    interactionMode: 'auto',
    isGovernancePaused: true,
    governanceReason: 'stalled',
    governanceRecoveryAttempt: 2,
    governanceRecoveryMaxRounds: 2,
    deliveryFailed: false,
    continuationPolicy: 'stop',
    canAutoRepairByRounds: false,
    autoRepairStalled: false,
    hasFollowUpPending: false,
    followUpSignatureChanged: false,
    followUpStallStreak: 0,
    blockedFollowUpOnly: false,
    signals: {
      ...baseSignals,
      progressStalled: true,
    },
  });
  assert(pauseDecision.action === 'pause', `pauseDecision.action 异常: ${pauseDecision.action}`);
}

function main() {
  const mod = loadCompiledModule(path.join('orchestrator', 'core', 'recovery-decision-kernel.js'));
  testDeriveSignals(mod);
  testRecoveryDecisions(mod);

  console.log('\n=== recovery decision kernel regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'replan-signals-linked',
      'auto-repair-decision',
      'auto-governance-resume-decision',
      'ask-confirmation-decision',
      'auto-followup-decision',
      'pause-decision',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('recovery decision kernel 回归失败:', error?.stack || error);
  process.exit(1);
}
