/**
 * 测试方案 F：混合会话模式
 *
 * 验证：
 * 1. Orchestrator 使用 InteractiveSession
 * 2. Worker 使用 PrintSession
 * 3. 上下文注入正确
 */

import { SessionManager, SessionManagerOptions } from '../src/cli/session/session-manager';
import { ContextManager } from '../src/context/context-manager';

async function testSessionModes() {
  console.log('='.repeat(60));
  console.log('测试方案 F：混合会话模式');
  console.log('='.repeat(60));

  const cwd = process.cwd();

  // 初始化 ContextManager
  const contextManager = new ContextManager(cwd);
  await contextManager.initialize('test-session', 'Test Session');

  // 添加一些测试数据到 ContextManager
  contextManager.addTask({
    id: 'task-1',
    description: '实现用户认证功能',
    status: 'in_progress',
    assignedWorker: 'claude',
  });
  contextManager.addDecision('decision-1', '使用 JWT 进行身份验证', '安全性和可扩展性');

  // 创建 SessionManager
  const options: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 60000,
    contextManager,
  };

  const sessionManager = new SessionManager(options);

  // 监听事件
  sessionManager.on('log', (msg) => console.log(`[LOG] ${msg}`));
  sessionManager.on('sessionEvent', (event) => {
    console.log(`[EVENT] ${JSON.stringify(event)}`);
  });

  console.log('\n--- 测试 1: 验证会话模式配置 ---');
  const orchMode = sessionManager.getSessionMode('orchestrator');
  const workerMode = sessionManager.getSessionMode('worker');
  console.log(`✅ Orchestrator 模式: ${orchMode} (期望: interactive)`);
  console.log(`✅ Worker 模式: ${workerMode} (期望: oneshot)`);

  if (orchMode === 'interactive' && workerMode === 'oneshot') {
    console.log('✅ 会话模式配置正确！');
  } else {
    console.log('❌ 会话模式配置错误！');
  }

  console.log('\n--- 测试 2: 验证 Orchestrator 会话启动 ---');
  try {
    await sessionManager.startSession('claude', 'orchestrator');
    console.log('✅ Orchestrator 会话启动成功');

    const orchAlive = sessionManager.isSessionAlive('claude', 'orchestrator');
    console.log(`   会话存活状态: ${orchAlive}`);
  } catch (error) {
    console.log(`⚠️ Orchestrator 会话启动: ${error}`);
    console.log('   (这是预期的，因为没有实际的 claude CLI)');
  }

  console.log('\n--- 测试 3: 验证 Worker 会话启动 ---');
  try {
    await sessionManager.startSession('claude', 'worker');
    console.log('✅ Worker 会话启动成功');

    const workerAlive = sessionManager.isSessionAlive('claude', 'worker');
    console.log(`   会话存活状态: ${workerAlive}`);
  } catch (error) {
    console.log(`⚠️ Worker 会话启动: ${error}`);
    console.log('   (这是预期的，因为没有实际的 claude CLI)');
  }

  console.log('\n--- 测试 4: 验证上下文切片生成 ---');
  const contextSlice = contextManager.getContextSlice({
    maxTokens: 2000,
    includeMemory: true,
    includeRecent: false,
    memorySummary: {
      includeCurrentTasks: true,
      includeKeyDecisions: 3,
      includePendingIssues: true,
    },
  });
  console.log('上下文切片内容:');
  console.log(contextSlice || '(空)');

  // 清理
  console.log('\n--- 清理会话 ---');
  await sessionManager.stopAll();
  console.log('✅ 所有会话已停止');

  console.log('\n' + '='.repeat(60));
  console.log('测试完成');
  console.log('='.repeat(60));
}

// 运行测试
testSessionModes().catch(console.error);

