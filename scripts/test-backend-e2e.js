#!/usr/bin/env node
/**
 * 后端完整 E2E 测试
 *
 * 测试范围：
 * 1. 消息接收 → IntelligentOrchestrator 入口
 * 2. 编排逻辑 → MissionDrivenEngine 规划与执行
 * 3. CLI 调用 → Worker 执行与消息处理
 * 4. 结果处理 → 统一格式化返回
 * 5. 事件发射 → 模拟前端接收的事件流
 *
 * 用法:
 *   node scripts/test-backend-e2e.js [场景过滤]
 */

const fs = require('fs');
const path = require('path');

// Mock vscode module
const Module = require('module');
const vscodeMock = {
  languages: { getDiagnostics: () => [] },
  DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
  window: { showInformationMessage: () => {}, showErrorMessage: () => {} },
  workspace: { workspaceFolders: [] },
};
const originalModuleLoad = Module._load;
Module._load = function load(request, parent, isMain) {
  if (request === 'vscode') return vscodeMock;
  return originalModuleLoad(request, parent, isMain);
};

const ROOT = '/Users/xie/code/MultiCLI';
const { CLIAdapterFactory } = require(path.join(ROOT, 'out/cli/adapter-factory'));
const { IntelligentOrchestrator } = require(path.join(ROOT, 'out/orchestrator/intelligent-orchestrator'));
const { UnifiedTaskManager } = require(path.join(ROOT, 'out/task/unified-task-manager'));
const { SessionManagerTaskRepository } = require(path.join(ROOT, 'out/task/session-manager-task-repository'));
const { SnapshotManager } = require(path.join(ROOT, 'out/snapshot-manager'));
const { UnifiedSessionManager } = require(path.join(ROOT, 'out/session'));
const { globalEventBus } = require(path.join(ROOT, 'out/events'));

// ============================================================================
// 测试基础设施
// ============================================================================

class EventCollector {
  constructor() {
    this.events = [];
    this.standardMessages = [];
    this.standardCompletes = [];
    this.unsubscribes = [];
  }

  setup(cliFactory) {
    // 收集 CLI 标准消息
    cliFactory.removeAllListeners('standardMessage');
    cliFactory.removeAllListeners('standardComplete');

    cliFactory.on('standardMessage', (msg) => {
      this.standardMessages.push({
        ...msg,
        _receivedAt: Date.now(),
      });
    });

    cliFactory.on('standardComplete', (msg) => {
      this.standardCompletes.push({
        ...msg,
        _receivedAt: Date.now(),
      });
    });

    // 收集全局事件（模拟前端接收）
    const eventTypes = [
      'orchestrator:phase_changed',
      'orchestrator:ui_message',
      'orchestrator:mode_changed',
      'task:started',
      'task:completed',
      'task:failed',
      'task:cancelled',
      'subtask:started',
      'subtask:completed',
      'subtask:failed',
      'plan:created',
      'plan:confirmed',
      'mission:created',
      'mission:phase_changed',
      'mission:completed',
    ];

    for (const type of eventTypes) {
      const unsub = globalEventBus.on(type, (event) => {
        this.events.push({
          type,
          event,
          _receivedAt: Date.now(),
        });
      });
      this.unsubscribes.push(unsub);
    }
  }

  cleanup() {
    for (const unsub of this.unsubscribes) {
      try { unsub(); } catch {}
    }
    this.unsubscribes = [];
  }

  reset() {
    this.events = [];
    this.standardMessages = [];
    this.standardCompletes = [];
  }

  getEventsByType(type) {
    return this.events.filter(e => e.type === type);
  }

  getMessagesByLifecycle(lifecycle) {
    return this.standardMessages.filter(m => m.lifecycle === lifecycle);
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(`ASSERTION FAILED: ${message}`);
  }
}

function extractTextFromBlocks(blocks) {
  if (!Array.isArray(blocks)) return '';
  return blocks
    .filter(b => b && b.type === 'text' && typeof b.content === 'string')
    .map(b => b.content)
    .join('\n');
}

// ============================================================================
// 测试场景定义
// ============================================================================

const scenarios = [
  {
    name: 'ASK模式-简单问答',
    prompt: '你好，你是谁？只需简短回答。',
    mode: 'ask',
    expectations: {
      hasResult: true,
      minResultLength: 5,
      noSubtasks: true,
      eventsRequired: ['task:started', 'task:completed'],
    },
  },
  {
    name: 'AUTO模式-能力询问（应自动判断为ask）',
    prompt: 'Claude Code 是什么？简要说明即可。',
    mode: 'auto',
    expectations: {
      hasResult: true,
      minResultLength: 10,
      eventsRequired: ['task:started', 'task:completed'],
    },
  },
  {
    name: 'AGENT模式-单Worker任务',
    prompt: '创建一个名为 test_backend_e2e_single.txt 的文件，内容为 BACKEND_E2E_OK',
    mode: 'agent',
    clarifyBehavior: 'skip',
    expectations: {
      hasResult: true,
      hasStandardMessages: true,
      eventsRequired: ['task:started', 'task:completed'],
    },
  },
  {
    name: '消息格式验证-StandardMessage结构',
    prompt: '请简单介绍一下 TypeScript 的主要特性。',
    mode: 'ask',
    expectations: {
      hasResult: true,
      validateMessageFormat: true,
    },
  },
  {
    name: '错误处理-无效任务（测试错误路径）',
    prompt: '', // 空 prompt 应该触发错误处理
    mode: 'auto',
    expectations: {
      shouldFail: true,
    },
  },
  {
    name: '事件流完整性-验证事件序列',
    prompt: '你能告诉我当前时间吗？只需直接回答。',
    mode: 'ask',
    expectations: {
      hasResult: true,
      validateEventSequence: true,
    },
  },
];

