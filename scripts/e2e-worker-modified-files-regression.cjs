#!/usr/bin/env node

const fs = require('fs');
const fsp = require('fs/promises');
const os = require('os');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {
          workspaceFolders: [],
          getConfiguration: () => ({ get: () => undefined }),
          fs: { stat: async () => ({}), readDirectory: async () => [], readFile: async () => Buffer.from('') },
          findFiles: async () => [],
          openTextDocument: async () => ({
            uri: { fsPath: '', toString: () => '' },
            getText: () => '',
            positionAt: () => ({ line: 0, character: 0 }),
            lineAt: () => ({ text: '' }),
            languageId: 'typescript',
            save: async () => true,
          }),
        },
        window: {
          createOutputChannel: () => ({ appendLine() {}, append() {}, clear() {}, show() {}, dispose() {} }),
          showErrorMessage: async () => undefined,
          showWarningMessage: async () => undefined,
          showInformationMessage: async () => undefined,
          onDidCloseTerminal: () => ({ dispose() {} }),
          onDidOpenTerminal: () => ({ dispose() {} }),
          createTerminal: () => ({ sendText() {}, show() {}, dispose() {} }),
          terminals: [],
        },
        commands: { executeCommand: async () => undefined, registerCommand: () => ({ dispose() {} }) },
        languages: { getDiagnostics: () => [] },
        env: { shell: process.env.SHELL || '/bin/bash', clipboard: { readText: async () => '', writeText: async () => {} } },
        Uri: {
          file: (p) => ({ fsPath: p, path: p, toString: () => p }),
          parse: (p) => ({ fsPath: p, path: p, toString: () => p }),
        },
        EventEmitter: class { constructor() { this.event = () => ({ dispose() {} }); } fire() {} dispose() {} },
        Disposable: class { dispose() {} },
        Position: class { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
        ViewColumn: { One: 1, Two: 2, Three: 3 },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function createWorker(AutonomousWorker, WorkerSessionManager) {
  const worker = new AutonomousWorker(
    'codex',
    { getProfile() { return {}; } },
    { buildSelfCheckGuidance() { return ''; } },
    {
      async prepareForExecution() { return true; },
      async start() {},
      async complete() {},
      async fail() {},
    },
    {
      contextAssembler: {},
      fileSummaryCache: {},
      sharedContextPool: { getByMission() { return []; }, add() {} },
    },
    new WorkerSessionManager({ autoCleanup: false }),
  );
  worker.discoverRelevantFiles = async () => [];
  worker.buildTargetFileContext = async () => null;
  worker.writeInsights = async () => {};
  worker.hasWorkerSharedFacts = () => true;
  worker.collectUnknownRequiredContracts = () => [];
  return worker;
}

function createAssignment(todo, overrides = {}) {
  return {
    id: 'assignment-modified-files',
    missionId: 'mission-modified-files',
    workerId: 'codex',
    shortTitle: 'modified-files-regression',
    responsibility: 'verify modified files behavior',
    delegationBriefing: 'verify modified files behavior',
    assignmentReason: {},
    scope: { includes: ['modified-files'], excludes: [], scopeHints: [], targetPaths: [], requiresModification: false, ...overrides.scope },
    guidancePrompt: '',
    producerContracts: [],
    consumerContracts: [],
    todos: [todo],
    planningStatus: 'pending',
    status: 'pending',
    progress: 0,
    createdAt: Date.now(),
    ...overrides,
  };
}

function createTodo(type) {
  return {
    id: `todo-${type}`,
    sessionId: 'session-modified-files',
    missionId: 'mission-modified-files',
    assignmentId: 'assignment-modified-files',
    content: `run ${type} task`,
    reasoning: 'regression',
    expectedOutput: 'verified',
    type,
    priority: 2,
    status: 'pending',
    dependsOn: [],
    requiredContracts: [],
  };
}

async function testRealModifiedFilesBackfill(ToolManager, AutonomousWorker) {
  const workspaceRoot = await fsp.mkdtemp(path.join(os.tmpdir(), 'magi-modified-files-'));
  const trackedFile = path.join(workspaceRoot, 'tracked.txt');
  await fsp.writeFile(trackedFile, 'tracked', 'utf8');

  const toolManager = new ToolManager({
    workspaceRoot,
    workspaceFolders: [{ name: path.basename(workspaceRoot), path: workspaceRoot }],
    permissions: { allowEdit: true, allowBash: false, allowWeb: false },
  });
  toolManager.setAuthorizationCallback(async () => true);
  toolManager.setSnapshotManager({ createSnapshotForMission() {} });
  toolManager.setSnapshotContext({
    sessionId: 'session-modified-files',
    missionId: 'mission-modified-files',
    assignmentId: 'assignment-modified-files',
    todoId: 'placeholder',
    workerId: 'codex',
  });

  const { WorkerSessionManager } = loadCompiledModule(path.join('orchestrator', 'worker', 'worker-session.js'));
  const worker = createWorker(AutonomousWorker, WorkerSessionManager);
  worker.executeWithWorker = async () => {
    const result = await toolManager.execute({
      id: 'remove-tracked',
      name: 'file_remove',
      arguments: { paths: ['tracked.txt'] },
    }, undefined, { workerId: 'codex', role: 'worker' });
    assert(result.isError !== true, `file_remove 执行失败: ${result.content}`);
    return { summary: 'removed tracked.txt' };
  };

  const todo = createTodo('fix');
  const assignment = createAssignment(todo);
  const outcome = await worker.executeTodo(
    todo,
    assignment,
    {
      workingDirectory: workspaceRoot,
      adapterFactory: { getToolManager: () => toolManager },
    },
    null,
  );

  assert(outcome.success === true, '真实写入场景下 executeTodo 应成功');
  assert(todo.output?.modifiedFiles?.includes('tracked.txt'), `modifiedFiles 未回填真实文件: ${JSON.stringify(todo.output)}`);
  assert(!fs.existsSync(trackedFile), '真实文件删除未生效');
  worker.dispose();
  toolManager.clearSnapshotContext('codex');
  await fsp.rm(workspaceRoot, { recursive: true, force: true });
}

function testReadOnlyTodoCanSucceedWithoutModifiedFiles(AutonomousWorker) {
  const { WorkerSessionManager } = loadCompiledModule(path.join('orchestrator', 'worker', 'worker-session.js'));
  const worker = createWorker(AutonomousWorker, WorkerSessionManager);
  const todo = { ...createTodo('verification'), output: { success: true, summary: 'verified', modifiedFiles: [], duration: 1 } };
  const assignment = createAssignment(todo, { scope: { requiresModification: false } });
  const gated = worker.applyQualityGate(assignment, { success: true, completedTodos: [todo], failedTodos: [], errors: [] }, { budgetUsage: 0 }, Date.now());
  assert(gated.success === true, `读/分析型 todo 不应因 modifiedFiles 为空失败: ${JSON.stringify(gated.errors || [])}`);
  worker.dispose();
}

function testRequiresModificationFailsWithoutRealChanges(AutonomousWorker) {
  const { WorkerSessionManager } = loadCompiledModule(path.join('orchestrator', 'worker', 'worker-session.js'));
  const worker = createWorker(AutonomousWorker, WorkerSessionManager);
  const todo = { ...createTodo('verification'), output: { success: true, summary: 'verified', modifiedFiles: [], duration: 1 } };
  const assignment = createAssignment(todo, { scope: { requiresModification: true } });
  const gated = worker.applyQualityGate(assignment, { success: true, completedTodos: [todo], failedTodos: [], errors: [] }, { budgetUsage: 0 }, Date.now());
  assert(gated.success === false, 'requiresModification=true 且无真实改动时必须失败');
  assert((gated.errors || []).some((item) => String(item).includes('Assignment required real file modifications')), `缺少 assignment 真实修改门禁错误: ${JSON.stringify(gated.errors || [])}`);
  worker.dispose();
}

function testWriteTodoFailsWithoutRealChanges(AutonomousWorker) {
  const { WorkerSessionManager } = loadCompiledModule(path.join('orchestrator', 'worker', 'worker-session.js'));
  const worker = createWorker(AutonomousWorker, WorkerSessionManager);
  const todo = { ...createTodo('fix'), output: { success: true, summary: 'done', modifiedFiles: [], duration: 1 } };
  const assignment = createAssignment(todo, { scope: { requiresModification: false } });
  const gated = worker.applyQualityGate(assignment, { success: true, completedTodos: [todo], failedTodos: [], errors: [] }, { budgetUsage: 0 }, Date.now());
  assert(gated.success === false, '实施类 todo 无真实改动时必须失败');
  assert((gated.errors || []).some((item) => String(item).includes('Completed write-required todos recorded no real file modifications')), `缺少实施类 todo 真实修改门禁错误: ${JSON.stringify(gated.errors || [])}`);
  worker.dispose();
}

async function main() {
  installVscodeStub();
  const { ToolManager } = loadCompiledModule(path.join('tools', 'tool-manager.js'));
  const { AutonomousWorker } = loadCompiledModule(path.join('orchestrator', 'worker', 'autonomous-worker.js'));

  await testRealModifiedFilesBackfill(ToolManager, AutonomousWorker);
  testReadOnlyTodoCanSucceedWithoutModifiedFiles(AutonomousWorker);
  testRequiresModificationFailsWithoutRealChanges(AutonomousWorker);
  testWriteTodoFailsWithoutRealChanges(AutonomousWorker);

  console.log('worker modified_files regression: ok');
}

main().catch((error) => {
  console.error(error?.stack || String(error));
  process.exit(1);
});