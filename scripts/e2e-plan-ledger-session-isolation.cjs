#!/usr/bin/env node
/**
 * Plan Ledger 会话隔离回归
 *
 * 验证 session-A 与 session-B 的计划、索引、事件日志互不污染。
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

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-session-isolation-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);

    const sessionA = sessionManager.createSession('A', 'session-plan-A');
    const sessionB = sessionManager.createSession('B', 'session-plan-B');

    const planA = await ledger.createDraft({
      sessionId: sessionA.id,
      turnId: 'turn-A-1',
      mode: 'standard',
      prompt: '会话 A 计划',
      summary: 'A 计划',
    });

    const planB = await ledger.createDraft({
      sessionId: sessionB.id,
      turnId: 'turn-B-1',
      mode: 'deep',
      prompt: '会话 B 计划',
      summary: 'B 计划',
    });

    await ledger.markExecuting(sessionA.id, planA.planId);
    await ledger.upsertDispatchItem(sessionA.id, planA.planId, {
      itemId: 'A-task-1',
      title: 'A 任务',
      worker: 'codex',
      category: 'simple',
      requiresModification: true,
    });

    const snapshotA = ledger.getSnapshot(sessionA.id);
    const snapshotB = ledger.getSnapshot(sessionB.id);

    assert(snapshotA.plans.length === 1, `sessionA 计划数量异常: ${snapshotA.plans.length}`);
    assert(snapshotB.plans.length === 1, `sessionB 计划数量异常: ${snapshotB.plans.length}`);
    assert(snapshotA.plans[0].planId === planA.planId, 'sessionA 计划 ID 异常');
    assert(snapshotB.plans[0].planId === planB.planId, 'sessionB 计划 ID 异常');
    assert(snapshotA.plans[0].sessionId !== snapshotB.plans[0].sessionId, '计划发生会话污染');

    const sessionAPlanDir = sessionManager.getPlansDir(sessionA.id);
    const sessionBPlanDir = sessionManager.getPlansDir(sessionB.id);
    assert(fs.existsSync(path.join(sessionAPlanDir, `${planA.planId}.json`)), 'sessionA 计划文件缺失');
    assert(fs.existsSync(path.join(sessionBPlanDir, `${planB.planId}.json`)), 'sessionB 计划文件缺失');
    assert(!fs.existsSync(path.join(sessionAPlanDir, `${planB.planId}.json`)), 'sessionA 目录出现 sessionB 计划文件');
    assert(!fs.existsSync(path.join(sessionBPlanDir, `${planA.planId}.json`)), 'sessionB 目录出现 sessionA 计划文件');

    console.log('\n=== plan ledger session isolation regression ===');
    console.log(JSON.stringify({
      sessionA: sessionA.id,
      sessionB: sessionB.id,
      planA: planA.planId,
      planB: planB.planId,
      sessionAPlans: snapshotA.plans.length,
      sessionBPlans: snapshotB.plans.length,
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger session isolation 回归失败:', error?.stack || error);
  process.exit(1);
});
