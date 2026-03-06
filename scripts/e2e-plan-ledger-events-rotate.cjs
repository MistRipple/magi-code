#!/usr/bin/env node
/**
 * Plan Ledger events 文件轮转回归
 *
 * 验证点：
 * 1) events 达到阈值后会触发轮转。
 * 2) 历史轮转文件数量受 keep 限制。
 * 3) 轮转后仍可持续写入新的 events 文件。
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

  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-plan-ledger-rotate-'));

  const originalRotateMax = PlanLedgerService.EVENTS_ROTATE_MAX_BYTES;
  const originalRotateKeep = PlanLedgerService.EVENTS_ROTATE_KEEP_FILES;
  PlanLedgerService.EVENTS_ROTATE_MAX_BYTES = 1024;
  PlanLedgerService.EVENTS_ROTATE_KEEP_FILES = 2;

  try {
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const ledger = new PlanLedgerService(sessionManager);

    const session = sessionManager.createSession('rotate', 'session-plan-ledger-rotate');
    const sessionId = session.id;

    const draft = await ledger.createDraft({
      sessionId,
      turnId: 'turn-rotate-1',
      mode: 'standard',
      prompt: '验证 events 轮转',
      summary: 'events 轮转回归计划',
      constraints: ['轮转策略稳定'],
      acceptanceCriteria: ['触发轮转且保留上限有效'],
    });

    for (let i = 0; i < 80; i++) {
      await ledger.upsertDispatchItem(sessionId, draft.planId, {
        itemId: `task-${i % 3}`,
        title: `轮转回归任务-${i}-${'x'.repeat(96)}`,
        worker: 'orchestrator',
        category: 'simple',
        requiresModification: false,
      });
    }

    await ledger.markExecuting(sessionId, draft.planId);
    await ledger.upsertDispatchItem(sessionId, draft.planId, {
      itemId: 'task-final',
      title: `轮转终态检查-${'y'.repeat(128)}`,
      worker: 'orchestrator',
      category: 'simple',
      requiresModification: false,
    });

    const plansDir = sessionManager.getPlansDir(sessionId);
    const activeEventsFile = path.join(plansDir, `${draft.planId}.events.jsonl`);
    assert(fs.existsSync(activeEventsFile), '缺少当前 events 文件');

    const rotatedFiles = fs.readdirSync(plansDir)
      .filter((name) => {
        const match = new RegExp(`^${draft.planId}\\.events\\.(\\d+)(?:-[a-z0-9]+)?\\.jsonl$`, 'i').exec(name);
        return Boolean(match);
      })
      .sort();

    assert(rotatedFiles.length > 0, '未触发 events 轮转');
    assert(rotatedFiles.length <= 2, `轮转文件超出保留上限: ${rotatedFiles.length}`);

    const activeContent = fs.readFileSync(activeEventsFile, 'utf8').trim();
    assert(activeContent.length > 0, '当前 events 文件为空');

    const totalLines = rotatedFiles.reduce((sum, fileName) => {
      const content = fs.readFileSync(path.join(plansDir, fileName), 'utf8').trim();
      if (!content) return sum;
      return sum + content.split('\n').filter(Boolean).length;
    }, 0) + activeContent.split('\n').filter(Boolean).length;

    assert(totalLines >= 4, `events 总条数异常: ${totalLines}`);

    console.log('\n=== plan ledger events rotate regression ===');
    console.log(JSON.stringify({
      sessionId,
      planId: draft.planId,
      rotatedFiles: rotatedFiles.length,
      keepLimit: 2,
      totalLines,
      pass: true,
    }, null, 2));
  } finally {
    PlanLedgerService.EVENTS_ROTATE_MAX_BYTES = originalRotateMax;
    PlanLedgerService.EVENTS_ROTATE_KEEP_FILES = originalRotateKeep;
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('plan ledger events rotate 回归失败:', error?.stack || error);
  process.exit(1);
});
