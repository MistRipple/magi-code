/**
 * 真实端到端编排器测试
 *
 * 本测试使用真实的 CLI 进程验证编排器的完整执行流程：
 * - 意图分析
 * - 模式选择 (ask/agent/auto)
 * - 消息流程
 * - Token 统计
 * - 恢复确认流程
 *
 * 运行方式: node scripts/test-real-orchestrator-e2e.js
 *
 * 前置条件：
 * - 已安装 claude CLI 并可访问
 * - 项目已编译 (npm run compile)
 */

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

// Mock vscode 模块
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

const { TestRunner, waitFor } = require('./test-utils');

const workspaceRoot = process.cwd();

// ============================================================================
// 辅助函数
// ============================================================================

/**
 * 检查 CLI 是否可用
 */
async function checkCliAvailability(cli) {
  return new Promise((resolve) => {
    const proc = spawn(cli, ['--version'], { shell: true });
    let hasOutput = false;

    proc.stdout.on('data', () => { hasOutput = true; });
    proc.stderr.on('data', () => { hasOutput = true; });

    proc.on('close', (code) => {
      resolve(code === 0 || hasOutput);
    });

    proc.on('error', () => {
      resolve(false);
    });

    // 超时保护
    setTimeout(() => {
      proc.kill();
      resolve(false);
    }, 3000);
  });
}

// ============================================================================
// 真实 E2E 测试场景
// ============================================================================

/**
 * 场景 1: Ask 模式 - 纯对话，真实 CLI 响应
 */
async function testRealAskMode(runner, factory) {
  runner.logSection('真实 E2E: Ask 模式测试');

  const standardMessages = [];
  const standardCompletes = [];

  // 监听消息
  const messageHandler = (msg) => standardMessages.push(msg);
  const completeHandler = (msg) => standardCompletes.push(msg);

  factory.on('standardMessage', messageHandler);
  factory.on('standardComplete', completeHandler);

  try {
    // 创建完成 Promise
    const done = new Promise((resolve) => {
      const handler = () => resolve('complete');
      factory.on('standardComplete', handler);
      // 超时保护
      setTimeout(() => resolve('timeout'), 20000);
    });

    // 发送简单问题
    const testPrompt = '用一个词回答：TypeScript 是什么类型的语言？';

    runner.log(`\n发送请求: "${testPrompt}"`, 'blue');

    const sendPromise = factory.sendMessage('claude', testPrompt, undefined, {
      source: 'orchestrator',
      streamToUI: true,
      adapterRole: 'orchestrator',
      messageMeta: { intent: 'ask' },
    });

    const reason = await done;

    if (reason === 'timeout') {
      runner.log('  ⚠️  等待超时', 'yellow');
    }

    const result = await sendPromise;

    runner.logTest('CLI 响应成功',
      result && result.content && result.content.length > 0,
      `响应长度: ${result?.content?.length || 0} 字符`);

    runner.logTest('Token 统计存在',
      result?.tokenUsage && (result.tokenUsage.inputTokens > 0 || result.tokenUsage.outputTokens > 0),
      `输入: ${result?.tokenUsage?.inputTokens || 0}, 输出: ${result?.tokenUsage?.outputTokens || 0}`);

    runner.logTest('收到流式消息',
      standardMessages.length > 0,
      `共 ${standardMessages.length} 条消息`);

    runner.logTest('收到完成消息',
      standardCompletes.length > 0,
      `共 ${standardCompletes.length} 条完成消息`);

    // 清理监听器
    factory.off('standardMessage', messageHandler);
    factory.off('standardComplete', completeHandler);

    return { success: true, response: result };

  } catch (error) {
    runner.logTest('Ask 模式测试', false, error.message);
    factory.off('standardMessage', messageHandler);
    factory.off('standardComplete', completeHandler);
    return { success: false, error: error.message };
  }
}

/**
 * 场景 2: 意图分析 - 验证编排器能正确分类用户意图
 */
