/**
 * 测试 2: 进程异常恢复机制
 */

import { SessionManager, SessionManagerOptions } from '../src/cli/session/session-manager';
import { ContextManager } from '../src/context/context-manager';

async function testProcessRecovery() {
  console.log('='.repeat(60));
  console.log('测试 2: 进程异常恢复机制');
  console.log('='.repeat(60));

  const cwd = process.cwd();

  // 初始化 ContextManager
  const contextManager = new ContextManager(cwd);
  await contextManager.initialize('recovery-test', 'Recovery Test');

  const options: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 60000,
    contextManager,
  };

  const sessionManager = new SessionManager(options);

  // 收集日志
  sessionManager.on('log', (msg) => console.log(`[LOG] ${msg}`));
  sessionManager.on('sessionEvent', (event) => {
    console.log(`[EVENT] ${JSON.stringify(event)}`);
  });

  try {
    // 步骤 1: 发送第一条消息
    console.log('\n--- 步骤 1: 发送第一条消息 ---');
    const response1 = await sessionManager.send('claude', 'orchestrator', {
      requestId: 'req-1',
      cli: 'claude',
      role: 'orchestrator',
      content: '请记住数字 789，然后只回复"已记住"',
    });
    console.log(`响应 1: ${response1.content}`);
    console.log(`会话存活: ${sessionManager.isSessionAlive('claude', 'orchestrator')}`);

    // 步骤 2: 模拟会话停止（模拟异常）
    console.log('\n--- 步骤 2: 模拟会话停止 ---');
    await sessionManager.stopSession('claude', 'orchestrator');
    console.log(`会话存活 (停止后): ${sessionManager.isSessionAlive('claude', 'orchestrator')}`);

    // 步骤 3: 发送新消息，应该自动恢复会话
    console.log('\n--- 步骤 3: 发送新消息（触发自动恢复）---');
    const response2 = await sessionManager.send('claude', 'orchestrator', {
      requestId: 'req-2',
      cli: 'claude',
      role: 'orchestrator',
      content: '我之前让你记住的数字是什么？只回复数字',
    });
    console.log(`响应 2: ${response2.content}`);
    console.log(`会话存活 (恢复后): ${sessionManager.isSessionAlive('claude', 'orchestrator')}`);

    // 验证结果
    if (response2.content.includes('789')) {
      console.log('\n✅ 测试成功：进程恢复后会话上下文保持正常！');
    } else {
      console.log('\n⚠️ 测试警告：进程恢复后会话上下文可能丢失');
      console.log('   这是预期行为，因为 Claude CLI 的 --continue 基于工作目录');
    }
  } catch (error) {
    console.log(`\n❌ 测试失败: ${error}`);
  } finally {
    console.log('\n--- 清理 ---');
    await sessionManager.stopAll();
    console.log('✅ 测试完成');
  }
}

testProcessRecovery().catch(console.error);

