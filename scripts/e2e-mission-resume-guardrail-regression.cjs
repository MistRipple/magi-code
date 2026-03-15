#!/usr/bin/env node
/**
 * Mission Resume 守卫回归（运行时优先）
 *
 * 覆盖目标：
 * 1) mission -> ledger 计划恢复必须可运行验证（getLatestPlanByMission）。
 * 2) 恢复 markExecuting 必须具备 CAS 语义，拒绝陈旧 revision。
 * 3) terminal plan 默认不可作为恢复目标（includeTerminal=false）。
 * 4) 引擎恢复链路关键 guard 仍存在（受控失败 + 审计 + 幂等派发）。
 */

const fs = require('fs');
const os = require('os');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');
const ENGINE_PATH = path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts');
const DISPATCH_PATH = path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts');

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

async function testResumeLedgerRuntime() {
  const { UnifiedSessionManager } = loadCompiledModule(path.join('session', 'unified-session-manager.js'));
  const { PlanLedgerService } = loadCompiledModule(path.join('orchestrator', 'plan-ledger', 'plan-ledger-service.js'));

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-resume-guardrail-'));
  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);
    const session = sessionManager.createSession('resume-guardrail', 'session-resume-guardrail');
    const sessionId = session.id;
    const missionId = 'mission-resume-runtime-1';

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-resume-runtime-1',
      missionId,
      mode: 'deep',
      prompt: 'resume runtime regression',
      summary: 'resume runtime regression',
      acceptanceCriteria: ['恢复链路可重建计划上下文'],
    });

    const missingMissionPlan = ledger.getLatestPlanByMission(sessionId, 'missing-mission-id');
    assert(missingMissionPlan === null, '不存在 mission 时应返回 null（fail-closed）');

    const resumePlan = ledger.getLatestPlanByMission(sessionId, missionId);
    assert(resumePlan && resumePlan.planId === draft.planId, 'mission 维度恢复未命中最新计划');

    const marked = await ledger.markExecuting(sessionId, draft.planId, {
      expectedRevision: draft.revision,
      auditReason: 'resume-guardrail:first-markExecuting',
    });
    assert(marked && marked.status === 'executing', 'markExecuting 失败');

    const staleMutation = await ledger.markExecuting(sessionId, draft.planId, {
      expectedRevision: draft.revision,
      auditReason: 'resume-guardrail:stale-markExecuting',
    });
    assert(staleMutation === null, '陈旧 revision 未被拒绝（CAS 失效）');

    const completed = await ledger.finalize(sessionId, draft.planId, 'completed', {
      auditReason: 'resume-guardrail:complete',
    });
    assert(completed && completed.status === 'completed', '计划终态写入失败');

    const latestActiveAfterTerminal = ledger.getLatestPlanByMission(sessionId, missionId);
    assert(latestActiveAfterTerminal === null, 'terminal plan 不应作为默认恢复目标');

    const latestIncludingTerminal = ledger.getLatestPlanByMission(sessionId, missionId, { includeTerminal: true });
    assert(
      latestIncludingTerminal && latestIncludingTerminal.planId === draft.planId,
      'includeTerminal=true 时应可读到 terminal plan',
    );

    return { sessionId, planId: draft.planId };
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

function testSourceLevelGuards() {
  const source = fs.readFileSync(ENGINE_PATH, 'utf8');
  const dispatchSource = fs.readFileSync(DISPATCH_PATH, 'utf8');

  assert(
    source.includes('缺少可恢复计划，已终止恢复执行'),
    '恢复计划缺失的 fail-closed 守卫缺失',
  );
  assert(
    source.includes("auditReason: 'resume-mission-ledger-recovery'"),
    '恢复 markExecuting 缺少审计原因',
  );
  assert(
    source.includes("op: 'resume-markExecuting'"),
    '恢复 markExecuting 缺少关键写入失败守卫',
  );
  assert(
    source.includes('resolveExecutionFinalStatus('),
    '终态保持链路缺少统一终态决策入口',
  );
  assert(
    dispatchSource.includes('dispatchIdempotencyStore.claimOrGet'),
    '恢复派发链路缺少幂等占位保护',
  );
}

async function main() {
  const runtime = await testResumeLedgerRuntime();
  testSourceLevelGuards();

  console.log('\n=== mission resume guardrail regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'ledger_runtime_resume_lookup',
      'resume_missing_plan_fail_closed_runtime',
      'resume_mark_executing_cas_runtime',
      'resume_terminal_plan_excluded_by_default',
      'resume_source_guardrails_present',
      'resume_dispatch_idempotency_guarded',
    ],
    sessionId: runtime.sessionId,
    planId: runtime.planId,
  }, null, 2));
}

main().catch((error) => {
  console.error('mission resume guardrail 回归失败:', error?.stack || error);
  process.exit(1);
});
