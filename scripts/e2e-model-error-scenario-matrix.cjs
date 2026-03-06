#!/usr/bin/env node
/**
 * 模型异常场景矩阵回归
 *
 * 覆盖场景：
 * - 鉴权失败
 * - 限流
 * - 上下文超限
 * - 空响应
 * - 非模型错误（控制组）
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

function pickLastBy(arr, predicate) {
  for (let i = arr.length - 1; i >= 0; i--) {
    if (predicate(arr[i])) {
      return arr[i];
    }
  }
  return undefined;
}

function runScenario(factory, emitted, protocol, scenario) {
  emitted.length = 0;
  factory.workerError('codex', scenario.rawReason);
  factory.taskRejected(`req-${scenario.id}`, scenario.rawReason);
  factory.subTaskCard({
    id: `task-${scenario.id}`,
    title: `场景 ${scenario.id}`,
    status: 'failed',
    worker: 'codex',
    summary: scenario.rawReason,
    error: scenario.rawReason,
  });

  const workerErrorMsg = pickLastBy(
    emitted,
    (msg) => msg.type === protocol.MessageType.ERROR,
  );
  assert(workerErrorMsg, `[${scenario.id}] 缺少 worker ERROR 消息`);

  const taskRejectedMsg = pickLastBy(
    emitted,
    (msg) => msg.control?.controlType === protocol.ControlMessageType.TASK_REJECTED,
  );
  assert(taskRejectedMsg, `[${scenario.id}] 缺少 TASK_REJECTED 控制消息`);

  const taskCardMsg = pickLastBy(
    emitted,
    (msg) => msg.type === protocol.MessageType.TASK_CARD,
  );
  assert(taskCardMsg, `[${scenario.id}] 缺少 TASK_CARD 消息`);

  const workerErrorText = String(workerErrorMsg.blocks?.[0]?.content || '');
  const rejectedReason = String(taskRejectedMsg.control?.payload?.reason || '');
  const taskCardText = String(taskCardMsg.blocks?.[0]?.content || '');
  const taskCardPayload = taskCardMsg.metadata?.subTaskCard || {};
  const taskCardSummary = String(taskCardPayload.summary || '');

  if (scenario.modelCause) {
    assert(
      workerErrorText.includes(scenario.expectedKeyword),
      `[${scenario.id}] workerError 未归一化: ${workerErrorText}`,
    );
    assert(
      rejectedReason.includes(scenario.expectedKeyword),
      `[${scenario.id}] taskRejected 未归一化: ${rejectedReason}`,
    );
    assert(
      taskCardText.includes(scenario.expectedKeyword) || taskCardSummary.includes(scenario.expectedKeyword),
      `[${scenario.id}] taskCard 未归一化: ${taskCardText} / ${taskCardSummary}`,
    );
    assert(
      taskRejectedMsg.control?.payload?.modelOriginIssue === true,
      `[${scenario.id}] taskRejected 未标记 modelOriginIssue`,
    );
  } else {
    assert(
      workerErrorText.includes(scenario.expectedKeyword),
      `[${scenario.id}] 非模型错误不应被改写: ${workerErrorText}`,
    );
    assert(
      rejectedReason.includes(scenario.expectedKeyword),
      `[${scenario.id}] 非模型错误 taskRejected 被错误改写: ${rejectedReason}`,
    );
    assert(
      !taskRejectedMsg.control?.payload?.modelOriginIssue,
      `[${scenario.id}] 非模型错误不应标记 modelOriginIssue`,
    );
  }
}

async function main() {
  const modelOrigin = loadCompiledModule(path.join('errors', 'model-origin.js'));
  const { MessageFactory } = loadCompiledModule(path.join('orchestrator', 'core', 'message-factory.js'));
  const protocol = loadCompiledModule(path.join('protocol', 'message-protocol.js'));

  const scenarios = [
    {
      id: 'auth',
      rawReason: '500 {"error":{"message":"auth_unavailable: no auth available"}}',
      expectedKeyword: '鉴权失败',
      modelCause: true,
      expectedKind: 'auth',
    },
    {
      id: 'rate-limit',
      rawReason: '429 too many requests, rate limit reached',
      expectedKeyword: '限流',
      modelCause: true,
      expectedKind: 'rate_limit',
    },
    {
      id: 'context-limit',
      rawReason: 'maximum context length exceeded, token limit reached',
      expectedKeyword: '上下文超出限制',
      modelCause: true,
      expectedKind: 'context_limit',
    },
    {
      id: 'empty-response',
      rawReason: 'LLM 响应为空：流式传输完成但未收到有效内容',
      expectedKeyword: '未返回可执行内容',
      modelCause: true,
      expectedKind: 'empty_response',
    },
    {
      id: 'non-model-control',
      rawReason: '读取文件失败: path does not exist',
      expectedKeyword: '读取文件失败',
      modelCause: false,
    },
  ];

  for (const scenario of scenarios) {
    const classified = modelOrigin.classifyModelOriginIssue(scenario.rawReason);
    assert(
      classified.isModelCause === scenario.modelCause,
      `[${scenario.id}] isModelCause 结果异常: ${JSON.stringify(classified)}`,
    );
    if (scenario.modelCause) {
      assert(
        classified.kind === scenario.expectedKind,
        `[${scenario.id}] kind 异常: ${classified.kind}, 期望: ${scenario.expectedKind}`,
      );
      const userMessage = modelOrigin.toModelOriginUserMessage(scenario.rawReason);
      assert(
        userMessage.includes(scenario.expectedKeyword),
        `[${scenario.id}] 用户文案异常: ${userMessage}`,
      );
    }
  }

  const emitted = [];
  const pipeline = {
    process(message) {
      emitted.push(message);
      return true;
    },
    clearMessageState() {},
    getRequestMessageId() {
      return undefined;
    },
  };

  const factory = new MessageFactory(pipeline, 'trace-model-scenario-matrix');
  for (const scenario of scenarios) {
    runScenario(factory, emitted, protocol, scenario);
  }

  console.log('\n=== 模型异常场景矩阵回归结果 ===');
  console.log(JSON.stringify({
    pass: true,
    totalScenarios: scenarios.length,
    scenarios: scenarios.map((item) => ({ id: item.id, modelCause: item.modelCause })),
  }, null, 2));
}

main().catch((error) => {
  console.error('模型异常场景矩阵回归失败:', error?.stack || error);
  process.exit(1);
});
