/**
 * 架构优化综合测试脚本
 *
 * 测试覆盖:
 * - 阶段 1: PolicyEngine 与 ProfileLoader 集成
 * - 阶段 2: ConflictResolver 冲突解决机制
 * - 阶段 3: TaskDependencyGraph 文件依赖调度
 * - 阶段 4: MessageDeduplicator 消息去重
 */

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

const { PolicyEngine } = require('../out/orchestrator/policy-engine');
const { ProfileLoader } = require('../out/orchestrator/profile');
const { CLISelector } = require('../out/task/cli-selector');
const { ConflictResolver } = require('../out/task/conflict-resolver');
const { TaskDependencyGraph } = require('../out/orchestrator/task-dependency-graph');
const { MessageDeduplicator } = require('../out/normalizer/message-deduplicator');
const { MessageLifecycle, MessageType } = require('../out/protocol/message-protocol');
const { TestRunner } = require('./test-utils');

const workspaceRoot = process.cwd();

// ============================================================================
// 阶段 1 测试: PolicyEngine 与 ProfileLoader 集成
// ============================================================================

async function testPhase1_PolicyEngineIntegration(runner) {
  runner.logSection('阶段 1: PolicyEngine 与 ProfileLoader 集成测试');

  const profileLoader = new ProfileLoader(workspaceRoot);
  await profileLoader.load();

  // 测试 1.1: PolicyEngine 使用 ProfileLoader 而非硬编码
  try {
    const policyEngine = new PolicyEngine(profileLoader);

    // 验证可以获取任务类型的首选 CLI
    const subTasks = [
      { id: 'task-1', description: '重构用户认证模块', category: 'refactor', assignedWorker: undefined },
      { id: 'task-2', description: '修复登录bug', category: 'bugfix', assignedWorker: undefined },
    ];

    const strategy = policyEngine.decideExecutionStrategy(subTasks);

    runner.logTest(
      'PolicyEngine 使用 ProfileLoader 获取分类配置',
      strategy.parallel.length > 0 || strategy.serial.length > 0,
      `并行任务: ${strategy.parallel.length}, 串行任务: ${strategy.serial.length}`
    );
  } catch (error) {
    runner.logTest('PolicyEngine 集成 ProfileLoader', false, error.message);
  }

  // 测试 1.2: PolicyEngine 实例化而非全局单例
  try {
    const engine1 = new PolicyEngine(profileLoader);
    const engine2 = new PolicyEngine(profileLoader);

    runner.logTest(
      'PolicyEngine 支持多实例',
      engine1 !== engine2,
      '两个实例应该独立存在'
    );
  } catch (error) {
    runner.logTest('PolicyEngine 多实例', false, error.message);
  }

  // 测试 1.3: 从 ProfileLoader 读取分类关键词
  try {
    const categoryConfig = profileLoader.getCategory('architecture');

    runner.logTest(
      'ProfileLoader 提供分类配置',
      categoryConfig && categoryConfig.keywords && categoryConfig.keywords.length > 0,
      `architecture 分类有 ${categoryConfig?.keywords?.length || 0} 个关键词`
    );
  } catch (error) {
    runner.logTest('ProfileLoader 分类配置', false, error.message);
  }
}

// ============================================================================
// 阶段 2 测试: ConflictResolver 冲突解决机制
// ============================================================================

