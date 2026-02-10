/**
 * 统一错误处理系统
 *
 * 提供：
 * - 结构化错误类型
 * - 错误分类和恢复策略
 * - 统一的错误处理逻辑
 */

import { logger, LogCategory } from '../logging';

/**
 * 错误类别
 */
export enum ErrorCategory {
  CONFIGURATION = 'configuration',
  ORCHESTRATION = 'orchestration',
  EXECUTION = 'execution',
  CONTEXT = 'context',
  SNAPSHOT = 'snapshot',
  TASK = 'task',
  PROFILE = 'profile',
  NETWORK = 'network',
  FILESYSTEM = 'filesystem',
  VALIDATION = 'validation',
  UNKNOWN = 'unknown',
}

/**
 * 错误严重程度
 */
export enum ErrorSeverity {
  LOW = 'low',
  MEDIUM = 'medium',
  HIGH = 'high',
  CRITICAL = 'critical',
}

/**
 * Magi 基础错误类
 */
export class MagiError extends Error {
  constructor(
    message: string,
    public code: string,
    public category: ErrorCategory,
    public severity: ErrorSeverity = ErrorSeverity.MEDIUM,
    public recoverable: boolean = true,
    public context?: Record<string, any>
  ) {
    super(message);
    this.name = 'MagiError';
    Error.captureStackTrace(this, this.constructor);
  }

  /**
   * 转换为 JSON
   */
  toJSON(): Record<string, any> {
    return {
      name: this.name,
      message: this.message,
      code: this.code,
      category: this.category,
      severity: this.severity,
      recoverable: this.recoverable,
      context: this.context,
      stack: this.stack,
    };
  }
}

/**
 * 配置错误
 */
export class ConfigurationError extends MagiError {
  constructor(message: string, context?: Record<string, any>) {
    super(
      message,
      'CONFIG_ERROR',
      ErrorCategory.CONFIGURATION,
      ErrorSeverity.HIGH,
      false,
      context
    );
    this.name = 'ConfigurationError';
  }
}

/**
 * 编排错误
 */
export class OrchestrationError extends MagiError {
  constructor(message: string, recoverable: boolean = true, context?: Record<string, any>) {
    super(
      message,
      'ORCHESTRATION_ERROR',
      ErrorCategory.ORCHESTRATION,
      ErrorSeverity.MEDIUM,
      recoverable,
      context
    );
    this.name = 'OrchestrationError';
  }
}

/**
 * 执行错误
 */
export class ExecutionError extends MagiError {
  constructor(message: string, recoverable: boolean = true, context?: Record<string, any>) {
    super(
      message,
      'EXECUTION_ERROR',
      ErrorCategory.EXECUTION,
      ErrorSeverity.MEDIUM,
      recoverable,
      context
    );
    this.name = 'ExecutionError';
  }
}

/**
 * 上下文错误
 */
export class ContextError extends MagiError {
  constructor(message: string, context?: Record<string, any>) {
    super(
      message,
      'CONTEXT_ERROR',
      ErrorCategory.CONTEXT,
      ErrorSeverity.LOW,
      true,
      context
    );
    this.name = 'ContextError';
  }
}

/**
 * 快照错误
 */
export class SnapshotError extends MagiError {
  constructor(message: string, recoverable: boolean = true, context?: Record<string, any>) {
    super(
      message,
      'SNAPSHOT_ERROR',
      ErrorCategory.SNAPSHOT,
      ErrorSeverity.MEDIUM,
      recoverable,
      context
    );
    this.name = 'SnapshotError';
  }
}

/**
 * 任务错误
 */
export class TaskError extends MagiError {
  constructor(message: string, context?: Record<string, any>) {
    super(
      message,
      'TASK_ERROR',
      ErrorCategory.TASK,
      ErrorSeverity.LOW,
      true,
      context
    );
    this.name = 'TaskError';
  }
}

/**
 * 画像错误
 */
export class ProfileError extends MagiError {
  constructor(message: string, context?: Record<string, any>) {
    super(
      message,
      'PROFILE_ERROR',
      ErrorCategory.PROFILE,
      ErrorSeverity.MEDIUM,
      false,
      context
    );
    this.name = 'ProfileError';
  }
}

/**
 * 验证错误
 */
export class ValidationError extends MagiError {
  constructor(message: string, context?: Record<string, any>) {
    super(
      message,
      'VALIDATION_ERROR',
      ErrorCategory.VALIDATION,
      ErrorSeverity.LOW,
      true,
      context
    );
    this.name = 'ValidationError';
  }
}

/**
 * 错误处理结果
 */
export interface ErrorHandlingResult {
  /** 是否应该重试 */
  shouldRetry: boolean;
  /** 是否应该回滚 */
  shouldRollback: boolean;
  /** 用户友好的错误消息 */
  userMessage: string;
  /** 建议的恢复操作 */
  suggestedAction?: string;
}

