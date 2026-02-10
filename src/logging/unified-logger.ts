/**
 * 统一日志管理系统
 *
 * 设计原则：
 * 1. 完全统一 - 所有日志使用相同格式和接口
 * 2. 不保留 console.log 方式
 * 3. 配置驱动 - 通过配置控制所有行为
 * 4. 性能优先 - 延迟初始化、异步写入
 */

import * as fs from 'fs';
import * as path from 'path';
import { EventEmitter } from 'events';

// ============================================================================
// 类型定义
// ============================================================================

/** 日志级别 */
export enum LogLevel {
  DEBUG = 0,
  INFO = 1,
  WARN = 2,
  ERROR = 3,
  SILENT = 4,
}

/** 日志分类 */
export enum LogCategory {
  SYSTEM = 'system',
  AGENT = 'agent',
  TASK = 'task',
  WORKER = 'worker',
  ORCHESTRATOR = 'orchestrator',
  SESSION = 'session',
  RECOVERY = 'recovery',
  UI = 'ui',
  LLM = 'llm',
  TOOLS = 'tools',
  SHELL = 'shell',
}

/** 日志记录 */
export interface LogRecord {
  timestamp: number;
  level: LogLevel;
  category: LogCategory;
  message: string;
  data?: unknown;
  error?: Error;
  metadata?: {
    file?: string;
    line?: number;
    function?: string;
    [key: string]: unknown;
  };
}

/** Agent 消息日志 */
export interface AgentMessageLog {
  timestamp: number;
  direction: 'send' | 'receive';
  agent: string;
  role: 'worker' | 'orchestrator';
  requestId: string;
  content: string;
  contentLength: number;
  truncated: boolean;
  duration?: number;
  // 完整内容和处理信息
  fullContent?: string;  // 完整的原始内容（用于文件日志）
  processedContent?: string;  // 格式处理后的内容
  // 对话上下文（必需）
  conversationContext: {
    sessionId?: string;
    taskId?: string;
    subTaskId?: string;
    messageIndex?: number;  // 对话中的消息序号
    totalMessages?: number;  // 对话总消息数
  };
}

/** 日志配置 */
export interface LogConfig {
  enabled: boolean;
  level: LogLevel;
  categories: Partial<Record<LogCategory, LogLevel>>;
  console: {
    enabled: boolean;
    colorize: boolean;
    timestamp: boolean;
  };
  file: {
    enabled: boolean;
    path: string;
    maxSize: number;
    maxFiles: number;
  };
  agent: {
    logMessages: boolean;
    logResponses: boolean;
    maxLength: number;  // 控制台显示的最大长度
    maxLengthFile: number;  // 文件日志的最大长度（0 表示不限制）
  };
}

// ============================================================================
// 默认配置
// ============================================================================

const DEFAULT_CONFIG: LogConfig = {
  enabled: true,
  level: LogLevel.INFO,
  categories: {
    [LogCategory.SYSTEM]: LogLevel.INFO,
    [LogCategory.AGENT]: LogLevel.INFO,
    [LogCategory.TASK]: LogLevel.INFO,
    [LogCategory.WORKER]: LogLevel.INFO,
    [LogCategory.ORCHESTRATOR]: LogLevel.INFO,
    [LogCategory.SESSION]: LogLevel.WARN,
    [LogCategory.RECOVERY]: LogLevel.INFO,
    [LogCategory.UI]: LogLevel.INFO,
  },
  console: {
    enabled: true,
    colorize: true,
    timestamp: true,
  },
  file: {
    enabled: false,
    path: '.magi-logs',
    maxSize: 10 * 1024 * 1024, // 10MB
    maxFiles: 5,
  },
  agent: {
    logMessages: true,
    logResponses: true,
    maxLength: 500,  // 控制台显示截断
    maxLengthFile: 0,  // 文件日志不限制（完整保存）
  },
};

// ============================================================================
// 颜色定义
// ============================================================================

