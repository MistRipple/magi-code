const path = require('path');
const fs = require('fs');
const Module = require('module');
const assert = (cond, msg) => {
  if (!cond) {
    throw new Error(msg);
  }
};

const vscodeMock = {
  languages: {
    getDiagnostics: () => [],
  },
  DiagnosticSeverity: {
    Error: 0,
    Warning: 1,
    Information: 2,
    Hint: 3,
  },
};

const originalModuleLoad = Module._load;
Module._load = function load(request, parent, isMain) {
  if (request === 'vscode') {
    return vscodeMock;
  }
  return originalModuleLoad(request, parent, isMain);
};

const ROOT = '/Users/xie/code/MultiCLI';
const TEST_ROOT = path.join(ROOT, 'TEST');

const { CLISelector } = require(path.join(ROOT, 'out/task/cli-selector.js'));
const { TaskAnalyzer } = require(path.join(ROOT, 'out/task/task-analyzer.js'));
const { TaskSplitter } = require(path.join(ROOT, 'out/task/task-splitter.js'));
const { ExecutionStats } = require(path.join(ROOT, 'out/orchestrator/execution-stats.js'));
const { ContextManager } = require(path.join(ROOT, 'out/context/context-manager.js'));
const { TaskDependencyGraph } = require(path.join(ROOT, 'out/orchestrator/task-dependency-graph.js'));
const { FileLockManager } = require(path.join(ROOT, 'out/orchestrator/file-lock-manager.js'));
const { INTERACTION_MODE_CONFIGS } = require(path.join(ROOT, 'out/types.js'));
const { EventEmitter, globalEventBus } = require(path.join(ROOT, 'out/events.js'));
const { MessageBus } = require(path.join(ROOT, 'out/orchestrator/message-bus.js'));
const { RiskPolicy } = require(path.join(ROOT, 'out/orchestrator/risk-policy.js'));
const { AITaskDecomposer } = require(path.join(ROOT, 'out/task/ai-task-decomposer.js'));
const { SessionManager } = require(path.join(ROOT, 'out/session-manager.js'));
const { SnapshotManager } = require(path.join(ROOT, 'out/snapshot-manager.js'));

const tests = [];
const test = (name, fn) => tests.push({ name, fn });

function recordExecutions(stats, cli, count, success) {
  for (let i = 0; i < count; i += 1) {
    stats.recordExecution({
      cli,
      taskId: `task-${cli}-${i}`,
      subTaskId: `sub-${cli}-${i}`,
      success,
      duration: 100 + i,
      error: success ? undefined : 'timeout',
      inputTokens: 10,
      outputTokens: 20,
    });
  }
}

// 1) 后端类别识别
test('TaskAnalyzer detects backend category', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('实现后端 API 接口，并提供数据库支持');
  assert(analysis.category === 'backend', `expected backend, got ${analysis.category}`);
});

// 2) backend 默认映射为 codex
test('CLISelector picks codex for backend by default', () => {
  const selector = new CLISelector();
  selector.setAvailableCLIs(['claude', 'codex', 'gemini']);
  const selection = selector.selectByCategory('backend');
  assert(selection.cli === 'codex', `expected codex, got ${selection.cli}`);
});

// 3) 前后端任务拆分
test('TaskSplitter splits fullstack tasks into frontend + backend', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('同时实现前端页面和后端 API 登录功能');
  const selector = new CLISelector();
  selector.setAvailableCLIs(['claude', 'codex', 'gemini']);
  const splitter = new TaskSplitter(selector);
  const result = splitter.split(analysis);
  assert(result.subTasks.length === 2, `expected 2 subTasks, got ${result.subTasks.length}`);
  const categories = result.subTasks.map(t => t.category);
  assert(categories.includes('backend'), 'missing backend subTask');
  assert(categories.includes('frontend'), 'missing frontend subTask');
});

// 4) Bug 修复场景识别
test('TaskAnalyzer detects bugfix category', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('修复 src/api/auth.ts 的登录错误');
  assert(analysis.category === 'bugfix', `expected bugfix, got ${analysis.category}`);
  assert(analysis.suggestedMode === 'sequential', 'single file bugfix should be sequential');
});

