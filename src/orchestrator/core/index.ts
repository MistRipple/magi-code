/**
 * Core Module - Mission-Driven Architecture 核心
 *
 * 提供任务编排和执行的核心组件：
 * - MissionOrchestrator: 任务编排核心
 * - MissionDrivenEngine: 编排引擎
 */

export {
  MissionOrchestrator,
  MissionCreationResult,
  PlanningOptions,
  MissionVerificationResult,
  MissionSummary,
} from './mission-orchestrator';

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
  type SubTaskCardPayload,
  type MessageHubEvents,
} from './message-hub';
