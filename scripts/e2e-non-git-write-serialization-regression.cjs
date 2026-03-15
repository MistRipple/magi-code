#!/usr/bin/env node
/**
 * 非 Git 工作区写任务串行降级回归脚本
 *
 * 覆盖目标：
 * 1) 非 Git 工作区下的写任务不再 hard-fail
 * 2) 写任务自动降级为单写串行模式（通过追加 depends_on）
 * 3) 只读任务不受该降级影响
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
  const notifications = [];
  let missionSeq = 0;

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
    feature: 'claude',
    review: 'codex',
    analysis: 'gemini',
  };

  const manager = new DispatchManager({
    adapterFactory: {
      getToolManager() { return toolManager; },
    },
    profileLoader: {
      getEnabledProfiles() {
        return new Map([
          ['claude', { worker: 'claude', persona: { strengths: ['feature'] } }],
          ['codex', { worker: 'codex', persona: { strengths: ['review'] } }],
          ['gemini', { worker: 'gemini', persona: { strengths: ['analysis'] } }],
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
          ['feature', { displayName: 'feature' }],
          ['review', { displayName: 'review' }],
          ['analysis', { displayName: 'analysis' }],
        ]);
      },
      getWorkerForCategory(category) {
        return categoryOwner[category] || 'claude';
      },
      getCategory(category) {
        return { name: category };
      },
    },
    messageHub: {
      notify(message, level) {
        notifications.push({ message, level });
      },
      subTaskCard() {},
      workerInstruction() {},
    },
    missionOrchestrator: new EventEmitter(),
    workspaceRoot,
    getActiveUserPrompt: () => '',
    getActiveImagePaths: () => undefined,
    getCurrentSessionId: () => 'session-non-git-write-serialization',
    getMissionIdsBySession: async () => [],
    ensureMissionForDispatch: async () => `mission-non-git-write-serialization-${++missionSeq}`,
    getCurrentTurnId: () => 'turn-non-git-write-serialization',
    getProjectKnowledgeBase: () => undefined,
    processWorkerWisdom() {},
    getSnapshotManager: () => null,
    getContextManager: () => null,
    getTodoManager: () => null,
    recordOrchestratorTokens() {},
    recordWorkerTokenUsage() {},
    getSupplementaryQueue: () => null,
  });

  // 本回归只验证 dispatch 注册与自动串行降级，不触发 Worker 执行。
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
  return { manager, handlers, notifications };
}

async function main() {
  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-non-git-write-serialization-'));
  const { manager, handlers, notifications } = createDispatchManager(DispatchManager, workspaceRoot);

  const writeTaskBase = {
    requiresModification: true,
    goal: 'implement feature safely',
    acceptance: ['pass'],
    constraints: ['keep stable'],
    context: ['ctx'],
    scopeHint: ['src/app'],
  };

  try {
    const first = await handlers.dispatch({
      task_name: 'write-task-1',
      category: 'feature',
      ...writeTaskBase,
    });
    assert(first.status === 'dispatched', `首个写任务不应 hard-fail: ${JSON.stringify(first)}`);
    assert(first.degraded === true, '非 Git 写任务应标记 degraded=true');
    assert(typeof first.routing_reason === 'string' && first.routing_reason.includes('非 Git 工作区'), `首个写任务 routing_reason 缺少降级原因: ${first.routing_reason}`);
    assert(!first.error, `首个写任务不应返回 error: ${first.error}`);

    const second = await handlers.dispatch({
      task_name: 'write-task-2',
      category: 'review',
      ...writeTaskBase,
    });
    assert(second.status === 'dispatched', `第二个写任务不应 hard-fail: ${JSON.stringify(second)}`);
    assert(second.degraded === true, '第二个写任务应标记 degraded=true');
    assert(typeof second.routing_reason === 'string' && second.routing_reason.includes('非 Git 工作区'), `第二个写任务 routing_reason 缺少降级原因: ${second.routing_reason}`);
    assert(!second.error, `第二个写任务不应返回 error: ${second.error}`);

    const secondEntry = manager.activeBatch.getEntry(second.task_id);
    assert(secondEntry, '第二个写任务未注册到 activeBatch');
    assert(
      JSON.stringify(secondEntry.taskContract.dependsOn) === JSON.stringify([first.task_id]),
      `第二个写任务应自动依赖首个写任务，实际 dependsOn=${JSON.stringify(secondEntry.taskContract.dependsOn)}`,
    );

    const readonlyTask = await handlers.dispatch({
      task_name: 'read-task-1',
      category: 'analysis',
      requiresModification: false,
      goal: 'analyze structure',
      acceptance: ['pass'],
      constraints: ['readonly'],
      context: ['ctx'],
      scopeHint: ['src'],
    });
    assert(readonlyTask.status === 'dispatched', `只读任务不应受非 Git 写降级影响: ${JSON.stringify(readonlyTask)}`);
    assert(readonlyTask.degraded === false, `只读任务不应因非 Git 写模式被标记 degraded，实际=${readonlyTask.degraded}`);
    assert(typeof readonlyTask.routing_reason === 'string' && !readonlyTask.routing_reason.includes('非 Git 工作区'), `只读任务 routing_reason 不应含非 Git 写降级说明: ${readonlyTask.routing_reason}`);

    const nonGitInfoCount = notifications.filter((item) => item.message.includes('Git 隔离')).length;
    assert(nonGitInfoCount >= 1, '非 Git 串行降级应至少提示一次');

    console.log('\n=== non-git write serialization regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'non-git-write-no-hard-fail',
        'non-git-write-auto-serialized',
        'readonly-task-not-degraded',
      ],
      sample: {
        first,
        second,
        readonlyTask,
      },
      notifications,
    }, null, 2));
  } finally {
    manager.dispose();
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('non-git write serialization 回归失败:', error?.stack || error);
  process.exit(1);
});
