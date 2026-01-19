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

// 智能编排器
export {
  IntelligentOrchestrator,
  type ExecutionPlan,
  type SubTaskPlan,
  type ExecutionResult,
  type OrchestratorConfig,
  type OrchestratorPhase,
  type ConfirmationCallback,
  type RecoveryConfirmationCallback,
  type ClarificationCallback,
  type WorkerQuestionCallback,
} from './intelligent-orchestrator';

// 风险策略内核
export { RiskPolicy } from './risk-policy';

// 统一策略引擎
export {
  PolicyEngine,
  type CLISelectionPolicy,
  type ConflictDetectionResult,
  type VerificationDecision,
  type CLIHealthStatus,
} from './policy-engine';

// 消息总线
export { MessageBus, globalMessageBus } from './message-bus';

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

// 恢复处理器
export {
  RecoveryHandler,
  type RecoveryStrategy,
  type RecoveryResult,
  type RecoveryConfig,
} from './recovery-handler';
