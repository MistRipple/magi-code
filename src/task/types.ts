/**
 * 任务系统 - 统一类型定义
 *
 * 重构说明：
 * - 统一 TaskStatus 和 SubTaskStatus 定义
 * - 删除所有向后兼容字段（assignedCli, WorkerType）
 * - 添加新功能字段（priority, retry, timeout, pause）
 */

import { CLIType } from '../types';

// ============================================================================
// 状态定义
// ============================================================================

/**
 * 任务状态
 *
 * 状态转换图:
 * pending → running → completed
 *              ↓
 *           paused → running
 *              ↓
 *           failed → retrying → running
 *              ↓
 *          cancelled
 */
export type TaskStatus =
  | 'pending'      // 等待执行
  | 'running'      // 执行中
  | 'paused'       // 已暂停
  | 'retrying'     // 重试中
  | 'completed'    // 已完成
  | 'failed'       // 失败
  | 'cancelled';   // 已取消

/**
 * 子任务状态
 */
export type SubTaskStatus =
  | 'pending'      // 等待执行
  | 'running'      // 执行中
  | 'paused'       // 已暂停
  | 'retrying'     // 重试中
  | 'completed'    // 已完成
  | 'failed'       // 失败
  | 'skipped';     // 跳过

// ============================================================================
// Task 接口
// ============================================================================

/**
 * Task - 用户任务
 * 用户每次输入 Prompt 时由 Orchestrator 创建
 */
export interface Task {
  // 基础信息
  id: string;
  sessionId: string;
  prompt: string;

  // 状态管理
  status: TaskStatus;
  priority: number;              // 优先级 (1-10, 1 最高)

  // 子任务
  subTasks: SubTask[];

  // 时间戳
  createdAt: number;
  startedAt?: number;
  pausedAt?: number;
  completedAt?: number;
  cancelledAt?: number;

  // 重试机制
  retryCount: number;            // 当前重试次数
  maxRetries: number;            // 最大重试次数

  // 超时控制
  timeout?: number;              // 超时时间（毫秒）
  timeoutAt?: number;            // 超时时间点

  // 执行计划
  planId?: string;
  planStatus?: 'draft' | 'ready' | 'executing' | 'completed' | 'failed';
  planSummary?: string;
  planCreatedAt?: number;
  planUpdatedAt?: number;

  // 功能契约
  featureContract?: string;
  acceptanceCriteria?: string[];
}

// ============================================================================
// SubTask 接口
// ============================================================================

/**
 * SubTask - 子任务
 * Task 分解后的执行单元，每个 SubTask 由一个 Worker 执行
 */
export interface SubTask {
  // 基础信息
  id: string;
  taskId: string;
  description: string;
  title?: string;

  // Worker 分配（统一命名，删除 assignedCli）
  assignedWorker: CLIType;
  reason?: string;
  prompt?: string;

  // 文件跟踪
  targetFiles: string[];
  modifiedFiles?: string[];

  // 依赖关系
  dependencies: string[];
  conflictDomain?: string;
  dependencyChain?: string[];

  // 任务分类
  priority: number;              // 优先级 (1-10, 1 最高)
  kind?: 'implementation' | 'integration' | 'repair' | 'architecture' | 'batch' | 'background';
  featureId?: string;
  batchItems?: string[];
  background?: boolean;

  // 状态管理
  status: SubTaskStatus;
  progress: number;              // 进度 (0-100)

  // 重试机制
  retryCount: number;            // 当前重试次数
  maxRetries: number;            // 最大重试次数

  // 超时控制
  timeout?: number;              // 超时时间（毫秒）
  timeoutAt?: number;            // 超时时间点

  // 执行结果
  output: string[];
  result?: WorkerResult;
  error?: string;

  // 时间戳
  startedAt?: number;
  pausedAt?: number;
  completedAt?: number;
}

// ============================================================================
// Worker 结果
// ============================================================================

export interface WorkerResult {
  cliType: CLIType;
  success: boolean;
  output?: string;
  modifiedFiles?: string[];
  error?: string;
  duration: number;
  timestamp: Date;
  inputTokens?: number;
  outputTokens?: number;
}

// ============================================================================
// 辅助类型
// ============================================================================

/**
 * Task 创建参数
 */
export interface CreateTaskParams {
  prompt: string;
  priority?: number;
  maxRetries?: number;
  timeout?: number;
}

/**
 * SubTask 创建参数
 */
export interface CreateSubTaskParams {
  description: string;
  assignedWorker: CLIType;
  targetFiles?: string[];
  dependencies?: string[];
  priority?: number;
  maxRetries?: number;
  timeout?: number;
  reason?: string;
  prompt?: string;
  kind?: SubTask['kind'];
  background?: boolean;
}
