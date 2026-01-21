/**
 * 依赖注入容器配置
 *
 * 使用 InversifyJS 管理依赖关系
 */

import 'reflect-metadata';
import { Container } from 'inversify';
import { TYPES } from './types';

// 导入需要注册的类
import { ConfigManager } from '../config';
import { IDGenerator } from '../utils/id-generator';
import { TokenCounter } from '../utils/token-counter';
import { PerformanceMonitor } from '../monitoring/performance-monitor';
import { ErrorHandler } from '../errors';
import { LockManager } from '../utils/file-lock';

/**
 * 创建并配置 DI 容器
 */
export function createContainer(): Container {
  const container = new Container();

  // 配置管理 - Singleton
  container.bind(TYPES.ConfigManager).toDynamicValue(() => {
    return ConfigManager.getInstance();
  }).inSingletonScope();

  // 工具类 - Singleton（静态类，返回类本身）
  container.bind(TYPES.IDGenerator).toConstantValue(IDGenerator);
  container.bind(TYPES.TokenCounter).toConstantValue(TokenCounter);

  // 性能监控 - Singleton
  container.bind(TYPES.PerformanceMonitor).toDynamicValue(() => {
    return PerformanceMonitor.getInstance();
  }).inSingletonScope();

  // 错误处理 - Singleton（静态类，返回类本身）
  container.bind(TYPES.ErrorHandler).toConstantValue(ErrorHandler);

  // 锁管理 - Singleton
  container.bind(TYPES.LockManager).toDynamicValue(() => {
    return LockManager.getInstance();
  }).inSingletonScope();

  return container;
}

/**
 * 全局容器实例
 */
let globalContainer: Container | null = null;

/**
 * 获取全局容器实例
 */
export function getContainer(): Container {
  if (!globalContainer) {
    globalContainer = createContainer();
  }
  return globalContainer;
}

/**
 * 重置全局容器（主要用于测试）
 */
export function resetContainer(): void {
  globalContainer = null;
}

/**
 * 便捷的依赖获取函数
 */
export function get<T>(serviceIdentifier: symbol): T {
  return getContainer().get<T>(serviceIdentifier);
}
