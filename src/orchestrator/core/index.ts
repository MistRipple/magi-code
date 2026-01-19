/**
 * Core Module - Mission-Driven Architecture 核心
 *
 * 提供任务编排和执行的核心组件：
 * - MissionOrchestrator: 任务编排核心
 * - MissionExecutor: 任务执行器
 * - MissionDrivenEngine: OrchestratorAgent 替代品（Phase 8 迁移）
 */

export {
  MissionOrchestrator,
  MissionCreationResult,
  PlanningOptions,
  MissionVerificationResult,
  MissionSummary,
} from './mission-orchestrator';

export {
  MissionExecutor,
  ExecutionOptions,
  ExecutionProgress,
  ExecutionResult,
  BlockedItemType,
  BlockingReason,
  BlockedItem,
} from './mission-executor';

// Phase 8: OrchestratorAgent 替代品
export {
  MissionDrivenEngine,
  MissionDrivenEngineConfig,
  MissionDrivenContext,
  ConfirmationCallback as MissionConfirmationCallback,
  RecoveryConfirmationCallback as MissionRecoveryConfirmationCallback,
  ClarificationCallback as MissionClarificationCallback,
  WorkerQuestionCallback as MissionWorkerQuestionCallback,
} from './mission-driven-engine';
