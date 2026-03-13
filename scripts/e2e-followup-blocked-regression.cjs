#!/usr/bin/env node
/**
 * Auto + Deep 阻断续跑回归脚本
 *
 * 目标：
 * 1) 当 nextSteps 仅包含阻断类步骤时，不触发自动续跑
 * 2) 输出阻断提示，避免无限循环
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
        nextSteps: response.nextSteps,
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
  const session = sessionManager.createSession(`followup-blocked-${Date.now()}`);
  const result = await engine.execute('阻断续跑回归验证', '', session.id);

  expectations(adapterFactory, result);
  console.log(`\n=== follow-up blocked regression (${label}) ===`);
  console.log(JSON.stringify({
    pass: true,
    attempts: adapterFactory.attempts,
    preview: String(result || '').replace(/\s+/g, ' ').slice(0, 200),
  }, null, 2));
}

async function main() {
  await runScenario('blocked-only', [
    {
      content: '已完成本轮执行。',
      nextSteps: ['请用户确认后再继续执行'],
      snapshot: undefined,
    },
  ], (adapterFactory, result) => {
    const output = String(result || '');
    assert(adapterFactory.attempts === 1, `阻断续跑应仅触发 1 轮，实际: ${adapterFactory.attempts}`);
    assert(output.includes('已停止自动续跑'), '结果应包含阻断停止提示');
    assert(!output.includes('自动续跑轮次记录'), '阻断续跑不应产生自动续跑记录');
  });

  Module._load = originalModuleLoad;
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('follow-up blocked 回归失败:', error?.stack || error);
  process.exit(1);
});
