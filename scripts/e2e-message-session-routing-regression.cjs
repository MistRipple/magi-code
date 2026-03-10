#!/usr/bin/env node
/**
 * Message session routing regression
 *
 * 目标：
 * 1) metadata.sessionId 优先级最高
 * 2) 当 traceId 与 payload.sessionId 冲突时，必须以 traceId 为准
 * 3) 无 traceId 时，允许回退到 payload.sessionId
 */

const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function createStandardMessage(overrides = {}) {
  const now = Date.now();
  return {
    id: `msg_${Math.random().toString(36).slice(2, 10)}`,
    traceId: '',
    source: 'orchestrator',
    agent: 'orchestrator',
    type: 'result',
    category: 'data',
    lifecycle: 'completed',
    createdAt: now,
    updatedAt: now,
    data: {
      dataType: 'terminalStreamFrame',
      payload: {},
    },
    ...overrides,
  };
}

async function main() {
  const ROOT = path.resolve(__dirname, '..');
  const OUT = path.join(ROOT, 'out');
  const svcPath = path.join(OUT, 'ui', 'event-binding-service.js');
  if (!fs.existsSync(svcPath)) {
    throw new Error('缺少 out/ui/event-binding-service.js，请先执行 npm run compile');
  }

  const { EventBindingService } = require(svcPath);

  class FakeMessageHub extends EventEmitter {
    phaseChange() {}
    workerStatus() {}
    sendMessage() {}
  }

  class FakeAdapterFactory extends EventEmitter {}

  const messageHub = new FakeMessageHub();
  const adapterFactory = new FakeAdapterFactory();
  const posted = [];

  const service = new EventBindingService({
    getActiveSessionId: () => 'session-active',
    getMessageHub: () => messageHub,
    getOrchestratorEngine: () => ({ running: false }),
    getAdapterFactory: () => adapterFactory,
    getMissionOrchestrator: () => undefined,
    getMessageIdToRequestId: () => new Map(),
    sendStateUpdate: () => {},
    sendData: () => {},
    sendToast: () => {},
    sendExecutionStats: () => {},
    sendOrchestratorMessage: () => {},
    appendLog: () => {},
    postMessage: (message) => {
      posted.push(message);
    },
    logMessageFlow: () => {},
    resolveRequestTimeoutFromMessage: () => {},
    clearRequestTimeout: () => {},
    interruptCurrentTask: async () => {},
    tryResumePendingRecovery: () => {},
  });

  service.bindAll();

  // Case 1: metadata.sessionId > traceId > payload.sessionId
  posted.length = 0;
  messageHub.emit('unified:message', createStandardMessage({
    traceId: 'session-trace',
    metadata: { sessionId: 'session-meta' },
    data: {
      dataType: 'terminalStreamFrame',
      payload: { sessionId: 'session-payload' },
    },
  }));
  assert(posted.length === 1, 'Case1 未投递消息');
  assert(posted[0].sessionId === 'session-meta', `Case1 期望 session-meta，实际=${posted[0].sessionId}`);

  // Case 2: traceId 与 payload.sessionId 冲突时，必须使用 traceId
  posted.length = 0;
  messageHub.emit('unified:message', createStandardMessage({
    traceId: 'session-trace',
    data: {
      dataType: 'terminalStreamFrame',
      payload: { sessionId: 'session-stale' },
    },
  }));
  assert(posted.length === 1, 'Case2 未投递消息');
  assert(posted[0].sessionId === 'session-trace', `Case2 期望 session-trace，实际=${posted[0].sessionId}`);

  // Case 3: 无 traceId 时回退 payload.sessionId
  posted.length = 0;
  messageHub.emit('unified:message', createStandardMessage({
    traceId: '',
    data: {
      dataType: 'terminalStreamFrame',
      payload: { sessionId: 'session-payload-only' },
    },
  }));
  assert(posted.length === 1, 'Case3 未投递消息');
  assert(
    posted[0].sessionId === 'session-payload-only',
    `Case3 期望 session-payload-only，实际=${posted[0].sessionId}`,
  );

  console.log(JSON.stringify({
    pass: true,
    cases: 3,
  }, null, 2));
}

main().catch((error) => {
  console.error('message session routing 回归失败:', error?.stack || error);
  process.exit(1);
});
