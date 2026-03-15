#!/usr/bin/env node
/**
 * 轮级流中断自动续跑回归脚本（provider-agnostic）
 *
 * 覆盖目标：
 * 1) Orchestrator 工具模式下，首轮流式中断后可自动续跑
 * 2) Worker 工具循环下，首轮流式中断后可自动续跑
 * 3) 续跑前将首轮已输出文本写回历史，并注入“继续输出”系统提示
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

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

class MockNormalizer extends EventEmitter {
  constructor() {
    super();
    this.seq = 0;
  }

  startStream(traceId, _initialContent, messageId, visibility) {
    const id = messageId || `stream-${++this.seq}`;
    this.emit('message', {
      id,
      traceId,
      visibility,
      role: 'assistant',
      content: '',
      status: 'streaming',
    });
    return id;
  }

  processTextDelta(streamId, content) {
    this.emit('update', {
      messageId: streamId,
      appendText: content,
      tokenUsage: undefined,
    });
  }

  processThinking(streamId, thinking) {
    this.emit('update', {
      messageId: streamId,
      thinking,
    });
  }

  endStream(streamId, errorMessage) {
    if (errorMessage) {
      this.emit('error', { messageId: streamId, error: errorMessage });
      return;
    }
    this.emit('complete', streamId, {
      id: streamId,
      messageId: streamId,
      status: 'completed',
    });
  }
}

function createMessageHub(traceId) {
  const sentMessages = [];
  const sentUpdates = [];
  const dataEvents = [];
  return {
    sentMessages,
    sentUpdates,
    dataEvents,
    hub: {
      sendMessage(message) {
        sentMessages.push(message);
      },
      sendUpdate(update) {
        sentUpdates.push(update);
      },
      data(type, payload) {
        dataEvents.push({ type, payload });
      },
      workerOutput(worker, content, options) {
        sentMessages.push({
          type: 'workerOutput',
          worker,
          content,
          metadata: options?.metadata,
        });
      },
      getTraceId() {
        return traceId;
      },
      getRequestContext() {
        return undefined;
      },
      getRequestMessageId() {
        return undefined;
      },
    },
  };
}

function createToolManagerStub() {
  let executeCount = 0;
  return {
    get executeCount() {
      return executeCount;
    },
    async getTools() {
      return [];
    },
    async buildToolsSummary() {
      return '';
    },
    async execute() {
      executeCount += 1;
      throw new Error('不应执行工具');
    },
    getSnapshotContext() {
      return undefined;
    },
    getTodos() {
      return [];
    },
    updateSnapshotTodoId() {
      // noop
    },
  };
}

function createInterruptedThenRecoveredClient(firstDelta, secondDelta) {
  let attempts = 0;
  const messageSnapshots = [];

  return {
    get attempts() {
      return attempts;
    },
    get messageSnapshots() {
      return messageSnapshots;
    },
    async sendMessage() {
      throw new Error('sendMessage should not be called in this regression');
    },
    async streamMessage(params, onChunk) {
      attempts += 1;
      messageSnapshots.push(params.messages);
      if (attempts === 1) {
        onChunk({ type: 'content_delta', content: firstDelta });
        const error = new Error('fetch failed');
        error.code = 'ECONNRESET';
        throw error;
      }
      if (attempts === 2) {
        const hasRecoveryPrompt = params.messages.some(
          (msg) => msg.role === 'user'
            && typeof msg.content === 'string'
            && msg.content.includes('已自动续跑'),
        );
        const hasRecoveredAssistantPartial = params.messages.some(
          (msg) => msg.role === 'assistant'
            && typeof msg.content === 'string'
            && msg.content.includes(firstDelta.trim()),
        );
        assert(hasRecoveryPrompt, '续跑轮应包含自动续跑系统提示');
        assert(hasRecoveredAssistantPartial, '续跑轮应包含首轮已输出文本');
        onChunk({ type: 'content_delta', content: secondDelta });
        return {
          content: secondDelta,
          toolCalls: [],
          usage: {
            inputTokens: 12,
            outputTokens: 4,
          },
        };
      }
      throw new Error(`unexpected stream attempt: ${attempts}`);
    },
  };
}

function createToolSignalInterruptedThenRecoveredClient(secondRoundText) {
  let attempts = 0;
  const messageSnapshots = [];

  return {
    get attempts() {
      return attempts;
    },
    get messageSnapshots() {
      return messageSnapshots;
    },
    async sendMessage() {
      throw new Error('sendMessage should not be called in this regression');
    },
    async streamMessage(params, onChunk) {
      attempts += 1;
      messageSnapshots.push(params.messages);
      if (attempts === 1) {
        onChunk({
          type: 'tool_call_start',
          toolCall: {
            id: 'tc_recover_1',
            name: 'read_file',
            arguments: { path: 'README.md' },
          },
        });
        const error = new Error('fetch failed');
        error.code = 'ECONNRESET';
        throw error;
      }
      if (attempts === 2) {
        const hasToolRecoveryPrompt = params.messages.some(
          (msg) => msg.role === 'user'
            && typeof msg.content === 'string'
            && msg.content.includes('工具调用阶段因网络波动中断'),
        );
        assert(hasToolRecoveryPrompt, '工具信号中断续跑轮应包含工具阶段续跑提示');
        onChunk({ type: 'content_delta', content: secondRoundText });
        return {
          content: secondRoundText,
          toolCalls: [],
          usage: {
            inputTokens: 9,
            outputTokens: 3,
          },
        };
      }
      throw new Error(`unexpected stream attempt: ${attempts}`);
    },
  };
}

async function testOrchestratorRoundRecovery() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));

  const normalizer = new MockNormalizer();
  const messageHub = createMessageHub('trace-orchestrator');
  const toolManager = createToolManagerStub();
  const client = createInterruptedThenRecoveredClient('前半句，', '续跑完成。');

  const adapter = new OrchestratorLLMAdapter({
    client,
    normalizer,
    toolManager,
    config: {
      provider: 'anthropic',
      model: 'claude-sonnet-4',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    messageHub: messageHub.hub,
    systemPrompt: 'orchestrator-system',
  });

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const result = await adapter.sendMessage('round recovery orchestrator');
  const history = adapter.getHistory();

  assert(client.attempts === 2, `Orchestrator 应自动续跑 1 次，实际请求次数: ${client.attempts}`);
  assert(result.includes('续跑完成'), `Orchestrator 最终返回异常: ${result}`);
  assert(
    history.some((msg) => msg.role === 'assistant' && typeof msg.content === 'string' && msg.content.includes('前半句')),
    'Orchestrator 历史缺少首轮已输出文本',
  );
  assert(
    history.some((msg) => msg.role === 'user' && typeof msg.content === 'string' && msg.content.includes('已自动续跑')),
    'Orchestrator 历史缺少续跑提示',
  );
  assert(toolManager.executeCount === 0, 'Orchestrator 本用例不应执行任何工具');
}

async function testOrchestratorPlainRoundRecovery() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));

  const normalizer = new MockNormalizer();
  const messageHub = createMessageHub('trace-orchestrator-plain');
  const toolManager = createToolManagerStub();
  const client = createInterruptedThenRecoveredClient('普通模式前半句，', '普通模式续跑完成。');

  const adapter = new OrchestratorLLMAdapter({
    client,
    normalizer,
    toolManager,
    config: {
      provider: 'anthropic',
      model: 'claude-sonnet-4',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    messageHub: messageHub.hub,
    systemPrompt: 'orchestrator-system',
  });

  await adapter.connect();
  const result = await adapter.sendMessage('round recovery orchestrator plain');
  const history = adapter.getHistory();

  assert(client.attempts === 2, `Orchestrator 普通模式应自动续跑 1 次，实际请求次数: ${client.attempts}`);
  assert(result.includes('普通模式续跑完成'), `Orchestrator 普通模式最终返回异常: ${result}`);
  assert(
    history.some((msg) => msg.role === 'assistant' && typeof msg.content === 'string' && msg.content.includes('普通模式前半句')),
    'Orchestrator 普通模式历史缺少首轮已输出文本',
  );
  assert(
    history.some((msg) => msg.role === 'user' && typeof msg.content === 'string' && msg.content.includes('已自动续跑')),
    'Orchestrator 普通模式历史缺少续跑提示',
  );
  assert(toolManager.executeCount === 0, 'Orchestrator 普通模式本用例不应执行任何工具');
}

async function testWorkerRoundRecovery() {
  const { WorkerLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'worker-adapter.js'));

  const normalizer = new MockNormalizer();
  const messageHub = createMessageHub('trace-worker');
  const toolManager = createToolManagerStub();
  const client = createInterruptedThenRecoveredClient('已输出步骤一，', '步骤二完成。');

  const adapter = new WorkerLLMAdapter({
    client,
    normalizer,
    toolManager,
    config: {
      provider: 'anthropic',
      model: 'claude-sonnet-4',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    messageHub: messageHub.hub,
    workerSlot: 'codex',
    systemPrompt: 'worker-system',
    profileLoader: {
      getProfile() {
        return {
          persona: {
            displayName: 'mock',
            roleDefinition: 'mock',
            strengths: [],
            workStyle: [],
            constraints: [],
          },
          assignedCategories: [],
        };
      },
    },
  });

  await adapter.connect();
  const result = await adapter.sendMessage('round recovery worker');
  const history = adapter.getHistory();

  assert(client.attempts === 2, `Worker 应自动续跑 1 次，实际请求次数: ${client.attempts}`);
  assert(result.includes('步骤二完成'), `Worker 最终返回异常: ${result}`);
  assert(
    history.some((msg) => msg.role === 'assistant' && typeof msg.content === 'string' && msg.content.includes('已输出步骤一')),
    'Worker 历史缺少首轮已输出文本',
  );
  assert(
    history.some((msg) => msg.role === 'user' && typeof msg.content === 'string' && msg.content.includes('已自动续跑')),
    'Worker 历史缺少续跑提示',
  );
  assert(
    messageHub.sentMessages.some(
      (msg) => msg.type === 'workerOutput'
        && typeof msg.content === 'string'
        && msg.content.includes('当前 Worker')
        && msg.content.includes('已自动续跑'),
    ),
    'Worker 自动续跑缺少可见过程消息',
  );
  assert(toolManager.executeCount === 0, 'Worker 本用例不应执行任何工具');
}

async function testOrchestratorToolSignalRecovery() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));

  const normalizer = new MockNormalizer();
  const messageHub = createMessageHub('trace-orchestrator-tool-signal');
  const toolManager = createToolManagerStub();
  const client = createToolSignalInterruptedThenRecoveredClient('工具信号续跑完成。');

  const adapter = new OrchestratorLLMAdapter({
    client,
    normalizer,
    toolManager,
    config: {
      provider: 'anthropic',
      model: 'claude-sonnet-4',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    messageHub: messageHub.hub,
    systemPrompt: 'orchestrator-system',
  });

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const result = await adapter.sendMessage('tool signal recovery orchestrator');
  const history = adapter.getHistory();

  assert(client.attempts === 2, `Orchestrator 工具信号中断应自动续跑 1 次，实际请求次数: ${client.attempts}`);
  assert(result.includes('工具信号续跑完成'), `Orchestrator 工具信号续跑最终返回异常: ${result}`);
  assert(
    history.some((msg) => msg.role === 'user' && typeof msg.content === 'string' && msg.content.includes('工具调用阶段因网络波动中断')),
    'Orchestrator 历史缺少工具阶段续跑提示',
  );
}

async function testWorkerToolSignalRecovery() {
  const { WorkerLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'worker-adapter.js'));

  const normalizer = new MockNormalizer();
  const messageHub = createMessageHub('trace-worker-tool-signal');
  const toolManager = createToolManagerStub();
  const client = createToolSignalInterruptedThenRecoveredClient('Worker 工具信号续跑完成。');

  const adapter = new WorkerLLMAdapter({
    client,
    normalizer,
    toolManager,
    config: {
      provider: 'anthropic',
      model: 'claude-sonnet-4',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    messageHub: messageHub.hub,
    workerSlot: 'codex',
    systemPrompt: 'worker-system',
    profileLoader: {
      getProfile() {
        return {
          persona: {
            displayName: 'mock',
            roleDefinition: 'mock',
            strengths: [],
            workStyle: [],
            constraints: [],
          },
          assignedCategories: [],
        };
      },
    },
  });

  await adapter.connect();
  const result = await adapter.sendMessage('tool signal recovery worker');
  const history = adapter.getHistory();

  assert(client.attempts === 2, `Worker 工具信号中断应自动续跑 1 次，实际请求次数: ${client.attempts}`);
  assert(result.includes('Worker 工具信号续跑完成'), `Worker 工具信号续跑最终返回异常: ${result}`);
  assert(
    history.some((msg) => msg.role === 'user' && typeof msg.content === 'string' && msg.content.includes('工具调用阶段因网络波动中断')),
    'Worker 历史缺少工具阶段续跑提示',
  );
  assert(
    messageHub.sentMessages.some(
      (msg) => msg.type === 'workerOutput'
        && typeof msg.content === 'string'
        && msg.content.includes('工具调用阶段')
        && msg.content.includes('已自动续跑'),
    ),
    'Worker 工具阶段自动续跑缺少可见过程消息',
  );
}

async function main() {
  await testOrchestratorRoundRecovery();
  await testOrchestratorPlainRoundRecovery();
  await testWorkerRoundRecovery();
  await testOrchestratorToolSignalRecovery();
  await testWorkerToolSignalRecovery();

  console.log('\n=== round stream interruption recovery regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'orchestrator-round-interruption-auto-recover',
      'orchestrator-plain-round-interruption-auto-recover',
      'worker-round-interruption-auto-recover',
      'orchestrator-tool-signal-interruption-auto-recover',
      'worker-tool-signal-interruption-auto-recover',
      'history-contains-partial-and-recovery-prompt',
      'no-tool-side-effects',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('round stream interruption recovery 回归失败:', error?.stack || error);
  process.exit(1);
});
