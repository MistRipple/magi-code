/**
 * 简单测试：直接测试 InteractiveSession（基于 --continue 的新实现）
 */

import { InteractiveSession } from '../src/cli/session/interactive-session';

async function testInteractiveSession() {
  console.log('='.repeat(60));
  console.log('测试 InteractiveSession（基于 --continue）');
  console.log('='.repeat(60));

  const session = new InteractiveSession({
    cli: 'claude',
    cwd: process.cwd(),
    command: 'claude',
    args: ['-p', '--output-format', 'stream-json', '--verbose'],
    idleTimeoutMs: 60000,
    sessionId: 'test-session',
  });

  // 监听事件
  session.on('log', (msg) => console.log(`[LOG] ${msg}`));
  session.on('output', (chunk) => process.stdout.write(`[OUT] ${chunk}`));
  session.on('error', (err) => console.log(`[ERR] ${err}`));

  try {
    console.log('\n--- 启动会话 ---');
    await session.start();
    console.log(`会话存活: ${session.isAlive}`);

    console.log('\n--- 发送第一条消息 ---');
    const startTime1 = Date.now();

    const response1 = await session.send({
      requestId: 'test-1',
      cli: 'claude',
      role: 'orchestrator',
      content: '请记住数字 456，然后只回复"已记住"',
    });

    const elapsed1 = Date.now() - startTime1;
    console.log(`\n--- 响应 1 (${elapsed1}ms) ---`);
    console.log(`内容: ${response1.content}`);
    console.log(`Token: ${JSON.stringify(response1.tokenUsage)}`);

    console.log('\n--- 发送第二条消息（验证会话持续性）---');
    const startTime2 = Date.now();

    const response2 = await session.send({
      requestId: 'test-2',
      cli: 'claude',
      role: 'orchestrator',
      content: '我之前让你记住的数字是什么？只回复数字',
    });

    const elapsed2 = Date.now() - startTime2;
    console.log(`\n--- 响应 2 (${elapsed2}ms) ---`);
    console.log(`内容: ${response2.content}`);
    console.log(`Token: ${JSON.stringify(response2.tokenUsage)}`);

    if (response2.content.includes('456')) {
      console.log('\n✅ 测试成功：会话上下文保持正常！');
    } else {
      console.log('\n⚠️ 测试警告：会话上下文可能未保持');
    }
  } catch (error) {
    console.log(`\n❌ 测试失败: ${error}`);
  } finally {
    console.log('\n--- 停止会话 ---');
    await session.stop();
  }
}

testInteractiveSession().catch(console.error);

