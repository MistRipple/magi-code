/**
 * 统一日志系统调试测试
 */

import { logger, LogLevel, LogCategory } from '../logging';

console.log('=== 调试测试 ===\n');

// 检查配置
const config = logger.getConfig();
console.log('配置:', {
  enabled: config.enabled,
  cliLogMessages: config.cli.logMessages,
  cliLogResponses: config.cli.logResponses,
  cliCategory: config.categories[LogCategory.CLI],
});

// 检查 shouldLog
console.log('\nshouldLog 检查:');
console.log('- DEBUG + CLI:', logger.isDebugEnabled(LogCategory.CLI));
console.log('- INFO + CLI:', logger.isInfoEnabled(LogCategory.CLI));

// 尝试记录 CLI 消息
console.log('\n尝试记录 CLI 消息...');
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-test',
  message: 'Test message',
  conversationContext: {
    sessionId: 'test-session',
    taskId: 'test-task',
  },
});

console.log('完成');

setTimeout(() => {
  logger.destroy();
  process.exit(0);
}, 100);
