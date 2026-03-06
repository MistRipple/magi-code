#!/usr/bin/env node
/**
 * Plan Ledger 对账回归
 *
 * 验证会话恢复时，计划状态可根据 Mission 终态自动对账并收敛。
 */

const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

async function main() {
  const planLedgerOut = path.join(OUT, 'orchestrator', 'plan-ledger', 'plan-ledger-service.js');
  const sessionManagerOut = path.join(OUT, 'session', 'unified-session-manager.js');
  if (!fs.existsSync(planLedgerOut) || !fs.existsSync(sessionManagerOut)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { UnifiedSessionManager } = require(sessionManagerOut);
  const { PlanLedgerService } = require(planLedgerOut);

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-reconcile-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);

    const session = sessionManager.createSession('reconcile', 'session-plan-reconcile');

    const completedPlan = await ledger.createDraft({
      sessionId: session.id,
      turnId: 'turn-reconcile-1',
      missionId: 'mission-reconcile-1',
      mode: 'standard',
      prompt: '完成态对账',
      summary: '完成态计划',
    });

    await ledger.markExecuting(session.id, completedPlan.planId);
    await ledger.upsertDispatchItem(session.id, completedPlan.planId, {
      itemId: 'task-reconcile-1',
      title: '任务一',
      worker: 'claude',
      category: 'simple',
      requiresModification: true,
    });

    const failedPlan = await ledger.createDraft({
      sessionId: session.id,
      turnId: 'turn-reconcile-2',
      missionId: 'mission-reconcile-2',
      mode: 'standard',
      prompt: '失败态对账',
      summary: '失败态计划',
    });

    await ledger.markExecuting(session.id, failedPlan.planId);
    await ledger.upsertDispatchItem(session.id, failedPlan.planId, {
      itemId: 'task-reconcile-2',
      title: '任务二',
      worker: 'gemini',
      category: 'debug',
      requiresModification: true,
    });

    await ledger.updateAssignmentStatus(session.id, failedPlan.planId, 'task-reconcile-2', 'running');

    const reconciledCount = await ledger.reconcileByMissions(session.id, [
      { id: 'mission-reconcile-1', status: 'completed' },
      { id: 'mission-reconcile-2', status: 'failed' },
    ]);

    assert(reconciledCount >= 2, `对账更新数量异常: ${reconciledCount}`);

    const latestCompleted = ledger.getPlan(session.id, completedPlan.planId);
    const latestFailed = ledger.getPlan(session.id, failedPlan.planId);

    assert(latestCompleted?.status === 'completed', `完成态计划对账异常: ${latestCompleted?.status}`);
    assert(latestFailed?.status === 'failed', `失败态计划对账异常: ${latestFailed?.status}`);

    console.log('\n=== plan ledger reconcile regression ===');
    console.log(JSON.stringify({
      sessionId: session.id,
      reconciledCount,
      completedPlanStatus: latestCompleted?.status,
      failedPlanStatus: latestFailed?.status,
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger reconcile 回归失败:', error?.stack || error);
  process.exit(1);
});
