/**
 * Mission-Driven Architecture - 模块导出
 *
 * 本模块提供新架构的核心组件：
 * - Mission 数据模型和类型定义
 * - Contract 契约管理
 * - Assignment 职责分配
 * - Storage 存储层
 * - StateMapper 状态映射
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

// 状态映射器
export {
  MissionStateMapper,
  globalMissionStateMapper,
  type TaskView,
  type SubTaskView as MissionSubTaskView,
  type TodoView,
  type TaskViewStatus,
  type SubTaskViewStatus,
  type TodoViewStatus,
  type StateChangeCallback,
} from './state-mapper';