// ============================================================================
// 测试执行器
// ============================================================================

async function runScenario(orchestrator, cliFactory, collector, scenario) {
  collector.reset();

  // 设置交互模式
  orchestrator.setInteractionMode(scenario.mode);

  // 设置回调
  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setQuestionCallback(async (questions) => {
    return questions.map(q => `Q: ${q}\nA: 已回答`).join('\n\n');
  });

  if (scenario.clarifyBehavior === 'skip') {
    orchestrator.setClarificationCallback(async (questions) => {
      const answers = {};
      questions.forEach(q => { answers[q] = '跳过澄清'; });
      return { answers, additionalInfo: '' };
    });
  } else if (scenario.clarifyBehavior === 'cancel') {
    orchestrator.setClarificationCallback(async () => null);
  } else {
    orchestrator.setClarificationCallback(async (questions) => {
      const answers = {};
      questions.forEach(q => { answers[q] = '默认回答'; });
      return { answers, additionalInfo: '' };
    });
  }

  const taskId = `test-${Date.now()}`;
  const start = Date.now();
  let result = null;
  let error = null;

  try {
    result = await orchestrator.execute(scenario.prompt, taskId);
  } catch (err) {
    error = err instanceof Error ? err : new Error(String(err));
  }

  const duration = Date.now() - start;

  return {
    name: scenario.name,
    prompt: scenario.prompt,
    mode: scenario.mode,
    result,
    error,
    duration,
    events: [...collector.events],
    standardMessages: [...collector.standardMessages],
    standardCompletes: [...collector.standardCompletes],
    expectations: scenario.expectations,
  };
}

function validateResult(testResult) {
  const { expectations, result, error, events, standardMessages, standardCompletes } = testResult;
  const failures = [];

  // 基本结果验证
  if (expectations.shouldFail) {
    if (!error) {
      failures.push('期望失败但成功执行');
    }
  } else {
    if (error) {
      failures.push(`执行失败: ${error.message}`);
    }
  }

  if (expectations.hasResult && !result) {
    failures.push('期望有返回结果但为空');
  }

  if (expectations.minResultLength && result && result.length < expectations.minResultLength) {
    failures.push(`结果长度不足: ${result.length} < ${expectations.minResultLength}`);
  }

  // 子任务验证
  if (expectations.noSubtasks) {
    const subtaskEvents = events.filter(e => e.type.startsWith('subtask:'));
    if (subtaskEvents.length > 0) {
      failures.push(`期望无子任务但有 ${subtaskEvents.length} 个子任务事件`);
    }
  }

  // 事件验证
  if (expectations.eventsRequired) {
    for (const eventType of expectations.eventsRequired) {
      const found = events.some(e => e.type === eventType);
      if (!found) {
        failures.push(`缺少必需事件: ${eventType}`);
      }
    }
  }

  // StandardMessage 格式验证
  if (expectations.validateMessageFormat && standardCompletes.length > 0) {
    for (const msg of standardCompletes) {
      if (!msg.id) failures.push('StandardMessage 缺少 id');
      if (!msg.type) failures.push('StandardMessage 缺少 type');
      if (!msg.lifecycle) failures.push('StandardMessage 缺少 lifecycle');
      if (!msg.cli) failures.push('StandardMessage 缺少 cli');
      if (!Array.isArray(msg.blocks)) failures.push('StandardMessage blocks 不是数组');
    }
  }

  // 事件序列验证
  if (expectations.validateEventSequence) {
    const taskStarted = events.find(e => e.type === 'task:started');
    const taskCompleted = events.find(e => e.type === 'task:completed');

    if (taskStarted && taskCompleted) {
      if (taskStarted._receivedAt > taskCompleted._receivedAt) {
        failures.push('事件顺序错误: task:started 应在 task:completed 之前');
      }
    }
  }

  // 标准消息验证
  if (expectations.hasStandardMessages && standardMessages.length === 0 && standardCompletes.length === 0) {
    failures.push('期望有 StandardMessage 但未收到');
  }

  return failures;
}

// ============================================================================
// 主测试流程
// ============================================================================

