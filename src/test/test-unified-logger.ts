/**
 * 统一日志系统测试
 */

import { logger, LogLevel, LogCategory } from '../logging';

console.log('=== 统一日志系统测试 ===\n');

// 测试 1: 基本日志
console.log('1. 基本日志测试:');
logger.debug('这是一条调试信息', { detail: 'debug data' });
logger.info('这是一条普通信息', { detail: 'info data' });
logger.warn('这是一条警告信息', { detail: 'warn data' });
logger.error('这是一条错误信息', new Error('测试错误'));

console.log('\n2. 分类日志测试:');
logger.info('系统启动', undefined, LogCategory.SYSTEM);
logger.info('任务创建', { taskId: 'task-123' }, LogCategory.TASK);
logger.debug('子代理.执行', { workerId: 'worker-1' }, LogCategory.WORKER);
logger.info('编排器分析', { planId: 'plan-456' }, LogCategory.ORCHESTRATOR);

console.log('\n3. CLI 消息日志测试:');
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-123',
  message: 'Please implement the following feature:\n\n1. Add a new function\n2. Write tests\n3. Update documentation',
  conversationContext: {
    sessionId: 'session-test',
    taskId: 'task-123',
    subTaskId: 'subtask-1',
    messageIndex: 0,
    totalMessages: 2,
  },
});

setTimeout(() => {
  logger.logCLIResponse({
    cli: 'claude',
    role: 'worker',
    requestId: 'req-123',
    response: 'I have implemented the feature:\n\n[Modified Files]\n- src/feature.ts\n- src/feature.test.ts\n- README.md\n\nAll tests are passing.',
    duration: 5000,
    conversationContext: {
      sessionId: 'session-test',
      taskId: 'task-123',
      subTaskId: 'subtask-1',
      messageIndex: 1,
      totalMessages: 2,
    },
  });

  console.log('\n4. 长消息截断测试:');
  const longMessage = 'A'.repeat(1000);
  logger.logCLIMessage({
    cli: 'codex',
    role: 'worker',
    requestId: 'req-456',
    message: longMessage,
    conversationContext: {
      sessionId: 'session-test',
      taskId: 'task-456',
    },
  });

  console.log('\n5. 配置测试:');
  console.log('当前配置:', logger.getConfig());

  console.log('\n6. 条件日志测试:');
  if (logger.isDebugEnabled(LogCategory.CLI)) {
    logger.debug('CLI 调试已启用', undefined, LogCategory.CLI);
  }

  console.log('\n=== 测试完成 ===');

  // 清理
  setTimeout(() => {
    logger.destroy();
    process.exit(0);
  }, 100);
}, 100);
