#!/usr/bin/env node
/**
 * Plan Ledger Attempt 状态机回归
 *
 * 验证目标：
 * 1) created -> inflight -> terminal 转移可用。
 * 2) 同 scope/target 的 sequence 正确递增。
 * 3) Plan finalize 会收敛残留 inflight Attempt。
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

function pickLatest(attempts, scope, targetId) {
  return attempts
    .filter((item) => item.scope === scope && item.targetId === targetId)
    .sort((a, b) => b.sequence - a.sequence)[0];
}

async function main() {
  const planLedgerOut = path.join(OUT, 'orchestrator', 'plan-ledger', 'plan-ledger-service.js');
  const sessionManagerOut = path.join(OUT, 'session', 'unified-session-manager.js');
  if (!fs.existsSync(planLedgerOut) || !fs.existsSync(sessionManagerOut)) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { UnifiedSessionManager } = require(sessionManagerOut);
  const { PlanLedgerService } = require(planLedgerOut);

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-attempt-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);
    const session = sessionManager.createSession('attempt-lifecycle', 'session-plan-ledger-attempt');
    const sessionId = session.id;

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-attempt-1',
      mode: 'standard',
      prompt: '验证 attempt 状态机',
      summary: 'attempt 状态机回归计划',
    });

    await ledger.approve(sessionId, draft.planId, 'tester');
    await ledger.markExecuting(sessionId, draft.planId);

    await ledger.startAttempt(sessionId, draft.planId, {
      scope: 'orchestrator',
      targetId: 'orchestrator-turn-attempt-1',
      reason: 'orchestrator-started',
    });

    await ledger.startAttempt(sessionId, draft.planId, {
      scope: 'assignment',
      targetId: 'assignment-1',
      assignmentId: 'assignment-1',
      reason: 'assignment-1-started',
    });

    await ledger.startAttempt(sessionId, draft.planId, {
      scope: 'todo',
      targetId: 'todo-1',
      assignmentId: 'assignment-1',
      todoId: 'todo-1',
      reason: 'todo-1-started',
    });

    await ledger.completeLatestAttempt(sessionId, draft.planId, {
      scope: 'todo',
      targetId: 'todo-1',
      assignmentId: 'assignment-1',
      todoId: 'todo-1',
      status: 'succeeded',
      reason: 'todo-1-completed',
    });
    // 重复终态事件应幂等合并，不能生成新的 attempt sequence
    await ledger.completeLatestAttempt(sessionId, draft.planId, {
      scope: 'todo',
      targetId: 'todo-1',
      assignmentId: 'assignment-1',
      todoId: 'todo-1',
      status: 'succeeded',
      reason: 'todo-1-completed-duplicate',
      evidenceIds: ['dup-event-1'],
    });

    await ledger.completeLatestAttempt(sessionId, draft.planId, {
      scope: 'assignment',
      targetId: 'assignment-1',
      assignmentId: 'assignment-1',
      status: 'timeout',
      reason: 'assignment-timeout',
      error: 'worker timed out',
    });

    await ledger.startAttempt(sessionId, draft.planId, {
      scope: 'assignment',
      targetId: 'assignment-1',
      assignmentId: 'assignment-1',
      reason: 'assignment-retry',
    });
    await ledger.completeLatestAttempt(sessionId, draft.planId, {
      scope: 'assignment',
      targetId: 'assignment-1',
      assignmentId: 'assignment-1',
      status: 'succeeded',
      reason: 'assignment-retry-completed',
    });

    await ledger.startAttempt(sessionId, draft.planId, {
      scope: 'todo',
      targetId: 'todo-inflight',
      assignmentId: 'assignment-2',
      todoId: 'todo-inflight',
      reason: 'todo-inflight-started',
    });

    await ledger.finalize(sessionId, draft.planId, 'failed');

    const latest = ledger.getLatestPlan(sessionId);
    assert(latest, '缺少 latest plan');
    assert(latest.status === 'failed', `计划终态异常: ${latest.status}`);

    const assignmentLatest = pickLatest(latest.attempts, 'assignment', 'assignment-1');
    assert(assignmentLatest, '缺少 assignment attempt');
    assert(assignmentLatest.sequence === 2, `assignment attempt sequence 异常: ${assignmentLatest.sequence}`);
    assert(assignmentLatest.status === 'succeeded', `assignment 最新 attempt 状态异常: ${assignmentLatest.status}`);

    const timeoutAttempt = latest.attempts.find((item) =>
      item.scope === 'assignment' && item.targetId === 'assignment-1' && item.sequence === 1);
    assert(timeoutAttempt && timeoutAttempt.status === 'timeout', 'assignment 首次 attempt 未进入 timeout');

    const todoCompleted = pickLatest(latest.attempts, 'todo', 'todo-1');
    assert(todoCompleted && todoCompleted.status === 'succeeded', 'todo-1 attempt 未成功');
    const todoOneAttempts = latest.attempts.filter((item) => item.scope === 'todo' && item.targetId === 'todo-1');
    assert(todoOneAttempts.length === 1, `todo-1 出现重复 attempt: ${todoOneAttempts.length}`);
    assert(todoCompleted.evidenceIds.includes('dup-event-1'), '重复终态事件证据未合并');

    const lingeringInflight = latest.attempts.filter((item) => item.status === 'inflight' || item.status === 'created');
    assert(lingeringInflight.length === 0, `finalize 后仍有 inflight attempt: ${lingeringInflight.length}`);

    const inflightTodo = pickLatest(latest.attempts, 'todo', 'todo-inflight');
    assert(inflightTodo && inflightTodo.status === 'failed', `残留 inflight todo 未被收敛: ${inflightTodo?.status}`);

    const eventsFile = path.join(sessionManager.getPlansDir(sessionId), `${draft.planId}.events.jsonl`);
    assert(fs.existsSync(eventsFile), '缺少 attempt events 文件');
    const eventLines = fs.readFileSync(eventsFile, 'utf8').trim().split('\n').filter(Boolean);
    assert(eventLines.some((line) => line.includes('"reason":"attempt-started"')), '缺少 attempt-started 事件');
    assert(eventLines.some((line) => line.includes('"reason":"attempt-timeout"')), '缺少 attempt-timeout 事件');

    console.log('\n=== plan ledger attempt lifecycle regression ===');
    console.log(JSON.stringify({
      sessionId,
      planId: draft.planId,
      attempts: latest.attempts.length,
      status: latest.status,
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger attempt lifecycle 回归失败:', error?.stack || error);
  process.exit(1);
});
