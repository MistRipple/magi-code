#!/usr/bin/env node
/**
 * LLM retry runtime UI routing 回归脚本
 *
 * 目标：
 * 1) scheduled 写入 store
 * 2) attempt_started 覆盖当前状态
 * 3) settled 只清理对应 messageId
 */

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
  global.$derived = (value) => value;
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

async function main() {
  const browser = installBrowserEnv();
  const handlerPath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
  const storePath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
  const protocolPath = path.join(OUT, 'protocol', 'message-protocol.js');
  for (const file of [handlerPath, storePath, protocolPath]) {
    if (!fs.existsSync(file)) throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run compile`);
  }

  const { initMessageHandler } = require(handlerPath);
  const { retryRuntimeState, clearAllRetryRuntime } = require(storePath);
  const { createDataMessage } = require(protocolPath);

  clearAllRetryRuntime();
  initMessageHandler();

  const emitRuntime = (payload) => {
    browser.dispatch({
      type: 'unifiedMessage',
      message: createDataMessage('llmRetryRuntime', payload, 'session-ui-retry-runtime'),
    });
  };

  const now = Date.now();
  emitRuntime({ messageId: 'msg-1', phase: 'scheduled', attempt: 2, maxAttempts: 6, delayMs: 3000, nextRetryAt: now + 3000 });
  let runtime = retryRuntimeState.byMessageId.get('msg-1');
  assert(runtime && runtime.phase === 'scheduled', 'scheduled 未写入 msg-1 runtime');
  assert(runtime.delayMs === 3000 && runtime.nextRetryAt === now + 3000, `scheduled 字段异常: ${JSON.stringify(runtime)}`);

  emitRuntime({ messageId: 'msg-2', phase: 'scheduled', attempt: 3, maxAttempts: 6, delayMs: 5000, nextRetryAt: now + 5000 });
  assert(retryRuntimeState.byMessageId.size === 2, `应同时跟踪两个 messageId，实际: ${retryRuntimeState.byMessageId.size}`);

  emitRuntime({ messageId: 'msg-1', phase: 'attempt_started', attempt: 2, maxAttempts: 6 });
  runtime = retryRuntimeState.byMessageId.get('msg-1');
  assert(runtime && runtime.phase === 'attempt_started', 'attempt_started 未覆盖 msg-1 runtime');
  assert(runtime.delayMs === undefined && runtime.nextRetryAt === undefined, `attempt_started 不应保留 scheduled 字段: ${JSON.stringify(runtime)}`);
  assert(retryRuntimeState.byMessageId.get('msg-2')?.phase === 'scheduled', 'msg-2 runtime 不应被 msg-1 覆盖');

  emitRuntime({ messageId: 'msg-1', phase: 'settled', outcome: 'success' });
  assert(!retryRuntimeState.byMessageId.has('msg-1'), 'settled 未清理 msg-1 runtime');
  assert(retryRuntimeState.byMessageId.has('msg-2'), 'settled 不应清理其他 messageId');

  emitRuntime({ messageId: 'msg-2', phase: 'settled', outcome: 'failed' });
  assert(retryRuntimeState.byMessageId.size === 0, `全部 settled 后 runtime 应清空，实际: ${retryRuntimeState.byMessageId.size}`);

  console.log(JSON.stringify({ pass: true, remaining: retryRuntimeState.byMessageId.size }, null, 2));
}

main().catch((error) => {
  console.error('llm retry runtime UI routing 回归失败:', error?.stack || error);
  process.exit(1);
});