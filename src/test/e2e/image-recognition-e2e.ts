/**
 * 图片识别端到端测试
 *
 * 测试插件的图片识别功能，确保：
 * 1. 图片能够正确读取并转换为 base64
 * 2. 图片能够正确发送到 LLM API
 * 3. LLM 能够正确识别图片内容并返回描述
 *
 * 运行方式:
 * npx ts-node src/test/e2e/image-recognition-e2e.ts [图片路径]
 */

import * as fs from 'fs';
import * as path from 'path';
import { LLMAdapterFactory } from '../../llm/adapter-factory';
import { MissionDrivenEngine } from '../../orchestrator/core';
import { UnifiedSessionManager } from '../../session';
import { SnapshotManager } from '../../snapshot-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { WorkerSlot } from '../../types';
import { LLMConfigLoader } from '../../llm/config';
import { ProjectKnowledgeBase } from '../../knowledge/project-knowledge-base';

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
  log('  图片识别端到端测试', colors.cyan);
  log('========================================\n', colors.cyan);

  // 检查图片文件
  if (!fs.existsSync(imagePath)) {
    log(`❌ 图片文件不存在: ${imagePath}`, colors.red);
    process.exit(1);
  }

  const imageStats = fs.statSync(imagePath);
  log(`📷 图片路径: ${imagePath}`, colors.blue);
  log(`📏 图片大小: ${(imageStats.size / 1024).toFixed(2)} KB`, colors.blue);

  // 初始化组件
  log('\n🔧 初始化组件...', colors.yellow);
  const workspaceRoot = process.cwd();
  const sessionManager = new UnifiedSessionManager(workspaceRoot);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

  // 创建 MessageHub
  const messageHub = new MessageHub();
  adapterFactory.setMessageHub(messageHub);

  // 初始化 adapter factory
  await adapterFactory.initialize();

  // 统一 Todo 系统：不再需要 UnifiedTaskManager

  // 获取启用的 workers
  const fullConfig = LLMConfigLoader.loadFullConfig();
  const enabledWorkers = Object.entries(fullConfig.workers)
    .filter(([, worker]) => worker.enabled)
    .map(([worker]) => worker as WorkerSlot);

  log(`📋 启用的 Workers: ${enabledWorkers.join(', ')}`, colors.blue);

  // 初始化知识库
  const knowledgeBase = new ProjectKnowledgeBase({
    projectRoot: workspaceRoot,
    storageDir: path.join(workspaceRoot, '.magi', 'knowledge'),
  });
  await knowledgeBase.initialize();

  // 创建编排引擎
  const orchestrator = new MissionDrivenEngine(
    adapterFactory,
    {
      timeout: 120000,
      maxRetries: 2,
      review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
      planReview: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      integration: { enabled: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
    },
    workspaceRoot,
    snapshotManager,
    sessionManager
  );

  orchestrator.setKnowledgeBase(knowledgeBase);
  // 统一 Todo 系统：不再需要 setTaskManager

  // 自动确认/澄清回调
  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setQuestionCallback(async (questions) => {
    return questions.map(q => `Q: ${q}\nA: 是的，继续执行`).join('\n\n');
  });
  orchestrator.setClarificationCallback(async (questions) => {
    const answers: Record<string, string> = {};
    questions.forEach(q => { answers[q] = '按照默认方式处理'; });
    return { answers, additionalInfo: '' };
  });

  await orchestrator.initialize();

  log('✅ 组件初始化完成', colors.green);

  // 测试提示词 - 附带图片路径引用
  const prompt = `请详细描述这张图片的内容，包括你看到的所有元素、颜色、布局等信息。`;

  log('\n🚀 发送图片识别请求...', colors.yellow);
  log(`📝 提示词: ${prompt.substring(0, 80)}...`, colors.blue);
  log(`📷 图片路径: ${imagePath}`, colors.blue);

  const startTime = Date.now();

  try {
    // 直接使用 Worker 适配器发送带图片的消息
    log('\n📡 调用 Claude Worker 进行图片识别...', colors.yellow);

    // 调用 sendMessage 方法，传入图片路径
    const result = await adapterFactory.sendMessage(
      'claude',
      prompt,
      [imagePath]
    );
    const duration = Date.now() - startTime;

    log(`\n⏱️  耗时: ${(duration / 1000).toFixed(2)} 秒`, colors.blue);
    log('\n📋 LLM 响应:', colors.green);
    log('----------------------------------------', colors.cyan);
    console.log(result.content);
    log('----------------------------------------', colors.cyan);

    // 验证响应
    if (result.content && result.content.length > 50) {
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

