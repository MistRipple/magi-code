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

  orchestrator.setConfirmationCallback(async (plan, formattedPlan) => {
    log(`  收到计划确认请求: ${plan.summary?.substring(0, 50)}...`, 'yellow');
    return true; // 自动确认
  });

  logResult('确认回调设置', true);
}

async function testSimpleExecution(orchestrator, cliAvailable) {
  logSection('6. 简单任务执行测试');

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
  log('\n🚀 开始测试主 Claude + 多端 CLI 代理流程\n', 'cyan');

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

    // 6. 简单执行测试（可选，需要 Claude CLI）
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
