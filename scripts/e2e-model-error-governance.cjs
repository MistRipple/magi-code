#!/usr/bin/env node
/**
 * 模型异常治理回归脚本
 *
 * 目标：
 * 1. 验证模型异常分类规则稳定可用
 * 2. 验证 MessageFactory 会把模型异常统一归一化为用户友好文案
 * 3. 防回归检查：Worker 执行层不应再出现 "LLM 执行失败:" 包装透传
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

function testModelClassification() {
  const { classifyModelOriginIssue, toModelOriginUserMessage } = loadCompiledModule(path.join('errors', 'model-origin.js'));

  const empty = classifyModelOriginIssue('LLM 响应为空：流式传输完成但未收到有效内容');
  assert(empty.isModelCause === true, '空响应应识别为模型原因');
  assert(empty.kind === 'empty_response', `空响应 kind 异常: ${empty.kind}`);

  const auth = classifyModelOriginIssue('500 {"error":{"message":"auth_unavailable: no auth available"}}');
  assert(auth.isModelCause === true, '鉴权失败应识别为模型原因');
  assert(auth.kind === 'auth', `鉴权失败 kind 异常: ${auth.kind}`);

  const leak = classifyModelOriginIssue('**Analyzing company roles and services** I need to check more details and continue exploring');
  assert(leak.isModelCause === false, '过短的推理片段不应误判');

  const leakedLong = classifyModelOriginIssue('**Analyzing company roles and services** I need to check this and I still need to inspect multiple modules before I can answer. I should validate backend and frontend mismatches and then continue with more analysis about runtime safety.');
  assert(leakedLong.isModelCause === true, '长推理泄露应识别为模型原因');
  assert(leakedLong.kind === 'reasoning_leak', `推理泄露 kind 异常: ${leakedLong.kind}`);

  const wrapped = toModelOriginUserMessage('LLM 执行失败: LLM 响应为空：流式传输完成但未收到有效内容');
  assert(!wrapped.includes('LLM 执行失败'), '用户文案不应包含旧包装前缀');
}

function testMessageFactoryNormalization() {
  const { MessageFactory } = loadCompiledModule(path.join('orchestrator', 'core', 'message-factory.js'));
  const { MessageType, ControlMessageType } = loadCompiledModule(path.join('protocol', 'message-protocol.js'));

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

  const factory = new MessageFactory(pipeline, 'trace-model-governance');
  factory.workerError('codex', 'LLM 执行失败: LLM 响应为空：流式传输完成但未收到有效内容');
  factory.taskRejected('req-model-1', 'Error during LLM edit generation: model returned non-string content');

  const workerErrorMsg = emitted.find((msg) => msg.type === MessageType.ERROR);
  assert(workerErrorMsg, '未捕获到 Worker ERROR 消息');
  const workerErrorText = workerErrorMsg.blocks?.[0]?.content || '';
  assert(!workerErrorText.includes('LLM 执行失败'), 'Worker ERROR 文案仍包含旧前缀');
  assert(workerErrorText.includes('模型本轮未返回可执行内容'), `Worker ERROR 文案未归一化: ${workerErrorText}`);

  const rejectedMsg = emitted.find(
    (msg) => msg.control?.controlType === ControlMessageType.TASK_REJECTED,
  );
  assert(rejectedMsg, '未捕获到 TASK_REJECTED 控制消息');
  const payload = rejectedMsg.control?.payload || {};
  assert(payload.reason && String(payload.reason).includes('模型本轮未返回可执行内容'),
    `TASK_REJECTED reason 未归一化: ${JSON.stringify(payload)}`);
  assert(payload.modelOriginIssue === true, 'TASK_REJECTED 应标记 modelOriginIssue=true');
}

function testSourceGuardrails() {
  const workerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'worker', 'autonomous-worker.ts'),
    'utf8',
  );
  assert(!workerSource.includes('new Error(`LLM 执行失败:'),
    'Worker 仍存在 LLM 执行失败包装（应由统一分类器处理）');

  const fileExecutorSource = fs.readFileSync(
    path.join(ROOT, 'src', 'tools', 'file-executor.ts'),
    'utf8',
  );
  assert(fileExecutorSource.includes('toModelOriginUserMessage'),
    'file_executor 尚未接入模型异常友好化');
}

async function main() {
  testModelClassification();
  testMessageFactoryNormalization();
  testSourceGuardrails();

  console.log('\n=== 模型异常治理回归结果 ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'classification',
      'message_factory_normalization',
      'worker_source_guardrail',
      'file_executor_guardrail',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('模型异常治理回归失败:', error?.stack || error);
  process.exit(1);
});
