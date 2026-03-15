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
  assert(source.includes('private syncWorkerStatusToWebview(force: boolean, reason: string): void {'), '缺少 Worker 状态重放统一入口');
  assert(source.includes("this.syncWorkerStatusToWebview(false, 'webviewReady');"), 'webviewReady 未补发 Worker 状态');
  assert(source.includes("this.syncWorkerStatusToWebview(false, 'getState');"), 'getState 未补发 Worker 状态');
  assert(source.includes("this.syncWorkerStatusToWebview(false, 'requestState');"), 'requestState 未补发 Worker 状态');
}

function createHarness(WebviewProvider) {
  const calls = [];
  const provider = Object.create(WebviewProvider.prototype);

  provider.startupRecoveryPromise = null;
  provider.commandHandlers = [];
  provider.sendStateUpdate = () => { calls.push('state'); };
  provider.sendCurrentSessionToWebview = async () => { calls.push('session'); };
  provider.workerStatusService = {
    async sendWorkerStatus(force) {
      calls.push(`status:${String(force)}`);
    },
  };
  provider.sendToast = () => {};
  provider.shouldAwaitRuntimeInitialization = () => false;

  return { provider, calls };
}

async function testMessageReplaysWorkerStatus(WebviewProvider, messageType) {
  const { provider, calls } = createHarness(WebviewProvider);
  await provider.handleMessage({ type: messageType });
  assert(
    calls.join(',') === 'state,session,status:false',
    `${messageType} 未按预期补发 Worker 状态: ${calls.join(',')}`,
  );
}

async function main() {
  testSourceGuardrails();
  const { WebviewProvider } = loadCompiledModule(path.join('ui', 'webview-provider.js'));
  await testMessageReplaysWorkerStatus(WebviewProvider, 'webviewReady');
  await testMessageReplaysWorkerStatus(WebviewProvider, 'getState');
  await testMessageReplaysWorkerStatus(WebviewProvider, 'requestState');
  console.log('\n=== worker status webview ready regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'webview_ready_replays_worker_status',
      'get_state_replays_worker_status',
      'request_state_replays_worker_status',
    ],
  }, null, 2));
  Module._load = originalModuleLoad;
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('worker status webview ready 回归失败:', error?.stack || error);
  process.exit(1);
});
