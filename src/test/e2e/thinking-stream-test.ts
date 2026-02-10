/**
 * Thinking 流式输出测试
 *
 * 测试多种模型的 thinking/reasoning 输出是否正确流式传输
 *
 * 运行方式:
 * npx ts-node src/test/e2e/thinking-stream-test.ts
 */

import { UniversalLLMClient } from '../../llm/clients/universal-client';
import { LLMConfig } from '../../types/agent-types';
import { LLMStreamChunk } from '../../llm/types';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  bright: '\x1b[1m',
  dim: '\x1b[2m',
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

// 加载 LLM 配置
function loadLLMConfig(): any {
  const configPath = path.join(os.homedir(), '.magi', 'llm.json');
  if (fs.existsSync(configPath)) {
    return JSON.parse(fs.readFileSync(configPath, 'utf-8'));
  }
  throw new Error(`配置文件不存在: ${configPath}`);
}

async function testThinkingStream() {
  console.log('\n' + '='.repeat(60));
  console.log(' Thinking 流式输出测试 (多模型)');
  console.log('='.repeat(60) + '\n');

  // 加载配置
  const llmConfig = loadLLMConfig();

  // 测试 Claude Worker (使用 Anthropic provider)
  const claudeConfig = llmConfig.workers?.claude;
  const codexConfig = llmConfig.workers?.codex;
  const geminiConfig = llmConfig.workers?.gemini;

  // 选择可用的 worker
  const workerName = process.argv[2] || 'claude';
  let workerConfig: any;
  switch (workerName) {
    case 'codex':
      workerConfig = codexConfig;
      break;
    case 'gemini':
      workerConfig = geminiConfig;
      break;
    default:
      workerConfig = claudeConfig;
  }

  if (!workerConfig || !workerConfig.enabled) {
    log('SKIP', `${workerName} Worker 未启用`, colors.yellow);
    return;
  }

  const config: LLMConfig = {
    provider: workerConfig.provider || 'anthropic',
    baseUrl: workerConfig.baseUrl,
    apiKey: workerConfig.apiKey,
    model: workerConfig.model,
    enabled: true,
  };

  log('CONFIG', `Worker: ${workerName}`, colors.cyan);
  log('CONFIG', `Provider: ${config.provider}`, colors.cyan);
  log('CONFIG', `Model: ${config.model}`, colors.cyan);
  log('CONFIG', `BaseUrl: ${config.baseUrl}`, colors.cyan);

  const client = new UniversalLLMClient(config);

  // 测试统计
  let thinkingChunks = 0;
  let contentChunks = 0;
  let thinkingContent = '';
  let responseContent = '';

  console.log('\n' + '-'.repeat(60));
  console.log(' 开始流式测试...');
  console.log('-'.repeat(60) + '\n');

  try {
    const response = await client.streamMessage(
      {
        messages: [
          {
            role: 'user',
            content: '请用 3-5 步解释为什么天空是蓝色的。',
          },
        ],
        maxTokens: 4096,
        temperature: 0.7,
      },
      (chunk: LLMStreamChunk) => {
        if (chunk.type === 'thinking' && chunk.thinking) {
          thinkingChunks++;
          thinkingContent += chunk.thinking;
          // 只打印前几个 thinking chunk 作为示例
          if (thinkingChunks <= 3) {
            log('THINKING', `[chunk ${thinkingChunks}] ${chunk.thinking.substring(0, 50)}...`, colors.magenta);
          } else if (thinkingChunks === 4) {
            log('THINKING', `... 更多 thinking 输出中 ...`, colors.dim);
          }
        } else if (chunk.type === 'content_delta' && chunk.content) {
          contentChunks++;
          responseContent += chunk.content;
          // 只打印前几个 content chunk 作为示例
          if (contentChunks <= 3) {
            log('CONTENT', `[chunk ${contentChunks}] ${chunk.content.substring(0, 50)}...`, colors.green);
          } else if (contentChunks === 4) {
            log('CONTENT', `... 更多内容输出中 ...`, colors.dim);
          }
        } else if (chunk.type === 'content_start') {
          log('EVENT', 'content_start', colors.blue);
        } else if (chunk.type === 'content_end') {
          log('EVENT', 'content_end', colors.blue);
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

    console.log('\n' + '-'.repeat(60));
    console.log(' 验证结果');
    console.log('-'.repeat(60) + '\n');

    // 验证
    const hasThinking = thinkingChunks > 0;
    const hasContent = contentChunks > 0;
    const hasResponse = response.content.length > 0;

    if (hasThinking) {
      log('✓ PASS', 'Thinking 流式输出正常工作', colors.green);
    } else {
      log('✗ FAIL', 'Thinking 流式输出未收到任何 chunk', colors.red);
      log('INFO', '注意: 需要使用支持 extended thinking 的模型 (claude-3-5-sonnet 等)', colors.yellow);
    }

    if (hasContent) {
      log('✓ PASS', 'Content 流式输出正常工作', colors.green);
    } else {
      log('✗ FAIL', 'Content 流式输出未收到任何 chunk', colors.red);
    }

    if (hasResponse) {
      log('✓ PASS', '最终响应正确返回', colors.green);
    } else {
      log('✗ FAIL', '最终响应为空', colors.red);
    }

    console.log('\n' + '='.repeat(60));
    if (hasContent && hasResponse) {
      console.log(colors.green + ' 测试通过! 流式输出工作正常' + colors.reset);
      if (!hasThinking) {
        console.log(colors.yellow + ' 注意: Thinking 输出需要特定模型支持' + colors.reset);
      }
    } else {
      console.log(colors.red + ' 测试失败!' + colors.reset);
    }
    console.log('='.repeat(60) + '\n');

    // 打印 Thinking 内容摘要
    if (thinkingContent) {
      console.log('\n--- Thinking 内容摘要 ---');
      console.log(thinkingContent.substring(0, 500) + (thinkingContent.length > 500 ? '...' : ''));
    }

    // 打印响应内容摘要
    console.log('\n--- 响应内容摘要 ---');
    console.log(responseContent.substring(0, 500) + (responseContent.length > 500 ? '...' : ''));

  } catch (error: any) {
    console.log('\n' + '-'.repeat(60));
    log('ERROR', `测试失败: ${error.message}`, colors.red);
    console.log('-'.repeat(60));

    if (error.message.includes('401')) {
      log('HINT', 'API Key 可能无效，请检查 ANTHROPIC_API_KEY', colors.yellow);
    } else if (error.message.includes('model')) {
      log('HINT', '模型可能不支持，尝试更换为其他 Claude 模型', colors.yellow);
    }

    process.exit(1);
  }
}

// 运行测试
testThinkingStream().catch(console.error);
