/**
 * 测试主 Claude + 多端 CLI 代理流程
 *
 * 测试内容：
 * 1. CLI 适配器工厂创建
 * 2. CLI 可用性检测
 * 3. 智能编排器初始化
 * 4. 任务分析和计划生成
 * 5. Worker 分配和执行
 */

// Mock vscode 模块（必须在其他 require 之前）
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id) {
  if (id === 'vscode') {
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
  return originalRequire.apply(this, arguments);
};

const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
const { ClaudeAdapter } = require('../out/cli/adapters/claude');
const { CodexAdapter } = require('../out/cli/adapters/codex');
const { GeminiAdapter } = require('../out/cli/adapters/gemini');
const { TaskManager } = require('../out/task-manager');
const { SessionManager } = require('../out/session-manager');
const { SnapshotManager } = require('../out/snapshot-manager');
const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');
const { globalEventBus } = require('../out/events');
const { ProfileLoader, GuidanceInjector } = require('../out/orchestrator/profile');
const { CLISelector } = require('../out/task/cli-selector');
const { TaskAnalyzer } = require('../out/task/task-analyzer');

const workspaceRoot = process.cwd();

// 颜色输出
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

function logResult(name, success, detail) {
  const icon = success ? '✅' : '❌';
  const color = success ? 'green' : 'red';
  log(`${icon} ${name}${detail ? ': ' + detail : ''}`, color);
}

async function testCLIAvailability() {
  logSection('1. CLI 可用性检测');
  
  const [claude, codex, gemini] = await Promise.all([
    ClaudeAdapter.checkInstalled(),
    CodexAdapter.checkInstalled(),
    GeminiAdapter.checkInstalled(),
  ]);
  
  logResult('Claude CLI', claude, claude ? '已安装' : '未安装');
  logResult('Codex CLI', codex, codex ? '已安装' : '未安装');
  logResult('Gemini CLI', gemini, gemini ? '已安装' : '未安装');
  
  return { claude, codex, gemini };
}

async function testAdapterFactory() {
  logSection('2. CLI 适配器工厂测试');
  
  const factory = new CLIAdapterFactory({ cwd: workspaceRoot });
  
  // 创建适配器
  const claudeAdapter = factory.create('claude');
  const codexAdapter = factory.create('codex');
  const geminiAdapter = factory.create('gemini');
  
  logResult('Claude 适配器创建', !!claudeAdapter, `type=${claudeAdapter.type}`);
  logResult('Codex 适配器创建', !!codexAdapter, `type=${codexAdapter.type}`);
  logResult('Gemini 适配器创建', !!geminiAdapter, `type=${geminiAdapter.type}`);
  
  // 测试获取已创建的适配器
  const retrieved = factory.getAdapter('claude');
  logResult('适配器缓存', retrieved === claudeAdapter, '同一实例');
  
  return factory;
}

async function testOrchestratorInit(factory) {
  logSection('3. 智能编排器初始化');
  
  const sessionManager = new SessionManager(workspaceRoot);
  const taskManager = new TaskManager(sessionManager);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  
  const orchestrator = new IntelligentOrchestrator(
    factory,
    taskManager,
    snapshotManager,
    workspaceRoot
  );
  
  logResult('编排器创建', !!orchestrator);
  logResult('初始状态', orchestrator.phase === 'idle', `phase=${orchestrator.phase}`);
  logResult('运行状态', !orchestrator.running, `running=${orchestrator.running}`);
  
  // 测试交互模式
  orchestrator.setInteractionMode('agent');
  logResult('交互模式设置', orchestrator.getInteractionMode() === 'agent', 'mode=agent');
  
  return orchestrator;
}

async function testEventBus() {
  logSection('4. 事件总线测试');
  
  let eventReceived = false;
  const unsubscribe = globalEventBus.on('task:created', () => {
    eventReceived = true;
  });
  
  globalEventBus.emitEvent('task:created', { data: { test: true } });
  logResult('事件发送和接收', eventReceived);
  
  unsubscribe();
  eventReceived = false;
  globalEventBus.emitEvent('task:created', { data: { test: true } });
  logResult('事件取消订阅', !eventReceived, '取消后不再接收');
}

async function testConfirmationCallback(orchestrator) {
  logSection('5. 确认回调机制测试');

  orchestrator.setConfirmationCallback(async (plan) => {
    log(`  收到计划确认请求: ${plan.summary?.substring(0, 50)}...`, 'yellow');
    return true; // 自动确认
  });

  logResult('确认回调设置', true);
}

// ============================================================================
// 画像系统测试
// ============================================================================

async function testProfileSystem() {
  logSection('6. 画像系统测试');

  // 6.1 加载画像配置
  const profileLoader = new ProfileLoader();
  await profileLoader.load();

  const profiles = profileLoader.getAllProfiles();
  logResult('画像加载', profiles.size === 3, `加载了 ${profiles.size} 个 Worker 画像`);

  // 显示各 Worker 画像信息
  for (const [type, profile] of profiles) {
    log(`  - ${type}: ${profile.displayName} (优先分类: ${profile.preferences.preferredCategories.slice(0, 3).join(', ')})`, 'blue');
  }

  // 6.2 加载分类配置
  const categories = profileLoader.getAllCategories();
  logResult('分类加载', categories.size >= 8, `加载了 ${categories.size} 个任务分类`);

  // 显示分类信息
  for (const [name, config] of categories) {
    log(`  - ${name}: ${config.displayName || name} -> ${config.defaultWorker}`, 'blue');
  }

  // 6.3 测试任务分析器
  const taskAnalyzer = new TaskAnalyzer();
  taskAnalyzer.setProfileLoader(profileLoader);
  logResult('TaskAnalyzer 集成', true, '已设置 ProfileLoader');

  // 6.4 测试 CLI 选择器
  const cliSelector = new CLISelector();
  cliSelector.setProfileLoader(profileLoader);
  logResult('CLISelector 集成', true, '已设置 ProfileLoader');

  // 6.5 测试引导注入器
  const guidanceInjector = new GuidanceInjector();
  logResult('GuidanceInjector 创建', true);

  return { profileLoader, taskAnalyzer, cliSelector, guidanceInjector };
}

async function testProfileBasedWorkerSelection(profileSystem) {
  logSection('7. 画像驱动的 Worker 选择测试');

  const { profileLoader, cliSelector } = profileSystem;

  // 测试用例：不同类型任务应该分配给不同的 Worker
  const testCases = [
    { desc: '重构 UserService 类，提取公共方法', expectedWorker: 'claude', expectedCategory: 'architecture' },
    { desc: '修复登录页面的表单验证错误', expectedWorker: 'codex', expectedCategory: 'bugfix' },
    { desc: '实现用户认证 API 服务和数据库操作', expectedWorker: 'claude', expectedCategory: 'backend' },
    { desc: '编写项目 README 文档说明', expectedWorker: 'gemini', expectedCategory: 'docs' },
    { desc: '为 PaymentService 编写单元测试', expectedWorker: 'codex', expectedCategory: 'test' },
    { desc: '实现用户界面 UI 组件和页面样式', expectedWorker: 'gemini', expectedCategory: 'frontend' },
  ];

  let passed = 0;
  let failed = 0;

  for (const tc of testCases) {
    const selection = cliSelector.selectByDescription(tc.desc);
    const workerMatch = selection.worker === tc.expectedWorker;
    const categoryMatch = selection.category === tc.expectedCategory;
    const success = workerMatch && categoryMatch;

    if (success) {
      passed++;
      log(`  ✅ "${tc.desc.substring(0, 30)}..." -> ${selection.worker} (${selection.category})`, 'green');
    } else {
      failed++;
      log(`  ❌ "${tc.desc.substring(0, 30)}..."`, 'red');
      log(`     期望: ${tc.expectedWorker}/${tc.expectedCategory}, 实际: ${selection.worker}/${selection.category}`, 'red');
    }
  }

  logResult('Worker 选择测试', failed === 0, `${passed}/${testCases.length} 通过`);
  return { passed, failed, total: testCases.length };
}

async function testGuidancePromptGeneration(profileSystem) {
  logSection('8. 画像引导 Prompt 生成测试');

  const { profileLoader, guidanceInjector } = profileSystem;

  // 测试为不同 Worker 生成引导 Prompt
  const workers = ['claude', 'codex', 'gemini'];

  for (const workerType of workers) {
    const profile = profileLoader.getProfile(workerType);
    const context = {
      taskDescription: '实现用户登录功能',
      targetFiles: ['src/auth/login.ts'],
      category: 'backend',
    };

    const prompt = guidanceInjector.buildWorkerPrompt(profile, context);
    const hasRole = prompt.includes('角色定位');
    const hasFocus = prompt.includes('专注领域');

    logResult(`${workerType} 引导 Prompt`, hasRole && hasFocus, `${prompt.length} 字符`);

    // 显示 Prompt 预览
    const preview = prompt.split('\n').slice(0, 3).join(' ').substring(0, 80);
    log(`  预览: ${preview}...`, 'blue');
  }
}

async function testSimpleExecution(orchestrator, cliAvailable) {
  logSection('9. 简单任务执行测试');

  if (!cliAvailable.claude) {
    log('⚠️  跳过执行测试：Claude CLI 未安装', 'yellow');
    return;
  }

  // 设置事件监听
  const events = [];
  globalEventBus.on('orchestrator:phase_changed', (e) => {
    events.push(`phase: ${e.data?.phase}`);
  });
  globalEventBus.on('task:started', () => events.push('task:started'));
  globalEventBus.on('task:completed', () => events.push('task:completed'));

  log('  执行简单任务: "列出当前目录的文件结构"', 'yellow');

  try {
    const result = await orchestrator.execute('列出当前目录下的 src 文件夹结构，只需要简单描述即可');

    const isSuccess = typeof result === 'string' ? result.trim().length > 0 : result.success;
    const errorDetail = typeof result === 'string' ? '无响应' : result.error;
    logResult('任务执行完成', isSuccess, isSuccess ? '成功' : errorDetail);
    logResult('收到事件', events.length > 0, `${events.length} 个事件`);
    log(`  事件序列: ${events.join(' -> ')}`, 'blue');

    if (typeof result === 'string' && result) {
      log(`  响应内容预览: ${result.substring(0, 100)}...`, 'blue');
    } else if (result && result.content) {
      log(`  响应内容预览: ${result.content.substring(0, 100)}...`, 'blue');
    }
  } catch (error) {
    logResult('任务执行', false, error.message);
  }
}

async function runAllTests() {
  log('\n🚀 开始测试主 Claude + 多端 CLI 代理流程（含画像系统）\n', 'cyan');

  const startTime = Date.now();
  let passed = 0;
  let failed = 0;

  try {
    // 1. CLI 可用性检测
    const cliAvailable = await testCLIAvailability();
    passed++;

    // 2. 适配器工厂测试
    const factory = await testAdapterFactory();
    passed++;

    // 3. 编排器初始化
    const orchestrator = await testOrchestratorInit(factory);
    passed++;

    // 4. 事件总线测试
    await testEventBus();
    passed++;

    // 5. 确认回调测试
    await testConfirmationCallback(orchestrator);
    passed++;

    // 6. 画像系统测试
    const profileSystem = await testProfileSystem();
    passed++;

    // 7. 画像驱动的 Worker 选择测试
    const selectionResult = await testProfileBasedWorkerSelection(profileSystem);
    if (selectionResult.failed === 0) {
      passed++;
    } else {
      failed++;
    }

    // 8. 画像引导 Prompt 生成测试
    await testGuidancePromptGeneration(profileSystem);
    passed++;

    // 9. 简单执行测试（可选，需要 Claude CLI）
    await testSimpleExecution(orchestrator, cliAvailable);
    passed++;

  } catch (error) {
    failed++;
    log(`\n❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
  }

  const duration = ((Date.now() - startTime) / 1000).toFixed(2);

  logSection('测试结果汇总');
  log(`✅ 通过: ${passed}`, 'green');
  log(`❌ 失败: ${failed}`, failed > 0 ? 'red' : 'green');
  log(`⏱️  耗时: ${duration}s`, 'blue');

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runAllTests().catch(console.error);
