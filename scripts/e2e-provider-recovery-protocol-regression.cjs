#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

const originalModuleLoad = Module._load;
Module._load = function patchedModuleLoad(request, parent, isMain) {
  if (request === 'vscode') {
    return {
      workspace: {
        getConfiguration() {
          return {
            get(_key, fallback) { return fallback; },
            update() { return Promise.resolve(); },
          };
        },
      },
      ConfigurationTarget: { Global: 1 },
      Uri: {
        file(filePath) { return { fsPath: filePath, path: filePath, toString() { return filePath; } }; },
        joinPath(base, ...parts) {
          const basePath = base && typeof base.path === 'string' ? base.path : '';
          const resolved = path.join(basePath, ...parts);
          return { fsPath: resolved, path: resolved, toString() { return resolved; } };
        },
      },
      window: {},
      commands: { executeCommand() { return Promise.resolve(); } },
    };
  }
  return originalModuleLoad.call(this, request, parent, isMain);
};

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function testSourceGuardrails() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'ui', 'webview-provider.ts'), 'utf8');
  assert(source.includes('private pendingRecoveryContext: PendingRecoveryContext | null = null;'), 'pendingRecoveryContext 未建立单一恢复上下文');
  assert(source.includes("await this.resumeInterruptedTask({ taskId: message.taskId });"), 'resumeTask 仍未消费显式 taskId');
  assert(source.includes("this.sendData('recoveryRequest', {"), 'provider 未发送 recoveryRequest');
  assert(source.includes('const executionStatus = this.orchestratorEngine.getLastExecutionStatus();'), 'executeWithOrchestrator 未消费 engine 正式执行事实');
  assert(!source.includes('private async getLastInterruptedTask()'), '仍残留最近 cancelled 任务扫描语义');
}

function createHarness(WebviewProvider) {
  const sentData = [];
  const toasts = [];
  const messages = [];
  const executions = [];
  const provider = Object.create(WebviewProvider.prototype);

  provider.orchestratorEngine = { running: false };
  provider.snapshotManager = {
    getPendingChanges() {
      return [{ missionId: 'mission-1', filePath: 'src/app.ts', additions: 3, deletions: 1 }];
    },
  };
  provider.pendingRecoveryContext = null;
  provider.pendingRecoveryRetry = false;
  provider.pendingRecoveryPrompt = null;
  provider.sendData = (type, payload) => sentData.push({ type, payload });
  provider.sendToast = (message, level) => toasts.push({ message, level });
  provider.sendOrchestratorMessage = (payload) => messages.push(payload);
  provider.executeTask = async (...args) => { executions.push(args); };

  return { provider, sentData, toasts, messages, executions };
}

function createExecutionResult(overrides = {}) {
  return {
    success: false,
    taskId: 'mission-1',
    runtimeReason: 'interrupted',
    errors: [],
    recoverable: true,
    error: '任务已中断',
    ...overrides,
  };
}

function testInterruptedRecoveryRequest(WebviewProvider) {
  const { provider, sentData } = createHarness(WebviewProvider);
  provider.setPendingRecoveryFromExecution({
    result: createExecutionResult(),
    prompt: '修复 deep 续航问题',
    sessionId: 'session-1',
  });

  assert(provider.pendingRecoveryContext, 'recoverable 执行后未建立 pendingRecoveryContext');
  assert(provider.pendingRecoveryContext.taskId === 'mission-1', 'pendingRecoveryContext.taskId 异常');
  assert(sentData.length === 1 && sentData[0].type === 'recoveryRequest', `recoveryRequest 未发送: ${JSON.stringify(sentData)}`);
  assert(sentData[0].payload.taskId === 'mission-1', 'recoveryRequest.taskId 异常');
  assert(sentData[0].payload.canRetry === true, 'recoverable 执行必须允许 retry');
  assert(sentData[0].payload.canRollback === true, '存在变更时 recoveryRequest 必须允许 rollback');
}

function testCancelledDoesNotCreateRecovery(WebviewProvider) {
  const { provider, sentData } = createHarness(WebviewProvider);
  provider.setPendingRecoveryFromExecution({
    result: createExecutionResult({
      runtimeReason: 'cancelled',
      recoverable: false,
      error: '用户取消',
    }),
    prompt: '修复 deep 续航问题',
    sessionId: 'session-1',
  });

  assert(provider.pendingRecoveryContext === null, 'cancelled 不应建立恢复上下文');
  assert(sentData.length === 0, 'cancelled 不应发送 recoveryRequest');
}

async function testResumeConsumesPendingContext(WebviewProvider) {
  const { provider, executions, messages } = createHarness(WebviewProvider);
  provider.pendingRecoveryContext = {
    taskId: 'mission-1',
    prompt: '修复 deep 续航问题',
    sessionId: 'session-1',
    runtimeReason: 'interrupted',
    errors: [],
    canRetry: true,
    canRollback: false,
  };

  await provider.resumeInterruptedTask({ taskId: 'mission-1', extraInstruction: '请继续完成剩余步骤' });

  assert(messages.length === 1 && messages[0].metadata.phase === 'resuming', '恢复执行前未发送 resuming 进度消息');
  assert(executions.length === 1, 'resumeInterruptedTask 未消费 pendingRecoveryContext 执行恢复');
  const options = executions[0][5];
  assert(options.resumeMissionId === 'mission-1', '恢复执行未透传 resumeMissionId');
  assert(options.recoveryBasePrompt === '修复 deep 续航问题', '恢复执行未保留原始恢复提示源');
}

async function testResumeRejectsMismatchedTask(WebviewProvider) {
  const { provider, executions, toasts } = createHarness(WebviewProvider);
  provider.pendingRecoveryContext = {
    taskId: 'mission-1',
    prompt: '修复 deep 续航问题',
    sessionId: 'session-1',
    runtimeReason: 'interrupted',
    errors: [],
    canRetry: true,
    canRollback: false,
  };

  await provider.resumeInterruptedTask({ taskId: 'mission-2' });

  assert(executions.length === 0, 'taskId 不匹配时不应恢复其他任务');
  assert(toasts.some((item) => item.message === '没有可恢复的任务'), 'taskId 不匹配时应提示没有可恢复任务');
}

async function main() {
  testSourceGuardrails();
  const { WebviewProvider } = loadCompiledModule(path.join('ui', 'webview-provider.js'));
  testInterruptedRecoveryRequest(WebviewProvider);
  testCancelledDoesNotCreateRecovery(WebviewProvider);
  await testResumeConsumesPendingContext(WebviewProvider);
  await testResumeRejectsMismatchedTask(WebviewProvider);
  console.log('\n=== provider recovery protocol regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'recoverable_execution_emits_recovery_request',
      'cancelled_does_not_emit_recovery_request',
      'resume_consumes_pending_recovery_context',
      'resume_taskid_mismatch_fail_closed',
    ],
  }, null, 2));
  Module._load = originalModuleLoad;
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('provider recovery protocol 回归失败:', error?.stack || error);
  process.exit(1);
});