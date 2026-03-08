#!/usr/bin/env node
/**
 * 生成终止指标样本（用于 CI 发布闸门冷启动与回归演练）
 *
 * 默认输出：
 * - .magi/metrics/termination.jsonl
 *
 * 默认策略：
 * - 生成 24 条 completed + 6 条 failed（总计 30）
 * - shadow 全量启用且 consistent=true
 * - 默认覆盖输出文件（可通过 MAGI_TERMINATION_SEED_APPEND=1 追加）
 */

const fs = require('fs');
const path = require('path');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const root = path.resolve(__dirname, '..');
  const metricsDir = path.join(root, '.magi', 'metrics');
  const outputPath = process.env.MAGI_TERMINATION_CANDIDATE_PATH
    || path.join(metricsDir, 'termination.jsonl');
  const total = Number(process.env.MAGI_TERMINATION_SEED_TOTAL || '30');
  const completedRatio = Number(process.env.MAGI_TERMINATION_SEED_COMPLETED_RATIO || '0.8');
  const append = process.env.MAGI_TERMINATION_SEED_APPEND === '1';

  assert(Number.isFinite(total) && total > 0, `无效样本总数: ${total}`);
  assert(Number.isFinite(completedRatio) && completedRatio > 0 && completedRatio < 1, `无效 completed 比例: ${completedRatio}`);

  const completedCount = Math.max(1, Math.min(total - 1, Math.floor(total * completedRatio)));
  const failedCount = total - completedCount;
  const lines = [];
  const now = Date.now();

  for (let i = 0; i < total; i++) {
    const isCompleted = i < completedCount;
    const reason = isCompleted ? 'completed' : 'failed';
    const record = {
      timestamp: new Date(now + i * 1000).toISOString(),
      session_id: `seed-session-${i}`,
      plan_id: `seed-plan-${i}`,
      turn_id: `seed-turn-${i}`,
      mode: 'standard',
      final_status: isCompleted ? 'completed' : 'failed',
      reason,
      rounds: isCompleted ? 2 : 4,
      duration_ms: isCompleted ? 1200 + i : 2400 + i,
      token_used: isCompleted ? 280 + i : 360 + i,
      evidence_ids: [`seed-evidence-${i}`],
      progress_vector: {
        terminalRequiredTodos: isCompleted ? 3 : 2,
        acceptedCriteria: isCompleted ? 3 : 1,
        criticalPathResolved: isCompleted ? 1 : 0.6,
        unresolvedBlockers: isCompleted ? 0 : 1,
      },
      review_state: {
        accepted: isCompleted ? 3 : 1,
        total: 3,
      },
      blocker_state: {
        open: isCompleted ? 0 : 1,
        score: isCompleted ? 0 : 2.5,
      },
      budget_state: {
        elapsedMs: isCompleted ? 1200 + i : 2400 + i,
        tokenUsed: isCompleted ? 280 + i : 360 + i,
        errorRate: isCompleted ? 0 : 0.1,
      },
      required_total: 3,
      failed_required: isCompleted ? 0 : 1,
      running_or_pending_required: 0,
      shadow: {
        enabled: true,
        reason,
        consistent: true,
      },
    };
    lines.push(JSON.stringify(record));
  }

  fs.mkdirSync(metricsDir, { recursive: true });
  const payload = `${lines.join('\n')}\n`;
  if (append && fs.existsSync(outputPath)) {
    fs.appendFileSync(outputPath, payload, 'utf8');
  } else {
    fs.writeFileSync(outputPath, payload, 'utf8');
  }

  console.log('\n=== seed termination metrics ===');
  console.log(JSON.stringify({
    pass: true,
    outputPath,
    append,
    total,
    completedCount,
    failedCount,
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('seed termination metrics 失败:', error?.stack || error);
  process.exit(1);
}
