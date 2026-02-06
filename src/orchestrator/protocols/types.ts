/**
 * 独立编排者架构 - 核心类型定义
 *
 * 架构理念：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */

import { WorkerSlot, SubTask, PermissionMatrix, StrategyConfig } from '../../types';

// 重新导出统一类型
export { SubTask, WorkerSlot };

// ============================================================================
// Worker 相关类型
// ============================================================================

/** Worker 状态 */
export type WorkerState = 'idle' | 'executing' | 'completed' | 'failed' | 'cancelled';

/** Worker 信息 */
export interface WorkerInfo {
  id: string;
  type: WorkerSlot;
  state: WorkerState;
  currentTaskId?: string;
  lastActivity?: number;
}

/** 执行计划 */
export interface ExecutionPlan {
  id: string;
  analysis: string;
  isSimpleTask?: boolean;
  /** 是否需要 Worker 执行（false 表示编排者直接回答） */
  needsWorker?: boolean;
  /** 编排者直接回答的内容（当 needsWorker=false 时使用） */
  directResponse?: string;
  /** 是否需要用户补充信息 */
  needsUserInput?: boolean;
  /** 需要用户回答的问题列表 */
  questions?: string[];
  skipReason?: string;
  needsCollaboration: boolean;
  subTasks: SubTask[];
  executionMode: 'parallel' | 'sequential';
  summary: string;
  /** 功能契约（统一前后端约束） */
  featureContract: string;
  /** 验收清单 */
  acceptanceCriteria: string[];
  createdAt: number;
  /** 风险等级（来自画像系统） */
  riskLevel?: 'low' | 'medium' | 'high' | 'critical';
}

/** 执行结果 */
export interface ExecutionResult {
  workerId: string;
  workerType: WorkerSlot;
  taskId: string;
  subTaskId: string;
  dispatchId?: string;
  result: string;
  success: boolean;
  duration: number;
  modifiedFiles?: string[];
  error?: string;
  inputTokens?: number;
  outputTokens?: number;
}

// ============================================================================
// 消息总线类型
// ============================================================================

/** 消息类型 */
export type MessageType =
  | 'task_dispatch'           // 编排者 -> Worker：分配任务
  | 'task_cancel'             // 编排者 -> Worker：取消任务
  | 'progress_report'         // Worker -> 编排者：进度汇报
  | 'task_completed'          // Worker -> 编排者：任务完成
  | 'task_failed'             // Worker -> 编排者：任务失败
  | 'worker_ready'            // Worker -> 编排者：Worker 就绪
  | 'orchestrator_command'    // 编排者广播命令
  | 'clarification_request'   // 编排者 -> 用户：请求澄清需求
  | 'clarification_response'  // 用户 -> 编排者：回答澄清问题
  | 'worker_question'         // Worker -> 编排者：子代理提问
  | 'worker_answer';          // 编排者 -> Worker：回答子代理问题

/** 基础消息结构 */
export interface BaseMessage {
  id: string;
  type: MessageType;
  timestamp: number;
  source: string;  // 发送者 ID
  target?: string; // 接收者 ID（可选，用于定向消息）
}

/** 任务分发消息 */
export interface TaskDispatchMessage extends BaseMessage {
  type: 'task_dispatch';
  payload: {
    taskId: string;
    subTask: SubTask;
    context?: string;
    dispatchId?: string;
  };
}

/** 任务取消消息 */
export interface TaskCancelMessage extends BaseMessage {
  type: 'task_cancel';
  payload: {
    taskId: string;
    subTaskId?: string;
    reason?: string;
  };
}

/** 进度汇报消息 */
export interface ProgressReportMessage extends BaseMessage {
  type: 'progress_report';
  payload: {
    taskId: string;
    subTaskId: string;
    dispatchId?: string;
    status: 'started' | 'in_progress' | 'completed' | 'failed';
    progress?: number; // 0-100
    message?: string;
    output?: string;   // 流式输出内容
  };
}

/** 任务完成消息 */
export interface TaskCompletedMessage extends BaseMessage {
  type: 'task_completed';
  payload: {
    result: ExecutionResult;
  };
}

/** 任务失败消息 */
export interface TaskFailedMessage extends BaseMessage {
  type: 'task_failed';
  payload: {
    taskId: string;
    subTaskId: string;
    error: string;
    canRetry: boolean;
    dispatchId?: string;
  };
}

