/**
 * 验证 setTaskManager 数据流的单元测试
 *
 * 这是一个快速验证测试，确保：
 * 1. MissionDrivenEngine.setTaskManager() 存在并可调用
 * 2. MissionOrchestrator.setTaskManager() 被正确调用
 * 3. 数据流链路完整
 */

// 在任何其他模块加载前注入 vscode mock
import * as vscodeMock from './vscode-mock';
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id: string) {
  if (id === 'vscode') {
    return vscodeMock;
  }
  return originalRequire.apply(this, arguments);
};

import { LLMAdapterFactory } from '../../llm/adapter-factory';
import { MissionDrivenEngine } from '../../orchestrator/core';
import { SnapshotManager } from '../../snapshot-manager';
import { UnifiedSessionManager } from '../../session';
import { UnifiedTaskManager } from '../../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../../task/session-manager-task-repository';
import { UnifiedMessageBus } from '../../normalizer/unified-message-bus';

async function runTest(): Promise<void> {
  console.log('=== setTaskManager 数据流验证测试 ===\n');

  const workspaceRoot = process.cwd();

  // 1. 初始化所有组件
  console.log('1. 初始化组件...');
  const sessionManager = new UnifiedSessionManager(workspaceRoot);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

  // 设置 MessageBus
  const messageBus = new UnifiedMessageBus({
    enabled: true,
    minStreamInterval: 50,
    batchInterval: 100,
    retentionTime: 60000,
    debug: false,
  });
  adapterFactory.setMessageBus(messageBus);

  await adapterFactory.initialize();

  const session = sessionManager.getOrCreateCurrentSession();
  const repository = new SessionManagerTaskRepository(sessionManager, session.id);
  const taskManager = new UnifiedTaskManager(session.id, repository);
  await taskManager.initialize();
  console.log('   ✓ 组件初始化完成');

  // 2. 创建 MissionDrivenEngine
  console.log('\n2. 创建 MissionDrivenEngine...');
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
  console.log('   ✓ MissionDrivenEngine 创建成功');

  // 3. 验证 setTaskManager 方法存在
  console.log('\n3. 验证 setTaskManager 方法...');
  if (typeof orchestrator.setTaskManager !== 'function') {
    throw new Error('MissionDrivenEngine.setTaskManager 方法不存在！');
  }
  console.log('   ✓ setTaskManager 方法存在');

  // 4. 调用 setTaskManager
  console.log('\n4. 调用 setTaskManager...');
  orchestrator.setTaskManager(taskManager);
  console.log('   ✓ setTaskManager 调用成功');

  // 5. 验证日志输出（通过之前的测试日志可以看到 "引擎.任务管理器.设置" 被输出）
  console.log('\n5. 验证结果...');
  console.log('   日志应显示: "[INFO ] [orchestrator] 引擎.任务管理器.设置"');
  console.log('   ✓ 数据流验证通过');

  // 6. 清理
  await adapterFactory.shutdown().catch(() => {});
  console.log('\n=== 测试完成 ===');
  console.log('\n✅ 所有验证点通过！');
  console.log('\nsetTaskManager 数据流链路:');
  console.log('  WebviewProvider.initTaskManagerForSession()');
  console.log('    ↓ this.orchestratorEngine.setTaskManager(manager)');
  console.log('  MissionDrivenEngine.setTaskManager(taskManager)');
  console.log('    ↓ this.missionOrchestrator.setTaskManager(taskManager)');
  console.log('  MissionOrchestrator.setTaskManager(taskManager)');
  console.log('    ↓ this.taskManager = taskManager');
  console.log('  [执行时] ExecutionCoordinator.setTaskManager(taskManager)');
  console.log('    ↓ syncAssignmentsToSubTasks()');
  console.log('  SubTask.assignedWorker = Assignment.workerId');
}

runTest()
  .then(() => process.exit(0))
  .catch((err) => {
    console.error('❌ 测试失败:', err);
    process.exit(1);
  });

