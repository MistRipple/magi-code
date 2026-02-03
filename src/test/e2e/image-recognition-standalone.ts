/**
 * 图片识别独立测试（不依赖 VS Code）
 *
 * 直接调用 Claude API 测试图片识别功能
 * 使用自定义 baseUrl（代理服务器）
 *
 * 运行方式:
 * npx ts-node src/test/e2e/image-recognition-standalone.ts [图片路径]
 */

import * as fs from 'fs';
import * as path from 'path';
import Anthropic from '@anthropic-ai/sdk';
import { LLMConfigLoader } from '../../llm/config';

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
};

function log(msg: string, color = colors.reset) {
  console.log(`${color}${msg}${colors.reset}`);
}

/**
 * 测试图片识别
 */
async function testImageRecognition(imagePath: string): Promise<void> {
  log('\n========================================', colors.cyan);
  log('  图片识别端到端测试（独立模式）', colors.cyan);
  log('========================================\n', colors.cyan);

  // 检查图片文件
  if (!fs.existsSync(imagePath)) {
    log(`❌ 图片文件不存在: ${imagePath}`, colors.red);
    process.exit(1);
  }

  const imageStats = fs.statSync(imagePath);
  log(`📷 图片路径: ${imagePath}`, colors.blue);
  log(`📏 图片大小: ${(imageStats.size / 1024).toFixed(2)} KB`, colors.blue);

  // 读取配置
  log('\n🔧 加载 LLM 配置...', colors.yellow);
  const config = LLMConfigLoader.loadFullConfig();
  const claudeConfig = config.workers.claude;

  if (!claudeConfig?.enabled) {
    log('❌ Claude 未启用，请检查配置', colors.red);
    process.exit(1);
  }

  const apiKey = claudeConfig.apiKey || process.env.ANTHROPIC_API_KEY;
  const baseUrl = claudeConfig.baseUrl;

  if (!apiKey) {
    log('❌ 未找到 Claude API Key', colors.red);
    process.exit(1);
  }

  log(`📋 模型: ${claudeConfig.model}`, colors.blue);
  log(`📋 BaseUrl: ${baseUrl || 'https://api.anthropic.com'}`, colors.blue);
  log('✅ 配置加载完成', colors.green);

  // 读取图片并转换为 base64
  log('\n📖 读取图片...', colors.yellow);
  const imageBuffer = fs.readFileSync(imagePath);
  const base64Data = imageBuffer.toString('base64');
  const ext = path.extname(imagePath).toLowerCase().slice(1);
  const mediaType = ext === 'jpg' ? 'image/jpeg' : `image/${ext}` as 'image/jpeg' | 'image/png' | 'image/gif' | 'image/webp';

  log(`📋 图片格式: ${mediaType}`, colors.blue);
  log(`📋 Base64 长度: ${base64Data.length} 字符`, colors.blue);

  // 创建 Anthropic 客户端（使用自定义 baseUrl）
  const client = new Anthropic({
    apiKey,
    baseURL: baseUrl || undefined,
  });

  // 测试提示词
  const prompt = '请详细描述这张图片的内容，包括你看到的所有元素、颜色、布局等信息。';

  log('\n🚀 发送图片识别请求...', colors.yellow);
  log(`📝 提示词: ${prompt}`, colors.blue);

  const startTime = Date.now();

  try {
    const response = await client.messages.create({
      model: claudeConfig.model || 'claude-sonnet-4-20250514',
      max_tokens: 2048,
      messages: [
        {
          role: 'user',
          content: [
            {
              type: 'image',
              source: {
                type: 'base64',
                media_type: mediaType,
                data: base64Data,
              },
            },
            {
              type: 'text',
              text: prompt,
            },
          ],
        },
      ],
    });

    const duration = Date.now() - startTime;
    const textContent = response.content.find(c => c.type === 'text');
    const result = textContent?.type === 'text' ? textContent.text : '';

    log(`\n⏱️  耗时: ${(duration / 1000).toFixed(2)} 秒`, colors.blue);
    log(`📊 Token 使用: 输入 ${response.usage.input_tokens}, 输出 ${response.usage.output_tokens}`, colors.blue);
    log('\n📋 LLM 响应:', colors.green);
    log('----------------------------------------', colors.cyan);
    console.log(result);
    log('----------------------------------------', colors.cyan);

    // 验证响应
    if (result && result.length > 50) {
      log('\n✅ 测试通过！LLM 成功识别并描述了图片内容', colors.green);
    } else {
      log('\n⚠️  响应过短，可能识别失败', colors.yellow);
    }

  } catch (error) {
    const duration = Date.now() - startTime;
    log(`\n⏱️  耗时: ${(duration / 1000).toFixed(2)} 秒`, colors.blue);
    log(`\n❌ 测试失败: ${error}`, colors.red);
    if (error instanceof Error) {
      log(`   堆栈: ${error.stack}`, colors.red);
    }
    process.exit(1);
  }

  log('\n========================================', colors.cyan);
  log('  测试结束', colors.cyan);
  log('========================================\n', colors.cyan);
}

// 主函数
async function main() {
  const args = process.argv.slice(2);
  const imagePath = args[0] || path.join(process.cwd(), 'image.png');
  await testImageRecognition(imagePath);
}

main().catch(console.error);