/** Worker 就绪消息 */
export interface WorkerReadyMessage extends BaseMessage {
  type: 'worker_ready';
  payload: {
    workerInfo: WorkerInfo;
  };
}

/** 编排者命令消息 */
export interface OrchestratorCommandMessage extends BaseMessage {
  type: 'orchestrator_command';
  payload: {
    command: 'pause_all' | 'resume_all' | 'cancel_all' | 'status_check';
  };
}

/** 需求澄清请求消息（编排者 -> 用户） */
export interface ClarificationRequestMessage extends BaseMessage {
  type: 'clarification_request';
  payload: {
    taskId: string;
    questions: string[];           // 需要用户回答的问题列表
    context: string;               // 问题上下文
    ambiguityScore: number;        // 模糊度评分 (0-100)
    originalPrompt: string;        // 原始用户输入
  };
}

/** 需求澄清响应消息（用户 -> 编排者） */
export interface ClarificationResponseMessage extends BaseMessage {
  type: 'clarification_response';
  payload: {
    taskId: string;
    answers: Record<string, string>;  // 问题-答案映射
    additionalInfo?: string;          // 用户补充的额外信息
  };
}

/** Worker 提问消息（Worker -> 编排者） */
export interface WorkerQuestionMessage extends BaseMessage {
  type: 'worker_question';
  payload: {
    taskId: string;
    subTaskId: string;
    workerId: string;
    question: string;              // 问题内容
    context: string;               // 问题上下文
    options?: string[];            // 可选的选项
    timeout?: number;              // 等待超时（毫秒）
    questionId: string;            // 问题唯一ID
  };
}

/** Worker 回答消息（编排者 -> Worker） */
export interface WorkerAnswerMessage extends BaseMessage {
  type: 'worker_answer';
  payload: {
    taskId: string;
    subTaskId: string;
    questionId: string;            // 对应的问题ID
    answer: string;                // 回答内容
    answeredBy: 'user' | 'orchestrator';  // 回答来源
  };
}

/** 所有消息类型联合 */
export type BusMessage =
  | TaskDispatchMessage
  | TaskCancelMessage
  | ProgressReportMessage
  | TaskCompletedMessage
  | TaskFailedMessage
  | WorkerReadyMessage
  | OrchestratorCommandMessage
  | ClarificationRequestMessage
  | ClarificationResponseMessage
  | WorkerQuestionMessage
  | WorkerAnswerMessage;

// ============================================================================
// 编排者相关类型
// ============================================================================

/** 编排者状态 */
export type OrchestratorState =
  | 'idle'
  | 'running'
  | 'clarifying'              // 需求澄清阶段
  | 'analyzing'
  | 'waiting_questions'
  | 'waiting_confirmation'
  | 'dispatching'
  | 'monitoring'
  | 'waiting_worker_answer'   // 等待 Worker 问题回答
  | 'integrating'
  | 'verifying'
  | 'recovering'
  | 'summarizing'
  | 'completed'
  | 'failed';

/** 用户问题回调类型 */
export type QuestionCallback = (questions: string[], plan: ExecutionPlan) => Promise<string | null>;

/** 编排者配置 */
export interface OrchestratorConfig {
  /** 超时时间（毫秒） */
  timeout: number;
  /** 空闲超时时间（毫秒） */
  idleTimeout?: number;
  /** 最大执行超时时间（毫秒） */
  maxTimeout?: number;
  /** 最大重试次数 */
  maxRetries: number;
  /** 子任务自检/互检配置 */
  review?: {
    /** 子任务自检（默认 true） */
    selfCheck?: boolean;
    /** 互检策略（默认 auto） */
    peerReview?: 'auto' | 'always' | 'never';
    /** 自检/互检失败后的重做轮次（默认 1） */
    maxRounds?: number;
    /** 高风险文件后缀（用于 auto 互检） */
    highRiskExtensions?: string[];
    /** 高风险关键词（用于 auto 互检） */
    highRiskKeywords?: string[];
  };
  /** 验证配置 */
  verification?: {
    compileCheck?: boolean;
    compileCommand?: string;
    ideCheck?: boolean;
    lintCheck?: boolean;
    lintCommand?: string;
    testCheck?: boolean;
    testCommand?: string;
    timeout?: number;
  };
  /** 计划评审配置 */
  planReview?: {
    enabled?: boolean;
    reviewer?: WorkerSlot;
  };
  /** 功能集成配置 */
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: WorkerSlot;
  };
  /** 权限矩阵 */
  permissions?: PermissionMatrix;
  /** 策略开关 */
  strategy?: StrategyConfig;
  /** 上下文注入配置 */
  context?: {
    /** Worker 使用的最大 token 数 */
    workerMaxTokens?: number;
    /** Memory 摘要占比（0-1） */
    workerMemoryRatio?: number;
    /** 高风险任务额外 token */
    workerHighRiskExtraTokens?: number;
  };
}

