/**
 * Agent 模式响应时间测试
 * 测试完整的规划-分发-执行流程
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

const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');
const { UnifiedTaskManager } = require('../out/task/unified-task-manager');
const { SessionManagerTaskRepository } = require('../out/task/session-manager-task-repository');
const { SnapshotManager } = require('../out/snapshot-manager');
const { UnifiedSessionManager } = require('../out/session');
const { globalEventBus } = require('../out/events');

const events = [];
const testStart = Date.now();

function log(label, data = {}) {
  const elapsed = Date.now() - testStart;
  events.push({ elapsed, label, data });
  console.log(`[${elapsed}ms] ${label}`, Object.keys(data).length > 0 ? JSON.stringify(data) : '');
}

async function main() {
  log('测试开始');

  const workspacePath = process.cwd();
  const sessionManager = new UnifiedSessionManager(workspacePath);
  const snapshotManager = new SnapshotManager(workspacePath);
  const taskRepository = new SessionManagerTaskRepository(sessionManager);
  const taskManager = new UnifiedTaskManager(taskRepository, snapshotManager);

  const cliFactory = new CLIAdapterFactory({ cwd: workspacePath });

  const orchestrator = new IntelligentOrchestrator(
    cliFactory,
    sessionManager,
    snapshotManager,
    workspacePath
  );

  log('组件初始化完成');

  // 设置模式为 agent
  orchestrator.setInteractionMode('agent');
  log('设置 Agent 模式');

  // 监听关键事件
  globalEventBus.on('task:started', (e) => log('task:started', { taskId: e?.taskId }));
  globalEventBus.on('subtask:started', (e) => log('subtask:started', { cli: e?.data?.cli, todoId: e?.data?.todoId }));
  globalEventBus.on('subtask:completed', (e) => log('subtask:completed', { todoId: e?.data?.todoId }));
  globalEventBus.on('subtask:failed', (e) => log('subtask:failed', { error: e?.data?.error }));
  globalEventBus.on('task:completed', (e) => log('task:completed', { taskId: e?.taskId }));
  globalEventBus.on('task:failed', (e) => log('task:failed', e));
  globalEventBus.on('task:cancelled', (e) => log('task:cancelled', e));

  cliFactory.on('standardComplete', (msg) => {
    log('standardComplete', { source: msg.source, blocks: msg.blocks?.length });
  });

  // 测试简单任务 (强制 Agent 模式)
  const testPrompt = '创建一个名为 test-output.txt 的文件，内容为 "Hello from Agent Mode"';
  log(`执行任务: "${testPrompt}"`);

  try {
    const result = await Promise.race([
      orchestrator.execute(testPrompt),
      new Promise((_, reject) => setTimeout(() => reject(new Error('超时 60s')), 60000))
    ]);

    log('执行完成', { resultLength: String(result).length });
    console.log('\n结果预览:', String(result).substring(0, 300));
  } catch (err) {
    log('执行失败', { error: err.message });
    console.error('错误:', err);
  }

  // 打印事件时间线
  console.log('\n========== 事件时间线 ==========');
  events.forEach(e => {
    console.log(`  ${e.elapsed}ms: ${e.label}`, Object.keys(e.data).length > 0 ? JSON.stringify(e.data) : '');
  });

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
