#!/usr/bin/env node

/**
 * CLI 询问功能测试脚本
 * 测试 interaction 消息流是否正确工作
 */

const { IntelligentOrchestrator } = require('../out/orchestrator/intelligent-orchestrator');
const { logger } = require('../out/logging');

async function testCliQuestion() {
  console.log('\n=== CLI 询问功能测试 ===\n');

  const orchestrator = new IntelligentOrchestrator({
    cwd: process.cwd(),
    interactionMode: 'manual',
  });

  try {
    // 测试场景：执行一个会触发 CLI 询问的任务
    // 例如：使用 git 命令但没有配置用户信息
    console.log('📝 测试任务：执行可能触发 CLI 询问的命令\n');

    const result = await orchestrator.execute(
      '使用 echo 命令输出 "测试 CLI 询问功能"，然后等待 2 秒',
      {
        conversationContext: {
          conversationId: 'test-cli-question',
          turnId: 'turn-1',
        }
      }
    );

    console.log('\n✅ 测试完成');
    console.log('结果:', result);

  } catch (error) {
    console.error('\n❌ 测试失败:', error.message);
    throw error;
  } finally {
    await orchestrator.cleanup();
  }
}

// 运行测试
testCliQuestion().catch(error => {
  console.error('测试异常:', error);
  process.exit(1);
});
