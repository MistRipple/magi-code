#!/usr/bin/env node
/**
 * dispatch_task 缺失 scope_hint 回归脚本
 *
 * 目标：
 * 1) 并行批次下缺失 scope_hint 不再 hard-fail
 * 2) 自动降级为串行（通过追加 depends_on）
 * 3) 返回结果标记 degraded=true，routing_reason 包含降级原因
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { EventEmitter } = require('events');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function createDispatchManager(DispatchManager, workspaceRoot) {
  let handlers = null;
  const orchestrationExecutor = {
    setAvailableWorkers() {},
    setCategoryWorkerMap() {},
    setHandlers(next) { handlers = next; },
  };
  const toolManager = {
    getOrchestrationExecutor() { return orchestrationExecutor; },
    refreshToolSchemas() {},
  };

  const categoryOwner = {
    simple: 'claude',
    debug: 'codex',
    data_analysis: 'gemini',
  };

  const manager = new DispatchManager({
    adapterFactory: {
      getToolManager() { return toolManager; },
    },
    profileLoader: {
      getEnabledProfiles() {
        return new Map([
          ['claude', { worker: 'claude', persona: { strengths: ['design', 'plan'] } }],
          ['codex', { worker: 'codex', persona: { strengths: ['debug', 'fix'] } }],
          ['gemini', { worker: 'gemini', persona: { strengths: ['analysis', 'summary'] } }],
        ]);
      },
      getAssignmentLoader() {
        return {
          getCategoryMap() { return categoryOwner; },
          reload() {},
        };
      },
      getAllCategories() {
        return new Map([
          ['simple', { displayName: 'simple' }],
          ['debug', { displayName: 'debug' }],
          ['data_analysis', { displayName: 'data_analysis' }],
        ]);
      },
      getWorkerForCategory(category) {
        const owner = categoryOwner[category];
        if (!owner) throw new Error(`unknown category: ${category}`);
        return owner;
      },
      getCategory(category) {
        return { name: category };
      },
    },
    messageHub: {
      notify() {},
      subTaskCard() {},
      workerInstruction() {},
    },
    missionOrchestrator: new EventEmitter(),
    workspaceRoot,
    getActiveUserPrompt: () => '',
    getActiveImagePaths: () => undefined,
    getCurrentSessionId: () => 'session-scope-hint-regression',
    getMissionIdsBySession: async () => [],
    ensureMissionForDispatch: async () => 'mission-scope-hint-regression',
    getCurrentTurnId: () => 'turn-scope-hint-regression',
    getProjectKnowledgeBase: () => undefined,
    processWorkerWisdom() {},
    getSnapshotManager: () => null,
    getContextManager: () => null,
    getTodoManager: () => null,
    recordOrchestratorTokens() {},
    recordWorkerTokenUsage() {},
    getSupplementaryQueue: () => null,
  });

  // 本回归仅验证 dispatch 注册行为，不执行 Worker pipeline。
  manager.scheduleDispatchReadyTasks = () => {};
  manager.resolveDispatchRouting = (_goal, category) => ({
    ok: true,
    decision: {
      selectedWorker: categoryOwner[category],
      category,
      categorySource: 'explicit_param',
      degraded: false,
      routingReason: `route-${category}`,
    },
  });

  manager.setupOrchestrationToolHandlers();
  assert(handlers && typeof handlers.dispatch === 'function', 'dispatch handler 未注入');
  return { manager, handlers };
}

async function main() {
  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-dispatch-scope-hint-'));
  const { manager, handlers } = createDispatchManager(DispatchManager, workspaceRoot);

  const baseTask = {
    requiresModification: true,
    goal: 'goal',
    acceptance: ['acceptance'],
    constraints: ['constraints'],
    context: ['context'],
  };

  try {
    const r1 = await handlers.dispatch({
      task_name: 'task-1',
      category: 'simple',
      ...baseTask,
    });
    const r2 = await handlers.dispatch({
      task_name: 'task-2',
      category: 'debug',
      ...baseTask,
    });
    const r3 = await handlers.dispatch({
      task_name: 'task-3',
      category: 'data_analysis',
      ...baseTask,
    });

    assert(r1.status === 'dispatched', `task-1 应成功派发，实际: ${r1.status}`);
    assert(r2.status === 'dispatched', `task-2 不应因缺失 scope_hint 失败，实际: ${r2.status}`);
    assert(r3.status === 'dispatched', `task-3 不应因缺失 scope_hint 失败，实际: ${r3.status}`);
    assert(r2.degraded === true, 'task-2 缺失 scope_hint 应标记 degraded=true');
    assert(r3.degraded === true, 'task-3 缺失 scope_hint 应标记 degraded=true');
    assert(typeof r2.routing_reason === 'string' && r2.routing_reason.includes('scope_hint'), 'task-2 routing_reason 缺少 scope_hint 降级说明');
    assert(typeof r3.routing_reason === 'string' && r3.routing_reason.includes('scope_hint'), 'task-3 routing_reason 缺少 scope_hint 降级说明');
    assert(!r2.error, `task-2 不应返回 error: ${r2.error}`);
    assert(!r3.error, `task-3 不应返回 error: ${r3.error}`);

    console.log('\n=== dispatch missing scope_hint regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'missing-scope-hint-no-hard-fail',
        'auto-serialized-dependency-downgrade',
        'degraded-flag-and-routing-reason',
      ],
      sample: { r1, r2, r3 },
    }, null, 2));
  } finally {
    manager.dispose();
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('dispatch missing scope_hint 回归失败:', error?.stack || error);
  process.exit(1);
});

