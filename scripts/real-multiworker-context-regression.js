#!/usr/bin/env node
/* eslint-disable no-console */

const Module = require('module');
const path = require('path');

// VS Code 运行时 Mock（用于 Node 环境启动 out 产物）
const originalRequire = Module.prototype.require;
Module.prototype.require = function patchedRequire(id) {
  if (id === 'vscode') {
    const disposable = { dispose: () => {} };
    const event = () => disposable;
    return {
      window: {
        createOutputChannel: () => ({ append: () => {}, appendLine: () => {}, show: () => {}, dispose: () => {} }),
        showInformationMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showQuickPick: () => Promise.resolve(undefined),
        showInputBox: () => Promise.resolve(undefined),
        createTerminal: () => ({
          show: () => {},
          sendText: () => {},
          dispose: () => {},
          processId: Promise.resolve(12345),
        }),
        onDidCloseTerminal: event,
        onDidOpenTerminal: event,
        activeTextEditor: undefined,
        visibleTextEditors: [],
        terminals: [],
      },
      workspace: {
        workspaceFolders: [{ uri: { fsPath: process.cwd() } }],
        getConfiguration: () => ({
          get: (key) => (key === 'magi.timeout' ? 120000 : undefined),
          update: () => Promise.resolve(),
        }),
        onDidChangeConfiguration: event,
        onDidSaveTextDocument: event,
        onDidOpenTextDocument: event,
        onDidCloseTextDocument: event,
        fs: {
          readFile: () => Promise.resolve(Buffer.from('')),
          writeFile: () => Promise.resolve(),
          stat: () => Promise.resolve({ type: 1 }),
        },
      },
      commands: {
        registerCommand: () => disposable,
        executeCommand: () => Promise.resolve(),
      },
      ExtensionContext: class {},
      Uri: {
        file: (p) => ({ fsPath: p, scheme: 'file', path: p }),
        parse: (s) => ({ fsPath: s, scheme: 'file', path: s }),
      },
      FileType: { File: 1, Directory: 2 },
      EventEmitter: class { event = () => {}; fire = () => {}; dispose = () => {}; },
      Disposable: class { dispose = () => {}; static from = () => ({ dispose: () => {} }); },
      Range: class { constructor(start, end) { this.start = start; this.end = end; } },
      Position: class { constructor(line, character) { this.line = line; this.character = character; } },
      ThemeIcon: class { constructor(id) { this.id = id; } },
    };
  }
  return originalRequire.apply(this, arguments);
};

const { MissionDrivenEngine } = require('../out/orchestrator/core/mission-driven-engine');
const { LLMAdapterFactory } = require('../out/llm/adapter-factory');
const { UnifiedSessionManager } = require('../out/session/unified-session-manager');
const { SnapshotManager } = require('../out/snapshot-manager');
const { MessageHub } = require('../out/orchestrator/core/message-hub');

const TARGET_TYPES = ['decision', 'contract', 'risk', 'constraint'];

function buildPromptCandidates(userPrompt) {
  if (userPrompt && userPrompt.trim()) {
    return [userPrompt.trim()];
  }
  return [
    '分析对象固定为当前工作区代码库 Magi（路径：/Users/xie/code/Magi）。请直接并行执行 2 个 worker（claude 与 codex），无需规划阶段、无需拆分 Todo、禁止调用任何工具。只做只读分析：claude 输出 1 条 decision + 1 条 constraint，codex 输出 1 条 risk + 1 条 constraint。禁止修改任何文件，最后由编排者输出合并结论。',
  ];
}

function answerClarification(question, workspaceRoot) {
  const q = (question || '').toLowerCase();
  if (
    q.includes('项目')
    || q.includes('仓库')
    || q.includes('目录')
    || q.includes('代码库')
    || q.includes('分析对象')
    || q.includes('范围')
  ) {
    return `分析对象是当前工作区代码库 Magi，路径为 ${workspaceRoot}。`;
  }
  if (q.includes('工具') || q.includes('读取') || q.includes('访问')) {
    return '保持只读分析，不修改文件，不执行命令，不访问外网。';
  }
  return `按当前工作区 ${workspaceRoot} 进行多 worker 并行只读分析。`;
}

