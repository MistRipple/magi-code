#!/usr/bin/env node
/**
 * Ask + Deep Replan Gate 回归
 *
 * 覆盖目标：
 * 1) ask + deep + follow-up pending 必须进入确认阻塞，并在确认后继续执行。
 * 2) ask + deep + 预算压力信号命中时必须进入确认阻塞，并在拒绝后停止续跑。
 * 3) 确认请求必须带 requestType=replan_followup，保证前端解释链路可区分。
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
    budgetState: {
      elapsedMs: 120_000,
      tokenUsed: 8_000,
      errorRate: 0.1,
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
        reason: response.reason || 'completed',
        rounds: 1,
        snapshot: response.snapshot,
      },
    };
  }
}

async function runScenario(label, options) {
  const { MissionDrivenEngine } = loadCompiledModule(path.join('orchestrator', 'core', 'mission-driven-engine.js'));
  const { UnifiedSessionManager } = loadCompiledModule(path.join('session', 'unified-session-manager.js'));
  const { SnapshotManager } = loadCompiledModule(path.join('snapshot-manager.js'));

  const adapterFactory = new StubAdapterFactory(options.responses);
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

  const confirmations = [];
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
  engine.awaitPlanConfirmation = async () => true;
  engine.awaitDeliveryRepairConfirmation = async (input) => {
    confirmations.push(input);
    return options.decision;
  };

  engine.setInteractionMode('ask');
  await engine.initialize();
  try {
    const session = sessionManager.createSession(`replan-gate-ask-${Date.now()}`);
    const result = await engine.execute('replan gate ask 回归验证', '', session.id);
    options.expectations({ adapterFactory, confirmations, result });
    console.log(`\n=== replan gate ask regression (${label}) ===`);
    console.log(JSON.stringify({
      pass: true,
      attempts: adapterFactory.attempts,
      confirmationCount: confirmations.length,
      preview: String(result || '').replace(/\s+/g, ' ').slice(0, 180),
    }, null, 2));
  } finally {
    engine.dispose();
  }
}

async function main() {
  await runScenario('followup-confirm-repair', {
    responses: [
      {
        content: [
          '已完成第一轮执行。',
          '下一步建议：',
          '- 继续补充修复',
          '- 再次执行验收',
        ].join('\n'),
        snapshot: buildSnapshot(3, 1, 2),
      },
      {
        content: '补充修复已完成，验收通过。',
        snapshot: buildSnapshot(3, 3, 0),
      },
    ],
    decision: 'repair',
    expectations({ adapterFactory, confirmations, result }) {
      assert(adapterFactory.attempts === 2, `confirm-repair 应继续执行到第 2 轮，实际: ${adapterFactory.attempts}`);
      assert(confirmations.length === 1, `confirm-repair 应触发 1 次确认，实际: ${confirmations.length}`);
      assert(confirmations[0].requestType === 'replan_followup', 'confirm-repair 缺少 replan_followup 请求类型');
      assert(String(result).length > 0, 'confirm-repair 结果不应为空');
    },
  });

  await runScenario('budget-reject-stop', {
    responses: [
      {
        content: '当前轮次未输出后续步骤。',
        reason: 'budget_exceeded',
        snapshot: buildSnapshot(4, 1, 3),
      },
    ],
    decision: 'stop',
    expectations({ adapterFactory, confirmations, result }) {
      assert(adapterFactory.attempts === 1, `budget-reject 应在首轮停止，实际: ${adapterFactory.attempts}`);
      assert(confirmations.length === 1, `budget-reject 应触发 1 次确认，实际: ${confirmations.length}`);
      assert(confirmations[0].requestType === 'replan_followup', 'budget-reject 缺少 replan_followup 请求类型');
      assert(String(confirmations[0].summary).includes('预算'), 'budget-reject 未携带预算压力解释');
      assert(String(result).includes('暂停继续执行'), 'budget-reject 结果缺少暂停提示');
    },
  });

  const missionSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'mission-driven-engine.ts'),
    'utf8',
  );
  const kernelSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'core', 'recovery-decision-kernel.ts'),
    'utf8',
  );
  assert(kernelSource.includes("'scope_expansion'"), '缺少 scope_expansion 触发源');
  assert(kernelSource.includes("'acceptance_failure'"), '缺少 acceptance_failure 触发源');
  assert(missionSource.includes('scope_issues='), '缺少 scope issue 结构化原因');

  Module._load = originalModuleLoad;
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('replan gate ask 回归失败:', error?.stack || error);
  process.exit(1);
});
