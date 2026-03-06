#!/usr/bin/env node
/**
 * Plan Ledger 生命周期回归
 *
 * 验证链路：draft -> awaiting_confirmation -> approved -> executing -> completed
 * 并校验 dispatch/todo 回写、事件日志落盘。
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

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-lifecycle-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);

    const session = sessionManager.createSession('lifecycle', 'session-plan-ledger-lifecycle');
    const sessionId = session.id;

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-lifecycle-1',
      mode: 'standard',
      prompt: '验证计划生命周期',
      summary: '生命周期回归计划',
      constraints: ['保持链路稳定'],
      acceptanceCriteria: ['状态可闭环'],
    });

    assert(draft.status === 'draft', 'draft 状态异常');

    await ledger.markAwaitingConfirmation(sessionId, draft.planId, '## 回归计划');
    await ledger.approve(sessionId, draft.planId, 'tester');
    await ledger.markExecuting(sessionId, draft.planId);

    await ledger.upsertDispatchItem(sessionId, draft.planId, {
      itemId: 'task-1',
      title: '执行子任务',
      worker: 'claude',
      category: 'simple',
      requiresModification: true,
    });

    await ledger.bindAssignmentTodos(sessionId, draft.planId, 'task-1', [
      { id: 'todo-1', status: 'pending' },
    ]);

    await ledger.updateTodoStatus(sessionId, draft.planId, 'task-1', 'todo-1', 'running');
    await ledger.updateTodoStatus(sessionId, draft.planId, 'task-1', 'todo-1', 'completed');
    await ledger.updateAssignmentStatus(sessionId, draft.planId, 'task-1', 'completed');

    await ledger.finalize(sessionId, draft.planId, 'completed');

    const latest = ledger.getLatestPlan(sessionId);
    assert(latest, '未找到最新计划');
    assert(latest.status === 'completed', `计划终态异常: ${latest.status}`);

    const eventsFile = path.join(sessionManager.getPlansDir(sessionId), `${draft.planId}.events.jsonl`);
    assert(fs.existsSync(eventsFile), '缺少计划事件日志文件');
    const eventLines = fs.readFileSync(eventsFile, 'utf8').trim().split('\n').filter(Boolean);
    assert(eventLines.length >= 5, '计划事件日志条数异常');

    console.log('\n=== plan ledger lifecycle regression ===');
    console.log(JSON.stringify({
      sessionId,
      planId: draft.planId,
      status: latest.status,
      eventCount: eventLines.length,
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger lifecycle 回归失败:', error?.stack || error);
  process.exit(1);
});
