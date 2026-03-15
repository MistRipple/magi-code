#!/usr/bin/env node
/**
 * Message stream binding regression
 *
 * 目标：
 * 1) 普通文本流更新不能仅凭旧 cardId 回写到中间历史消息
 * 2) placeholder 被真实消息替换时，如 synthetic 实体已存在，必须收敛为单一节点
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
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

function createUiTextMessage(id, content, metadata = {}) {
  return {
    id,
    role: 'assistant',
    source: 'orchestrator',
    content,
    blocks: content ? [{ type: 'text', content }] : [],
    timestamp: Date.now(),
    isStreaming: false,
    isComplete: true,
    type: 'text',
    metadata,
  };
}

function verifySourceGuardrails() {
  const handlerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts'),
    'utf8',
  );
  const storeSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.ts'),
    'utf8',
  );

  assert(handlerSource.includes('function isStableCardMessage(message: Message | undefined): boolean {'), 'message-handler 缺少稳定卡片判定函数');
  assert(handlerSource.includes("return message.type === 'task_card' || message.type === 'instruction';"), 'message-handler 未将 cardId 恢复限制到稳定卡片实体');
  assert(handlerSource.includes('const replacementBase = existingRealMessage && hasRenderableContent(existingRealMessage)'), 'message-handler 未合并 synthetic/placeholder 实体');
  assert(handlerSource.includes('replaceThreadMessage(placeholderId, newMessage);'), 'message-handler 未通过 replaceThreadMessage 收敛 placeholder 替换');
  assert(storeSource.includes('const filtered = messagesState.threadMessages.filter((_, i) => i !== conflictIndex);'), 'messages store 未在 replaceThreadMessage 中清理冲突 synthetic 节点');
}

function assertMessageOrder(messages, expectedIds, label) {
  const actualIds = messages.map((message) => message.id);
  assert(
    JSON.stringify(actualIds) === JSON.stringify(expectedIds),
    `${label} 顺序异常: expected=${JSON.stringify(expectedIds)} actual=${JSON.stringify(actualIds)}`,
  );
}

async function main() {
  const browser = installBrowserEnv();
  const handlerPath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.js');
  const storePath = path.join(OUT, 'ui', 'webview-svelte', 'src', 'stores', 'messages.svelte.js');
  const protocolPath = path.join(OUT, 'protocol', 'message-protocol.js');
  for (const file of [handlerPath, storePath, protocolPath]) {
    ensureCompiled(file);
  }

  verifySourceGuardrails();

  const { initMessageHandler } = require(handlerPath);
  const {
    clearAllMessages,
    clearAllRequestBindings,
    clearProcessingState,
    createRequestBinding,
    getState,
    setThreadMessages,
  } = require(storePath);
  const { createStreamingMessage } = require(protocolPath);

  clearAllMessages();
  clearAllRequestBindings();
  clearProcessingState();
  initMessageHandler();

  // Case 1: 普通文本流更新不能仅凭旧 cardId 回写到历史消息
  setThreadMessages([
    createUiTextMessage('msg-top', 'top content'),
    createUiTextMessage('old-msg', 'old middle content', { cardId: 'shared-card' }),
    createUiTextMessage('msg-tail', 'tail content'),
  ]);

  browser.dispatch({
    type: 'unifiedUpdate',
    update: {
      messageId: 'new-msg',
      cardId: 'shared-card',
      updateType: 'append',
      appendText: 'new stream text',
      timestamp: Date.now(),
    },
  });

  let state = getState();
  assertMessageOrder(state.threadMessages, ['msg-top', 'old-msg', 'msg-tail', 'new-msg'], 'Case1');
  const oldMessage = state.threadMessages.find((message) => message.id === 'old-msg');
  const newMessage = state.threadMessages.find((message) => message.id === 'new-msg');
  assert(oldMessage && oldMessage.content === 'old middle content', `Case1 历史消息被错误改写: ${oldMessage && oldMessage.content}`);
  assert(newMessage, 'Case1 未创建新流式消息');
  assert(newMessage.content === 'new stream text', `Case1 新流式消息内容异常: ${newMessage.content}`);
  assert(newMessage.isStreaming === true, 'Case1 新流式消息应保持 streaming');

  // Case 2: placeholder 被真实消息替换时必须收敛为单一节点
  clearAllMessages();
  clearAllRequestBindings();
  clearProcessingState();
  setThreadMessages([
    createUiTextMessage('placeholder-1', '正在思考...', {
      requestId: 'req-1',
      isPlaceholder: true,
      userMessageId: 'user-1',
      placeholderState: 'thinking',
    }),
  ]);
  createRequestBinding({
    requestId: 'req-1',
    userMessageId: 'user-1',
    placeholderMessageId: 'placeholder-1',
    createdAt: Date.now(),
  });

  browser.dispatch({
    type: 'unifiedUpdate',
    update: {
      messageId: 'real-1',
      updateType: 'append',
      appendText: 'partial stream',
      timestamp: Date.now(),
    },
  });

  state = getState();
  assert(state.threadMessages.filter((message) => message.id === 'real-1').length === 1, 'Case2 提前流更新后应只创建一个 synthetic 实体');
  assert(state.threadMessages.find((message) => message.id === 'real-1')?.content === 'partial stream', 'Case2 synthetic 实体未保留流式文本');

  browser.dispatch({
    type: 'unifiedMessage',
    message: createStreamingMessage('orchestrator', 'orchestrator', 'session-stream-binding', {
      id: 'real-1',
      metadata: { requestId: 'req-1' },
    }),
  });

  state = getState();
  const realMessages = state.threadMessages.filter((message) => message.id === 'real-1');
  assert(realMessages.length === 1, `Case2 真实消息接入后应只保留一个 real-1，实际=${realMessages.length}`);
  assert(!state.threadMessages.some((message) => message.id === 'placeholder-1'), 'Case2 placeholder 未被清理');
  assert(realMessages[0].content === 'partial stream', `Case2 流式内容丢失: ${realMessages[0].content}`);
  assert(realMessages[0].metadata?.requestId === 'req-1', 'Case2 真实消息未保留 requestId 绑定');

  console.log('\n=== message stream binding regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'append_update_does_not_rebind_plain_text_by_card_id',
      'placeholder_replacement_merges_synthetic_real_message',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('message stream binding 回归失败:', error?.stack || error);
  process.exit(1);
});
