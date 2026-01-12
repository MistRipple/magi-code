/**
 * 独立编排者架构 - 核心类型定义
 *
 * 架构理念：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */

import { CLIType, SubTask, WorkerType } from '../../types';

// 重新导出统一类型，保持向后兼容
export { SubTask, WorkerType };

// ============================================================================
// Worker 相关类型
// ============================================================================

/** Worker 状态 */
export type WorkerState = 'idle' | 'executing' | 'completed' | 'failed' | 'cancelled';

/** Worker 信息 */
export interface WorkerInfo {
  id: string;
  type: WorkerType;
  state: WorkerState;
  currentTaskId?: string;
  lastActivity?: number;
}

/** 执行计划 */
export interface ExecutionPlan {
  id: string;
  analysis: string;
  isSimpleTask?: boolean;
  /** 🆕 是否需要 Worker 执行（false 表示编排者直接回答） */
  needsWorker?: boolean;
  /** 🆕 编排者直接回答的内容（当 needsWorker=false 时使用） */
  directResponse?: string;
  /** 🆕 是否需要用户补充信息 */
  needsUserInput?: boolean;
  /** 🆕 需要用户回答的问题列表 */
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
}

/** 执行结果 */
export interface ExecutionResult {
  workerId: string;
  workerType: WorkerType;
  taskId: string;
  subTaskId: string;
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
  | 'task_dispatch'      // 编排者 -> Worker：分配任务
  | 'task_cancel'        // 编排者 -> Worker：取消任务
  | 'progress_report'    // Worker -> 编排者：进度汇报
  | 'task_completed'     // Worker -> 编排者：任务完成
  | 'task_failed'        // Worker -> 编排者：任务失败
  | 'worker_ready'       // Worker -> 编排者：Worker 就绪
  | 'orchestrator_command'; // 编排者广播命令

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

/** 所有消息类型联合 */
export type BusMessage =
  | TaskDispatchMessage
  | TaskCancelMessage
  | ProgressReportMessage
  | TaskCompletedMessage
  | TaskFailedMessage
  | WorkerReadyMessage
  | OrchestratorCommandMessage;

// ============================================================================
// 编排者相关类型
// ============================================================================

/** 编排者状态 */
export type OrchestratorState =
  | 'idle'
  | 'analyzing'
  | 'waiting_questions'
  | 'waiting_confirmation'
  | 'dispatching'
  | 'monitoring'
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
  /** 功能集成配置 */
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: WorkerType;
  };
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
  | 'direct_response'  // 🆕 编排者直接回答（不需要 Worker）
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
    workerType?: WorkerType;
    subTaskId?: string;
    progress?: number;
    plan?: ExecutionPlan;
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