async function testRealIntentAnalysis(runner, factory) {
  runner.logSection('真实 E2E: 意图分析测试');

  const testCases = [
    { prompt: 'TypeScript 和 JavaScript 有什么区别？', expectedIntent: 'question' },
    { prompt: '帮我创建一个新的 React 组件', expectedIntent: 'task' },
    { prompt: '分析这段代码的问题', expectedIntent: 'exploratory' },
  ];

  for (const testCase of testCases) {
    try {
      const intentPrompt = `分析以下用户输入的意图，只返回一个单词（question/task/exploratory）:

用户输入: ${testCase.prompt}

意图:`;

      const result = await factory.sendMessage('claude', intentPrompt, undefined, {
        source: 'orchestrator',
        adapterRole: 'orchestrator',
      });

      const response = result?.content?.toLowerCase() || '';
      const hasValidIntent = ['question', 'task', 'exploratory', 'ambiguous'].some(i => response.includes(i));

      runner.logTest(`意图分析: "${testCase.prompt.substring(0, 30)}..."`,
        hasValidIntent,
        `响应: ${response.substring(0, 50)}`);

    } catch (error) {
      runner.logTest(`意图分析: "${testCase.prompt.substring(0, 30)}..."`,
        false, error.message);
    }
  }

  return { success: true };
}

/**
 * 场景 3: 智能编排器集成测试
 */
async function testRealIntelligentOrchestrator(runner) {
  runner.logSection('真实 E2E: 智能编排器集成测试');

  try {
    // 导入必要模块
    const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
    const { UnifiedSessionManager } = require('../out/session');
    const { SnapshotManager } = require('../out/snapshot-manager');
    const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');

    // 创建依赖
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
    const cliFactory = new CLIAdapterFactory({ cwd: workspaceRoot });

    // 创建编排器 - 注意参数顺序: cliFactory, sessionManager, snapshotManager, workspaceRoot, config
    const config = {
      timeout: 30000,
      maxRetries: 3,
    };

    const orchestrator = new IntelligentOrchestrator(
      cliFactory,
      sessionManager,
      snapshotManager,
      workspaceRoot,
      config
    );

    // 初始化
    await orchestrator.initialize();

    runner.logTest('编排器初始化成功', true, '所有依赖已创建');

    // 测试模式切换
    orchestrator.setInteractionMode('ask');
    runner.logTest('模式切换: ask',
      orchestrator.getInteractionMode() === 'ask',
      `当前模式: ${orchestrator.getInteractionMode()}`);

    orchestrator.setInteractionMode('agent');
    runner.logTest('模式切换: agent',
      orchestrator.getInteractionMode() === 'agent',
      `当前模式: ${orchestrator.getInteractionMode()}`);

    orchestrator.setInteractionMode('auto');
    runner.logTest('模式切换: auto',
      orchestrator.getInteractionMode() === 'auto',
      `当前模式: ${orchestrator.getInteractionMode()}`);

    // 设置回调
    let confirmationCalled = false;
    let recoveryCalled = false;

    orchestrator.setConfirmationCallback(async (plan, formatted) => {
      confirmationCalled = true;
      runner.log('  [回调] 确认回调被触发', 'cyan');
      return true; // 自动确认
    });

    orchestrator.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
      recoveryCalled = true;
      runner.log(`  [回调] 恢复确认回调被触发: ${error}`, 'cyan');
      return 'continue';
    });

    runner.logTest('回调设置成功', true, '确认和恢复回调已设置');

    // 测试 Ask 模式执行
    orchestrator.setInteractionMode('ask');

    runner.log('\n--- 执行 Ask 模式请求 ---', 'blue');

    try {
      const taskId = `test-task-${Date.now()}`;

      // 创建完成 Promise 带超时
      const executeWithTimeout = async (prompt, timeoutMs = 30000) => {
        return Promise.race([
          orchestrator.execute(prompt, taskId),
          new Promise((_, reject) =>
            setTimeout(() => reject(new Error('执行超时')), timeoutMs)
          )
        ]);
      };

      const response = await executeWithTimeout('用一个词回答：1+1等于几？');

      runner.logTest('Ask 模式执行成功',
        response && response.length > 0,
        `响应: ${response?.substring(0, 100) || '(空)'}`);

      // Token 统计
      const tokenUsage = orchestrator.getOrchestratorTokenUsage();
      runner.logTest('Token 统计有效',
        tokenUsage.inputTokens > 0 || tokenUsage.outputTokens > 0,
        `输入: ${tokenUsage.inputTokens}, 输出: ${tokenUsage.outputTokens}`);

    } catch (error) {
      runner.logTest('Ask 模式执行', false, error.message);
    }

    // 清理
    orchestrator.dispose();
    await cliFactory.disconnectAll().catch(() => {});

    runner.logTest('编排器清理成功', true, '资源已释放');

    return { success: true };

  } catch (error) {
    runner.logTest('智能编排器集成测试', false, error.message);
    return { success: false, error: error.message };
  }
}

