/**
 * Orchestrator 模块导出
 * 独立编排者架构
 */

// ============================================================================
// Mission-Driven Architecture (新架构)
// ============================================================================

// Mission 数据模型和类型
export * from './mission';

// 核心编排和执行
export * from './core';

// 自主 Worker 系统
export * from './worker';

// 画像感知评审
export * from './review';

// 画像感知恢复
export * from './recovery';

// ============================================================================
// 核心编排器
// ============================================================================

// 编排引擎
export {
  MissionDrivenEngine,
  type MissionDrivenEngineConfig,
} from './core';

// 协议类型
export type {
  ExecutionPlan,
  ExecutionResult,
  OrchestratorState as OrchestratorPhase,
} from './protocols';

// 风险策略内核
export { RiskPolicy } from './risk-policy';

// 统一策略引擎
export {
  PolicyEngine,
  type WorkerSelectionPolicy,
  type ConflictDetectionResult,
  type VerificationDecision,
  type WorkerHealthStatus,
} from './policy-engine';

// 消息总线
export { MessageBus, globalMessageBus } from './message-bus';

// 统一消息出口（Phase 4）
export {
  MessageHub,
  globalMessageHub,
  type SubTaskCardPayload,
  type MessageHubEvents,
} from './core';

// 协议类型
export * from './protocols';

// 编排者专用 Prompts
export * from './prompts/orchestrator-prompts';

// 验证执行器
export {
  VerificationRunner,
  type VerificationConfig,
  type VerificationResult,
  type CommandResult,
  type IDEDiagnosticResult,
} from './verification-runner';

// 恢复处理器 - 使用 ProfileAwareRecoveryHandler (已移至 recovery/ 目录)
export {
  ProfileAwareRecoveryHandler,
  type RecoveryStrategyType,
  type RecoveryDecision,
  type FailureAnalysis,
} from './recovery';
