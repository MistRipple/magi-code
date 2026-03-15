#!/usr/bin/env node
/**
 * Worktree 执行上下文透传回归脚本
 *
 * 覆盖目标：
 * 1) WorkerAdapter 执行工具时必须携带 worktreePath（若已注入）
 * 2) 工具上下文透传链路代码必须完整存在：
 *    autonomous-worker -> adapter-factory -> worker-adapter
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

function read(relPath) {
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

class FakeNormalizer extends EventEmitter {
  constructor() {
    super();
    this.streamSeq = 0;
  }

  startStream() {
    this.streamSeq += 1;
    return `fake-stream-${this.streamSeq}`;
  }

  processTextDelta() {}
  processThinking() {}
  addToolCall() {}
  finishToolCall() {}
  endStream() {}
}

class FakeMessageHub {
  getTraceId() {
    return 'trace-worktree-context';
  }

  getRequestMessageId() {
    return undefined;
  }

  sendMessage() {}
  sendUpdate() {}
  data() {}
}

function createFakeProfileLoader() {
  return {
    getProfile() {
      return {
        worker: 'codex',
        assignedCategories: ['feature_development'],
        persona: {
          displayName: 'Test Worker',
          baseRole: 'Test role',
          strengths: ['coding'],
          weaknesses: [],
          collaboration: {
            asLeader: [],
            asCollaborator: [],
          },
          outputPreferences: [],
          reasoningGuidelines: [],
        },
      };
    },
  };
}

function createFakeToolManager(capture) {
  return {
    async getTools() {
      return [{
        name: 'file_view',
        description: 'View file',
        input_schema: {
          type: 'object',
          properties: {
            path: { type: 'string' },
          },
          required: ['path'],
        },
        metadata: {
          source: 'builtin',
        },
      }];
    },
    async buildToolsSummary() {
      return '';
    },
    requiresUserAuthorization() {
      return false;
    },
    async execute(toolCall, _signal, executionContext) {
      capture.calls.push({ toolCall, executionContext });
      return {
        toolCallId: toolCall.id,
        content: 'OK: viewed',
        isError: false,
      };
    },
    async executeInternalTool() {
      throw new Error('executeInternalTool should not be called in non-terminal scenario');
    },
    getShellExecutor() {
      return {
        on() {},
        off() {},
        async readProcess() {
          return {};
        },
      };
    },
  };
}

function createFakeClient() {
  let round = 0;
  return {
    async streamMessage(_params, _onChunk) {
      round += 1;
      if (round === 1 || round === 3) {
        return {
          content: 'use tool',
          toolCalls: [{
            id: `tool-${round}`,
            name: 'file_view',
            arguments: { path: round === 1 ? 'src/index.ts' : 'src/worker.ts' },
          }],
          usage: { inputTokens: 1, outputTokens: 1 },
        };
      }
      return {
        content: 'done',
        toolCalls: [],
        usage: { inputTokens: 1, outputTokens: 1 },
      };
    },
    async sendMessage() {
      return {
        content: 'silent',
        usage: { inputTokens: 1, outputTokens: 1 },
      };
    },
  };
}

function testSourceChain() {
  const adapterScopeInterface = read('src/adapters/adapter-factory-interface.ts');
  const adapterFactory = read('src/llm/adapter-factory.ts');
  const autonomousWorker = read('src/orchestrator/worker/autonomous-worker.ts');
  const workerAdapter = read('src/llm/adapters/worker-adapter.ts');

  assert(
    adapterScopeInterface.includes('toolExecutionContext?: Partial<ToolExecutionContext>;'),
    'AdapterOutputScope 未声明 toolExecutionContext',
  );
  assert(
    adapterFactory.includes('setCurrentToolExecutionContext(options?.toolExecutionContext);'),
    'AdapterFactory 未注入 toolExecutionContext 到 Adapter',
  );
  assert(
    adapterFactory.includes('setCurrentToolExecutionContext(undefined);'),
    'AdapterFactory 未在请求结束后清理 toolExecutionContext',
  );
  assert(
    autonomousWorker.includes('toolExecutionContext: {') && autonomousWorker.includes('worktreePath: options.workingDirectory'),
    'AutonomousWorker 未将 workingDirectory 透传为 worktreePath',
  );
  assert(
    workerAdapter.includes('this.executeToolCalls(toolCalls, toolExecutionContext)') && workerAdapter.includes('executionContext: toolExecutionContext'),
    'WorkerAdapter 未使用统一工具执行上下文',
  );
}

async function testRuntimePropagation() {
  const { WorkerLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'worker-adapter.js'));
  const capture = { calls: [] };
  const adapter = new WorkerLLMAdapter({
    client: createFakeClient(),
    normalizer: new FakeNormalizer(),
    toolManager: createFakeToolManager(capture),
    config: {
      provider: 'openai',
      model: 'gpt-5',
      baseUrl: 'http://127.0.0.1',
      apiKey: 'test-key',
      enabled: true,
    },
    messageHub: new FakeMessageHub(),
    workerSlot: 'codex',
    profileLoader: createFakeProfileLoader(),
    systemPrompt: 'test system prompt',
  });

  await adapter.connect();
  adapter.setCurrentToolExecutionContext({ worktreePath: '/tmp/magi-worktree-runtime' });
  const first = await adapter.sendMessage('run with worktree context');
  assert(first === 'done', `首轮响应异常: ${first}`);
  assert(capture.calls.length === 1, `首轮工具调用数量异常: ${capture.calls.length}`);
  assert(capture.calls[0].executionContext?.workerId === 'codex', '首轮 workerId 上下文缺失');
  assert(capture.calls[0].executionContext?.role === 'worker', '首轮 role 上下文缺失');
  assert(
    capture.calls[0].executionContext?.worktreePath === '/tmp/magi-worktree-runtime',
    `首轮 worktreePath 透传失败: ${capture.calls[0].executionContext?.worktreePath}`,
  );

  adapter.setCurrentToolExecutionContext(undefined);
  const second = await adapter.sendMessage('run without worktree context');
  assert(second === 'done', `次轮响应异常: ${second}`);
  assert(capture.calls.length === 2, `次轮工具调用数量异常: ${capture.calls.length}`);
  assert(
    capture.calls[1].executionContext?.worktreePath === undefined,
    `次轮应清空 worktreePath，实际: ${capture.calls[1].executionContext?.worktreePath}`,
  );
}

async function main() {
  testSourceChain();
  await testRuntimePropagation();
  console.log('\n=== worktree context propagation regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'source-chain-integrity',
      'runtime-worktree-context-forwarding',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('worktree context propagation 回归失败:', error?.stack || error);
  process.exit(1);
});