// 5) 重构场景可拆分
test('TaskAnalyzer marks refactor task as splittable', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('重构并且优化用户模块性能');
  assert(analysis.category === 'refactor', `expected refactor, got ${analysis.category}`);
  assert(analysis.splittable === true, 'refactor with multiple actions should be splittable');
});

// 6) 架构任务拆分为设计+实现
test('TaskSplitter splits architecture task into design + implement', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('进行系统架构设计并实现基础结构');
  const selector = new CLISelector();
  selector.setAvailableCLIs(['claude', 'codex', 'gemini']);
  const splitter = new TaskSplitter(selector);
  const result = splitter.split(analysis);
  assert(result.subTasks.length === 2, `expected 2 subTasks, got ${result.subTasks.length}`);
  const [designTask, implTask] = result.subTasks;
  assert(implTask.dependencies.includes(designTask.id), 'implementation should depend on design');
});

// 7) 并行模式建议
test('TaskAnalyzer suggests parallel mode for multi-file tasks', () => {
  const analyzer = new TaskAnalyzer();
  const analysis = analyzer.analyze('同时修改 src/a.ts 和 src/b.ts');
  assert(analysis.splittable === true, 'multi-action should be splittable');
  assert(analysis.suggestedMode === 'parallel', 'multi-file splittable tasks should be parallel');
});

// 8) CLI 降级选择
test('CLISelector degrades when preferred CLI unavailable', () => {
  const selector = new CLISelector();
  selector.setAvailableCLIs(['gemini']);
  const selection = selector.selectByCategory('backend');
  assert(selection.cli === 'gemini', `expected gemini fallback, got ${selection.cli}`);
  assert(selection.degraded === true, 'selection should be marked degraded');
});

// 9) 健康评分与降级建议
test('ExecutionStats healthScore + fallback suggestion', () => {
  const stats = new ExecutionStats();
  stats.configure({ healthThreshold: 0.7 });

  recordExecutions(stats, 'claude', 6, true);
  recordExecutions(stats, 'claude', 4, false);
  recordExecutions(stats, 'codex', 5, true);

  const claudeStats = stats.getStats('claude');
  assert(claudeStats.healthScore < 0.7, 'healthScore should be below threshold');
  assert(claudeStats.isHealthy === false, 'claude should be unhealthy');

  const fallback = stats.getFallbackSuggestion('claude', [], ['codex']);
  assert(fallback && fallback.suggestedCli === 'codex', 'fallback should suggest codex');
});

// 10) 上下文截断预压缩
test('ContextManager truncates oversized recent message', async () => {
  const cm = new ContextManager(TEST_ROOT);
  await cm.initialize('test-session', 'test-session');

  const longMessage = 'A'.repeat(5000);
  cm.addMessage({ role: 'user', content: longMessage });

  const context = cm.getContextSlice({ maxTokens: 50, includeMemory: false, includeRecent: true });
  assert(context.includes('<response clipped>'), 'context should include truncation notice');
});

// 11) Memory 摘要注入
test('ContextManager injects memory summary when enabled', async () => {
  const cm = new ContextManager(TEST_ROOT);
  await cm.initialize('test-session-2', 'test-session-2');
  cm.addTask({ id: 't1', description: '实现登录功能', status: 'pending', assignedWorker: 'codex' });
  cm.addDecision('d1', '采用 FastAPI', '快速开发');
  const context = cm.getContextSlice({
    maxTokens: 200,
    includeMemory: true,
    includeRecent: false,
    memorySummary: { includeCurrentTasks: true, includeKeyDecisions: 1 }
  });
  assert(context.includes('会话上下文'), 'context should include memory summary');
});

// 12) 工具输出截断
test('ContextManager truncates tool output', () => {
  const cm = new ContextManager(TEST_ROOT);
  const longOutput = 'B'.repeat(60000);
  const truncated = cm.truncateToolOutput(longOutput);
  assert(truncated.wasTruncated === true, 'tool output should be truncated');
  assert(truncated.content.includes('<response clipped>'), 'tool output should include truncation notice');
});

