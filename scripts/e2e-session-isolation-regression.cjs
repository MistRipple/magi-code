#!/usr/bin/env node
/**
 * Session 隔离链路回归脚本
 *
 * 目标：
 * 1. 验证 unifiedMessage 会按消息 trace/session 归属转发，而不是 activeSessionId。
 * 2. 验证 unifiedUpdate 在 message 之前到达时会缓冲，并在 message 到达后按正确 session 回放。
 * 3. 验证缺少 session 标识的消息会被丢弃，避免跨会话污染。
 *
 * 运行：
 *   npm run compile
 *   node scripts/e2e-session-isolation-regression.cjs
 */

const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function pickMessages(records, type) {
  return records.filter((record) => record.type === type);
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'ui', 'event-binding-service.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  const { MessageHub } = require(path.join(OUT, 'orchestrator', 'core', 'message-hub.js'));
  const { EventBindingService } = require(path.join(OUT, 'ui', 'event-binding-service.js'));
  const {
    createStandardMessage,
    MessageCategory,
    MessageType,
    MessageLifecycle,
  } = require(path.join(OUT, 'protocol', 'message-protocol.js'));

  const posted = [];
  let activeSessionId = 'session-ui-active';

  const fakeAdapterFactory = new EventEmitter();
  const messageHub = new MessageHub('session-bootstrap');

  const ctx = {
    getActiveSessionId: () => activeSessionId,
    getMessageHub: () => messageHub,
    getOrchestratorEngine: () => ({ running: false }),
    getAdapterFactory: () => fakeAdapterFactory,
    getMissionOrchestrator: () => undefined,
    getMessageIdToRequestId: () => new Map(),
    sendStateUpdate() {},
    sendData() {},
    sendToast() {},
    sendExecutionStats() {},
    sendOrchestratorMessage() {},
    appendLog() {},
    postMessage(message) {
      posted.push({
        type: message.type,
        sessionId: message.sessionId,
        messageId: message.message?.id || message.update?.messageId || '',
        updateType: message.update?.updateType || '',
      });
    },
    logMessageFlow() {},
    resolveRequestTimeoutFromMessage() {},
    clearRequestTimeout() {},
    interruptCurrentTask: async () => {},
    tryResumePendingRecovery() {},
  };

  const bindingService = new EventBindingService(ctx);
  bindingService.bindAll();

  // case-1: message 的 session 归属应使用 traceId（session-a），而非 activeSessionId（session-ui-active）
  const messageA = createStandardMessage({
    id: 'msg-session-a-1',
    traceId: 'session-a',
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.STARTED,
    blocks: [{ type: 'text', content: 'session-a-start' }],
    metadata: {},
  });
  messageHub.sendMessage(messageA);
  messageHub.sendUpdate({
    messageId: 'msg-session-a-1',
    updateType: 'append',
    appendText: ' delta',
    timestamp: Date.now(),
  });

  // case-2: update 先到达时应缓冲，message 到达后回放并保持 session-a
  messageHub.sendUpdate({
    messageId: 'msg-session-a-2',
    updateType: 'append',
    appendText: 'pre-message',
    timestamp: Date.now(),
  });
  activeSessionId = 'session-ui-after-switch';
  const messageA2 = createStandardMessage({
    id: 'msg-session-a-2',
    traceId: 'session-a',
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source: 'worker',
    agent: 'claude',
    lifecycle: MessageLifecycle.STARTED,
    blocks: [{ type: 'text', content: 'worker-start' }],
    metadata: {},
  });
  messageHub.sendMessage(messageA2);

  // case-3: 缺少 session 标识（traceId 为空且 metadata 无 sessionId）应被丢弃
  const droppedMessage = createStandardMessage({
    id: 'msg-no-session',
    traceId: '',
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: 'should-drop' }],
    metadata: {},
  });
  messageHub.sendMessage(droppedMessage);

  const unifiedMessages = pickMessages(posted, 'unifiedMessage');
  const unifiedUpdates = pickMessages(posted, 'unifiedUpdate');

  const msgARecord = unifiedMessages.find((item) => item.messageId === 'msg-session-a-1');
  assert(msgARecord, '未捕获 msg-session-a-1 的 unifiedMessage');
  assert(msgARecord.sessionId === 'session-a', `msg-session-a-1 会话归属错误: ${msgARecord.sessionId}`);

  const msgAUpdate = unifiedUpdates.find((item) => item.messageId === 'msg-session-a-1');
  assert(msgAUpdate, '未捕获 msg-session-a-1 的 unifiedUpdate');
  assert(msgAUpdate.sessionId === 'session-a', `msg-session-a-1 update 会话归属错误: ${msgAUpdate.sessionId}`);

  const msgA2Update = unifiedUpdates.find((item) => item.messageId === 'msg-session-a-2');
  assert(msgA2Update, '未捕获 msg-session-a-2 的缓冲回放 unifiedUpdate');
  assert(msgA2Update.sessionId === 'session-a', `msg-session-a-2 update 会话归属错误: ${msgA2Update.sessionId}`);

  const dropped = unifiedMessages.find((item) => item.messageId === 'msg-no-session');
  assert(!dropped, '缺少 session 标识的消息未被丢弃');

  bindingService.disposeToolAuthorization();

  console.log('\n=== session 隔离回归结果 ===');
  console.log(JSON.stringify({
    postedCount: posted.length,
    unifiedMessageCount: unifiedMessages.length,
    unifiedUpdateCount: unifiedUpdates.length,
    sessionARecords: posted.filter((item) => item.sessionId === 'session-a').length,
    droppedNoSessionMessage: !dropped,
    pass: true,
  }, null, 2));

  process.exit(0);
}

main().catch((error) => {
  console.error('session 隔离回归失败:', error?.stack || error);
  process.exit(1);
});
