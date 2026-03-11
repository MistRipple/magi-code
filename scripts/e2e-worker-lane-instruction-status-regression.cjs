#!/usr/bin/env node

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
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function createDispatchManager(DispatchManager, workspaceRoot, workerInstructions) {
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

  const manager = new DispatchManager({
    adapterFactory: {
      getToolManager() { return toolManager; },
    },
    profileLoader: {
      getEnabledProfiles() {
        return new Map([
          ['codex', { worker: 'codex', persona: { strengths: ['debug'] } }],
        ]);
      },
      getAssignmentLoader() {
        return {
          getCategoryMap() {
            return { debug: 'codex' };
          },
          reload() {},
        };
      },
      getAllCategories() {
        return new Map([['debug', { displayName: 'debug' }]]);
      },
      getWorkerForCategory() {
        return 'codex';
      },
      getCategory(category) {
        return { name: category };
      },
    },
    messageHub: {
      notify() {},
      subTaskCard() {},
      workerInstruction(worker, content, metadata) {
        workerInstructions.push({ worker, content, metadata });
      },
    },
    missionOrchestrator: new EventEmitter(),
    workspaceRoot,
    getActiveUserPrompt: () => '',
    getActiveImagePaths: () => undefined,
    getCurrentSessionId: () => 'session-worker-lane-status',
    getMissionIdsBySession: async () => [],
    ensureMissionForDispatch: async () => 'mission-worker-lane-status',
    getCurrentTurnId: () => 'turn-worker-lane-status',
    getProjectKnowledgeBase: () => undefined,
    processWorkerWisdom() {},
    getSnapshotManager: () => null,
    getContextManager: () => null,
    getTodoManager: () => null,
    recordOrchestratorTokens() {},
    recordWorkerTokenUsage() {},
    getSupplementaryQueue: () => null,
  });

  manager.scheduleDispatchReadyTasks = () => {};
  manager.resolveDispatchRouting = () => ({
    ok: true,
    decision: {
      selectedWorker: 'codex',
      category: 'debug',
      categorySource: 'explicit_param',
      degraded: false,
      routingReason: 'route-debug',
    },
  });

  manager.setupOrchestrationToolHandlers();
  assert(handlers && typeof handlers.dispatch === 'function', 'dispatch handler 未注入');
  return { manager, handlers };
}

function latestWorkerInstruction(workerInstructions) {
  assert(workerInstructions.length > 0, '未捕获到 workerInstruction');
  return workerInstructions[workerInstructions.length - 1];
}

async function main() {
  const dispatchManagerSource = fs.readFileSync(path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts'), 'utf8');
  const instructionCardSource = fs.readFileSync(path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'components', 'InstructionCard.svelte'), 'utf8');
  assert(!dispatchManagerSource.includes("if (isCurrent) {\n      return t('dispatch.lane.status.running');"), 'DispatchManager 仍将 current 强制映射为 running');
  assert(!instructionCardSource.includes('task-status status-running">{statusLabel(currentTask.status)}</span>'), 'InstructionCard 当前行仍硬编码 status-running');
  assert(instructionCardSource.includes('statusClass(currentTask.status)'), 'InstructionCard 当前行未按真实状态动态映射样式');

  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-worker-lane-status-'));
  const workerInstructions = [];
  const { manager, handlers } = createDispatchManager(DispatchManager, workspaceRoot, workerInstructions);

  const baseTask = {
    category: 'debug',
    requiresModification: true,
    goal: 'lane status',
    acceptance: ['acceptance'],
    constraints: ['constraints'],
    context: ['context'],
    scopeHint: ['src/debug.ts'],
  };

  try {
    const first = await handlers.dispatch({
      task_name: '[Review] Schema数据模型质量审查',
      ...baseTask,
    });
    const second = await handlers.dispatch({
      task_name: '[Fix] Schema数据模型修复',
      dependsOn: [first.task_id],
      ...baseTask,
    });

    const batch = manager.activeBatch;
    assert(batch, 'activeBatch 未创建');

    batch.markRunning(first.task_id);
    let latest = latestWorkerInstruction(workerInstructions);
    assert(latest.metadata?.laneCurrentTaskId === first.task_id, '运行态 laneCurrentTaskId 应指向首任务');
    assert(latest.metadata?.laneTasks?.[0]?.status === 'running', '运行态 laneTasks[0] 应为 running');

    batch.markCompleted(first.task_id, { success: true, summary: 'done' });
    latest = latestWorkerInstruction(workerInstructions);
    const laneTasks = latest.metadata?.laneTasks || [];
    const taskOne = laneTasks.find((item) => item.taskId === first.task_id);
    const taskTwo = laneTasks.find((item) => item.taskId === second.task_id);
    assert(taskOne?.status === 'completed', '完成后首任务 lane status 应更新为 completed');
    assert(taskTwo?.status === 'pending', '依赖就绪后第二任务应更新为 pending');
    assert(latest.metadata?.laneCurrentTaskId === second.task_id, '依赖就绪后 laneCurrentTaskId 应切换到下一任务');
    assert(typeof latest.content === 'string' && latest.content.includes('已完成'), 'lane 文案应包含终态标签，不能继续显示进行中');

    console.log('\n=== worker lane instruction status regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'current_not_forced_to_running',
        'task_status_changed_refreshes_lane_card',
        'task_ready_refreshes_lane_focus',
        'instruction_card_current_row_not_hardcoded_running',
      ],
    }, null, 2));
  } finally {
    manager.dispose();
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error('worker lane instruction status 回归失败:', error?.stack || error);
  process.exit(1);
});