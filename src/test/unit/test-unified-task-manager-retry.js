/**
 * UnifiedTaskManager 重试机制测试
 */

const { UnifiedTaskManager } = require('../../../out/task/unified-task-manager');

// Mock TaskRepository
class MockTaskRepository {
  constructor() {
    this.tasks = new Map();
  }

  async saveTask(task) {
    this.tasks.set(task.id, JSON.parse(JSON.stringify(task)));
  }

  async getTask(taskId) {
    const task = this.tasks.get(taskId);
    return task ? JSON.parse(JSON.stringify(task)) : null;
  }

  async getTasksBySession(sessionId) {
    return Array.from(this.tasks.values())
      .filter(t => t.sessionId === sessionId)
      .map(t => JSON.parse(JSON.stringify(t)));
  }

  async deleteTask(taskId) {
    this.tasks.delete(taskId);
  }

  async clear() {
    this.tasks.clear();
  }
}

async function runTests() {
  console.log('\n================================================================================');
  console.log('  UnifiedTaskManager 重试机制测试');
  console.log('================================================================================\n');

  const sessionId = 'test-session';
  let passCount = 0;
  let failCount = 0;

  // Test 1: canRetrySubTask - 基本功能
  try {
    const repository = new MockTaskRepository();
    const taskManager = new UnifiedTaskManager(sessionId, repository);
    await taskManager.initialize();

    const task = await taskManager.createTask({
      prompt: 'Test task',
      maxRetries: 3,
    });

    const subTask = await taskManager.createSubTask(task.id, {
      description: 'Test subtask',
      assignedWorker: 'claude',
      maxRetries: 3,
    });

    // 初始状态应该可以重试
    const canRetry = taskManager.canRetrySubTask(task.id, subTask.id);
    if (canRetry) {
      console.log('✅ canRetrySubTask - 初始状态可以重试');
      passCount++;
    } else {
      console.log('❌ canRetrySubTask - 初始状态应该可以重试');
      failCount++;
    }
  } catch (error) {
    console.log('❌ canRetrySubTask - 测试失败:', error.message);
    failCount++;
  }

  // Test 2: resetSubTaskForRetry - 重置状态
  try {
    const repository = new MockTaskRepository();
    const taskManager = new UnifiedTaskManager(sessionId, repository);
    await taskManager.initialize();

    const task = await taskManager.createTask({
      prompt: 'Test task',
    });

    const subTask = await taskManager.createSubTask(task.id, {
      description: 'Test subtask',
      assignedWorker: 'claude',
      maxRetries: 3,
    });

    // 启动并失败
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Test error');

    // 重置为重试状态
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    // 验证状态
    const updatedTask = await taskManager.getTask(task.id);
    const updatedSubTask = updatedTask.subTasks.find(st => st.id === subTask.id);

    if (updatedSubTask.status === 'retrying' &&
        updatedSubTask.retryCount === 1 &&
        updatedSubTask.error === undefined &&
        updatedSubTask.progress === 0) {
      console.log('✅ resetSubTaskForRetry - 正确重置状态');
      passCount++;
    } else {
      console.log('❌ resetSubTaskForRetry - 状态重置不正确');
      console.log('   status:', updatedSubTask.status, '(expected: retrying)');
      console.log('   retryCount:', updatedSubTask.retryCount, '(expected: 1)');
      failCount++;
    }
  } catch (error) {
    console.log('❌ resetSubTaskForRetry - 测试失败:', error.message);
    failCount++;
  }

  // Test 3: 达到最大重试次数
  try {
    const repository = new MockTaskRepository();
    const taskManager = new UnifiedTaskManager(sessionId, repository);
    await taskManager.initialize();

    const task = await taskManager.createTask({
      prompt: 'Test task',
    });

    const subTask = await taskManager.createSubTask(task.id, {
      description: 'Test subtask',
      assignedWorker: 'claude',
      maxRetries: 1,
    });

    // 失败一次
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Test error');

    // 重试一次
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    // 再次失败
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Test error 2');

    // 尝试再次重试应该抛出错误
    let errorThrown = false;
    try {
      await taskManager.resetSubTaskForRetry(task.id, subTask.id);
    } catch (error) {
      if (error.message.includes('has reached max retries')) {
        errorThrown = true;
      }
    }

    if (errorThrown) {
      console.log('✅ 达到最大重试次数时正确抛出错误');
      passCount++;
    } else {
      console.log('❌ 达到最大重试次数时应该抛出错误');
      failCount++;
    }
  } catch (error) {
    console.log('❌ 最大重试次数测试失败:', error.message);
    failCount++;
  }

  // Test 4: retryCount 递增
  try {
    const repository = new MockTaskRepository();
    const taskManager = new UnifiedTaskManager(sessionId, repository);
    await taskManager.initialize();

    const task = await taskManager.createTask({
      prompt: 'Test task',
    });

    const subTask = await taskManager.createSubTask(task.id, {
      description: 'Test subtask',
      assignedWorker: 'claude',
      maxRetries: 3,
    });

    // 初始 retryCount = 0
    let currentTask = await taskManager.getTask(task.id);
    let currentSubTask = currentTask.subTasks.find(st => st.id === subTask.id);
    const initialRetryCount = currentSubTask.retryCount;

    // 失败并重试
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Error 1');
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    currentTask = await taskManager.getTask(task.id);
    currentSubTask = currentTask.subTasks.find(st => st.id === subTask.id);
    const firstRetryCount = currentSubTask.retryCount;

    // 再次失败并重试
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Error 2');
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    currentTask = await taskManager.getTask(task.id);
    currentSubTask = currentTask.subTasks.find(st => st.id === subTask.id);
    const secondRetryCount = currentSubTask.retryCount;

    if (initialRetryCount === 0 && firstRetryCount === 1 && secondRetryCount === 2) {
      console.log('✅ retryCount 正确递增 (0 → 1 → 2)');
      passCount++;
    } else {
      console.log('❌ retryCount 递增不正确');
      console.log('   初始:', initialRetryCount, '第一次:', firstRetryCount, '第二次:', secondRetryCount);
      failCount++;
    }
  } catch (error) {
    console.log('❌ retryCount 递增测试失败:', error.message);
    failCount++;
  }

  // Test 5: 完整重试流程
  try {
    const repository = new MockTaskRepository();
    const taskManager = new UnifiedTaskManager(sessionId, repository);
    await taskManager.initialize();

    const task = await taskManager.createTask({
      prompt: 'Test task',
    });

    const subTask = await taskManager.createSubTask(task.id, {
      description: 'Test subtask',
      assignedWorker: 'claude',
      maxRetries: 2,
    });

    // 第一次尝试：失败
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Error 1');

    // 第一次重试
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    // 第二次尝试：再次失败
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.failSubTask(task.id, subTask.id, 'Error 2');

    // 第二次重试
    await taskManager.resetSubTaskForRetry(task.id, subTask.id);

    // 第三次尝试：成功
    await taskManager.startSubTask(task.id, subTask.id);
    await taskManager.completeSubTask(task.id, subTask.id, {
      agentType: 'claude',
      success: true,
      duration: 1000,
      timestamp: new Date(),
    });

    const finalTask = await taskManager.getTask(task.id);
    const finalSubTask = finalTask.subTasks.find(st => st.id === subTask.id);

    if (finalSubTask.status === 'completed' && finalSubTask.retryCount === 2) {
      console.log('✅ 完整重试流程：失败 → 重试 → 失败 → 重试 → 成功');
      passCount++;
    } else {
      console.log('❌ 完整重试流程失败');
      console.log('   status:', finalSubTask.status, 'retryCount:', finalSubTask.retryCount);
      failCount++;
    }
  } catch (error) {
    console.log('❌ 完整重试流程测试失败:', error.message);
    failCount++;
  }

  // 总结
  console.log('\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
  console.log('  测试结果汇总');
  console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
  console.log(`✅ 通过: ${passCount}/5 (${(passCount / 5 * 100).toFixed(1)}%)`);
  if (failCount > 0) {
    console.log(`❌ 失败: ${failCount}/5`);
  }
  console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n');

  return failCount === 0 ? 0 : 1;
}

// 运行测试
runTests().then(exitCode => {
  process.exit(exitCode);
}).catch(error => {
  console.error('测试运行失败:', error);
  process.exit(1);
});
