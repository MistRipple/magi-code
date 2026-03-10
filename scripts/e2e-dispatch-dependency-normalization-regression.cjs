#!/usr/bin/env node
/**
 * dispatch_task depends_on 归一化回归脚本
 *
 * 覆盖目标：
 * 1) 依赖历史已完成任务：自动视为满足，不再 hard-fail
 * 2) 依赖未知任务：降级忽略，不再 hard-fail
 * 3) 依赖跨会话任务：降级忽略，避免跨会话串扰
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
  const currentSessionId = 'session-dependency-normalization';

  const orchestrationExecutor = {
    setAvailableWorkers() {},
    setCategoryWorkerMap() {},
    setHandlers(next) { handlers = next; },
  };
  const toolManager = {
    getOrchestrationExecutor() { return orchestrationExecutor; },
    refreshToolSchemas() {},
  };

  const manager = new DispatchManager({
    adapterFactory: {
      getToolManager() { return toolManager; },
    },
    profileLoader: {
      getEnabledProfiles() {
        return new Map([
          ['claude', { worker: 'claude', persona: { strengths: ['design'] } }],
          ['codex', { worker: 'codex', persona: { strengths: ['debug'] } }],
          ['gemini', { worker: 'gemini', persona: { strengths: ['analysis'] } }],
        ]);
      },
      getAssignmentLoader() {
        return {
          getCategoryMap() {
            return {
              backend: 'claude',
              debug: 'codex',
              data_analysis: 'gemini',
            };
          },
          reload() {},
        };
      },
      getAllCategories() {
        return new Map([
          ['backend', { displayName: 'backend' }],
          ['debug', { displayName: 'debug' }],
          ['data_analysis', { displayName: 'data_analysis' }],
        ]);
      },
      getWorkerForCategory(category) {
        if (category === 'backend') return 'claude';
        if (category === 'debug') return 'codex';
        if (category === 'data_analysis') return 'gemini';
        return 'claude';
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
    getCurrentSessionId: () => currentSessionId,
    getMissionIdsBySession: async () => [],
    ensureMissionForDispatch: async () => `mission-dependency-normalization-${++missionSeq}`,
    getCurrentTurnId: () => 'turn-dependency-normalization',
    getProjectKnowledgeBase: () => undefined,
    processWorkerWisdom() {},
    getSnapshotManager: () => null,
    getContextManager: () => null,
    getTodoManager: () => null,
    recordOrchestratorTokens() {},
    recordWorkerTokenUsage() {},
    getSupplementaryQueue: () => null,
  });

  // 本回归只验证注册与依赖归一化，不触发 Worker 执行。
  manager.scheduleDispatchReadyTasks = () => {};
  manager.resolveDispatchRouting = (_goal, category) => ({
    ok: true,
    decision: {
      selectedWorker: category === 'debug' ? 'codex' : 'claude',
      category,
      categorySource: 'explicit_param',
      degraded: false,
      routingReason: `route-${category}`,
    },
  });

  manager.setupOrchestrationToolHandlers();
  assert(handlers && typeof handlers.dispatch === 'function', 'dispatch handler 未注入');

  return { manager, handlers, notifications, currentSessionId };
}

async function main() {
  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-dispatch-dep-normalize-'));
  const { manager, handlers, notifications, currentSessionId } = createDispatchManager(DispatchManager, workspaceRoot);

  const baseTask = {
    category: 'backend',
    requiresModification: true,
    goal: 'normalize dependency',
    acceptance: ['a'],
    constraints: ['c'],
    context: ['ctx'],
    scopeHint: ['src/server'],
  };

  try {
    // 第一批：创建历史任务
    const first = await handlers.dispatch({
      task_name: 'base-task',
      ...baseTask,
    });
    assert(first.status === 'dispatched', `首个任务派发失败: ${JSON.stringify(first)}`);

    // 模拟第一批任务已完成（进入幂等账本历史）
    const updated = manager.dispatchIdempotencyStore.updateStatusByTaskId(first.task_id, 'completed');
    assert(updated && updated.status === 'completed', '历史任务状态未成功回写为 completed');

    // 切换到新批次，模拟“模型引用旧 task_id”
    manager.activeBatch = null;

    const second = await handlers.dispatch({
      task_name: 'depends-on-historical-completed',
      dependsOn: [first.task_id],
      ...baseTask,
    });
    assert(second.status === 'dispatched', `历史依赖不应 hard-fail: ${JSON.stringify(second)}`);
    assert(!second.error, `历史依赖不应返回 error: ${second.error}`);
    const secondEntry = manager.activeBatch.getEntry(second.task_id);
    assert(secondEntry, '第二个任务未注册到 activeBatch');
    assert(secondEntry.taskContract.dependsOn.length === 0, `历史已完成依赖应被视为满足，实际 dependsOn=${secondEntry.taskContract.dependsOn.join(',')}`);

    const unknownDependencyId = 'dispatch-unknown-predecessor-task';
    const third = await handlers.dispatch({
      task_name: 'depends-on-unknown',
      dependsOn: [unknownDependencyId],
      ...baseTask,
    });
    assert(third.status === 'dispatched', `未知依赖不应 hard-fail: ${JSON.stringify(third)}`);
    assert(third.degraded === true, '未知依赖应触发 degraded=true');
    assert(!third.error, `未知依赖不应返回 error: ${third.error}`);
    const thirdEntry = manager.activeBatch.getEntry(third.task_id);
    assert(thirdEntry, '第三个任务未注册到 activeBatch');
    assert(thirdEntry.taskContract.dependsOn.length === 0, `未知依赖应被忽略，实际 dependsOn=${thirdEntry.taskContract.dependsOn.join(',')}`);

    const crossSessionDependencyTaskId = 'dispatch-cross-session-completed';
    manager.dispatchIdempotencyStore.claimOrGet({
      key: `${currentSessionId}::cross::${crossSessionDependencyTaskId}`,
      sessionId: 'session-other',
      missionId: 'mission-other',
      taskId: crossSessionDependencyTaskId,
      worker: 'claude',
      category: 'backend',
      taskName: 'cross-session-task',
      routingReason: 'route-backend',
      degraded: false,
      status: 'completed',
    });

    const fourth = await handlers.dispatch({
      task_name: 'depends-on-cross-session',
      dependsOn: [crossSessionDependencyTaskId],
      ...baseTask,
    });
    assert(fourth.status === 'dispatched', `跨会话依赖不应 hard-fail: ${JSON.stringify(fourth)}`);
    assert(fourth.degraded === true, '跨会话依赖应触发 degraded=true');
    assert(!fourth.error, `跨会话依赖不应返回 error: ${fourth.error}`);
    const fourthEntry = manager.activeBatch.getEntry(fourth.task_id);
    assert(fourthEntry, '第四个任务未注册到 activeBatch');
    assert(fourthEntry.taskContract.dependsOn.length === 0, `跨会话依赖应被忽略，实际 dependsOn=${fourthEntry.taskContract.dependsOn.join(',')}`);

    const warningCount = notifications.filter(item => item.level === 'warning').length;
    assert(warningCount >= 2, `未知/跨会话依赖应有 warning 通知，实际: ${warningCount}`);

    console.log('\n=== dispatch dependency normalization regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'historical-completed-dependency-auto-satisfied',
        'unknown-dependency-soft-degraded',
        'cross-session-dependency-soft-degraded',
      ],
      sample: { first, second, third, fourth },
      warningCount,
    }, null, 2));
  } finally {
    manager.dispose();
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('dispatch dependency normalization 回归失败:', error?.stack || error);
  process.exit(1);
});

