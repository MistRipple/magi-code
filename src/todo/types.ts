/**
 * Unified Todo System - Type Definitions
 *
 * 统一的 Todo 类型定义
 *
 * 设计原则：
 * - 单一数据源：不再有两套 Todo 系统
 * - 高可用：支持持久化、超时、重试
 * - 契约驱动：保留契约依赖检查
 * - 范围检查：保留超范围审批机制
 */

import { WorkerSlot } from '../types';

// ============================================================================
// 状态枚举
// ============================================================================

/**
 * Todo 类型 - 描述工作项的性质
 */
export type TodoType =
  | 'discovery'      // 探索/调研
  | 'design'         // 设计/规划
  | 'implementation' // 实现
  | 'verification'   // 验证/测试
  | 'integration'    // 集成
  | 'fix'            // 修复
  | 'refactor';      // 重构

/**
 * Todo 状态 - 统一的状态机
 *
 * 状态转换图:
 * ```
 * pending ──┬──▶ blocked ──▶ ready
 *           │       ▲          │
 *           │       │          ▼
 *           └──────────────▶ running ──┬──▶ completed
 *                              │        │
 *                              │        └──▶ failed ──▶ pending (retry)
 *                              │
 *                              └──▶ skipped
 * ```
 */
export type TodoStatus =
  | 'pending'     // 初始状态，等待依赖检查
  | 'blocked'     // 被阻塞（依赖/契约未满足）
  | 'ready'       // 就绪（依赖已满足，可执行）
  | 'running'     // 执行中
  | 'completed'   // 完成
  | 'failed'      // 失败
  | 'skipped';    // 跳过

/**
 * 审批状态 - 超范围任务的审批
 */
export type ApprovalStatus = 'pending' | 'approved' | 'rejected';

// ============================================================================
// 核心接口
// ============================================================================

/**
 * UnifiedTodo - 统一的工作项
 */
export interface UnifiedTodo {
  // ===== 标识 =====
  /** 唯一 ID */
  id: string;
  /** 所属 Mission ID */
  missionId: string;
  /** 所属 Assignment ID */
  assignmentId: string;
  /** 父 Todo ID（动态拆分时指向宏观 Todo） */
  parentId?: string;

  // ===== 内容 =====
  /** 任务描述 */
  content: string;
  /** 推理说明（为什么需要这个 Todo） */
  reasoning: string;
  /** 预期产出 */
  expectedOutput?: string;
  /** Worker 执行 prompt（可选，用于复杂任务） */
  prompt?: string;

  // ===== 分类 =====
  /** Todo 类型 */
  type: TodoType;
  /** 分配的 Worker */
  workerId: WorkerSlot;
  /** 优先级 1-5，1 最高 */
  priority: number;

  // ===== 依赖管理 =====
  /** 依赖的 Todo ID（同 Mission 内） */
  dependsOn: string[];
  /** 依赖的契约（必须已实现才能执行） */
  requiredContracts: string[];
  /** 产生的契约（执行后实现这些契约） */
  producesContracts: string[];
  /** 阻塞原因（当 status 为 blocked 时） */
  blockedReason?: string;

  // ===== 范围检查 =====
  /** 是否超出职责范围 */
  outOfScope: boolean;
  /** 超范围审批状态 */
  approvalStatus?: ApprovalStatus;
  /** 审批说明 */
  approvalNote?: string;

  // ===== 状态 =====
  /** 当前状态 */
  status: TodoStatus;
  /** 进度 0-100 */
  progress: number;

  // ===== 超时与重试 =====
  /** 超时时间（毫秒） */
  timeout?: number;
  /** 超时时间点 */
  timeoutAt?: number;
  /** 已重试次数 */
  retryCount: number;
  /** 最大重试次数 */
  maxRetries: number;

  // ===== 执行结果 =====
  /** 执行输出 */
  output?: TodoOutput;
  /** 错误信息 */
  error?: string;
  /** 修改的文件列表 */
  modifiedFiles?: string[];
  /** 目标文件列表（计划要修改的） */
  targetFiles?: string[];

