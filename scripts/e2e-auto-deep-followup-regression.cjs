#!/usr/bin/env node
/**
 * Auto + Deep 自动续跑回归脚本
 *
 * 目标：
 * 1) 当编排输出包含“下一步建议”时，auto+deep 能继续自动续跑
 * 2) 当存在未完成 required todos 时，即便无“下一步建议”也会自动续跑
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
  const session = sessionManager.createSession(`auto-deep-followup-${Date.now()}`);
  const result = await engine.execute('自动续跑回归验证', '', session.id);

  expectations(adapterFactory, result);
  console.log(`\n=== auto deep follow-up regression (${label}) ===`);
  console.log(JSON.stringify({
    pass: true,
    attempts: adapterFactory.attempts,
    preview: String(result || '').replace(/\s+/g, ' ').slice(0, 180),
  }, null, 2));
}

async function main() {
  await runScenario('next-steps', [
    {
      content: [
        '已完成初步检查。',
        '下一步建议：',
        '- 继续执行补充修复任务',
        '- 重新运行验收',
      ].join('\n'),
      snapshot: buildSnapshot(2, 0, 2),
    },
    {
      content: '已完成补充修复并通过验收。',
      snapshot: buildSnapshot(2, 2, 0),
    },
  ], (adapterFactory, result) => {
    assert(adapterFactory.attempts === 2, `next-steps 续跑应触发 2 轮，实际: ${adapterFactory.attempts}`);
    assert(String(result).includes('自动续跑轮次记录'), '结果应包含自动续跑轮次记录');
    assert(String(result).includes('继续执行补充修复任务'), '结果应包含续跑步骤记录');
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
    assert(String(result).includes('自动续跑轮次记录'), '结果应包含自动续跑轮次记录');
  });

  Module._load = originalModuleLoad;
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('auto deep follow-up 回归失败:', error?.stack || error);
  process.exit(1);
});
