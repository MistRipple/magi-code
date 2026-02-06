/**
 * 消息类别测试 - 验证消息是 CONTENT 还是 DATA
 *
 * 这个测试文件用于验证：
 * 1. messageHub.result() 创建的消息类别是 CONTENT
 * 2. messageHub.orchestratorMessage() 创建的消息类别是 CONTENT
 * 3. messageHub.data() 创建的消息类别是 DATA
 */

import { MessageHub } from '../../orchestrator/core/message-hub';
import { MessageCategory } from '../../protocol/message-protocol';

async function testMessageCategories() {
  console.log('=== 消息类别测试 ===\n');

  const hub = new MessageHub('test-trace');

  // 收集发送的消息
  const sentMessages: Array<{ id: string; category: string; type: string; content?: string }> = [];

  hub.on('unified:message', (message) => {
    sentMessages.push({
      id: message.id,
      category: message.category,
      type: message.type,
      content: message.blocks?.[0]?.type === 'text' ? message.blocks[0].content?.substring(0, 50) : undefined,
    });
    console.log(`[收到消息] id=${message.id}, category=${message.category}, type=${message.type}`);
  });

  // 测试 1: result() 应该是 CONTENT
  console.log('\n--- 测试 1: result() ---');
  hub.result('这是一个测试结果');

  // 测试 2: orchestratorMessage() 应该是 CONTENT
  console.log('\n--- 测试 2: orchestratorMessage() ---');
  hub.orchestratorMessage('这是一个编排者消息');

  // 测试 3: data() 应该是 DATA
  console.log('\n--- 测试 3: data() ---');
  hub.data('stateUpdate', { test: true });

  // 等待事件处理
  await new Promise(resolve => setTimeout(resolve, 100));

  // 验证结果
  console.log('\n=== 测试结果 ===');
  console.log('发送的消息:', JSON.stringify(sentMessages, null, 2));

  const resultMsg = sentMessages.find(m => m.type === 'result');
  const orchestratorMsg = sentMessages.find(m => m.type === 'text');
  const dataMsg = sentMessages.find(m => m.category === MessageCategory.DATA);

  console.log('\n验证:');
  console.log(`- result() 类别: ${resultMsg?.category} (期望: ${MessageCategory.CONTENT})`);
  console.log(`  ✓ 正确: ${resultMsg?.category === MessageCategory.CONTENT}`);

  console.log(`- orchestratorMessage() 类别: ${orchestratorMsg?.category} (期望: ${MessageCategory.CONTENT})`);
  console.log(`  ✓ 正确: ${orchestratorMsg?.category === MessageCategory.CONTENT}`);

  console.log(`- data() 类别: ${dataMsg?.category} (期望: ${MessageCategory.DATA})`);
  console.log(`  ✓ 正确: ${dataMsg?.category === MessageCategory.DATA}`);

  // 清理
  hub.dispose();

  const allCorrect =
    resultMsg?.category === MessageCategory.CONTENT &&
    orchestratorMsg?.category === MessageCategory.CONTENT &&
    dataMsg?.category === MessageCategory.DATA;

  if (allCorrect) {
    console.log('\n✅ 所有测试通过！消息类别设置正确。');
  } else {
    console.log('\n❌ 测试失败！消息类别设置有误。');
  }

  return allCorrect;
}

// 运行测试
testMessageCategories().then(success => {
  process.exit(success ? 0 : 1);
}).catch(error => {
  console.error('测试出错:', error);
  process.exit(1);
});
