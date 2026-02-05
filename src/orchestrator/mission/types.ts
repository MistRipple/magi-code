/**
 * Mission-Driven Architecture - 核心类型定义
 *
 * 本文件定义了核心数据模型：
 * - Mission: 任务使命
 * - Contract: 协作契约
 * - Assignment: 职责分配
 *
 * 注意：WorkerTodo 已迁移到 src/todo/types.ts (UnifiedTodo)
 */

import { WorkerSlot } from '../../types';
import { UnifiedTodo, TodoType, TodoStatus, TodoOutput } from '../../todo/types';

// ============================================================================
// 状态枚举
// ============================================================================

/**
 * Mission 状态
 */
export type MissionStatus =
  | 'draft'            // 草稿
  | 'planning'         // 规划中
  | 'pending_review'   // 等待审查
  | 'pending_approval' // 等待用户确认
  | 'executing'        // 执行中
  | 'paused'           // 暂停
  | 'reviewing'        // 验收中
  | 'completed'        // 完成
  | 'failed'           // 失败
  | 'cancelled';       // 取消

/**
 * Mission 阶段
 */
export type MissionPhase =
  | 'goal_understanding'       // 理解目标
  | 'participant_selection'    // 确定参与者
  | 'contract_definition'      // 定义契约
  | 'responsibility_assignment' // 分配职责
  | 'worker_planning'          // Worker 规划
  | 'plan_review'              // 规划审查
  | 'execution'                // 执行
  | 'verification'             // 验收
  | 'summary';                 // 总结

/**
 * Contract 类型
 */
export type ContractType =
  | 'api'        // API 接口契约
  | 'data'       // 数据结构契约
  | 'event'      // 事件契约
  | 'file'       // 文件/目录契约
  | 'style'      // 样式/规范契约
  | 'dependency'; // 依赖契约

/**
 * Contract 状态
 */
export type ContractStatus =
  | 'draft'        // 草稿
  | 'proposed'     // 已提议
  | 'agreed'       // 已同意
  | 'implemented'  // 已实现
  | 'verified'     // 已验证
  | 'violated';    // 已违反

/**
 * Assignment 状态
 */
export type AssignmentStatus =
  | 'pending'    // 等待 Worker 规划
  | 'planning'   // Worker 正在规划
  | 'ready'      // 规划完成，等待执行
  | 'executing'  // 执行中
  | 'blocked'    // 被阻塞
  | 'completed'  // 完成
  | 'failed';    // 失败

/**
 * Assignment 规划状态
 */
export type PlanningStatus =
  | 'pending'   // 等待规划
  | 'planning'  // 规划中
  | 'planned'   // 已规划
  | 'approved'  // 已批准
  | 'rejected'; // 已拒绝

// TodoType 和 TodoStatus 已迁移到 src/todo/types.ts
// 重新导出以保持兼容性
export { TodoType, TodoStatus } from '../../todo/types';

/**
 * 评审级别
 */
export type ReviewLevel = 'light' | 'standard' | 'strict';

/**
 * 风险等级
 */
export type RiskLevel = 'low' | 'medium' | 'high';

/**
 * 执行路径
 */
export type ExecutionPath = 'light' | 'standard' | 'full';

// ============================================================================
// Mission 接口
// ============================================================================

/**
 * Mission - 任务使命
 * 替代现有的 ExecutionPlan，更关注"目标"而非"任务列表"
 */
export interface Mission {
  id: string;
  sessionId: string;

  // ===== 目标定义 =====
  /** 用户原始输入 */
  userPrompt: string;
  /** 提炼后的目标（一句话描述） */
  goal: string;
  /** 目标分析（为什么需要这样做） */
  analysis: string;
  /** 项目上下文 */
  context: string;

  // ===== 约束与验收 =====
  /** 约束条件（必须遵守） */
  constraints: Constraint[];
  /** 验收标准（如何判断完成） */
  acceptanceCriteria: AcceptanceCriterion[];

  // ===== 协作定义 =====
  /** 契约列表（Worker 之间的约定） */
  contracts: Contract[];
  /** 职责分配列表 */
  assignments: Assignment[];

  // ===== 风险评估 =====
  /** 风险等级 */
  riskLevel: RiskLevel;
  /** 风险因素 */
  riskFactors: string[];
  /** 执行路径 */
  executionPath: ExecutionPath;

  // ===== 状态管理 =====
  status: MissionStatus;
  /** 当前阶段 */
  phase: MissionPhase;

  // ===== 外部关联 =====
  /** 关联的外部 Task ID（用于与 UnifiedTaskManager 同步） */
  externalTaskId?: string;
  /** 快照追踪（记录此 Mission 创建的所有快照 ID） */
  snapshots?: string[];

  // ===== 时间戳 =====
  createdAt: number;
  updatedAt: number;
  startedAt?: number;
  completedAt?: number;
}

