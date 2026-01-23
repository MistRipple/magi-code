/**
 * 统一日志系统调试测试
 */

import { logger, LogLevel, LogCategory } from '../logging';

console.log('=== 调试测试 ===\n');

// 检查配置
const config = logger.getConfig();
console.log('配置:', {
  enabled: config.enabled,
  agentLogMessages: config.agent.logMessages,
  agentLogResponses: config.agent.logResponses,
  agentCategory: config.categories[LogCategory.AGENT],
});

// 检查 shouldLog
console.log('\nshouldLog 检查:');
console.log('- DEBUG + AGENT:', logger.isDebugEnabled(LogCategory.AGENT));
console.log('- INFO + AGENT:', logger.isInfoEnabled(LogCategory.AGENT));

// 尝试记录 Agent 消息
console.log('\n尝试记录 Agent 消息...');
logger.logAgentMessage({
  agent: 'claude',
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
