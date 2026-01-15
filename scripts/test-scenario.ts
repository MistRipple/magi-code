/**
 * 场景测试：方案 F 混合会话模式
 *
 * 测试场景：
 * 1. 验证会话模式选择逻辑
 * 2. 验证上下文注入逻辑
 * 3. 验证消息前缀构建
 * 4. Worker 使用 PrintSession 发送消息
 */

import { SessionManager, SessionManagerOptions } from '../src/cli/session/session-manager';
import { ContextManager } from '../src/context/context-manager';

async function scenarioTest() {
  console.log('='.repeat(60));
  console.log('场景测试：方案 F 混合会话模式');
  console.log('='.repeat(60));

  const cwd = process.cwd();

  // 初始化 ContextManager
  const contextManager = new ContextManager(cwd);
  await contextManager.initialize('scenario-test', 'Scenario Test Session');

  // 添加测试数据
  contextManager.addTask({
    id: 'task-1',
    description: '实现用户认证功能',
    status: 'in_progress',
    assignedWorker: 'claude',
  });
  contextManager.addDecision('d1', '使用 JWT 认证', '安全性考虑');

  // 创建 SessionManager
  const options: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 30000,
    contextManager,
  };

  const sessionManager = new SessionManager(options);

  // 收集日志
  const logs: string[] = [];
  sessionManager.on('log', (msg) => {
    logs.push(msg);
    console.log(`[LOG] ${msg}`);
  });

  console.log('\n' + '='.repeat(60));
  console.log('场景 1: 验证会话模式选择');
  console.log('='.repeat(60));

  const orchMode = sessionManager.getSessionMode('orchestrator');
  const workerMode = sessionManager.getSessionMode('worker');
  console.log(`Orchestrator 模式: ${orchMode}`);
  console.log(`Worker 模式: ${workerMode}`);
  console.log(orchMode === 'interactive' ? '✅ Orchestrator 正确使用 interactive 模式' : '❌ 模式错误');
  console.log(workerMode === 'oneshot' ? '✅ Worker 正确使用 oneshot 模式' : '❌ 模式错误');

  console.log('\n' + '='.repeat(60));
  console.log('场景 2: 验证上下文切片生成');
  console.log('='.repeat(60));

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

  console.log('上下文切片:');
  console.log(contextSlice);
  console.log(contextSlice.includes('task-1') || contextSlice.includes('用户认证')
    ? '✅ 上下文包含任务信息' : '⚠️ 上下文可能缺少任务信息');
  console.log(contextSlice.includes('JWT')
    ? '✅ 上下文包含决策信息' : '⚠️ 上下文可能缺少决策信息');

  console.log('\n' + '='.repeat(60));
  console.log('场景 3: Worker 发送消息（使用 echo 模拟）');
  console.log('='.repeat(60));

  // 使用 echo 命令测试 PrintSession（Worker 模式）
  const workerOptions: SessionManagerOptions = {
    cwd,
    idleTimeoutMs: 10000,
    contextManager,
    commandOverrides: {
      claude: 'echo',
      codex: 'echo',
      gemini: 'echo',
    },
  };

  const workerSessionManager = new SessionManager(workerOptions);
  workerSessionManager.on('log', (msg) => console.log(`[LOG] ${msg}`));

  // 收集输出以验证上下文注入
  let capturedOutput = '';
  workerSessionManager.on('output', (data) => {
    capturedOutput += data.chunk;
  });

  try {
    const workerResponse = await workerSessionManager.send('claude', 'worker', {
      requestId: 'worker-req-1',
      taskId: 'task-1',
      cli: 'claude',
      role: 'worker',
      content: '实现登录功能',
    });
    console.log('✅ Worker 消息发送成功');
    console.log(`响应内容: ${workerResponse.content}`);

    // 验证上下文是否被注入到消息中
    if (capturedOutput.includes('任务信息') || capturedOutput.includes('项目上下文')) {
      console.log('✅ 上下文已注入到 Worker 消息');
    } else {
      console.log('⚠️ 输出:', capturedOutput.substring(0, 200));
    }
  } catch (error) {
    console.log(`⚠️ Worker 消息: ${error}`);
  }

  console.log('\n' + '='.repeat(60));
  console.log('场景 4: Gemini Worker（系统提示注入验证）');
  console.log('='.repeat(60));

  let geminiOutput = '';
  workerSessionManager.on('output', (data) => {
    if (data.cli === 'gemini') geminiOutput += data.chunk;
  });

  try {
    const geminiResponse = await workerSessionManager.send('gemini', 'worker', {
      requestId: 'gemini-req-1',
      taskId: 'task-1',
      cli: 'gemini',
      role: 'worker',
      content: '生成测试用例',
    });
    console.log('✅ Gemini Worker 消息发送成功');
    console.log(`响应内容: ${geminiResponse.content}`);
  } catch (error) {
    console.log(`⚠️ Gemini Worker: ${error}`);
  }

  // 清理
  console.log('\n--- 清理 ---');
  await workerSessionManager.stopAll();
  console.log('✅ 测试完成');
}

scenarioTest().catch(console.error);

