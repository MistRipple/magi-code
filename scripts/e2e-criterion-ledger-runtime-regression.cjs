#!/usr/bin/env node
/**
 * Criterion 级验收账本回归
 *
 * 覆盖目标：
 * 1) runtime.acceptance.criteria 可持久化 evidence/owner/scope/reviewHistory/batch/worker 元信息。
 * 2) 当仅更新 criteria 且未显式传 summary 时，summary 能基于 criterion 状态重算。
 * 3) mission-driven-engine 已将 specResults 映射到 criterion 级写入路径。
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
    throw new Error('缺少 out 编译产物，请先执行 npm run -s compile');
  }

  const { UnifiedSessionManager } = require(sessionManagerOut);
  const { PlanLedgerService } = require(planLedgerOut);

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-criterion-ledger-'));

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);
    const session = sessionManager.createSession('criterion-ledger', 'session-criterion-ledger');
    const sessionId = session.id;

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-criterion-ledger-1',
      mode: 'deep',
      prompt: 'criterion ledger regression',
      summary: 'criterion ledger regression',
      acceptanceCriteria: ['验证 A', '验证 B'],
    });
    assert(draft.runtime.acceptance.criteria.length === 2, '初始 criterion 数量异常');

    const first = draft.runtime.acceptance.criteria[0];
    const second = draft.runtime.acceptance.criteria[1];
    const now = Date.now();
    const updated = await ledger.updateRuntimeState(sessionId, draft.planId, {
      acceptance: {
        criteria: [
          {
            ...first,
            status: 'failed',
            evidence: ['spec:验证 A 未通过'],
            owner: 'claude',
            scope: 'batch_verification',
            lastBatchId: 'batch-1',
            lastWorkerId: 'claude',
            reviewHistory: [{
              status: 'failed',
              reviewer: 'system:spec-verifier',
              detail: '验证 A 未通过',
              reviewedAt: now,
              round: 1,
              batchId: 'batch-1',
              workerId: 'claude',
            }],
          },
          {
            ...second,
            status: 'passed',
            evidence: ['spec:验证 B 通过'],
            owner: 'codex',
            scope: 'batch_verification',
            lastBatchId: 'batch-1',
            lastWorkerId: 'codex',
            reviewHistory: [{
              status: 'passed',
              reviewer: 'system:spec-verifier',
              detail: '验证 B 通过',
              reviewedAt: now,
              round: 1,
              batchId: 'batch-1',
              workerId: 'codex',
            }],
          },
        ],
      },
    });
    assert(updated, 'criteria 元信息写入失败');
    assert(updated.runtime.acceptance.summary === 'failed', `criteria 重算 summary 失败: ${updated.runtime.acceptance.summary}`);

    const persisted = ledger.getPlan(sessionId, draft.planId);
    assert(persisted, '读取持久化计划失败');
    const persistedFirst = persisted.runtime.acceptance.criteria[0];
    assert(Array.isArray(persistedFirst.evidence) && persistedFirst.evidence.includes('spec:验证 A 未通过'), 'evidence 未持久化');
    assert(persistedFirst.owner === 'claude', 'owner 未持久化');
    assert(persistedFirst.scope === 'batch_verification', 'scope 未持久化');
    assert(persistedFirst.lastBatchId === 'batch-1', 'lastBatchId 未持久化');
    assert(persistedFirst.lastWorkerId === 'claude', 'lastWorkerId 未持久化');
    assert(Array.isArray(persistedFirst.reviewHistory) && persistedFirst.reviewHistory.length === 1, 'reviewHistory 未持久化');

    const scopedCriteria = await ledger.updateRuntimeState(sessionId, draft.planId, {
      acceptance: {
        criteria: [
          {
            id: 'acceptance-scope-api',
            description: '共享验收条目',
            verifiable: true,
            verificationMethod: 'auto',
            status: 'pending',
            owner: 'claude',
            scope: 'api',
          },
          {
            id: 'acceptance-scope-ui',
            description: '共享验收条目',
            verifiable: true,
            verificationMethod: 'auto',
            status: 'pending',
            owner: 'claude',
            scope: 'ui',
          },
        ],
      },
    });
    assert(scopedCriteria, 'scope 去重回归写入失败');
    assert(scopedCriteria.runtime.acceptance.criteria.length === 2, '同描述不同 scope 的 criterion 被错误去重');
    const criterionScopes = new Set(scopedCriteria.runtime.acceptance.criteria.map((criterion) => criterion.scope));
    assert(criterionScopes.has('api') && criterionScopes.has('ui'), 'scope 维度未完整保留');

    const allPassed = await ledger.updateRuntimeState(sessionId, draft.planId, {
      acceptance: {
        criteria: scopedCriteria.runtime.acceptance.criteria.map((criterion) => ({
          ...criterion,
          status: 'passed',
        })),
      },
    });
    assert(allPassed, '全量 passed 写入失败');
    assert(allPassed.runtime.acceptance.summary === 'passed', `all passed 后 summary 异常: ${allPassed.runtime.acceptance.summary}`);

    const engineSource = fs.readFileSync(
      path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'),
      'utf8',
    );
    assert(engineSource.includes('mergeAcceptanceCriteriaWithSpecResults'), '缺少 specResults -> criterion 映射方法');
    assert(engineSource.includes('specResults: outcome.specResults'), '缺少 specResults 映射调用');

    console.log('\n=== criterion ledger runtime regression ===');
    console.log(JSON.stringify({
      pass: true,
      sessionId,
      planId: draft.planId,
      summaryAfterMetaPatch: updated.runtime.acceptance.summary,
      summaryAfterAllPassed: allPassed.runtime.acceptance.summary,
    }, null, 2));
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('criterion ledger runtime 回归失败:', error?.stack || error);
  process.exit(1);
});
