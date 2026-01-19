/**
 * Mission-Driven Architecture - 模块导出
 *
 * 本模块提供新架构的核心组件：
 * - Mission 数据模型和类型定义
 * - Contract 契约管理
 * - Assignment 职责分配
 * - Storage 存储层
 * - Migration 迁移工具
 */

// 类型导出
export * from './types';

// 存储层
export {
  IMissionStorage,
  InMemoryMissionStorage,
  FileBasedMissionStorage,
  MissionStorageManager,
  createMissionStorage,
  createFileBasedMissionStorage,
} from './mission-storage';

// 契约管理
export { ContractManager } from './contract-manager';

// 职责分配管理
export { AssignmentManager } from './assignment-manager';

// 迁移工具
export {
  MissionMigrationTool,
  MigrationStats,
  MigrationOptions,
  migrateSessionPlans,
  migrateAllSessions,
} from './migration-tool';
