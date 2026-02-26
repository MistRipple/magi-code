#!/usr/bin/env node
/**
 * 验证“懒创建任务”链路：
 * 1) 静态校验：Mission 仅在 dispatch 路径创建
 * 2) 运行时校验：无 dispatch 的普通对话不产生 TaskView
 * 3) 运行时校验：有 dispatch 的对话会创建任务并进入任务流
 *
 * 运行方式：
 *   npm run compile
 *   node scripts/verify-lazy-task-creation.js
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const Module = require('module');
const { EventEmitter } = require('events');

const ROOT = path.resolve(__dirname, '..');
const SRC = path.join(ROOT, 'src');
const OUT = path.join(ROOT, 'out');

let totalChecks = 0;
let passedChecks = 0;
let failedChecks = 0;

function check(name, condition, detail) {
  totalChecks++;
  if (condition) {
    passedChecks++;
    console.log(`✅ ${name}${detail ? ` (${detail})` : ''}`);
    return;
  }
  failedChecks++;
  console.log(`❌ ${name}${detail ? ` (${detail})` : ''}`);
}

function readSrc(relativePath) {
  return fs.readFileSync(path.join(SRC, relativePath), 'utf8');
}

function runStaticChecks() {
  const mde = readSrc('orchestrator/core/mission-driven-engine.ts');
  const dm = readSrc('orchestrator/core/dispatch-manager.ts');
  const mo = readSrc('orchestrator/core/mission-orchestrator.ts');
  const ms = readSrc('orchestrator/mission/mission-storage.ts');
  const ebs = readSrc('ui/event-binding-service.ts');

  const mdeCreateMissionCount = (mde.match(/createMission\(/g) || []).length;
  check('MDE 仅保留一个 createMission 调用', mdeCreateMissionCount === 1, `count=${mdeCreateMissionCount}`);
  check('MDE 暴露 ensureMissionForDispatch', /private\s+async\s+ensureMissionForDispatch\(/.test(mde));
  check('DispatchManager 在 dispatch 路径调用 ensureMissionForDispatch', dm.includes('missionId = await this.deps.ensureMissionForDispatch();'));
  check('DispatchBatch 使用 missionId 作为 batchId', dm.includes('this.activeBatch = new DispatchBatch(missionId);'));
  check('MissionStorage 发出 missionDeleted', ms.includes("this.emit('missionDeleted'"));
  check('MissionOrchestrator 转发 missionDeleted', mo.includes("this.storage.on('missionDeleted'"));
  check('EventBindingService 监听 missionDeleted', ebs.includes("mo.on('missionDeleted'"));
}

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {
          workspaceFolders: [],
          getConfiguration: () => ({ get: () => undefined }),
        },
        window: {
          createOutputChannel: () => ({ appendLine() {}, append() {}, clear() {}, show() {}, dispose() {} }),
          showErrorMessage: async () => undefined,
          showWarningMessage: async () => undefined,
          showInformationMessage: async () => undefined,
          onDidCloseTerminal: () => ({ dispose() {} }),
          createTerminal: () => ({
            sendText() {},
            show() {},
            dispose() {},
          }),
        },
        env: {},
        Uri: { file: (p) => ({ fsPath: p }) },
        EventEmitter: class {
          constructor() { this.event = () => {}; }
          fire() {}
          dispose() {}
        },
        Disposable: class { dispose() {} },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

function assertCompileOutputExists() {
  const entry = path.join(OUT, 'orchestrator/core/mission-driven-engine.js');
  if (!fs.existsSync(entry)) {
    throw new Error(`缺少编译产物: ${entry}。请先运行 npm run compile`);
  }
}

class MockToolManager {
  setPermissions() {}
  setSnapshotContext() {}
  clearSnapshotContext() {}
  async buildToolsSummary() { return ''; }
}

class NonDispatchAdapterFactory extends EventEmitter {
  constructor() {
    super();
    this.toolManager = new MockToolManager();
  }
  async sendMessage() {
    return {
      content: '这是直接回答（无 dispatch）。',
      done: true,
      tokenUsage: { inputTokens: 8, outputTokens: 6 },
    };
  }
  async interrupt() {}
  async interruptAll() {}
  async shutdown() {}
  isConnected() { return true; }
  isBusy() { return false; }
  clearAdapterHistory() {}
  clearAllAdapterHistories() {}
  getAdapterHistoryInfo() { return null; }
  getToolManager() { return this.toolManager; }
  async clearAdapter() {}
  getMCPExecutor() { return null; }
  async reloadMCP() {}
  async reloadSkills() {}
  async refreshUserRules() {}
  getEnvironmentPrompt() { return ''; }
  getUserRulesPrompt() { return ''; }
}

function loadRuntimeModules() {
  const { MissionDrivenEngine } = require(path.join(OUT, 'orchestrator/core/mission-driven-engine.js'));
  const { UnifiedSessionManager } = require(path.join(OUT, 'session/unified-session-manager.js'));
  const { SnapshotManager } = require(path.join(OUT, 'snapshot-manager.js'));
  const { ToolManager } = require(path.join(OUT, 'tools/tool-manager.js'));
  return { MissionDrivenEngine, UnifiedSessionManager, SnapshotManager, ToolManager };
}

class DispatchingAdapterFactory extends EventEmitter {
  constructor(toolManager, getEngine) {
    super();
    this.toolManager = toolManager;
    this.getEngine = getEngine;
  }

  async sendMessage(agent) {
    if (agent !== 'orchestrator') {
      return {
        content: 'Worker mock 响应',
        done: true,
        tokenUsage: { inputTokens: 5, outputTokens: 5 },
      };
    }

    const dispatchResult = await this.toolManager.getOrchestrationExecutor().execute({
      id: `tc-dispatch-${Date.now()}`,
      name: 'dispatch_task',
      arguments: {
        worker: 'auto',
        category: 'general',
        task: '最小分发验证任务',
        requires_modification: true,
      },
    });

    if (dispatchResult.isError) {
      return {
        content: '',
        done: true,
        error: `dispatch_task 失败: ${dispatchResult.content}`,
        tokenUsage: { inputTokens: 6, outputTokens: 4 },
      };
    }

    const engine = this.getEngine();
    const batch = engine?.dispatchManager?.getActiveBatch?.();
    if (batch && batch.status !== 'archived') {
      batch.transitionTo('archived');
    }

    return {
      content: `已完成分发: ${dispatchResult.content}`,
      done: true,
      tokenUsage: { inputTokens: 12, outputTokens: 9 },
    };
  }

  async interrupt() {}
  async interruptAll() {}
  async shutdown() {}
  isConnected() { return true; }
  isBusy() { return false; }
  clearAdapterHistory() {}
  clearAllAdapterHistories() {}
  getAdapterHistoryInfo() { return null; }
  getToolManager() { return this.toolManager; }
  async clearAdapter() {}
  getMCPExecutor() { return null; }
  async reloadMCP() {}
  async reloadSkills() {}
  async refreshUserRules() {}
  getEnvironmentPrompt() { return ''; }
  getUserRulesPrompt() { return ''; }
}

async function runRuntimeNoDispatchCheck(modules) {
  const { MissionDrivenEngine, UnifiedSessionManager, SnapshotManager } = modules;

  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-lazy-task-'));
  const sessionManager = new UnifiedSessionManager(workspace);
  const session = sessionManager.createSession('lazy-task-runtime-check');
  const snapshotManager = new SnapshotManager(sessionManager, workspace);
  const adapterFactory = new NonDispatchAdapterFactory();
  const engine = new MissionDrivenEngine(
    adapterFactory,
    { timeout: 120000, maxRetries: 1 },
    workspace,
    snapshotManager,
    sessionManager
  );

  try {
    const before = await engine.listTaskViews(session.id);
    const taskContext = await engine.executeWithTaskContext('请直接回答一句话，不需要调用任何工具。', session.id);
    const after = await engine.listTaskViews(session.id);

    check('运行时：普通对话前任务数为 0', before.length === 0, `before=${before.length}`);
    check('运行时：普通对话后任务数仍为 0', after.length === 0, `after=${after.length}`);
    check('运行时：无 dispatch 时 taskId 为空', taskContext.taskId === '', `taskId=${taskContext.taskId || '<empty>'}`);
  } finally {
    engine.dispose();
  }
}

async function runRuntimeDispatchCheck(modules) {
  const { MissionDrivenEngine, UnifiedSessionManager, SnapshotManager, ToolManager } = modules;

  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-dispatch-task-'));
  const sessionManager = new UnifiedSessionManager(workspace);
  const session = sessionManager.createSession('dispatch-task-runtime-check');
  const snapshotManager = new SnapshotManager(sessionManager, workspace);
  const toolManager = new ToolManager({ workspaceRoot: workspace });

  let engineRef = null;
  const adapterFactory = new DispatchingAdapterFactory(toolManager, () => engineRef);
  const engine = new MissionDrivenEngine(
    adapterFactory,
    { timeout: 120000, maxRetries: 1 },
    workspace,
    snapshotManager,
    sessionManager
  );
  engineRef = engine;

  try {
    await engine.initialize();

    const dispatchManager = engine.dispatchManager;
    dispatchManager.resolveDispatchRouting = function resolveDispatchRoutingForTest() {
      return {
        ok: true,
        decision: {
          requestedWorker: 'auto',
          selectedWorker: 'codex',
          category: 'general',
          categorySource: 'explicit_param',
          degraded: false,
          routingReason: '测试注入路由',
        },
      };
    };
    dispatchManager.dispatchReadyTasksWithIsolation = function dispatchReadyTasksWithIsolationForTest() {
      // 运行时验证只关注“创建任务时机”，不在此脚本触发真实 Worker 执行。
    };

    const before = await engine.listTaskViews(session.id);
    const taskContext = await engine.executeWithTaskContext('请分发一个子任务进行验证。', session.id);
    const after = await engine.listTaskViews(session.id);
    const createdTask = after[0];

    check('运行时：dispatch 前任务数为 0', before.length === 0, `before=${before.length}`);
    check('运行时：dispatch 后任务数为 1', after.length === 1, `after=${after.length}`);
    check('运行时：dispatch 后 taskId 非空', taskContext.taskId.length > 0, `taskId=${taskContext.taskId || '<empty>'}`);
    check('运行时：dispatch 任务状态进入完成态', createdTask?.status === 'completed', `status=${createdTask?.status || '<none>'}`);
  } finally {
    engine.dispose();
  }
}

async function main() {
  installVscodeStub();
  assertCompileOutputExists();
  const modules = loadRuntimeModules();

  console.log('=== 懒创建任务链路验证 ===');
  runStaticChecks();
  await runRuntimeNoDispatchCheck(modules);
  await runRuntimeDispatchCheck(modules);
  console.log(`\n结果: ${passedChecks}/${totalChecks} 通过，${failedChecks} 失败`);
  process.exit(failedChecks > 0 ? 1 : 0);
}

main().catch((error) => {
  console.error(`\n验证失败: ${error instanceof Error ? error.message : String(error)}`);
  process.exit(1);
});