function testPhase2_ConflictResolution(runner) {
  runner.logSection('阶段 2: ConflictResolver 冲突解决机制测试');

  // 测试 2.1: 用户偏好优先级
  try {
    const resolver = new ConflictResolver({ userPreference: 'always-respect' });

    const result = resolver.resolve({
      userPreference: 'gemini',
      profileRecommendation: 'claude',
      statsRecommendation: 'codex',
      availableClis: ['claude', 'codex', 'gemini'],
    });

    runner.logTest(
      'ConflictResolver 用户偏好优先 (Level 1)',
      result.cli === 'gemini' && result.level === 'user',
      `选择结果: ${result.cli}, 决策层级: ${result.level}, 原因: ${result.reason}`
    );
  } catch (error) {
    runner.logTest('用户偏好优先级', false, error.message);
  }

  // 测试 2.2: 统计推荐优先级 (无用户偏好时)
  try {
    // 创建 Mock ExecutionStats
    const mockStats = {
      getStats: (cli) => ({
        totalExecutions: 10,
        successfulExecutions: 9,
        failedExecutions: 1,
        successRate: 0.9,
        averageExecutionTime: 1000,
        isHealthy: true,
      }),
    };

    const resolver = new ConflictResolver(
      { statsVsProfile: 'prefer-stats-if-healthy' },
      mockStats
    );

    const result = resolver.resolve({
      profileRecommendation: 'claude',
      statsRecommendation: 'codex',
      availableClis: ['claude', 'codex', 'gemini'],
    });

    runner.logTest(
      'ConflictResolver 统计推荐优先 (Level 2)',
      result.cli === 'codex' && result.level === 'stats',
      `选择结果: ${result.cli}, 决策层级: ${result.level}`
    );
  } catch (error) {
    runner.logTest('统计推荐优先级', false, error.message);
  }

  // 测试 2.3: 画像推荐优先级 (无用户偏好和统计时)
  try {
    const resolver = new ConflictResolver();

    const result = resolver.resolve({
      profileRecommendation: 'claude',
      availableClis: ['claude', 'codex', 'gemini'],
    });

    runner.logTest(
      'ConflictResolver 画像推荐优先 (Level 3)',
      result.cli === 'claude' && result.level === 'profile',
      `选择结果: ${result.cli}, 决策层级: ${result.level}`
    );
  } catch (error) {
    runner.logTest('画像推荐优先级', false, error.message);
  }

  // 测试 2.4: 默认值回退 (Level 4)
  try {
    const resolver = new ConflictResolver();

    const result = resolver.resolve({
      availableClis: ['claude'],
    });

    runner.logTest(
      'ConflictResolver 默认值回退 (Level 4)',
      result.cli === 'claude' && result.level === 'default',
      `选择结果: ${result.cli}, 决策层级: ${result.level}`
    );
  } catch (error) {
    runner.logTest('默认值回退', false, error.message);
  }

  // 测试 2.5: CLISelector 集成 ConflictResolver
  try {
    const cliSelector = new CLISelector();
    cliSelector.setAvailableCLIs(['claude', 'codex', 'gemini']);

    const selection = cliSelector.select(
      { category: 'architecture', complexity: 'high' },
      'gemini' // 用户偏好
    );

    runner.logTest(
      'CLISelector 集成 ConflictResolver',
      selection.cli === 'gemini',
      `用户指定 gemini, 实际选择: ${selection.cli}, 原因: ${selection.reason}`
    );
  } catch (error) {
    runner.logTest('CLISelector 集成', false, error.message);
  }
}

// ============================================================================
// 阶段 3 测试: TaskDependencyGraph 文件依赖调度
// ============================================================================

function testPhase3_FileDependencyScheduling(runner) {
  runner.logSection('阶段 3: TaskDependencyGraph 文件依赖调度测试');

  // 测试 3.1: 添加带文件的任务节点
  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-1', '修改用户服务', {}, ['src/user/service.ts']);
    graph.addTask('task-2', '修改用户控制器', {}, ['src/user/controller.ts']);
    graph.addTask('task-3', '修改用户服务配置', {}, ['src/user/service.ts']); // 文件冲突!

    const task1 = graph.getTask('task-1');

    runner.logTest(
      'TaskDependencyGraph 支持 targetFiles',
      task1 && task1.targetFiles && task1.targetFiles.length === 1,
      `task-1 目标文件: ${task1?.targetFiles?.join(', ')}`
    );
  } catch (error) {
    runner.logTest('支持 targetFiles', false, error.message);
  }

  // 测试 3.2: 检测文件冲突
  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-1', '修改 A', {}, ['src/file.ts']);
    graph.addTask('task-2', '修改 B', {}, ['src/other.ts']);
    graph.addTask('task-3', '修改 C', {}, ['src/file.ts']); // 冲突

    const conflicts = graph.detectFileConflicts();

    runner.logTest(
      'TaskDependencyGraph 检测文件冲突',
      conflicts.length === 1 && conflicts[0].file === 'src/file.ts',
      `检测到 ${conflicts.length} 个冲突: ${conflicts.map(c => c.file).join(', ')}`
    );
  } catch (error) {
    runner.logTest('检测文件冲突', false, error.message);
  }

  // 测试 3.3: 自动添加文件依赖 (sequential 策略)
  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-1', '修改 A', {}, ['src/file.ts']);
    graph.addTask('task-3', '修改 C', {}, ['src/file.ts']);
    graph.addTask('task-2', '修改 B', {}, ['src/file.ts']);

    const addedCount = graph.addFileDependencies('sequential');

    // 应该添加 2 个依赖: task-2 -> task-1, task-3 -> task-2 (按 ID 排序)
    runner.logTest(
      'TaskDependencyGraph 自动添加文件依赖',
      addedCount === 2,
      `添加了 ${addedCount} 个依赖关系`
    );
  } catch (error) {
    runner.logTest('自动添加文件依赖', false, error.message);
  }

  // 测试 3.4: 分析包含文件冲突信息
  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-1', '任务 A', {}, ['src/a.ts']);
    graph.addTask('task-2', '任务 B', {}, ['src/a.ts', 'src/b.ts']);
    graph.addTask('task-3', '任务 C', {}, ['src/c.ts']);

    const analysis = graph.analyze();

    runner.logTest(
      'DependencyAnalysis 包含文件冲突信息',
      analysis.fileConflicts && analysis.fileConflicts.length > 0,
      `文件冲突: ${analysis.fileConflicts?.length || 0} 个`
    );
  } catch (error) {
    runner.logTest('分析包含文件冲突', false, error.message);
  }

  // 测试 3.5: 获取文件的所有相关任务
  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-1', '任务 A', {}, ['src/shared.ts']);
    graph.addTask('task-2', '任务 B', {}, ['src/shared.ts']);
    graph.addTask('task-3', '任务 C', {}, ['src/other.ts']);

    const tasks = graph.getTasksByFile('src/shared.ts');

    runner.logTest(
      'TaskDependencyGraph 获取文件相关任务',
      tasks.length === 2 && tasks.includes('task-1') && tasks.includes('task-2'),
      `src/shared.ts 相关任务: ${tasks.join(', ')}`
    );
  } catch (error) {
    runner.logTest('获取文件相关任务', false, error.message);
  }
}

