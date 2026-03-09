#!/usr/bin/env node
/**
 * 无 Todo 轨道下“工具后模糊文本提前终止”回归脚本
 *
 * 目标：
 * 1) 工具轮后首个无工具模糊文本不应直接 completed
 * 2) 编排器应继续下一轮，直到收到显式 final 文本再结束
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

  let streamCallCount = 0;
  const captured = {
    messages: [],
    updates: [],
  };

  const fakeMessageHub = {
    getTraceId: () => 'trace-ambiguous',
    getRequestContext: () => 'req-ambiguous',
    getRequestMessageId: () => 'placeholder-ambiguous',
    sendMessage: (message) => { captured.messages.push(message); return true; },
    sendUpdate: (update) => { captured.updates.push(update); return true; },
    data: () => {},
  };

  const fakeClient = {
    streamMessage: async (_params, onChunk) => {
      streamCallCount += 1;
      if (streamCallCount === 1) {
        onChunk({ type: 'content_delta', content: '开始多轮对比测试。每轮使用相同查询分别调用两个工具，从多个维度评分。' });
        return {
          content: '',
          toolCalls: [
            { id: 'tc-1', name: 'local_retrieval', arguments: { query: '用户登录认证流程' } },
          ],
          usage: { inputTokens: 20, outputTokens: 30 },
        };
      }
      if (streamCallCount === 2) {
        // 模糊中间态文本：历史缺陷会在这里直接 completed
        onChunk({ type: 'content_delta', content: 'Round 1: 模糊业务概念查询。查询词："用户登录认证流程"。' });
        return {
          content: '',
          toolCalls: [],
          usage: { inputTokens: 10, outputTokens: 20 },
        };
      }
      // 第三轮返回显式最终结论，预期在此轮终止
      onChunk({ type: 'content_delta', content: '最终结论：用户登录认证流程已完成对比分析，关键证据见上述工具结果。' });
      return {
        content: '',
        toolCalls: [],
        usage: { inputTokens: 8, outputTokens: 18 },
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

  let snapshotSeq = 0;
  adapter.buildTerminationSnapshot = async (params) => {
    snapshotSeq += 1;
    return {
      baseline: params.baseline || null,
      snapshot: {
        snapshotId: `snap-ambiguous-${snapshotSeq}`,
        planId: 'plan-ambiguous',
        attemptSeq: snapshotSeq,
        progressVector: {
          terminalRequiredTodos: 0,
          acceptedCriteria: 0,
          criticalPathResolved: 0,
          unresolvedBlockers: 0,
        },
        reviewState: { accepted: 0, total: 0 },
        blockerState: {
          open: 0,
          score: 0,
          externalWaitOpen: 0,
          maxExternalWaitAgeMs: 0,
        },
        budgetState: {
          elapsedMs: 1000 + snapshotSeq * 100,
          tokenUsed: 100 + snapshotSeq * 10,
          errorRate: 0,
        },
        cpVersion: 1,
        requiredTotal: 0,
        failedRequired: 0,
        runningOrPendingRequired: 0,
        sourceEventIds: [],
        computedAt: Date.now(),
      },
      progressed: false,
      cpRebased: false,
    };
  };

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const finalText = await adapter.sendMessage('执行对比测试并输出结论');

  assert(streamCallCount >= 3, `工具后模糊文本被提前终止（streamCallCount=${streamCallCount}）`);
  assert(finalText.includes('最终结论'), '未等到显式最终结论即结束');

  console.log('\n=== no todo post-tool ambiguous regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'ambiguous-no-tool-text-not-completed-immediately',
      'continue-until-explicit-final',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('no todo post-tool ambiguous 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});

