#!/usr/bin/env node
/**
 * LLM retry runtime 真相源回归脚本
 *
 * 目标：
 * 1) 首次失败后发出 scheduled
 * 2) 下一次真实开始时发出 attempt_started
 * 3) 成功/失败结束时统一发出 settled
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function createOpenAIConfig() {
  return {
    baseUrl: 'http://127.0.0.1:1',
    apiKey: 'test-key',
    model: 'gpt-5',
    provider: 'openai',
    enabled: true,
  };
}

async function testRetryRuntimeSuccessSequence() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  const events = [];
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsTools: true, supportsThinking: true },
    async send() {
      attempts += 1;
      if (attempts === 1) {
        const error = new Error('service unavailable');
        error.status = 503;
        throw error;
      }
      return {
        content: 'retry recovered',
        stopReason: 'end_turn',
        toolCalls: [],
        usage: { inputTokens: 1, outputTokens: 1 },
      };
    },
    async stream() { throw new Error('stream not used'); },
  };

  const response = await client.sendMessage({
    messages: [{ role: 'user', content: 'retry runtime success regression' }],
    retryPolicy: { maxRetries: 3, baseDelayMs: 0, retryOnTimeout: true, retryOnAllErrors: true, retryDelaysMs: [1, 1, 1] },
    retryRuntimeHook: (event) => events.push(event),
  });

  assert(response.content === 'retry recovered', `成功场景响应异常: ${response.content}`);
  assert(attempts === 2, `成功场景应尝试 2 次，实际: ${attempts}`);
  assert(events.length === 3, `成功场景事件数异常: ${events.length}`);

  const [scheduled, started, settled] = events;
  assert(scheduled.phase === 'scheduled', `首个事件必须是 scheduled，实际: ${scheduled.phase}`);
  assert(scheduled.attempt === 2 && scheduled.maxAttempts === 3, `scheduled attempt/maxAttempts 异常: ${JSON.stringify(scheduled)}`);
  assert(scheduled.delayMs === 1, `scheduled delayMs 异常: ${scheduled.delayMs}`);
  assert(Number.isFinite(scheduled.nextRetryAt), 'scheduled nextRetryAt 必须是数字时间戳');
  assert(started.phase === 'attempt_started', `第二个事件必须是 attempt_started，实际: ${started.phase}`);
  assert(started.attempt === 2 && started.maxAttempts === 3, `attempt_started 字段异常: ${JSON.stringify(started)}`);
  assert(settled.phase === 'settled' && settled.outcome === 'success', `settled(success) 异常: ${JSON.stringify(settled)}`);

  return { attempts, phases: events.map((event) => event.phase) };
}

async function testRetryRuntimeFailedSequence() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  const events = [];
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsTools: true, supportsThinking: true },
    async send() {
      attempts += 1;
      const error = new Error('Unauthorized');
      error.status = 401;
      throw error;
    },
    async stream() { throw new Error('stream not used'); },
  };

  let caught;
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'retry runtime failed regression' }],
      retryPolicy: { maxRetries: 6, baseDelayMs: 0, retryOnTimeout: true, retryOnAllErrors: true, retryDelaysMs: [1, 1, 1], deterministicErrorStreakLimit: 3 },
      retryRuntimeHook: (event) => events.push(event),
    });
  } catch (error) {
    caught = error;
  }

  assert(caught, '失败场景应抛出异常');
  assert(attempts === 3, `确定性失败应在第 3 次止损，实际: ${attempts}`);
  assert(events.map((event) => event.phase).join(',') === 'scheduled,attempt_started,scheduled,attempt_started,settled', `失败场景事件序列异常: ${JSON.stringify(events)}`);
  const settled = events[events.length - 1];
  assert(settled.phase === 'settled' && settled.outcome === 'failed', `settled(failed) 异常: ${JSON.stringify(settled)}`);

  return { attempts, phases: events.map((event) => event.phase), error: String(caught.message || caught) };
}

async function main() {
  const success = await testRetryRuntimeSuccessSequence();
  const failed = await testRetryRuntimeFailedSequence();
  console.log(JSON.stringify({ pass: true, success, failed }, null, 2));
}

main().catch((error) => {
  console.error('llm retry runtime 回归失败:', error?.stack || error);
  process.exit(1);
});