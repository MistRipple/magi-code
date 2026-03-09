#!/usr/bin/env node
/**
 * 门禁 fail-open 与收尾轮回归脚本
 *
 * 目标：
 * 1) 摘要劫持检测收紧，避免主模式单点误判
 * 2) 摘要劫持第 3 次及以上不再硬终止（fail-open）
 * 3) 工具轮命中 completed/failed 时，先进入无工具收尾轮再终止
 * 4) 无 Todo 工具循环与 Worker 空转可收敛，不再无限重复
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
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function main() {
  const llmTypes = read('src/llm/types.ts');
  const orchestratorAdapter = read('src/llm/adapters/orchestrator-adapter.ts');
  const workerAdapter = read('src/llm/adapters/worker-adapter.ts');

  assert(
    llmTypes.includes('const hasMainPattern = SUMMARY_HIJACK_MAIN_PATTERN.test(normalized);')
      && llmTypes.includes('const hasTagPair = hasAnalysisTag && hasSummaryTag;')
      && llmTypes.includes('if (hasMainPattern && (hasNoTools || hasTagPair)) {'),
    '摘要劫持检测未收紧，仍存在主模式单点命中风险'
  );

  assert(
    orchestratorAdapter.includes('已强制禁用工具并继续执行')
      && !orchestratorAdapter.includes('检测到重复摘要劫持输出，任务已安全终止。请重试当前任务。'),
    'Orchestrator 摘要劫持门禁仍存在硬终止路径'
  );
  assert(
    workerAdapter.includes('已强制禁用工具并继续执行')
      && !workerAdapter.includes('检测到重复摘要劫持输出，任务已安全终止。请重试当前任务。'),
    'Worker 摘要劫持门禁仍存在硬终止路径'
  );

  assert(
    orchestratorAdapter.includes('let pendingTerminalReason: Exclude<OrchestratorTerminationReason, \'unknown\'> | null = null;')
      && orchestratorAdapter.includes('this.shouldRequestTerminalSynthesisAfterToolRound(resolved.reason, toolCalls.length)')
      && orchestratorAdapter.includes('this.buildTerminalSynthesisPrompt(resolved.reason, progressState.snapshot)'),
    'Orchestrator 未接入工具轮终止前收尾轮逻辑'
  );

  assert(
    orchestratorAdapter.includes('检测到异常摘要模板输出，已自动忽略。请继续当前任务。')
      && orchestratorAdapter.includes('Orchestrator.检测到摘要劫持输出_已降级为不中断'),
    '非工具路径摘要劫持仍可能直接抛错中断'
  );

  assert(
    orchestratorAdapter.includes('let noTodoToolRoundStreak = 0;')
      && orchestratorAdapter.includes('this.buildNoTodoToolLoopPrompt(noTodoToolRoundStreak, repeatedNoTodoToolSignatureStreak)')
      && orchestratorAdapter.includes('noTodoToolRoundStreak >= 4 || repeatedNoTodoToolSignatureStreak >= 2'),
    'Orchestrator 无 Todo 工具循环缺少收敛门禁'
  );

  assert(
    workerAdapter.includes('forceNoToolsNextRound = true;')
      && workerAdapter.includes('下一轮已禁用工具，请直接输出最终结论或明确修改计划')
      && workerAdapter.includes('工具执行已完成，但模型未输出文本结论。请查看上方工具结果。'),
    'Worker 空转收敛策略未生效（仍可能无限循环或空文本终止）'
  );

  console.log('\n=== gate fail-open regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'summary-hijack-detection-hardened',
      'summary-hijack-fail-open',
      'tool-round-terminal-handoff',
      'non-tool-hijack-degrade',
      'no-todo-tool-loop-convergence',
      'worker-stall-convergence',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('gate fail-open 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}
