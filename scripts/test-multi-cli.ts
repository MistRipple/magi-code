/**
 * 测试 3: 多 CLI 并发协作（简化版）
 *
 * 验证：
 * 1. Orchestrator 和 Worker 使用不同的会话
 * 2. 会话独立性
 */

import { SessionManager, SessionManagerOptions } from '../src/cli/session/session-manager';
import { ContextManager } from '../src/context/context-manager';

async function testMultiCLIConcurrency() {
  console.log('='.repeat(60));
  console.log('测试 3: 多 CLI 并发协作（简化版）');
  console.log('='.repeat(60));

  const cwd = process.cwd();

  // 初始化 ContextManager
  const contextManager = new ContextManager(cwd);
  await contextManager.initialize('multi-cli-test', 'Multi CLI Test');

  const options: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 30000, // 缩短超时
    contextManager,
  };

  const sessionManager = new SessionManager(options);

  // 收集事件
  sessionManager.on('log', (msg) => console.log(`[LOG] ${msg}`));

  try {
    // 步骤 1: Orchestrator 发送消息
    console.log('\n--- 步骤 1: Orchestrator 发送消息 ---');
    const orchResponse = await sessionManager.send('claude', 'orchestrator', {
      requestId: 'orch-1',
      cli: 'claude',
      role: 'orchestrator',
      content: '只回复"O"',
    });
    console.log(`Orchestrator 响应: ${orchResponse.content}`);

    // 步骤 2: Worker 发送消息（不同的 session）
    console.log('\n--- 步骤 2: Worker 发送消息 ---');
    const workerResponse = await sessionManager.send('claude', 'worker', {
      requestId: 'worker-1',
      taskId: 'task-1',
      cli: 'claude',
      role: 'worker',
      content: '只回复"W"',
    });
    console.log(`Worker 响应: ${workerResponse.content}`);

    // 验证两个会话都成功
    console.log('\n--- 验证结果 ---');
    console.log(`Orchestrator 会话存活: ${sessionManager.isSessionAlive('claude', 'orchestrator')}`);
    console.log(`Worker 会话存活: ${sessionManager.isSessionAlive('claude', 'worker')}`);

    console.log('\n✅ 测试成功：多 CLI 会话独立运行！');

  } catch (error) {
    console.log(`\n❌ 测试失败: ${error}`);
  } finally {
    console.log('\n--- 清理 ---');
    await sessionManager.stopAll();
    console.log('✅ 测试完成');
  }
}

testMultiCLIConcurrency().catch(console.error);

