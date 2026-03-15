#!/usr/bin/env node
/**
 * Plan Ledger 并发与状态约束回归
 *
 * 覆盖目标：
 * 1) CAS(expectedRevision) 冲突必须 fail-closed（返回 null + 审计事件）
 * 2) 非法计划状态迁移必须被拒绝并落审计
 * 3) 非法 runtime.review 状态迁移必须被拒绝并落审计
 * 4) schema N/N-1 在线迁移必须自动回写并记录迁移事件
 * 5) 不受支持的 schema 版本必须 fail-closed（拒绝加载）
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

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-guardrails-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);
    const session = sessionManager.createSession('guardrails', 'session-plan-ledger-guardrails');
    const sessionId = session.id;
    const plansDir = sessionManager.getPlansDir(sessionId);

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-guardrails-1',
      mode: 'deep',
      prompt: '验证 plan-ledger 并发和状态约束',
      summary: 'plan-ledger guardrails 回归',
      acceptanceCriteria: ['拒绝非法迁移并保留审计轨迹'],
    });
    assert(draft.status === 'draft', '初始 draft 状态异常');

    const approved = await ledger.approve(
      sessionId,
      draft.planId,
      'tester',
      'guardrails-approve',
      {
        expectedRevision: draft.revision,
        auditReason: 'regression:approve',
      },
    );
    assert(approved && approved.status === 'approved', 'approve 失败');

    const casConflict = await ledger.markExecuting(sessionId, draft.planId, {
      expectedRevision: draft.revision,
      auditReason: 'regression:stale-cas',
    });
    assert(casConflict === null, 'CAS 冲突未被拒绝');

    const afterCas = ledger.getPlan(sessionId, draft.planId);
    assert(afterCas && afterCas.status === 'approved', `CAS 冲突后状态异常: ${afterCas?.status}`);

    const invalidPlanTransition = await ledger.markAwaitingConfirmation(
      sessionId,
      draft.planId,
      '## invalid-transition',
      {
        expectedRevision: afterCas.revision,
        auditReason: 'regression:invalid-plan-transition',
      },
    );
    assert(invalidPlanTransition === null, '非法计划状态迁移未被拒绝');

    const afterInvalidPlanTransition = ledger.getPlan(sessionId, draft.planId);
    assert(
      afterInvalidPlanTransition && afterInvalidPlanTransition.status === 'approved',
      `非法迁移后计划状态异常: ${afterInvalidPlanTransition?.status}`,
    );

    const reviewAccepted = await ledger.updateRuntimeState(
      sessionId,
      draft.planId,
      {
        review: {
          state: 'accepted',
          reason: 'regression:review-accepted',
        },
      },
      {
        expectedRevision: afterInvalidPlanTransition.revision,
        auditReason: 'regression:review-accepted',
      },
    );
    assert(reviewAccepted && reviewAccepted.runtime.review.state === 'accepted', 'review accepted 写入失败');

    const invalidReviewTransition = await ledger.updateRuntimeState(
      sessionId,
      draft.planId,
      {
        review: {
          state: 'rejected',
          reason: 'regression:invalid-review-transition',
        },
      },
      {
        expectedRevision: reviewAccepted.revision,
        auditReason: 'regression:invalid-review-transition',
      },
    );
    assert(invalidReviewTransition === null, '非法 runtime.review 迁移未被拒绝');

    const latest = ledger.getPlan(sessionId, draft.planId);
    assert(latest && latest.runtime.review.state === 'accepted', '非法 runtime.review 迁移后状态被污染');

    const planFile = path.join(plansDir, `${draft.planId}.json`);
    assert(fs.existsSync(planFile), '缺少计划文件，无法执行 schema 迁移回归');
    const legacyPlan = JSON.parse(fs.readFileSync(planFile, 'utf8'));
    delete legacyPlan.runtime.acceptance;
    legacyPlan.acceptanceCriteria = ['legacy-schema-migration'];
    legacyPlan.schemaVersion = 1;
    fs.writeFileSync(planFile, JSON.stringify(legacyPlan, null, 2));

    // 通过新实例绕过内存缓存，验证磁盘加载触发在线迁移
    const migrationReader = new PlanLedgerService(sessionManager);
    const migrated = migrationReader.getPlan(sessionId, draft.planId);
    assert(migrated && migrated.schemaVersion === 2, `legacy schema 未迁移到当前版本: ${migrated?.schemaVersion}`);
    assert(migrated.runtime.acceptance.criteria.length === 1, 'legacy acceptanceCriteria 未迁移到 runtime.acceptance');
    const persistedAfterMigration = JSON.parse(fs.readFileSync(planFile, 'utf8'));
    assert(persistedAfterMigration.schemaVersion === 2, 'schema 在线迁移后未回写计划文件');

    const unsupportedPlanId = `${draft.planId}-unsupported-schema`;
    const unsupportedPlanFile = path.join(plansDir, `${unsupportedPlanId}.json`);
    const unsupportedPlan = {
      ...persistedAfterMigration,
      planId: unsupportedPlanId,
      schemaVersion: 99,
      revision: 1,
      version: 1,
      updatedAt: Date.now(),
    };
    fs.writeFileSync(unsupportedPlanFile, JSON.stringify(unsupportedPlan, null, 2));
    const unsupportedLoaded = migrationReader.getPlan(sessionId, unsupportedPlanId);
    assert(unsupportedLoaded === null, '不受支持 schema 版本未 fail-closed');

    const eventsFile = path.join(plansDir, `${draft.planId}.events.jsonl`);
    assert(fs.existsSync(eventsFile), '缺少事件日志文件');
    const eventLines = fs.readFileSync(eventsFile, 'utf8').trim().split('\n').filter(Boolean);

    const hasRevisionConflictAudit = eventLines.some((line) => line.includes('"reason":"audit:revision_conflict:'));
    const hasInvalidPlanTransitionAudit = eventLines.some((line) => line.includes('"reason":"audit:invalid_plan_status_transition:'));
    const hasInvalidReviewTransitionAudit = eventLines.some((line) => line.includes('"reason":"audit:invalid_runtime_review_transition:'));
    const hasSchemaMigratedEvent = eventLines.some((line) => line.includes('"reason":"schema-migrated:1->2"'));

    assert(hasRevisionConflictAudit, '缺少 revision_conflict 审计事件');
    assert(hasInvalidPlanTransitionAudit, '缺少 invalid_plan_status_transition 审计事件');
    assert(hasInvalidReviewTransitionAudit, '缺少 invalid_runtime_review_transition 审计事件');
    assert(hasSchemaMigratedEvent, '缺少 schema-migrated 在线迁移事件');

    console.log('\n=== plan ledger guardrails regression ===');
    console.log(JSON.stringify({
      sessionId,
      planId: draft.planId,
      status: migrated.status,
      reviewState: migrated.runtime.review.state,
      schemaVersion: migrated.schemaVersion,
      audits: {
        revisionConflict: hasRevisionConflictAudit,
        invalidPlanStatusTransition: hasInvalidPlanTransitionAudit,
        invalidRuntimeReviewTransition: hasInvalidReviewTransitionAudit,
        schemaMigrated: hasSchemaMigratedEvent,
      },
      pass: true,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger guardrails 回归失败:', error?.stack || error);
  process.exit(1);
});