// 13) 交互模式配置覆盖
test('Interaction mode configs match design expectations', () => {
  const ask = INTERACTION_MODE_CONFIGS.ask;
  const agent = INTERACTION_MODE_CONFIGS.agent;
  const auto = INTERACTION_MODE_CONFIGS.auto;

  assert(ask.allowFileModification === false, 'ask should disallow file modification');
  assert(ask.allowCommandExecution === false, 'ask should disallow command execution');
  assert(agent.requirePlanConfirmation === true, 'agent should require plan confirmation');
  assert(auto.autoRollbackOnFailure === true, 'auto should enable auto rollback');
});

// 14) 依赖图分析：批次与拓扑
test('TaskDependencyGraph analyzes batches and topological order', () => {
  const graph = new TaskDependencyGraph();
  graph.addTask('A', 'Task A');
  graph.addTask('B', 'Task B');
  graph.addTask('C', 'Task C');
  graph.addDependency('B', 'A');
  graph.addDependency('C', 'B');

  const analysis = graph.analyze();
  assert(analysis.hasCycle === false, 'graph should be acyclic');
  assert(analysis.topologicalOrder.join(',') === 'A,B,C', 'topological order should be A,B,C');
  assert(analysis.executionBatches.length === 3, 'should have 3 batches');
  assert(analysis.executionBatches[0].taskIds.includes('A'), 'batch 0 should include A');
});

// 15) 依赖图循环检测
test('TaskDependencyGraph detects cycle', () => {
  const graph = new TaskDependencyGraph();
  graph.addTask('D', 'Task D');
  graph.addTask('E', 'Task E');
  const ok1 = graph.addDependency('D', 'E');
  const ok2 = graph.addDependency('E', 'D'); // should be rejected
  assert(ok1 === true, 'first dependency should be accepted');
  assert(ok2 === false, 'cycle dependency should be rejected');

  const analysis = graph.analyze();
  assert(analysis.hasCycle === false, 'cycle should be prevented');
});

// 16) 文件锁互斥
test('FileLockManager enforces exclusive locks', async () => {
  const manager = new FileLockManager();
  const releaseFirst = await manager.acquire(['src/app.ts']);
  let secondResolved = false;

  const secondPromise = manager.acquire(['src/app.ts']).then(release => {
    secondResolved = true;
    release();
  });

  // 短暂等待，确保第二个未立即获取
  await new Promise(resolve => setTimeout(resolve, 20));
  assert(secondResolved === false, 'second lock should wait until release');

  releaseFirst();
  await secondPromise;
  assert(secondResolved === true, 'second lock should resolve after release');
});

// 18) 智能选择：统计驱动降级
test('CLISelector uses stats-based selection when preferred unhealthy', () => {
  const stats = new ExecutionStats();
  stats.configure({ healthThreshold: 0.8 });

  recordExecutions(stats, 'claude', 2, false);
  recordExecutions(stats, 'codex', 5, true);

  const selector = new CLISelector();
  selector.setAvailableCLIs(['claude', 'codex']);
  selector.setExecutionStats(stats);
  selector.configureSmartSelection({ enabled: true, healthThreshold: 0.8 });

  const selection = selector.selectByCategory('general');
  assert(selection.cli === 'codex', `expected codex, got ${selection.cli}`);
  assert(selection.preferred === 'claude', `expected preferred claude, got ${selection.preferred}`);
});

// 19) 依赖图就绪任务
test('TaskDependencyGraph ready tasks update with status changes', () => {
  const graph = new TaskDependencyGraph();
  graph.addTask('A', 'Task A');
  graph.addTask('B', 'Task B');
  graph.addDependency('B', 'A');

  let ready = graph.getReadyTasks().map(t => t.id);
  assert(ready.length === 1 && ready[0] === 'A', 'only A should be ready initially');

  graph.updateTaskStatus('A', 'completed');
  ready = graph.getReadyTasks().map(t => t.id);
  assert(ready.includes('B'), 'B should be ready after A completed');
});

