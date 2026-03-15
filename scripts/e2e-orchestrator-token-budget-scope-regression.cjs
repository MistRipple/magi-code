#!/usr/bin/env node
/**
 * Orchestrator token 预算口径回归
 *
 * 目标：
 * 1) tokenUsed 必须按“单次 sendMessageWithTools 增量”计
 * 2) adapter 历史累计 token 很高时，不应导致本轮首轮即 budget_exceeded
 */

const path = require('path');
const fs = require('fs');

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

async function main() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));
  const { CodexNormalizer } = loadCompiledModule(path.join('normalizer', 'codex-normalizer.js'));

  const fakeMessageHub = {
    getTraceId: () => 'trace-token-budget',
    getRequestContext: () => 'req-token-budget',
    getRequestMessageId: () => 'placeholder-token-budget',
    sendMessage: () => true,
    sendUpdate: () => true,
    data: () => {},
  };

  const fakeClient = {
    streamMessage: async (_params, onChunk) => {
      onChunk({ type: 'content_delta', content: '最终结论：本轮测试完成。' });
      return {
        content: '',
        toolCalls: [],
        usage: { inputTokens: 20, outputTokens: 10 },
      };
    },
  };

  const fakeToolManager = {
    getTools: async () => [],
    requiresUserAuthorization: () => false,
    execute: async (toolCall) => {
      if (toolCall?.name === 'todo_list') {
        return {
          toolCallId: toolCall.id,
          content: '[]',
          isError: false,
        };
      }
      return {
        toolCallId: toolCall?.id || 'unknown',
        content: '{}',
        isError: false,
      };
    },
  };

  const adapter = new OrchestratorLLMAdapter({
    client: fakeClient,
    normalizer: new CodexNormalizer({ agent: 'orchestrator', defaultSource: 'orchestrator' }),
    toolManager: fakeToolManager,
    config: {
      provider: 'openai',
      model: 'gpt-5',
      apiKey: 'test',
      baseURL: 'http://localhost',
    },
    messageHub: fakeMessageHub,
    deepTask: false,
  });

  // 构造“会话历史累计 token 已很高”的场景。
  // 旧实现会直接把累计值带入本轮 budget 判定，导致首轮即 budget_exceeded。
  adapter.totalTokenUsage = {
    inputTokens: 180000,
    outputTokens: 60000,
  };

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const content = await adapter.sendMessage('请给出最终结论');
  const runtime = adapter.getLastRuntimeState();

  assert(content.includes('最终结论'), '未返回预期最终结论文本');
  assert(runtime.reason !== 'budget_exceeded', `检测到错误预算口径，当前轮被误判为 budget_exceeded（reason=${runtime.reason}）`);
  assert(runtime.reason === 'completed', `期望 completed，实际 ${runtime.reason}`);

  console.log('\n=== orchestrator token budget scope regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'token-budget-uses-per-run-delta',
      'no-immediate-budget-exceeded-by-lifetime-tokens',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('orchestrator token budget scope 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});

