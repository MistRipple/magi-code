/**
 * 测试：Worker 模式优化效果
 *
 * 验证：Worker 使用 InteractiveSession 后的缓存效果
 */

import { SessionManager, SessionManagerOptions } from '../src/cli/session/session-manager';
import { ContextManager } from '../src/context/context-manager';

async function testWorkerOptimization() {
  console.log('='.repeat(60));
  console.log('测试 Worker 模式优化效果');
  console.log('='.repeat(60));

  const cwd = process.cwd();

  // 初始化 ContextManager
  const contextManager = new ContextManager(cwd);
  await contextManager.initialize('worker-opt-test', 'Worker Optimization Test');

  const options: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 30000,
    contextManager,
  };

  const sessionManager = new SessionManager(options);
  sessionManager.on('log', (msg) => console.log(`[LOG] ${msg}`));

  try {
    // 测试 Worker 模式
    console.log('\n--- Worker 第一次请求（冷启动）---');
    const start1 = Date.now();
    const response1 = await sessionManager.send('claude', 'worker', {
      requestId: 'worker-1',
      taskId: 'task-1',
      cli: 'claude',
      role: 'worker',
      content: '只回复"W1"',
    });
    const elapsed1 = Date.now() - start1;
    console.log(`响应: ${response1.content}`);
    console.log(`耗时: ${elapsed1}ms`);
    console.log(`Token: input=${response1.tokenUsage?.inputTokens}, cache_read=${response1.tokenUsage?.cacheReadTokens}`);

    console.log('\n--- Worker 第二次请求（利用缓存）---');
    const start2 = Date.now();
    const response2 = await sessionManager.send('claude', 'worker', {
      requestId: 'worker-2',
      taskId: 'task-1',
      cli: 'claude',
      role: 'worker',
      content: '只回复"W2"',
    });
    const elapsed2 = Date.now() - start2;
    console.log(`响应: ${response2.content}`);
    console.log(`耗时: ${elapsed2}ms`);
    console.log(`Token: input=${response2.tokenUsage?.inputTokens}, cache_read=${response2.tokenUsage?.cacheReadTokens}`);

    console.log('\n--- Worker 第三次请求（验证缓存稳定性）---');
    const start3 = Date.now();
    const response3 = await sessionManager.send('claude', 'worker', {
      requestId: 'worker-3',
      taskId: 'task-1',
      cli: 'claude',
      role: 'worker',
      content: '只回复"W3"',
    });
    const elapsed3 = Date.now() - start3;
    console.log(`响应: ${response3.content}`);
    console.log(`耗时: ${elapsed3}ms`);
    console.log(`Token: input=${response3.tokenUsage?.inputTokens}, cache_read=${response3.tokenUsage?.cacheReadTokens}`);

    // 分析结果
    console.log('\n' + '='.repeat(60));
    console.log('优化效果分析');
    console.log('='.repeat(60));
    console.log(`第一次请求: ${elapsed1}ms`);
    console.log(`第二次请求: ${elapsed2}ms (${elapsed2 < elapsed1 ? '✅ 更快' : '⚠️ 未优化'})`);
    console.log(`第三次请求: ${elapsed3}ms`);

    const cacheUsed = (response2.tokenUsage?.cacheReadTokens ?? 0) > 0;
    console.log(`缓存使用: ${cacheUsed ? '✅ 是' : '❌ 否'}`);

  } catch (error) {
    console.log(`\n❌ 测试失败: ${error}`);
  } finally {
    console.log('\n--- 清理 ---');
    await sessionManager.stopAll();
    console.log('✅ 测试完成');
  }
}

testWorkerOptimization().catch(console.error);