// 20) 文件锁取消
test('FileLockManager aborts pending lock on abort signal', async () => {
  const manager = new FileLockManager();
  const releaseFirst = await manager.acquire(['src/locked.ts']);

  const controller = new AbortController();
  let aborted = false;
  const pending = manager.acquire(['src/locked.ts'], 5, controller.signal)
    .then(() => {
      throw new Error('pending lock should not resolve');
    })
    .catch(() => {
      aborted = true;
    });

  controller.abort(new Error('abort'));
  await pending;
  releaseFirst();
  assert(aborted === true, 'pending lock should be aborted');
});

// 21) Memory 摘要截断
test('ContextManager truncates oversized memory summary', async () => {
  const cm = new ContextManager(TEST_ROOT);
  await cm.initialize('test-session-3', 'test-session-3');
  for (let i = 0; i < 10; i += 1) {
    cm.addDecision(`d-${i}`, 'A'.repeat(200), 'reason');
  }
  const context = cm.getContextSlice({
    maxTokens: 80,
    includeMemory: true,
    includeRecent: false,
    memorySummary: { includeKeyDecisions: 10 }
  });
  assert(context.includes('<response clipped>'), 'memory summary should be truncated');
});

// 23) 并行批次可视化
test('TaskDependencyGraph produces parallel batches for independent tasks', () => {
  const graph = new TaskDependencyGraph();
  graph.addTask('A', 'Task A');
  graph.addTask('B', 'Task B');
  graph.addTask('C', 'Task C');
  graph.addDependency('C', 'A');
  graph.addDependency('C', 'B');

  const analysis = graph.analyze();
  assert(analysis.executionBatches.length === 2, 'should have 2 batches');
  const batch0 = analysis.executionBatches[0].taskIds.sort().join(',');
  assert(batch0 === 'A,B', `expected parallel batch A,B, got ${batch0}`);
});

// 24) 事件总线 onAll/once 流程
test('EventEmitter supports onAll and once', () => {
  const bus = new EventEmitter();
  const events = [];
  bus.onAll(event => events.push(`all:${event.type}`));

  let onceCount = 0;
  bus.once('alpha', () => {
    onceCount += 1;
  });

  bus.emitEvent('alpha', { payload: 1 });
  bus.emitEvent('alpha', { payload: 2 });
  bus.emitEvent('beta', { payload: 3 });

  assert(onceCount === 1, 'once listener should fire once');
  assert(events.length === 3, 'onAll should capture all events');
});

// 25) 全局事件总线可用性
test('globalEventBus emits events and counts listeners', () => {
  let captured = 0;
  const off = globalEventBus.on('ui_event', () => {
    captured += 1;
  });

  globalEventBus.emitEvent('ui_event', { payload: 'ping' });
  assert(captured === 1, 'globalEventBus should deliver events');
  assert(globalEventBus.listenerCount('ui_event') >= 1, 'listenerCount should reflect active listeners');
  off();
  assert(globalEventBus.listenerCount('ui_event') === 0, 'listener should be removable');
});

// 26) MessageBus 事件历史与过滤
test('MessageBus stores history and supports filters', () => {
  const bus = new MessageBus();
  bus.dispatchTask('orchestrator', 'worker-1', 'task-1', { id: 'sub-1', prompt: 'Do it' }, 'ctx');
  bus.reportProgress('worker-1', 'orchestrator', 'task-1', 'sub-1', 'running', { progress: 50 });
  bus.reportTaskCompleted('worker-1', 'orchestrator', { taskId: 'task-1', subTaskId: 'sub-1' });

  const history = bus.getHistory();
  assert(history.length === 3, 'history should track all messages');

  const progressOnly = bus.getHistory({ type: 'progress_report' });
  assert(progressOnly.length === 1, 'filtered history should return only progress reports');

  const byTarget = bus.getHistory({ target: 'orchestrator' });
  assert(byTarget.length === 2, 'target filter should match messages to orchestrator');
});

// 27) MessageBus 订阅路由（目标与类型）
test('MessageBus routes messages to target and type subscribers', () => {
  const bus = new MessageBus();
  let targetCount = 0;
  let typeCount = 0;

  bus.subscribe('worker-2', () => {
    targetCount += 1;
  });
  bus.subscribe('task_cancel', () => {
    typeCount += 1;
  });

  bus.cancelTask('orchestrator', 'worker-2', 'task-9', 'sub-9', 'user_cancel');
  assert(targetCount === 1, 'target subscriber should receive message');
  assert(typeCount === 1, 'type subscriber should receive message');
});

