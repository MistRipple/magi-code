/**
 * 完整架构编排流程测试（无需真实 CLI）
 *
 * 覆盖点：
 * - 计划解析与多 CLI 分配（codex/gemini/claude）
 * - Worker 执行与文件修改
 * - 快照/变更统计与任务回填
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const Module = require('module');
const { EventEmitter } = require('events');

const assert = (cond, msg) => {
  if (!cond) {
    throw new Error(msg);
  }
};

// Mock vscode 模块
const originalModuleLoad = Module._load;
Module._load = function load(request, parent, isMain) {
  if (request === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
      Uri: { file: (p) => ({ fsPath: p, path: p }), parse: (s) => ({ fsPath: s, path: s }) },
      workspace: {
        workspaceFolders: [],
        getConfiguration: () => ({ get: () => undefined, update: () => Promise.resolve() }),
        onDidChangeConfiguration: () => ({ dispose: () => {} }),
      },
      window: {
        showInformationMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        createOutputChannel: () => ({ appendLine: () => {}, show: () => {}, dispose: () => {} }),
      },
      commands: { registerCommand: () => ({ dispose: () => {} }), executeCommand: () => Promise.resolve() },
      EventEmitter: class { event = () => {}; fire() {} dispose() {} },
    };
  }
  return originalModuleLoad(request, parent, isMain);
};

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const { IntelligentOrchestrator } = require(path.join(OUT, 'orchestrator/intelligent-orchestrator.js'));
const { TaskManager } = require(path.join(OUT, 'task-manager.js'));
const { SessionManager } = require(path.join(OUT, 'session-manager.js'));
const { SnapshotManager } = require(path.join(OUT, 'snapshot-manager.js'));
const { globalEventBus } = require(path.join(OUT, 'events.js'));

const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
};

function log(msg, color = 'reset') {
  console.log(`${colors[color]}${msg}${colors.reset}`);
}

function logSection(title) {
  console.log('\n' + '='.repeat(60));
  log(`  ${title}`, 'cyan');
  console.log('='.repeat(60));
}

function extractFilesFromText(text) {
  if (!text) return [];
  const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml|vue)/gi;
  const matches = text.match(filePattern) || [];
  return Array.from(new Set(matches));
}

class FakeAdapter extends EventEmitter {
  constructor(type, handler) {
    super();
    this.type = type;
    this._handler = handler;
    this._state = 'idle';
    this._connected = false;
    this._busy = false;
  }

  get state() {
    return this._state;
  }

  get isConnected() {
    return this._connected;
  }

  get isBusy() {
    return this._busy;
  }

  async connect() {
    this._connected = true;
    this._state = 'ready';
  }

  async disconnect() {
    this._connected = false;
    this._state = 'disconnected';
  }

  async sendMessage(message, _imagePaths, meta) {
    this._busy = true;
    this._state = 'busy';
    try {
      return await this._handler(this.type, message, meta);
    } finally {
      this._busy = false;
      this._state = 'ready';
    }
  }

  async interrupt() {}
}

class FakeCLIAdapterFactory extends EventEmitter {
  constructor(workspaceRoot, plan) {
    super();
    this.workspaceRoot = workspaceRoot;
    this.plan = plan;
    this.adapters = new Map();
  }

  getOrCreate(type, role = 'worker') {
    const key = `${type}:${role}`;
    if (!this.adapters.has(key)) {
      const adapter = new FakeAdapter(type, this.handleMessage.bind(this));
      this.adapters.set(key, adapter);
    }
    return this.adapters.get(key);
  }

  getAdapter(type, role = 'worker') {
    const key = `${type}:${role}`;
    return this.adapters.get(key);
  }

  getAllAdapters() {
    return Array.from(this.adapters.values());
  }

  async sendMessage(type, message, _imagePaths, options) {
    const role = options?.adapterRole ?? (options?.source === 'orchestrator' ? 'orchestrator' : 'worker');
    const adapter = this.getOrCreate(type, role);
    if (!adapter.isConnected) {
      await adapter.connect();
    }
    return adapter.sendMessage(message, undefined, options?.messageMeta);
  }

  getAllStatus() {
    return ['claude', 'codex', 'gemini'].map(type => ({
      type,
      connected: true,
      busy: false,
      state: 'ready',
    }));
  }

  emitOrchestratorMessageToUI(_type, _message) {}

  async handleMessage(type, message, meta) {
    const intent = meta?.intent || '';
    if (intent === 'orchestrator_analyze') {
      return {
        content: JSON.stringify(this.plan, null, 2),
        done: true,
      };
    }

    if (intent === 'worker_execute') {
      const files = extractFilesFromText(message);
      files.forEach((file) => {
        const abs = path.join(this.workspaceRoot, file);
        const dir = path.dirname(abs);
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true });
        }
        const suffix = file.endsWith('.py') ? '# updated by ' : '// updated by ';
        fs.appendFileSync(abs, `\n${suffix}${type}\n`, 'utf-8');
      });
      return {
        content: `完成任务(${type})`,
        done: true,
        fileChanges: files.map(filePath => ({ filePath, type: 'modify' })),
      };
    }

    return { content: 'OK', done: true };
  }
}

async function runTest() {
  logSection('1. 初始化测试工作区');
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'multicli-arch-'));
  const backendFile = path.join(tmpRoot, 'backend', 'app.py');
  const frontendFile = path.join(tmpRoot, 'frontend', 'Login.vue');
  fs.mkdirSync(path.dirname(backendFile), { recursive: true });
  fs.mkdirSync(path.dirname(frontendFile), { recursive: true });
  fs.writeFileSync(backendFile, 'print("hello")\n', 'utf-8');
  fs.writeFileSync(frontendFile, '<template></template>\n', 'utf-8');
  log(`工作区: ${tmpRoot}`, 'blue');

  logSection('2. 构建假编排环境');
  const sessionManager = new SessionManager(tmpRoot);
  const taskManager = new TaskManager(sessionManager);
  const snapshotManager = new SnapshotManager(sessionManager, tmpRoot);
  const plan = {
    analysis: '全栈登录功能，需要前后端协作',
    isSimpleTask: false,
    needsWorker: true,
    needsUserInput: false,
    questions: [],
    needsCollaboration: true,
    featureContract: 'Python 提供登录 API，Vue 提供登录表单与交互',
    acceptanceCriteria: ['登录成功后返回 token', '前端表单校验并可登录'],
    subTasks: [
      {
        id: 'backend-1',
        description: '实现 Python 登录 API',
        assignedWorker: 'codex',
        reason: '后端实现',
        targetFiles: ['backend/app.py'],
        dependencies: [],
        prompt: '实现登录接口，包含账号密码校验与 token 返回',
      },
      {
        id: 'frontend-1',
        description: '实现 Vue 登录页面',
        assignedWorker: 'gemini',
        reason: '前端实现',
        targetFiles: ['frontend/Login.vue'],
        dependencies: [],
        prompt: '实现登录表单、校验与提交逻辑',
      },
    ],
    executionMode: 'parallel',
    summary: '拆分前后端子任务并行执行',
  };

  const fakeFactory = new FakeCLIAdapterFactory(tmpRoot, plan);
  const orchestrator = new IntelligentOrchestrator(
    fakeFactory,
    taskManager,
    snapshotManager,
    tmpRoot,
    {
      integration: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
      maxRetries: 1,
    }
  );
  orchestrator.setInteractionMode('agent');
  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setQuestionCallback(async () => '');
  await orchestrator.initialize();
  log('编排器初始化完成', 'green');

  logSection('3. 执行完整架构编排任务');
  const prompt = '做一个登录功能，包含前后端，python和vue';
  const result = await orchestrator.execute(prompt);
  assert(typeof result === 'string' && result.length >= 0, '编排执行未返回结果');
  log('任务执行完成', 'green');

  logSection('4. 验证任务分发与文件变更');
  const tasks = taskManager.getAllTasks();
  assert(tasks.length > 0, '未创建 Task');
  const task = tasks[tasks.length - 1];
  assert(task.subTasks.length >= 2, '子任务数量不足');

  const workers = task.subTasks.map(st => st.assignedWorker);
  assert(workers.includes('codex'), '未分配 codex 子任务');
  assert(workers.includes('gemini'), '未分配 gemini 子任务');
  log('子任务 CLI 分配正确', 'green');

  const modifiedCounts = task.subTasks.map(st => (st.modifiedFiles || []).length);
  assert(modifiedCounts.some(c => c > 0), '未回填 modifiedFiles');
  log(`modifiedFiles 回填成功: ${modifiedCounts.join(', ')}`, 'green');

  const pendingChanges = snapshotManager.getPendingChanges();
  assert(pendingChanges.length >= 2, '快照变更未记录');
  log(`快照变更记录: ${pendingChanges.length} 个`, 'green');

  const backendContent = fs.readFileSync(backendFile, 'utf-8');
  const frontendContent = fs.readFileSync(frontendFile, 'utf-8');
  assert(backendContent.includes('updated by codex'), '后端文件未被修改');
  assert(frontendContent.includes('updated by gemini'), '前端文件未被修改');
  log('文件修改验证通过', 'green');

  logSection('测试结果');
  log('✅ 完整架构编排流程通过', 'green');
}

runTest()
  .then(() => process.exit(0))
  .catch((error) => {
    log(`❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
    process.exit(1);
  });
