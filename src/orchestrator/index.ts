/**
 * Orchestrator 模块导出
 * 独立编排者架构
 */

// 核心编排器
export {
  IntelligentOrchestrator,
  type ExecutionPlan,
  type SubTaskPlan,
  type ExecutionResult,
  type OrchestratorConfig,
  type OrchestratorPhase,
  type ConfirmationCallback,
  type RecoveryConfirmationCallback,
} from './intelligent-orchestrator';

// 独立编排者 Agent
export { OrchestratorAgent } from './orchestrator-agent';

// Worker Agent
export { WorkerAgent, type WorkerConfig } from './worker-agent';

// Worker Pool
export { WorkerPool, type WorkerPoolConfig } from './worker-pool';

// 消息总线
export { MessageBus, globalMessageBus } from './message-bus';

// 协议类型
export * from './protocols';

// 编排者专用 Prompts
export * from './prompts/orchestrator-prompts';

// 任务状态管理
export {
  TaskStateManager,
  type TaskState,
  type TaskStatus,
  type StateChangeCallback,
} from './task-state-manager';

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

// 旧版 Prompt 构建器（向后兼容）
export {
  buildTaskAnalysisPrompt,
  buildExecutionPrompt,
  buildSummaryPrompt,
  formatPlanForUser,
} from './prompts';