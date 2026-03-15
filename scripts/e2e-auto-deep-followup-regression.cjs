#!/usr/bin/env node
/**
 * Auto + Deep 自动续跑回归脚本
 *
 * 目标：
 * 1) 当 runtime 提供结构化 nextSteps 时，auto+deep 能继续自动续跑
 * 2) 当存在未完成 required todos 时，即便无 nextSteps 也会自动续跑
 * 3) 续跑会在进度收敛后自然停止（无死循环）
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const originalModuleLoad = Module._load;
Module._load = function patchedModuleLoad(request, parent, isMain) {
  if (request === 'vscode') {
    return {
      workspace: {
        getConfiguration() {
          return {
            get(_key, fallback) { return fallback; },
            update() { return Promise.resolve(); },
          };
        },
      },
      ConfigurationTarget: { Global: 1 },
      Uri: {
        file(filePath) { return { fsPath: filePath, path: filePath, toString() { return filePath; } }; },
        joinPath(base, ...parts) {
          const basePath = base && typeof base.path === 'string' ? base.path : '';
          const resolved = path.join(basePath, ...parts);
          return { fsPath: resolved, path: resolved, toString() { return resolved; } };
        },
      },
      window: {},
      commands: { executeCommand() { return Promise.resolve(); } },
    };
  }
  return originalModuleLoad.call(this, request, parent, isMain);
};

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function verifySourceGuardrails() {
  const source = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'),
    'utf8',
  );
  const orchestratorAdapterSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapters', 'orchestrator-adapter.ts'),
    'utf8',
  );

  assert(
    source.includes('didFollowUpRoundProduceExecutionActivity'),
    '缺少自动续跑实际执行活动检测',
  );
  assert(
    source.includes('这是执行轮，不是规划轮。'),
    '自动续跑 prompt 未收紧为执行轮约束',
  );
  assert(
    source.includes('禁止只输出“现在启动”“准备派发”“已确认结构”“派发修复：”这类口头承诺'),
    '自动续跑 prompt 未拦截口头派发空转',
  );
  assert(
    orchestratorAdapterSource.includes('若当前 mission 仍有明确后续阶段'),
    '收尾总结轮未要求保留后续阶段的结构化 next_steps',
  );
  assert(
    orchestratorAdapterSource.includes('收尾轮缺少结构化结论，触发补跑'),
    '收尾总结轮缺少 outcome block 时未触发补跑',
  );
  const planLedgerTypes = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'plan-ledger', 'types.ts'),
    'utf8',
  );
  assert(
    planLedgerTypes.includes('export interface PlanRuntimePhaseState'),
    'PlanLedger 缺少显式阶段运行态定义',
  );
  assert(
    source.includes('resolvePersistedPhaseFollowUpSteps'),
    '自动续跑未接入持久化 phase 续跑恢复',
  );
  assert(
    source.includes('markPhaseRuntimeRunning'),
    '自动续跑未推进显式阶段运行态',
  );
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function buildSnapshot(requiredTotal, terminalRequired, runningOrPendingRequired) {
  return {
    requiredTotal,
    runningOrPendingRequired,
    progressVector: {
      terminalRequiredTodos: terminalRequired,
    },
  };
}

class StubAdapterFactory {
  constructor(responses) {
    this.responses = responses;
    this.attempts = 0;
    this.toolManager = {
      setPermissions() {},
      setSnapshotContext() {},
      clearSnapshotContext() {},
      clearDispatchFileWriteTracker() {},
      buildToolsSummary() { return ''; },
      refreshToolSchemas() {},
      getOrchestrationExecutor() {
        return {
          setAvailableWorkers() {},
          setCategoryWorkerMap() {},
          setHandlers() {},
        };
      },
    };
  }

  isDeepTask() {
    return true;
  }

  getToolManager() {
    return this.toolManager;
  }

  getUserRulesPrompt() {
    return '';
  }

  getAdapterHistoryInfo() {
    return undefined;
  }

  interrupt() {
    return Promise.resolve();
  }

  async sendMessage() {
    const response = this.responses[Math.min(this.attempts, this.responses.length - 1)];
    this.attempts += 1;
    return {
      content: response.content,
      tokenUsage: { inputTokens: 0, outputTokens: 0 },
      orchestratorRuntime: {
        reason: 'completed',
        rounds: 1,
        snapshot: response.snapshot,
        nextSteps: Array.isArray(response.nextSteps) ? response.nextSteps : [],
      },
    };
  }
}

async function runScenario(label, responses, expectations) {
  const { MissionDrivenEngine } = loadCompiledModule(path.join('orchestrator', 'core', 'mission-driven-engine.js'));
  const { UnifiedSessionManager } = loadCompiledModule(path.join('session', 'unified-session-manager.js'));
  const { SnapshotManager } = loadCompiledModule(path.join('snapshot-manager.js'));

  const adapterFactory = new StubAdapterFactory(responses);
  const sessionManager = new UnifiedSessionManager(ROOT);
  const snapshotManager = new SnapshotManager(sessionManager, ROOT);
  const engine = new MissionDrivenEngine(
    adapterFactory,
    {
      timeout: 30000,
      maxRetries: 1,
      permissions: { allowEdit: false, allowBash: false, allowWeb: false },
      strategy: { enableVerification: false, enableRecovery: true, autoRollbackOnFailure: false },
    },
    ROOT,
    snapshotManager,
    sessionManager,
  );

  engine.evaluatePlanGovernance = async () => ({
    riskScore: 0,
    confidence: 1,
    affectedFiles: 0,
    crossModules: 0,
    writeToolRatio: 0,
    historicalFailureRate: 0,
    sourceCoverage: 0,
    signalAgreement: 1,
    historicalCalibration: 1,
    decision: 'auto',
    reasons: [],
  });

  await engine.initialize();
  try {
    const session = sessionManager.createSession(`auto-deep-followup-${Date.now()}`);
    const result = await engine.execute('自动续跑回归验证', '', session.id);

    expectations(adapterFactory, result);
    console.log(`\n=== auto deep follow-up regression (${label}) ===`);
    console.log(JSON.stringify({
      pass: true,
      attempts: adapterFactory.attempts,
      preview: String(result || '').replace(/\s+/g, ' ').slice(0, 180),
    }, null, 2));
  } finally {
    engine.dispose();
  }
}

async function main() {
  verifySourceGuardrails();
  await runScenario('next-steps', [
    {
      content: [
        '已完成初步检查。',
        '下一步建议：',
        '- 继续执行补充修复任务',
        '- 重新运行验收',
      ].join('\n'),
      nextSteps: ['继续执行补充修复任务', '重新运行验收'],
      snapshot: buildSnapshot(2, 0, 2),
    },
    {
      content: '已完成补充修复并通过验收。',
      snapshot: buildSnapshot(2, 2, 0),
    },
  ], (adapterFactory, result) => {
    assert(adapterFactory.attempts === 2, `next-steps 续跑应触发 2 轮，实际: ${adapterFactory.attempts}`);
    assert(String(result).length > 0, 'next-steps 结果不应为空');
  });

  await runScenario('pending-required', [
    {
      content: '已完成首轮执行。',
      snapshot: buildSnapshot(3, 1, 2),
    },
    {
      content: '必需 Todo 已清空。',
      snapshot: buildSnapshot(3, 3, 0),
    },
  ], (adapterFactory, result) => {
    assert(adapterFactory.attempts === 2, `pending-required 续跑应触发 2 轮，实际: ${adapterFactory.attempts}`);
    assert(String(result).length > 0, 'pending-required 结果不应为空');
  });

  await runScenario('phase-completed-next-phase-pending', [
    {
      content: [
        '已完成 Phase 1 并行审查。',
        '下一步建议：',
        '- Phase 2：派发修复实施',
        '- Phase 3：修复完成后执行复审验证',
      ].join('\n'),
      nextSteps: ['Phase 2：派发修复实施', 'Phase 3：修复完成后执行复审验证'],
      snapshot: buildSnapshot(3, 3, 0),
    },
    {
      content: '已继续推进下一阶段并完成本轮实施。',
      snapshot: buildSnapshot(3, 3, 0),
    },
  ], (adapterFactory, result) => {
    assert(adapterFactory.attempts === 2, `phase-completed-next-phase-pending 应触发 2 轮，实际: ${adapterFactory.attempts}`);
    assert(String(result).length > 0, 'phase-completed-next-phase-pending 结果不应为空');
  });

  await runScenario('non-task-chat-no-followup', [
    {
      content: [
        '上轮已完成回答。',
        '下一步建议：',
        '- 如有新的开发需求，请随时提出。',
        '- 我可以帮你做功能开发、Bug 修复、架构设计、代码审查。',
      ].join('\n'),
      snapshot: buildSnapshot(0, 0, 0),
    },
  ], (adapterFactory, result) => {
    assert(adapterFactory.attempts === 1, `非任务对话不应自动续跑，实际轮次: ${adapterFactory.attempts}`);
    assert(String(result).length > 0, 'non-task-chat 结果不应为空');
  });

  Module._load = originalModuleLoad;
  // 显式退出，避免全局单例（日志/事件总线）残留句柄导致回归脚本悬挂。
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('auto deep follow-up 回归失败:', error?.stack || error);
  process.exit(1);
});