  // ===== 时间戳 =====
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
  pausedAt?: number;
}

/**
 * Todo 执行输出
 */
export interface TodoOutput {
  /** 是否成功 */
  success: boolean;
  /** 输出摘要 */
  summary: string;
  /** 修改的文件 */
  modifiedFiles: string[];
  /** 产生的新契约（如果有） */
  newContracts?: string[];
  /** 发现的问题 */
  issues?: string[];
  /** 错误信息 */
  error?: string;
  /** 执行时长（毫秒） */
  duration: number;
  /** Token 使用 */
  tokenUsage?: {
    input: number;
    output: number;
  };
}

// ============================================================================
// 创建参数
// ============================================================================

/**
 * 创建 Todo 的参数
 */
export interface CreateTodoParams {
  missionId: string;
  assignmentId: string;
  parentId?: string;
  content: string;
  reasoning: string;
  type: TodoType;
  workerId: WorkerSlot;
  priority?: number;
  expectedOutput?: string;
  prompt?: string;
  dependsOn?: string[];
  requiredContracts?: string[];
  producesContracts?: string[];
  targetFiles?: string[];
  timeout?: number;
  maxRetries?: number;
}

/**
 * 更新 Todo 的参数
 */
export interface UpdateTodoParams {
  content?: string;
  reasoning?: string;
  expectedOutput?: string;
  priority?: number;
  dependsOn?: string[];
  requiredContracts?: string[];
  producesContracts?: string[];
}

// ============================================================================
// 事件类型
// ============================================================================

/**
 * Todo 事件
 */
export interface TodoEvents {
  'todo:created': (todo: UnifiedTodo) => void;
  'todo:ready': (todo: UnifiedTodo) => void;
  'todo:started': (todo: UnifiedTodo) => void;
  'todo:progress': (todo: UnifiedTodo, progress: number) => void;
  'todo:completed': (todo: UnifiedTodo) => void;
  'todo:failed': (todo: UnifiedTodo, error: string) => void;
  'todo:blocked': (todo: UnifiedTodo, reason: string) => void;
  'todo:unblocked': (todo: UnifiedTodo) => void;
  'todo:skipped': (todo: UnifiedTodo) => void;
  'todo:timeout': (todo: UnifiedTodo) => void;
  'todo:retrying': (todo: UnifiedTodo) => void;
  'todo:approval-requested': (todo: UnifiedTodo) => void;
  'todo:approved': (todo: UnifiedTodo) => void;
  'todo:rejected': (todo: UnifiedTodo) => void;
}

// ============================================================================
// 规划相关
// ============================================================================

/**
 * 规划审查反馈
 */
export interface PlanReviewFeedback {
  /** 审查状态 */
  status: 'approved' | 'needs_revision' | 'rejected';
  /** 需要添加的 Todo */
  todosToAdd?: CreateTodoParams[];
  /** 需要移除的 Todo ID */
  todosToRemove?: string[];
  /** 需要修改的 Todo */
  todosToModify?: Array<{
    todoId: string;
    updates: UpdateTodoParams;
  }>;
  /** 审查意见 */
  comments?: string;
  /** 拒绝原因 */
  rejectionReason?: string;
}

// ============================================================================
// 查询接口
// ============================================================================

/**
 * Todo 查询条件
 */
export interface TodoQuery {
  missionId?: string;
  assignmentId?: string;
  workerId?: WorkerSlot;
  status?: TodoStatus | TodoStatus[];
  type?: TodoType | TodoType[];
  outOfScope?: boolean;
}

/**
 * Todo 统计信息
 */
export interface TodoStats {
  total: number;
  byStatus: Record<TodoStatus, number>;
  byType: Record<TodoType, number>;
  byWorker: Record<string, number>;
  completionRate: number;
  averageDuration: number;
}
