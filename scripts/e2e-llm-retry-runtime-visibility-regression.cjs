#!/usr/bin/env node
/**
 * LLM retry runtime visible/silent 边界回归脚本
 *
 * 目标：
 * 1) 用户可见主请求会透出 llmRetryRuntime
 * 2) sendSilentMessage 不透出 llmRetryRuntime
 */

const fs = require('fs');
const path = require('path');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {},
        window: {},
        Uri: { file: (p) => ({ fsPath: p }) },
        Position: class Position { constructor(line, character) { this.line = line; this.character = character; } },
        Range: class Range { constructor(start, end) { this.start = start; this.end = end; } },
        Selection: class Selection { constructor(anchor, active) { this.anchor = anchor; this.active = active; } },
        RelativePattern: class RelativePattern { constructor(base, pattern) { this.baseUri = base; this.pattern = pattern; } },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

class FakeMessageHub {
  constructor() {
    this.traceId = 'session-retry-runtime';
    this.requestId = 'req-visible';
    this.requestMessageId = 'msg-visible';
    this.events = [];
  }
  getTraceId() { return this.traceId; }
  getRequestContext() { return this.requestId; }
  getRequestMessageId(requestId) { return requestId === this.requestId ? this.requestMessageId : undefined; }
  sendMessage(message) { this.events.push({ type: 'message', payload: message }); return true; }
  sendUpdate(update) { this.events.push({ type: 'update', payload: update }); return true; }
  data(type, payload) { this.events.push({ type, payload }); return true; }
}

async function main() {
  const workerAdapterPath = path.join(OUT, 'llm', 'adapters', 'worker-adapter.js');
  const normalizerPath = path.join(OUT, 'normalizer', 'base-normalizer.js');
  for (const file of [workerAdapterPath, normalizerPath]) {
    if (!fs.existsSync(file)) throw new Error(`缺少 out 编译产物: ${file}，请先执行 npm run compile`);
  }

  installVscodeStub();
  const { WorkerLLMAdapter } = require(workerAdapterPath);
  const { BaseNormalizer } = require(normalizerPath);

  class DummyNormalizer extends BaseNormalizer {
    constructor() { super({ agent: 'claude', defaultSource: 'assistant' }); }
    parseChunk() { return []; }
    finalizeContext() {}
    detectInteraction() { return null; }
  }

  const fakeClient = {
    streamCalls: [],
    sendCalls: [],
    async streamMessage(params, onChunk) {
      this.streamCalls.push(params);
      assert(typeof params.retryRuntimeHook === 'function', 'visible 主请求必须注入 retryRuntimeHook');
      params.retryRuntimeHook({ phase: 'scheduled', attempt: 2, maxAttempts: 6, delayMs: 25, nextRetryAt: Date.now() + 25 });
      params.retryRuntimeHook({ phase: 'attempt_started', attempt: 2, maxAttempts: 6 });
      onChunk({ type: 'content_delta', content: 'visible result' });
      params.retryRuntimeHook({ phase: 'settled', outcome: 'success' });
      return { content: 'visible result', toolCalls: [], usage: { inputTokens: 1, outputTokens: 1 } };
    },
    async sendMessage(params) {
      this.sendCalls.push(params);
      assert(params.retryRuntimeHook === undefined, 'silent 请求不应注入 retryRuntimeHook');
      return { content: 'silent result', toolCalls: [], usage: { inputTokens: 1, outputTokens: 1 } };
    },
  };

  const messageHub = new FakeMessageHub();
  const adapter = new WorkerLLMAdapter({
    client: fakeClient,
    normalizer: new DummyNormalizer(),
    toolManager: { getTools: async () => [], buildToolsSummary: async () => '' },
    config: { baseUrl: '', apiKey: 'test-key', model: 'test-model', provider: 'openai', enabled: true },
    messageHub,
    workerSlot: 'claude',
    systemPrompt: 'worker test prompt',
    profileLoader: { getProfile() { throw new Error('profileLoader 不应在该回归中被调用'); } },
    executionPolicy: { requestTimeoutMs: 1000, retryPolicy: { maxRetries: 6, baseDelayMs: 1, retryOnTimeout: true, retryOnAllErrors: true, retryDelaysMs: [25, 25], deterministicErrorStreakLimit: 3 } },
  });

  await adapter.connect();
  const visibleResult = await adapter.sendMessage('visible retry runtime');
  assert(visibleResult === 'visible result', `visible 响应异常: ${visibleResult}`);

  const visibleEvents = messageHub.events.filter((event) => event.type === 'llmRetryRuntime');
  assert(visibleEvents.length === 3, `visible 请求应发出 3 个 llmRetryRuntime 事件，实际: ${visibleEvents.length}`);
  assert(fakeClient.streamCalls.length === 1, `visible 请求应走 streamMessage 一次，实际: ${fakeClient.streamCalls.length}`);

  const firstPayload = visibleEvents[0].payload;
  assert(firstPayload.traceId === 'session-retry-runtime', `traceId 异常: ${firstPayload.traceId}`);
  assert(firstPayload.messageId === 'msg-visible', `messageId 异常: ${firstPayload.messageId}`);
  assert(firstPayload.agent === 'claude' && firstPayload.role === 'worker', `agent/role 异常: ${JSON.stringify(firstPayload)}`);
  assert(firstPayload.provider === 'openai' && firstPayload.model === 'test-model', `provider/model 异常: ${JSON.stringify(firstPayload)}`);

  const beforeSilent = visibleEvents.length;
  const silentResult = await adapter.sendSilentMessage('silent retry runtime');
  assert(silentResult === 'silent result', `silent 响应异常: ${silentResult}`);
  assert(fakeClient.sendCalls.length === 1, `silent 请求应走 sendMessage 一次，实际: ${fakeClient.sendCalls.length}`);

  const afterSilent = messageHub.events.filter((event) => event.type === 'llmRetryRuntime').length;
  assert(afterSilent === beforeSilent, `silent 请求不应新增 llmRetryRuntime，前=${beforeSilent} 后=${afterSilent}`);

  console.log(JSON.stringify({
    pass: true,
    visiblePhases: visibleEvents.map((event) => event.payload.phase),
    visibleMessageId: firstPayload.messageId,
    silentEventsAdded: afterSilent - beforeSilent,
  }, null, 2));
}

main().catch((error) => {
  console.error('llm retry runtime visible/silent 回归失败:', error?.stack || error);
  process.exit(1);
});