/**
 * 编排者专用 Prompt 模板
 *
 * 核心理念：
 * - 编排者 Claude 专职编排，不执行任何编码任务
 * - 所有 Prompt 都围绕"分析、规划、监控、汇总"设计
 */
import { WorkerType, ExecutionResult, ExecutionPlan } from '../protocols/types';
/** 获取 Worker 能力描述 */
export declare function getWorkerDescription(worker: WorkerType): string;
/**
 * 构建任务分析 Prompt
 * 编排者分析用户需求，生成执行计划
 */
export declare function buildOrchestratorAnalysisPrompt(userPrompt: string, availableWorkers: WorkerType[], projectContext?: string): string;
/**
 * 构建汇总报告 Prompt
 * 编排者整合所有 Worker 的执行结果
 */
export declare function buildOrchestratorSummaryPrompt(originalPrompt: string, executionResults: ExecutionResult[]): string;
/**
 * 格式化执行计划为用户可读的文本
 */
export declare function formatPlanForUser(plan: ExecutionPlan): string;
/**
 * 构建进度更新消息
 */
export declare function buildProgressMessage(completedTasks: number, totalTasks: number, currentWorker?: WorkerType, currentTask?: string): string;
export declare const OrchestratorPrompts: {
    getWorkerDescription: typeof getWorkerDescription;
    buildOrchestratorAnalysisPrompt: typeof buildOrchestratorAnalysisPrompt;
    buildOrchestratorSummaryPrompt: typeof buildOrchestratorSummaryPrompt;
    formatPlanForUser: typeof formatPlanForUser;
    buildProgressMessage: typeof buildProgressMessage;
};
//# sourceMappingURL=orchestrator-prompts.d.ts.map