#!/usr/bin/env node
/**
 * 消息流诊断脚本
 *
 * 追踪消息从 CLI 进程到 UI 的完整流程：
 * 1. CLI 进程输出 → InteractiveSession 接收
 * 2. InteractiveSession → Normalizer 标准化
 * 3. 标准化消息 → CLIAdapterFactory 事件发射
 * 4. 事件 → WebviewProvider 应该接收的格式
 *
 * 用法:
 *   node scripts/diagnose-message-flow.js [prompt]
 */

const path = require('path');
const Module = require('module');

// Mock vscode
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
// 诊断收集器
// ============================================================================

class DiagnosticCollector {
  constructor() {
    this.checkpoints = [];
    this.rawOutputs = [];
    this.normalizedMessages = [];
    this.standardMessages = [];
    this.standardCompletes = [];
    this.globalEvents = [];
    this.errors = [];
  }

  log(checkpoint, data) {
    const entry = {
      checkpoint,
      timestamp: Date.now(),
      data: typeof data === 'object' ? JSON.stringify(data, null, 2) : data,
    };
    this.checkpoints.push(entry);
    console.log(`\n🔍 [${checkpoint}]`);
    if (data) {
      const preview = typeof data === 'string'
        ? data.slice(0, 200)
        : JSON.stringify(data, null, 2).slice(0, 500);
      console.log(preview + (preview.length >= 200 ? '...' : ''));
    }
  }

  logError(checkpoint, error) {
    this.errors.push({ checkpoint, error: error.message, stack: error.stack });
    console.error(`\n❌ [${checkpoint}] ERROR:`, error.message);
  }
}

// ============================================================================
// 主诊断流程
// ============================================================================

