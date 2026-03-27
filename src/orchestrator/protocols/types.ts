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
// 执行相关类型
// ============================================================================

/** 执行计划 */
export interface ExecutionPlan {
  id: string;
  analysis: string;
  isSimpleTask?: boolean;
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
  /** 任务分类（决定哪些 Worker 参与） */
  categories?: string[];
  /** 执行入口路径 */
  entryPath?: 'direct_response' | 'lightweight_analysis' | 'task_execution';
  /** 任务委派说明（每个 Worker 的职责） */
  delegationBriefings?: string[];
  /** 执行模式 */
  executionMode?: 'direct' | 'analysis' | 'sequential' | 'parallel' | 'dependency_chain';
  /** 本轮是否展示 thinking */
  includeThinking?: boolean;
  /** 本轮是否允许工具调用 */
  includeToolCalls?: boolean;
  /** 本轮允许的工具白名单 */
  allowedToolNames?: string[];
  /** 本轮历史注入策略 */
  historyMode?: 'session' | 'isolated';
  /** 是否需要修改文件 */
  requiresModification?: boolean;
  /** 分类决策因子（供审计/校准使用） */
  decisionFactors?: string[];
  /** 决策理由（用户可见） */
  reason: string;
}
