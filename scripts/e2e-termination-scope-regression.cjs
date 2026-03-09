#!/usr/bin/env node
/**
 * Termination 作用域一致性回归脚本
 *
 * 覆盖目标：
 * 1) SnapshotContext 在 mission 创建后必须绑定真实 mission.id（不能继续使用 turnId）
 * 2) WorkerPipeline 执行上下文 missionId 必须与 Assignment/Todo 一致
 * 3) stalled 判定仅在 required todos 已建立后生效，避免未入轨误终止
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function read(relPath) {
  const abs = path.join(ROOT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`文件不存在: ${abs}`);
  }
  return fs.readFileSync(abs, 'utf8');
}

function main() {
  const missionDrivenEngine = read(path.join('src', 'orchestrator', 'core', 'mission-driven-engine.ts'));
  const dispatchManager = read(path.join('src', 'orchestrator', 'core', 'dispatch-manager.ts'));
  const orchestratorAdapter = read(path.join('src', 'llm', 'adapters', 'orchestrator-adapter.ts'));
  const decisionEngine = read(path.join('src', 'llm', 'adapters', 'orchestrator-decision-engine.ts'));

  assert(
    missionDrivenEngine.includes('missionId: mission.id,'),
    'Mission 创建后 SnapshotContext 仍未绑定真实 mission.id'
  );
  assert(
    !missionDrivenEngine.includes('missionId: this.currentTurnId || mission.id'),
    '检测到旧逻辑：SnapshotContext 仍使用 turnId 覆盖 mission.id'
  );

  assert(
    !dispatchManager.includes('missionId: this.deps.getCurrentTurnId() || missionId'),
    '检测到旧逻辑：WorkerPipeline missionId 仍使用 turnId 覆盖真实 missionId'
  );
  assert(
    dispatchManager.includes('missionId,'),
    'WorkerPipeline 未显式传递真实 missionId'
  );

  const stalledGuardPatternInAdapter = /snapshot\.requiredTotal > 0[\s\S]{0,180}noProgressStreak >= OrchestratorLLMAdapter\.STALLED_WINDOW_SIZE[\s\S]{0,180}snapshot\.blockerState\.externalWaitOpen === 0/;
  const stalledGuardPatternInDecisionEngine = /snapshot\.requiredTotal > 0[\s\S]{0,180}noProgressStreak >= this\.policy\.stalledWindowSize[\s\S]{0,180}snapshot\.blockerState\.externalWaitOpen === 0/;
  assert(
    stalledGuardPatternInAdapter.test(orchestratorAdapter)
      || stalledGuardPatternInDecisionEngine.test(decisionEngine),
    'stalled 判定缺少 requiredTotal > 0 守卫，存在未入轨误终止风险'
  );

  console.log('\n=== termination scope regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'snapshot-context-mission-id-consistency',
      'worker-pipeline-mission-id-consistency',
      'stalled-guard-required-total',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('termination scope 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}
