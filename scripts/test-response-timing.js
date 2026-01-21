/**
 * 响应时间端到端测试
 * 测量从用户消息到各组件响应的完整流程时间
 *
 * 用法:
 *   node scripts/test-response-timing.js
 */

// Mock vscode module
const Module = require('module');
const originalModuleLoad = Module._load;
Module._load = function load(request, parent, isMain) {
  if (request === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
      window: {
        showInformationMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        createOutputChannel: () => ({ appendLine: () => {}, show: () => {}, dispose: () => {} }),
      },
      workspace: {
        workspaceFolders: [],
        getConfiguration: () => ({ get: () => undefined, update: () => Promise.resolve() }),
        onDidChangeConfiguration: () => ({ dispose: () => {} }),
      },
      commands: { registerCommand: () => ({ dispose: () => {} }), executeCommand: () => Promise.resolve() },
      EventEmitter: class { event = () => {}; fire() {} dispose() {} },
      Uri: { file: (p) => ({ fsPath: p, path: p }), parse: (s) => ({ fsPath: s, path: s }) },
    };
  }
  return originalModuleLoad(request, parent, isMain);
};

const path = require('path');
const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');
const { UnifiedTaskManager } = require('../out/task/unified-task-manager');
const { SessionManagerTaskRepository } = require('../out/task/session-manager-task-repository');
const { SnapshotManager } = require('../out/snapshot-manager');
const { UnifiedSessionManager } = require('../out/session');
const { globalEventBus } = require('../out/events');

// 时间记录
const timings = {
  testStart: 0,
  initStart: 0,
  initEnd: 0,
  executeStart: 0,
  firstStreamEvent: 0,
  firstStandardComplete: 0,
  planningStart: 0,
  planningEnd: 0,
  workerDispatchStart: 0,
  firstWorkerResponse: 0,
  executeEnd: 0,
};

const events = [];

function logTiming(label) {
  const now = Date.now();
  const elapsed = now - timings.testStart;
  events.push({ label, time: now, elapsed });
  console.log(`[${elapsed}ms] ${label}`);
  return now;
}

function printSummary() {
  console.log('\n========== 响应时间统计 ==========');

  const init = timings.initEnd - timings.initStart;
  console.log(`初始化耗时: ${init}ms`);

  if (timings.executeStart && timings.executeEnd) {
    const total = timings.executeEnd - timings.executeStart;
    console.log(`总执行时间: ${total}ms`);
  }

  if (timings.firstStreamEvent) {
    const toFirstStream = timings.firstStreamEvent - timings.executeStart;
    console.log(`首次流事件: ${toFirstStream}ms`);
  }

  if (timings.firstStandardComplete) {
    const toFirstComplete = timings.firstStandardComplete - timings.executeStart;
    console.log(`首次消息完成: ${toFirstComplete}ms`);
  }

  if (timings.planningStart && timings.planningEnd) {
    const planning = timings.planningEnd - timings.planningStart;
    console.log(`规划阶段耗时: ${planning}ms`);
  }

  if (timings.workerDispatchStart) {
    const toDispatch = timings.workerDispatchStart - timings.executeStart;
    console.log(`Worker 分发时间: ${toDispatch}ms`);
  }

  if (timings.firstWorkerResponse) {
    const toWorkerResponse = timings.firstWorkerResponse - timings.executeStart;
    console.log(`首次 Worker 响应: ${toWorkerResponse}ms`);
  }

  console.log('\n========== 事件时间线 ==========');
  events.forEach(e => {
    console.log(`  ${e.elapsed}ms: ${e.label}`);
  });
}

async function main() {
  timings.testStart = Date.now();
  logTiming('测试开始');

  const workspacePath = process.cwd();

  // 初始化
  timings.initStart = logTiming('开始初始化组件');

  const sessionManager = new UnifiedSessionManager(workspacePath);
  const snapshotManager = new SnapshotManager(workspacePath);
  const taskRepository = new SessionManagerTaskRepository(sessionManager);
  const taskManager = new UnifiedTaskManager(taskRepository, snapshotManager);

  const cliFactory = new CLIAdapterFactory({
    cwd: workspacePath,
  });

  // 正确的构造函数参数顺序
  const orchestrator = new IntelligentOrchestrator(
    cliFactory,
    sessionManager,
    snapshotManager,
    workspacePath
  );

  timings.initEnd = logTiming('组件初始化完成');

  // 设置事件监听
  let streamEventCount = 0;
  let standardCompleteCount = 0;

  cliFactory.on('stream', (data) => {
    streamEventCount++;
    if (streamEventCount === 1) {
      timings.firstStreamEvent = logTiming('首次流事件');
    }
  });

  cliFactory.on('standardComplete', (msg) => {
    standardCompleteCount++;
    if (standardCompleteCount === 1) {
      timings.firstStandardComplete = logTiming('首次消息完成');
    }
    console.log(`  [standardComplete] source=${msg.source}, blocks=${msg.blocks?.length || 0}`);
  });

  globalEventBus.on('orchestrator:planning_start', () => {
    timings.planningStart = logTiming('规划开始');
  });

  globalEventBus.on('orchestrator:planning_complete', () => {
    timings.planningEnd = logTiming('规划完成');
  });

  globalEventBus.on('subtask:started', (event) => {
    if (!timings.workerDispatchStart) {
      timings.workerDispatchStart = logTiming(`Worker 分发: ${event.data?.cli || 'unknown'}`);
    }
  });

  globalEventBus.on('worker:response', () => {
    if (!timings.firstWorkerResponse) {
      timings.firstWorkerResponse = logTiming('首次 Worker 响应');
    }
  });

  // 测试简单问题
  console.log('\n========== 测试: 简单问题 ==========');
  const testPrompt = '你好，请用一句话介绍你自己';

  timings.executeStart = logTiming(`开始执行: "${testPrompt}"`);

  try {
    const result = await Promise.race([
      orchestrator.execute(testPrompt, ''),
      new Promise((_, reject) => setTimeout(() => reject(new Error('执行超时 (30s)')), 30000))
    ]);

    timings.executeEnd = logTiming('执行完成');
    console.log(`\n结果: ${String(result).substring(0, 200)}...`);
  } catch (err) {
    timings.executeEnd = logTiming('执行失败');
    console.error('错误:', err.message);
  }

  printSummary();

  // 清理
  try {
    await orchestrator.shutdown?.();
    await cliFactory.shutdown?.();
  } catch {}

  process.exit(0);
}

main().catch(err => {
  console.error('测试失败:', err);
  process.exit(1);
});