/**
 * 约束条件
 */
export interface Constraint {
  id: string;
  type: 'must' | 'should' | 'should_not' | 'must_not';
  description: string;
  source: 'user' | 'system' | 'profile';
}

/**
 * 验证规格类型
 */
export type VerificationSpecType =
  | 'file_exists'     // 文件存在验证
  | 'file_content'    // 文件内容验证
  | 'task_completed'  // 任务完成验证
  | 'test_pass'       // 测试通过验证
  | 'custom';         // 自定义验证

/**
 * 结构化验证规格
 * 替代正则解析 description 的脆弱方式
 */
export interface VerificationSpec {
  /** 验证类型 */
  type: VerificationSpecType;
  /** 目标文件路径（file_exists, file_content） */
  targetPath?: string;
  /** 期望内容（file_content） */
  expectedContent?: string;
  /** 内容匹配模式：exact=精确匹配, contains=包含, regex=正则 */
  contentMatchMode?: 'exact' | 'contains' | 'regex';
  /** 任务匹配模式（task_completed） */
  taskPattern?: string;
  /** 测试命令（test_pass） */
  testCommand?: string;
  /** 自定义验证函数名（custom） */
  customValidator?: string;
}

/**
 * 验收标准
 */
export interface AcceptanceCriterion {
  id: string;
  description: string;
  verifiable: boolean;
  verificationMethod?: 'auto' | 'manual' | 'test';
  status: 'pending' | 'passed' | 'failed';
  /** 结构化验证规格（优先于 description 解析） */
  verificationSpec?: VerificationSpec;
}

// ============================================================================
// Contract 接口
// ============================================================================

/**
 * Contract - 协作契约
 * Worker 之间的显式约定，替代隐式的依赖关系
 */
export interface Contract {
  id: string;
  missionId: string;

  // ===== 契约类型 =====
  type: ContractType;
  /** 契约名称 */
  name: string;
  /** 契约描述 */
  description: string;

  // ===== 契约内容 =====
  specification: ContractSpecification;

  // ===== 参与方 =====
  /** 提供方（谁定义/实现这个契约） */
  producer: WorkerSlot;
  /** 消费方（谁使用这个契约） */
  consumers: WorkerSlot[];

  // ===== 状态 =====
  status: ContractStatus;

  // ===== 验证 =====
  /** 验证方式 */
  verificationMethod?: 'type_check' | 'test' | 'manual';
  /** 验证结果 */
  verificationResult?: ContractVerificationResult;
}

/**
 * 契约规范
 */
export interface ContractSpecification {
  // API 契约
  api?: {
    endpoint?: string;
    method?: string;
    requestSchema?: string;
    responseSchema?: string;
    errorCodes?: Record<string, string>;
  };

  // 数据契约
  data?: {
    schema: string;
    examples?: string[];
    validationRules?: string[];
  };

  // 事件契约
  event?: {
    eventName: string;
    payload: string;
    trigger: string;
  };

  // 文件契约
  file?: {
    patterns: string[];
    namingConvention?: string;
    structure?: string;
  };
}

/**
 * 契约验证结果
 */
export interface ContractVerificationResult {
  passed: boolean;
  issues: string[];
  verifiedAt: number;
}

/**
 * 契约违反
 */
export interface ContractViolation {
  contractId: string;
  type: 'missing_producer' | 'unused_contract' | 'schema_mismatch' |
        'missing_implementation' | 'breaking_change';
  message: string;
  severity?: 'warning' | 'error';
}

/**
 * 契约解决方案
 */
export interface ContractResolution {
  action: 'block_consumer' | 'notify_consumers' | 'log' | 'auto_fix';
  message: string;
}

// ============================================================================
// Assignment 接口
// ============================================================================

/**
 * Assignment - 职责分配
 * 告诉 Worker "你负责什么"，而不是"你要做什么"
 * 替代现有的 SubTask，但更抽象
 */
export interface Assignment {
  id: string;
  missionId: string;

  // ===== Worker 分配 =====
  workerId: WorkerSlot;
  /** 分配原因（基于画像的决策记录） */
  assignmentReason: AssignmentReason;

  // ===== 职责定义 =====
  /** 简短标题，用于 Worker 卡片显示（如"分析依赖"），≤20字 */
  shortTitle: string;
  /** 职责描述（结构化，用于日志和内部逻辑） */
  responsibility: string;
  /**
   * AI 生成的委托说明（自然语言）
   * 由 Orchestrator 在理解用户意图后生成，用于向 Worker 传达任务背景和期望
   * 比 responsibility 更丰富，包含上下文理解、重点关注点、期望产出等
   */
  delegationBriefing?: string;
  /** 职责范围 */
  scope: AssignmentScope;
  /** 引导 Prompt（从画像生成） */
  guidancePrompt: string;