// 28) 风险策略：高风险触发 hard stop
test('RiskPolicy flags high risk for interface + config change', () => {
  const policy = new RiskPolicy();
  const plan = {
    id: 'plan-1',
    analysis: '新增 API 接口与配置变更',
    needsCollaboration: true,
    subTasks: [
      { id: '1', taskId: 't1', description: '更新接口', assignedWorker: 'codex', targetFiles: ['src/api/auth.ts'] },
      { id: '2', taskId: 't1', description: '更新依赖', assignedWorker: 'claude', targetFiles: ['package.json'] },
    ],
    executionMode: 'parallel',
    summary: 'test',
    featureContract: '新增登录 API 接口，调整请求与响应字段',
    acceptanceCriteria: ['API 正常返回'],
    createdAt: Date.now(),
  };
  const assessment = policy.evaluate('新增 API 接口与配置', plan);
  assert(assessment.level === 'high', `expected high risk, got ${assessment.level}`);
  assert(assessment.hardStop === true, 'expected hardStop for high risk');
  assert(assessment.verification === 'full', 'expected full verification for high risk');
});

// 29) AI 分解启用条件
test('AITaskDecomposer shouldUseAI respects complexity threshold', () => {
  const fakeFactory = { sendMessage: async () => ({ error: 'skip', content: '' }) };
  const selector = new CLISelector();
  const decomposer = new AITaskDecomposer(fakeFactory, selector, { complexityThreshold: 3 });

  const analyzer = new TaskAnalyzer();
  const high = analyzer.analyze('需要同时完成多模块重构并优化性能');
  const low = analyzer.analyze('修复一个小问题');

  assert(decomposer.shouldUseAI(high) === true, 'high complexity should use AI decomposition');
  assert(decomposer.shouldUseAI(low) === false, 'low complexity should not use AI decomposition');
});

// 30) 快照变更文件回填
test('SnapshotManager returns changed files by subTaskId', () => {
  const tmpRoot = path.join(TEST_ROOT, 'snapshot-tests');
  fs.mkdirSync(tmpRoot, { recursive: true });
  const sessionManager = new SessionManager(tmpRoot);
  const snapshotManager = new SnapshotManager(sessionManager, tmpRoot);
  const session = sessionManager.getOrCreateCurrentSession();

  const filePath = path.join(tmpRoot, 'src', 'demo.txt');
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, 'before', 'utf-8');

  const subTaskId = 'sub-snap-1';
  snapshotManager.createSnapshot(filePath, 'claude', subTaskId);
  fs.writeFileSync(filePath, 'after', 'utf-8');

  const changed = snapshotManager.getChangedFilesForSubTask(subTaskId);
  const relative = path.relative(tmpRoot, filePath);
  assert(changed.includes(relative), 'changed files should include modified file');
});

// 31) 测试已移除 - OrchestratorAgent 已被 MissionDrivenEngine 替代

// 33) 并行 -> 串行锁冲突（域锁）
test('FileLockManager blocks conflicting domain locks', async () => {
  const manager = new FileLockManager();
  const releaseFirst = await manager.acquire(['__domain:integration']);
  let secondResolved = false;
  const pending = manager.acquire(['__domain:integration']).then(release => {
    secondResolved = true;
    release();
  });
  await new Promise(resolve => setTimeout(resolve, 20));
  assert(secondResolved === false, 'domain lock should block concurrent acquisition');
  releaseFirst();
  await pending;
  assert(secondResolved === true, 'domain lock should release and allow next');
});

async function run() {
  let failed = 0;
  for (const { name, fn } of tests) {
    try {
      await fn();
      console.log(`PASS: ${name}`);
    } catch (error) {
      failed += 1;
      console.error(`FAIL: ${name}`);
      console.error(error && error.stack ? error.stack : error);
    }
  }

  if (failed > 0) {
    console.error(`\n${failed} test(s) failed.`);
    process.exit(1);
  }
  console.log('\nAll tests passed.');
}

run();
