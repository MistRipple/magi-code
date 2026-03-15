#!/usr/bin/env node

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
  if (!condition) throw new Error(message);
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function testSourceGuardrail() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'ui', 'webview-provider.ts'), 'utf8');
  assert(!source.includes("return { ...task, status: 'cancelled' as const };"), 'buildUIState 仍在将 running 任务伪造成 cancelled');
}

function createProviderHarness(WebviewProvider) {
  const provider = Object.create(WebviewProvider.prototype);
  provider.activeSessionId = 'session-1';
  provider.locale = 'zh-CN';
  provider.logs = [];
  provider.interactionModeUpdatedAt = 0;
  provider.assertValidArray = () => {};
  provider.getTaskViews = async () => [{
    id: 'mission-1',
    goal: '修复 deep 续航显示错位',
    prompt: '修复 deep 续航显示错位',
    status: 'running',
    priority: 1,
    subTasks: [],
    createdAt: 10,
    startedAt: 20,
    completedAt: undefined,
    progress: 50,
    missionId: 'mission-1',
    failureReason: undefined,
  }];
  provider.orchestratorEngine = {
    running: false,
    getInteractionMode() { return 'ask'; },
    phase: 'idle',
    getActivePlanState() { return undefined; },
    getPlanLedgerSnapshot() { return { plans: [] }; },
  };
  provider.sessionManager = {
    getCurrentSession() { return { id: 'session-1' }; },
    getSessionMetas() { return [{ id: 'session-1', name: '会话 1', updatedAt: Date.now(), messageCount: 0 }]; },
  };
  provider.adapterFactory = {
    isConnected() { return true; },
  };
  provider.snapshotManager = {
    getPendingChanges() { return []; },
  };
  return provider;
}

async function main() {
  testSourceGuardrail();
  const { WebviewProvider } = loadCompiledModule(path.join('ui', 'webview-provider.js'));
  const provider = createProviderHarness(WebviewProvider);
  const state = await provider.buildUIState();
  assert(Array.isArray(state.tasks) && state.tasks.length === 1, `tasks 数量异常: ${JSON.stringify(state.tasks)}`);
  assert(state.tasks[0].status === 'running', `engine 未运行时不应改写 TaskView.status，实际: ${state.tasks[0].status}`);
  assert(state.currentTask?.status === 'running', `currentTask 应保持权威 running 状态，实际: ${JSON.stringify(state.currentTask)}`);
  console.log('\n=== provider ui state task status regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'build_ui_state_keeps_taskview_running_status',
      'current_task_uses_authoritative_status',
    ],
  }, null, 2));
  Module._load = originalModuleLoad;
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('provider ui state task status 回归失败:', error?.stack || error);
  process.exit(1);
});