const COLORS = {
  reset: '\x1b[0m',
  bright: '\x1b[1m',
  dim: '\x1b[2m',

  // 前景色
  black: '\x1b[30m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  white: '\x1b[37m',
  gray: '\x1b[90m',

  // 背景色
  bgRed: '\x1b[41m',
  bgGreen: '\x1b[42m',
  bgYellow: '\x1b[43m',
  bgBlue: '\x1b[44m',
};

// ============================================================================
// UnifiedLogger 类
// ============================================================================

export class UnifiedLogger extends EventEmitter {
  private config: LogConfig;
  private fileStream?: fs.WriteStream;
  private currentFileSize: number = 0;
  private logBuffer: string[] = [];
  private flushTimer?: NodeJS.Timeout;

  constructor(config?: Partial<LogConfig>) {
    super();
    this.config = this.mergeConfig(config);
    this.loadConfigFromEnv();

    if (this.config.file.enabled) {
      this.initFileLogging();
    }
  }

  // --------------------------------------------------------------------------
  // 配置管理
  // --------------------------------------------------------------------------

  private mergeConfig(config?: Partial<LogConfig>): LogConfig {
    return {
      ...DEFAULT_CONFIG,
      ...config,
      categories: {
        ...DEFAULT_CONFIG.categories,
        ...config?.categories,
      },
      console: {
        ...DEFAULT_CONFIG.console,
        ...config?.console,
      },
      file: {
        ...DEFAULT_CONFIG.file,
        ...config?.file,
      },
      agent: {
        ...DEFAULT_CONFIG.agent,
        ...config?.agent,
      },
    };
  }

  private loadConfigFromEnv(): void {
    // 全局开关
    if (process.env.MAGI_LOG_ENABLED !== undefined) {
      this.config.enabled = process.env.MAGI_LOG_ENABLED === 'true';
    }

    // 全局级别
    if (process.env.MAGI_LOG_LEVEL) {
      const level = process.env.MAGI_LOG_LEVEL.toUpperCase();
      if (level in LogLevel) {
        this.config.level = LogLevel[level as keyof typeof LogLevel];
      }
    }

    // 分类级别
    Object.values(LogCategory).forEach(category => {
      const envKey = `MAGI_LOG_${category.toUpperCase()}`;
      if (process.env[envKey]) {
        const level = process.env[envKey]!.toUpperCase();
        if (level in LogLevel) {
          this.config.categories[category] = LogLevel[level as keyof typeof LogLevel];
        }
      }
    });

    // Agent 消息日志
    if (process.env.MAGI_LOG_AGENT_MESSAGES !== undefined) {
      this.config.agent.logMessages = process.env.MAGI_LOG_AGENT_MESSAGES === 'true';
    }
    if (process.env.MAGI_LOG_AGENT_RESPONSES !== undefined) {
      this.config.agent.logResponses = process.env.MAGI_LOG_AGENT_RESPONSES === 'true';
    }
  }

  updateConfig(config: Partial<LogConfig>): void {
    this.config = this.mergeConfig(config);
    if (this.config.file.enabled && !this.fileStream) {
      this.initFileLogging();
    } else if (!this.config.file.enabled && this.fileStream) {
      this.closeFileLogging();
    }
  }

  // --------------------------------------------------------------------------
  // 便捷配置方法
  // --------------------------------------------------------------------------

  /** 配置 Agent 消息日志 */
  configureAgentLogging(options: {
    enabled?: boolean;
    logMessages?: boolean;
    logResponses?: boolean;
    maxLength?: number;
    maxLengthFile?: number;
  }): void {
    this.config.agent = {
      ...this.config.agent,
      logMessages: options.logMessages ?? this.config.agent.logMessages,
      logResponses: options.logResponses ?? this.config.agent.logResponses,
      maxLength: options.maxLength ?? this.config.agent.maxLength,
      maxLengthFile: options.maxLengthFile ?? this.config.agent.maxLengthFile,
    };

    // 如果启用 Agent 日志，确保 AGENT 分类级别至少是 DEBUG
    if (options.enabled !== false && (options.logMessages || options.logResponses)) {
      if (!this.config.categories[LogCategory.AGENT] || this.config.categories[LogCategory.AGENT] > LogLevel.DEBUG) {
        this.config.categories[LogCategory.AGENT] = LogLevel.DEBUG;
      }
    }
  }

  /** 配置文件日志 */
  configureFileLogging(options: {
    enabled: boolean;
    path?: string;
    maxSize?: number;
    maxFiles?: number;
  }): void {
    const wasEnabled = this.config.file.enabled;

    this.config.file = {
      ...this.config.file,
      enabled: options.enabled,
      path: options.path ?? this.config.file.path,
      maxSize: options.maxSize ?? this.config.file.maxSize,
      maxFiles: options.maxFiles ?? this.config.file.maxFiles,
    };

    // 处理文件流的启用/禁用
    if (options.enabled && !wasEnabled) {
      this.initFileLogging();
    } else if (!options.enabled && wasEnabled) {
      this.closeFileLogging();
    }
  }

  /** 配置控制台输出 */
  configureConsoleLogging(options: {
    enabled?: boolean;
    colorize?: boolean;
    timestamp?: boolean;
  }): void {
    this.config.console = {
      ...this.config.console,
      enabled: options.enabled ?? this.config.console.enabled,
      colorize: options.colorize ?? this.config.console.colorize,
      timestamp: options.timestamp ?? this.config.console.timestamp,
    };
  }

  /** 获取当前配置 */
  getConfig(): Readonly<LogConfig> {
    return { ...this.config };
  }

  /** 重置为默认配置 */
  resetConfig(): void {
    const wasFileEnabled = this.config.file.enabled;
    this.config = this.mergeConfig({});
    this.loadConfigFromEnv();

    if (wasFileEnabled && !this.config.file.enabled) {
      this.closeFileLogging();
    } else if (!wasFileEnabled && this.config.file.enabled) {
      this.initFileLogging();
    }
  }

  // --------------------------------------------------------------------------
  // 核心日志方法
  // --------------------------------------------------------------------------

  debug(message: string, data?: unknown, category: LogCategory = LogCategory.SYSTEM): void {
    this.log(LogLevel.DEBUG, category, message, data);
  }

  info(message: string, data?: unknown, category: LogCategory = LogCategory.SYSTEM): void {
    this.log(LogLevel.INFO, category, message, data);
  }

  warn(message: string, data?: unknown, category: LogCategory = LogCategory.SYSTEM): void {
    this.log(LogLevel.WARN, category, message, data);
  }

  error(message: string, error?: Error | unknown, category: LogCategory = LogCategory.SYSTEM): void {
    const errorObj = error instanceof Error ? error : undefined;
    const data = error instanceof Error ? undefined : error;
    this.log(LogLevel.ERROR, category, message, data, errorObj);
  }

  private log(
    level: LogLevel,
    category: LogCategory,
    message: string,
    data?: unknown,
    error?: Error
  ): void {
    if (!this.config.enabled) return;
    if (!this.shouldLog(level, category)) return;

    const record: LogRecord = {
      timestamp: Date.now(),
      level,
      category,
      message,
      data,
      error,
    };

    this.emit('log', record);
    this.writeToConsole(record);
    this.writeToFile(record);
  }

  private shouldLog(level: LogLevel, category: LogCategory): boolean {
    // 优先检查分类级别
    const categoryLevel = this.config.categories[category];
    if (categoryLevel !== undefined) {
      return level >= categoryLevel;
    }

    // 使用全局级别
    return level >= this.config.level;
  }

  // --------------------------------------------------------------------------
  // Agent 消息日志
  // --------------------------------------------------------------------------

  logAgentMessage(params: {
    agent: string;
    role: 'worker' | 'orchestrator';
    requestId: string;
    message: string;
    processedMessage?: string;  // 格式处理后的消息
    conversationContext: {      // 必需参数
      sessionId?: string;
      taskId?: string;
      subTaskId?: string;
      messageIndex?: number;
      totalMessages?: number;
    };
  }): void {
    if (!this.config.enabled) return;
    if (!this.config.agent.logMessages) return;
    if (!this.shouldLog(LogLevel.DEBUG, LogCategory.AGENT)) return;

    const { message, processedMessage, conversationContext, ...rest } = params;

    // 控制台显示用的截断内容
    const truncated = message.length > this.config.agent.maxLength;
    const content = truncated
      ? message.substring(0, this.config.agent.maxLength) + '...'
      : message;

    const log: AgentMessageLog = {
      timestamp: Date.now(),
      direction: 'send',
      content,  // 控制台显示的截断内容
      contentLength: message.length,
      truncated,
      fullContent: message,  // 完整的原始内容
      processedContent: processedMessage,  // 格式处理后的内容
      conversationContext,
      ...rest,
    };

    this.emit('agent-message', log);
    this.writeAgentMessageToConsole(log);
    this.writeAgentMessageToFile(log);
  }

  logAgentResponse(params: {
    agent: string;
    role: 'worker' | 'orchestrator';
    requestId: string;
    response: string;
    duration: number;
    processedResponse?: string;  // 格式处理后的响应
    conversationContext: {       // 必需参数
      sessionId?: string;
      taskId?: string;
      subTaskId?: string;
      messageIndex?: number;
      totalMessages?: number;
    };
  }): void {
    if (!this.config.enabled) return;
    if (!this.config.agent.logResponses) return;
    if (!this.shouldLog(LogLevel.DEBUG, LogCategory.AGENT)) return;

    const { response, processedResponse, conversationContext, ...rest } = params;

    // 控制台显示用的截断内容
    const truncated = response.length > this.config.agent.maxLength;
    const content = truncated
      ? response.substring(0, this.config.agent.maxLength) + '...'
      : response;

    const log: AgentMessageLog = {
      timestamp: Date.now(),
      direction: 'receive',
      content,  // 控制台显示的截断内容
      contentLength: response.length,
      truncated,
      fullContent: response,  // 完整的原始内容
      processedContent: processedResponse,  // 格式处理后的内容
      conversationContext,
      ...rest,
    };

    this.emit('agent-response', log);
    this.writeAgentMessageToConsole(log);
    this.writeAgentMessageToFile(log);
  }

  // --------------------------------------------------------------------------
  // 控制台输出
  // --------------------------------------------------------------------------

  private writeToConsole(record: LogRecord): void {
    if (!this.config.console.enabled) return;

    const parts: string[] = [];

    // 时间戳
    if (this.config.console.timestamp) {
      const time = new Date(record.timestamp).toISOString().substring(11, 23);
      parts.push(this.colorize(`[${time}]`, COLORS.gray));
    }

    // 级别
    const levelStr = this.formatLevel(record.level);
    parts.push(levelStr);

    // 分类
    parts.push(this.colorize(`[${record.category}]`, COLORS.cyan));

    // 消息
    parts.push(record.message);

    // 输出
    const line = parts.join(' ');
    console.log(line);

    // 数据
    if (record.data !== undefined) {
      console.log(this.colorize('  Data:', COLORS.gray), record.data);
    }

    // 错误
    if (record.error) {
      console.log(this.colorize('  Error:', COLORS.red), record.error.message);
      if (record.error.stack) {
        console.log(this.colorize(record.error.stack, COLORS.dim));
      }
    }
  }

  private writeAgentMessageToConsole(log: AgentMessageLog): void {
    if (!this.config.console.enabled) return;

    const time = new Date(log.timestamp).toISOString().substring(11, 23);
    const arrow = log.direction === 'send' ? '→' : '←';
    const color = log.direction === 'send' ? COLORS.blue : COLORS.green;

    console.log('');
    console.log(this.colorize(`━━━ Agent ${log.direction === 'send' ? '发送' : '接收'} ━━━`, color));
    console.log(this.colorize(`  时间: ${time}`, COLORS.gray));
    console.log(`  Agent: ${log.agent} (${log.role})`);
    console.log(`  Request ID: ${log.requestId}`);

    // 显示对话上下文（必需）
    if (log.conversationContext) {
      const ctx = log.conversationContext;
      if (ctx.sessionId) {
        console.log(this.colorize(`  Session: ${ctx.sessionId}`, COLORS.gray));
      }
      if (ctx.taskId) {
        console.log(`  Task: ${ctx.taskId}${ctx.subTaskId ? ' / ' + ctx.subTaskId : ''}`);
      }
      if (ctx.messageIndex !== undefined && ctx.totalMessages !== undefined) {
        console.log(this.colorize(`  Message: ${ctx.messageIndex + 1}/${ctx.totalMessages}`, COLORS.gray));
      }
    }

    if (log.duration) {
      console.log(`  Duration: ${(log.duration / 1000).toFixed(2)}s`);
    }

    console.log(this.colorize('  ┌─────────────────────────────────────────────────────────────┐', COLORS.gray));

    // 分行显示内容
    const lines = log.content.split('\n');
    lines.forEach(line => {
      const truncated = line.length > 60 ? line.substring(0, 57) + '...' : line;
      console.log(this.colorize('  │', COLORS.gray), truncated.padEnd(59), this.colorize('│', COLORS.gray));
    });

    if (log.truncated) {
      console.log(this.colorize('  │', COLORS.gray), this.colorize(`... (截断，总长度: ${log.contentLength})`, COLORS.yellow).padEnd(59), this.colorize('│', COLORS.gray));
    }

    // 如果有处理后的内容，显示提示
    if (log.processedContent && log.processedContent !== log.fullContent) {
      console.log(this.colorize('  │', COLORS.gray), this.colorize('(已格式化处理，详见文件日志)', COLORS.cyan).padEnd(59), this.colorize('│', COLORS.gray));
    }

    console.log(this.colorize('  └─────────────────────────────────────────────────────────────┘', COLORS.gray));
    console.log('');
  }

  private formatLevel(level: LogLevel): string {
    const labels = {
      [LogLevel.DEBUG]: 'DEBUG',
      [LogLevel.INFO]: 'INFO ',
      [LogLevel.WARN]: 'WARN ',
      [LogLevel.ERROR]: 'ERROR',
      [LogLevel.SILENT]: 'SILENT',
    };

    const colors = {
      [LogLevel.DEBUG]: COLORS.gray,
      [LogLevel.INFO]: COLORS.blue,
      [LogLevel.WARN]: COLORS.yellow,
      [LogLevel.ERROR]: COLORS.red,
      [LogLevel.SILENT]: COLORS.gray,
    };

    const label = labels[level];
    const color = colors[level];
    return this.colorize(`[${label}]`, color);
  }

  private colorize(text: string, color: string): string {
    if (!this.config.console.colorize) return text;
    return `${color}${text}${COLORS.reset}`;
  }

  // --------------------------------------------------------------------------
  // 文件输出
  // --------------------------------------------------------------------------

  private initFileLogging(): void {
    const logDir = this.config.file.path;
    if (!fs.existsSync(logDir)) {
      fs.mkdirSync(logDir, { recursive: true });
    }

    const logFile = path.join(logDir, `magi-${Date.now()}.log`);
    this.fileStream = fs.createWriteStream(logFile, { flags: 'a' });
    this.currentFileSize = 0;

    // 定期刷新缓冲区
    this.flushTimer = setInterval(() => this.flushBuffer(), 1000);
  }

  private closeFileLogging(): void {
    if (this.flushTimer) {
      clearInterval(this.flushTimer);
      this.flushTimer = undefined;
    }

    this.flushBuffer();

    if (this.fileStream) {
      this.fileStream.end();
      this.fileStream = undefined;
    }
  }

  private writeToFile(record: LogRecord): void {
    if (!this.config.file.enabled || !this.fileStream) return;

    const line = JSON.stringify({
      timestamp: new Date(record.timestamp).toISOString(),
      level: LogLevel[record.level],
      category: record.category,
      message: record.message,
      data: record.data,
      error: record.error ? {
        message: record.error.message,
        stack: record.error.stack,
      } : undefined,
    }) + '\n';

    this.logBuffer.push(line);
    this.currentFileSize += line.length;

    // 检查文件大小
    if (this.currentFileSize >= this.config.file.maxSize) {
      this.rotateLogFile();
    }
  }

  private writeAgentMessageToFile(log: AgentMessageLog): void {
    if (!this.config.file.enabled || !this.fileStream) return;

    // 文件日志保存完整内容
    const fileContent = this.config.agent.maxLengthFile > 0 && log.fullContent
      ? log.fullContent.substring(0, this.config.agent.maxLengthFile)
      : log.fullContent || log.content;

    const line = JSON.stringify({
      timestamp: new Date(log.timestamp).toISOString(),
      type: 'agent-message',
      direction: log.direction,
      agent: log.agent,
      role: log.role,
      requestId: log.requestId,
      // 保存完整内容和处理后的内容
      content: fileContent,
      contentLength: log.fullContent?.length || log.contentLength,
      processedContent: log.processedContent,
      // 对话上下文
      conversationContext: log.conversationContext,
      // 执行时长
      duration: log.duration,
      // 标记是否被截断
      truncatedInFile: this.config.agent.maxLengthFile > 0 && log.fullContent && log.fullContent.length > this.config.agent.maxLengthFile,
    }) + '\n';

    this.logBuffer.push(line);
    this.currentFileSize += line.length;

    if (this.currentFileSize >= this.config.file.maxSize) {
      this.rotateLogFile();
    }
  }

  private flushBuffer(): void {
    if (this.logBuffer.length === 0 || !this.fileStream) return;

    const content = this.logBuffer.join('');
    this.fileStream.write(content);
    this.logBuffer = [];
  }

  private rotateLogFile(): void {
    this.flushBuffer();

    if (this.fileStream) {
      this.fileStream.end();
    }

    // 清理历史文件
    this.cleanOldLogFiles();

    // 创建新文件
    const logFile = path.join(this.config.file.path, `magi-${Date.now()}.log`);
    this.fileStream = fs.createWriteStream(logFile, { flags: 'a' });
    this.currentFileSize = 0;
  }

  private cleanOldLogFiles(): void {
    const logDir = this.config.file.path;
    if (!fs.existsSync(logDir)) return;

    const files = fs.readdirSync(logDir)
      .filter(f => f.startsWith('magi-') && f.endsWith('.log'))
      .map(f => ({
        name: f,
        path: path.join(logDir, f),
        time: fs.statSync(path.join(logDir, f)).mtime.getTime(),
      }))
      .sort((a, b) => b.time - a.time);

    // 保留最新的 N 个文件
    const filesToDelete = files.slice(this.config.file.maxFiles);
    filesToDelete.forEach(file => {
      try {
        fs.unlinkSync(file.path);
      } catch (error) {
        // 忽略删除错误
      }
    });
  }

  // --------------------------------------------------------------------------
  // 工具方法
  // --------------------------------------------------------------------------

  isDebugEnabled(category: LogCategory = LogCategory.SYSTEM): boolean {
    return this.shouldLog(LogLevel.DEBUG, category);
  }

  isInfoEnabled(category: LogCategory = LogCategory.SYSTEM): boolean {
    return this.shouldLog(LogLevel.INFO, category);
  }

  // --------------------------------------------------------------------------
  // 清理
  // --------------------------------------------------------------------------

  destroy(): void {
    this.closeFileLogging();
    this.removeAllListeners();
  }
}

// ============================================================================
// 全局单例
// ============================================================================

export const logger = new UnifiedLogger();

// 进程退出时清理
process.on('exit', () => {
  logger.destroy();
});

process.on('SIGINT', () => {
  logger.destroy();
  process.exit(0);
});

process.on('SIGTERM', () => {
  logger.destroy();
  process.exit(0);
});
