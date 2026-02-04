/**
 * Worker Module - 自主 Worker 系统
 *
 * 提供 Worker 自主规划和执行能力：
 * - TodoManager: 统一 Todo 管理器 (已迁移到 src/todo/)
 * - AutonomousWorker: 自主 Worker
 */

// TodoPlanner 已迁移到 TodoManager (src/todo/todo-manager.ts)
// 请使用: import { TodoManager } from '../../todo';

export {
  AutonomousWorker,
  TodoExecuteOptions,
  AutonomousExecutionResult,
} from './autonomous-worker';

export {
  WorkerSession,
  WorkerSessionManager,
  SessionCreateOptions,
  SessionUpdateOptions,
  ConversationMessage,
  FileCacheEntry,
  SessionStateSnapshot,
  getGlobalSessionManager,
  resetGlobalSessionManager,
} from './worker-session';
