#!/usr/bin/env node
/**
 * 工具轮重复输出（运行态）回归脚本
 *
 * 目标：
 * 1) 工具轮直接命中终止（budget_exceeded）时，不应触发二次 fallback 回灌
 * 2) 同一段 assistant 文本只出现一次（单卡片收口）
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

function extractText(message) {
  const blocks = Array.isArray(message?.blocks) ? message.blocks : [];
  return blocks
    .filter((block) => block?.type === 'text' && typeof block?.content === 'string')
    .map((block) => block.content)
    .join('\n');
}

async function main() {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));
  const { CodexNormalizer } = loadCompiledModule(path.join('normalizer', 'codex-normalizer.js'));

  const streamedText = '开始多轮对比测试。每轮使用相同查询分别调用两个工具，从多个维度评分。';
  const captured = {
    messages: [],
    updates: [],
  };

  const fakeMessageHub = {
    getTraceId: () => 'trace-regression',
    getRequestContext: () => 'req-regression',
    getRequestMessageId: () => 'placeholder-regression',
    sendMessage: (message) => { captured.messages.push(message); return true; },
    sendUpdate: (update) => { captured.updates.push(update); return true; },
    data: () => {},
  };

  const fakeClient = {
    streamMessage: async (_params, onChunk) => {
      onChunk({ type: 'content_delta', content: streamedText });
      return {
        content: '',
        toolCalls: [
          {
            id: 'tool-call-1',
            name: 'local_retrieval',
            arguments: { query: '用户登录认证流程' },
          },
        ],
        usage: { inputTokens: 20, outputTokens: 30 },
      };
    },
  };

  const fakeToolManager = {
    getTools: async () => [
      {
        name: 'local_retrieval',
        description: 'local retrieval',
        input_schema: {
          type: 'object',
          properties: { query: { type: 'string' } },
        },
        metadata: { source: 'builtin' },
      },
    ],
    requiresUserAuthorization: () => false,
    execute: async () => ({ toolCallId: 'unused', content: '{}', isError: false }),
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

  // 构造“required todo 轨道 + 工具轮硬预算终止”的场景：
  // 如果 finalTextDelivered 没在工具轮置位，会触发循环外 fallback 二次回灌（历史缺陷）。
  adapter.executeToolCalls = async (toolCalls) => toolCalls.map((toolCall) => ({
    toolCallId: toolCall.id,
    content: '{"ok":true}',
    isError: false,
    standardized: {
      schemaVersion: 'tool-result.v1',
      source: 'builtin',
      toolName: toolCall.name,
      toolCallId: toolCall.id,
      status: 'success',
      message: '{"ok":true}',
    },
  }));

  adapter.buildTerminationSnapshot = async (params) => ({
    baseline: params.baseline || null,
    snapshot: {
      snapshotId: 'snap-runtime-dup',
      planId: 'plan-runtime-dup',
      attemptSeq: 3,
      progressVector: {
        terminalRequiredTodos: 0,
        acceptedCriteria: 0,
        criticalPathResolved: 0,
        unresolvedBlockers: 0,
      },
      reviewState: { accepted: 0, total: 1 },
      blockerState: {
        open: 0,
        score: 0,
        externalWaitOpen: 0,
        maxExternalWaitAgeMs: 0,
      },
      budgetState: {
        // 标准预算 420_000ms，硬阈值=1.2x=504_000ms；此处直接命中硬终止
        elapsedMs: 560_000,
        tokenUsed: 10,
        errorRate: 0,
      },
      cpVersion: 1,
      requiredTotal: 1,
      failedRequired: 0,
      runningOrPendingRequired: 1,
      sourceEventIds: [],
      computedAt: Date.now(),
    },
    progressed: false,
    cpRebased: false,
  });

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const finalText = await adapter.sendMessage('请执行一次工具调用并给出结论');

  const contentMessages = captured.messages.filter((message) => message?.category === 'content');
  const startedCount = contentMessages.filter((message) => message?.lifecycle === 'started').length;
  const completedWithSameText = contentMessages.filter((message) => {
    if (message?.lifecycle !== 'completed') return false;
    const text = extractText(message);
    return text.includes(streamedText);
  });

  assert(finalText.includes(streamedText), '最终返回文本缺少工具轮输出内容');
  assert(startedCount === 1, `检测到异常二次开流（started=${startedCount}），疑似 fallback 重复回灌`);
  assert(completedWithSameText.length === 1, `同段文本被重复完成输出 ${completedWithSameText.length} 次`);

  console.log('\n=== tool round runtime no-duplicate regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'no-secondary-fallback-stream',
      'single-complete-message-for-tool-round-text',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('tool round runtime no-duplicate 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});
