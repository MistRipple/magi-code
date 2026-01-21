/**
 * 真实 CLI 编排 E2E 场景覆盖
 * 覆盖：ASK / 清晰任务 / 澄清取消 / 澄清补充 / 单 Worker / 多 Worker
 *
 * 用法:
 *   node scripts/test-real-orchestrator-scenarios.js
 */

// Mock vscode module for non-VSCode runtime
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

const fs = require('fs');
const path = require('path');
const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');
const { UnifiedTaskManager } = require('../out/task/unified-task-manager');
const { SessionManagerTaskRepository } = require('../out/task/session-manager-task-repository');
const { SnapshotManager } = require('../out/snapshot-manager');
const { UnifiedSessionManager } = require('../out/session');
const { globalEventBus } = require('../out/events');

function extractTextFromBlocks(blocks) {
  if (!Array.isArray(blocks)) return '';
  return blocks
    .filter(b => b && b.type === 'text' && typeof b.content === 'string')
    .map(b => b.content)
    .join('\n');
}

function normalizeText(input) {
  return String(input || '')
    .replace(/^#{1,6}\s*/gm, '')
    .replace(/\*\*/g, '')
    .replace(/\s+/g, ' ')
    .trim()
    .toLowerCase();
}

async function runScenario(orchestrator, cliFactory, scenario) {
  const standardCompletes = [];
  const duplicates = [];
  const seenContent = new Set();
  const subtaskStarts = [];
  const progressEvents = [];

  cliFactory.removeAllListeners('standardComplete');
  cliFactory.on('standardComplete', (msg) => {
    standardCompletes.push(msg);
    const content = extractTextFromBlocks(msg.blocks);
    const key = normalizeText(content);
    if (key && seenContent.has(key)) {
      duplicates.push({ id: msg.id, content });
    } else if (key) {
      seenContent.add(key);
    }
  });

  const unsubUi = globalEventBus.on('orchestrator:ui_message', (event) => {
    const data = event.data || {};
    if (data.type === 'progress_update') {
      progressEvents.push(data.content);
    }
  });
  const unsubStart = globalEventBus.on('subtask:started', (event) => {
    const data = event.data || {};
    subtaskStarts.push({ subTaskId: event.subTaskId, dispatchId: data.dispatchId, cli: data.cli });
  });

  let result = '';
  let error = null;
  const start = Date.now();
  try {
    result = await orchestrator.execute(scenario.prompt, '');
  } catch (err) {
    error = err instanceof Error ? err : new Error(String(err));
  } finally {
    unsubUi();
    unsubStart();
  }

  const duration = Date.now() - start;
  return {
    name: scenario.name,
    prompt: scenario.prompt,
    result,
    error,
    duration,
    progressEvents,
    subtaskStarts,
    duplicates,
  };
}

async function run() {
  const cwd = process.cwd();
  const tempRoot = path.join(cwd, '.tmp', 'multicli-e2e');
  try {
    fs.mkdirSync(tempRoot, { recursive: true });
  } catch (err) {
    console.warn('创建临时目录失败:', tempRoot, err.message);
  }
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

  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setQuestionCallback(async (questions) => {
    return questions.map(q => `Q: ${q}\nA: 已补充`).join('\n\n');
  });

  await orchestrator.initialize();

  const availability = await cliFactory.checkAllAvailability();
  const available = Object.entries(availability).filter(([, ok]) => ok).map(([k]) => k);
  if (available.length === 0) {
    console.error('没有可用的 CLI，无法进行真实编排测试。');
    process.exit(1);
  }

  console.log('=== 真实 CLI 编排 E2E 场景 ===');
  console.log(`可用 CLI: ${available.join(', ')}`);
  console.log('');

  const scenarios = [
    {
      name: 'ASK 能力询问',
      prompt: '你能做编排任务吗？只需回答即可。',
      clarify: 'none',
    },
    {
      name: '需求清晰不澄清（单文件 workspace）',
      prompt: `在 ${path.join(tempRoot, 'multicli_e2e_clear.txt')} 写入 OK_CLEAR`,
      clarify: 'none',
    },
    {
      name: '澄清 -> 取消',
      prompt: '优化一下性能',
      clarify: 'cancel',
    },
    {
      name: '澄清 -> 补充 -> 执行',
      prompt: '优化一下性能（仅说明方向，不需要实际改代码）',
      clarify: 'answer',
    },
    {
      name: '单 Worker 执行',
      prompt: `创建文件 ${path.join(tempRoot, 'multicli_e2e_single.txt')} 并写入 SINGLE_OK`,
      clarify: 'none',
    },
    {
      name: '多 Worker 并行',
      prompt: `拆分为 3 个并行子任务：1) Claude 在 ${path.join(tempRoot, 'multicli_e2e_claude.txt')} 写入 CLAUDE_OK 2) Codex 在 ${path.join(tempRoot, 'multicli_e2e_codex.txt')} 写入 CODEX_OK 3) Gemini 在 ${path.join(tempRoot, 'multicli_e2e_gemini.txt')} 写入 GEMINI_OK。要求并行执行。`,
      clarify: 'none',
    },
  ];

  const filterArg = (process.argv.slice(2).join(' ') || '').trim().toLowerCase();
  const results = [];
  for (const scenario of scenarios) {
    if (filterArg && !scenario.name.toLowerCase().includes(filterArg)) {
      continue;
    }
    if (scenario.name.includes('多 Worker') && available.length < 2) {
      console.log(`跳过 ${scenario.name}（可用 CLI 不足）`);
      continue;
    }

    if (scenario.clarify === 'cancel') {
      orchestrator.setClarificationCallback(async () => null);
    } else if (scenario.clarify === 'answer') {
      orchestrator.setClarificationCallback(async (questions) => {
        const answers = {};
        questions.forEach(q => { answers[q] = '主要关注启动时长与渲染耗时'; });
        return { answers, additionalInfo: '仅给出优化方向与建议，不需要改代码' };
      });
    } else {
      orchestrator.setClarificationCallback(async (questions) => {
        const answers = {};
        questions.forEach(q => { answers[q] = '无'; });
        return { answers, additionalInfo: '' };
      });
    }

    console.log(`\n=== 场景: ${scenario.name} ===`);
    console.log(`用户输入: ${scenario.prompt}`);
    const r = await runScenario(orchestrator, cliFactory, scenario);
    results.push(r);

    console.log(`耗时: ${r.duration}ms`);
    if (r.error) console.log(`错误: ${r.error.message}`);
    console.log(`返回: ${r.result || '(empty)'}`);
    console.log(`进度事件: ${r.progressEvents.length}`);
    console.log(`子任务分发: ${r.subtaskStarts.length}`);
    if (r.duplicates.length) {
      console.log(`重复内容: ${r.duplicates.length}`);
    } else {
      console.log('重复内容: 0');
    }
  }

  await cliFactory.disconnectAll().catch(() => {});

  console.log('\n=== 场景汇总 ===');
  results.forEach(r => {
    console.log(`- ${r.name}: ${r.error ? '失败' : '成功'}, 子任务=${r.subtaskStarts.length}, 重复内容=${r.duplicates.length}`);
  });

  // 防止残留定时器/句柄导致进程不退出
  setTimeout(() => process.exit(0), 1000);
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