function setupSharedContextMetrics(engine) {
  const contextManager = engine.contextManager;
  const missionOrchestrator = engine.missionOrchestrator;
  const sharedContextPool = contextManager?.getSharedContextPool?.();
  if (!contextManager || !missionOrchestrator || !sharedContextPool) {
    return null;
  }

  const missionSnapshots = new Map();
  const readCountByEntry = new Map();
  const originalGetByMission = sharedContextPool.getByMission.bind(sharedContextPool);
  const originalGetByType = sharedContextPool.getByType.bind(sharedContextPool);
  const originalClearMissionContext = contextManager.clearMissionContext.bind(contextManager);

  sharedContextPool.getByMission = function patchedGetByMission(missionId, options = {}) {
    const entries = originalGetByMission(missionId, options);
    for (const entry of entries) {
      readCountByEntry.set(entry.id, (readCountByEntry.get(entry.id) || 0) + 1);
    }
    return entries;
  };

  sharedContextPool.getByType = function patchedGetByType(missionId, type, maxTokens) {
    const entries = originalGetByType(missionId, type, maxTokens);
    for (const entry of entries) {
      readCountByEntry.set(entry.id, (readCountByEntry.get(entry.id) || 0) + 1);
    }
    return entries;
  };

  contextManager.clearMissionContext = function patchedClearMissionContext(missionId) {
    const entries = originalGetByMission(missionId, {});
    missionSnapshots.set(
      missionId,
      entries.map((entry) => ({
        ...entry,
        sources: Array.isArray(entry.sources) ? [...entry.sources] : undefined,
      }))
    );
    return originalClearMissionContext(missionId);
  };

  async function summarize(missionId) {
    if (!missionId) {
      return null;
    }
    const mission = await missionOrchestrator.getMission(missionId);
    const snapshotEntries = missionSnapshots.get(missionId) || originalGetByMission(missionId, {});
    const targetEntries = snapshotEntries.filter((entry) => TARGET_TYPES.includes(entry.type));

    const typeCounts = {
      decision: 0,
      contract: 0,
      risk: 0,
      constraint: 0,
    };
    for (const entry of targetEntries) {
      if (entry.type in typeCounts) {
        typeCounts[entry.type] += 1;
      }
    }

    const readCounts = targetEntries.map((entry) => readCountByEntry.get(entry.id) || 0);
    const readCoverageCount = readCounts.filter((count) => count > 0).length;
    const reuseCount = readCounts.filter((count) => count > 1).length;
    const totalReads = readCounts.reduce((sum, count) => sum + count, 0);
    const mergedSourceCount = targetEntries.filter((entry) => (entry.sources?.length || 0) > 1).length;
    const assignments = mission?.assignments?.length || 0;
    const workers = Array.from(new Set((mission?.assignments || []).map((a) => a.workerId)));

    return {
      missionId,
      assignments,
      workers,
      targetTypeCounts: typeCounts,
      totalTargetEntries: targetEntries.length,
      writeDensityPerAssignment: assignments > 0 ? targetEntries.length / assignments : targetEntries.length,
      readCoverageRate: targetEntries.length > 0 ? readCoverageCount / targetEntries.length : 0,
      reuseRate: targetEntries.length > 0 ? reuseCount / targetEntries.length : 0,
      avgReadTimes: targetEntries.length > 0 ? totalReads / targetEntries.length : 0,
      mergedSourceRate: targetEntries.length > 0 ? mergedSourceCount / targetEntries.length : 0,
    };
  }

  return { summarize };
}

