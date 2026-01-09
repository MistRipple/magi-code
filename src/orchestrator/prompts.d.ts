/**
 * Prompt 模板系统
 * 用于 Claude 驱动的智能编排
 * 架构理念：各 CLI 各司其职、独立执行、最后汇总
 */
import { CLIType } from '../types';
/**
 * Phase 1: 任务分析 Prompt - 让 Claude 分析任务并输出执行计划
 * 分析完成后需要 Hard Stop，等待用户确认
 */
export declare function buildTaskAnalysisPrompt(userPrompt: string, availableCLIs: CLIType[], projectContext?: string): string;
/**
 * Phase 3: 执行指令 Prompt - 让各 CLI 直接执行修改
 * 架构理念：各 CLI 拥有完整写入权限，直接修改文件
 */
export declare function buildExecutionPrompt(taskDescription: string, cli: CLIType, targetFiles?: string[]): string;
/**
 * Phase 4: 汇总报告 Prompt - 让 Claude 汇总各 CLI 的执行结果
 * 注意：Claude 只汇总结果，不重新执行代码
 */
export declare function buildSummaryPrompt(originalPrompt: string, executionResults: Array<{
    cli: CLIType;
    task: string;
    result: string;
    success: boolean;
}>): string;
/**
 * Code Review Prompt - 可选的代码审查功能
 */
export declare function buildCodeReviewPrompt(originalPrompt: string, changedFiles: Array<{
    path: string;
    content: string;
}>, diff: string): string;
/**
 * 格式化执行计划为用户可读的文本（用于 Hard Stop 展示）
 */
export declare function formatPlanForUser(plan: {
    analysis: string;
    isSimpleTask?: boolean;
    skipReason?: string;
    needsCollaboration: boolean;
    subTasks: Array<{
        id: string;
        description: string;
        assignedCli: string;
        reason: string;
        targetFiles?: string[];
    }>;
    executionMode: string;
    summary: string;
}): string;
export declare const PromptTemplates: {
    buildTaskAnalysisPrompt: typeof buildTaskAnalysisPrompt;
    buildExecutionPrompt: typeof buildExecutionPrompt;
    buildSummaryPrompt: typeof buildSummaryPrompt;
    buildCodeReviewPrompt: typeof buildCodeReviewPrompt;
    formatPlanForUser: typeof formatPlanForUser;
};
//# sourceMappingURL=prompts.d.ts.map