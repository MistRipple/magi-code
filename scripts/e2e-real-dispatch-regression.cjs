#!/usr/bin/env node
/**
 * 真实 LLM 全链路回归脚本（编排器 + 3 Worker + dispatch/wait 汇总）
 *
 * 目标：
 * 1. 按当前分工配置完成一轮多 Worker 分发（dispatch 3 个分类任务）
 * 2. 显式使用 requires_modification=false（只读任务）
 * 3. 验证最终任务视图与状态，防止回归到“读任务被按写任务治理”
 *
 * 运行：
 *   npm run compile
 *   node scripts/e2e-real-dispatch-regression.cjs
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');
const DEFAULT_TIMEOUT_MS = 180_000;
const EXECUTE_TIMEOUT_MS = Number(process.env.MAGI_E2E_EXEC_TIMEOUT_MS || 300_000);

function parseDeepTaskFlag() {
  const raw = String(process.env.MAGI_E2E_DEEP_TASK ?? '').trim().toLowerCase();
  if (!raw) return false;
  if (raw === '1' || raw === 'true' || raw === 'on' || raw === 'deep' || raw === 'project') return true;
  if (raw === '0' || raw === 'false' || raw === 'off' || raw === 'regular' || raw === 'feature') return false;
  return false;
}

function installVscodeStub(deepTaskEnabled) {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {
          workspaceFolders: [],
          getConfiguration: (section) => ({
            get: (key, defaultValue) => {
              const normalizedKey = typeof key === 'string' ? key : '';
              if ((section === 'magi' && normalizedKey === 'deepTask')
                || (!section && (normalizedKey === 'magi.deepTask' || normalizedKey === 'deepTask'))) {
                return deepTaskEnabled;
              }
              return defaultValue;
            },
            update: async () => undefined,
          }),
          fs: {
            stat: async () => ({}),
            readDirectory: async () => [],
            readFile: async () => Buffer.from(''),
          },
          findFiles: async () => [],
          openTextDocument: async () => ({
            uri: { fsPath: '', toString: () => '' },
            getText: () => '',
            positionAt: () => ({ line: 0, character: 0 }),
            lineAt: () => ({ text: '' }),
            languageId: 'typescript',
          }),
        },
        window: {
          createOutputChannel: () => ({ appendLine() {}, append() {}, clear() {}, show() {}, dispose() {} }),
          showErrorMessage: async () => undefined,
          showWarningMessage: async () => undefined,
          showInformationMessage: async () => undefined,
          onDidCloseTerminal: () => ({ dispose() {} }),
          onDidOpenTerminal: () => ({ dispose() {} }),
          createTerminal: () => ({ sendText() {}, show() {}, dispose() {} }),
          terminals: [],
          activeTextEditor: undefined,
          visibleTextEditors: [],
        },
        commands: {
          executeCommand: async () => undefined,
          registerCommand: () => ({ dispose() {} }),
        },
        languages: {
          getDiagnostics: () => [],
        },
        env: {
          shell: process.env.SHELL || '/bin/zsh',
          clipboard: {
            readText: async () => '',
            writeText: async () => {},
          },
        },
        Uri: {
          file: (p) => ({ fsPath: p, path: p, toString: () => p }),
          parse: (p) => ({ fsPath: p, path: p, toString: () => p }),
          joinPath: (...parts) => ({
            fsPath: parts.map(p => (typeof p === 'string' ? p : p.path || '')).join('/'),
            toString() { return this.fsPath; },
          }),
        },
        EventEmitter: class {
          constructor() {
            this.listeners = new Set();
            this.event = (listener) => {
              this.listeners.add(listener);
              return { dispose: () => this.listeners.delete(listener) };
            };
          }
          fire(data) {
            for (const listener of this.listeners) {
              try { listener(data); } catch {}
            }
          }
          dispose() { this.listeners.clear(); }
        },
        Disposable: class { dispose() {} },
        Position: class { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
        ViewColumn: { One: 1, Two: 2, Three: 3 },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

function withTimeout(promise, timeoutMs, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      setTimeout(() => reject(new Error(`${label} 超时 (${timeoutMs}ms)`)), timeoutMs);
    }),
  ]);
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'llm', 'adapter-factory.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const deepTaskEnabled = parseDeepTaskFlag();
  const modeLabel = deepTaskEnabled ? 'project' : 'feature';
  let exitCode = 0;

  installVscodeStub(deepTaskEnabled);

  const { LLMAdapterFactory } = require(path.join(OUT, 'llm', 'adapter-factory.js'));
  const { MissionDrivenEngine } = require(path.join(OUT, 'orchestrator', 'core', 'mission-driven-engine.js'));
  const { UnifiedSessionManager } = require(path.join(OUT, 'session', 'unified-session-manager.js'));
  const { SnapshotManager } = require(path.join(OUT, 'snapshot-manager.js'));
  const { WorkerAssignmentLoader } = require(path.join(OUT, 'orchestrator', 'profile', 'worker-assignments.js'));

  const sessionManager = new UnifiedSessionManager(ROOT);
  const snapshotManager = new SnapshotManager(sessionManager, ROOT);
  const assignmentLoader = new WorkerAssignmentLoader();
  const dispatchCategories = ['architecture', 'general', 'data_analysis'];
  const expectedWorkers = [...new Set(dispatchCategories.map(category => assignmentLoader.getWorkerForCategory(category)))];
  const adapterFactory = new LLMAdapterFactory({
    cwd: ROOT,
    workspaceFolders: [{ name: path.basename(ROOT), path: ROOT }],
  });

  const engine = new MissionDrivenEngine(
    adapterFactory,
    {
      timeout: DEFAULT_TIMEOUT_MS,
      maxRetries: 2,
      permissions: { allowEdit: false, allowBash: false, allowWeb: false },
      strategy: { enableVerification: false, enableRecovery: true, autoRollbackOnFailure: false },
    },
    ROOT,
    snapshotManager,
    sessionManager,
  );

  try {
    adapterFactory.setMessageHub(engine.getMessageHub());
    await withTimeout(adapterFactory.initialize(), 30_000, 'AdapterFactory.initialize');
    await withTimeout(engine.initialize(), 30_000, 'MissionDrivenEngine.initialize');
    const runtimeDeepTask = adapterFactory.isDeepTask();

    const session = sessionManager.createSession(`real-dispatch-regression-${Date.now()}`);
    const prompt = [
      '请严格执行以下流程，并且只执行一轮：',
      '1) 连续调用 3 次 worker_dispatch，且每次都必须包含 requires_modification 参数：',
      '- category=architecture, requires_modification=false：分析 src/orchestrator 编排链路，输出 2 条风险。',
      '- category=general, requires_modification=false：阅读 src/orchestrator/core/dispatch-manager.ts，输出 worker_dispatch 路由链路的 3 点摘要。',
      '- category=data_analysis, requires_modification=false：统计 src/orchestrator 下 .ts 文件数量并给结论。',
      '2) 调用一次 worker_wait 等待上述任务完成。',
      '3) 最后输出汇总结论，不要再追加任何 worker_dispatch。',
      '硬性约束：禁止修改文件、禁止执行命令、禁止联网。',
    ].join('\n');

    let result = '';
    let executeTimedOut = false;
    let executeError = null;
    try {
      result = await withTimeout(
        engine.execute(prompt, '', session.id),
        EXECUTE_TIMEOUT_MS,
        'MissionDrivenEngine.execute'
      );
    } catch (error) {
      const message = String(error?.message || error || '');
      if (message.includes('MissionDrivenEngine.execute 超时')) {
        executeTimedOut = true;
      } else {
        executeError = error;
      }
    }
    if (executeError) {
      throw executeError;
    }

    const taskViews = await engine.listTaskViews(session.id);
    const latest = taskViews.sort((a, b) => b.createdAt - a.createdAt)[0];
    const latestWithSubTasks = taskViews
      .filter(taskView => Array.isArray(taskView?.subTasks) && taskView.subTasks.length > 0)
      .sort((a, b) => b.createdAt - a.createdAt)[0];
    const selectedView = latestWithSubTasks ?? latest;
    const subTasks = selectedView?.subTasks || [];
    const workers = [...new Set(subTasks.map(task => task.assignedWorker))];
    const failedSubTasks = subTasks.filter(task => task.status === 'failed');
    const completedSubTasks = subTasks.filter(task => task.status === 'completed');
    const nonTerminalSubTasks = subTasks.filter(task => task.status === 'running' || task.status === 'pending');
    const targetWriteGuardFailures = subTasks.filter(task => {
      const outputJoined = (task.output || []).join('\n');
      const text = `${task.error || ''}\n${outputJoined}`;
      return text.includes('未检测到对目标文件的修改');
    });

    const finishedStatePass =
      Boolean(selectedView)
      && subTasks.length >= 3
      && completedSubTasks.length >= 2
      && nonTerminalSubTasks.length === 0
      && targetWriteGuardFailures.length === 0;

    const pass =
      finishedStatePass
      && runtimeDeepTask === deepTaskEnabled;

    console.log('\n=== 真实分发回归结果 ===');
    console.log(JSON.stringify({
      modeLabel,
      inputDeepTask: deepTaskEnabled,
      runtimeDeepTask,
      sessionId: session.id,
      taskViewCount: taskViews.length,
      latestTaskId: latest?.id || null,
      selectedTaskId: selectedView?.id || null,
      subTaskCount: subTasks.length,
      expectedWorkers,
      workers,
      subTaskStatuses: subTasks.map(task => `${task.assignedWorker}:${task.status}`),
      completedSubTaskCount: completedSubTasks.length,
      failedSubTaskCount: failedSubTasks.length,
      nonTerminalSubTaskCount: nonTerminalSubTasks.length,
      targetWriteGuardFailureCount: targetWriteGuardFailures.length,
      executeTimedOut,
      executeTimeoutMs: EXECUTE_TIMEOUT_MS,
      resultPreview: String(result || '').replace(/\s+/g, ' ').slice(0, 220),
      finishedStatePass,
      pass,
    }, null, 2));

    if (!pass) {
      exitCode = 2;
    }
  } catch (error) {
    console.error('真实回归失败:', error?.stack || error);
    exitCode = 1;
  } finally {
    await engine.dispose();
    await adapterFactory.shutdown();
    process.exit(exitCode);
  }
}

main();