async function diagnose() {
  const prompt = process.argv.slice(2).join(' ') || '你好，简单回复即可';
  const collector = new DiagnosticCollector();

  console.log('╔══════════════════════════════════════════════════════════════╗');
  console.log('║            消息流诊断 - 追踪 CLI → UI 完整路径              ║');
  console.log('╚══════════════════════════════════════════════════════════════╝');
  console.log(`\n用户输入: ${prompt}\n`);

  // 初始化组件
  const cwd = ROOT;
  const sessionManager = new UnifiedSessionManager(cwd);
  const sessionId = sessionManager.getOrCreateCurrentSession().id;
  const repository = new SessionManagerTaskRepository(sessionManager, sessionId);
  const taskManager = new UnifiedTaskManager(sessionId, repository);
  await taskManager.initialize();
  const snapshotManager = new SnapshotManager(sessionManager, cwd);
  const cliFactory = new CLIAdapterFactory({ cwd });

  collector.log('初始化完成', { sessionId, cwd });

  // ============================================================================
  // 检查点 1: CLI 进程原始输出
  // ============================================================================

  // 监听 CLIAdapterFactory 内部事件
  cliFactory.on('rawOutput', (data) => {
    collector.rawOutputs.push(data);
    collector.log('CLI原始输出', {
      cli: data.cli,
      contentLength: data.content?.length,
      preview: data.content?.slice(0, 100),
    });
  });

  // ============================================================================
  // 检查点 2: 标准化后的消息
  // ============================================================================

  cliFactory.on('normalizedMessage', (msg) => {
    collector.normalizedMessages.push(msg);
    collector.log('标准化消息', {
      id: msg.id,
      type: msg.type,
      lifecycle: msg.lifecycle,
      blocksCount: msg.blocks?.length,
    });
  });

  // ============================================================================
  // 检查点 3: StandardMessage 事件
  // ============================================================================

  cliFactory.on('standardMessage', (msg) => {
    collector.standardMessages.push(msg);
    collector.log('StandardMessage事件', {
      id: msg.id,
      type: msg.type,
      lifecycle: msg.lifecycle,
      cli: msg.cli,
      source: msg.source,
      blocksCount: msg.blocks?.length,
      hasMetadata: !!msg.metadata,
    });
  });

  cliFactory.on('standardComplete', (msg) => {
    collector.standardCompletes.push(msg);
    collector.log('StandardComplete事件', {
      id: msg.id,
      type: msg.type,
      lifecycle: msg.lifecycle,
      blocksCount: msg.blocks?.length,
      blocks: msg.blocks?.map(b => ({
        type: b.type,
        contentLength: b.content?.length,
        preview: b.content?.slice(0, 50),
      })),
    });
  });

  // ============================================================================
  // 检查点 4: 全局事件总线（UI 应该监听的）
  // ============================================================================

  const uiRelevantEvents = [
    'orchestrator:ui_message',
    'orchestrator:phase_changed',
    'task:started',
    'task:completed',
    'task:failed',
    'subtask:started',
    'subtask:completed',
    'plan:created',
    'mission:created',
    'cli:message',
    'cli:output',
    'worker:output',
    'worker:message',
  ];

  const unsubscribes = [];
  for (const eventType of uiRelevantEvents) {
    const unsub = globalEventBus.on(eventType, (event) => {
      collector.globalEvents.push({ type: eventType, event });
      collector.log(`全局事件[${eventType}]`, {
        taskId: event.taskId,
        subTaskId: event.subTaskId,
        dataType: typeof event.data,
        dataKeys: event.data ? Object.keys(event.data) : [],
      });
    });
    unsubscribes.push(unsub);
  }

  // ============================================================================
  // 执行测试
  // ============================================================================

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
  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setClarificationCallback(async (questions) => {
    const answers = {};
    questions.forEach(q => { answers[q] = '跳过'; });
    return { answers, additionalInfo: '' };
  });
  await orchestrator.initialize();

  collector.log('Orchestrator初始化完成', { mode: orchestrator.getInteractionMode() });

  const taskId = `diag-${Date.now()}`;
  let result = null;
  let error = null;

  try {
    collector.log('开始执行', { taskId, prompt });
    result = await orchestrator.execute(prompt, taskId);
    collector.log('执行完成', { resultLength: result?.length });
  } catch (err) {
    error = err;
    collector.logError('执行失败', err);
  }

  // 清理
  for (const unsub of unsubscribes) {
    try { unsub(); } catch {}
  }
  await cliFactory.disconnectAll().catch(() => {});

  // ============================================================================
  // 诊断报告
  // ============================================================================

  console.log('\n');
  console.log('╔══════════════════════════════════════════════════════════════╗');
  console.log('║                        诊断报告                              ║');
  console.log('╚══════════════════════════════════════════════════════════════╝');

  console.log('\n=== 消息流统计 ===');
  console.log(`  CLI原始输出: ${collector.rawOutputs.length}`);
  console.log(`  标准化消息: ${collector.normalizedMessages.length}`);
  console.log(`  StandardMessage: ${collector.standardMessages.length}`);
  console.log(`  StandardComplete: ${collector.standardCompletes.length}`);
  console.log(`  全局事件: ${collector.globalEvents.length}`);
  console.log(`  错误: ${collector.errors.length}`);

  console.log('\n=== 断点分析 ===');

  // 分析断点
  const issues = [];

  if (collector.rawOutputs.length === 0) {
    issues.push('❌ CLI原始输出为空 - CLI进程可能未正确输出或InteractiveSession未捕获');
  } else {
    console.log(`✅ CLI原始输出正常 (${collector.rawOutputs.length} 条)`);
  }

  if (collector.normalizedMessages.length === 0 && collector.rawOutputs.length > 0) {
    issues.push('❌ 标准化消息为空 - Normalizer可能未正确处理CLI输出');
  } else if (collector.normalizedMessages.length > 0) {
    console.log(`✅ 消息标准化正常 (${collector.normalizedMessages.length} 条)`);
  }

  if (collector.standardMessages.length === 0 && collector.normalizedMessages.length > 0) {
    issues.push('❌ StandardMessage事件为空 - CLIAdapterFactory可能未正确发射事件');
  } else if (collector.standardMessages.length > 0) {
    console.log(`✅ StandardMessage事件正常 (${collector.standardMessages.length} 条)`);
  }

  if (collector.standardCompletes.length === 0) {
    issues.push('❌ StandardComplete事件为空 - 消息可能未正确完成');
  } else {
    console.log(`✅ StandardComplete事件正常 (${collector.standardCompletes.length} 条)`);
  }

  // 检查全局事件
  const hasUiMessage = collector.globalEvents.some(e => e.type === 'orchestrator:ui_message');
  const hasCliMessage = collector.globalEvents.some(e =>
    e.type === 'cli:message' || e.type === 'cli:output' || e.type === 'worker:output'
  );

  if (!hasUiMessage && !hasCliMessage) {
    issues.push('❌ 无UI相关全局事件 - WebviewProvider可能无法接收消息');
  } else {
    console.log(`✅ UI相关事件正常`);
  }

  // 事件类型分布
  console.log('\n=== 事件类型分布 ===');
  const eventCounts = {};
  for (const e of collector.globalEvents) {
    eventCounts[e.type] = (eventCounts[e.type] || 0) + 1;
  }
  for (const [type, count] of Object.entries(eventCounts)) {
    console.log(`  ${type}: ${count}`);
  }

  // 问题汇总
  if (issues.length > 0) {
    console.log('\n=== 发现的问题 ===');
    for (const issue of issues) {
      console.log(issue);
    }
  } else {
    console.log('\n✅ 后端消息流正常，问题可能在UI层');
  }

  // 结果内容
  if (result) {
    console.log('\n=== 执行结果 ===');
    console.log(result.slice(0, 500) + (result.length > 500 ? '...' : ''));
  }

  // StandardComplete 内容详情
  if (collector.standardCompletes.length > 0) {
    console.log('\n=== StandardComplete 详情 ===');
    for (const msg of collector.standardCompletes.slice(0, 3)) {
      console.log(`  ID: ${msg.id}`);
      console.log(`  Type: ${msg.type}, Lifecycle: ${msg.lifecycle}`);
      console.log(`  CLI: ${msg.cli}, Source: ${msg.source}`);
      console.log(`  Blocks: ${msg.blocks?.length}`);
      for (const block of (msg.blocks || []).slice(0, 2)) {
        console.log(`    - ${block.type}: ${block.content?.slice(0, 100)}...`);
      }
    }
  }

  // 建议
  console.log('\n=== 诊断建议 ===');
  if (issues.length === 0) {
    console.log('后端消息流正常。问题可能在：');
    console.log('  1. WebviewProvider 未正确监听事件');
    console.log('  2. WebviewProvider 未正确转发消息到 Webview');
    console.log('  3. Webview 端渲染逻辑问题');
    console.log('');
    console.log('建议检查 WebviewProvider 的 postMessage 调用');
  } else {
    console.log('发现后端消息流问题，请检查上述断点');
  }

  setTimeout(() => process.exit(error ? 1 : 0), 1000);
}

diagnose().catch((err) => {
  console.error('诊断失败:', err);
  process.exit(1);
});
