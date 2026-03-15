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
  assert(source.includes("transitionKind === 'forced'"), 'message-handler 未按 forced idle 消费 processingStateChanged(false)');
  assert(!source.includes("case 'task_failed': {\n      // 任务生命周期终极信号：彻底清除所有处理态"), 'task_failed 仍把请求级结束当成系统级空闲');
  assert(source.includes("transitionKind === 'forced') {"), 'message-handler forced idle 分支缺失');

  const { initMessageHandler } = require(handlerPath);
  const {
    messagesState,
    clearProcessingState,
    setIsProcessing,
    markMessageActive,
  } = require(storePath);
  const { createDataMessage, createControlMessage, ControlMessageType } = require(protocolPath);

  clearProcessingState();
  initMessageHandler();

  const emitData = (payload) => {
    browser.dispatch({
      type: 'unifiedMessage',
      message: createDataMessage('processingStateChanged', payload, 'trace-processing-state'),
    });
  };

  const emitControl = (controlType, payload = {}) => {
    browser.dispatch({
      type: 'unifiedMessage',
      message: createControlMessage(controlType, payload, 'trace-processing-state'),
    });
  };

  setIsProcessing(true);
  markMessageActive('msg-streaming-1');
  assert(messagesState.isProcessing === true, '初始化 processing 状态失败');

  emitControl(ControlMessageType.TASK_COMPLETED, { requestId: 'req-1' });
  assert(messagesState.isProcessing === true, 'task_completed 不应直接清空系统处理态');

  emitData({ isProcessing: false, transitionKind: 'derived' });
  assert(messagesState.isProcessing === true, 'derived false 不应清空处理态');

  emitData({ isProcessing: false, transitionKind: 'forced' });
  assert(messagesState.isProcessing === false, 'forced false 应清空处理态');
  assert(messagesState.activeMessageIds.size === 0, 'forced false 应清空 activeMessageIds');
  assert(messagesState.pendingRequests.size === 0, 'forced false 应清空 pendingRequests');

  setIsProcessing(true);
  markMessageActive('msg-streaming-stale');
  emitData({ isProcessing: false, transitionKind: 'derived' });
  assert(messagesState.isProcessing === true, 'derived false 后残留处理态应保持，等待 authoritative idle');
  emitData({ isProcessing: false, transitionKind: 'forced' });
  assert(messagesState.isProcessing === false, '重复 forced false 仍应清空残留处理态');
  assert(messagesState.activeMessageIds.size === 0, '重复 forced false 仍应清空 activeMessageIds');

  console.log('\n=== processing state authoritative idle regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'task_completed_does_not_clear_system_processing',
      'derived_false_is_ignored',
      'forced_false_clears_processing_state',
      'repeated_forced_false_clears_residual_processing_state',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('processing state authoritative idle 回归失败:', error?.stack || error);
  process.exit(1);
});
