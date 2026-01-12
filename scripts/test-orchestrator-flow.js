#!/usr/bin/env node
/**
 * Headless orchestrator flow test (no UI).
 *
 * Usage:
 *   node scripts/test-orchestrator-flow.js --prompt "给我做一个登录页面"
 *   node scripts/test-orchestrator-flow.js --prompt "解释一下JWT是什么" --mode ask
 *   node scripts/test-orchestrator-flow.js --prompt "设计一个登录功能的接口契约" --mode plan
 *   node scripts/test-orchestrator-flow.js --prompt "实现登录API" --force-agent
 */

const path = require('path');
const Module = require('module');
const { execSync } = require('child_process');

const originalLoad = Module._load;
Module._load = function (request, parent, isMain) {
  if (request === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1 },
    };
  }
  return originalLoad.apply(this, arguments);
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

const defaultWorkspaceRoot = path.resolve(__dirname, '..');

function parseArgs(argv) {
  const args = {};
  for (let i = 2; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith('--')) continue;
    const key = token.slice(2);
    const next = argv[i + 1];
    if (!next || next.startsWith('--')) {
      args[key] = true;
    } else {
      args[key] = next;
      i += 1;
    }
  }
  return args;
}

function logSection(title) {
  console.log('\n' + '='.repeat(70));
  console.log(title);
  console.log('='.repeat(70));
}

function resolveCliPath(command) {
  try {
    const result = execSync(`command -v ${command}`, { stdio: ['ignore', 'pipe', 'ignore'] })
      .toString()
      .trim();
    return result || null;
  } catch {
    return null;
  }
}

async function checkCliAvailability() {
  const [claude, codex, gemini] = await Promise.all([
    ClaudeAdapter.checkInstalled(),
    CodexAdapter.checkInstalled(),
    GeminiAdapter.checkInstalled(),
  ]);
  return { claude, codex, gemini };
}

async function main() {
  const args = parseArgs(process.argv);
  const prompt = args.prompt || '给我做一个登录页面';
  const workspaceRoot = args.workspace
    ? path.resolve(args.workspace)
    : defaultWorkspaceRoot;
  const requestedMode = args.mode;
  const mode = requestedMode === 'ask' ? 'ask' : requestedMode === 'agent' || requestedMode === 'plan' ? 'agent' : null;
  const forcePlan = requestedMode === 'plan';
  const forceAgent = Boolean(args['force-agent']) || forcePlan;
  const planOnly = Boolean(args['plan-only']);

  logSection('MultiCLI Headless Orchestrator Test');
  console.log('workspace:', workspaceRoot);
  console.log('PATH:', process.env.PATH || '');
  console.log('prompt:', prompt);
  console.log('mode:', requestedMode || mode || 'auto');
  console.log('forceAgent:', forceAgent);
  console.log('forcePlan:', forcePlan);
  console.log('planOnly:', planOnly);

  const cliPaths = {
    claude: resolveCliPath('claude'),
    codex: resolveCliPath('codex'),
    gemini: resolveCliPath('gemini'),
  };
  console.log('cli paths:', cliPaths);

  const cliAvailability = await checkCliAvailability();
  console.log('cli availability:', cliAvailability);

  const env = { ...process.env };
  const sessionManager = new SessionManager(workspaceRoot);
  const taskManager = new TaskManager(sessionManager);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  const cliFactory = new CLIAdapterFactory({ cwd: workspaceRoot, env, cliPaths });
  cliFactory.on('error', ({ type, error }) => {
    console.error('[cli:error]', type, error?.message || error);
  });
  cliFactory.on('output', ({ type, chunk, source, adapterRole }) => {
    const text = String(chunk || '');
    if (!text.trim()) return;
    const prefix = `[cli:${type}][${adapterRole || 'worker'}][${source || 'unknown'}]`;
    console.log(prefix, text.slice(0, 400));
  });
  const orchestrator = new IntelligentOrchestrator(
    cliFactory,
    taskManager,
    snapshotManager,
    workspaceRoot
  );

  if (mode) orchestrator.setInteractionMode(mode);

  orchestrator.setConfirmationCallback(async (plan) => {
    console.log('\n[confirm] plan summary:', plan?.summary || '');
    return !planOnly;
  });
  orchestrator.setQuestionCallback(async (questions) => {
    console.log('\n[questions]');
    questions.forEach((q, idx) => {
      console.log(`  ${idx + 1}. ${q}`);
    });
    const answer = args.answer || [
      '场景：VSCode 扩展内的账号/API Key 管理，无独立后端服务。',
      '前端：基于当前 Webview/HTML + 现有样式体系。',
      '后端：无新增服务端，仅在扩展侧处理。',
      '认证方式：API Token 或 Key（不做 OAuth）。',
      '存储：VS Code secrets（必要时用 globalState 做标记）。',
      '功能范围：仅登录/绑定，不含注册、找回密码等。'
    ].join(' ');
    console.log('[answer]', answer);
    return answer;
  });

  globalEventBus.on('orchestrator:phase_changed', (event) => {
    const phase = event?.data?.phase || 'unknown';
    console.log('[phase]', phase);
  });
  globalEventBus.on('orchestrator:plan_ready', (event) => {
    const plan = event?.data?.plan;
    console.log('[plan] subTasks:', plan?.subTasks?.length || 0, 'mode:', plan?.executionMode || '');
  });
  globalEventBus.on('subtask:started', (event) => {
    console.log('[subtask:start]', event?.subTaskId || '');
  });
  globalEventBus.on('subtask:completed', (event) => {
    console.log('[subtask:done]', event?.subTaskId || '');
  });
  globalEventBus.on('subtask:failed', (event) => {
    console.log('[subtask:failed]', event?.subTaskId || '', event?.data?.error || '');
  });

  await orchestrator.initialize();

  const task = taskManager.createTask(prompt);
  const taskId = task.id;
  const finalPrompt = forceAgent ? `/task ${prompt}` : prompt;

  try {
    const result = await orchestrator.execute(finalPrompt, taskId, taskId);
    logSection('Final Result');
    console.log(result);
  } catch (error) {
    logSection('Execution Failed');
    console.error(error);
    process.exit(1);
  } finally {
    const plan = orchestrator.plan;
    if (plan) {
      logSection('Plan Snapshot');
      console.log(JSON.stringify({
        id: plan.id,
        needsWorker: plan.needsWorker,
        executionMode: plan.executionMode,
        subTasks: (plan.subTasks || []).map(t => ({
          id: t.id,
          worker: t.assignedWorker,
          kind: t.kind,
          deps: t.dependencies || [],
        })),
      }, null, 2));
    }
    orchestrator.dispose();
    await cliFactory.dispose();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
