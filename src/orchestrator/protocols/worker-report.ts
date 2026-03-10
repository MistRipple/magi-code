/**
 * Worker 汇报协议
 *
 * 定义 Worker 与 Orchestrator 之间的双向通信协议：
 * - WorkerReport: Worker → Orchestrator 汇报
 * - OrchestratorResponse: Orchestrator → Worker 响应
 *
 * 设计原则：
 * - Worker 每完成一个 Todo 必须汇报
 * - Orchestrator 可以中途调整 Worker 行为
 * - 禁止 Worker 自行决定流程
 */

import { WorkerSlot } from '../../types';

// ============================================================================
// Worker → Orchestrator 汇报
// ============================================================================

/**
 * 汇报类型
 */
export type WorkerReportType =
  | 'progress'    // 进度报告（完成一个 Todo）
  | 'question'    // 遇到问题需要决策
  | 'completed'   // 任务完成
  | 'failed';     // 任务失败

/**
 * Worker 汇报
 */
export interface WorkerReport {
  /** 汇报类型 */
  type: WorkerReportType;

  /** Worker 标识 */
  workerId: WorkerSlot;

  /** Assignment ID */
  assignmentId: string;

  /** 汇报时间戳 */
  timestamp: number;

  /** 进度信息（type=progress 时） */
  progress?: WorkerProgress;

  /** 执行结果（type=completed/failed 时） */
  result?: WorkerResult;

  /** 问题信息（type=question 时） */
  question?: WorkerQuestion;

  /** 错误信息（type=failed 时） */
  error?: string;
}

/**
 * Worker 进度
 */
export interface WorkerProgress {
  /** 当前执行的步骤 */
  currentStep: string;

  /** 当前 Todo ID */
  currentTodoId: string;

  /** 已完成的步骤 */
  completedSteps: string[];

  /** 剩余步骤 */
  remainingSteps: string[];

  /** 完成百分比 0-100 */
  percentage: number;

  /** 本次步骤耗时(ms) */
  stepDuration: number;
}

/**
 * Worker 执行结果
 */
export interface WorkerResult {
  /** 是否成功 */
  success: boolean;

  /** 修改的文件 */
  modifiedFiles: string[];

  /** 新建的文件 */
  createdFiles: string[];

  /** 执行摘要 */
  summary: string;

  /** 总耗时(ms) */
  totalDuration: number;

  /** Token 使用统计 */
  tokenUsage?: {
    inputTokens: number;
    outputTokens: number;
  };

  /** 验证证据 - 提案 4.2 */
  evidence?: WorkerEvidence;

  /** 结构化知识提取 - 提案 4.5 */
  wisdomExtraction?: WisdomExtraction;
}

// ============================================================================
// 验证证据类型 - 提案 4.2: Trust But Verify
// ============================================================================

/**
 * 命令执行记录
 */
export interface CommandRecord {
  /** 执行的命令 */
  command: string;
  /** 退出码 */
  exitCode: number;
  /** 标准输出（截断） */
  stdout?: string;
  /** 标准错误（截断） */
  stderr?: string;
  /** 执行时间(ms) */
  duration?: number;
}

/**
 * 测试结果
 */
export interface TestResult {
  /** 测试框架 */
  framework: string;
  /** 总测试数 */
  total: number;
  /** 通过数 */
  passed: number;
  /** 失败数 */
  failed: number;
  /** 跳过数 */
  skipped?: number;
  /** 执行时间(ms) */
  duration: number;
  /** 失败的测试名称 */
  failedTests?: string[];
}

/**
 * 类型检查结果
 */
export interface TypeCheckResult {
  /** 是否通过 */
  passed: boolean;
  /** 错误列表 */
  errors?: string[];
  /** 警告数 */
  warningCount?: number;
}

/**
 * 文件变更记录
 */
export interface FileChangeRecord {
  /** 文件路径 */
  path: string;
  /** 变更类型 */
  action: 'create' | 'modify' | 'delete';
  /** 新增行数 */
  linesAdded?: number;
  /** 删除行数 */
  linesRemoved?: number;
  /** 文件大小(bytes) */
  size?: number;
}

/**
 * Worker 验证证据
 */
export interface WorkerEvidence {
  /** 执行的命令及输出 */
  commandsRun?: CommandRecord[];

  /** 测试结果 */
  testResults?: TestResult;

  /** 类型检查结果 */
  typeCheckResult?: TypeCheckResult;

  /** 文件变更证据 */
  fileChanges?: FileChangeRecord[];

  /** 验证时间戳 */
  verifiedAt?: number;

  /** 验证状态 */
  verificationStatus?: 'pending' | 'verified' | 'failed';

  /** 验证失败原因 */
  verificationIssues?: string[];
}

// ============================================================================
// Wisdom 提取类型 - 提案 4.5
// ============================================================================

/**
 * 结构化知识提取
 */
