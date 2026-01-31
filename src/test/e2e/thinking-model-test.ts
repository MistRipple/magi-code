/**
 * 测试 thinking 模型
 */

import { UniversalLLMClient } from '../../llm/clients/universal-client';
import { LLMConfig } from '../../types/agent-types';
import { LLMStreamChunk } from '../../llm/types';

const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  red: '\x1b[31m',
};

function log(prefix: string, message: string, color: string = colors.reset) {
  console.log(`${color}[${prefix}]${colors.reset} ${message}`);
}

async function testThinkingModel() {
  console.log('\n' + '='.repeat(60));
  console.log(' Thinking 模型测试');
  console.log('='.repeat(60) + '\n');

  // 使用带 -thinking 后缀的模型
  const config: LLMConfig = {
    provider: 'anthropic',
    baseUrl: 'http://38.165.40.233:8317',
    apiKey: 'xt-sk-1b7b63eee3378bd76ad7cf58ae4c01e6c3b497d92e09c358',
    model: 'gemini-claude-sonnet-4-5-thinking',  // 带 -thinking 后缀
    enabled: true,
  };

  log('CONFIG', `Model: ${config.model}`, colors.cyan);
  log('CONFIG', `BaseUrl: ${config.baseUrl}`, colors.cyan);

  const client = new UniversalLLMClient(config);

  let thinkingChunks = 0;
  let contentChunks = 0;
  let thinkingContent = '';
  let responseContent = '';

  try {
    const response = await client.streamMessage(
      {
        messages: [{ role: 'user', content: '请用一句话解释为什么天空是蓝色的。' }],
        maxTokens: 8192,
      },
      (chunk: LLMStreamChunk) => {
        if (chunk.type === 'thinking' && chunk.thinking) {
          thinkingChunks++;
          thinkingContent += chunk.thinking;
          if (thinkingChunks <= 5) {
            log('THINKING', `[chunk ${thinkingChunks}] ${chunk.thinking.substring(0, 60)}...`, colors.magenta);
          }
        } else if (chunk.type === 'content_delta' && chunk.content) {
          contentChunks++;
          responseContent += chunk.content;
          if (contentChunks <= 3) {
            log('CONTENT', `[chunk ${contentChunks}] ${chunk.content.substring(0, 60)}...`, colors.green);
          }
        }
      }
    );

    console.log('\n' + '-'.repeat(60));
    console.log(' 测试结果');
    console.log('-'.repeat(60) + '\n');

    log('STATS', `Thinking chunks: ${thinkingChunks}`, colors.cyan);
    log('STATS', `Content chunks: ${contentChunks}`, colors.cyan);
    log('STATS', `Thinking 总长度: ${thinkingContent.length} 字符`, colors.cyan);
    log('STATS', `Response 总长度: ${responseContent.length} 字符`, colors.cyan);

    if (thinkingChunks > 0) {
      log('✓ PASS', 'Thinking 输出正常工作!', colors.green);
      console.log('\n--- Thinking 内容摘要 ---');
      console.log(thinkingContent.substring(0, 500) + (thinkingContent.length > 500 ? '...' : ''));
    } else {
      log('✗ FAIL', 'Thinking 输出未收到任何 chunk', colors.red);
    }

    if (contentChunks > 0) {
      log('✓ PASS', 'Content 输出正常工作', colors.green);
      console.log('\n--- 响应内容 ---');
      console.log(responseContent);
    }

  } catch (error: any) {
    log('ERROR', error.message, colors.red);
    process.exit(1);
  }
}

testThinkingModel().catch(console.error);
