#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function installBrowserEnv() {
  const listeners = new Map();
  global.$state = (value) => value;
  global.$derived = Object.assign((value) => value, {
    by: (factory) => factory(),
  });
  global.window = {
    __INITIAL_LOCALE__: 'zh-CN',
    __DEBUG_MODE__: false,
    addEventListener(type, listener) {
      const list = listeners.get(type) || [];
      list.push(listener);
      listeners.set(type, list);
    },
    removeEventListener(type, listener) {
      const list = listeners.get(type) || [];
      listeners.set(type, list.filter((item) => item !== listener));
    },
  };

  const storage = new Map();
  global.localStorage = {
    getItem(key) { return storage.has(key) ? storage.get(key) : null; },
    setItem(key, value) { storage.set(key, String(value)); },
    removeItem(key) { storage.delete(key); },
    clear() { storage.clear(); },
  };
  global.window.localStorage = global.localStorage;

  return {
    dispatch(data) {
      const handlers = listeners.get('message') || [];
      for (const handler of handlers) {
        handler({ data });
      }
    },
  };
}

function ensureCompiled(file) {
  if (!fs.existsSync(file)) {
    throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run -s compile`);
  }
}

async function main() {
  const browser = installBrowserEnv();
  const handlerPath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
  const storePath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
  const protocolPath = path.join(OUT, 'protocol', 'message-protocol.js');
  const sourcePath = path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts');
  for (const file of [handlerPath, storePath, protocolPath]) {
    ensureCompiled(file);
  }

  const source = fs.readFileSync(sourcePath, 'utf8');
  assert(source.includes('settleProcessingForManualInteraction();'), 'manual interaction 未统一结算请求运行态');

  const { initMessageHandler } = require(handlerPath);
  const {
    getState,
    messagesState,
    setAppState,
    setIsProcessing,
    addPendingRequest,
    markMessageActive,
    createRequestBinding,
    getRequestBinding,
    clearProcessingState,
    clearAllRequestBindings,
  } = require(storePath);
  const { createDataMessage } = require(protocolPath);

  clearAllRequestBindings();
  clearProcessingState();
  initMessageHandler();

  setAppState({ interactionMode: 'ask' });
  setIsProcessing(true);
  addPendingRequest('req-1');
  markMessageActive('msg-1');
  createRequestBinding({
    requestId: 'req-1',
    userMessageId: 'user-1',
    placeholderMessageId: 'msg-1',
    createdAt: Date.now(),
    timeoutId: setTimeout(() => {}, 60000),
  });

  browser.dispatch({
    type: 'unifiedMessage',
    message: createDataMessage('confirmationRequest', { formattedPlan: '请确认计划' }, 'session-interaction'),
  });

  const state = getState();
  assert(state.pendingConfirmation, 'ask 模式未建立 pendingConfirmation');
  assert(state.isProcessing === false, '进入人工交互等待态后 isProcessing 应为 false');
  assert(messagesState.pendingRequests.size === 0, '进入人工交互等待态后 pendingRequests 应清空');
  assert(messagesState.activeMessageIds.size === 0, '进入人工交互等待态后 activeMessageIds 应清空');
  assert(!getRequestBinding('req-1'), '进入人工交互等待态后 requestBinding 应清空');

  console.log('\n=== manual interaction processing settlement regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'manual_interaction_clears_processing_state',
      'manual_interaction_clears_pending_requests',
      'manual_interaction_clears_request_binding',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('manual interaction processing settlement 回归失败:', error?.stack || error);
  process.exit(1);
});