export interface WisdomExtraction {
  /** 学习到的信息 */
  learnings?: string[];
  /** 做出的决策 */
  decisions?: string[];
  /** 需要注意的问题 */
  warnings?: string[];
  /** 值得跨会话保存的重要经验 */
  significantLearning?: string;
}

/**
 * Worker 问题
 */
export interface WorkerQuestion {
  /** 问题内容 */
  content: string;

  /** 可选项（如果有） */
  options?: string[];

  /** 是否阻塞执行 */
  blocking: boolean;

  /** 问题类型 */
  questionType: 'clarification' | 'approval' | 'decision';

  /** 相关 Todo ID */
  todoId?: string;
}

// ============================================================================
// Orchestrator → Worker 响应
// ============================================================================

/**
 * Orchestrator 响应动作
 */
export type OrchestratorAction =
  | 'continue'    // 继续执行
  | 'adjust'      // 调整策略
  | 'abort'       // 终止执行
  | 'answer';     // 回答问题

/**
 * Orchestrator 响应
 */
export interface OrchestratorResponse {
  /** 响应动作 */
  action: OrchestratorAction;

  /** 响应时间戳 */
  timestamp: number;

  /** 调整指令（action=adjust 时） */
  adjustment?: OrchestratorAdjustment;

  /** 回答内容（action=answer 时） */
  answer?: string;

  /** 终止原因（action=abort 时） */
  abortReason?: string;
}

/**
 * Orchestrator 调整指令
 */
export interface OrchestratorAdjustment {
  /** 新的指令 */
  newInstructions?: string;

  /** 跳过的步骤 */
  skipSteps?: string[];

  /** 新增的步骤 */
  addSteps?: string[];

  /** 优先级调整 */
  priorityChanges?: Record<string, number>;

  /** 超时调整(ms) */
  timeoutAdjustment?: number;
}

// ============================================================================
// 汇报回调接口
// ============================================================================

/**
 * 汇报回调函数类型
 * Worker 通过此回调向 Orchestrator 汇报
 */
export type ReportCallback = (report: WorkerReport) => Promise<OrchestratorResponse>;

/**
 * 汇报选项
 */
export interface ReportOptions {
  /** 汇报回调 */
  onReport: ReportCallback;

  /** 汇报超时(ms)。未配置时由 Worker 侧按汇报类型使用内置默认值。 */
  reportTimeout?: number;

  /** 是否在每个 Todo 完成后汇报，默认 true */
  reportOnTodoComplete?: boolean;
}

// ============================================================================
// 工厂函数
// ============================================================================

/**
 * 创建进度汇报
 */
export function createProgressReport(
  workerId: WorkerSlot,
  assignmentId: string,
  progress: WorkerProgress
): WorkerReport {
  return {
    type: 'progress',
    workerId,
    assignmentId,
    timestamp: Date.now(),
    progress,
  };
}

/**
 * 创建完成汇报
 */
export function createCompletedReport(
  workerId: WorkerSlot,
  assignmentId: string,
  result: WorkerResult
): WorkerReport {
  return {
    type: 'completed',
    workerId,
    assignmentId,
    timestamp: Date.now(),
    result,
  };
}

/**
 * 创建失败汇报
 */
export function createFailedReport(
  workerId: WorkerSlot,
  assignmentId: string,
  error: string,
  result?: Partial<WorkerResult>
): WorkerReport {
  return {
    type: 'failed',
    workerId,
    assignmentId,
    timestamp: Date.now(),
    error,
    result: result ? {
      success: false,
      modifiedFiles: result.modifiedFiles || [],
      createdFiles: result.createdFiles || [],
      summary: result.summary || '',
      totalDuration: result.totalDuration || 0,
    } : undefined,
  };
}

/**
 * 创建问题汇报
 */
export function createQuestionReport(
  workerId: WorkerSlot,
  assignmentId: string,
  question: WorkerQuestion
): WorkerReport {
  return {
    type: 'question',
    workerId,
    assignmentId,
    timestamp: Date.now(),
    question,
  };
}

/**
 * 创建继续响应
 */
export function createContinueResponse(): OrchestratorResponse {
  return {
    action: 'continue',
    timestamp: Date.now(),
  };
}

/**
 * 创建调整响应
 */
export function createAdjustResponse(
  adjustment: OrchestratorAdjustment
): OrchestratorResponse {
  return {
    action: 'adjust',
    timestamp: Date.now(),
    adjustment,
  };
}

/**
 * 创建终止响应
 */
export function createAbortResponse(reason: string): OrchestratorResponse {
  return {
    action: 'abort',
    timestamp: Date.now(),
    abortReason: reason,
  };
}

/**
 * 创建回答响应
 */
export function createAnswerResponse(answer: string): OrchestratorResponse {
  return {
    action: 'answer',
    timestamp: Date.now(),
    answer,
  };
}
