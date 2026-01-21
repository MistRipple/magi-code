/**
 * 依赖注入类型标识符
 *
 * 使用 Symbol 确保唯一性
 */

export const TYPES = {
  // 配置管理
  ConfigManager: Symbol.for('ConfigManager'),

  // 工具类
  IDGenerator: Symbol.for('IDGenerator'),
  TokenCounter: Symbol.for('TokenCounter'),
  PerformanceMonitor: Symbol.for('PerformanceMonitor'),

  // 错误处理
  ErrorHandler: Symbol.for('ErrorHandler'),

  // 锁管理
  LockManager: Symbol.for('LockManager'),

  // 核心服务
  ContextManager: Symbol.for('ContextManager'),
  SnapshotManager: Symbol.for('SnapshotManager'),
  TaskManager: Symbol.for('TaskManager'),

  // 编排器
  MissionOrchestrator: Symbol.for('MissionOrchestrator'),
  MissionExecutor: Symbol.for('MissionExecutor'),

  // Worker 相关
  ProfileLoader: Symbol.for('ProfileLoader'),
  GuidanceInjector: Symbol.for('GuidanceInjector'),

  // CLI 适配器
  CLIAdapterFactory: Symbol.for('CLIAdapterFactory'),
};
