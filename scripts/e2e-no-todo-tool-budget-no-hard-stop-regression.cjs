#!/usr/bin/env node
/**
 * 无 Todo 轨道下“工具轮预算误停”回归脚本
 *
 * 目标：
 * 1) requiredTotal=0 时，即使 budgetState 超阈值，也不应在工具轮直接终止
 * 2) 工具轮后必须进入下一轮无工具总结，由模型给出最终文本
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

  const fakeMessageHub = {
    getTraceId: () => 'trace-no-todo-budget',
    getRequestContext: () => 'req-no-todo-budget',
    getRequestMessageId: () => 'placeholder-no-todo-budget',
    sendMessage: () => true,
    sendUpdate: () => true,
    data: () => {},
  };

  const fakeClient = {
    streamMessage: async (_params, onChunk) => {
      streamCallCount += 1;
      if (streamCallCount === 1) {
        onChunk({ type: 'content_delta', content: '先执行工具拿到证据。' });
        return {
          content: '',
          toolCalls: [
            { id: 'tool-1', name: 'local_lookup', arguments: { q: 'budget gate' } },
          ],
          usage: { inputTokens: 100, outputTokens: 50 },
        };
      }
      onChunk({ type: 'content_delta', content: '最终结论：工具结果已分析，问题定位完成。' });
      return {
        content: '',
        toolCalls: [],
        usage: { inputTokens: 100, outputTokens: 50 },
      };
    },
  };

  const fakeToolManager = {
    getTools: async () => [
      {
        name: 'local_lookup',
        description: 'local lookup',
        input_schema: {
          type: 'object',
          properties: { q: { type: 'string' } },
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
        snapshotId: `snap-no-todo-budget-${snapshotSeq}`,
        planId: 'plan-no-todo-budget',
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
        // 人为构造“预算超阈值”快照，旧实现会在工具轮直接 budget_exceeded 终止
        budgetState: {
          elapsedMs: 999999,
          tokenUsed: 999999,
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
  const finalText = await adapter.sendMessage('执行工具并完成最终结论');
  const runtime = adapter.getLastRuntimeState();

  assert(streamCallCount >= 2, `检测到工具轮预算误停（streamCallCount=${streamCallCount}）`);
  assert(runtime.reason === 'completed', `期望 completed，实际 ${runtime.reason}`);
  assert(finalText.includes('最终结论'), '未返回最终结论文本');

  console.log('\n=== no todo tool budget no hard-stop regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'no-budget-hard-stop-when-required-total-zero',
      'tool-round-continues-to-final-synthesis',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('no todo tool budget no hard-stop 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});