async function runAttempt(prompt, attemptIndex) {
  const attemptTimeoutMs = Number(process.env.REGRESSION_TIMEOUT_MS || 420000);
  const workspaceRoot = process.cwd();
  const sessionManager = new UnifiedSessionManager(workspaceRoot);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });
  const messageHub = new MessageHub();
  adapterFactory.setMessageHub(messageHub);
  await adapterFactory.initialize();
  const toolManager = adapterFactory.getToolManager();
  toolManager.setAuthorizationCallback(async () => false);

  const engine = new MissionDrivenEngine(
    adapterFactory,
    {
      timeout: 240000,
      maxRetries: 1,
      review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
      planReview: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      integration: { enabled: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
      permissions: { allowEdit: false, allowBash: false, allowWeb: false },
    },
    workspaceRoot,
    snapshotManager,
    sessionManager
  );

  engine.setConfirmationCallback(async () => true);
  engine.setQuestionCallback(async (questions) => (
    questions.map((q) => `Q: ${q}\nA: ${answerClarification(q, workspaceRoot)}`).join('\n\n')
  ));
  engine.setClarificationCallback(async (questions) => ({
    answers: Object.fromEntries(questions.map((q) => [q, answerClarification(q, workspaceRoot)])),
    additionalInfo: `分析对象固定为当前工作区 ${workspaceRoot}。`,
  }));

  await engine.initialize();
  const metricsTracker = setupSharedContextMetrics(engine);

  const workerOutputs = new Set();
  adapterFactory.on('standardMessage', (msg) => {
    if (msg?.source === 'worker' && msg?.agent) {
      workerOutputs.add(msg.agent);
    }
  });

  let error = null;
  const start = Date.now();
  let executePromise = null;
  try {
    executePromise = engine.execute(prompt, `shared-context-regression-${Date.now()}-${attemptIndex}`);
    const timeoutPromise = new Promise((_, reject) => {
      setTimeout(() => reject(new Error(`回归执行超时(${attemptTimeoutMs}ms)`)), attemptTimeoutMs);
    });
    await Promise.race([executePromise, timeoutPromise]);
  } catch (err) {
    error = err instanceof Error ? err : new Error(String(err));
    if (error.message.includes('回归执行超时')) {
      await engine.cancel().catch(() => {});
      if (executePromise) {
        await Promise.race([
          executePromise.catch(() => {}),
          new Promise((resolve) => setTimeout(resolve, 5000)),
        ]);
      }
    }
  }
  const durationMs = Date.now() - start;

  const missionId = engine.lastMissionId || engine?._context?.mission?.id || null;
  const metrics = metricsTracker ? await metricsTracker.summarize(missionId) : null;

  await adapterFactory.shutdown().catch(() => {});
  messageHub.dispose();
  engine.dispose();

  return {
    prompt,
    durationMs,
    error,
    missionId,
    metrics,
    workerOutputs: Array.from(workerOutputs),
  };
}

async function main() {
  const cliPrompt = process.argv.slice(2).join(' ');
  const prompts = buildPromptCandidates(cliPrompt);

  console.log('=== 真实多 Worker 上下文回归 ===');
  let chosen = null;
  const attempts = [];

  for (let i = 0; i < prompts.length; i++) {
    const prompt = prompts[i];
    console.log(`\n--- 尝试 ${i + 1}/${prompts.length} ---`);
    console.log(`prompt: ${prompt}`);
    const result = await runAttempt(prompt, i + 1);
    attempts.push(result);

    const workerCount = result.metrics?.workers?.length || result.workerOutputs.length;
    const hasTargetFacts = (result.metrics?.totalTargetEntries || 0) > 0;
    console.log(`durationMs: ${result.durationMs}`);
    console.log(`missionId: ${result.missionId || 'n/a'}`);
    console.log(`workers: ${result.metrics?.workers?.join(', ') || result.workerOutputs.join(', ') || 'none'}`);
    if (result.metrics) {
      console.log(`typeCounts: ${JSON.stringify(result.metrics.targetTypeCounts)}`);
      console.log(`readCoverageRate: ${(result.metrics.readCoverageRate * 100).toFixed(1)}%`);
    }
    if (result.error) {
      console.log(`error: ${result.error.message}`);
    }

    if (!result.error && workerCount >= 2 && hasTargetFacts) {
      chosen = result;
      break;
    }
  }

  const latestWithMetrics = [...attempts].reverse().find((item) => item.metrics);
  const finalResult = chosen || latestWithMetrics || attempts[attempts.length - 1];
  if (!finalResult) {
    console.error('未执行任何回归尝试。');
    process.exit(1);
  }

  console.log('\n=== SharedContextPool 指标 ===');
  if (!finalResult.metrics) {
    console.log('未获取到指标（mission/context 未就绪）。');
    process.exit(finalResult.error ? 1 : 2);
  }

  const m = finalResult.metrics;
  console.log(`missionId: ${m.missionId}`);
  console.log(`assignments: ${m.assignments}`);
  console.log(`workers: ${m.workers.join(', ') || 'none'}`);
  console.log(`typeCounts: ${JSON.stringify(m.targetTypeCounts)}`);
  console.log(`totalEntries: ${m.totalTargetEntries}`);
  console.log(`writeDensityPerAssignment: ${m.writeDensityPerAssignment.toFixed(2)}`);
  console.log(`readCoverageRate: ${(m.readCoverageRate * 100).toFixed(1)}%`);
  console.log(`reuseRate(读取>1): ${(m.reuseRate * 100).toFixed(1)}%`);
  console.log(`avgReadTimes: ${m.avgReadTimes.toFixed(2)}`);
  console.log(`mergedSourceRate: ${(m.mergedSourceRate * 100).toFixed(1)}%`);

  if (finalResult.error) {
    process.exit(1);
  }

  if (m.workers.length < 2) {
    process.exit(2);
  }

  process.exit(0);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