// ============================================================================
// 阶段 4 测试: MessageDeduplicator 消息去重
// ============================================================================

async function testPhase4_MessageDeduplication(runner) {
  runner.logSection('阶段 4: MessageDeduplicator 消息去重测试');

  // 辅助函数: 创建测试消息
  function createMessage(id, lifecycle, source = 'worker') {
    return {
      id,
      traceId: 'test-trace',
      type: MessageType.TEXT,
      source,
      cli: 'claude',
      lifecycle,
      blocks: [],
      metadata: { priority: 0, tags: [] },
      timestamp: Date.now(),
      updatedAt: Date.now(),
    };
  }

  // 测试 4.1: STARTED 消息总是发送
  try {
    const deduplicator = new MessageDeduplicator({ enabled: true });

    const msg1 = createMessage('msg-1', MessageLifecycle.STARTED);
    const msg2 = createMessage('msg-1', MessageLifecycle.STARTED); // 重复 STARTED

    const shouldSend1 = deduplicator.shouldSend(msg1);
    const shouldSend2 = deduplicator.shouldSend(msg2);

    runner.logTest(
      'MessageDeduplicator STARTED 消息总是发送',
      shouldSend1 === true && shouldSend2 === true,
      `第一次: ${shouldSend1}, 第二次: ${shouldSend2}`
    );
  } catch (error) {
    runner.logTest('STARTED 消息总是发送', false, error.message);
  }

  // 测试 4.2: 已完成消息不再发送
  try {
    const deduplicator = new MessageDeduplicator({ enabled: true });

    const msg1 = createMessage('msg-2', MessageLifecycle.STARTED);
    const msg2 = createMessage('msg-2', MessageLifecycle.COMPLETED);
    const msg3 = createMessage('msg-2', MessageLifecycle.STREAMING);

    deduplicator.shouldSend(msg1); // STARTED
    deduplicator.shouldSend(msg2); // COMPLETED
    const shouldSend3 = deduplicator.shouldSend(msg3); // 尝试在完成后发送

    runner.logTest(
      'MessageDeduplicator 完成后消息不再发送',
      shouldSend3 === false,
      '完成后的 STREAMING 消息应该被跳过'
    );
  } catch (error) {
    runner.logTest('完成后消息不再发送', false, error.message);
  }

  // 测试 4.3: STREAMING 消息限制发送间隔
  try {
    const deduplicator = new MessageDeduplicator({
      enabled: true,
      minStreamInterval: 100, // 100ms 间隔
    });

    const msg1 = createMessage('msg-3', MessageLifecycle.STARTED);
    const msg2 = createMessage('msg-3', MessageLifecycle.STREAMING);
    const msg3 = createMessage('msg-3', MessageLifecycle.STREAMING);

    const shouldSend1 = deduplicator.shouldSend(msg1); // STARTED - 总是发送
    const shouldSend2 = deduplicator.shouldSend(msg2); // 第一次 STREAMING - 应该发送
    const shouldSend3 = deduplicator.shouldSend(msg3); // 立即发送第二次 - 应该被拒绝

    runner.logTest(
      'MessageDeduplicator STREAMING 消息限制间隔',
      shouldSend1 === true && shouldSend2 === true && shouldSend3 === false,
      `STARTED: ${shouldSend1}, 第一次 STREAMING: ${shouldSend2}, 立即第二次: ${shouldSend3}`
    );

    // 等待 100ms 后应该可以发送
    await new Promise(resolve => setTimeout(resolve, 110));
    const msg4 = createMessage('msg-3', MessageLifecycle.STREAMING);
    const shouldSend4 = deduplicator.shouldSend(msg4);

    runner.logTest(
      'MessageDeduplicator 等待后可发送 STREAMING',
      shouldSend4 === true,
      '等待 100ms 后应该允许发送'
    );
  } catch (error) {
    runner.logTest('STREAMING 消息限制间隔', false, error.message);
  }

  // 测试 4.4: 不同 source 消息隔离
  try {
    const deduplicator = new MessageDeduplicator({ enabled: true });

    const msg1 = createMessage('msg-4', MessageLifecycle.STARTED, 'orchestrator');
    const msg2 = createMessage('msg-4', MessageLifecycle.STARTED, 'worker');

    const shouldSend1 = deduplicator.shouldSend(msg1);
    const shouldSend2 = deduplicator.shouldSend(msg2);

    runner.logTest(
      'MessageDeduplicator 不同 source 消息隔离',
      shouldSend1 === true && shouldSend2 === true,
      'orchestrator 和 worker 的相同 ID 消息应该独立'
    );
  } catch (error) {
    runner.logTest('不同 source 消息隔离', false, error.message);
  }

  // 测试 4.5: 按 source 获取消息
  try {
    const deduplicator = new MessageDeduplicator({ enabled: true });

    deduplicator.shouldSend(createMessage('msg-5', MessageLifecycle.STARTED, 'orchestrator'));
    deduplicator.shouldSend(createMessage('msg-6', MessageLifecycle.STARTED, 'worker'));
    deduplicator.shouldSend(createMessage('msg-7', MessageLifecycle.STARTED, 'orchestrator'));

    const orchestratorMessages = deduplicator.getMessagesBySource('orchestrator');
    const workerMessages = deduplicator.getMessagesBySource('worker');

    runner.logTest(
      'MessageDeduplicator 按 source 获取消息',
      orchestratorMessages.length === 2 && workerMessages.length === 1,
      `orchestrator: ${orchestratorMessages.length}, worker: ${workerMessages.length}`
    );
  } catch (error) {
    runner.logTest('按 source 获取消息', false, error.message);
  }

  // 测试 4.6: 统计信息
  try {
    const deduplicator = new MessageDeduplicator({ enabled: true });

    deduplicator.shouldSend(createMessage('msg-8', MessageLifecycle.STARTED));
    deduplicator.shouldSend(createMessage('msg-8', MessageLifecycle.COMPLETED));
    deduplicator.shouldSend(createMessage('msg-9', MessageLifecycle.STARTED));

    const stats = deduplicator.getStats();

    runner.logTest(
      'MessageDeduplicator 统计信息',
      stats.totalMessages === 2 && stats.completedMessages === 1,
      `总消息: ${stats.totalMessages}, 已完成: ${stats.completedMessages}`
    );
  } catch (error) {
    runner.logTest('统计信息', false, error.message);
  }

  // 测试 4.7: 禁用去重
  try {
    const deduplicator = new MessageDeduplicator({ enabled: false });

    const msg1 = createMessage('msg-10', MessageLifecycle.STARTED);
    const msg2 = createMessage('msg-10', MessageLifecycle.COMPLETED);
    const msg3 = createMessage('msg-10', MessageLifecycle.STREAMING);

    const shouldSend1 = deduplicator.shouldSend(msg1);
    const shouldSend2 = deduplicator.shouldSend(msg2);
    const shouldSend3 = deduplicator.shouldSend(msg3);

    runner.logTest(
      'MessageDeduplicator 禁用时总是发送',
      shouldSend1 && shouldSend2 && shouldSend3,
      '禁用去重后所有消息都应该发送'
    );
  } catch (error) {
    runner.logTest('禁用去重', false, error.message);
  }
}

// ============================================================================
// 主测试流程
// ============================================================================

async function runAllTests() {
  const runner = new TestRunner('架构优化综合测试');

  try {
    await testPhase1_PolicyEngineIntegration(runner);
    testPhase2_ConflictResolution(runner);
    testPhase3_FileDependencyScheduling(runner);
    await testPhase4_MessageDeduplication(runner);

    // 使用 TestRunner 的统一输出
    process.exit(runner.finish());

  } catch (error) {
    runner.log(`\n❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
    process.exit(1);
  }
}

// 运行测试
runAllTests().catch(error => {
  console.error('Fatal error:', error);
  process.exit(1);
});