async function run() {
  const cwd = ROOT;
  const tempRoot = path.join(cwd, '.tmp', 'backend-e2e');

  try {
    fs.mkdirSync(tempRoot, { recursive: true });
  } catch {}

  console.log('╔══════════════════════════════════════════════════════════════╗');
  console.log('║          MultiCLI 后端完整 E2E 测试                          ║');
  console.log('╚══════════════════════════════════════════════════════════════╝');
  console.log('');

  // 初始化组件
  const sessionManager = new UnifiedSessionManager(cwd);
  const sessionId = sessionManager.getOrCreateCurrentSession().id;
  const repository = new SessionManagerTaskRepository(sessionManager, sessionId);
  const taskManager = new UnifiedTaskManager(sessionId, repository);
  await taskManager.initialize();
  const snapshotManager = new SnapshotManager(sessionManager, cwd);
  const cliFactory = new CLIAdapterFactory({ cwd });

  const orchestrator = new IntelligentOrchestrator(
    cliFactory,
    sessionManager,
    snapshotManager,
    cwd,
    {
      review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
      planReview: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      integration: { enabled: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
    }
  );
  orchestrator.setTaskManager(taskManager, sessionId);
  await orchestrator.initialize();

  // 检查 CLI 可用性
  const availability = await cliFactory.checkAllAvailability();
  const available = Object.entries(availability).filter(([, ok]) => ok).map(([k]) => k);

  console.log(`✓ 可用 CLI: ${available.length > 0 ? available.join(', ') : '无'}`);
  console.log(`✓ 会话 ID: ${sessionId}`);
  console.log('');

  if (available.length === 0) {
    console.error('❌ 没有可用的 CLI，无法进行测试');
    process.exit(1);
  }

  // 初始化事件收集器
  const collector = new EventCollector();
  collector.setup(cliFactory);

  // 执行测试场景
  const filterArg = (process.argv.slice(2).join(' ') || '').trim().toLowerCase();
  const results = [];
  let passed = 0;
  let failed = 0;
  let skipped = 0;

  for (const scenario of scenarios) {
    if (filterArg && !scenario.name.toLowerCase().includes(filterArg)) {
      skipped++;
      continue;
    }

    console.log(`\n┌─ 场景: ${scenario.name}`);
    console.log(`│  模式: ${scenario.mode}`);
    console.log(`│  输入: ${scenario.prompt.slice(0, 60)}${scenario.prompt.length > 60 ? '...' : ''}`);

    const testResult = await runScenario(orchestrator, cliFactory, collector, scenario);
    const failures = validateResult(testResult);

    results.push({
      ...testResult,
      failures,
      passed: failures.length === 0,
    });

    if (failures.length === 0) {
      passed++;
      console.log(`│  ✅ 通过 (${testResult.duration}ms)`);
      console.log(`│  事件: ${testResult.events.length}, 消息: ${testResult.standardCompletes.length}`);
    } else {
      failed++;
      console.log(`│  ❌ 失败 (${testResult.duration}ms)`);
      for (const f of failures) {
        console.log(`│     - ${f}`);
      }
    }
    console.log('└──────────────────────────────────────────────────');
  }

  // 清理
  collector.cleanup();
  await cliFactory.disconnectAll().catch(() => {});

  // 汇总报告
  console.log('\n╔══════════════════════════════════════════════════════════════╗');
  console.log('║                        测试汇总                              ║');
  console.log('╠══════════════════════════════════════════════════════════════╣');
  console.log(`║  总计: ${results.length} 个场景                                           ║`);
  console.log(`║  ✅ 通过: ${passed}                                                      ║`);
  console.log(`║  ❌ 失败: ${failed}                                                      ║`);
  console.log(`║  ⏭️  跳过: ${skipped}                                                      ║`);
  console.log('╚══════════════════════════════════════════════════════════════╝');

  // 详细报告
  console.log('\n=== 详细结果 ===');
  for (const r of results) {
    const status = r.passed ? '✅' : '❌';
    console.log(`${status} ${r.name}`);
    console.log(`   耗时: ${r.duration}ms, 事件: ${r.events.length}, 消息: ${r.standardCompletes.length}`);
    if (r.result) {
      console.log(`   返回: ${r.result.slice(0, 100)}${r.result.length > 100 ? '...' : ''}`);
    }
    if (r.error) {
      console.log(`   错误: ${r.error.message}`);
    }
    if (r.failures.length > 0) {
      for (const f of r.failures) {
        console.log(`   ❌ ${f}`);
      }
    }
  }

  // 事件流分析
  console.log('\n=== 事件流分析 ===');
  const allEvents = results.flatMap(r => r.events);
  const eventCounts = {};
  for (const e of allEvents) {
    eventCounts[e.type] = (eventCounts[e.type] || 0) + 1;
  }
  for (const [type, count] of Object.entries(eventCounts)) {
    console.log(`  ${type}: ${count}`);
  }

  // 消息格式分析
  console.log('\n=== 消息格式分析 ===');
  const allMessages = results.flatMap(r => r.standardCompletes);
  const messagesByType = {};
  for (const m of allMessages) {
    const type = m.type || 'unknown';
    messagesByType[type] = (messagesByType[type] || 0) + 1;
  }
  for (const [type, count] of Object.entries(messagesByType)) {
    console.log(`  ${type}: ${count}`);
  }

  // 退出
  setTimeout(() => process.exit(failed > 0 ? 1 : 0), 1000);
}

run().catch((err) => {
  console.error('测试执行失败:', err);
  process.exit(1);
});
