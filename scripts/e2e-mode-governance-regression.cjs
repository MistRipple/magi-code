#!/usr/bin/env node
/**
 * 两模式治理回归脚本（常规=功能级，深度=项目级）
 *
 * 目标：
 * 1. 验证 Worker 验收复审策略按模式分化（常规 2 轮，深度 8 轮）
 * 2. 验证深度模式下编排者写工具被硬约束（必须委派 Worker）
 * 3. 验证常规模式下编排者仍保留小规模直改能力（最多 3 文件）
 * 4. 验证关键治理阈值已从“无限”语义收敛为“高预算 + 硬上限”
 */

const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function createOrchestratorAdapter(OrchestratorLLMAdapter, deepTask) {
  const normalizer = new EventEmitter();
  const toolManager = {};
  const messageHub = {
    sendMessage() {},
    sendUpdate() {},
    getTraceId() { return 'trace-e2e'; },
  };
  const client = {};
  const config = {};
  return new OrchestratorLLMAdapter({
    client,
    normalizer,
    toolManager,
    config,
    messageHub,
    deepTask,
  });
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'orchestrator', 'worker', 'autonomous-worker.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { AutonomousWorker } = require(path.join(OUT, 'orchestrator', 'worker', 'autonomous-worker.js'));
  const { OrchestratorLLMAdapter } = require(path.join(OUT, 'llm', 'adapters', 'orchestrator-adapter.js'));

  const worker = new AutonomousWorker(
    'claude',
    {},
    {},
    {},
    {
      contextAssembler: {},
      fileSummaryCache: {},
      sharedContextPool: {},
    }
  );

  const resolveReviewPolicy = worker.resolveReviewPolicy || worker['resolveReviewPolicy'];
  assert(typeof resolveReviewPolicy === 'function', '无法访问 Worker 复审策略解析函数');

  const featurePolicy = resolveReviewPolicy.call(worker, {
    adapterFactory: { isDeepTask: () => false },
  });
  const projectPolicy = resolveReviewPolicy.call(worker, {
    adapterFactory: { isDeepTask: () => true },
  });

  assert(featurePolicy.mode === 'feature', `常规模式策略类型异常: ${featurePolicy.mode}`);
  assert(featurePolicy.maxReviewRounds === 2, `常规模式复审轮次异常: ${featurePolicy.maxReviewRounds}`);
  assert(projectPolicy.mode === 'project', `深度模式策略类型异常: ${projectPolicy.mode}`);
  assert(projectPolicy.maxReviewRounds === 8, `深度模式复审轮次异常: ${projectPolicy.maxReviewRounds}`);

  const regularAdapter = createOrchestratorAdapter(OrchestratorLLMAdapter, false);
  const deepAdapter = createOrchestratorAdapter(OrchestratorLLMAdapter, true);
  const checkRestriction = regularAdapter.checkOrchestratorToolRestriction || regularAdapter['checkOrchestratorToolRestriction'];
  const checkRestrictionDeep = deepAdapter.checkOrchestratorToolRestriction || deepAdapter['checkOrchestratorToolRestriction'];
  assert(typeof checkRestriction === 'function', '无法访问编排者工具约束检查函数');
  assert(typeof checkRestrictionDeep === 'function', '无法访问深度模式编排者工具约束检查函数');

  const regularResults = [];
  for (let i = 1; i <= 4; i++) {
    regularResults.push(checkRestriction.call(regularAdapter, {
      id: `regular-edit-${i}`,
      name: 'file_edit',
      arguments: { path: `src/demo-${i}.ts` },
    }));
  }
  assert(regularResults[0] === null && regularResults[1] === null && regularResults[2] === null,
    `常规模式前 3 次 file_edit 不应被拦截: ${JSON.stringify(regularResults.slice(0, 3))}`);
  assert(typeof regularResults[3] === 'string' && regularResults[3].includes('超过 3 个'),
    `常规模式第 4 次 file_edit 应触发规模约束: ${regularResults[3]}`);

  const deepEditRestriction = checkRestrictionDeep.call(deepAdapter, {
    id: 'deep-edit-1',
    name: 'file_edit',
    arguments: { path: 'src/deep.ts' },
  });
  assert(typeof deepEditRestriction === 'string' && deepEditRestriction.includes('深度模式下编排者不可直接执行'),
    `深度模式 file_edit 应被强约束: ${deepEditRestriction}`);

  const deepDispatchRestriction = checkRestrictionDeep.call(deepAdapter, {
    id: 'deep-dispatch',
    name: 'worker_dispatch',
    arguments: { category: 'general' },
  });
  assert(deepDispatchRestriction === null, `深度模式 worker_dispatch 不应被拦截: ${deepDispatchRestriction}`);

  const deepTerminalTools = [
    'shell',
  ];
  const deepTerminalChecks = deepTerminalTools.map((toolName) => ({
    tool: toolName,
    restriction: checkRestrictionDeep.call(deepAdapter, {
      id: `deep-terminal-${toolName}`,
      name: toolName,
      arguments: {},
    }),
  }));
  assert(
    deepTerminalChecks.every((item) => item.restriction === null),
    `深度模式终端工具不应被拦截: ${JSON.stringify(deepTerminalChecks)}`
  );

  const orchestratorAdapterSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'orchestrator-adapter.ts'),
    'utf8'
  );
  const decisionEngineSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'orchestrator-decision-engine.ts'),
    'utf8'
  );
  const hasLegacyRoundLimit = /MAX_ORCHESTRATOR_ROUNDS|failure_limit|round_limit/.test(orchestratorAdapterSource);
  assert(!hasLegacyRoundLimit, '编排者仍残留旧轮次/失败次数终止口径');

  const hasBudgetBudgetConstants = orchestratorAdapterSource.includes('private static readonly STANDARD_BUDGET')
    && orchestratorAdapterSource.includes('private static readonly DEEP_BUDGET');
  const hasLegacyBudgetCollector = orchestratorAdapterSource.includes('collectBudgetCandidates(')
    && /createTerminationCandidate\(\s*'budget_exceeded'/.test(orchestratorAdapterSource);
  const hasDecisionEngineBudgetCollector = orchestratorAdapterSource.includes('OrchestratorDecisionEngine')
    && orchestratorAdapterSource.includes('decisionEngine.collectBudgetCandidates(')
    && decisionEngineSource.includes("createCandidate('budget_exceeded'")
    && decisionEngineSource.includes("createCandidate('external_wait_timeout'");
  const hasBudgetGovernance = hasBudgetBudgetConstants
    && (hasLegacyBudgetCollector || hasDecisionEngineBudgetCollector);
  assert(hasBudgetGovernance, '编排者预算治理口径缺失（STANDARD/DEEP_BUDGET + budget_exceeded）');

  const workerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'worker', 'autonomous-worker.ts'),
    'utf8'
  );
  assert(workerSource.includes('maxReviewRounds: 8'), '深度模式复审轮次未命中预期（应为 8）');
  assert(workerSource.includes('maxReviewRounds: 2'), '常规模式复审轮次未命中预期（应为 2）');

  console.log('\n=== 模式治理回归结果 ===');
  console.log(JSON.stringify({
    featurePolicy,
    projectPolicy,
    regularFileEditChecks: regularResults,
    deepEditRestriction,
    deepDispatchRestriction,
    deepTerminalChecks,
    pass: true,
  }, null, 2));

  process.exit(0);
}

main().catch((error) => {
  console.error('模式治理回归失败:', error?.stack || error);
  process.exit(1);
});
