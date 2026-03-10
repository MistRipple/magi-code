#!/usr/bin/env node
/**
 * 流式空闲超时回归脚本
 *
 * 覆盖目标：
 * 1) 持续有 chunk 输出时，不应因总时长超过阈值而中断
 * 2) 仅当超过空闲阈值无 chunk 到达时，才应触发超时
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

function createAbortError() {
  try {
    return new DOMException('The operation was aborted.', 'AbortError');
  } catch {
    const error = new Error('The operation was aborted.');
    error.name = 'AbortError';
    return error;
  }
}

async function sleepWithSignal(ms, signal) {
  if (ms <= 0) return;
  if (signal?.aborted) throw createAbortError();
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      cleanup();
      resolve();
    }, ms);
    const onAbort = () => {
      cleanup();
      reject(createAbortError());
    };
    const cleanup = () => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}

function createFakeProtocolAdapter(streamImpl) {
  return {
    provider: 'openai',
    protocol: 'openai.responses',
    capabilities: {
      supportsTextIO: true,
      supportsImageInput: true,
      supportsFunctionTools: true,
      supportsToolChoice: true,
      supportsParallelToolCalls: 'supported',
      supportsThinkingStream: 'supported',
      supportsStatefulConversation: 'supported',
    },
    async send() {
      throw new Error('本回归不应调用 send');
    },
    stream: streamImpl,
  };
}

async function testContinuousChunksShouldNotTimeout() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient({
    provider: 'openai',
    model: 'gpt-5',
    apiKey: 'test-key',
    baseUrl: 'http://127.0.0.1:1',
    enabled: true,
  });

  const chunkCount = 5;
  const chunkIntervalMs = 40;
  const idleTimeoutMs = 80;
  const chunks = [];

  client.protocolAdapter = createFakeProtocolAdapter(async (request, onEvent) => {
    for (let i = 0; i < chunkCount; i += 1) {
      await sleepWithSignal(chunkIntervalMs, request.signal);
      onEvent({ type: 'content_delta', content: `C${i}` });
    }
    return {
      content: chunks.join(''),
      usage: { inputTokens: 10, outputTokens: chunkCount },
      stopReason: 'end_turn',
    };
  });

  const startedAt = Date.now();
  const response = await client.streamMessage({
    messages: [{ role: 'user', content: 'stream idle timeout keepalive test' }],
    stream: true,
    streamIdleTimeoutMs: idleTimeoutMs,
    retryPolicy: { maxRetries: 1, baseDelayMs: 0, retryOnTimeout: true },
  }, (chunk) => {
    if (chunk.type === 'content_delta' && chunk.content) {
      chunks.push(chunk.content);
    }
  });
  const elapsed = Date.now() - startedAt;

  assert(chunks.length === chunkCount, `应收到 ${chunkCount} 个 chunk，实际: ${chunks.length}`);
  assert(elapsed >= chunkIntervalMs * chunkCount, `总耗时应超过空闲阈值，实际: ${elapsed}ms`);
  assert(response.stopReason === 'end_turn', `stopReason 异常: ${response.stopReason}`);
}

async function testIdleGapShouldTimeout() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient({
    provider: 'openai',
    model: 'gpt-5',
    apiKey: 'test-key',
    baseUrl: 'http://127.0.0.1:1',
    enabled: true,
  });

  const idleTimeoutMs = 80;
  let abortObserved = false;

  client.protocolAdapter = createFakeProtocolAdapter(async (request, onEvent) => {
    onEvent({ type: 'content_delta', content: 'start' });
    try {
      await sleepWithSignal(idleTimeoutMs + 120, request.signal);
    } catch (error) {
      abortObserved = true;
      throw error;
    }
    onEvent({ type: 'content_delta', content: 'should-not-arrive' });
    return {
      content: 'should-not-arrive',
      usage: { inputTokens: 1, outputTokens: 1 },
      stopReason: 'end_turn',
    };
  });

  let caught;
  try {
    await client.streamMessage({
      messages: [{ role: 'user', content: 'stream idle timeout should fire' }],
      stream: true,
      streamIdleTimeoutMs: idleTimeoutMs,
      retryPolicy: { maxRetries: 1, baseDelayMs: 0, retryOnTimeout: true },
    }, () => {});
  } catch (error) {
    caught = error;
  }

  assert(abortObserved, '空闲超时应触发底层 signal abort');
  assert(caught, '空闲超时场景应抛错');
  assert(caught.code === 'ETIMEDOUT', `空闲超时错误码应为 ETIMEDOUT，实际: ${String(caught.code)}`);
  assert(
    String(caught.message || '').includes('Stream idle timed out'),
    `空闲超时错误信息异常: ${String(caught.message)}`,
  );
}

async function main() {
  await testContinuousChunksShouldNotTimeout();
  await testIdleGapShouldTimeout();

  console.log('\n=== stream idle timeout regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'stream-continuous-output-no-absolute-cutoff',
      'stream-idle-gap-timeout',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('stream idle timeout 回归失败:', error?.stack || error);
  process.exit(1);
});