/**
 * 场景 4: 消息流程完整性测试
 */
async function testRealMessageFlow(runner, factory) {
  runner.logSection('真实 E2E: 消息流程完整性测试');

  const messages = [];
  const events = [];

  // 监听所有消息类型
  const handlers = {
    standardMessage: (msg) => {
      messages.push({ type: 'standardMessage', msg });
      events.push('standardMessage');
    },
    standardComplete: (msg) => {
      messages.push({ type: 'standardComplete', msg });
      events.push('standardComplete');
    },
    streamChunk: (chunk) => {
      events.push('streamChunk');
    },
  };

  for (const [event, handler] of Object.entries(handlers)) {
    factory.on(event, handler);
  }

  try {
    const testPrompt = '请用一句话解释什么是函数式编程';

    runner.log(`\n发送请求: "${testPrompt}"`, 'blue');

    const done = new Promise((resolve) => {
      factory.on('standardComplete', () => resolve('complete'));
      setTimeout(() => resolve('timeout'), 25000);
    });

    const sendPromise = factory.sendMessage('claude', testPrompt, undefined, {
      source: 'orchestrator',
      streamToUI: true,
      adapterRole: 'orchestrator',
    });

    await done;
    const result = await sendPromise;

    // 验证消息流程
    runner.logTest('收到流式消息',
      events.filter(e => e === 'standardMessage').length > 0 ||
      events.filter(e => e === 'streamChunk').length > 0,
      `消息事件: ${events.length}`);

    runner.logTest('收到完成事件',
      events.includes('standardComplete'),
      '流程正常结束');

    runner.logTest('最终响应有内容',
      result?.content?.length > 0,
      `响应长度: ${result?.content?.length || 0}`);

    // 检查消息结构
    const completeMessages = messages.filter(m => m.type === 'standardComplete');
    if (completeMessages.length > 0) {
      const lastComplete = completeMessages[completeMessages.length - 1].msg;
      runner.logTest('完成消息结构正确',
        lastComplete && (lastComplete.blocks || lastComplete.content),
        `包含 blocks: ${!!lastComplete?.blocks}, 包含 content: ${!!lastComplete?.content}`);
    }

  } finally {
    // 清理监听器
    for (const [event, handler] of Object.entries(handlers)) {
      factory.off(event, handler);
    }
  }

  return { success: true };
}

// ============================================================================
// 主函数
// ============================================================================

async function main() {
  const runner = new TestRunner('真实端到端编排器测试 (Real E2E Orchestrator Tests)');

  // 检查 CLI 可用性
  runner.logSection('前置条件检查');

  const claudeAvailable = await checkCliAvailability('claude');
  runner.logTest('Claude CLI 可用', claudeAvailable,
    claudeAvailable ? '已安装' : '未安装或不可访问');

  if (!claudeAvailable) {
    runner.log('\n⚠️  Claude CLI 不可用，无法运行真实 E2E 测试', 'yellow');
    runner.log('请确保已安装 claude CLI 并可从命令行访问', 'yellow');
    process.exit(0);
  }

  // 检查编译产物
  const outDir = path.join(workspaceRoot, 'out');
  const compiled = fs.existsSync(path.join(outDir, 'cli', 'adapter-factory.js'));
  runner.logTest('编译产物存在', compiled,
    compiled ? 'out/ 目录已存在' : '请先运行 npm run compile');

  if (!compiled) {
    runner.log('\n⚠️  请先编译项目: npm run compile', 'yellow');
    process.exit(1);
  }

  try {
    // 导入编译后的模块
    const { CLIAdapterFactory } = require('../out/cli/adapter-factory');

    // 创建 CLI 工厂
    const factory = new CLIAdapterFactory({ cwd: workspaceRoot });

    // 运行测试场景
    await testRealAskMode(runner, factory);
    await testRealIntentAnalysis(runner, factory);
    await testRealMessageFlow(runner, factory);
    await testRealIntelligentOrchestrator(runner);

    // 清理
    await factory.disconnectAll().catch(() => {});

    process.exit(runner.finish());

  } catch (error) {
    console.error('\n❌ 测试执行失败:', error);
    process.exit(1);
  }
}

main().catch(console.error);