/** 任务上下文 */
export interface TaskContext {
  taskId: string;
  sessionId?: string;
  userPrompt: string;
  plan?: ExecutionPlan;
  results: ExecutionResult[];
  risk?: {
    level: 'low' | 'medium' | 'high';
    path: 'light' | 'standard' | 'full';
    hardStop: boolean;
    verification: 'none' | 'basic' | 'full';
    score: number;
    signals: string[];
  };
  startTime: number;
  endTime?: number;
}

/** 编排者向用户发送的消息类型 */
export type OrchestratorMessageType =
  | 'plan_ready'
  | 'progress_update'
  | 'worker_output'
  | 'verification_result'
  | 'summary'
  | 'direct_response' 
  | 'question_request'
  | 'error';

/** 编排者消息（发送给前端） */
export interface OrchestratorUIMessage {
  type: OrchestratorMessageType;
  taskId: string;
  timestamp: number;
  content: string;
  metadata?: {
    phase?: OrchestratorState;
    workerId?: string;
    workerType?: WorkerSlot;
    subTaskId?: string;
    dispatchId?: string;
    progress?: number;
    plan?: ExecutionPlan;
    planId?: string;
    formattedPlan?: string;
    review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string };
    result?: ExecutionResult;
    retryAttempt?: number;
    retryDelay?: number;
    canRetry?: boolean;
    questions?: string[];
  };
}

// ============================================================================
// 事件类型
// ============================================================================

/** 消息总线事件 */
export interface MessageBusEvents {
  message: (message: BusMessage) => void;
  error: (error: Error) => void;
}

/** Worker 事件 */
export interface WorkerEvents {
  stateChange: (state: WorkerState) => void;
  output: (chunk: string) => void;
  progress: (progress: number, message?: string) => void;
  completed: (result: ExecutionResult) => void;
  failed: (error: string) => void;
}

/** 编排者事件 */
export interface OrchestratorEvents {
  stateChange: (state: OrchestratorState) => void;
  planReady: (plan: ExecutionPlan) => void;
  workerProgress: (workerId: string, progress: ProgressReportMessage['payload']) => void;
  taskCompleted: (result: ExecutionResult) => void;
  allCompleted: (results: ExecutionResult[]) => void;
  error: (error: Error) => void;
}

// ============================================================================
// Phase 2: 需求分析（合并目标理解 + 路由决策）
// ============================================================================

/**
 * Phase 2: 需求分析结果
 * 合并目标理解和路由决策，一次 LLM 调用输出完整决策
 *
 * @see docs/workflow/workflow-design.md - 5 阶段工作流
 */
export interface RequirementAnalysis {
  // ---- 目标理解 ----
  /** 用户想要达成什么 */
  goal: string;
  /** 任务的复杂度和关键点 */
  analysis: string;
  /** 任何限制条件 */
  constraints?: string[];
  /** 如何判断任务完成 */
  acceptanceCriteria?: string[];
  /** 风险等级 */
  riskLevel?: 'low' | 'medium' | 'high';
  /** 可能的风险因素 */
  riskFactors?: string[];

  // ---- 路由决策 ----
  /** 是否需要 Worker 执行 */
  needsWorker: boolean;
  /** needsWorker=false 时的直接回答 */
  directResponse?: string;
  /** 任务分类（决定哪些 Worker 参与） */
  categories?: string[];
  /** 任务委派说明（每个 Worker 的职责） */
  delegationBriefings?: string[];
  /** 执行模式 */
  executionMode?: 'direct' | 'sequential' | 'parallel' | 'dependency_chain';
  /** 是否需要工具调用 */
  needsTooling?: boolean;
  /** 是否需要修改文件 */
  requiresModification?: boolean;
  /** 决策理由（用户可见） */
  reason: string;
}
