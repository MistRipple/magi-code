/**
 * 日志系统配置测试脚本
 * 用于测试和演示日志系统的各种配置选项
 */

const { logger, LogLevel, LogCategory } = require('./out/logging');

console.log('='.repeat(80));
console.log('日志系统配置测试');
console.log('='.repeat(80));

// 1. 查看默认配置
console.log('\n【1】默认配置:');
console.log(JSON.stringify(logger.getConfig(), null, 2));

// 2. 启用 CLI 消息日志（DEBUG 级别）
console.log('\n【2】启用 CLI 消息日志（DEBUG 级别）:');
logger.configureCLILogging({
  enabled: true,
  logMessages: true,
  logResponses: true,
  maxLength: 500,        // 控制台显示最多 500 字符
  maxLengthFile: 0,      // 文件日志不限制（完整保存）
});

console.log('CLI 日志配置已更新:');
console.log('- logMessages: true');
console.log('- logResponses: true');
console.log('- maxLength: 500');
console.log('- CLI category level: DEBUG (自动设置)');

// 3. 启用文件日志
console.log('\n【3】启用文件日志:');
logger.configureFileLogging({
  enabled: true,
  path: '.multicli-logs',
  maxSize: 10 * 1024 * 1024,  // 10MB
  maxFiles: 5,
});
console.log('文件日志已启用: .multicli-logs/');

// 4. 测试 CLI 消息日志
console.log('\n【4】测试 CLI 消息日志:');
console.log('-'.repeat(80));

logger.logCLIMessage({
  cli: 'claude',
  role: 'orchestrator',
  requestId: 'req-001',
  message: '这是一条测试消息，用于验证 CLI 日志功能是否正常工作。',
  conversationContext: {
    sessionId: 'session-test',
    taskId: 'task-001',
    subTaskId: 'subtask-001',
    messageIndex: 1,
    totalMessages: 3,
  },
});

logger.logCLIResponse({
  cli: 'claude',
  role: 'orchestrator',
  requestId: 'req-001',
  response: '收到！我会帮您完成这个任务。让我先分析一下需求...',
  duration: 1234,
  conversationContext: {
    sessionId: 'session-test',
    taskId: 'task-001',
    subTaskId: 'subtask-001',
    messageIndex: 2,
    totalMessages: 3,
  },
});

// 5. 测试长消息截断
console.log('\n【5】测试长消息截断（控制台显示 500 字符，文件保存完整）:');
console.log('-'.repeat(80));

const longMessage = '这是一条很长的消息。'.repeat(100);  // 约 1000 字符
logger.logCLIMessage({
  cli: 'codex',
  role: 'worker',
  requestId: 'req-002',
  message: longMessage,
  conversationContext: {
    sessionId: 'session-test',
    taskId: 'task-002',
  },
});

// 6. 测试不同日志级别
console.log('\n【6】测试不同日志级别:');
console.log('-'.repeat(80));

logger.debug('这是 DEBUG 级别日志', { detail: 'debug data' }, LogCategory.CLI);
logger.info('这是 INFO 级别日志', { detail: 'info data' }, LogCategory.CLI);
logger.warn('这是 WARN 级别日志', { detail: 'warn data' }, LogCategory.CLI);
logger.error('这是 ERROR 级别日志', new Error('测试错误'), LogCategory.CLI);

// 7. 查看更新后的配置
console.log('\n【7】更新后的配置:');
const config = logger.getConfig();
console.log('CLI 配置:', config.cli);
console.log('文件配置:', config.file);
console.log('CLI 分类级别:', config.categories.cli);

// 8. 配置选项说明
console.log('\n' + '='.repeat(80));
console.log('配置选项说明');
console.log('='.repeat(80));
console.log(`
【CLI 日志配置】
logger.configureCLILogging({
  enabled: true,           // 启用 CLI 日志
  logMessages: true,       // 记录发送的消息
  logResponses: true,      // 记录接收的响应
  maxLength: 500,          // 控制台显示最大长度（超过会截断）
  maxLengthFile: 0,        // 文件日志最大长度（0 = 不限制）
});

【文件日志配置】
logger.configureFileLogging({
  enabled: true,           // 启用文件日志
  path: '.multicli-logs',  // 日志文件目录
  maxSize: 10485760,       // 单个文件最大大小（10MB）
  maxFiles: 5,             // 最多保留文件数
});

【控制台日志配置】
logger.configureConsoleLogging({
  enabled: true,           // 启用控制台输出
  colorize: true,          // 启用颜色
  timestamp: true,         // 显示时间戳
});

【日志级别配置】
// CLI 日志配置会自动设置 CLI 分类为 DEBUG 级别
logger.configureCLILogging({ enabled: true, logMessages: true });
// LogLevel: DEBUG(0), INFO(1), WARN(2), ERROR(3), SILENT(4)
// LogCategory: SYSTEM, CLI, TASK, WORKER, ORCHESTRATOR, SESSION, RECOVERY, UI

【查看配置】
const config = logger.getConfig();

【重置配置】
logger.resetConfig();
`);

console.log('='.repeat(80));
console.log('测试完成！');
console.log('='.repeat(80));
console.log('\n提示:');
console.log('- 控制台日志已显示在上方');
console.log('- 文件日志已保存到: .multicli-logs/ 目录');
console.log('- 可以查看文件日志以验证完整内容是否保存');
console.log('\n查看文件日志:');
console.log('  ls -la .multicli-logs/');
console.log('  cat .multicli-logs/multicli-*.log');

// 等待文件写入完成
setTimeout(() => {
  logger.destroy();
  console.log('\n日志系统已关闭。');
  process.exit(0);
}, 500);