/**
 * 错误处理器
 */
export class ErrorHandler {
  /**
   * 处理错误
   */
  static handle(error: Error, context?: Record<string, any>): ErrorHandlingResult {
    if (error instanceof MagiError) {
      return this.handleMagiError(error);
    }

    // 未知错误
    logger.error('未知错误', { error: error.message, stack: error.stack, context }, LogCategory.SYSTEM);

    return {
      shouldRetry: false,
      shouldRollback: true,
      userMessage: '发生未知错误，请查看日志获取详细信息',
      suggestedAction: '检查日志文件并报告问题',
    };
  }

  /**
   * 处理 Magi 错误
   */
  private static handleMagiError(error: MagiError): ErrorHandlingResult {
    // 记录日志
    const logCategory = this.mapCategoryToLogCategory(error.category);
    const logLevel = this.mapSeverityToLogLevel(error.severity);

    if (logLevel === 'error') {
      logger.error(
        `${error.category}.错误`,
        { code: error.code, message: error.message, context: error.context },
        logCategory
      );
    } else if (logLevel === 'warn') {
      logger.warn(
        `${error.category}.警告`,
        { code: error.code, message: error.message, context: error.context },
        logCategory
      );
    }

    // 确定恢复策略
    return {
      shouldRetry: error.recoverable && error.severity !== ErrorSeverity.CRITICAL,
      shouldRollback: !error.recoverable || error.severity === ErrorSeverity.CRITICAL,
      userMessage: error.message,
      suggestedAction: this.getSuggestedAction(error),
    };
  }

  /**
   * 映射错误类别到日志类别
   */
  private static mapCategoryToLogCategory(category: ErrorCategory): LogCategory {
    const mapping: Record<ErrorCategory, LogCategory> = {
      [ErrorCategory.CONFIGURATION]: LogCategory.SYSTEM,
      [ErrorCategory.ORCHESTRATION]: LogCategory.ORCHESTRATOR,
      [ErrorCategory.EXECUTION]: LogCategory.ORCHESTRATOR,
      [ErrorCategory.CONTEXT]: LogCategory.SESSION,
      [ErrorCategory.SNAPSHOT]: LogCategory.RECOVERY,
      [ErrorCategory.TASK]: LogCategory.ORCHESTRATOR,
      [ErrorCategory.PROFILE]: LogCategory.ORCHESTRATOR,
      [ErrorCategory.NETWORK]: LogCategory.SYSTEM,
      [ErrorCategory.FILESYSTEM]: LogCategory.SYSTEM,
      [ErrorCategory.VALIDATION]: LogCategory.ORCHESTRATOR,
      [ErrorCategory.UNKNOWN]: LogCategory.SYSTEM,
    };

    return mapping[category] || LogCategory.SYSTEM;
  }

  /**
   * 映射严重程度到日志级别
   */
  private static mapSeverityToLogLevel(severity: ErrorSeverity): 'error' | 'warn' | 'info' {
    if (severity === ErrorSeverity.CRITICAL || severity === ErrorSeverity.HIGH) {
      return 'error';
    } else if (severity === ErrorSeverity.MEDIUM) {
      return 'warn';
    }
    return 'info';
  }

  /**
   * 获取建议的操作
   */
  private static getSuggestedAction(error: MagiError): string {
    switch (error.category) {
      case ErrorCategory.CONFIGURATION:
        return '检查配置文件 ~/.magi/config.json 或环境变量';
      case ErrorCategory.ORCHESTRATION:
        return error.recoverable ? '重试操作或调整计划' : '检查任务定义和依赖关系';
      case ErrorCategory.EXECUTION:
        return error.recoverable ? '重试执行或检查 Worker 状态' : '回滚更改并检查错误日志';
      case ErrorCategory.CONTEXT:
        return '清理上下文缓存或重启会话';
      case ErrorCategory.SNAPSHOT:
        return '检查文件权限和磁盘空间';
      case ErrorCategory.TASK:
        return '检查任务队列状态';
      case ErrorCategory.PROFILE:
        return '检查 Worker 画像配置';
      case ErrorCategory.VALIDATION:
        return '检查输入数据格式';
      default:
        return '查看日志获取更多信息';
    }
  }

  /**
   * 包装异步函数，自动处理错误
   */
  static async wrap<T>(
    fn: () => Promise<T>,
    context?: Record<string, any>
  ): Promise<{ success: boolean; data?: T; error?: ErrorHandlingResult }> {
    try {
      const data = await fn();
      return { success: true, data };
    } catch (error) {
      const result = this.handle(error as Error, context);
      return { success: false, error: result };
    }
  }
}