  // ===== 契约关联 =====
  /** 作为提供方的契约 */
  producerContracts: string[];
  /** 作为消费方的契约 */
  consumerContracts: string[];

  // ===== Worker 规划 =====
  /**
   * Todo 列表 - 使用统一的 UnifiedTodo 类型
   * 由 TodoManager 管理，这里保留引用以便快速访问
   */
  todos: UnifiedTodo[];
  /** 规划状态 */
  planningStatus: PlanningStatus;
  /** 规划审查结果 */
  planReview?: PlanReviewResult;

  // ===== 状态 =====
  status: AssignmentStatus;
  /** 进度 0-100，基于 Todo 完成度自动计算 */
  progress: number;

  // ===== 时间戳 =====
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
}

/**
 * 职责范围
 */
export interface AssignmentScope {
  /** 职责范围内（应该做的） */
  includes: string[];
  /** 职责范围外（不应该做的） */
  excludes: string[];
  /** 目标文件/目录 */
  targetPaths?: string[];
  /** 是否需要对目标文件产生实际修改 */
  requiresModification?: boolean;
}

/**
 * 分配原因
 */
export interface AssignmentReason {
  /** 匹配的画像偏好 */
  profileMatch: {
    category: string;
    score: number;
    matchedKeywords: string[];
  };
  /** 契约角色 */
  contractRole: 'producer' | 'consumer' | 'both' | 'none';
  /** 决策说明 */
  explanation: string;
  /** 备选方案 */
  alternatives: Array<{
    workerId: WorkerSlot;
    score: number;
    reason: string;
  }>;
}

/**
 * 规划审查结果
 */
export interface PlanReviewResult {
  status: 'approved' | 'needs_revision' | 'rejected';
  feedback: string;
  issues?: PlanIssue[];
  reviewedAt: number;
}

/**
 * 规划问题
 */
export interface PlanIssue {
  type: 'suboptimal_assignment' | 'weakness_match' | 'scope_violation' |
        'missing_dependency' | 'critical';
  taskId?: string;
  message: string;
  suggestion?: string;
  reviewLevel?: ReviewLevel;
}

// ============================================================================
// WorkerTodo - DEPRECATED
// ============================================================================
// WorkerTodo 接口已迁移到 src/todo/types.ts 中的 UnifiedTodo
// 请使用 import { UnifiedTodo } from '../../todo/types'

/**
 * @deprecated 使用 UnifiedTodo 替代
 */
export type WorkerTodo = UnifiedTodo;

// 重新导出 TodoOutput 以保持兼容性
export { TodoOutput } from '../../todo/types';

// ============================================================================
// 创建参数接口
// ============================================================================

/**
 * Mission 创建参数
 */
export interface CreateMissionParams {
  userPrompt: string;
  sessionId: string;
  context?: string;
}

/**
 * Contract 创建参数
 */
export interface CreateContractParams {
  missionId: string;
  type: ContractType;
  name: string;
  description: string;
  producer: WorkerSlot;
  consumers: WorkerSlot[];
  specification?: ContractSpecification;
}

/**
 * Assignment 创建参数
 */
export interface CreateAssignmentParams {
  missionId: string;
  workerId: WorkerSlot;
  shortTitle: string;
  responsibility: string;
  scope: AssignmentScope;
  assignmentReason: AssignmentReason;
  producerContracts?: string[];
  consumerContracts?: string[];
}

// CreateTodoParams 已迁移到 src/todo/types.ts
// 请使用 import { CreateTodoParams } from '../../todo/types'
export { CreateTodoParams } from '../../todo/types';

// ============================================================================
// 事件类型
// ============================================================================

/**
 * Mission 事件
 */
export interface MissionEvents {
  missionCreated: { mission: Mission };
  missionStatusChanged: { missionId: string; oldStatus: MissionStatus; newStatus: MissionStatus };
  missionPhaseChanged: { missionId: string; oldPhase: MissionPhase; newPhase: MissionPhase };
  collaborationPlanned: { mission: Mission };
  workersPlanned: { mission: Mission };
  missionCompleted: { mission: Mission };
  missionFailed: { mission: Mission; error: string };
}

/**
 * Assignment 事件
 */
export interface AssignmentEvents {
  assignmentCreated: { assignment: Assignment };
  assignmentStatusChanged: { assignmentId: string; oldStatus: AssignmentStatus; newStatus: AssignmentStatus };
  planningCompleted: { assignmentId: string; todoIds: string[] };
  planReviewCompleted: { assignmentId: string; result: PlanReviewResult };
}

// Todo 事件已迁移到 src/todo/types.ts
// 请使用 import { TodoEvents } from '../../todo/types'

/**
 * Contract 事件
 */
export interface ContractEvents {
  contractCreated: { contract: Contract };
  contractStatusChanged: { contractId: string; oldStatus: ContractStatus; newStatus: ContractStatus };
  contractViolated: { contractId: string; violation: ContractViolation };
}
