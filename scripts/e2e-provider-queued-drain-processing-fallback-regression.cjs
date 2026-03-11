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
  if (!condition) throw new Error(message);
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

async function main() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'ui', 'webview-provider.ts'), 'utf8');
  assert(source.includes('|| this.queuedMessagesDrainRunning'), 'processing fallback 未将 queuedMessagesDrainRunning 纳入忙碌条件');

  const { WebviewProvider } = loadCompiledModule(path.join('ui', 'webview-provider.js'));
  const provider = Object.create(WebviewProvider.prototype);
  let forcedFalseCount = 0;

  provider.processingResetFallbackTimer = null;
  provider.orchestratorEngine = { running: false };
  provider.orchestratorQueueRunning = false;
  provider.pendingExecutionQueue = [];
  provider.queuedMessagesDrainRunning = true;
  provider.messageHub = {
    getProcessingState() {
      return { isProcessing: true, source: 'orchestrator', agent: 'orchestrator' };
    },
    forceProcessingState(isProcessing) {
      if (isProcessing === false) {
        forcedFalseCount += 1;
      }
    },
  };
  provider.cancelProcessingResetFallback = WebviewProvider.prototype.cancelProcessingResetFallback;
  provider.scheduleProcessingResetFallback = WebviewProvider.prototype.scheduleProcessingResetFallback;

  provider.scheduleProcessingResetFallback('queued-drain-regression');
  await new Promise((resolve) => setTimeout(resolve, 1700));

  assert(forcedFalseCount === 0, `queued drain 过程中不应 force idle，实际次数: ${forcedFalseCount}`);

  provider.queuedMessagesDrainRunning = false;
  provider.scheduleProcessingResetFallback('queued-drain-regression-idle');
  await new Promise((resolve) => setTimeout(resolve, 1700));
  assert(forcedFalseCount === 1, `真正空闲时应触发一次 force idle，实际次数: ${forcedFalseCount}`);

  Module._load = originalModuleLoad;
  console.log('\n=== provider queued drain processing fallback regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'queued_drain_blocks_force_idle_fallback',
      'real_idle_allows_force_idle_fallback',
    ],
  }, null, 2));
  process.exit(0);
}

main().catch((error) => {
  Module._load = originalModuleLoad;
  console.error('provider queued drain processing fallback 回归失败:', error?.stack || error);
  process.exit(1);
});