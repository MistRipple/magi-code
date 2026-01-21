/**
 * Orchestrator Module - 编排器模块
 *
 * 导出所有编排器相关模块
 */

// 主门面
export { IntelligentOrchestrator } from './orchestrator-facade';

// 类型导出
export type {
  OrchestratorConfig,
  SubTaskPlan,
  ExecutionPlan,
  ExecutionResult,
  SubTask,
  OrchestratorPhase,
  ConfirmationCallback,
  RecoveryConfirmationCallback,
  ClarificationCallback,
  WorkerQuestionCallback,
} from './orchestrator-facade';

// 子模块导出（供高级用户使用）
export { ConfigResolver } from './config-resolver';
export { TaskContextManager } from './task-context-manager';
export { InteractionModeManager } from './interaction-mode-manager';
export { PlanCoordinator } from './plan-coordinator';
export { ExecutionCoordinator } from './execution-coordinator';
