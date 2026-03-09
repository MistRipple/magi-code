#!/usr/bin/env node
/**
 * Orchestrator 门禁去抖回归
 *
 * 覆盖目标：
 * 1) required todo 轨道下，预算门禁单轮超限不应立即终止（需去抖）
 * 2) required todo 轨道下，external_wait 单轮超限不应立即终止（需去抖）
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

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function createSnapshot(partial, seq) {
  return {
    snapshotId: `snap-gate-debounce-${seq}`,
    planId: 'plan-gate-debounce',
    attemptSeq: seq,
    progressVector: {
      terminalRequiredTodos: partial.terminalRequiredTodos ?? 0,
      acceptedCriteria: partial.acceptedCriteria ?? 0,
      criticalPathResolved: partial.criticalPathResolved ?? 0,
      unresolvedBlockers: partial.unresolvedBlockers ?? 0,
    },
    reviewState: {
      accepted: partial.accepted ?? 0,
      total: partial.total ?? 2,
    },
    blockerState: {
      open: partial.blockerOpen ?? 0,
      score: partial.blockerScore ?? 0,
      externalWaitOpen: partial.externalWaitOpen ?? 0,
      maxExternalWaitAgeMs: partial.maxExternalWaitAgeMs ?? 0,
    },
    budgetState: {
      elapsedMs: partial.elapsedMs ?? 1000,
      tokenUsed: partial.tokenUsed ?? 500,
      errorRate: partial.errorRate ?? 0,
    },
    cpVersion: 1,
    requiredTotal: partial.requiredTotal ?? 2,
    failedRequired: partial.failedRequired ?? 0,
    runningOrPendingRequired: partial.runningOrPendingRequired ?? 0,
    sourceEventIds: [],
    computedAt: Date.now(),
  };
}

async function runCase(caseName, snapshots) {
  const { OrchestratorLLMAdapter } = loadCompiledModule(path.join('llm', 'adapters', 'orchestrator-adapter.js'));
  const { CodexNormalizer } = loadCompiledModule(path.join('normalizer', 'codex-normalizer.js'));

  let streamCallCount = 0;
  const fakeMessageHub = {
    getTraceId: () => `trace-${caseName}`,
    getRequestContext: () => `req-${caseName}`,
    getRequestMessageId: () => `placeholder-${caseName}`,
    sendMessage: () => true,
    sendUpdate: () => true,
    data: () => {},
  };

  const fakeClient = {
    streamMessage: async (_params, onChunk) => {
      streamCallCount += 1;
      if (streamCallCount === 1) {
        onChunk({ type: 'content_delta', content: '先执行工具拿取执行证据。' });
        return {
          content: '',
          toolCalls: [
            { id: `tc-${caseName}-1`, name: 'local_lookup', arguments: { q: caseName } },
          ],
          usage: { inputTokens: 30, outputTokens: 20 },
        };
      }
      if (streamCallCount === 2) {
        onChunk({ type: 'content_delta', content: '继续推进中，尚有必需 Todo 未完成。' });
        return {
          content: '',
          toolCalls: [],
          usage: { inputTokens: 20, outputTokens: 20 },
        };
      }
      onChunk({ type: 'content_delta', content: `最终结论：${caseName} 门禁去抖验证通过。` });
      return {
        content: '',
        toolCalls: [],
        usage: { inputTokens: 20, outputTokens: 20 },
      };
    },
  };

  const fakeToolManager = {
    getTools: async () => [
      {
        name: 'local_lookup',
        description: 'local lookup',
        input_schema: { type: 'object', properties: { q: { type: 'string' } } },
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

  let seq = 0;
  adapter.buildTerminationSnapshot = async (params) => {
    seq += 1;
    const pick = snapshots[Math.min(seq - 1, snapshots.length - 1)];
    return {
      baseline: params.baseline || null,
      snapshot: createSnapshot(pick, seq),
      progressed: pick.progressed !== false,
      cpRebased: false,
    };
  };

  await adapter.connect();
  adapter.setTempEnableToolCalls(true);
  const finalText = await adapter.sendMessage(`执行 ${caseName} 去抖回归`);
  const runtime = adapter.getLastRuntimeState();

  assert(streamCallCount >= 3, `${caseName} 出现提前门禁终止（streamCallCount=${streamCallCount}）`);
  assert(runtime.reason === 'completed', `${caseName} 期望 completed，实际 ${runtime.reason}`);
  assert(finalText.includes('最终结论'), `${caseName} 未返回最终结论文本`);
}

async function main() {
  const budgetMaxDuration = 420000;
  const externalWaitSla = 180000;

  // case1: budget 单轮超限（旧逻辑会在工具轮直接 budget_exceeded）
  await runCase('budget-single-spike', [
    {
      requiredTotal: 2,
      terminalRequiredTodos: 0,
      runningOrPendingRequired: 2,
      elapsedMs: budgetMaxDuration + 5000,
      tokenUsed: 1000,
      progressed: true,
    },
    {
      requiredTotal: 2,
      terminalRequiredTodos: 1,
      runningOrPendingRequired: 1,
      elapsedMs: 2000,
      tokenUsed: 1200,
      progressed: true,
    },
    {
      requiredTotal: 2,
      terminalRequiredTodos: 2,
      runningOrPendingRequired: 0,
      elapsedMs: 2500,
      tokenUsed: 1400,
      progressed: true,
    },
  ]);

  // case2: external_wait 单轮超限（旧逻辑会在工具轮直接 external_wait_timeout）
  await runCase('external-wait-single-spike', [
    {
      requiredTotal: 2,
      terminalRequiredTodos: 0,
      runningOrPendingRequired: 2,
      externalWaitOpen: 1,
      maxExternalWaitAgeMs: externalWaitSla + 5000,
      progressed: true,
    },
    {
      requiredTotal: 2,
      terminalRequiredTodos: 1,
      runningOrPendingRequired: 1,
      externalWaitOpen: 0,
      maxExternalWaitAgeMs: 0,
      progressed: true,
    },
    {
      requiredTotal: 2,
      terminalRequiredTodos: 2,
      runningOrPendingRequired: 0,
      externalWaitOpen: 0,
      maxExternalWaitAgeMs: 0,
      progressed: true,
    },
  ]);

  console.log('\n=== orchestrator gate debounce regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'budget-single-spike-not-hard-stop',
      'external-wait-single-spike-not-hard-stop',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('orchestrator gate debounce 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});

