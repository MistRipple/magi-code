/**
 * Core Module - Mission-Driven Architecture 核心
 *
 * 提供任务编排和执行的核心组件：
 * - MissionOrchestrator: 任务编排核心（含执行能力，MissionExecutor 已合并）
 * - MissionDrivenEngine: 编排引擎
 */

export {
  MissionOrchestrator,
  MissionCreationResult,
  PlanningOptions,
  MissionVerificationResult,
  MissionSummary,
  // 执行相关类型（从 MissionExecutor 合并）
  ExecutionOptions,
  ExecutionProgress,
  ExecutionResult,
} from './mission-orchestrator';

// 阻塞相关类型
export {
  BlockedItem,
  BlockedItemType,
  BlockingReason,
} from './executors/blocking-manager';

// 编排引擎
export {
  MissionDrivenEngine,
  MissionDrivenEngineConfig,
  MissionDrivenContext,
  ConfirmationCallback as MissionConfirmationCallback,
  RecoveryConfirmationCallback as MissionRecoveryConfirmationCallback,
  ClarificationCallback as MissionClarificationCallback,
  WorkerQuestionCallback as MissionWorkerQuestionCallback,
} from './mission-driven-engine';

// 统一消息出口
export {
  MessageHub,
  globalMessageHub,
  type SubTaskView,
  type MessageHubEvents,
} from './message-hub';
