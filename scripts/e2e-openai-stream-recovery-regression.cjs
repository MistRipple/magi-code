#!/usr/bin/env node
/**
 * OpenAI Responses 流中断恢复回归脚本
 *
 * 覆盖目标：
 * 1) 流式输出已开始后发生网络中断，不直接失败
 * 2) 基于 response_id 自动 retrieve 并补齐剩余文本
 * 3) 最终返回完整 content，前端流事件收到补齐 delta + content_end
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

async function main() {
  const { OpenAIResponsesProtocolAdapter } = loadCompiledModule(path.join('llm', 'protocol', 'adapters', 'openai-responses-adapter.js'));

  let retrieveCalls = 0;
  const fakeClient = {
    responses: {
      async create() {
        const events = [
          {
            type: 'response.created',
            response: { id: 'resp_recover_1', status: 'in_progress' },
          },
          {
            type: 'response.output_text.delta',
            delta: 'Hello ',
          },
        ];
        return {
          [Symbol.asyncIterator]() {
            let idx = 0;
            return {
              async next() {
                if (idx < events.length) {
                  return { done: false, value: events[idx++] };
                }
                throw new Error('fetch failed');
              },
            };
          },
        };
      },
      async retrieve(responseId) {
        retrieveCalls += 1;
        assert(responseId === 'resp_recover_1', `responseId 不匹配: ${responseId}`);
        return {
          id: responseId,
          status: 'completed',
          output_text: 'Hello world',
          usage: {
            input_tokens: 12,
            output_tokens: 3,
          },
        };
      },
    },
  };

  const adapter = new OpenAIResponsesProtocolAdapter(
    {
      provider: 'openai',
      model: 'gpt-5',
      apiKey: 'test-key',
      baseUrl: 'http://127.0.0.1:1',
      enabled: true,
    },
    fakeClient,
  );

  const chunks = [];
  const response = await adapter.stream(
    {
      messages: [{ role: 'user', content: 'stream recovery test' }],
      stream: true,
    },
    (chunk) => {
      chunks.push(chunk);
    },
  );

  const deltaText = chunks
    .filter((c) => c.type === 'content_delta')
    .map((c) => c.content || '')
    .join('');
  const hasContentEnd = chunks.some((c) => c.type === 'content_end');

  assert(retrieveCalls >= 1, `流中断后应触发 retrieve 恢复，实际调用: ${retrieveCalls}`);
  assert(response.content === 'Hello world', `最终内容应完整恢复，实际: ${response.content}`);
  assert(deltaText === 'Hello world', `流式 delta 应补齐完整文本，实际: ${deltaText}`);
  assert(hasContentEnd, '恢复链路应补发 content_end');

  console.log('\n=== openai stream recovery regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'stream-interruption-recovered-by-response-id',
      'delta-backfill-after-retrieve',
      'final-content-complete',
    ],
    retrieveCalls,
    chunkCount: chunks.length,
    response,
  }, null, 2));
}

main().catch((error) => {
  console.error('openai stream recovery 回归失败:', error?.stack || error);
  process.exit(1);
});

