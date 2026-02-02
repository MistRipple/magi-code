/**
 * 日志工具模块
 * 提供统一的日志记录功能
 */

/**
 * 日志级别枚举
 */
const LogLevel = {
  DEBUG: 'DEBUG',
  INFO: 'INFO',
  WARN: 'WARN',
  ERROR: 'ERROR'
};

/**
 * 当前日志级别（可通过环境变量配置）
 */
const currentLogLevel = process.env.LOG_LEVEL || LogLevel.INFO;

/**
 * 日志级别优先级映射
 */
const levelPriority = {
  [LogLevel.DEBUG]: 0,
  [LogLevel.INFO]: 1,
  [LogLevel.WARN]: 2,
  [LogLevel.ERROR]: 3
};

/**
 * 格式化时间戳
 * @returns {string} 格式化后的时间字符串
 */
function formatTimestamp() {
  const now = new Date();
  return now.toISOString();
}

/**
 * 判断是否应该输出日志
 * @param {string} level - 日志级别
 * @returns {boolean} 是否应该输出
 */
function shouldLog(level) {
  return levelPriority[level] >= levelPriority[currentLogLevel];
}

/**
 * 输出日志
 * @param {string} level - 日志级别
 * @param {string} message - 日志消息
 * @param {...any} args - 额外参数
 */
function log(level, message, ...args) {
  if (!shouldLog(level)) {
    return;
  }
  
  const timestamp = formatTimestamp();
  const prefix = `[${timestamp}] [${level}]`;
  
  if (args.length > 0) {
    console.log(prefix, message, ...args);
  } else {
    console.log(prefix, message);
  }
}

/**
 * 日志记录器对象
 */
const logger = {
  /**
   * 调试日志
   * @param {string} message - 日志消息
   * @param {...any} args - 额外参数
   */
  debug(message, ...args) {
    log(LogLevel.DEBUG, message, ...args);
  },

  /**
   * 信息日志
   * @param {string} message - 日志消息
   * @param {...any} args - 额外参数
   */
  info(message, ...args) {
    log(LogLevel.INFO, message, ...args);
  },

  /**
   * 警告日志
   * @param {string} message - 日志消息
   * @param {...any} args - 额外参数
   */
  warn(message, ...args) {
    log(LogLevel.WARN, message, ...args);
  },

  /**
   * 错误日志
   * @param {string} message - 日志消息
   * @param {...any} args - 额外参数
   */
  error(message, ...args) {
    log(LogLevel.ERROR, message, ...args);
  }
};

module.exports = logger;
